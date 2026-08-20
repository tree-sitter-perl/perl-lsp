//! Registration front doors: cache/workspace/pack registration, surface
//! freshness, the enrichment overlay, residency/rehydration, and eviction.

use super::*;

impl ModuleIndex {
    /// Re-register a WHOLE (non-stripped) cached copy carrying materialized
    /// gated emissions. The cache slot routes through `insert_cache` — the
    /// canonical registration seam — so `edges.feed` publishes the freshly
    /// synthesized accessors' name records (making them cross-file-visible)
    /// and the loader-shape / import-gen bookkeeping runs; the path-keyed
    /// registry pins the whole copy so `whole_present` answers with the
    /// emissions and no per-query enriched-overlay hop is needed.
    ///
    /// WHOLE-COPY residency here is a deliberate, bounded exception (visible
    /// to `whole_copy_registration_sites_are_allowlisted`): gated emissions
    /// exist only for plugin-triggered files whose `ClassIsa` gate resolves
    /// cross-file (sparse by construction), and materialization is
    /// CLI/batch-only (one-shot startup) — the warm server never calls it.
    pub(super) fn register_materialized_whole(
        &self,
        name: String,
        path: std::path::PathBuf,
        cm: Arc<CachedModule>,
    ) {
        self.all_files.insert(path.clone(), cm.clone());
        // The path's `all_defs` candidates must track the same generation —
        // a stale candidate would serve the pre-materialization analysis to
        // every by-name candidate reader.
        if let Some(rec) = self.registered_names.get(&path) {
            for (n, _) in rec.iter() {
                if let Some(mut v) = self.core.all_defs.get_mut(n) {
                    if let Some(i) = v.iter().position(|c| c.path == path) {
                        v[i] = cm.clone();
                    }
                }
            }
        }
        self.insert_cache(&name, Some(cm));
    }

    /// Insert a resolved module into the name-keyed cache slot — a thin
    /// front over `IndexCore::insert_resolved`, the one spelling shared
    /// with the resolver thread. CLI/test copies stay whole (`persisted:
    /// false` — nothing was written here, so the strip has no license).
    pub fn insert_cache(&self, module_name: &str, cached: Option<Arc<CachedModule>>) {
        self.insert_cache_providers(module_name, cached.map(|c| vec![c]));
    }

    /// `insert_cache` for a name's WHOLE provider set (`@INC` order). The
    /// single-copy front door above is this one with a one-element set —
    /// there is no separate one-provider path.
    pub fn insert_cache_providers(&self, module_name: &str, providers: Option<Providers>) {
        self.core.insert_resolved(module_name, providers, false, false);
    }

    /// The workspace-registration reads that need the FULL analysis:
    /// loaded-module tracking (imports + method-form plugin loads — fed
    /// even for PACKAGELESS entrypoint scripts, which is where `plugin
    /// 'X'` loads live) and loader-config shapes, which read the witness
    /// bag via `expr_type_at_span`. Callers stripping a resident copy run
    /// this on the whole analysis FIRST; `register_workspace_module`
    /// bundles it for full-copy callers.
    pub fn record_workspace_projections(&self, path: &std::path::Path, analysis: &FileAnalysis) {
        for imp in &analysis.imports {
            self.loaded_modules.insert(imp.module_name.clone(), ());
        }
        for f in &analysis.plugin.loads {
            self.loaded_modules.insert(f.name.clone(), ());
        }
        self.core.record_loader_shapes(&path.display().to_string(), analysis);
    }

    /// The residency half of workspace registration: the path-keyed
    /// registry, name/edge feeds, and the cache insert. The name/edge feeds
    /// read `symbols`, so a symbol-stripped copy must NOT register through
    /// here — indexers strip via `register_workspace_stripping`, which feeds
    /// from the whole analysis first. Files without a `package` declaration
    /// get only the path entry.
    /// Returns the `SurfaceDirty` outcome (verdict + on-`Changed` dirty
    /// consumer set) so re-registration seams (the watcher) act on the same
    /// record→verdict→dirty answer — dropping it leaves open consumers
    /// stale after an external edit (git pull) to a dep.
    pub fn register_workspace_resident(
        &self,
        path: std::path::PathBuf,
        analysis: Arc<FileAnalysis>,
    ) -> SurfaceDirty {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.bump_registration_gen(&path);
        let sd = self.record_and_dirty(&path, &analysis, SurfaceWrite::Background);
        let cached = Arc::new(CachedModule::new(path, analysis.clone()));
        // Path-keyed registry first: the relational retrieval resolves
        // candidate paths through `cached_by_path`, and packageless files
        // (Mojolicious::Lite entrypoints) exist ONLY here.
        self.all_files.insert(cached.path.clone(), cached.clone());
        let names = package_names(&analysis);
        if !names.is_empty() {
            // Whole copy: the feed inside the rebuild records the name list.
            self.core.edges.record_names(&cached.path, &analysis);
        }
        self.adopt_workspace_candidate(cached, names);
        sd
    }

    /// Adopt `cached` as a definition candidate for every package name it
    /// declares (`names`, extracted from the WHOLE analysis pre-strip), then
    /// rebuild the name-keyed registrations for every affected name — the
    /// declared set plus any a previous registration of the same path
    /// declared and this one dropped. Perl's package relation is
    /// name → MANY files (a package reopens anywhere), so the candidate
    /// table (`all_defs`) is the truth and the name-keyed cache slot is a
    /// derived winner — recomputed from the SET, never from arrival order.
    fn adopt_workspace_candidate(
        &self,
        cached: Arc<CachedModule>,
        names: Vec<(String, bool)>,
    ) {
        let prev = self
            .registered_names
            .insert(cached.path.clone(), names.clone());
        let mut affected: Vec<String> = names.iter().map(|(n, _)| n.clone()).collect();
        for (n, _) in prev.into_iter().flatten() {
            if !affected.contains(&n) {
                // Declared before, dropped now: the rebuild below sheds it.
                if let Some(mut v) = self.core.all_defs.get_mut(&n) {
                    v.retain(|c| c.path != cached.path);
                }
                self.core.all_defs.remove_if(&n, |_, v| v.is_empty());
                affected.push(n);
            }
        }
        for (name, _) in &names {
            let mut v = self.core.all_defs.entry(name.clone()).or_default();
            match v.iter().position(|c| c.path == cached.path) {
                Some(i) => v[i] = cached.clone(),
                None => v.push(cached.clone()),
            }
        }
        for name in &affected {
            self.rebuild_name_registration(name);
        }
    }

    /// Recompute everything keyed on `name` from the CURRENT candidate set:
    /// the edge feeds (purge, then one feed per candidate — a sibling
    /// file's edges survive any one file's re-registration by
    /// construction), the name-keyed cache winner (`best_candidate`:
    /// order-independent, smallest-path tie-break), and the workspace-name
    /// mark. With no candidates left, a workspace-owned slot empties; an
    /// occupant the workspace never owned (the @INC tier) is left alone.
    /// Evicted candidates replay their name feeds from the per-path records.
    fn rebuild_name_registration(&self, name: &str) {
        let _t = crate::util::ghost_stats::ScopedNs::start("reg.rebuild_name");
        // `get_cached` answers can move here without a registration-gen
        // mint — the enrichment epoch must move too.
        self.core.note_shape_change();
        let cands: Vec<Arc<CachedModule>> = self
            .core
            .all_defs
            .get(name)
            .map(|v| v.clone())
            .unwrap_or_default();
        crate::util::ghost_stats::count_by("reg.refeed_candidates", cands.len() as u64);
        // ABLATION LEVER (measurement only): `PERL_LSP_ABLATE_NAME_REBUILD=1`
        // replaces the purge + full re-feed with a single feed of the
        // newest candidate — behavior-equivalent on a COLD bulk index
        // (feed is idempotent and purge only matters for re-registration),
        // NOT safe for a long-lived server. Exists to attribute the
        // superlinear registration term; remove with the instrumentation.
        static ABLATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let ablate = *ABLATE
            .get_or_init(|| std::env::var_os("PERL_LSP_ABLATE_NAME_REBUILD").is_some());
        if ablate {
            if let Some(c) = cands.last() {
                self.core.edges.feed(name, &c.path, &c.analysis);
            }
        } else {
            crate::util::ghost_stats::timed("reg.purge_module", || {
                self.core.edges.purge_module(name)
            });
            let _f = crate::util::ghost_stats::ScopedNs::start("reg.refeed");
            for c in &cands {
                self.core.edges.feed(name, &c.path, &c.analysis);
            }
        }
        if cands.is_empty() {
            if self.workspace_modules.remove(name).is_some() {
                // The slot held the last workspace winner; an @INC
                // re-resolve refills on demand.
                self.core.cache.remove(name);
            }
            return;
        }
        self.workspace_modules.insert(name.to_string(), ());
        // The relation is shared with the @INC tier, but the winner is not:
        // project code shadows an installed copy of the same name, and
        // `best_candidate`'s path tie-break has no opinion about which tier
        // a file came from. `registered_names` holds exactly the paths a
        // workspace/pack front door registered, so it IS the tier test —
        // no second marker to keep in sync.
        let refs: Vec<&Arc<CachedModule>> = cands
            .iter()
            .filter(|c| self.registered_names.contains_key(&c.path))
            .collect();
        if refs.is_empty() {
            // Only @INC providers left: leave their slot alone.
            return;
        }
        let _b = crate::util::ghost_stats::ScopedNs::start("reg.best_candidate");
        if let Some(best) =
            best_candidate(&refs, name, &|m, n| self.module_defines_class(m, n))
        {
            self.core.cache.insert(name.to_string(), Some(best));
        }
    }

    /// The freshness gate, single-sourced: record `fa`'s span-free surface
    /// for `path` and, on a `Changed` verdict, walk its transitive dirty
    /// consumers (empty otherwise). Binding record → verdict → dirty in one
    /// seam means a caller cannot record a surface without the consumer
    /// answer from the same path (the "watcher dropped the verdict" bug
    /// class). The caller owns the ACT on the outcome (re-enrich open docs /
    /// accumulate a batch / deps-stamp refresh) — those legitimately differ.
    /// The pack tier discovers consumers by include-closure, not this walk,
    /// so it uses `record_surface` directly (a genuinely different axis).
    pub fn record_and_dirty(
        &self,
        path: &std::path::Path,
        fa: &FileAnalysis,
        write: SurfaceWrite,
    ) -> SurfaceDirty {
        self.record_and_dirty_value(path, crate::model::surface::Surface::project(fa), write)
    }

    /// `record_and_dirty` for an ALREADY-projected surface — the open-doc
    /// path records `Document::baseline_surface` (projected at build time,
    /// pre-enrichment) so the record can never fingerprint enriched state.
    pub fn record_and_dirty_value(
        &self,
        path: &std::path::Path,
        surface: crate::model::surface::Surface,
        write: SurfaceWrite,
    ) -> SurfaceDirty {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let verdict = self.record_surface_write(&canon, surface, write);
        let dirty = match verdict {
            crate::model::surface::SurfaceVerdict::Changed => self.dirty_consumers(&canon),
            _ => Default::default(),
        };
        SurfaceDirty { verdict, dirty }
    }

    /// Project + record `fa`'s span-free surface for `path` — the
    /// freshness engine's write half. Call with a WHOLE analysis (the
    /// projection reads symbols + the bag). Returns the early-cutoff
    /// verdict; `Changed` means `dirty_consumers(path)` names stale files.
    /// `Background` provenance — every direct caller is an indexer lane.
    /// Prefer `record_and_dirty` when you will act on the consumer set —
    /// it binds the walk to the record so it can't be forgotten.
    pub fn record_surface(
        &self,
        path: &std::path::Path,
        fa: &FileAnalysis,
    ) -> crate::model::surface::SurfaceVerdict {
        self.record_surface_value(path, crate::model::surface::Surface::project(fa))
    }

    /// Record an ALREADY-projected surface (the warm-stub path decodes the
    /// persisted projection; the fresh worker projects once and shares it
    /// with the stub encoder).
    pub fn record_surface_value(
        &self,
        path: &std::path::Path,
        surface: crate::model::surface::Surface,
    ) -> crate::model::surface::SurfaceVerdict {
        // Canonicalize here so every caller (open-doc, worker, watcher)
        // lands on one key — a fresh/canon split would make every edit
        // look FirstSeen and the gate never fires.
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.record_surface_write(&canon, surface, SurfaceWrite::Background)
    }

    /// The ONE freshness write (`canon` pre-canonicalized): applies the
    /// `SurfaceWrite` provenance rule — a `Background` write on an open
    /// doc's path is suppressed (Unchanged: consumers read the buffer, and
    /// what they read didn't move).
    fn record_surface_write(
        &self,
        canon: &std::path::Path,
        surface: crate::model::surface::Surface,
        write: SurfaceWrite,
    ) -> crate::model::surface::SurfaceVerdict {
        if write == SurfaceWrite::Background && self.open_doc_paths.contains_key(canon) {
            return crate::model::surface::SurfaceVerdict::Unchanged;
        }
        let verdict = self.freshness.record(canon, surface);
        match verdict {
            crate::model::surface::SurfaceVerdict::FirstSeen => {
                crate::util::ghost_stats::count("epoch.freshness.record_first_seen")
            }
            crate::model::surface::SurfaceVerdict::Changed => {
                crate::util::ghost_stats::count("epoch.freshness.record_changed")
            }
            crate::model::surface::SurfaceVerdict::Unchanged => {}
        }
        verdict
    }

    /// didOpen: the open-doc path owns `path`'s surface record until
    /// `mark_doc_closed` (see `SurfaceWrite`).
    pub fn mark_doc_open(&self, path: &std::path::Path) {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.open_doc_paths.insert(canon, ());
    }

    /// didClose: release the record to background writers and reconcile —
    /// consumers flip back to reading the indexed DISK copy, so re-record it
    /// (whole view: the resident copy may be stripped) and hand back whoever
    /// that flip dirtied. `None` when no indexed copy exists (never indexed,
    /// or deleted — the watcher's delete arm owns record removal): the
    /// open-doc record stays as the last truth until a background write
    /// corrects it.
    pub fn mark_doc_closed(&self, path: &std::path::Path) -> Option<SurfaceDirty> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.open_doc_paths.remove(&canon);
        let cm = self.all_files.get(&canon).map(|e| e.value().clone())?;
        let whole = crate::model::file_analysis::CrossFileLookup::whole_present(self, &cm);
        Some(self.record_and_dirty(&canon, &whole, SurfaceWrite::Background))
    }

    /// Every registration bumps this — the enrichment key's freshness
    /// token for the file itself and for providers whose facts aren't
    /// surface-covered (a body edit re-registers with a new generation,
    /// where a surface fingerprint deliberately stands still).
    pub(crate) fn bump_registration_gen(&self, path: &std::path::Path) {
        self.core.mint_registration_gen(path);
    }

    /// Stamp a generation for every name-keyed cache entry that lacks one —
    /// the warm scan loads @INC blobs straight into the cache without a
    /// registration front door. See `IndexCore::stamp_missing_import_gens`.
    pub(crate) fn stamp_import_generations(&self) {
        self.core.stamp_missing_import_gens();
    }

    fn registration_gen_of(&self, path: &std::path::Path) -> u64 {
        self.core.registration_gen.get(path).map(|g| *g).unwrap_or(0)
    }

    /// Drop `path`'s recorded surface and its dep edges (file deleted).
    /// A deleted file can't canonicalize, but the record was keyed under
    /// the RESOLVED path while it existed — resolve the parent and rejoin
    /// (the Perl watcher's delete fallback) so symlinked roots still hit.
    pub fn remove_surface(&self, path: &std::path::Path) {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| {
            path.parent()
                .and_then(|d| std::fs::canonicalize(d).ok())
                .and_then(|d| path.file_name().map(|f| d.join(f)))
                .unwrap_or_else(|| path.to_path_buf())
        });
        crate::util::ghost_stats::count("epoch.freshness.remove");
        self.freshness.remove(&canon);
        // Belt over braces: if the caller's raw spelling was the recorded
        // key (registration itself fell back), remove that too.
        if canon != path {
            crate::util::ghost_stats::count("epoch.freshness.remove");
            self.freshness.remove(path);
        }
    }

    /// The enrichment overlay (R4): an enriched copy of a workspace file's
    /// analysis, DERIVED and keyed by the surface fingerprints of the file
    /// plus its declared providers — never an in-place mutation of the
    /// shared Arc. Self-validating at read: any provider's surface change
    /// moves the key and the entry recomputes. Bounded (drop-oldest) so a
    /// whole-tree sweep churns through without pinning the tree resident.
    pub fn enriched_snapshot(
        &self,
        cached: &Arc<CachedModule>,
    ) -> Option<Arc<FileAnalysis>> {
        // Cycle guard: enriching A runs type queries whose cross-file chase
        // may ask for enriched(B), and B's enrichment may ask for
        // enriched(A) (mutual imports). A re-entrant request for a path
        // already enriching ON THIS THREAD answers None — the caller's
        // fallback serves the raw bag, the cycle breaks, and the outer
        // enrichment completes with the unenriched view of its cyclic dep.
        thread_local! {
            static ENRICHING: std::cell::RefCell<std::collections::HashSet<std::path::PathBuf>> =
                Default::default();
        }
        thread_local! {
            static DECLINED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        }
        struct Entered(std::path::PathBuf);
        impl Drop for Entered {
            fn drop(&mut self) {
                ENRICHING.with(|s| {
                    s.borrow_mut().remove(&self.0);
                });
            }
        }
        if !ENRICHING.with(|s| s.borrow_mut().insert(cached.path.clone())) {
            DECLINED.with(|c| c.set(c.get() + 1));
            crate::util::ghost_stats::count("enriched_snapshot.cycle_decline");
            return None;
        }
        let _entered = Entered(cached.path.clone());
        let declined_before = DECLINED.with(|c| c.get());
        // BYTE-bounded first (enriched copies are whole analyses — 64 of a
        // tree's biggest generated modules would quietly re-pin the
        // gigabytes the eviction axes stripped), entry-bounded second.
        // `PERL_LSP_ENRICHED_CAP` / `PERL_LSP_ENRICHED_MB` are measurement-
        // only overrides for cap-sweep experiments; unset ⇒ the stock bounds.
        static CAPS: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
        let &(enriched_cap, enriched_byte_cap) = CAPS.get_or_init(|| {
            let entries = std::env::var("PERL_LSP_ENRICHED_CAP")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(64);
            let bytes = std::env::var("PERL_LSP_ENRICHED_MB")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .map(|mb| mb * 1024 * 1024)
                .unwrap_or(128 * 1024 * 1024);
            (entries, bytes)
        });
        let path = &cached.path;
        let key = self.enrichment_key_memoized(cached);
        if let Some(e) = self.enriched.get(path) {
            if e.0 == key {
                let hit = e.1.clone();
                drop(e);
                // LRU touch — a FIFO would let any sweep evict the hot dep
                // entries the witness seams lean on, in insertion order.
                let mut order = self.enriched_order.lock().unwrap();
                order.retain(|p| p != path);
                order.push_back(path.clone());
                // `None` = a remembered DECLINE (giant / cycle-tainted):
                // repeat queries skip the deep-copy until the key moves.
                crate::util::ghost_stats::count("enriched_snapshot.hit");
                if let Some(g) = &self.enriched_ghost {
                    g.on_hit();
                }
                return hit;
            }
        }
        // Enrichment RECURSES: enriching A runs type queries that ask for
        // enriched(B), whose enrichment asks for enriched(C). The cycle
        // guard above only stops a REPEAT of a path already on this
        // thread's stack — a chain of DISTINCT files recurses as deep as
        // the dependency graph is long, and each level deep-copies and
        // enriches a whole analysis. Measured at 138k files: a single
        // `references` consult descended 220+ frames of
        // enrich → query → enrich and never came back.
        //
        // So cap the depth at which a NEW copy is built. A cached hit is
        // served at any depth (it is free and already correct); past the
        // cap a build declines exactly as a cycle declines — the caller
        // falls back to the raw bag, honestly unenriched. `0` = unbounded.
        static DEPTH_CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let depth_cap = *DEPTH_CAP.get_or_init(|| {
            std::env::var("PERL_LSP_ENRICH_DEPTH")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| if v == 0 { usize::MAX } else { v })
                .unwrap_or(4)
        });
        // Depth distribution of the builds that DO run — the measurement
        // that sizes the cap (and, later, level-indexed enrichment's K).
        // Gated: `count` is inert without `PERL_LSP_GHOST_STATS`.
        if crate::util::ghost_stats::enabled() {
            crate::util::ghost_stats::count(&format!(
                "enrich.depth.{:02}",
                ENRICHING.with(|s| s.borrow().len())
            ));
        }
        if ENRICHING.with(|s| s.borrow().len()) > depth_cap {
            DECLINED.with(|c| c.set(c.get() + 1));
            crate::util::ghost_stats::count("enriched_snapshot.depth_decline");
            crate::model::witnesses::ResolutionSession::mark_degraded("enrichment depth");
            return None;
        }
        crate::util::ghost_stats::count("enriched_snapshot.build");
        if let Some(g) = &self.enriched_ghost {
            g.on_miss(&path.to_string_lossy());
            // A key mismatch on a live entry is an INVALIDATION (a provider
            // changed), not capacity pressure.
            if self.enriched.contains_key(path) {
                g.on_invalidate(&path.to_string_lossy());
            }
        }
        let whole = crate::model::file_analysis::CrossFileLookup::whole_present(self, cached);
        // A private copy, because enrichment must never write through the
        // shared Arc (the R4 rule this overlay exists to enforce). `clone`
        // IS that private copy — the bincode round-trip this replaced
        // encoded and decoded the whole analysis to reach the same place and
        // then rebuilt from scratch every index the clone copies directly.
        // Measured over 150 substrate modules: 834 ms round-trip vs 67 ms
        // clone, 12.4x, and the round-trip is what made a build expensive
        // enough to sink level-indexed enrichment
        // (`docs/adr/level-indexed-enrichment.md`).
        //
        // It is also the more faithful copy. `bag_evicted`, `degraded` and
        // the ref/symbol eviction flags are `serde(skip)`, so the round-trip
        // silently reset them to false and `after_deserialize` never put
        // them back — an enriched copy of a DEGRADED analysis claimed to be
        // whole. Clone carries them.
        let mut copy: FileAnalysis = (*whole).clone();
        copy.enrich_imported_types_with_keys(Some(self));
        let arc = Arc::new(copy);
        // Cycle-tainted: some dep declined mid-enrich (mutual imports), so
        // this copy baked a RAW view of that dep. Caching it would serve
        // the degraded answer until an unrelated surface change; serving
        // it unretained would let the witness seams mint fresh copies per
        // recursion level (unbounded). Answer None — cyclic files honestly
        // degrade to their raw bags, deterministically.
        let tainted = DECLINED.with(|c| c.get()) != declined_before;
        let bytes = arc.heap_estimate().total();
        // Past the byte cap the copy can't be RETAINED, and an unretained
        // copy must never leave this function: the seams' termination and
        // memo validity both key on overlay-held Arc identity. Giants and
        // cycle-tainted builds honestly answer unenriched — and the
        // decline is CACHED so repeat queries don't rebuild the copy just
        // to re-decline it.
        let stored: Option<Arc<FileAnalysis>> =
            if tainted || bytes > enriched_byte_cap { None } else { Some(arc) };
        let entry_bytes = if stored.is_some() { bytes } else { 0 };
        self.enriched.insert(path.clone(), (key, stored.clone(), entry_bytes));
        {
            let mut order = self.enriched_order.lock().unwrap();
            order.retain(|p| p != path);
            order.push_back(path.clone());
            let total_bytes = |order: &std::collections::VecDeque<std::path::PathBuf>| {
                order
                    .iter()
                    .filter_map(|p| self.enriched.get(p))
                    .map(|e| e.2)
                    .sum::<usize>()
            };
            while order.len() > 1
                && (order.len() > enriched_cap || total_bytes(&order) > enriched_byte_cap)
            {
                if let Some(evictee) = order.pop_front() {
                    self.enriched.remove(&evictee);
                    if let Some(g) = &self.enriched_ghost {
                        g.on_evict(&evictee.to_string_lossy());
                    }
                }
            }
            if let Some(g) = &self.enriched_ghost {
                g.set_usage(total_bytes(&order) as u64, order.len() as u64);
            }
        }
        stored
    }

    /// The overlay's validity key — it must cover EVERYTHING
    /// `enrich_imported_types_with_keys` reads, or a stale snapshot gets
    /// served silently. The read set and its key coverage:
    ///
    /// - the file's own analysis → the source Arc's identity (a body edit
    ///   keeps the span-free fingerprint still, but every re-registration
    ///   mints a new Arc);
    /// - the file's own surface fingerprint (defense in depth alongside
    ///   the Arc identity);
    /// - every TRANSITIVELY reachable provider (imports ∪ parents ∪
    ///   bridges, then THEIR deps — enrichment walks ancestor chains, so a
    ///   grandparent's contract change must move the key): its freshness
    ///   fingerprint when recorded, else the provider analysis's Arc
    ///   identity (the @INC tier has no surface records; re-resolution
    ///   mints a new Arc). Unresolved providers hash as a distinct
    ///   discriminant so their later appearance recomputes;
    /// - the loader-config shapes (a REVERSE edge: caller files feed the
    ///   shapes this file's enrichment bakes) — hashed wholesale, so any
    ///   shape change over-invalidates rather than under.
    ///
    /// Extending enrichment with a new cross-file read means extending
    /// this key.
    /// The additive validity epoch for the enrichment-key memo: three
    /// monotone counters covering `enrichment_key`'s full read set —
    /// `gen_counter` (registration gens; every registration front door and
    /// @INC resolve mints), the freshness write count (fingerprints +
    /// dep-name edges; every mutating record/remove funnels through
    /// `FreshnessIndex` itself), and `shape_bumps` (cache-slot swaps +
    /// loader-shape rewrites). All only increase, so the sum moves whenever
    /// any leg does. A new mutation path must move one of the three legs —
    /// prefer bumping at the owning choke point, never at call sites.
    pub(super) fn enrichment_epoch(&self) -> u64 {
        self.core.gen_counter.load(std::sync::atomic::Ordering::Relaxed)
            + self.core.shape_bumps.load(std::sync::atomic::Ordering::Relaxed)
            + self.freshness.write_count()
    }

    /// `enrichment_key` behind the epoch memo, optimistic-read validated.
    ///
    /// ORDERING IS LOAD-BEARING: the epoch is read BEFORE the walk and
    /// re-read AFTER; the memo stores only when the two match. A key walk
    /// that raced a mutation reads a mix of old and new state — storing it
    /// under the post-mutation epoch would serve the torn key to every
    /// later consult until the NEXT unrelated write (a cached wrong value,
    /// amplified), where pre-memo a torn walk died after one use. The
    /// validated store keeps the memo's guarantee exactly the pre-memo one:
    /// a racing consult may answer from mixed state once, and the next
    /// consult recomputes.
    pub(super) fn enrichment_key_memoized(&self, cached: &Arc<CachedModule>) -> u64 {
        let epoch = self.enrichment_epoch();
        if let Some(m) = self.enrichment_key_memo.get(&cached.path) {
            if m.0 == epoch {
                crate::util::ghost_stats::count("enrichment_key.memo_hit");
                return m.1;
            }
        }
        let key = self.enrichment_key(cached);
        if self.enrichment_epoch() == epoch {
            // One overwritten-in-place entry per consulted path: bounded by
            // the number of registered files (~100 bytes each), never a
            // per-consult append.
            self.enrichment_key_memo.insert(cached.path.clone(), (epoch, key));
        }
        key
    }

    fn enrichment_key(&self, cached: &Arc<CachedModule>) -> u64 {
        crate::util::ghost_stats::count("enrichment_key");
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.registration_gen_of(&cached.path).hash(&mut h);
        self.freshness.fingerprint_of(&cached.path).unwrap_or(0).hash(&mut h);
        let mut seen: std::collections::HashSet<String> = Default::default();
        let mut frontier: Vec<String> = self.freshness.deps_of_names(&cached.path);
        frontier.sort_unstable();
        frontier.dedup();
        // Same bound as `resolve_method_in_ancestors` — enrichment's own
        // walks stop there too.
        for _depth in 0..20 {
            if frontier.is_empty() {
                break;
            }
            let mut next: Vec<String> = Vec::new();
            for dep in frontier {
                if !seen.insert(dep.clone()) {
                    continue;
                }
                dep.hash(&mut h);
                // EVERY candidate file of the dep rides the key — a losing
                // file's re-registration must move consumers' keys too
                // (over-invalidation, never staleness).
                let cands = crate::model::file_analysis::CrossFileLookup::def_candidates(self, &dep);
                if cands.is_empty() {
                    0u8.hash(&mut h);
                }
                for cm in &cands {
                    // Generation ALWAYS on the key, fingerprint too when
                    // recorded: enrichment's ctx-ful passes bake
                    // BODY-dependent provider facts the span-free
                    // fingerprint deliberately ignores, so a provider
                    // re-registration must move every consumer's key.
                    self.registration_gen_of(&cm.path).hash(&mut h);
                    match self.freshness.fingerprint_of(&cm.path) {
                        Some(fp) => {
                            1u8.hash(&mut h);
                            fp.hash(&mut h);
                            next.extend(self.freshness.deps_of_names(&cm.path));
                        }
                        None => {
                            2u8.hash(&mut h);
                            // @INC/recordless tier: its registration
                            // generation (minted at insert/warm by the
                            // resolver thread + `insert_cache`) already
                            // rode the key above — a re-resolve bumps it.
                            // No deps_of record; its parents ride the
                            // analysis itself.
                            for (_pkg, parents) in cm.analysis.package_parent_edges() {
                                next.extend(parents.iter().cloned());
                            }
                        }
                    }
                }
            }
            next.sort_unstable();
            next.dedup();
            frontier = next;
        }
        let mut shapes: Vec<(String, Vec<u8>)> = self
            .core
            .loader_config_shapes
            .iter()
            .map(|e| {
                let mut buf = Vec::new();
                for pair in e.value() {
                    buf.extend(bincode::serialize(pair).unwrap_or_default());
                }
                (e.key().clone(), buf)
            })
            .collect();
        shapes.sort();
        for (name, buf) in shapes {
            name.hash(&mut h);
            buf.hash(&mut h);
        }
        h.finish()
    }

    /// The transitive consumers of `path`'s last-recorded surface — the
    /// re-enrich set after a `Changed` verdict.
    pub fn dirty_consumers(
        &self,
        path: &std::path::Path,
    ) -> std::collections::HashSet<std::path::PathBuf> {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.freshness.dirty_consumers(&canon)
    }

    /// The pre-strip half of workspace registration: extract every declared
    /// package name and record the indexable-name list from the WHOLE
    /// analysis (a stripped copy's `symbols` is empty, so the later feeds
    /// replay this record). The candidate tables, edge feeds, and cache
    /// winner are all rebuilt by the residency half — on the deferred lane
    /// that is AFTER the blob commits, so an evicted copy is never
    /// name-reachable before it can rehydrate.
    pub(crate) fn workspace_feed_prestrip(
        &self,
        path: &std::path::Path,
        fa: &FileAnalysis,
    ) -> Vec<(String, bool)> {
        let names = package_names(fa);
        if !names.is_empty() {
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            self.core.edges.record_names(&canon, fa);
        }
        names
    }

    /// The residency half: the path-keyed registry + the cache slot. For a
    /// STRIPPED copy this must run only after its blob+rows are COMMITTED —
    /// an evicted copy registered before its persistence exists rehydrates
    /// to nothing and answers wrong-empty instead of "not yet indexed".
    /// Consumes a workspace token (minted only via `prepare_workspace_parts`
    /// in this module) — the same construct-is-proof discipline as
    /// `register_symbols_inner`. Surface recording is the caller's separate
    /// concern; the token's `surface` is dropped here.
    pub(crate) fn register_workspace_residency(
        &self,
        path: std::path::PathBuf,
        parts: WorkspaceRegistrationParts,
    ) {
        let WorkspaceRegistrationParts { arc, names, surface: _ } = parts;
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.bump_registration_gen(&path);
        let cached = Arc::new(CachedModule::new(path, arc));
        self.all_files.insert(cached.path.clone(), cached.clone());
        self.adopt_workspace_candidate(cached, names);
    }

    /// Registration-owned strip: feed the name/edge registrations from the
    /// WHOLE analysis, then evict the requested axes, then store the
    /// stripped arc — so a feed can never read an already-emptied `symbols`
    /// (the ordering bug a caller-side strip invites). Returns the stored
    /// arc for the caller's FileStore mirror. Synchronous-persistence
    /// callers only (the warm path — the blob already exists on disk);
    /// the bulk fresh path splits the halves around the writer's COMMIT.
    pub fn register_workspace_stripping(
        &self,
        path: std::path::PathBuf,
        fa: FileAnalysis,
        level: crate::model::file_analysis::Residency,
    ) -> Arc<FileAnalysis> {
        let mut parts = self.prepare_workspace_parts(&path, fa, level);
        parts.record_surface(self, &path);
        let arc = Arc::clone(parts.arc());
        self.register_workspace_residency(path, parts);
        arc
    }

    /// Remove a deleted workspace file's registrations — the path-keyed
    /// entry plus its name-keyed cache row and edges (a dead file must not
    /// stay a retrieval candidate or a phantom module).
    pub fn unregister_workspace_path(&self, path: &std::path::Path) {
        // The name-keyed cache slot drops below without a gen mint.
        crate::util::ghost_stats::count("epoch.shape.unregister_workspace_path");
        self.core.note_shape_change();
        self.remove_surface(path);
        // Registration keyed everything canonical; a deleted file can't
        // canonicalize, so rejoin through the parent (same fallback as
        // `remove_surface`).
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| {
            path.parent()
                .and_then(|d| std::fs::canonicalize(d).ok())
                .and_then(|d| path.file_name().map(|f| d.join(f)))
                .unwrap_or_else(|| path.to_path_buf())
        });
        self.all_files.remove(&canon);
        self.core.edges.remove_path_record(&canon);
        // The inverse of `record_workspace_projections`' shape half. Its own
        // retraction only fires when the SAME file re-registers, so a deleted
        // file would otherwise keep typing `$conf` in a plugin's `register`
        // from a contributor that no longer exists. Keyed exactly as the
        // recording side spells it.
        self.core.purge_loader_shapes(&canon.display().to_string());
        // `loaded_modules` deliberately has NO inverse: several files may load
        // one module, so dropping on one file's deletion would wrongly
        // un-suppress the entrypoint lint. Its reader is biased honest-quiet,
        // so never-remove is the safe direction there.
        if let Some((_, names)) = self.registered_names.remove(&canon) {
            for (name, _) in &names {
                if let Some(mut v) = self.core.all_defs.get_mut(name) {
                    v.retain(|c| c.path != canon);
                }
                self.core.all_defs.remove_if(name, |_, v| v.is_empty());
                // Survivors keep their edges and re-pick the winner; the
                // last candidate's departure empties the slot.
                self.rebuild_name_registration(name);
            }
            return;
        }
        // No record (registered through a legacy/test door): fall back to
        // the cache scan.
        let name = self.core.cache.iter().find_map(|entry| {
            entry
                .value()
                .as_ref()
                .filter(|cm| cm.path == canon || cm.path == path)
                .map(|_| entry.key().clone())
        });
        if let Some(name) = name {
            self.core.edges.purge_module(&name);
            self.core.cache.remove(&name);
            self.workspace_modules.remove(&name);
        }
    }

    #[cfg(test)]
    pub fn register_workspace_module(&self, path: std::path::PathBuf, analysis: Arc<FileAnalysis>) {
        // Loaded-module tracking feeds the entrypoint-scan lint and must
        // run BEFORE the packageless early-return: Mojolicious::Lite
        // scripts (no `package` decl) are exactly the entrypoints whose
        // `plugin 'X'` loads (via SyntheticUse imports) the lint needs
        // to see. Workspace scan re-runs every startup, so this set
        // needs no warm-rebuild feed.
        self.record_workspace_projections(&path, &analysis);
        self.register_workspace_resident(path, analysis);
    }

    /// Register a pack-language file under each CLASS name it defines —
    /// unlike Perl (one package per file), a C++ header / Python module
    /// holds many classes, and cross-file lookup is class-keyed. Language-
    /// GENERIC (keys on `SymKind::Class`), so every pack language gets
    /// cross-file from one indexer + its own per-language ModuleIndex.
    /// `get_cached("Box")` finds the file defining `Box`, and the same
    /// MethodOnClass / member-completion machinery resolves across files.
    /// Attach a per-language sub-index (`"cpp"`, `"python"`, …).
    pub fn attach_pack_index(&self, lang: &str, idx: Arc<ModuleIndex>) {
        // Share the hub's rehydration CELL (not its current contents — the
        // cell, so a later `set_workspace_root` install stays visible): the
        // sub-index can then serve a hub-owned path a sweep misroutes to it.
        if let Ok(mut g) = idx.foreign_bag_cache.write() {
            *g = Some(Arc::clone(&self.core.bag_cache));
        }
        self.pack_indexes.insert(lang.to_string(), idx);
    }

    /// Install this pack sub-index's Slice-2 bag-rehydration LRU before it is
    /// `Arc`-wrapped and registered. Consuming builder so the field is set once
    /// on the owned value (the index is shared immutably thereafter).
    pub fn with_bag_cache(
        self,
        cache: Arc<crate::index::pack_bag_cache::PackBagCache>,
    ) -> Self {
        self.set_bag_cache(cache);
        self
    }

    /// Post-`Arc` variant for the hub, set alongside the workspace root.
    /// LAST root wins — a re-rooted session must not keep rehydrating from
    /// the first root's DB while the writers moved to the new one.
    pub fn set_bag_cache(&self, cache: Arc<crate::index::pack_bag_cache::PackBagCache>) {
        if let Ok(mut g) = self.core.bag_cache.write() {
            *g = Some(cache);
        }
    }

    fn bag_cache_ref(&self) -> Option<Arc<crate::index::pack_bag_cache::PackBagCache>> {
        self.core.bag_cache.read().ok().and_then(|g| g.clone())
    }

    /// Install the relational ref index's read-connection opener (once).
    /// Callable post-`Arc` (interior `OnceLock`) because the hub is shared
    /// before the workspace root — and therefore the cache path — is known.
    pub fn set_ref_rows_opener(
        &self,
        opener: Arc<dyn Fn() -> Option<rusqlite::Connection> + Send + Sync>,
    ) {
        if let Ok(mut g) = self.ref_rows_opener.write() {
            *g = Some(opener);
        }
        // The retained conn belongs to the previous opener's DB.
        if let Ok(mut c) = self.ref_rows_conn.lock() {
            *c = None;
        }
    }

    /// Drop `path`'s rehydrated analysis from this index's LRU (a
    /// changed/saved file's copy is stale). Pack sub-indexes AND the Perl
    /// hub each carry one — the watcher and the bulk-index writers rely on
    /// this taking effect on both.
    pub fn invalidate_bag_cache(&self, path: &std::path::Path) {
        if let Some(bc) = self.bag_cache_ref() {
            bc.invalidate(path);
        }
    }

    /// Rehydrate `cached`'s whole persisted analysis through this index's
    /// LRU, degrading to the (evicted) resident copy on a miss rather than
    /// fabricating — the caller's query then answers as it would for a
    /// genuinely fact-less file. One body serves `bag_present` and
    /// `whole_present`: the miss policy and LRU selection must never diverge
    /// between the type path and the reference path.
    ///
    /// A miss here is ALWAYS an invariant break in-session: eviction is
    /// licensed only by a committed blob (persist-first), so an evicted
    /// registered copy that can't rehydrate means the blob vanished under
    /// us (a second writer's generation clobber, an external cache clear)
    /// or was never readable. Degrading keeps the server useful; the
    /// counter + strict mode keep it HONEST — under
    /// `PERL_LSP_STRICT_RESIDENCY=1` (the gold harness sets it) the miss
    /// panics so a run serving absence-as-answer fails loudly instead of
    /// scoring wrong results.
    pub(super) fn rehydrate_or_resident(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        self.rehydrate_axes_or_resident(cached, true)
    }

    /// The rows-axes twin: refs + symbols guaranteed, bag not promised —
    /// the LRU retains these entries bag-stripped so backward-walk traffic
    /// caches denser under the same cap. Same miss policy, same tripwire.
    pub(super) fn rehydrate_rows_or_resident(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        self.rehydrate_axes_or_resident(cached, false)
    }

    fn rehydrate_axes_or_resident(
        &self,
        cached: &Arc<CachedModule>,
        want_bag: bool,
    ) -> Arc<FileAnalysis> {
        let mut stage = "no bag cache installed on this index".to_string();
        if let Some(bc) = self.bag_cache_ref() {
            let got = crate::util::ghost_stats::timed("rehydrate.loader", || {
                if want_bag {
                    bc.bag_for_diag(&cached.path)
                } else {
                    bc.rows_for_diag(&cached.path)
                }
            });
            match got {
                Ok(full) => return full,
                // Discriminated cause (see `RehydrateMiss`) so the tripwire
                // below names the mechanism instead of shrugging.
                Err(miss) => stage = format!("loader miss: {miss}"),
            }
        }
        // Foreign route: sweeps mint `CachedModule`s from FileStore entries
        // and ask whatever index the query routed to — this index's own
        // loader can never serve a path a SIBLING tier persisted. One hop,
        // sibling's CACHE directly (never its `rehydrate_or_resident` — no
        // recursion): sub-index → the hub's cell; hub → the pack sibling
        // that registered the path.
        if let Some(fa) = self.rehydrate_foreign(&cached.path, want_bag) {
            return fa;
        }

        REHYDRATION_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        log::error!(
            "rehydration miss for evicted copy {:?} ({stage}) — serving stripped \
             resident (references/types for this file are quietly incomplete this \
             session)",
            cached.path
        );
        if crate::index::module_resolver::strict_residency() {
            panic!(
                "PERL_LSP_STRICT_RESIDENCY: evicted copy {:?} failed to rehydrate \
                 ({stage}). Refusing to serve absence-as-answer.",
                cached.path
            );
        }
        cached.analysis.clone()
    }

    /// A whole copy of `path` from the SIBLING tier that owns it — the
    /// cross-index half of `rehydrate_or_resident`. Exactly one hop, always
    /// through the sibling's cache (never its miss path), so routing can't
    /// recurse. `None` when no sibling owns the path — the caller's miss
    /// handling then applies.
    fn rehydrate_foreign(&self, path: &std::path::Path, want_bag: bool) -> Option<Arc<FileAnalysis>> {
        let ask = |bc: &crate::index::pack_bag_cache::PackBagCache| {
            if want_bag { bc.bag_for(path) } else { bc.rows_for(path) }
        };
        // Sub-index → the hub's cell (shared at `attach_pack_index`).
        let hub_cell = self.foreign_bag_cache.read().ok().and_then(|g| g.clone());
        if let Some(cell) = hub_cell {
            let hub_cache = cell.read().ok().and_then(|g| g.clone());
            if let Some(bc) = hub_cache {
                if let Some(fa) = ask(&bc) {
                    return Some(fa);
                }
            }
        }
        // Hub → the pack sibling that registered the path.
        for entry in self.pack_indexes.iter() {
            let sub = entry.value();
            if sub.all_files.contains_key(path) {
                if let Some(bc) = sub.bag_cache_ref() {
                    if let Some(fa) = ask(&bc) {
                        return Some(fa);
                    }
                }
            }
        }
        None
    }

    /// Does `m` declare a `Class`/type named `name`? Rank source for the
    /// cache-slot tie-break and the scoped/survivor re-picks. Reads the
    /// registration record first — the resident copy's `symbols` may be
    /// evicted — and falls back to the symbol scan for copies registered
    /// whole (recovery paths, tests).
    pub(super) fn module_defines_class(&self, m: &CachedModule, name: &str) -> bool {
        if let Some(rec) = self.registered_names.get(&m.path) {
            return rec.iter().any(|(n, is_class)| n == name && *is_class);
        }
        m.analysis
            .symbols()
            .iter()
            .any(|s| matches!(s.kind, SymKind::Class) && s.name == name)
    }

    /// Run `f` against this index's retained read connection to the
    /// relational row store, opening (or re-opening, if the DB file was
    /// unlinked/recreated) through the installed opener. `None` when no
    /// opener is set (tests, no cache dir) or the open fails. One retained
    /// connection per index so the statement cache amortizes across queries.
    pub(super) fn with_rows_conn<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> R) -> Option<R> {
        let opener = self.ref_rows_opener.read().ok().and_then(|g| g.clone())?;
        fn db_ino(conn: &rusqlite::Connection) -> u64 {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                return conn
                    .path()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.ino())
                    .unwrap_or(0);
            }
            #[cfg(not(unix))]
            {
                let _ = conn;
                0
            }
        }
        // Poison-proof: the Option is a pure cache — a panic in some earlier
        // holder must not permanently disable retrieval.
        let mut guard = self
            .ref_rows_conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((conn, ino)) = guard.as_ref() {
            if db_ino(conn) != *ino {
                *guard = None; // file unlinked/recreated — reopen below
            }
        }
        if guard.is_none() {
            *guard = opener().map(|c| {
                let ino = db_ino(&c);
                (c, ino)
            });
        }
        let (conn, _) = guard.as_ref()?;
        Some(f(conn))
    }

    /// Rows-backed workspace/symbol scan over THIS index's store — the
    /// enumeration surface for symbol-evicted copies. The hub serves Perl
    /// rows; callers fan out to `pack_index` sub-indexes for pack rows.
    pub fn sym_search(&self, query: &str) -> Vec<crate::index::module_cache::SymRowHit> {
        self.with_rows_conn(|conn| crate::index::module_cache::sym_rows_matching(conn, query))
            .unwrap_or_default()
    }

    /// The unused-exports view over THIS index's row store — exported syms
    /// with zero cross-file reference rows (`docs/adr/relational-ref-index.md`).
    /// `None` when the row store is unavailable (opener absent, cold cache);
    /// the caller degrades to the references projection.
    pub fn unused_exported_syms(&self) -> Option<Vec<crate::index::module_cache::DeadExportRow>> {
        self.with_rows_conn(crate::index::module_cache::unused_exported_syms)
    }

    /// The `--heatmap` pre-prune index: the DISTINCT ref-name-key set plus the
    /// shredded-path set (coverage witness). `None` when the row store is
    /// unavailable. The name set answers "could any file reference this name";
    /// the path set lets the caller verify the store actually covers the files
    /// its projection would scan before trusting an "absent ⇒ zero" verdict.
    pub fn ref_prune_index(
        &self,
    ) -> Option<(
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    )> {
        self.with_rows_conn(|conn| {
            (
                crate::index::module_cache::names_with_ref_rows(conn),
                crate::index::module_cache::paths_with_ref_rows(conn),
            )
        })
    }

    /// The sub-index for `lang`, if this distribution indexes it.
    pub fn pack_index(&self, lang: &str) -> Option<Arc<ModuleIndex>> {
        self.pack_indexes.get(lang).map(|e| e.value().clone())
    }

    /// The ONE speller of verb-routing store selection: which index serves
    /// cross-file queries for a file of `language` — its pack sub-index when
    /// attached, else this hub (Perl always; a pack language before its
    /// index attaches). Handlers and CLI mirrors hold the returned value
    /// and pass `as_lookup()` into `resolve()`; the pack POLICY is not
    /// decided here — the CandidateSet derives it from the origin's stamped
    /// `FileAnalysis.language`. A layering tripwire keeps `pack_index()`
    /// itself out of the LSP layer so this stays the only spelling.
    pub fn lookup_for(&self, language: &str) -> RoutedIndex<'_> {
        match self.pack_index(language) {
            Some(p) => RoutedIndex::Pack(p),
            None => RoutedIndex::Hub(self),
        }
    }

    /// Every attached pack-language sub-index, by language id. The CLI
    /// diagnostics path sweeps these for Mode-B (member-op) diagnostics —
    /// pack files live here, not in the Perl-only `FileStore` the whole-tree
    /// pass iterates.
    pub fn for_each_pack_index(&self, mut f: impl FnMut(&str, &Arc<ModuleIndex>)) {
        for entry in self.pack_indexes.iter() {
            f(entry.key(), entry.value());
        }
    }

    /// Register a pack-language file's named top-level entities — classes
    /// AND free functions — by name, so cross-file member completion (`obj.`
    /// → the class's file) and function goto-def (`compute()` → the file that
    /// declares/defines it) both resolve. Last writer wins on a name
    /// collision; a function_definition registered after its prototype means
    /// goto-def lands on the body. Methods need no entry — they resolve
    /// through their class's file.
    pub fn register_symbols(&self, path: std::path::PathBuf, analysis: Arc<FileAnalysis>) {
        // Feed source and stored copy are the same whole analysis here;
        // indexers that strip go through `register_symbols_stripping`.
        // `whole` mints the deliberate whole-copy token (feed + surface off
        // the unstripped arc) — the only door that pins a resident analysis.
        let mut parts = PackRegistrationParts::whole(analysis);
        parts.record_surface(self, &path);
        self.register_symbols_inner(path, parts);
    }

    /// Registration-owned strip for the pack bulk index: collect the
    /// linkage-visible feed from the WHOLE analysis, evict the requested
    /// axes, then register the stripped arc — the feeds can never read an
    /// already-emptied `symbols`. Returns the stored arc (the worker sends
    /// it to the persist writer).
    /// The pack feed half, computed on the WHOLE analysis pre-strip: the
    /// linkage-visible (name, is-class) pairs plus the specialization
    /// edges, in the exact shape `register_symbols_inner` consumes.
    pub(crate) fn prepare_pack_feed(
        fa: &FileAnalysis,
    ) -> (Vec<(String, bool)>, Vec<(String, String)>) {
        let feed = collect_linkage_feed(fa);
        let specs: Vec<(String, String)> =
            fa.pack.specializes.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        (feed, specs)
    }

    /// The Perl-workspace twin of `prepare_pack_parts`: the name feed and
    /// the surface project from the WHOLE analysis, THEN the requested axes
    /// evict, then the arc is minted. `register_workspace_stripping` and the
    /// fresh workspace worker both route here so the reads-whole-before-
    /// evict ordering has one speller per tier.
    pub(crate) fn prepare_workspace_parts(
        &self,
        path: &std::path::Path,
        mut fa: FileAnalysis,
        level: crate::model::file_analysis::Residency,
    ) -> WorkspaceRegistrationParts {
        let names = self.workspace_feed_prestrip(path, &fa);
        let surface = crate::model::surface::Surface::project(&fa);
        fa.evict_to(level);
        WorkspaceRegistrationParts { arc: Arc::new(fa), names, surface: Some(surface) }
    }

    /// The ONE speller of the pack strip ordering: feed + specs + surface
    /// project from the WHOLE analysis, THEN the requested axes evict, then
    /// the arc is minted. Every pack registration that strips (bulk warm,
    /// fresh worker, edit swap) routes here so the "reads-whole-before-
    /// evict" invariant can't drift between separately-spelled copies —
    /// and the stub encoder gets exactly the halves registration used.
    pub(crate) fn prepare_pack_parts(
        mut fa: FileAnalysis,
        level: crate::model::file_analysis::Residency,
    ) -> PackRegistrationParts {
        let (feed, specs) = Self::prepare_pack_feed(&fa);
        let surface = crate::model::surface::Surface::project(&fa);
        fa.evict_to(level);
        PackRegistrationParts { arc: Arc::new(fa), feed, specs, surface: Some(surface) }
    }

    pub fn register_symbols_stripping(
        &self,
        path: std::path::PathBuf,
        fa: FileAnalysis,
        level: crate::model::file_analysis::Residency,
    ) -> Arc<FileAnalysis> {
        let mut parts = Self::prepare_pack_parts(fa, level);
        parts.record_surface(self, &path);
        let arc = Arc::clone(parts.arc());
        self.register_symbols_inner(path, parts);
        arc
    }

    /// Register a pack file from its token. Consuming the token IS the proof
    /// the caller went through a mint choke point (`prepare_pack_parts` /
    /// `whole` / `from_warm_stub`) — a loose resident arc can't reach here.
    /// Surface recording is the caller's separate concern (the deferred
    /// writer records pre-COMMIT; the token's `surface` is dropped here).
    pub(crate) fn register_symbols_inner(
        &self,
        path: std::path::PathBuf,
        parts: PackRegistrationParts,
    ) {
        let PackRegistrationParts { arc: analysis, feed, specs: specializes, surface: _ } = parts;
        let feed = &feed;
        let specializes = &specializes;
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.bump_registration_gen(&path);
        let cached = Arc::new(CachedModule::new(path, analysis.clone()));
        // The registration record FIRST — before all_files/all_defs/cache
        // publish this (possibly symbol-stripped) copy: a concurrent
        // registration's tie-break consults the record via
        // `module_defines_class`, and the fallback symbol scan on a
        // stripped copy misjudges every Class as a value.
        self.registered_names.insert(cached.path.clone(), feed.to_vec());
        // Unconditional: even a file declaring nothing registrable (an
        // include-only shim) must be reachable by whole-project sweeps.
        self.all_files.insert(cached.path.clone(), cached.clone());
        for (name, is_class) in feed {
            let sym_name = name;
            let incoming_is_class = *is_class;
            self.workspace_modules.insert(sym_name.clone(), ());
            // Keep EVERY candidate for `name` (not just the winner below), so a
            // scoped query can pick the one reachable from its include closure.
            // Keyed by path: a re-registered file (edit, or prototype→definition)
            // REPLACES its own candidate so the scoped lookup never serves a
            // stale analysis, and duplicate paths never stack.
            {
                let mut v = self.core.all_defs.entry(sym_name.clone()).or_default();
                match v.iter().position(|c| c.path == cached.path) {
                    Some(i) => v[i] = cached.clone(),
                    None => v.push(cached.clone()),
                }
            }
            // A TYPE wins the cache slot over a callable/value of the same
            // name. C reuses names freely — a `#define OP(x)` macro in one
            // header (a Sub) vs the `OP` typedef in another (a Class). Member
            // completion + ancestor resolution key on the TYPE, so a Class
            // beats a Sub/value.
            //
            // When two WORKSPACE files define the SAME name at the SAME rank
            // (two files each declaring `class Box` — common with test
            // fixtures / vendored copies), the winner MUST NOT depend on the
            // parallel registration order, or `get_cached(name)` flips
            // per-process (the Rayon index registers in nondeterministic
            // order). Break the tie by the smallest canonical path — a
            // stable, order-independent choice.
            use dashmap::mapref::entry::Entry;
            match self.core.cache.entry(sym_name.clone()) {
                Entry::Vacant(v) => {
                    v.insert(Some(cached.clone()));
                }
                Entry::Occupied(mut o) => {
                    let replace = match o.get() {
                        None => true,
                        Some(existing) => {
                            let existing_is_class =
                                self.module_defines_class(existing, sym_name);
                            match (incoming_is_class, existing_is_class) {
                                (true, false) => true,  // Class beats Sub/value
                                (false, true) => false, // Sub/value never beats a Class
                                // Same rank: deterministic by path.
                                _ => cached.path < existing.path,
                            }
                        }
                    };
                    if replace {
                        o.insert(Some(cached.clone()));
                    }
                }
            }
        }
        // Specialization family edges: primary → spec NAMES. A spec's Class
        // symbol registered above makes `get_cached(spec_name)` resolve, so
        // the reverse map's values are the same by-name keys the rest of the
        // pack index uses. A stale entry (edited file dropped a spec)
        // self-heals at read: `direct_specializations_of` re-checks the pair
        // against the CURRENT analysis.
        for (spec, primary) in specializes {
            self.core.edges.publish_spec(primary, spec);
        }
        // Inverse inheritance edges: parent → child NAMES, so
        // `direct_children_of` (the INHERITS_INV cross-file leg the
        // implementations verb walks) can find the subclasses of a base. The
        // Perl `feed()` path populates `children` via `parent_classes`; the
        // pack path builds it here (it bypasses `feed`). Symmetric with the
        // spec map above: the child NAME is a by-name key `get_cached`
        // resolves, and `direct_children_of` re-checks each candidate's CURRENT
        // `package_parents`, so a stale entry (an edit dropped a base)
        // self-heals at read. `package_parents` survives every strip
        // (`evict_to` leaves it) and rides the warm-stub skeleton, so the arc
        // carries it on the fresh, warm, and whole paths alike.
        for (child, parents) in analysis.package_parent_edges() {
            for parent in parents {
                self.core.edges.publish_child(parent, child);
            }
        }
    }

    /// Remove a pack file's registrations: its `all_files` entry, its
    /// candidates in `all_defs`, and any global cache-slot wins — re-picking
    /// the winner among the remaining candidates with the SAME total order
    /// registration uses, so the slot never dangles on a deleted/edited file.
    /// The in-session inverse of `register_symbols` (deletes; and a changed
    /// file unregisters first so names its new version no longer defines
    /// don't linger).
    pub fn unregister_file(&self, path: &std::path::Path) {
        // Cache-slot re-picks below change `get_cached` answers without a
        // gen mint — the epoch must move or the enrichment-key memo lies.
        crate::util::ghost_stats::count("epoch.shape.unregister_file");
        self.core.note_shape_change();
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if self.all_files.remove(&canon).is_none() {
            return;
        }
        // Symbols may be evicted on the resident copy, and rehydration
        // would fetch the WRONG generation after an edit persists — so the
        // inverse runs on the name list registration recorded, not on
        // `old.analysis.symbols`.
        let names = self
            .registered_names
            .remove(&canon)
            .map(|(_, v)| v)
            .unwrap_or_default();
        for (name, _) in &names {
            if let Some(mut v) = self.core.all_defs.get_mut(name) {
                v.retain(|c| c.path != canon);
            }
            self.core.all_defs.remove_if(name, |_, v| v.is_empty());
            let survivor = self
                .core
                .all_defs
                .get(name)
                .and_then(|v| best_candidate(&v.iter().collect::<Vec<_>>(), name, &|m, n| {
                    self.module_defines_class(m, n)
                }));
            // Only touch the cache slot if the departing file held it.
            let held = self
                .core
                .cache
                .get(name)
                .map(|e| matches!(e.value(), Some(c) if c.path == canon))
                .unwrap_or(false);
            if held {
                match survivor {
                    Some(cand) => {
                        self.core.cache.insert(name.clone(), Some(cand));
                    }
                    None => {
                        self.core.cache.remove(name);
                    }
                }
            }
            if !self.core.all_defs.contains_key(name) {
                self.workspace_modules.remove(name);
            }
        }
    }

    /// Every file registered via `register_symbols` — the reverse-dependency
    /// sweep surface (a changed header's consumers are the registered files
    /// whose `include_closure` contains it).
    /// The residency tripwire's observable: fully-resident registered
    /// copies. After a bulk index with eviction on, every one of these must
    /// be accounted for by a deliberate whole-copy site (writer fallback,
    /// degraded/unpersisted analysis) — an unexplained count means a
    /// registration path is silently pinning whole analyses (the RAM
    /// regression no functional test can see).
    pub fn count_fully_resident(&self) -> usize {
        let mut n = 0usize;
        self.for_each_registered_file(&mut |cm| {
            if cm.analysis.is_fully_resident() {
                n += 1;
            }
        });
        n
    }

    pub fn for_each_registered_file(&self, f: &mut dyn FnMut(&Arc<CachedModule>)) {
        for entry in self.all_files.iter() {
            f(entry.value());
        }
    }

    /// Iterate every pack-language (C/C++/…) registered file's analysis. Pack
    /// symbols live in per-language sub-indexes, not the Perl FileStore
    /// workspace map, so `workspace/symbol` must sweep them separately or a
    /// C typedef/class/function never surfaces in a workspace search.
    pub fn for_each_pack_registered_file(
        &self,
        f: &mut dyn FnMut(&std::path::Path, &FileAnalysis),
    ) {
        for entry in self.pack_indexes.iter() {
            entry
                .value()
                .for_each_registered_file(&mut |cached| f(&cached.path, &cached.analysis));
        }
    }

    /// Rebuild the reverse index (`func → modules`) from the current cache.
    /// `warm_cache` writes straight into `cache_raw()` and never touches the
    /// reverse index, so a CLI/full-startup warm path that skips this leaves
    /// `find_exporters` blind to cached modules — the warm run then degrades
    /// "exported by X (not yet imported)" hints to a bare "not defined"
    /// (the B6 cold/warm attribution regression). The resolver thread already
    /// calls the equivalent rebuild after its own warm.
    pub fn rebuild_reverse_index_from_cache(&self) {
        self.core.rebuild_reverse_index();
        // The cache holds one WINNER per name; a same-name sibling file's
        // edges live only through the candidate table — re-feed every
        // candidate (idempotent) so a rebuild never blinds
        // `modules_with_symbol` to a reopened package's other files.
        for entry in self.core.all_defs.iter() {
            for c in entry.value().iter() {
                self.core.edges.feed(entry.key(), &c.path, &c.analysis);
            }
        }
    }
}
