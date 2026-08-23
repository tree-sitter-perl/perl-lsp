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
            module_cache::CONCLUSION_FINGERPRINT,
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
        // Build reverse index from warmed cache.
        core.rebuild_reverse_index();
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

    // Main resolve loop — drain priority first, then pending.
    loop {
        let batch = drain_next_batch(&core.queue);

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
