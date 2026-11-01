//! The one resolver loop: workspace-root wait, the dependency map, the
//! store and its warm, queue drain, and the per-name acquisition
//! (`acquire`), server and headless, for every served language.

use super::*;

/// The ONE resolver loop, server and headless alike. Mode differences are
/// explicit `server` gates inside this body — the per-name protocol
/// (locate, parse, persist, `land_dependency`) has a single spelling and
/// cannot drift between the two spawn fronts.
pub(super) fn resolver_loop(core: Arc<IndexCore>, server: Option<ServerSession>) {
    let served = crate::build::language_driver::LanguageRegistry::served();
    let ws_root = wait_for_workspace_root(&core);
    let root_path = ws_root.as_ref().and_then(|u| uri_to_path(u));
    // A map an indexer already read stands; otherwise the served
    // language's, for the workspace root.
    core.ensure_dependency_map(served, root_path.as_deref());
    let hub = core.map_snapshot().is_some_and(|s| s.driver.caps().hub_enrichment);

    let mut listed = core.list_available();
    let db = open_store(&core, server.as_ref(), ws_root.as_deref(), hub);

    // Track which extract version each module was resolved at.
    let mut seen: HashMap<String, i64> = HashMap::new();

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

    // Descent beyond parents and re-exports is a long-lived session's
    // prefetch, the Perl hub's only: a pack's wider descent waits on a
    // measurement. A one-shot run asks what its own files use and descends
    // as packs do, the same split as `Declared::Listed`: measured on FHEM
    // (php-stack-release-gates-2026-11-01), the prefetch landed 1,173
    // names instead of 303 and cost a cold `--check` 30 s; no diagnostic
    // moved, only the exporter lists a hint names grew.
    let prefetch = server.is_some() && hub;
    // Warm-copy strip is a long-lived-process behavior; one-shot CLI keeps
    // warm copies whole for wall (rehydration never amortizes), unless the
    // harness lane forces long-lived behavior on it.
    let strip_warm = core.long_lived.load(std::sync::atomic::Ordering::Relaxed)
        && eviction_enabled()
        && (server.is_some() || std::env::var("PERL_LSP_LONG_LIVED").as_deref() == Ok("1"));
    let mut adopted = 0u64;
    let mut progress = Progress::default();
    // Adopted before the first wait as well as after each, so what the map
    // declares is asked though nothing else is.
    adopt_map(&core, db.as_ref(), server.as_ref(), &mut seen, &mut adopted, &mut listed, &mut progress);
    core.queue.started.store(true, std::sync::atomic::Ordering::Release);
    notify_resolved(&core);

    // Main resolve loop — drain priority first, then pending.
    loop {
        let batch = drain_or_repair(&core, &core.queue, &mut repair_frontier, db.as_ref());
        adopt_map(&core, db.as_ref(), server.as_ref(), &mut seen, &mut adopted, &mut listed, &mut progress);

        // One diagnostics refresh per drained BATCH, not per module: a
        // resolve takes longer than the refresh debounce's settle window,
        // so per-module fires all survive debouncing and each one re-
        // enriches + republishes every open doc (measured: 939 resolves →
        // 346 full refresh bodies on one cold open). The queue keeps
        // accumulating while a batch runs, so the LAST non-empty batch
        // always ends with a fire — diagnostics converge once the queue
        // drains.
        let mut resolved_any = false;
        // Parsed a chunk at a time, in parallel, then landed one by one in
        // the order asked: the parse touches no shared state, the landing
        // is the serial half. The chunk bounds the whole analyses held
        // between the two.
        for chunk in batch.chunks(PARSE_CHUNK) {
            let mut todo: Vec<(String, bool)> = Vec::new();
            for module_name in chunk {
                let is_re_resolve = core.stale_modules.contains_key(module_name);
                // A warm load or a front door answered it since it was asked.
                if !is_re_resolve && matches!(core.cache.get(module_name).as_deref(), Some(Some(_))) {
                    continue;
                }
                // Allow re-resolution when extract version is outdated.
                if let Some(&ver) = seen.get(module_name) {
                    if ver >= module_cache::EXTRACT_VERSION {
                        continue;
                    }
                }
                seen.insert(module_name.clone(), module_cache::EXTRACT_VERSION);
                todo.push((module_name.clone(), is_re_resolve));
            }
            let snap = core.map_snapshot();
            // A current landing in the store answers before anything is
            // parsed; only the names it does not hold go to the parse.
            let mut answered: Vec<(String, bool, Result<Landed, MapMoved>)> = Vec::new();
            if let Some(s) = &snap {
                todo.retain(|(name, re)| {
                    if *re {
                        return true;
                    }
                    match acquire_warm(&core, s, name, db.as_ref(), prefetch, strip_warm) {
                        Some(landed) => {
                            answered.push((name.clone(), false, landed));
                            false
                        }
                        None => true,
                    }
                });
            }
            let placed: Vec<Vec<Placed>> = {
                use rayon::prelude::*;
                todo.par_iter()
                    .map(|(name, _)| snap.as_ref().map(|s| locate_and_parse(s, name)).unwrap_or_default())
                    .collect()
            };
            let parsed = todo.into_iter().zip(placed).filter_map(|((module_name, is_re_resolve), placed)| {
                // A landing earlier in this chunk answered it as a by-product.
                if !is_re_resolve && matches!(core.cache.get(&module_name).as_deref(), Some(Some(_))) {
                    return None;
                }
                if is_re_resolve {
                    log::info!("Re-resolving stale module '{}'", module_name);
                } else {
                    log::info!("Resolving module '{}'", module_name);
                }
                let landed = match &snap {
                    Some(snap) => acquire(&core, snap, &module_name, placed, db.as_ref(), prefetch),
                    None => Ok(Landed::Miss),
                };
                Some((module_name, is_re_resolve, landed))
            });
            for (module_name, _is_re_resolve, landed) in answered.into_iter().chain(parsed) {
                crate::util::ghost_stats::count("resolver.module_resolved");
                match landed {
                    Ok(Landed::Hit { providers, asks }) => {
                        log::info!("Resolved '{}': {} provider(s)", module_name, providers);
                        ask_unresolved(&core, &seen, asks);
                    }
                    Ok(Landed::Miss) => log::info!("Nothing provides '{}'", module_name),
                    Err(_moved) => {
                        // Asked again once the loop adopts the new map.
                        seen.remove(&module_name);
                        core.queue.ask([module_name]);
                        continue;
                    }
                }

                // Remove from stale set after re-resolution (no-op otherwise).
                core.stale_modules.remove(&module_name);

                // Report the prefetch's progress.
                if let Some(srv) = &server {
                    if progress.total > 0 && progress.done < progress.total {
                        progress.done += 1;
                        let pct = (progress.done * 100 / progress.total) as u32;
                        let token = NumberOrString::String("perl-lsp/indexing".to_string());
                        if progress.done < progress.total {
                            srv.handle.block_on(srv.client.send_notification::<notification::Progress>(
                                ProgressParams {
                                    token,
                                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                                        WorkDoneProgressReport {
                                            cancellable: Some(false),
                                            message: Some(format!("{} ({}/{})", module_name, progress.done, progress.total)),
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
                                            message: Some(format!("Indexed {} modules", progress.total)),
                                        },
                                    )),
                                },
                            ));
                        }
                    }
                }

                // Signal waiters (per module — bounded waits key on names).
                notify_resolved(&core);
                resolved_any = true;
            }
        }
        // After descent queued its asks, so "idle" never reads between the
        // batch and what it asked for.
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

/// Names parsed in parallel before their landings: bounds the whole
/// analyses a batch holds between the two.
const PARSE_CHUNK: usize = 32;

/// How far a session's prefetch of the declared dependencies has got.
#[derive(Default)]
struct Progress {
    total: usize,
    done: usize,
}

/// Adopt a new map: what was resolved under the old one may resolve
/// differently now, so its record of tries clears, the store drops what the
/// old map landed, the listing refreshes, and what the project declares is
/// asked — every `Declared::Loaded` in every session, a `Declared::Listed`
/// only where the session prefetches (a server; a one-shot run asks what
/// its files use, which is everything it runs).
fn adopt_map(
    core: &IndexCore,
    db: Option<&rusqlite::Connection>,
    server: Option<&ServerSession>,
    seen: &mut HashMap<String, i64>,
    adopted: &mut u64,
    listed: &mut u64,
    progress: &mut Progress,
) {
    use crate::build::language_driver::Declared;
    let generation = core.dependency_generation();
    if generation == *adopted {
        return;
    }
    *adopted = generation;
    seen.clear();
    let Some(snap) = core.map_snapshot() else { return };
    if let Some(conn) = db {
        let _ = module_cache::validate_dependency_fingerprint(conn, snap.fingerprint);
    }
    if generation != *listed {
        *listed = core.list_available();
    }
    let mut loaded: Vec<String> = Vec::new();
    let mut prefetched: Vec<String> = Vec::new();
    for d in snap.locator.declared_demand() {
        match d {
            Declared::Loaded(n) => loaded.push(n.clone()),
            Declared::Listed(n) if server.is_some() => prefetched.push(n.clone()),
            Declared::Listed(_) => {}
        }
    }
    prefetched.retain(|n| !core.cache.contains_key(n.as_str()));
    if let (Some(srv), false) = (server, prefetched.is_empty()) {
        log::info!("{} declared dependencies queued for indexing", prefetched.len());
        *progress = Progress { total: prefetched.len(), done: 0 };
        let token = NumberOrString::String("perl-lsp/indexing".to_string());
        let _ = srv.handle.block_on(srv.client.send_request::<request::WorkDoneProgressCreate>(
            WorkDoneProgressCreateParams { token: token.clone() },
        ));
        srv.handle.block_on(srv.client.send_notification::<notification::Progress>(ProgressParams {
            token,
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: "Indexing dependencies".into(),
                cancellable: Some(false),
                message: None,
                percentage: Some(0),
            })),
        }));
    }
    loaded.extend(prefetched);
    ask_unresolved(core, seen, loaded);
}

/// The served language's store, past its validity gates, with Perl's
/// builtin docs hydrated. Nothing warms here: a persisted landing is read
/// when its name is asked (`acquire`), through the one landing
/// (`IndexCore::land_dependency`), so a warm copy retires with its map like
/// a fresh one.
fn open_store(
    core: &IndexCore,
    server: Option<&ServerSession>,
    ws_root: Option<&str>,
    hub: bool,
) -> Option<rusqlite::Connection> {
    let snap = core.map_snapshot()?;
    let conn = module_cache::open_cache_db(ws_root, snap.driver.id())?;
    let _ = module_cache::validate_store(&conn, snap.driver);
    let _ = module_cache::validate_dependency_fingerprint(&conn, snap.fingerprint);
    if hub && server.is_some() {
        // Hydrate Perl builtin hover docs (cached in SQLite, re-parsed
        // from perlfunc.pod only when the perl version tag changes).
        // Server-only: a one-shot session would pay the cold parse for
        // hover docs it never serves.
        match module_cache::hydrate_builtins(&conn) {
            Ok(map) => {
                for entry in map.iter() {
                    core.builtins.insert(entry.key().clone(), entry.value().clone());
                }
            }
            Err(e) => log::warn!("Builtins hydrate failed: {}", e),
        }
    }
    Some(conn)
}

/// Queue `asks` that nothing answers yet and this map has not tried.
fn ask_unresolved(core: &IndexCore, seen: &HashMap<String, i64>, mut asks: Vec<String>) {
    asks.retain(|n| !core.cache.contains_key(n) && !seen.contains_key(n));
    if !asks.is_empty() {
        core.queue.ask(asks);
    }
}

fn notify_resolved(core: &IndexCore) {
    let _g = core.resolved.mu.lock().unwrap();
    core.resolved.cv.notify_all();
}

/// Drain the next batch from the queue, checking priority first.
///
/// Both checks live INSIDE the wait loop and `pending` is held across them.
/// `priority` has its own mutex, so — unlike a `pending` push — a priority
/// push is not serialized against this park by the condvar's mutex: a check
/// made before taking `pending` could be overtaken by a push whose notify
/// then reaches nobody, and the batch would sleep until unrelated `pending`
/// traffic arrived (forever, in an all-stale workload where every
/// `request_resolve` takes the priority branch). Producers close the other
/// half by notifying under `pending` (`ResolveQueue::notify_new_work`).
///
/// Lock order here is pending-then-priority; producers must never hold
/// `priority` while acquiring `pending`.
/// `drain_next_batch`, except the idle wait is spent re-baking conclusions.
///
/// Real work always wins: the queue is checked before every slice, so a resolve
/// request waits at most one slice (32 files) behind repair rather than behind
/// the whole frontier. When the frontier is empty this is exactly the old
/// blocking drain.
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
) -> Vec<String> {
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
fn try_drain_next_batch(queue: &ResolveQueue) -> Option<Vec<String>> {
    let mut pending = queue.pending.lock().unwrap();
    take_batch(queue, &mut pending)
}

pub(super) fn drain_next_batch(queue: &ResolveQueue) -> Vec<String> {
    let mut pending = queue.pending.lock().unwrap();
    loop {
        if let Some(batch) = take_batch(queue, &mut pending) {
            return batch;
        }
        pending = queue.condvar.wait(pending).unwrap();
    }
}

/// Take the next batch, the caller holding `pending`, and mark the queue
/// busy as it goes (`ResolveQueue::busy`). A moved map is an empty batch,
/// so the loop adopts it with nothing asked.
fn take_batch(queue: &ResolveQueue, pending: &mut Vec<String>) -> Option<Vec<String>> {
    let batch = {
        let mut priority = queue.priority.lock().unwrap();
        if !priority.is_empty() {
            Some(std::mem::take(&mut *priority))
        } else if !pending.is_empty() {
            Some(std::mem::take(pending))
        } else if queue.map_moved.swap(false, std::sync::atomic::Ordering::AcqRel) {
            Some(Vec::new())
        } else {
            None
        }
    };
    if batch.is_some() {
        queue.busy.store(true, std::sync::atomic::Ordering::Release);
    }
    batch
}

// ---- Internal helpers ----

/// The workspace root from initialize(), for the per-project store and the
/// map. An indexer that already read a map has said where the project is,
/// so that ends the wait too.
fn wait_for_workspace_root(core: &IndexCore) -> Option<String> {
    let ws_root_channel = &core.workspace_root;
    let mut guard = ws_root_channel.root.lock().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while guard.is_none() && core.dependency_state().is_none() {
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
