//! The one resolver loop: workspace-root wait, SQLite warm, queue
//! drain, and the per-module resolve protocol (server and headless).

use super::*;

/// The ONE resolver loop, server and headless alike. Mode differences are
/// explicit `server` gates inside this body — the per-module resolve
/// protocol (memoization, persistence, strip, `insert_resolved`) has a
/// single spelling and cannot drift between the two spawn fronts.
pub(super) fn resolver_loop(core: Arc<IndexCore>, server: Option<ServerSession>) {
    let mut inc_paths = discover_inc_paths();

    // Wait for workspace root from initialize() for per-project cache path.
    let ws_root = wait_for_workspace_root(&core.workspace_root);

    // Auto-discover project-local lib paths (lib/, local/lib/perl5/).
    if let Some(root_path) = ws_root.as_ref().and_then(|u| uri_to_path(u)) {
        add_project_lib_paths(&mut inc_paths, &root_path);
    }

    // Publish the resolved search path before anything can query: an
    // origin's visibility rank prefix-matches candidates against it, and
    // discovery shells out to `perl`, so no request path may re-derive it.
    core.set_inc_roots(&inc_paths);

    // Scan @INC for available module names (fast, no parsing — just readdir)
    scan_inc_module_names(&inc_paths, &core.available_modules);
    log::info!("@INC scan: {} modules available", core.available_modules.len());

    // Warm the in-memory cache from SQLite.
    let db = module_cache::open_cache_db(ws_root.as_deref(), "perl");
    if let Some(ref conn) = db {
        let _ = module_cache::validate_inc_paths(conn, &inc_paths);
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
        // Warm-copy strip is a long-lived-server behavior; one-shot CLI
        // keeps warm copies whole for wall (rehydration never amortizes).
        let strip_warm = server.is_some()
            && core.long_lived.load(std::sync::atomic::Ordering::Relaxed)
            && eviction_enabled();
        let (n, stale_names) = module_cache::warm_cache(conn, &core.cache, &core.all_defs, strip_warm);
        log::info!("Warmed module cache: {} entries loaded from disk, {} stale", n, stale_names.len());
        // Stamp generations for the warm-loaded @INC providers (they
        // landed in the cache without a registration front door).
        core.stamp_missing_import_gens();
        for name in &stale_names {
            core.stale_modules.insert(name.clone(), ());
        }
        // Server sessions re-resolve stale modules eagerly; headless ones
        // re-resolve on demand (`request_resolve` queues stale names with
        // priority).
        if server.is_some() && !stale_names.is_empty() {
            {
                let mut pq = core.queue.priority.lock().unwrap();
                pq.extend(stale_names);
            }
            core.queue.notify_new_work();
        }
        // Rebuild the reverse index from the warmed cache — only when the
        // warm load brought entries in without feeding the edges. A core
        // fed purely by registrations (a pack sub-index; a cold start) has
        // nothing to re-derive, and a rebuild is not free to readers: its
        // clear-then-refeed window empties every bucket, and this thread
        // wakes lazily, so on a one-shot CLI the window lands under the
        // diagnostics sweep.
        if n > 0 {
            core.rebuild_reverse_index();
        }
    }

    // Track which extract version each module was resolved at.
    let mut seen: HashMap<String, i64> = HashMap::new();

    // One parser + one parent-fallback memo for the whole sweep.
    // Without the memo, every child whose own exports are empty re-parses
    // its parent (e.g. ~50× Exporter, ~30× URI on a cold cpanfile run).
    let mut parser = create_parser();
    let mut parse_memo: ParseMemo = HashMap::new();

    // Queue cpanfile dependencies (non-blocking — lets priority items go first).
    // Track total for progress reporting in the main loop. Server-only: a
    // one-shot session must not burn its wall resolving the dep tree in the
    // background.
    let mut cpanfile_total = 0usize;
    let mut cpanfile_done = 0usize;
    if let Some(srv) = &server {
        if let Some(root_path) = ws_root.as_ref().and_then(|u| uri_to_path(u)) {
            let cpanfile_modules = cpanfile::parse_cpanfile(&root_path);
            let to_resolve: Vec<String> = cpanfile_modules
                .into_iter()
                .filter(|m| !core.cache.contains_key(m.as_str()))
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

                let mut pending = core.queue.pending.lock().unwrap();
                pending.extend(to_resolve);
                core.queue.condvar.notify_one();
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

    // Main resolve loop — drain priority first, then pending.
    loop {
        let batch = drain_or_repair(&core, &core.queue, &mut repair_frontier, db.as_ref());

        // One diagnostics refresh per drained BATCH, not per module: a
        // resolve takes longer than the refresh debounce's settle window,
        // so per-module fires all survive debouncing and each one re-
        // enriches + republishes every open doc (measured: 939 resolves →
        // 346 full refresh bodies on one cold open). The queue keeps
        // accumulating while a batch runs, so the LAST non-empty batch
        // always ends with a fire — diagnostics converge once the queue
        // drains.
        let mut resolved_any = false;
        for module_name in batch {
            // Allow re-resolution when extract version is outdated.
            if let Some(&ver) = seen.get(&module_name) {
                if ver >= module_cache::EXTRACT_VERSION {
                    continue;
                }
            }
            seen.insert(module_name.clone(), module_cache::EXTRACT_VERSION);

            let is_re_resolve = core.stale_modules.contains_key(&module_name);
            if is_re_resolve {
                log::info!("Re-resolving stale module '{}'", module_name);
                // Stale entry must not be served from the run-local memo.
                parse_memo.remove(&module_name);
            } else {
                log::info!("Resolving module '{}'", module_name);
            }

            crate::util::ghost_stats::count("resolver.module_resolved");
            let result = parse_module(&inc_paths, &module_name, &mut parser, &mut parse_memo);
            match &result {
                Some(providers) => log::info!(
                    "Resolved '{}': {} provider(s), {} export, {} export_ok",
                    module_name,
                    providers.len(),
                    providers[0].analysis.export.len(),
                    providers[0].analysis.export_ok.len()
                ),
                None => log::info!("No exports found for '{}'", module_name),
            }
            // Persistence is per FILE: each provider gets its own row, so a
            // shadowed twin survives the warm start too.
            let persisted = db
                .as_ref()
                .map(|conn| save_module_generation(conn, &module_name, &result))
                .unwrap_or(false);
            // The one spelling of "a resolution landed": stale-pin clear +
            // generation mint + whole-analysis projections + registration-
            // owned strip + the None-never-clobbers guard, all inside
            // `insert_resolved`.
            let stored = core.insert_resolved(
                &module_name,
                result.clone(),
                persisted,
                eviction_enabled(),
            );
            // The memo would otherwise pin the WHOLE closure for the
            // thread's lifetime — a second copy of the tier. Failed
            // resolves are NOT memoized (pre-existing semantics: the
            // parent-fallback re-probes, catching mid-session
            // installs).
            match &stored {
                Some(_) => {
                    parse_memo.insert(module_name.clone(), stored.clone());
                }
                None => {
                    parse_memo.remove(&module_name);
                }
            }

            // Descend into the module's own dependencies so the
            // chain keeps resolving beyond the open doc's direct
            // imports. Without this the cache stops at depth 1 —
            // e.g. opening a Mojolicious::Lite script resolves
            // Mojolicious.pm, but Mojolicious.pm's
            // `has routes => sub { Mojolicious::Routes->new }`
            // never triggers a resolve on Mojolicious::Routes,
            // and `$r->get` on line 71 of the demo chain-dies
            // because the intermediate class is a cache miss.
            //
            // The `seen` guard above makes this cycle-safe: a
            // transitively-enqueued name that was already
            // resolved at the current EXTRACT_VERSION gets
            // skipped on its next turn. Server-only: a one-shot
            // session resolves exactly what its query asks for.
            if server.is_some() {
                if let Some(ref providers) = result {
                    let mut pending = core.queue.pending.lock().unwrap();
                    let enqueue = |pending: &mut Vec<String>, name: String| {
                        if name.is_empty() { return; }
                        if core.cache.contains_key(&name) { return; }
                        if seen.contains_key(&name) { return; }
                        if !pending.iter().any(|p| p == &name) {
                            pending.push(name);
                        }
                    };
                    // Every provider's deps, not just the winner's: a
                    // shadowed twin has its own `use` list and `@ISA`, and a
                    // class only it names must still resolve.
                    for m in providers {
                    // Explicit imports — the module's own `use` statements.
                    for imp in &m.analysis.imports {
                        enqueue(&mut pending, imp.module_name.clone());
                    }
                    // Re-export edges — a re-exporting module (Test::Most →
                    // Test::More) pulls its producers' surfaces transitively,
                    // so those producers must be resolved even when no file
                    // `use`s them directly.
                    for re in &m.analysis.reexport_modules {
                        enqueue(&mut pending, re.clone());
                    }
                    // Parent classes — inheritance chain.
                    for (_pkg, parents) in m.analysis.package_parent_edges() {
                        for parent in parents {
                            enqueue(&mut pending, parent.clone());
                        }
                    }
                    // ClassName return types — `has foo => sub { Bar->new }`,
                    // plugin-emitted typed Subs, method return annotations.
                    // These are the chain-invisible-but-reachable classes
                    // the user's chain walks through at query time.
                    for sym in m.analysis.symbols() {
                        use crate::model::file_analysis::{InferredType, SymKind, SymbolDetail};
                        if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                        if !matches!(sym.detail, SymbolDetail::Sub { .. }) { continue; }
                        if let Some(InferredType::ClassName(c)) =
                            m.analysis.symbol_return_type_via_bag(sym.id, None)
                        {
                            enqueue(&mut pending, c);
                        }
                    }
                    }
                    if !pending.is_empty() {
                        core.queue.condvar.notify_one();
                    }
                }
            }

            // Remove from stale set after re-resolution (no-op otherwise).
            core.stale_modules.remove(&module_name);

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
                                        message: Some(format!("{} ({}/{})", module_name, cpanfile_done, cpanfile_total)),
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

            // Signal waiters (per module — bounded waits key on names).
            {
                let _g = core.resolved.mu.lock().unwrap();
                core.resolved.cv.notify_all();
            }
            resolved_any = true;
        }
        if resolved_any {
            if let Some(srv) = &server {
                crate::util::ghost_stats::count("resolver.on_resolved_fired");
                (srv.on_resolved)();
            }
        }
    }
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
    {
        let mut priority = queue.priority.lock().unwrap();
        if !priority.is_empty() {
            return Some(std::mem::take(&mut *priority));
        }
    }
    if !pending.is_empty() {
        return Some(std::mem::take(&mut *pending));
    }
    None
}

pub(super) fn drain_next_batch(queue: &ResolveQueue) -> Vec<String> {
    let mut pending = queue.pending.lock().unwrap();
    loop {
        {
            let mut priority = queue.priority.lock().unwrap();
            if !priority.is_empty() {
                return std::mem::take(&mut *priority);
            }
        }
        if !pending.is_empty() {
            return std::mem::take(&mut *pending);
        }
        pending = queue.condvar.wait(pending).unwrap();
    }
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
