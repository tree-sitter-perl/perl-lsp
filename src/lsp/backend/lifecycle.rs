//! Backend construction, the `PackHealCtx` single-flight gather heal,
//! debounced pack rebuilds, pack invalidation, and diagnostics publishing.

use super::*;

impl PackHealCtx {
    /// Single-flight gather request. If a gather loop is already running for
    /// `uri`, coalesces into it (no new task); otherwise registers the URI and
    /// spawns the loop. Never awaits a gather — the change path stays
    /// cached-only + fire-and-forget.
    pub(super) fn request_gather(&self, uri: Url) {
        if !self.gather_reg.request(&uri) {
            return; // a loop already owns this URI; the request coalesced in
        }
        let ctx = self.clone();
        tokio::spawn(async move {
            ctx.run_gather_loop(uri).await;
        });
    }

    /// One gather owner per URI: gather → (maybe) re-run once if the buffer
    /// moved mid-gather → retire. When the loop retires it clears the degraded
    /// window and ends the provisional-diagnostics progress — i.e. progress
    /// ends exactly when full-quality diagnostics have published.
    async fn run_gather_loop(self, uri: Url) {
        loop {
            self.run_gather_once(&uri).await;
            if !self.gather_reg.finish(&uri) {
                break;
            }
        }
        self.clear_degraded(&uri).await;
    }

    /// Announce the degraded window: begin a work-done progress that says the
    /// gather is warming and diagnostics are provisional. Idempotent per
    /// window — the token is reserved once and reused across keystrokes (no
    /// spam), and released by `clear_degraded`/close. Capability-gated: a no-op
    /// when the client never advertised `window/workDoneProgress`.
    pub(super) async fn begin_progress(&self, uri: &Url, language: &str) {
        if !self.work_done.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        static DEGRADED_TOKEN: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let token = NumberOrString::String(format!(
            "perl-lsp/degraded-{}",
            DEGRADED_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // Reserve the slot atomically so two concurrent begins for the same
        // URI mint exactly one token.
        if !reserve_degraded_token(&self.degraded_progress, uri, token.clone()) {
            return; // this window already announced itself; reuse the token
        }
        let title = format!("{language} index warming — diagnostics are provisional");
        progress_create_and_begin(&self.client, &token, &title).await;
    }

    /// End the degraded window's progress if one is live (removes the token —
    /// bounded, one End per Begin).
    async fn end_progress(&self, uri: &Url) {
        if let Some((_, token)) = self.degraded_progress.remove(uri) {
            progress_end(&self.client, token).await;
        }
    }

    /// Clear the degraded-open mark, wake `await_open_full` waiters, and end
    /// the provisional-diagnostics progress. The window is over.
    async fn clear_degraded(&self, uri: &Url) {
        if let Some((_, g)) = self.degraded_open.remove(uri) {
            g.open();
        }
        self.end_progress(uri).await;
    }

    /// One cross-file gather + full-quality re-analyze + re-publish for an open
    /// pack document. Cold gather allowed (this task has cached-only OFF).
    /// Does NOT clear the degraded window or spawn a successor — the enclosing
    /// `run_gather_loop` owns retirement. A stale-text result is dropped
    /// (no clobber); the loop's `finish` decides whether to re-run.
    async fn run_gather_once(&self, uri: &Url) {
        let Some((text, path, language)) = self
            .files
            .get_open(uri)
            .filter(|d| {
                // Only a gather-dependent language has anything to re-gather.
                crate::build::language_driver::LanguageRegistry::caps(d.language).context_gather
            })
            .map(|d| (d.text.clone(), d.path.clone(), d.language))
        else {
            return;
        };
        let snapshot = text.clone();
        // Full analyze on a blocking thread so the ~1.5 s gather never stalls
        // the executor.
        let analysis = tokio::task::spawn_blocking(move || {
            crate::build::language_driver::LanguageRegistry::with_enabled()
                .for_id(language)
                .map(|d| d.analyze_with_path(&text, path.as_deref()))
        })
        .await
        .ok()
        .flatten();
        let Some(analysis) = analysis else {
            return;
        };
        // A keystroke may have landed while we gathered; the debounced rebuild
        // owns the newer text, so don't clobber it with this stale build (the
        // loop re-runs against the latest text — the gather cache stays warm
        // for unchanged included files, so the re-run is cheap).
        if self
            .files
            .get_open(uri)
            .map(|d| d.text != snapshot)
            .unwrap_or(true)
        {
            return;
        }
        for imp in &analysis.imports {
            self.module_index.request_resolve(&imp.module_name);
        }
        for (_pkg, parents) in analysis.package_parent_edges() {
            for parent in parents {
                self.module_index.request_resolve(parent);
            }
        }
        if let Some(mut doc) = self.files.get_open_mut(uri) {
            doc.apply_rebuilt(analysis);
        }
        let diags = self
            .files
            .get_open(uri)
            .map(|doc| {
                symbols::pack_diagnostics(
                    &doc.analysis,
                    Some(self.module_index.lookup_for(doc.language).as_lookup()),
                    self.index_ready.pack.is_open(),
                    self.options,
                )
            });
        if let Some(diags) = diags {
            self.client
                .publish_diagnostics(uri.clone(), diags, None)
                .await;
        }
    }
}

impl Backend {
    /// Build the shared context a background pack-gather heal runs with.
    pub(super) fn pack_heal_ctx(&self) -> PackHealCtx {
        PackHealCtx {
            files: Arc::clone(&self.files),
            module_index: Arc::clone(&self.module_index),
            client: self.client.clone(),
            options: self.diagnostic_options(),
            degraded_open: Arc::clone(&self.degraded_open),
            degraded_progress: Arc::clone(&self.degraded_progress),
            gather_reg: Arc::clone(&self.gather_reg),
            work_done: Arc::clone(&self.work_done_progress),
            index_ready: Arc::clone(&self.index_ready),
        }
    }

    pub fn new(client: Client) -> Self {
        let files: Arc<FileStore> = Arc::new(FileStore::new());

        // We need Arc<ModuleIndex> so the refresh callback can access it.
        // Two-phase init: create ModuleIndex whose refresh callback references
        // a later-set Arc<ModuleIndex>, then wire up the Arc.
        let diag_options = Arc::new(std::sync::Mutex::new(symbols::DiagnosticOptions::default()));

        let module_index_holder: Arc<std::sync::OnceLock<Arc<ModuleIndex>>> =
            Arc::new(std::sync::OnceLock::new());

        let index_ready = Arc::new(IndexReady::default());
        let on_refresh = make_on_refresh(
            client.clone(),
            Arc::clone(&files),
            Arc::clone(&module_index_holder),
            Arc::clone(&diag_options),
            Arc::clone(&index_ready),
        );

        let module_index = Arc::new(ModuleIndex::new(client.clone(), on_refresh));
        let _ = module_index_holder.set(Arc::clone(&module_index));

        Backend {
            module_index,
            client,
            files,
            change_debounce: Arc::new(dashmap::DashMap::new()),
            diag_debounce: Arc::new(dashmap::DashMap::new()),
            perl_indexed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pack_indexed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            work_done_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            type_hierarchy_dynamic: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pack_invalidator: Arc::new(crate::index::pack_invalidator::PackInvalidator::default()),
            diag_options,
            rename_options: Arc::new(std::sync::Mutex::new(crate::index::resolve::RenameOptions::default())),
            index_ready,
            cold_wait_ms: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_COLD_WAIT_MS)),
            max_cache_mb: Arc::new(std::sync::atomic::AtomicU64::new(max_cache_mb_default())),
            opening: Arc::new(dashmap::DashMap::new()),
            degraded_open: Arc::new(dashmap::DashMap::new()),
            degraded_progress: Arc::new(dashmap::DashMap::new()),
            gather_reg: Arc::new(GatherRegistry::default()),
        }
    }

    /// Debounced pack rebuild for `uri`: OFF the document lock (snapshot text
    /// → `spawn_blocking` build → write back) + publish diagnostics — only
    /// the edit that survives the settle window rebuilds, so a burst of
    /// keystrokes collapses to ONE analysis after typing settles.
    pub(super) fn spawn_debounced_rebuild(&self, uri: Url) {
        let gate = Arc::clone(
            self.change_debounce
                .entry(uri.clone())
                .or_default()
                .value(),
        );
        let debounce = Arc::clone(&gate.debounce);
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let client = self.client.clone();
        let options = self.diagnostic_options();
        let degraded_open = Arc::clone(&self.degraded_open);
        let heal_ctx = self.pack_heal_ctx();
        let index_ready = Arc::clone(&self.index_ready);
        let handle = tokio::runtime::Handle::current();
        debounce.fire(&handle, std::time::Duration::from_millis(150), move |latest| async move {
            // One rebuild per URI at a time. The settle window collapses fires
            // that are superseded BEFORE they start; a burst spaced wider than
            // it would otherwise run several ~0.7s analyses of this file
            // concurrently, each holding its own text + tree + analysis. The
            // `still` re-probe below drops a superseded RESULT — it can't
            // un-spend that work, so the coalescing has to happen here.
            let _serialized = gate.run.lock().await;
            if !latest.still() {
                return;
            }
            // Snapshot the latest text off the lock; build on a blocking
            // thread so the ~0.7s analysis never stalls completion/hover.
            let Some((text, path, language)) = files
                .get_open(&uri)
                .map(|d| (d.text.clone(), d.path.clone(), d.language))
            else {
                return;
            };
            // A pack file's cross-file GATHER is cold on the first change after
            // a cold open (did_open's gather bails once the text changes, so it
            // can't warm us). Paying the ~24 s cold gather HERE would make the
            // first keystroke's diagnostics land 24 s late. Run CACHED-ONLY for
            // fast, degraded diagnostics — same as did_open — then heal via a
            // background gather refresh below. The flag is a thread-local no-op
            // for perl. See docs/open-forks.md.
            let analysis = tokio::task::spawn_blocking(move || {
                crate::build::cpp_reparse::set_gather_cached_only(true);
                let a = crate::build::language_driver::LanguageRegistry::with_enabled()
                    .for_id(language)
                    .map(|d| d.analyze_with_path(&text, path.as_deref()));
                crate::build::cpp_reparse::set_gather_cached_only(false);
                a
            })
            .await
            .ok()
            .flatten();
            let Some(analysis) = analysis else {
                return;
            };
            if !latest.still() {
                return; // a newer keystroke superseded this build
            }
            for imp in &analysis.imports {
                module_index.request_resolve(&imp.module_name);
            }
            for (_pkg, parents) in analysis.package_parent_edges() {
                for parent in parents {
                    module_index.request_resolve(parent);
                }
            }
            if let Some(mut doc) = files.get_open_mut(&uri) {
                doc.apply_rebuilt(analysis);
            }
            let diags = files.get_open(&uri).map(|doc| {
                symbols::pack_diagnostics(
                    &doc.analysis,
                    Some(module_index.lookup_for(doc.language).as_lookup()),
                    index_ready.pack.is_open(),
                    options,
                )
            });
            if let Some(diags) = diags {
                client.publish_diagnostics(uri.clone(), diags, None).await;
            }
            // Heal: warm the cross-file gather off this task and re-publish
            // full-quality diagnostics when it lands. The cached-only rebuild
            // just re-opened the degraded window for cross-file verbs; mark it
            // (so `await_open_full` holds Complete verbs until the heal lands),
            // announce it via progress (Part 1), then route the heal through
            // the single-flight registry (Part 2) so a typing burst coalesces
            // into ONE gather instead of abandoning one per keystroke. A
            // language with no gather has nothing to heal and is skipped.
            if crate::build::language_driver::LanguageRegistry::caps(language).context_gather {
                degraded_open
                    .entry(uri.clone())
                    .or_insert_with(|| Arc::new(ReadyGate::default()));
                heal_ctx.begin_progress(&uri, language).await;
                heal_ctx.request_gather(uri);
            }
        });
    }

    /// A pack file's bytes changed on disk (save or watcher event) — forward
    /// the fact to the invalidation owner off the message loop, then publish
    /// its outcome: every returned open URI re-gathers through the
    /// single-flight registry (Part 2), so a consumer already mid-gather
    /// coalesces (re-runs once against the freshly evicted caches) instead
    /// of double-gathering the same cone. Which analyses are stale, the
    /// serialization, and the H9 disciplines are all `PackInvalidator`'s.
    pub(super) fn schedule_pack_invalidate(&self, path: PathBuf, deleted: bool) {
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let invalidator = Arc::clone(&self.pack_invalidator);
        let root = self.module_index.workspace_root();
        let heal_ctx = self.pack_heal_ctx();
        tokio::spawn(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                invalidator.file_changed(root.as_deref(), &module_index, &files, &path, deleted)
            })
            .await;
            let Ok(outcome) = outcome else { return };
            if outcome.deferred {
                // Reconciled at end-of-index; `heal_open_docs` re-publishes
                // the open docs then.
                return;
            }
            for uri in outcome.refresh_open {
                heal_ctx.request_gather(uri);
            }
        });
    }

    /// Build the shared context a background diagnostics derivation runs
    /// with — Arcs only, safe to move into a spawned task.
    pub(super) fn diag_ctx(&self) -> DiagCtx {
        DiagCtx {
            files: Arc::clone(&self.files),
            module_index: Arc::clone(&self.module_index),
            client: self.client.clone(),
            options: self.diagnostic_options(),
            perl_indexed: Arc::clone(&self.perl_indexed),
            index_ready: Arc::clone(&self.index_ready),
        }
    }

    /// Debounced, OFF-pipeline diagnostics refresh for one open doc: surface
    /// record → publish → dirty-consumer republish, the exact sequence
    /// `did_open`/`did_change`/`did_save` used to run INLINE in their handler
    /// futures. tower-lsp polls every handler future (and the stdin reader)
    /// inside ONE joined task, so the synchronous enrichment this sequence
    /// pays (minutes against a 100k-file index) head-of-line blocked every
    /// other verb — the post-cold-index availability hole. Here the fire is
    /// (a) debounced per URI so a keystroke burst pays one enrichment,
    /// (b) serialized per URI so bodies never stack, and (c) derived on the
    /// blocking pool so no reactor thread ever runs it.
    pub(super) fn schedule_diag_refresh(&self, uri: Url) {
        let gate = Arc::clone(
            self.diag_debounce
                .entry(uri.clone())
                .or_default()
                .value(),
        );
        let ctx = self.diag_ctx();
        let handle = tokio::runtime::Handle::current();
        let debounce = Arc::clone(&gate.debounce);
        debounce.fire(&handle, std::time::Duration::from_millis(150), move |latest| async move {
            // One refresh body per URI at a time; a waiter superseded while
            // queued drops instead of replaying a stale generation.
            let _serialized = gate.run.lock().await;
            if !latest.still() {
                return;
            }
            let recorded = ctx.record_surface(&uri);
            ctx.publish(&uri).await;
            // Surface-gated consumer refresh: a body edit stops here
            // (Unchanged → empty dirty set); a contract change republishes
            // the open docs that can see it.
            if let Some(sd) = recorded {
                ctx.republish_dirty(&sd.dirty).await;
            }
        });
    }

    /// Fire-and-forget republish of a dirty closure's OPEN docs — the
    /// didClose/watcher spelling. Never awaited by a handler: each publish
    /// re-enriches, and enrichment must not run (or be waited on) inside the
    /// message pipeline.
    pub(super) fn spawn_republish(&self, dirty: std::collections::HashSet<std::path::PathBuf>) {
        if dirty.is_empty() {
            return;
        }
        let ctx = self.diag_ctx();
        tokio::spawn(async move {
            ctx.republish_dirty(&dirty).await;
        });
    }
}

/// Everything a background diagnostics derivation needs, detached from
/// `Backend` so it can move into spawned tasks (the diagnostics twin of
/// `PackHealCtx`). Holds Arcs/handles only.
#[derive(Clone)]
pub(super) struct DiagCtx {
    files: Arc<FileStore>,
    module_index: Arc<ModuleIndex>,
    client: Client,
    options: symbols::DiagnosticOptions,
    perl_indexed: Arc<std::sync::atomic::AtomicBool>,
    index_ready: Arc<IndexReady>,
}

impl DiagCtx {
    /// The freshness engine's consumption half for OPEN docs: after an
    /// edit to `uri` rebuilt its analysis, record the new surface. An
    /// `Unchanged` verdict is the early-cutoff — a body edit refreshes
    /// nobody. Records `Document::baseline_surface` — the build-time,
    /// pre-enrichment projection — through `record_and_dirty_value`, the
    /// shared record→verdict→dirty seam. Enrichment state can't reach the
    /// record by construction, so this may run before or after any publish.
    /// The caller acts on the returned set (republish). Hub languages only —
    /// pack freshness is the invalidator's disk-side gate.
    pub(super) fn record_surface(
        &self,
        uri: &Url,
    ) -> Option<crate::index::module_index::SurfaceDirty> {
        let surface = {
            let doc = self.files.get_open(uri)?;
            if !crate::build::language_driver::LanguageRegistry::caps(doc.language)
                .hub_enrichment
            {
                return None;
            }
            doc.baseline_surface.clone()?
        };
        let path = uri.to_file_path().ok()?;
        let canon = std::fs::canonicalize(&path).unwrap_or(path);
        Some(self.module_index.record_and_dirty_value(
            &canon,
            surface,
            crate::index::module_index::SurfaceWrite::OpenDoc,
        ))
    }

    /// Re-enrich + republish every OPEN doc in a dirty closure — the one
    /// speller of the membership rule (canonical-path match), shared by
    /// the in-editor verdict path and the watcher's aggregated closure.
    pub(super) async fn republish_dirty(
        &self,
        dirty: &std::collections::HashSet<std::path::PathBuf>,
    ) {
        if dirty.is_empty() {
            return;
        }
        let mut to_refresh: Vec<Url> = Vec::new();
        self.files.for_each_open(|u, _doc| {
            if let Ok(p) = u.to_file_path() {
                let c = std::fs::canonicalize(&p).unwrap_or(p);
                if dirty.contains(&c) {
                    to_refresh.push(u.clone());
                }
            }
        });
        for u in to_refresh {
            self.publish(&u).await;
        }
    }

    /// Publish `uri`'s diagnostics — a pure read over the derived enriched
    /// analysis. The one enrichment writer is `FileStore::enrich_open`
    /// (clone-and-enrich off the store lock, ptr-guarded swap); this path
    /// reads the artifact it returns, never mutates a stored analysis.
    /// The derivation (enrichment + collection) runs on the BLOCKING pool:
    /// it is minutes of synchronous CPU against a large index, and any
    /// future that runs it inline stalls tower-lsp's single serve task.
    pub(super) async fn publish(&self, uri: &Url) {
        crate::util::ghost_stats::count("publish_diagnostics");
        let language = self.files.get_open(uri).map(|d| d.language);
        let diagnostics = match language {
            Some(l) if crate::build::language_driver::LanguageRegistry::caps(l).hub_enrichment => {
                // Deferred while the Perl workspace index is landing: an
                // enrichment cascade started mid-registration builds the
                // overlay closure against keys the landing providers
                // immediately invalidate (~75% of warm-open overlay builds
                // were these rebuilds), and its diagnostics are derived from
                // a half-loaded index anyway. Skip the publish entirely —
                // no empty array either, which would clear prior diags on a
                // didChange inside the window. Coverage: `heal_open_docs`
                // republishes every open Perl doc at index completion, and
                // the gate opens BEFORE its sweep collects, so a doc whose
                // publish deferred is in the store by the time the sweep
                // runs — nothing can slip between.
                use std::sync::atomic::Ordering;
                if self.perl_indexed.load(Ordering::Relaxed)
                    && !self.index_ready.perl.is_open()
                {
                    crate::util::ghost_stats::count("publish_diagnostics.deferred");
                    return;
                }
                let files = Arc::clone(&self.files);
                let idx = Arc::clone(&self.module_index);
                let options = self.options;
                let uri = uri.clone();
                tokio::task::spawn_blocking(move || {
                    match files.enrich_open(&uri, &*idx) {
                        Some(analysis) => {
                            symbols::collect_diagnostics(&analysis, &idx, options)
                        }
                        None => vec![],
                    }
                })
                .await
                .unwrap_or_default()
            }
            // Pack languages stay honest-silent EXCEPT the always-on
            // member-access operator mismatch and the opt-in use-after-move
            // (gated by `DiagnosticOptions.use_after_move`) — a cheap
            // per-file read, no blocking hop needed.
            Some(_) => self
                .files
                .get_open(uri)
                .map(|doc| {
                    symbols::pack_diagnostics(
                        &doc.analysis,
                        Some(self.module_index.lookup_for(doc.language).as_lookup()),
                        self.index_ready.pack.is_open(),
                        self.options,
                    )
                })
                .unwrap_or_default(),
            None => vec![],
        };
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }
}

/// The resolver thread's diagnostics-refresh callback. Each resolved module
/// fires it (~33 in ~400ms opening a Perl file with a dozen `use`s), each
/// otherwise a full all-open re-enrich + publish — CPU + stdout pressure
/// that WIDENS the cold-open degraded window. `DebouncedLatest` collapses
/// the burst: only the latest fire surviving the settle window republishes.
/// The tokio handle is captured at construction because the callback runs on
/// the resolver thread, which has no tokio context.
///
/// The debounce bounds how many fires are SCHEDULED, not how many run at
/// once: a body that outlives the settle window is overtaken by the next
/// surviving fire, and on a large dep tree the bodies (a whole-analysis
/// re-enrich each) stacked ~20 deep and took the process to multi-GB. `run`
/// serializes them, and each waiter re-probes `Latest::still` after
/// acquiring it, so a queue of superseded fires collapses to the newest
/// instead of replaying every one.
fn make_on_refresh(
    client: Client,
    files: Arc<FileStore>,
    holder: Arc<std::sync::OnceLock<Arc<ModuleIndex>>>,
    diag_options: Arc<std::sync::Mutex<symbols::DiagnosticOptions>>,
    index_ready: Arc<IndexReady>,
) -> impl Fn() + Send + Sync + 'static {
    let debounce = Arc::new(DebouncedLatest::default());
    let run = Arc::new(tokio::sync::Mutex::new(()));
    let handle = tokio::runtime::Handle::current();
    move || {
        let client = client.clone();
        let files = Arc::clone(&files);
        let holder = Arc::clone(&holder);
        let diag_options = Arc::clone(&diag_options);
        let index_ready = Arc::clone(&index_ready);
        let run = Arc::clone(&run);
        crate::util::ghost_stats::count("on_refresh.fired");
        log::debug!("diag-refresh fired");
        debounce.fire(&handle, std::time::Duration::from_millis(120), move |latest| async move {
            let module_index = match holder.get() {
                Some(idx) => idx,
                None => return,
            };
            // One refresh body at a time; whoever waited out a long one and
            // was superseded meanwhile drops instead of re-running it.
            let _serialized = run.lock().await;
            if !latest.still() {
                return;
            }
            crate::util::ghost_stats::count("on_refresh.executed");
            log::debug!("diag-refresh executing");
            // Derive (uri, diagnostics) first without holding the store lock
            // across the await — publishing is async and could deadlock.
            // The derive runs on the BLOCKING pool: this body is a tokio
            // task, and a whole-open-set re-enrich is synchronous CPU that
            // must never pin a reactor worker.
            let options = *diag_options.lock().unwrap();
            let pack_settled = index_ready.pack.is_open();
            let pending = {
                let files = Arc::clone(&files);
                let module_index = Arc::clone(module_index);
                tokio::task::spawn_blocking(move || {
                    refresh_open_diagnostics(&files, &module_index, options, OpenDocScope::All, pack_settled)
                })
                .await
                .unwrap_or_default()
            };
            for (uri, diags) in pending {
                client.publish_diagnostics(uri, diags, None).await;
            }
        });
    }
}

/// Which open docs a bulk diagnostics refresh covers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenDocScope {
    /// Every open doc (the resolver refresh storm — hub-enriched docs
    /// re-enrich, the rest re-read).
    All,
    /// Hub-enriched docs only (the hub-family cold-open heal).
    PerlFamily,
}

/// Re-derive diagnostics for open docs: hub-enriched docs re-enrich through
/// `FileStore::enrich_open` (the one enrichment writer) and are read from
/// the returned artifact; the rest are read as-is. URIs are collected
/// under the read iterator first, then each doc is derived with no store
/// guard held — safe to run from any task, publish after.
pub(super) fn refresh_open_diagnostics(
    files: &FileStore,
    module_index: &ModuleIndex,
    options: symbols::DiagnosticOptions,
    scope: OpenDocScope,
    pack_settled: bool,
) -> Vec<(Url, Vec<Diagnostic>)> {
    crate::util::ghost_stats::count("refresh_open_diagnostics");
    let mut docs: Vec<(Url, &'static str)> = Vec::new();
    let hub = |language: &str| {
        crate::build::language_driver::LanguageRegistry::caps(language).hub_enrichment
    };
    files.for_each_open(|uri, doc| {
        if scope == OpenDocScope::All || hub(doc.language) {
            docs.push((uri.clone(), doc.language));
        }
    });
    let mut pending: Vec<(Url, Vec<Diagnostic>)> = Vec::new();
    for (uri, language) in docs {
        let diagnostics = if hub(language) {
            match files.enrich_open(&uri, module_index) {
                Some(analysis) => symbols::collect_diagnostics(&analysis, module_index, options),
                None => continue, // closed mid-iteration
            }
        } else {
            match files.get_open(&uri) {
                Some(doc) => symbols::pack_diagnostics(
                    &doc.analysis,
                    Some(module_index.lookup_for(language).as_lookup()),
                    // the undefined-type lane needs the workspace's pack
                    // index settled — the family's ready gate
                    pack_settled,
                    options,
                ),
                None => continue,
            }
        };
        pending.push((uri, diagnostics));
    }
    pending
}

/// `(RefLocation, text)` pairs → one `WorkspaceEdit` (per-member texts).
pub(super) fn edit_pairs_to_workspace_edit(
    edits: Vec<(crate::index::resolve::RefLocation, String)>,
) -> Option<WorkspaceEdit> {
    if edits.is_empty() {
        return None;
    }
    let mut all_changes: std::collections::HashMap<Url, Vec<TextEdit>> =
        std::collections::HashMap::new();
    for (loc, text) in edits {
        if let Some(uri) = loc.to_url() {
            all_changes.entry(uri).or_default().push(TextEdit {
                range: symbols::span_to_range(loc.span),
                new_text: text,
            });
        }
    }
    if all_changes.is_empty() {
        None
    } else {
        Some(WorkspaceEdit { changes: Some(all_changes), ..Default::default() })
    }
}


pub(super) fn refs_to_locations(results: Vec<crate::index::resolve::RefLocation>) -> Option<Vec<Location>> {
    let mut locations: Vec<Location> = results
        .into_iter()
        .filter_map(|r| {
            let uri = r.to_url()?;
            Some(Location {
                uri,
                range: symbols::span_to_range(r.span),
            })
        })
        .collect();
    if locations.is_empty() {
        return None;
    }
    locations.sort_by(|a, b| {
        a.uri.as_str().cmp(b.uri.as_str())
            .then_with(|| a.range.start.line.cmp(&b.range.start.line))
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
    });
    locations.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);
    Some(locations)
}

/// How often the parent-liveness monitor polls the client `processId`. ~10s is
/// the cadence vscode-languageserver-node / lsp4j / jdt.ls use — cheap enough to
/// run unconditionally, tight enough that a leaked server dies within a poll.
const PARENT_LIVENESS_POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn a detached timer that self-exits when the LSP client (parent) process
/// dies. This is INDEPENDENT of the stdin read loop by design: the leak cases
/// are exactly when the read loop isn't running (server wedged mid-analysis, or
/// a hard SIGKILL of the editor that delivered no clean EOF). `None` disables
/// the check — per spec, a null `processId` means the client didn't fork us.
pub(super) fn spawn_parent_liveness_monitor(process_id: Option<u32>) {
    let Some(pid) = process_id else { return };
    if pid == 0 {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PARENT_LIVENESS_POLL).await;
            if !parent_process_alive(pid) {
                // Client gone; nothing to flush after the connection drops.
                // Exit hard so background `spawn_blocking` indexing (which parks
                // on `send_request` once the client vanishes) can't keep the
                // runtime — and a multi-GB workspace index — alive.
                std::process::exit(0);
            }
        }
    });
}

/// Linux liveness probe: `/proc/<pid>` vanishes once the process is reaped. No
/// new dependency, no signal side effects (unlike `kill(pid, 0)`).
#[cfg(target_os = "linux")]
fn parent_process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Off Linux there's no cheap dependency-free probe, so assume alive — never
/// false-positive into an exit. The stdin-EOF path still covers clean shutdown.
#[cfg(not(target_os = "linux"))]
fn parent_process_alive(_pid: u32) -> bool {
    true
}
