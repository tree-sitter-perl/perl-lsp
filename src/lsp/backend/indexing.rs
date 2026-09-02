//! Lazy per-family workspace indexing, the cold-open bounded waits
//! (`await_*`), and the shared work-done-progress spellings.

use super::*;

impl Backend {
    /// Fire-and-forget re-index for one saved file.
    ///
    /// Spawned rather than awaited, the same shape `schedule_pack_invalidate`
    /// uses and for the same reason: tower-lsp runs notifications on one task,
    /// so awaiting a re-parse + re-register inside `did_save` would hold every
    /// following didChange behind it — and a save is the most frequent trigger
    /// this path has.
    pub(super) fn spawn_reindex_saved_perl(&self, path: PathBuf) {
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let ctx = self.diag_ctx();
        tokio::spawn(async move {
            let dirty = Self::reindex_perl_blocking(
                files,
                module_index,
                vec![(path, FileChangeType::CHANGED)],
            )
            .await;
            if !dirty.is_empty() {
                ctx.republish_dirty(&dirty).await;
            }
        });
    }

    /// Re-index changed Perl files and refresh whoever depended on them,
    /// awaiting the result. `didChangeWatchedFiles` uses this; a save uses the
    /// spawned twin above.
    pub(super) async fn reindex_saved_perl(
        &self,
        perl_changes: Vec<(PathBuf, FileChangeType)>,
    ) {
        let dirty = Self::reindex_perl_blocking(
            Arc::clone(&self.files),
            Arc::clone(&self.module_index),
            perl_changes,
        )
        .await;
        // Off-pipeline: each republish re-enriches.
        self.spawn_republish(dirty);
    }

    /// Re-index Perl files whose bytes on disk changed, and name everyone who
    /// must be refreshed because of it.
    ///
    /// Two seams reach this, and for a while only one of them existed.
    /// `didChangeWatchedFiles` is the obvious one — and it is the one that does
    /// not fire when the editor saves a buffer it has open, which is how a
    /// developer actually edits a dependency. `did_save` is the other, and its
    /// absence meant that saving a module left every consumer in the session
    /// answering from the version before the save, for the rest of the
    /// session. The pack languages were given this (`schedule_pack_invalidate`,
    /// the H1 fix); Perl was not. `e2e/saved_dep_edit.lua` is the guard.
    ///
    /// Over Arcs only, so the spawned caller can move it into a task.
    async fn reindex_perl_blocking(
        files: Arc<crate::index::file_store::FileStore>,
        module_index: Arc<ModuleIndex>,
        perl_changes: Vec<(PathBuf, FileChangeType)>,
    ) -> std::collections::HashSet<PathBuf> {
        if perl_changes.is_empty() {
            return Default::default();
        }
        tokio::task::spawn_blocking(move || {
            // Externally changed deps break their consumers' enrichment too
            // — collect the dirty closure while the records are in hand and
            // hand it back for the open-doc republish below.
            let mut dirty_all: std::collections::HashSet<PathBuf> = Default::default();
            // Each changed file's fresh map, and where the refresh wave
            // starts. They differ for a DELETED file: it has no map and
            // nothing to evaluate, so it enters the wave as its direct
            // consumers instead.
            let mut fresh: Vec<crate::index::conclusion_flush::FreshBake> = Vec::new();
            let mut frontier: Vec<PathBuf> = Vec::new();
            // The persisted generation (blob + ref rows) is now stale for
            // these paths; drop it so warm starts re-parse and the
            // relational retrieval can't serve outdated spans. The fresh
            // in-RAM copy registered below is FULL (never stripped), so the
            // resident sweep covers it until the next bulk index persists a
            // new generation.
            let ws_key = module_index.workspace_root();
            let conn = crate::index::module_cache::open_cache_db(ws_key.as_deref(), "perl");
            for (path, typ) in perl_changes {
                // A DELETED file can't canonicalize (it's gone) — resolve the
                // parent instead so the spelling still matches the canonical
                // keys everything was registered/persisted under.
                let canon = path.canonicalize().unwrap_or_else(|_| {
                    match (path.parent(), path.file_name()) {
                        (Some(dir), Some(name)) => std::fs::canonicalize(dir)
                            .map(|d| d.join(name))
                            .unwrap_or_else(|_| path.clone()),
                        _ => path.clone(),
                    }
                });
                if let Some(ref conn) = conn {
                    crate::index::module_cache::invalidate_generation(conn, &canon.to_string_lossy());
                    if canon != path {
                        crate::index::module_cache::invalidate_generation(
                            conn,
                            &path.to_string_lossy(),
                        );
                    }
                }
                module_index.invalidate_derived_copies(&canon);
                match typ {
                    FileChangeType::DELETED => {
                        files.remove_workspace(&path);
                        files.remove_workspace(&canon);
                        // Consumers of the departed file's packages, BEFORE
                        // the record (and its provided names) are removed.
                        dirty_all.extend(module_index.dirty_consumers(&canon));
                        // Consumers that answered through the departed file
                        // now resolve to nothing — a move, and the wave
                        // carries it onward from them.
                        frontier.extend(module_index.dirty_consumers(&canon));
                        // The hub's path/name registrations must go too, or
                        // the dead file stays a retrieval candidate and a
                        // phantom module in name lookups.
                        module_index.unregister_workspace_path(&canon);
                    }
                    _ => {
                        // Re-index the file (created or changed). The fresh
                        // copy registers WHOLE (refs + bag) in both stores:
                        // its persisted generation was just invalidated, so
                        // the resident copy is the only source until the
                        // next bulk index re-persists.
                        if let Ok(source) = std::fs::read_to_string(&path) {
                            let mut parser = crate::index::module_resolver::create_parser();
                            if let Some(tree) = parser.parse(&source, None) {
                                let analysis = crate::build::builder::build(&tree, source.as_bytes());
                                let arc = Arc::new(analysis);
                                files.insert_workspace_arc(canon.clone(), arc.clone());
                                module_index.record_workspace_projections(&canon, &arc);
                                // register_workspace_resident routes through
                                // record_and_dirty: the dirty set is bound to
                                // the record, so a re-register can't drop it.
                                let sd = module_index
                                    .register_workspace_resident(canon.clone(), arc.clone());
                                dirty_all.extend(sd.dirty);
                                // The caller bakes: the analysis is in hand
                                // and its blob was just invalidated, so the
                                // flush has nothing to decode it from.
                                fresh.push(
                                    crate::index::conclusion_flush::FreshBake {
                                        path: canon.clone(),
                                        map: crate::index::module_cache::bake_conclusion_map(
                                            &arc,
                                            &arc.witnesses,
                                        ),
                                        // Projected from the SAME analysis the
                                        // map was baked from. For a path that
                                        // is ALSO open, the freshness index
                                        // holds the buffer's fingerprint
                                        // instead, so this row reads absent —
                                        // correctly, since consumers of an
                                        // open file read its buffer.
                                        source_fingerprint:
                                            crate::model::surface::surface_fingerprint(
                                                &crate::model::surface::Surface::project(&arc),
                                            ),
                                    },
                                );
                                frontier.push(canon.clone());
                            }
                        }
                    }
                }
            }
            // The refresh wave. `dirty_consumers` names DIRECT consumers only,
            // so a change two hops away has never refreshed anything — the
            // wave carries it as far as the answers actually move and stops
            // there, which is the bound a transitive closure would not have.
            // Additive to the one-hop set rather than replacing it: a file
            // whose surface did not move can still need re-enrichment for
            // reasons this layer does not model.
            if let Some(ref conn) = conn {
                let candidates_of = |class: &str| -> Vec<PathBuf> {
                    use crate::model::file_analysis::CrossFileLookup;
                    module_index
                        .visible_def_candidates(class)
                        .iter()
                        .map(|c| c.path.clone())
                        .collect()
                };
                let consumers_of = |p: &std::path::Path| -> Vec<PathBuf> {
                    module_index.dirty_consumers(p).into_iter().collect()
                };
                let out = crate::util::timings::phase("flush.wave", || {
                    crate::index::conclusion_flush::flush_refresh_set(
                        conn,
                        fresh,
                        frontier,
                        &consumers_of,
                        &candidates_of,
                    )
                });
                crate::util::ghost_stats::count_by(
                    "flush.refresh_set",
                    out.changed.len() as u64,
                );
                // The push half of the re-stamp gate: every consumer this
                // wave enqueued has had a provider move, so its next
                // enrichment owes a re-stamp. One epoch for the whole wave —
                // a stamp taken during it must not look newer than the wave
                // that caused it.
                if !out.non_convergent {
                    module_index.mark_provider_diff(out.enqueued.iter().cloned());
                }
                dirty_all.extend(out.changed.into_iter().map(|(p, _)| p));
            }
            dirty_all
        })
        .await
        .unwrap_or_default()
    }
    pub(super) fn diagnostic_options(&self) -> symbols::DiagnosticOptions {
        *self.diag_options.lock().unwrap()
    }

    /// The configured method-override fan-out scope for references + rename.
    pub(super) fn override_scope(&self) -> crate::index::resolve::OverrideScope {
        self.rename_options.lock().unwrap().override_scope
    }

    /// Index the opened file's language FAMILY's workspace, once, in the
    /// background. `perl` → the `.pm/.pl/.t` scan; any pack language (C++/
    /// Python/…) → the pack-language scan. Latched per family so a C++-only
    /// session never touches the perl tree, and vice versa.
    pub(super) fn ensure_workspace_indexed(&self, language: &str) {
        use std::sync::atomic::Ordering;
        let want_perl = language == "perl";
        let latch = if want_perl { &self.perl_indexed } else { &self.pack_indexed };
        if latch.swap(true, Ordering::Relaxed) {
            return; // already indexed (or in flight)
        }
        let files = Arc::clone(&self.files);
        let client = self.client.clone();
        let module_index = Arc::clone(&self.module_index);
        let root = self.module_index.workspace_root();
        // Server-initiated progress requires the client capability; a client
        // that never advertised it may also never ANSWER the create request —
        // and indexing must proceed regardless (LSP spec).
        let progress = self
            .work_done_progress
            .load(std::sync::atomic::Ordering::Relaxed);
        let index_ready = Arc::clone(&self.index_ready);
        let heal_ctx = self.pack_heal_ctx();
        let bag_cache_bytes =
            self.max_cache_mb.load(std::sync::atomic::Ordering::Relaxed) as usize * 1024 * 1024;
        // H9-2: mark the pack index in flight BEFORE it is scheduled, so a save
        // racing the scheduling defers into the invalidator's reconcile set
        // instead of hitting an unattached (no-op) invalidation. Perl uses the
        // direct re-index path and never touches the invalidator.
        let pack_invalidator = (!want_perl).then(|| Arc::clone(&self.pack_invalidator));
        if let Some(ref inv) = pack_invalidator {
            inv.begin_bulk_index();
        }
        tokio::task::spawn_blocking(move || {
            // Announces completion (or the no-root early-out) to bounded waiters
            // on Drop — every exit path of this closure, panic included.
            let _done = IndexDoneGuard { ready: index_ready, want_perl };
            // Every perl exit path below opens the gate BEFORE running the
            // heal sweep (`open_perl_gate_then_heal`): a `publish_diagnostics`
            // that read "index in flight" and deferred is guaranteed its doc
            // is already in the store when the sweep collects — the doc
            // insert precedes the publish, and the publish's gate check
            // precedes open(). Without that order a doc opened between the
            // sweep's collection and the guard's open() would defer into a
            // window nobody republishes.
            let Some(root_uri) = root else {
                // Pack keeps its early-out shape (no heal without a root);
                // perl must still sweep so a publish deferred against the
                // kickoff-to-early-out window converges.
                if want_perl {
                    Self::open_perl_gate_then_heal(&_done, &heal_ctx, true);
                }
                return;
            };
            let Some(root_path) = root_uri.strip_prefix("file://") else {
                if want_perl {
                    Self::open_perl_gate_then_heal(&_done, &heal_ctx, true);
                }
                return;
            };
            let root_path = PathBuf::from(root_path);
            let rt = tokio::runtime::Handle::current();
            let token = NumberOrString::String(format!(
                "perl-lsp/workspace-index-{}",
                if want_perl { "perl" } else { "pack" }
            ));
            if progress {
                // tower-lsp holds the server→client request's oneshot SENDER in
                // its pending map until the reply lands, and panics ("receiver
                // already dropped") if that reply arrives after we dropped the
                // RECEIVER. A bare `timeout(.., send_request)` drops the receiver
                // on timeout, so a slow client's late `create` reply would take
                // the whole server down (#36). Spawn the request onto a DETACHED
                // task instead: dropping its `JoinHandle` on timeout leaves the
                // task — and its receiver — alive, so a late reply routes to a
                // live receiver (a harmless `Ok`) rather than panicking. The 2s
                // cap only bounds how long we wait; indexing must proceed even if
                // a capable-but-slow client never answers.
                let create = rt.spawn({
                    let client = client.clone();
                    let token = token.clone();
                    async move {
                        let _ = client
                            .send_request::<request::WorkDoneProgressCreate>(
                                WorkDoneProgressCreateParams { token },
                            )
                            .await;
                    }
                });
                let _ = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    create,
                ));
                rt.block_on(client.send_notification::<notification::Progress>(ProgressParams {
                    token: token.clone(),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                        WorkDoneProgressBegin {
                            title: "Indexing workspace".into(),
                            cancellable: Some(false),
                            message: Some("Scanning files...".into()),
                            percentage: Some(0),
                        },
                    )),
                }));
            }
            // Throttled percentage progress. The Rayon index workers call `cb`
            // per file (cheap: an atomic `fetch_max` guard); only a ≥2% advance
            // (or the final tick) crosses the channel, where a tokio task owns
            // the actual `Report` notification. This keeps `send_notification`
            // OFF the Rayon worker threads — no `block_on` from the pool — and
            // bounds emissions to ~50 per index regardless of file count.
            let emitter = progress.then(|| {
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<(u32, usize, usize)>();
                let client_e = client.clone();
                let token_e = token.clone();
                let handle = rt.spawn(async move {
                    while let Some((pct, done, total)) = rx.recv().await {
                        client_e
                            .send_notification::<notification::Progress>(ProgressParams {
                                token: token_e.clone(),
                                value: ProgressParamsValue::WorkDone(
                                    WorkDoneProgress::Report(WorkDoneProgressReport {
                                        cancellable: Some(false),
                                        message: Some(format!("{done}/{total} files")),
                                        percentage: Some(pct),
                                    }),
                                ),
                            })
                            .await;
                    }
                });
                (tx, handle)
            });
            let last_pct = std::sync::atomic::AtomicU8::new(0);
            let cb = emitter.as_ref().map(|(tx, _)| {
                let tx = tx.clone();
                move |done: usize, total: usize| {
                    let pct = if total == 0 {
                        100u8
                    } else {
                        ((done * 100 / total).min(100)) as u8
                    };
                    let prev = last_pct.fetch_max(pct, std::sync::atomic::Ordering::Relaxed);
                    if pct >= prev.saturating_add(2) || done >= total {
                        let _ = tx.send((pct as u32, done, total));
                    }
                }
            });
            let cb_ref: Option<&(dyn Fn(usize, usize) + Sync)> =
                cb.as_ref().map(|c| c as &(dyn Fn(usize, usize) + Sync));
            let count = if want_perl {
                // Once the walk finishes, the persist writer may keep
                // draining for minutes on a large tree — announce that phase
                // instead of sitting at 100% looking hung.
                let walk_done = progress.then(|| {
                    let client = client.clone();
                    let token = token.clone();
                    let rt = rt.clone();
                    move || {
                        rt.block_on(client.send_notification::<notification::Progress>(
                            ProgressParams {
                                token: token.clone(),
                                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                                    WorkDoneProgressReport {
                                        cancellable: Some(false),
                                        message: Some(
                                            "Saving index to cache...".into(),
                                        ),
                                        percentage: Some(100),
                                    },
                                )),
                            },
                        ));
                    }
                });
                let walk_done_ref: Option<&(dyn Fn() + Sync)> =
                    walk_done.as_ref().map(|c| c as &(dyn Fn() + Sync));
                crate::index::module_resolver::index_workspace_with_index(
                    &root_path,
                    &files,
                    Some(&module_index),
                    cb_ref,
                    walk_done_ref,
                )
            } else {
                crate::index::module_resolver::index_pack_languages(
                    &root_path,
                    Some(root_uri.as_str()),
                    &module_index,
                    cb_ref,
                    bag_cache_bytes,
                    // The server's latch is already per-FAMILY; inside the
                    // pack family it indexes every pack language, as before.
                    &crate::build::language_driver::LanguageScope::All,
                )
            };
            // Everything from here to `open_perl_gate_then_heal` is still
            // inside the readiness gate, so it is part of the "100% walked,
            // still not ready" window even though the walk and the writer are
            // both done. Timed as one segment because the window's SIZE has
            // been attributed to the writer drain, and an attribution is only
            // worth anything if the other terms were measured rather than
            // assumed.
            let _to_gate = crate::util::timings::PhaseGuard::start("index.indexer_return_to_gate");
            // Drop the sender(s) so the emitter's channel closes, then drain it
            // — guarantees the final Report lands before End.
            drop(cb);
            if let Some((tx, handle)) = emitter {
                drop(tx);
                let _ = rt.block_on(handle);
            }
            if progress {
                rt.block_on(client.send_notification::<notification::Progress>(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                        WorkDoneProgressEnd {
                            message: Some(if want_perl {
                                format!("Indexed {} Perl files", count)
                            } else {
                                let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
                                let langs: Vec<&str> = reg
                                    .pack_drivers()
                                    .map(|d| crate::build::language_driver::LanguageRegistry::display_name(d.id()))
                                    .collect();
                                format!("Indexed {} {} files", count, langs.join("/"))
                            }),
                        },
                    )),
                }));
            }
            // H9-2: the pack sub-indexes are now attached — the invalidator
            // reconciles every save deferred during the index exactly once
            // (its own lock + H9-1 generation guard; open docs are covered by
            // `heal_open_docs` below, so no per-path refresh set is needed).
            if let Some(inv) = pack_invalidator {
                inv.finish_bulk_index(Some(root_uri.as_str()), &module_index);
            }
            // Heal the cold-open degraded window: the index this file's family
            // needs has now ATTACHED (the latch marked KICKOFF; this is the
            // completion signal). Re-analyze + re-publish every open doc in the
            // family so pull-verb answers baked in the cached-only open window
            // (truncated cross-file closure, `None` gd/hover) self-heal without
            // the user re-triggering.
            Self::open_perl_gate_then_heal(&_done, &heal_ctx, want_perl);
        });
    }

    /// Perl: open the ready gate, THEN run the heal sweep — the order that
    /// makes deferred publishes race-free (see the comment at the guard).
    /// Costs pull-verb waiters a slightly earlier wakeup (they answer from
    /// the raw open doc while the sweep's enrichment is still running — the
    /// same degraded window the deferral accepts). Pack keeps the guard's
    /// drop-time open: its degraded window is owned by `degraded_open`.
    fn open_perl_gate_then_heal(done: &IndexDoneGuard, ctx: &PackHealCtx, want_perl: bool) {
        if want_perl {
            done.ready.perl.open();
        }
        Self::heal_open_docs(ctx, want_perl);
    }

    /// Re-derive + re-publish every OPEN document in a language family after its
    /// workspace index / macro gather lands — the pull-verb heal for the
    /// cold-open degraded window. Pack docs get a full OFF-lock re-analysis
    /// (their `did_open` gather was cached-only + the cross-file index is now
    /// warm); perl docs re-derive enrichment + diagnostics through
    /// `refresh_open_diagnostics` (URIs collected under the read iterator,
    /// each doc derived with no store guard held).
    fn heal_open_docs(ctx: &PackHealCtx, want_perl: bool) {
        log::debug!(
            "cold-window heal: index landed for {} family",
            if want_perl { "perl" } else { "pack" }
        );
        if want_perl {
            let pending = refresh_open_diagnostics(
                &ctx.files,
                &ctx.module_index,
                ctx.options,
                OpenDocScope::PerlFamily,
                false, // Perl family only: no pack doc derives here
            );
            if pending.is_empty() {
                return;
            }
            let client = ctx.client.clone();
            tokio::spawn(async move {
                for (uri, diags) in pending {
                    client.publish_diagnostics(uri, diags, None).await;
                }
            });
        } else {
            let mut uris: Vec<Url> = Vec::new();
            ctx.files.for_each_open(|uri, doc| {
                // Only gather-dependent docs have a cold-open window to heal
                // (a context-free analyze was already full quality).
                if crate::build::language_driver::LanguageRegistry::caps(doc.language)
                    .context_gather
                {
                    uris.push(uri.clone());
                }
            });
            // Route each through the single-flight registry: a doc already
            // mid-gather coalesces instead of double-gathering its cone.
            for uri in uris {
                ctx.request_gather(uri);
            }
        }
    }

    /// Bounded wait for the opened file's language-family workspace/pack index
    /// to finish, when — and ONLY when — it is actually in-flight: KICKED OFF
    /// (`ensure_workspace_indexed` flipped the latch at `did_open`) but not yet
    /// DONE. This closes the residual cold-open window for PULL verbs
    /// (goto-def / hover / references): unlike completion (`isIncomplete`) and
    /// diagnostics (server re-push), a one-shot gd/hover the user fired in the
    /// window got its degraded answer and is gone. Blocking the handler briefly
    /// for the imminent index lets it resolve against the warm cross-file index
    /// instead (e.g. references `op_free` 1 → 118).
    ///
    /// Zero added latency in the common cases: the warm session (index already
    /// `done` → returns before awaiting) and the no-index case (latch never set).
    /// Bounded by `cold_wait_ms` (0 opts out) so it can never wedge, and on
    /// timeout the handler resolves degraded exactly as before.
    ///
    /// GUARD DISCIPLINE: holds NO FileStore guard across the await — it touches
    /// only the family's `ReadyGate`. Callers peek `language` under
    /// a `get_open` guard that DROPS before this await, and snapshot `analysis`
    /// fresh AFTER it, picking up any heal (see the hazard note on
    /// `FileStore::for_each_open`).
    /// `WaitPolicy` → millisecond cap. `cold_wait_ms == 0` is the global
    /// "never block" opt-out and wins over any policy.
    fn wait_cap(&self, policy: WaitPolicy) -> u64 {
        let interactive = self
            .cold_wait_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        match policy {
            _ if interactive == 0 => 0,
            WaitPolicy::Interactive => interactive,
            // Generous ceiling: bounded (a wedged index can't hang the verb
            // forever) but far beyond any real build/index time.
            WaitPolicy::Complete => 120_000,
        }
    }

    pub(super) async fn await_index_ready(&self, language: &str, policy: WaitPolicy) {
        use std::sync::atomic::Ordering;
        let want_perl = language == "perl";
        let latch = if want_perl { &self.perl_indexed } else { &self.pack_indexed };
        let gate = if want_perl { &self.index_ready.perl } else { &self.index_ready.pack };
        // Only wait when an index is actually coming: kicked off but not done.
        if !latch.load(Ordering::Relaxed) || gate.is_open() {
            return;
        }
        let cap = self.wait_cap(policy);
        if cap == 0 {
            return; // opt-out
        }
        if let Some(waited) = gate.armed_wait(|| false) {
            self.bounded_wait_with_progress(cap, waited, "Waiting for workspace index")
                .await;
        }
    }

    /// Bounded wait for a freshly-opened document's INITIAL build, when it is
    /// still in flight (`did_open` runs the build off the message loop so the
    /// loop stays responsive during the ~1.3 s cold build of a big C file). A
    /// read verb calls this before `get_open`: a small/medium file (build <
    /// cap) resolves warm on the first pull exactly as before; a pathological
    /// file degrades after the cap and heals once the build lands + republishes.
    ///
    /// GUARD DISCIPLINE: holds NO FileStore / DashMap guard across the await —
    /// it snapshots the `ReadyGate` Arc out of the `opening` map and drops that
    /// guard before awaiting. Callers snapshot `analysis` fresh AFTER it.
    pub(super) async fn await_open_ready(&self, uri: &Url, policy: WaitPolicy) {
        /// Interactive floor for THIS wait specifically. The 400 ms
        /// `cold_wait_ms` default was sized for the workspace-index wait; the
        /// document's own initial build is the thing the verb is ABOUT, and a
        /// giant Perl file's build is seconds — answering early means
        /// answering null, which the client caches as "nothing there". Post
        /// the fold fixes almost every build fits under this floor, so the
        /// common case is a correct answer a moment later, and only the
        /// multi-MB tail falls through to the ContentModified terminal
        /// (`not_ready_or_null`). `cold_wait_ms == 0` still opts out of
        /// waiting entirely.
        const OPEN_BUILD_WAIT_MS: u64 = 5_000;
        if self.files.get_open(uri).is_some() {
            return; // already built
        }
        let Some(gate) = self.opening.get(uri).map(|n| Arc::clone(n.value())) else {
            return; // not an in-flight open (unknown/closed file)
        };
        let mut cap = self.wait_cap(policy);
        if cap == 0 {
            return; // opt-out
        }
        if matches!(policy, WaitPolicy::Interactive) {
            cap = cap.max(OPEN_BUILD_WAIT_MS);
        }
        let waited = gate.armed_wait(|| self.files.get_open(uri).is_some());
        if let Some(waited) = waited {
            self.bounded_wait_with_progress(cap, waited, "Waiting for file analysis")
                .await;
        }
    }

    /// The degraded terminal for a pull verb whose document ISN'T in the
    /// store after the bounded wait. A `null` here is a lie the client
    /// believes: LSP `null` means "nothing at this position" — a final
    /// answer, cached, never re-requested — so a build that outruns the wait
    /// makes the editor look dead until a server-initiated refresh happens to
    /// cover the verb (tokens have one; hover/definition don't). When the
    /// initial build is still in flight, answer `ContentModified` instead —
    /// the protocol's "computed against in-flux state, retry" — and keep the
    /// honest null for a URI with no build coming.
    pub(super) fn not_ready_or_null<T>(
        &self,
        uri: &Url,
    ) -> tower_lsp::jsonrpc::Result<Option<T>> {
        if self.opening.contains_key(uri) {
            Err(tower_lsp::jsonrpc::Error {
                code: tower_lsp::jsonrpc::ErrorCode::ContentModified,
                message: "initial analysis still in flight; retry".into(),
                data: None,
            })
        } else {
            Ok(None)
        }
    }

    /// Bounded wait for the open document's FULL-quality analysis — past the
    /// degraded cached-only-gather window (`degraded_open`). Only cross-file
    /// act-on-able verbs (references / rename / implementations) call this,
    /// AFTER `await_open_ready`: their answers read the cross-file closure,
    /// and inside the window they return a subset that looks complete (curl:
    /// 4 reference sites vs 155). Per-file verbs (outline, hover, completion)
    /// deliberately don't — their answers don't need the gather, and waiting
    /// would regress open→outline latency for nothing. `Interactive` policy
    /// returns immediately: fast-best-effort verbs keep today's behavior.
    pub(super) async fn await_open_full(&self, uri: &Url, policy: WaitPolicy) {
        if !matches!(policy, WaitPolicy::Complete) {
            return;
        }
        let Some(gate) = self.degraded_open.get(uri).map(|n| Arc::clone(n.value())) else {
            return; // not degraded (perl doc, heal already landed, or never opened)
        };
        let cap = self.wait_cap(policy);
        if cap == 0 {
            return; // opt-out
        }
        let waited = gate.armed_wait(|| !self.degraded_open.contains_key(uri));
        if let Some(waited) = waited {
            self.bounded_wait_with_progress(cap, waited, "Waiting for cross-file analysis")
                .await;
        }
    }

    /// Bounded wait that surfaces as client progress once it actually
    /// BLOCKS. Silent for the first 500 ms — warm paths and every
    /// `Interactive` wait (cap ≤ ~400 ms) resolve inside it, so no UI
    /// noise; only a `Complete` wait that outlives the quiet window mints
    /// a work-done token, keeping the honest-answer block visible instead
    /// of reading as a hung request.
    async fn bounded_wait_with_progress<F>(&self, cap_ms: u64, wait: F, title: &str)
    where
        F: std::future::Future<Output = ()>,
    {
        use std::time::Duration;
        const QUIET_MS: u64 = 500;
        tokio::pin!(wait);
        let quiet = cap_ms.min(QUIET_MS);
        if tokio::time::timeout(Duration::from_millis(quiet), &mut wait)
            .await
            .is_ok()
        {
            return;
        }
        let remaining = cap_ms.saturating_sub(quiet);
        if remaining == 0 {
            return;
        }
        // Server-initiated progress requires the client capability; a client
        // that never advertised it may also never answer the create request.
        if !self
            .work_done_progress
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let _ = tokio::time::timeout(Duration::from_millis(remaining), &mut wait).await;
            return;
        }
        static WAIT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let token = NumberOrString::String(format!(
            "perl-lsp/wait-{}",
            WAIT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        progress_create_and_begin(&self.client, &token, title).await;
        let _ = tokio::time::timeout(Duration::from_millis(remaining), &mut wait).await;
        progress_end(&self.client, token).await;
    }
}

/// The one spelling of "create + begin a work-done progress" — reused by the
/// blocking-wait announcement (`bounded_wait_with_progress`) and the degraded
/// diagnostics announcement (`PackHealCtx::begin_progress`). The detached
/// create-request task keeps the oneshot receiver alive past the 2 s timeout,
/// so a late reply can't panic tower-lsp's pending map (#36). Capability
/// gating is the caller's responsibility — a token minted here presumes the
/// client advertised `window/workDoneProgress`.
pub(super) async fn progress_create_and_begin(client: &Client, token: &NumberOrString, title: &str) {
    let create = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            let _ = client
                .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                    token,
                })
                .await;
        }
    });
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), create).await;
    client
        .send_notification::<notification::Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: title.into(),
                cancellable: Some(false),
                message: None,
                percentage: None,
            })),
        })
        .await;
}

/// Reserve the per-window progress slot for `uri` atomically. Returns `true`
/// exactly once per window — the first caller mints the token; every later
/// caller reuses it (returns `false`), so a keystroke burst inside one degraded
/// window announces itself with a single Begin, not one per change. Releasing
/// the slot (`clear_degraded`/close removes the entry) lets the next window
/// reserve again. The DashMap entry guard is dropped before return — no lock is
/// held across the caller's subsequent `.await`.
pub(super) fn reserve_degraded_token(
    map: &dashmap::DashMap<Url, NumberOrString>,
    uri: &Url,
    token: NumberOrString,
) -> bool {
    use dashmap::mapref::entry::Entry;
    match map.entry(uri.clone()) {
        Entry::Occupied(_) => false,
        Entry::Vacant(v) => {
            v.insert(token);
            true
        }
    }
}

/// The one spelling of "end a work-done progress".
pub(super) async fn progress_end(client: &Client, token: NumberOrString) {
    client
        .send_notification::<notification::Progress>(ProgressParams {
            token,
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                message: None,
            })),
        })
        .await;
}
