//! The one resolver loop: workspace-root wait, the store's gates, the ask
//! drain, map moves, and the per-ask acquisition (server and headless).
//! Every asker — a didOpen's or a save's imports, a one-shot run's demand,
//! a query file's imports, descent from a landed file — reaches the tier
//! through `IndexCore::ask`, and only this thread lands.

use super::*;
use crate::build::dependency_map::Ask;
use crate::index::module_index::{FrontDoor, Outcome};

/// The ONE resolver loop, server and headless alike. Mode differences are
/// explicit `server` gates inside this body — the per-module resolve
/// protocol (memoization, persistence, strip, `insert_resolved`) has a
/// single spelling and cannot drift between the two spawn fronts.
pub(super) fn resolver_loop(core: Arc<IndexCore>, server: Option<ServerSession>) {
    // Wait for workspace root from initialize() for per-project cache path.
    let ws_root = wait_for_workspace_root(&core.workspace_root);
    let root_path = ws_root.as_ref().and_then(|u| uri_to_path(u));

    // The map, read before anything can query: an origin's visibility rank
    // prefix-matches candidates against its search roots, and a reader may
    // shell out (perl's) or read a generated table (php's classmap), so no
    // request path may re-derive it. The walk may have read it first;
    // either way it is read once.
    let served = crate::build::language_driver::LanguageRegistry::served();
    core.dependency_map_for(served, root_path.as_deref());
    let (mut map, mut map_gen) = core.current_map();
    let mut parse = Parse::of(served);
    // The hub's store gates, builtin docs, cpanfile demand and conclusion
    // repair are its own; a pack's store has the one gate its indexer runs.
    let hub = FrontDoor::of(served) == FrontDoor::Hub;

    // Scan the search roots for available module names (fast, no parsing — just readdir)
    scan_inc_module_names(&map, &core.available_modules);
    log::info!("@INC scan: {} modules available", core.available_modules.len());

    // The store's gates run before any landing reads a row. There is no
    // warm lane: a landing takes each located file from its row in place of
    // parsing it, so a warm start lands what a cold one would. No gate reads
    // the map: a dependency row describes its file alone, valid by its own
    // stamp, whichever map located it.
    let db = module_cache::open_cache_db(ws_root.as_deref(), served.id());
    if let (Some(conn), false) = (&db, hub) {
        let _ = module_cache::validate_input_fingerprint(conn, served.analysis_input_fingerprint());
    }
    if let (Some(conn), true) = (&db, hub) {
        let _ = module_cache::validate_plugin_fingerprint(
            conn,
            &crate::build::plugin::rhai_host::plugin_fingerprint(),
        );
        // Beside the plugin gate, and for the same reason: a cached artifact
        // that describes a derivation we no longer run. This one clears
        // conclusions only — the blobs stay, because the repair is a re-bake.
        let _ = module_cache::validate_conclusion_fingerprint(
            conn,
            module_cache::conclusion_fingerprint(),
        );
        if server.is_some() {
            // Hydrate Perl builtin hover docs (cached in SQLite, re-parsed
            // from perlfunc.pod only when the perl version tag changes).
            // Server-only: a one-shot session would pay the cold parse for
            // hover docs it never serves.
            match module_cache::hydrate_builtins(conn) {
                Ok(map) => {
                    for entry in map.iter() {
                        core.builtins.insert(entry.key().clone(), entry.value().clone());
                    }
                }
                Err(e) => log::warn!("Builtins hydrate failed: {}", e),
            }
        }
    }

    // Queue cpanfile dependencies (non-blocking).
    // Track total for progress reporting in the main loop. Server-only: a
    // one-shot session must not burn its wall resolving the dep tree in the
    // background.
    let mut cpanfile_total = 0usize;
    let mut cpanfile_done = 0usize;
    if let (Some(srv), true) = (&server, hub) {
        if let Some(root_path) = ws_root.as_ref().and_then(|u| uri_to_path(u)) {
            let cpanfile_modules = cpanfile::parse_cpanfile(&root_path);
            let to_resolve: Vec<Ask> = cpanfile_modules
                .into_iter()
                .map(Ask::Name)
                .filter(|a| !core.asked_now(a))
                .collect();

            if !to_resolve.is_empty() {
                cpanfile_total = to_resolve.len();
                log::info!("cpanfile: {} modules queued for indexing", cpanfile_total);

                // Start progress bar.
                let token = NumberOrString::String("perl-lsp/indexing".to_string());
                let _ = srv.handle.block_on(srv.client.send_request::<request::WorkDoneProgressCreate>(
                    WorkDoneProgressCreateParams { token: token.clone() },
                ));
                srv.handle.block_on(srv.client.send_notification::<notification::Progress>(
                    ProgressParams {
                        token,
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                            WorkDoneProgressBegin {
                                title: "Indexing Perl modules".into(),
                                cancellable: Some(false),
                                message: None,
                                percentage: Some(0),
                            },
                        )),
                    },
                ));

                core.queue.ask(to_resolve);
            }
        }
    }

    // The conclusion-repair frontier: paths holding a valid blob whose map the
    // derivation-fingerprint gate cleared. Enumerated ONCE here rather than
    // re-queried per slice — the set only shrinks as we drain it, and a
    // re-query per slice would be an O(corpus) scan for every 32 files.
    //
    // Deliberately NOT drained before the loop. It has no deadline, and the
    // resolver thread is what answers a real cross-file request; a corpus-sized
    // repair run ahead of the loop would put minutes in front of the first
    // resolve. It runs in the gap where this thread would otherwise be blocked
    // on its condvar, which is the definition of "post-ready background".
    //
    // With baking switched off, the frontier narrows to the SURFACE half
    // rather than emptying: the map half is the second producer of a
    // conclusions row (leaving it on would let the control's own arm bake
    // the whole corpus in the background and measure a full layer from its
    // second warm run onward), but the surface half is the freshness
    // machinery's product, which the control does not claim to ablate —
    // gating the whole frontier left a NO_BAKE arm running against
    // un-repaired surfaces.
    let mut repair_frontier: Vec<String> = db
        .as_ref()
        .filter(|_| hub)
        .map(|conn| {
            let at = module_cache::current_generation(conn);
            let f = module_cache::paths_needing_repair(
                conn,
                at,
                !crate::model::witnesses::bake_disabled(),
            );
            if !f.is_empty() {
                log::info!(
                    "Derivation repair: {} file(s) hold a blob whose map or \
                     surface is missing or from another version; re-deriving \
                     in the background",
                    f.len()
                );
            }
            f
        })
        .unwrap_or_default();

    // A server descends further than a one-shot run (`descent`), and strips
    // what it lands: the rehydration a strip costs only amortizes in a
    // process that lives. A one-shot run keeps its landings whole.
    let prefetch = server.is_some();
    let strip = eviction_enabled()
        && (server.is_some() || core.long_lived.load(std::sync::atomic::Ordering::Relaxed));
    let mut sweep_owed = false;
    core.queue.started.store(true, std::sync::atomic::Ordering::Release);
    notify_resolved(&core);

    loop {
        let batch = drain_or_repair(&core, &core.queue, &mut repair_frontier, db.as_ref());

        if core.queue.map_inputs_moved.swap(false, std::sync::atomic::Ordering::AcqRel) {
            core.reread_dependency_map(served, root_path.as_deref());
        }
        // A move: every ask is asked again against the new map, which
        // restamps what it still places and lands what it newly places; the
        // sweep, once the re-asks drain, takes back what it no longer places.
        // The store's rows stay: a row describes its file, not the map that
        // placed it, and one the new map no longer reaches is `--gc-cache`'s.
        if core.queue.map_moved.swap(false, std::sync::atomic::Ordering::AcqRel) {
            (map, map_gen) = core.current_map();
            log::info!("Dependency map moved (generation {}); asking {} names again", map_gen, core.asked.len());
            core.available_modules.clear();
            scan_inc_module_names(&map, &core.available_modules);
            let again: Vec<Ask> = core.asked.iter().map(|e| e.key().clone()).collect();
            core.queue.ask(again);
            sweep_owed = true;
        }

        // One diagnostics refresh per drained BATCH, not per module: a
        // resolve takes longer than the refresh debounce's settle window,
        // so per-module fires all survive debouncing and each one re-
        // enriches + republishes every open doc (measured: 939 resolves →
        // 346 full refresh bodies on one cold open). The queue keeps
        // accumulating while a batch runs, so the LAST non-empty batch
        // always ends with a fire — diagnostics converge once the queue
        // drains.
        let mut resolved_any = false;
        for ask in batch {
            if core.asked.get(&ask).is_some_and(|e| e.0 == map_gen) {
                continue;
            }
            let names: Vec<String> = match &ask {
                // A project file answers the name as written. Otherwise the
                // name lands as the modules the map places it under: itself,
                // or the module it is a member of (`pkg.models.Base` loads
                // `pkg.models`).
                Ask::Name(n) if core.project_answers(n) => vec![n.clone()],
                Ask::Name(n) => {
                    let mut names: Vec<String> = Vec::new();
                    for l in map.locate(&ask) {
                        if let Some(m) = l.loads_as.filter(|m| !names.contains(m)) {
                            names.push(m);
                        }
                    }
                    if names.is_empty() {
                        names.push(n.clone());
                    }
                    names
                }
                // A file asks for the names the map places it under.
                Ask::File(_) => map.locate(&ask).into_iter().filter_map(|l| l.loads_as).collect(),
            };
            let mut landed = false;
            for module_name in names {
                if core.project_answers(&module_name) {
                    crate::util::ghost_stats::count("resolver.project_answered");
                    landed = true;
                    continue;
                }
                log::info!("Resolving module '{}'", module_name);
                crate::util::ghost_stats::count("resolver.module_resolved");
                let (whole, stored) = land_module(
                    &core,
                    db.as_ref(),
                    &map,
                    map_gen,
                    &module_name,
                    &mut parse,
                    strip,
                );
                match &stored {
                    Some(providers) => log::info!(
                        "Resolved '{}': {} provider(s), {} export, {} export_ok",
                        module_name,
                        providers.len(),
                        providers[0].analysis.export.len(),
                        providers[0].analysis.export_ok.len()
                    ),
                    None => log::info!("No exports found for '{}'", module_name),
                }
                landed |= match (&ask, &stored) {
                    (Ask::File(p), Some(providers)) => providers.iter().any(|m| m.path == *p),
                    (_, stored) => stored.is_some(),
                };
                if let Some(providers) = &whole {
                    core.ask(descent(providers, prefetch));
                }
            }
            let label = match &ask {
                Ask::Name(n) => n.clone(),
                Ask::File(p) => p.to_string_lossy().into_owned(),
            };
            core.asked.insert(ask, (map_gen, if landed { Outcome::Landed } else { Outcome::Miss }));

            // Report cpanfile progress.
            if let Some(srv) = &server {
                if cpanfile_total > 0 && cpanfile_done < cpanfile_total {
                    cpanfile_done += 1;
                    let pct = (cpanfile_done * 100 / cpanfile_total) as u32;
                    let token = NumberOrString::String("perl-lsp/indexing".to_string());
                    if cpanfile_done < cpanfile_total {
                        srv.handle.block_on(srv.client.send_notification::<notification::Progress>(
                            ProgressParams {
                                token,
                                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                                    WorkDoneProgressReport {
                                        cancellable: Some(false),
                                        message: Some(format!("{} ({}/{})", label, cpanfile_done, cpanfile_total)),
                                        percentage: Some(pct),
                                    },
                                )),
                            },
                        ));
                    } else {
                        srv.handle.block_on(srv.client.send_notification::<notification::Progress>(
                            ProgressParams {
                                token,
                                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                                    WorkDoneProgressEnd {
                                        message: Some(format!("Indexed {} modules", cpanfile_total)),
                                    },
                                )),
                            },
                        ));
                    }
                }
            }

            // Signal waiters (per ask — bounded waits key on asks).
            notify_resolved(&core);
            resolved_any = true;
        }

        // The sweep runs at idle, and only against the map this thread
        // re-asked through: a move published mid-batch is the next turn's.
        if sweep_owed
            && map_gen == core.map_generation()
            && !core.queue.map_moved.load(std::sync::atomic::Ordering::Acquire)
            && core.queue.pending.lock().unwrap().is_empty()
        {
            core.sweep_stale_placements();
            sweep_owed = false;
            resolved_any = true;
        }
        core.queue.busy.store(false, std::sync::atomic::Ordering::Release);
        notify_resolved(&core);
        if resolved_any {
            if let Some(srv) = &server {
                crate::util::ghost_stats::count("resolver.on_resolved_fired");
                (srv.on_resolved)();
            }
        }
    }
}

/// What a landed file asks for next. Parents and re-exports always: the
/// MRO and a module's export surface read them at query time. With
/// `prefetch` (a server), also its imports and the classes its subs
/// return — the chain a long-lived session walks before anyone asks.
fn descent(providers: &[Arc<CachedModule>], prefetch: bool) -> Vec<Ask> {
    use crate::model::file_analysis::{InferredType, SymKind, SymbolDetail};
    let mut asks: Vec<String> = Vec::new();
    // Every provider's, not just the winner's: a shadowed twin has its own
    // `use` list and `@ISA`, and a class only it names must still resolve.
    for m in providers {
        let fa = &m.analysis;
        for (_pkg, parents) in fa.package_parent_edges() {
            asks.extend(parents.iter().cloned());
        }
        // A re-exporting module (Test::Most → Test::More) pulls its
        // producers' surfaces transitively, so those producers must land
        // even when no file `use`s them directly.
        asks.extend(fa.reexport_modules.iter().cloned());
        if !prefetch {
            continue;
        }
        asks.extend(fa.imports.iter().map(|i| i.module_name.clone()));
        // `has foo => sub { Bar->new }`, plugin-emitted typed subs, return
        // annotations: classes a chain walks through at query time.
        for sym in fa.symbols() {
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) || !matches!(sym.detail, SymbolDetail::Sub { .. }) {
                continue;
            }
            if let Some(InferredType::ClassName(c)) = fa.symbol_return_type_via_bag(sym.id, None) {
                asks.push(c);
            }
        }
        // A gated emission's accessors name classes too (a `belongs_to` its
        // target); asked as candidates, like every other feed of gated
        // content.
        for w in fa.gated_candidates().witnesses {
            if let crate::model::witnesses::WitnessPayload::InferredType(InferredType::ClassName(c)) = w.payload {
                asks.push(c);
            }
        }
    }
    asks.retain(|n| !n.is_empty());
    asks.into_iter().map(Ask::Name).collect()
}

fn notify_resolved(core: &IndexCore) {
    let _g = core.resolved.mu.lock().unwrap();
    core.resolved.cv.notify_all();
}

/// `drain_next_batch`, except the idle wait is spent re-baking conclusions.
///
/// Real work always wins: the queue is checked before every slice, so an ask
/// waits at most one slice (32 files) behind repair rather than behind the
/// whole frontier. When the frontier is empty this is exactly the blocking
/// drain.
/// Move what consults pushed onto the repair frontier.
///
/// The frontier is enumerated once from the store's own "what is missing"
/// query; this is the other half — paths whose row EXISTS and was rejected,
/// which that query cannot see.
///
/// Adopted rather than signalled, so a push costs a consult one map insert and
/// nothing else. The cost is latency, not correctness: a push landing while
/// the resolver is blocked on its queue waits for the next resolve request.
/// Repair has no deadline, and the set is path-keyed, so the wait bounds at
/// one entry per stale file rather than one per rejection.
///
/// Split out of `drain_or_repair` so the handoff is testable on its own: the
/// caller blocks on the resolve queue, so a test of it through the resolver
/// would be a test of that race rather than of this transfer. Note the counter
/// is emitted HERE, at ADOPTION — an absent `repair.pushed` means nothing was
/// drained, never that nothing was pushed.
fn adopt_pushed_repairs(core: &IndexCore, frontier: &mut Vec<String>) {
    if core.repair_pushed.is_empty() {
        return;
    }
    let pushed: Vec<String> = core
        .repair_pushed
        .iter()
        .map(|e| e.key().to_string_lossy().into_owned())
        .collect();
    core.repair_pushed.clear();
    crate::util::ghost_stats::count_by("repair.pushed", pushed.len() as u64);
    frontier.extend(pushed);
}

fn drain_or_repair(
    core: &IndexCore,
    queue: &ResolveQueue,
    frontier: &mut Vec<String>,
    db: Option<&rusqlite::Connection>,
) -> Vec<Ask> {
    adopt_pushed_repairs(core, frontier);
    while !frontier.is_empty() {
        if let Some(batch) = try_drain_next_batch(queue) {
            return batch;
        }
        let Some(conn) = db else {
            // No store to repair into; forget the frontier rather than
            // spinning on it forever.
            frontier.clear();
            break;
        };
        let take = module_cache::REPAIR_SLICE.min(frontier.len());
        let slice: Vec<String> = frontier.split_off(frontier.len() - take);
        let at = module_cache::current_generation(conn);
        module_cache::repair_conclusions_slice(conn, &slice, at);
        if frontier.is_empty() {
            log::info!("Derivation repair: frontier drained");
        }
    }
    drain_next_batch(queue)
}

/// The non-blocking half of `drain_next_batch`. `None` means "nothing queued
/// right now", which is a different answer from the blocking form's "nothing
/// queued, so I waited".
fn try_drain_next_batch(queue: &ResolveQueue) -> Option<Vec<Ask>> {
    let mut pending = queue.pending.lock().unwrap();
    take_batch(queue, &mut pending)
}

pub(super) fn drain_next_batch(queue: &ResolveQueue) -> Vec<Ask> {
    let mut pending = queue.pending.lock().unwrap();
    loop {
        if let Some(batch) = take_batch(queue, &mut pending) {
            return batch;
        }
        pending = queue.condvar.wait(pending).unwrap();
    }
}

/// Take what is queued, or an empty batch when only a map flag is set (the
/// loop acts on the flag). Marks the queue busy under `pending`, so
/// `wait_idle` never sees an empty queue with a batch in flight.
fn take_batch(queue: &ResolveQueue, pending: &mut Vec<Ask>) -> Option<Vec<Ask>> {
    use std::sync::atomic::Ordering;
    let batch = if !pending.is_empty() {
        Some(std::mem::take(pending))
    } else if queue.map_moved.load(Ordering::Acquire) || queue.map_inputs_moved.load(Ordering::Acquire) {
        Some(Vec::new())
    } else {
        None
    };
    if batch.is_some() {
        queue.busy.store(true, Ordering::Release);
    }
    batch
}

// ---- Internal helpers ----

fn wait_for_workspace_root(ws_root_channel: &WorkspaceRootChannel) -> Option<String> {
    let mut guard = ws_root_channel.root.lock().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while guard.is_none() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            log::warn!("Timed out waiting for workspace root; using global cache");
            break;
        }
        let (g, _) = ws_root_channel
            .condvar
            .wait_timeout(guard, remaining)
            .unwrap();
        guard = g;
    }
    guard.clone().flatten()
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

#[cfg(test)]
mod repair_adoption_tests {
    use super::*;

    /// What consults pushed reaches the repair frontier, once, and the set
    /// empties behind it.
    ///
    /// This is the half neither local instrument could reach: a `--check` run
    /// never produces a stale row, and a saved-dep e2e produces eight but is
    /// over in 2.5 s — long before the resolver idles into a repair pass. So
    /// the transfer is asserted directly rather than by winning that race.
    ///
    /// The emptying is the load-bearing half. Without it every later pass
    /// re-adopts the same paths, and the frontier grows without bound while
    /// re-repairing files it already repaired.
    #[test]
    fn pushed_paths_are_adopted_once_and_the_set_empties() {
        let core = IndexCore::new();
        let mut frontier: Vec<String> = vec!["/pre/existing.pm".to_string()];

        core.repair_pushed.insert(std::path::PathBuf::from("/a.pm"), ());
        core.repair_pushed.insert(std::path::PathBuf::from("/b.pm"), ());

        adopt_pushed_repairs(&core, &mut frontier);
        frontier.sort();
        assert_eq!(
            frontier,
            vec!["/a.pm".to_string(), "/b.pm".to_string(), "/pre/existing.pm".to_string()],
            "pushed paths must join the frontier the store's own query built, \
             not replace it"
        );
        assert!(
            core.repair_pushed.is_empty(),
            "the set must empty on adoption — otherwise every later pass \
             re-adopts the same paths and the frontier grows without bound"
        );

        // A second pass with nothing pushed adds nothing.
        let before = frontier.len();
        adopt_pushed_repairs(&core, &mut frontier);
        assert_eq!(frontier.len(), before, "an empty set must adopt nothing");
    }
}
