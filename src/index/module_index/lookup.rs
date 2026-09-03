//! Bridged-entity enumeration, descendant walks, blocking resolution, and the
//! `CrossFileLookup` trait impl.

use super::*;

impl ModuleIndex {
    /// Every cached module that declares at least one `PluginNamespace`
    /// whose `bridges` list includes `Bridge::Class(class_name)`.
    /// Callers then pull the namespace's entities from the module's
    /// `FileAnalysis` and iterate. Explicit bridges rather than
    /// symbol-package inference.
    /// Is `module` imported by any workspace file (entrypoint scripts
    /// included)? `false` is honest only after the workspace scan has
    /// run — callers on the diagnostics path are post-startup.
    ///
    /// Matching is exact OR last-segment tail: a `plugin 'DataLog'`
    /// load records the default-namespace guess
    /// (`Mojolicious::Plugin::DataLog`) while the resolved provider
    /// may live in an app-custom tree (`Clove::App::Plugin::DataLog`).
    /// The looseness only SUPPRESSES the lint — the honest-quiet
    /// direction.
    pub fn is_module_loaded(&self, module: &str) -> bool {
        if self.loaded_modules.contains_key(module) {
            return true;
        }
        let tail = module.rsplit("::").next().unwrap_or(module);
        self.loaded_modules
            .iter()
            .any(|e| e.key().rsplit("::").next() == Some(tail))
    }

    /// Was `module` registered from the workspace tree (vs @INC)?
    pub fn is_workspace_module(&self, module: &str) -> bool {
        self.workspace_modules.contains_key(module)
    }

    /// Modules with a sub/method ATTRIBUTED to `package` — the class-keyed
    /// provider bucket (`ModuleEdgeIndexes::providers`). Sorted, so the
    /// caller's first match is stable across runs.
    pub fn modules_providing_package(&self, package: &str) -> Vec<String> {
        match self.core.edges.providers.get(package) {
            Some(bucket) => {
                let mut result = bucket.as_slice().to_vec();
                result.sort();
                result
            }
            None => Vec::new(),
        }
    }

    pub fn modules_bridging_to(&self, class_name: &str) -> Vec<String> {
        match self.core.edges.bridges.get(class_name) {
            Some(bucket) => {
                // The bucket is unique by construction; sort only, so the
                // order is stable across runs.
                let mut result = bucket.as_slice().to_vec();
                result.sort();
                result
            }
            None => Vec::new(),
        }
    }

    /// Every cached module containing a package that directly lists
    /// `class_name` as a parent (`isa` or role composition — the
    /// `children` edge map). Direct children only; transitive walks go
    /// through the graph walk (`walk(INHERITS_INV)`) — this is the
    /// depth-1 edge it composes.
    pub fn modules_with_parent(&self, class_name: &str) -> Vec<String> {
        match self.core.edges.children.get(class_name) {
            Some(bucket) => {
                // The bucket is unique by construction; sort only, so the
                // order is stable across runs.
                let mut result = bucket.as_slice().to_vec();
                result.sort();
                result
            }
            None => Vec::new(),
        }
    }

    /// Breadth-first walk over the inverse inheritance/composition
    /// graph from `class`: every (package, module) pair whose package
    /// transitively `isa`/composes `class`. Bounded by a package
    /// seen-set (cycles, diamonds) and a fan-out cap; never does I/O.
    ///
    /// The independent BFS oracle the graph-walk descendant test
    /// cross-checks `walk(INHERITS_INV)` against. Production reachability
    /// goes through the graph walk; this stays test-only so the two
    /// implementations can disagree loudly.
    #[cfg(test)]
    pub fn for_each_descendant_package<F>(&self, class: &str, mut visit: F)
    where
        F: FnMut(&str, &Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    {
        const MAX: usize = 512;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<String> =
            std::collections::VecDeque::from([class.to_string()]);
        let mut visited = 0usize;
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current.clone()) {
                continue;
            }
            visited += 1;
            if visited > MAX {
                break;
            }
            for module_name in self.modules_with_parent(&current) {
                // Every candidate file of the name — the parent edge may
                // live in a losing candidate (packages lane, never evicted).
                for cached in crate::model::file_analysis::CrossFileLookup::def_candidates(self, &module_name) {
                // A module can hold several packages; only the ones
                // actually listing `current` as a parent are children.
                for (pkg, parents) in cached.analysis.package_parent_edges() {
                    if !parents.iter().any(|p| p == &current) {
                        continue;
                    }
                    if seen.contains(pkg) {
                        continue;
                    }
                    if visit(pkg, &cached).is_break() {
                        return;
                    }
                    queue.push_back(pkg.clone());
                }
                }
            }
        }
    }

    /// Run a closure on every `Symbol` reachable from `class_name`
    /// through a plugin bridge. The namespace-wide `Bridge::Class(X)`
    /// identifies which namespaces to walk; per-entity `package`
    /// narrows to "this entity is bound on class X specifically".
    /// Lets one namespace span multiple classes (Mojo helpers on
    /// both Controller and Mojolicious, plus each proxy class in a
    /// dotted chain) without every entity being visible on every
    /// bridged class.
    ///
    /// Single source of truth for plugin-synthesized entity lookup
    /// across files — explicit bridges rather than a per-caller
    /// `symbol.package` scan.
    pub fn for_each_entity_bridged_to(
        &self,
        class_name: &str,
        // `mod_name` is the cache key the bridging module is registered under
        // — the authoritative handle for a follow-up `get_cached(mod_name)`.
        // Don't re-derive it from the analysis: the registration name and the
        // file's first `package` can differ.
        //
        // `Break` stops the walk BEFORE the next candidate's `symbols_present`
        // — the rehydrate is the cost, so a first-match caller that could not
        // stop the iteration paid a decode per bridging module for nothing.
        visit: impl FnMut(
            &str,
            &Arc<CachedModule>,
            &crate::model::file_analysis::Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        self.bridged_walk(class_name, None, visit)
    }

    /// The one speller of the bridged-entity loop. `name_hint` is the
    /// first-match-by-name callers' pre-filter license: a candidate whose
    /// syms rows PROVE nothing named `name_hint` is skipped before its
    /// `symbols_present` rehydrate — the per-miss decode of every bridging
    /// module was the cost the early exit alone could not remove. Fail-open
    /// everywhere the store cannot speak, container-BLIND (an entity's
    /// container is the plugin's home package, not the bridged class), and
    /// the visitor's semantics are unchanged for every candidate reached.
    fn bridged_walk(
        &self,
        class_name: &str,
        name_hint: Option<&str>,
        mut visit: impl FnMut(
            &str,
            &Arc<CachedModule>,
            &crate::model::file_analysis::Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        use crate::model::file_analysis::CrossFileLookup;
        for mod_name in self.modules_bridging_to(class_name) {
            // Every candidate file of the name — the bridging namespace may
            // live in a losing candidate.
            for cached in CrossFileLookup::def_candidates(self, &mod_name) {
            if let Some(name) = name_hint {
                if !self.candidate_may_name(&cached, name) {
                    crate::util::ghost_stats::count("bridged.candidate_prefiltered");
                    continue;
                }
            }
            // Entities index into `symbols`, which may be evicted on the
            // resident copy — resolve them against the symbols-axis view
            // (same generation: the LRU is invalidated on every rewrite).
            // Namespaces ride the never-evicted plugin lane, so symbols are
            // the only evictable axis this read touches.
            let whole = self.symbols_present(&cached);
            for ns in &whole.plugin.namespaces {
                let bridges_class = ns.bridges.iter().any(|b|
                    matches!(b, crate::model::file_analysis::Bridge::Class(c) if c == class_name));
                if !bridges_class { continue; }
                // Namespace membership IS the filter — if this namespace
                // bridges to `class_name`, every entity it owns is
                // visible from `class_name`. No `sym.package` gate: the
                // plugin picks ONE canonical home package and the
                // namespace's bridges control visibility, so no
                // per-bridge Method fan-out is needed.
                for sym_id in &ns.entities {
                    let idx = sym_id.0 as usize;
                    let Some(sym) = whole.symbols().get(idx) else { continue };
                    if visit(&mod_name, &cached, sym).is_break() {
                        return;
                    }
                }
            }
            }
        }
    }

    /// The name-only sibling of `candidate_may_declare`, for walks whose
    /// member test is container-blind (bridged entities live under the
    /// plugin's home package). Same guards, same skip license: only a
    /// covered store's proven absence skips.
    fn candidate_may_name(&self, cached: &Arc<CachedModule>, name: &str) -> bool {
        if !cached.analysis.symbols_are_evicted() {
            return true;
        }
        static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DISABLED
            .get_or_init(|| std::env::var_os("PERL_LSP_NO_MEMBER_PREFILTER").is_some())
        {
            return true;
        }
        let rows = self.with_rows_conn(|conn| {
            crate::index::module_cache::sym_name_row_exists(
                conn,
                &cached.path.to_string_lossy(),
                name,
            )
        });
        member_prefilter_may_declare(
            !cached.analysis.plugin.gated_emissions.is_empty(),
            rows,
        )
    }

    /// Block until `module_name` appears in the cache, or timeout.
    /// (Used by tests and the one-shot CLI import resolution.)
    #[doc(hidden)]
    pub fn wait_resolved(&self, module_name: &str, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut guard = self.core.resolved.mu.lock().unwrap();
        loop {
            if self.core.cache.contains_key(module_name) {
                return true;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (g, result) = self.core.resolved.cv.wait_timeout(guard, remaining).unwrap();
            guard = g;
            if result.timed_out() && !self.core.cache.contains_key(module_name) {
                return false;
            }
        }
    }

    /// Get cached module synchronously. WARNING: Does blocking I/O. Only for tests.
    #[cfg(test)]
    pub fn get_cached_blocking(&self, module_name: &str) -> Option<Arc<CachedModule>> {
        if let Some(entry) = self.core.cache.get(module_name) {
            return entry.clone();
        }
        let inc_paths = module_resolver::discover_inc_paths();
        let mut parser = module_resolver::create_parser();
        let result = module_resolver::resolve_and_parse(&inc_paths, module_name, &mut parser);
        self.core.note_shape_change();
        self.core.cache.insert(module_name.to_string(), result.clone());
        result
    }

    #[cfg(test)]
    pub(super) fn inc_paths(&self) -> Vec<PathBuf> {
        module_resolver::discover_inc_paths()
    }

    #[cfg(test)]
    pub fn resolve_module(&self, module_name: &str) -> Option<PathBuf> {
        let inc_paths = module_resolver::discover_inc_paths();
        module_resolver::resolve_module_path(&inc_paths, module_name)
    }
}

/// The capability `file_analysis`/`witnesses` query against. Delegates to
/// the inherent methods (inherent wins name resolution on `self`, so no
/// recursion); the generic inherent iterators accept the `&mut dyn FnMut`
/// trampolines directly.
impl CrossFileLookup for ModuleIndex {
    fn resolution_epoch(&self) -> u64 {
        // The same additive counter the enrichment-key memo validates
        // against — one home for "has anything a cross-file read depends
        // on moved", so a new mutation path bumps one leg and every memo
        // riding it invalidates together.
        self.enrichment_epoch()
    }

    fn flush_epoch(&self) -> u64 {
        self.core.flush_epoch.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn restamp_owed(&self, path: &std::path::Path, stamped_at: Option<u64>) -> bool {
        // Never stamped: owed, unconditionally. Every rehydrated copy reads
        // `None` (the field is `serde(skip)`), so this is the arm that keeps a
        // cold process behaving exactly as it did before the gate existed.
        let Some(stamped_at) = stamped_at else {
            crate::util::ghost_stats::count("restamp.owed_never_stamped");
            return true;
        };
        match self.core.provider_diff_gen.get(path) {
            Some(mark) if stamped_at >= *mark => {
                crate::util::ghost_stats::count("restamp.skipped");
                false
            }
            Some(_) => {
                crate::util::ghost_stats::count("restamp.owed_provider_moved");
                true
            }
            // No mark at all. Not "no provider moved" — "no wave has ever
            // spoken about this file", which is also what a lost mark, a
            // never-flushed session and an uncovered freshness edge all look
            // like. Fail open; the gate is worth only what it can prove.
            None => {
                crate::util::ghost_stats::count("restamp.owed_no_mark");
                true
            }
        }
    }

    fn get_cached(&self, module_name: &str) -> Option<Arc<CachedModule>> {
        self.get_cached(module_name)
    }

    fn get_cached_scoped(
        &self,
        module_name: &str,
        visible: &std::collections::HashSet<String>,
    ) -> Option<Arc<CachedModule>> {
        self.get_cached_scoped(module_name, visible)
    }


    fn whole_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        if cached.analysis.is_fully_resident() {
            return cached.analysis.clone();
        }
        self.rehydrate_or_resident(cached)
    }

    fn symbols_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        // The @INC strip is bag-only, so import-tier copies (the MRO
        // walk's ancestor set) answer resident; workspace copies are
        // symbol-evicted and rehydrate through the rows-axes lane —
        // bag-stripped LRU entries, so the ancestry storm's working set
        // fits the cap instead of cycling whole copies through it.
        if !cached.analysis.symbols_are_evicted() {
            return cached.analysis.clone();
        }
        self.rehydrate_rows_or_resident(cached)
    }

    fn prefetch_refs(&self, paths: &[std::path::PathBuf]) {
        use rayon::prelude::*;
        if paths.len() < 2 || std::env::var_os("PERL_LSP_REFS_NO_PREFETCH").is_some() {
            return;
        }
        // Bounded so a giant candidate set cannot churn the byte-capped LRU
        // past the entries the walk is about to read.
        let take = paths.len().min(4096);
        crate::util::ghost_stats::timed("refs.prefetch", || {
            paths[..take].par_iter().for_each(|p| {
                if let Some(cm) = self.cached_by_path(p) {
                    let _ = self.refs_present(&cm);
                }
            });
        });
    }
    fn refs_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        // Backward-walk view: refs AND symbols usable (the matcher reads
        // usage rows + declaration rows). The @INC strip is bag-only, so
        // import-tier copies answer resident; the workspace strip evicts
        // both row axes and rehydrates — never resident-or-empty, or an
        // evicted-empty ref table would read as "this file has no matching
        // refs" and `references` silently under-reports.
        if !cached.analysis.refs_are_evicted() && !cached.analysis.symbols_are_evicted() {
            return cached.analysis.clone();
        }
        self.rehydrate_rows_or_resident(cached)
    }

    fn sweep_consult_answer(
        &self,
        path: &std::path::Path,
        key: &crate::model::witnesses::ConsultVerdictKey,
    ) -> Option<Arc<crate::model::witnesses::ReducedValue>> {
        let guard = super::registration::sweep_answers_read()?;
        let sa = guard.as_ref()?;
        let stamp = self.core.shape_bumps.load(std::sync::atomic::Ordering::Relaxed);
        if sa.stamp_load() != stamp {
            // Lazy reset, same rule as SweepMemo: a shape change mid-sweep
            // voids every remembered verdict.
            sa.reset_to(stamp);
            crate::util::ghost_stats::count("sweepans.invalidated");
            return None;
        }
        let hit = sa.get(path, key);
        crate::util::ghost_stats::count(if hit.is_some() {
            "sweepans.hit"
        } else {
            "sweepans.miss"
        });
        hit
    }

    fn remember_sweep_consult(
        &self,
        path: &std::path::Path,
        key: &crate::model::witnesses::ConsultVerdictKey,
        value: &crate::model::witnesses::ReducedValue,
    ) {
        if let Some(guard) = super::registration::sweep_answers_read() {
            if let Some(sa) = guard.as_ref() {
                let stamp =
                    self.core.shape_bumps.load(std::sync::atomic::Ordering::Relaxed);
                if sa.stamp_load() == stamp {
                    sa.insert(path, key, value);
                    crate::util::ghost_stats::count("sweepans.store");
                }
            }
        }
    }

    fn candidate_may_declare(
        &self,
        cached: &Arc<CachedModule>,
        name: &str,
        class: &str,
    ) -> bool {
        // Gate on eviction FIRST: a symbols-resident copy answers the member
        // probe from RAM for free, and it may be fresher than its rows (the
        // whole-copy registration paths precede persist). Stripped copies
        // register only AFTER their chunk commits, so for them the rows are
        // at least as fresh as the copy — the freshness argument the skip
        // leans on.
        if !cached.analysis.symbols_are_evicted() {
            return true;
        }
        static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DISABLED
            .get_or_init(|| std::env::var_os("PERL_LSP_NO_MEMBER_PREFILTER").is_some())
        {
            return true;
        }
        let rows = self.with_rows_conn(|conn| {
            crate::index::module_cache::sym_member_row_exists(
                conn,
                &cached.path.to_string_lossy(),
                name,
                class,
            )
        });
        member_prefilter_may_declare(
            !cached.analysis.plugin.gated_emissions.is_empty(),
            rows,
        )
    }

    fn candidate_bag_may_answer(
        &self,
        cached: &Arc<CachedModule>,
        name: &str,
        class: &str,
        attributed: bool,
    ) -> bool {
        // Same freshness gate as `candidate_may_declare`: only a stripped
        // copy registered AFTER its chunk commit is provably no fresher
        // than its rows. Whole and RowsOnly (@INC-tier) copies fail open.
        if !cached.analysis.symbols_are_evicted() {
            return true;
        }
        static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DISABLED
            .get_or_init(|| std::env::var_os("PERL_LSP_NO_CONSULT_PREFILTER").is_some())
        {
            return true;
        }
        let path = cached.path.to_string_lossy();
        // Raw name only: the probes own the spelling policy (raw + match
        // key — `rows::probe_spelling`). `None` (file never shredded)
        // dominates `Some(false)` — fail open.
        let rows = self.with_rows_conn(|conn| {
            if attributed {
                crate::index::module_cache::sym_member_row_exists(conn, &path, name, class)
            } else {
                crate::index::module_cache::name_row_exists(conn, &path, name)
            }
        });
        member_prefilter_may_declare(
            !cached.analysis.plugin.gated_emissions.is_empty(),
            rows,
        )
    }

    fn ref_candidate_paths(&self, keys: &[String]) -> Vec<std::path::PathBuf> {
        self.with_rows_conn(|conn| {
            crate::index::module_cache::ref_candidate_files(conn, keys)
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
    }

    fn ref_indexed_paths(&self) -> std::collections::HashSet<std::path::PathBuf> {
        self.with_rows_conn(|conn| {
            crate::index::module_cache::paths_with_ref_rows(conn)
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
    }

    fn cached_by_path(&self, path: &std::path::Path) -> Option<Arc<CachedModule>> {
        // Pack sub-indexes: the per-path registry is O(1). The Perl hub's
        // cache is name-keyed with unique paths — linear fallback (hub
        // candidate sets are @INC-sized, not tree-sized).
        if let Some(cm) = self.all_files.get(path) {
            return Some(cm.value().clone());
        }
        self.core.cache.iter().find_map(|entry| {
            entry
                .value()
                .as_ref()
                .filter(|cm| cm.path == path)
                .cloned()
        })
    }

    fn enriched_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        if !self.core.long_lived.load(std::sync::atomic::Ordering::Relaxed) {
            return self.bag_present(cached);
        }
        self.enriched_snapshot(cached)
            .unwrap_or_else(|| self.bag_present(cached))
    }

    fn serves_enriched(&self) -> bool {
        // The overlay is long-lived-only (the deep copies never pay for
        // themselves in a one-shot process), so a not-long-lived index's
        // `enriched_present` is `bag_present` — never distinct.
        self.core.long_lived.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn class_is_bridged_to(&self, class: &str) -> bool {
        // Bucket NON-EMPTY, not key-present: `purge_module` can leave an empty
        // bucket behind, and a key-exists test would then report a bridge that
        // no longer exists — permanently pessimising a class that used to be
        // bridged.
        self.core
            .edges
            .bridges
            .get(class)
            .is_some_and(|b| !b.is_empty())
    }

    fn surface_fingerprint_of(&self, path: &std::path::Path) -> Option<u64> {
        self.freshness.fingerprint_of(path)
    }

    fn closedness_certificate(
        &self,
        class: &str,
    ) -> Option<std::sync::Arc<crate::model::witnesses::ClosednessCertificate>> {
        self.closedness.get(class)
    }

    fn store_closedness_certificate(
        &self,
        class: &str,
        cert: std::sync::Arc<crate::model::witnesses::ClosednessCertificate>,
    ) {
        self.closedness.put(class, cert);
    }

    fn conclusions_for(
        &self,
        path: &std::path::Path,
    ) -> Option<std::sync::Arc<crate::model::witnesses::ConclusionMap>> {
        use crate::index::conclusion_cache::Cached;
        // The Arc, not a clone of what it holds. Handing back an owned map
        // deep-copies a ~72-entry HashMap per consult, and the consult path
        // runs this tens of thousands of times per check — measured at a
        // 6.7% REGRESSION against no conclusions at all, which is the whole
        // saving spent on copying the thing that produced it.
        let Cached::Map(m, stamp) = self.conclusion_cache_ref()?.get(path) else {
            return None;
        };
        // THE validity decision, and the only one. A row asserts the surface
        // fingerprint of the analysis it was baked from; the index knows what
        // that path's surface fingerprints to NOW. Equal means the map still
        // describes what a consumer can see of this file.
        //
        // Never re-hash here. The fingerprint is looked UP, not computed —
        // consults run tens of thousands of times per check, and hashing a
        // projected surface on that path would cost more than the chase this
        // layer exists to avoid.
        //
        // One compare, three failure modes: an ORPHANED row whose `modules`
        // row was erased, a STALE row for an edited file, and a row from an
        // INTERRUPTED write all fail it identically and read as absent, which
        // falls back to the live chase. Correctness therefore stops depending
        // on any caller remembering an eraser — `invalidate_derived_copies`
        // survives for space and hygiene, not for truth.
        //
        // No freshness record means absent, not valid: a path the index has
        // never recorded is one we cannot vouch for, and the fail-open
        // direction here is "decode", never "trust".
        match self.freshness.fingerprint_of(path) {
            Some(fp) if fp == stamp.source_fingerprint => {
                // The fingerprint is the WHOLE decision, generation included.
                // A flush can publish a round while this walk is in flight,
                // but a row passes the compare only against the world the
                // index currently believes, and the bake is deterministic —
                // so two rows that both pass carry the same content whichever
                // generation published them. Reading one from N and one from
                // N+1 is not a torn read; it is the same answer twice.
                //
                // Refusing the second generation was the earlier design and
                // it was worse than the problem: the walk went blind for the
                // rest of the corpus, and WHICH rows it lost depended on the
                // order consults happened to arrive in.
                crate::model::witnesses::ResolutionSession::note_conclusion_generation(
                    self,
                    stamp.flush_generation.0,
                );
                crate::util::ghost_stats::count("conclrow.valid");
                Some(m)
            }
            Some(_) => {
                crate::util::ghost_stats::count("conclrow.stale");
                // Push half of the repair lane. This site is the only one
                // that KNOWS the row is wrong rather than missing — the
                // frontier query sees absence, and a fingerprint join would
                // turn a check we just performed into an O(corpus) scan. One
                // entry per path: a stale row rejected ten thousand times in
                // a sweep is still one repair.
                self.core.repair_pushed.insert(path.to_path_buf(), ());
                None
            }
            None => {
                crate::util::ghost_stats::count("conclrow.unrecorded");
                None
            }
        }
    }

    fn bag_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        // Never-evicted copy (open docs, degraded files kept whole): a cheap
        // Arc bump, no I/O.
        if !cached.analysis.bag_is_evicted() {
            crate::util::ghost_stats::count("bagpresent.resident_whole");
            return cached.analysis.clone();
        }
        crate::util::ghost_stats::count("bagpresent.rehydrate");
        self.rehydrate_or_resident(cached)
    }

    // `parents_cached`: the trait default unions over `def_candidates`
    // (every file declaring the package), which is the honest Perl relation.

    fn module_path_cached(&self, module_name: &str) -> Option<std::path::PathBuf> {
        self.module_path_cached(module_name)
    }

    fn inc_roots(&self) -> Arc<Vec<std::path::PathBuf>> {
        self.core
            .inc_roots
            .read()
            .map(|g| Arc::clone(&g))
            .unwrap_or_default()
    }
    fn is_dependency_path(&self, path: &std::path::Path) -> bool {
        match self.core.dependency_roots.read() {
            // Hub semantics: everything cached here came from `@INC`.
            Ok(g) => match g.as_ref() {
                None => true,
                Some(roots) => roots.iter().any(|r| path.starts_with(r)),
            },
            Err(_) => true,
        }
    }

    fn workspace_root_path(&self) -> Option<std::path::PathBuf> {
        self.workspace_root()
            .as_deref()
            .and_then(crate::index::module_resolver::uri_to_path)
    }

    fn modules_with_symbol(&self, name: &str) -> Vec<String> {
        self.modules_with_symbol(name)
    }

    fn find_exporters(&self, func_name: &str) -> Vec<String> {
        self.find_exporters(func_name)
    }

    fn defining_module_cached(&self, entry: &str, name: &str) -> Option<Arc<CachedModule>> {
        self.defining_module_cached(entry, name)
    }

    fn module_declaring_method_in_package(&self, name: &str, class: &str) -> Option<String> {
        self.module_declaring_method_in_package(name, class)
    }

    fn for_each_cached(&self, f: &mut dyn FnMut(&str, &Arc<CachedModule>)) {
        self.for_each_cached(f)
    }

    fn def_candidates(&self, name: &str) -> Vec<Arc<CachedModule>> {
        match self.core.all_defs.get(name) {
            Some(cands) if !cands.is_empty() => {
                // Path-ordered HERE, the one speller of candidate order —
                // `all_defs` vecs are insertion-ordered (parallel indexing),
                // and every consumer scan/union/tie-break leans on this
                // being deterministic.
                let mut v = cands.clone();
                v.sort_by(|a, b| a.path.cmp(&b.path));
                v
            }
            // No workspace candidates: fall back to the name-slot winner.
            // The @INC tier lands here — single-provider by CURRENT
            // construction (one resolve per name), not by Perl semantics:
            // @INC is per-entrypoint, so the honest shape there is the same
            // candidate relation scoped by the asker's @INC.
            _ => self.get_cached(name).into_iter().collect(),
        }
    }

    fn for_each_cached_file(&self, f: &mut dyn FnMut(&Arc<CachedModule>)) {
        // `all_files` is the complete per-path registry (pack indexes, fed by
        // `register_symbols`) — the name-keyed views can't see a file that
        // lost all its name ties OR declares nothing registrable. The Perl
        // hub's cache is written directly (module-name keys, unique per
        // file), so union it in for both worlds.
        let mut seen: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        for entry in self.all_files.iter() {
            if seen.insert(entry.key().clone()) {
                f(entry.value());
            }
        }
        for entry in self.core.cache.iter() {
            if let Some(ref cached) = *entry.value() {
                if seen.insert(cached.path.clone()) {
                    f(cached);
                }
            }
        }
    }

    fn for_each_reexport_module(
        &self,
        start: Vec<String>,
        visit: &mut dyn FnMut(&Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    ) {
        self.for_each_reexport_module(start, visit)
    }

    fn for_each_entity_bridged_to(
        &self,
        class_name: &str,
        f: &mut dyn FnMut(
            &str,
            &Arc<CachedModule>,
            &crate::model::file_analysis::Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        self.for_each_entity_bridged_to(class_name, f)
    }

    fn for_each_entity_bridged_to_named(
        &self,
        class_name: &str,
        name: &str,
        f: &mut dyn FnMut(
            &str,
            &Arc<CachedModule>,
            &crate::model::file_analysis::Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        self.bridged_walk(class_name, Some(name), f)
    }

    fn direct_children_of(&self, class: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for module in self.modules_with_parent(class) {
            // The child edge may live in a losing candidate (packages lane).
            for cached in CrossFileLookup::def_candidates(self, &module) {
                for (pkg, parents) in cached.analysis.package_parent_edges() {
                    if parents.iter().any(|p| p == class) {
                        out.push((pkg.clone(), module.clone()));
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn direct_specializations_of(&self, primary: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let modules: Vec<String> = self
            .core
            .edges
            .specs
            .get(primary)
            .map(|b| b.as_slice().to_vec())
            .unwrap_or_default();
        for module in modules {
            // The specialization edge may live in a losing candidate
            // (pack lane, never evicted).
            for cached in CrossFileLookup::def_candidates(self, &module) {
                for (spec, prim) in &cached.analysis.pack.specializes {
                    if prim == primary {
                        out.push((spec.clone(), module.clone()));
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn for_each_loader_shape(&self, f: &mut dyn FnMut(&str, &crate::model::file_analysis::InferredType)) {
        for entry in self.core.loader_config_shapes.iter() {
            for (_contributor, t) in entry.value() {
                f(entry.key(), t);
            }
        }
    }

    fn complete_module_names(&self, prefix: &str) -> Vec<(String, bool)> {
        self.complete_module_names(prefix)
    }

    fn visible_defs_with_prefix(
        &self,
        prefix: &str,
        visible: &std::collections::HashSet<String>,
    ) -> Vec<(String, Arc<CachedModule>)> {
        self.visible_defs_with_prefix(prefix, visible)
    }
    fn defs_with_prefix(&self, prefix: &str) -> Vec<(String, Vec<Arc<CachedModule>>)> {
        self.defs_with_prefix(prefix)
    }
}

/// The member pre-filter's verdict, separated from the store and the copy so
/// the truth table is testable without either. `rows` is
/// `sym_member_row_exists` behind `with_rows_conn`: outer `None` = no store,
/// inner `None` = file never shredded.
///
/// The ONLY skip is (no post-shred emissions, store present, file covered,
/// provably no matching row). Deferred plugin emissions materialize into the
/// resident copy AFTER the shred (`materialize_gated_emissions`), so their
/// symbols have no rows — a file carrying any must fail open or a DBIC result
/// class's synthesized accessors silently stop resolving.
pub(crate) fn member_prefilter_may_declare(
    has_gated_emissions: bool,
    rows: Option<Option<bool>>,
) -> bool {
    if has_gated_emissions {
        return true;
    }
    !matches!(rows, Some(Some(false)))
}

#[cfg(test)]
mod member_prefilter_tests {
    use super::member_prefilter_may_declare;

    /// Every unknown fails OPEN — to a decode, never away from one. The
    /// same discipline as `restamp_owed`: the skip needs positive evidence,
    /// and a wrong skip is a silently missing method, not an error.
    #[test]
    fn only_proven_absence_skips() {
        assert!(
            member_prefilter_may_declare(false, None),
            "no row store: the filter cannot speak, decode"
        );
        assert!(
            member_prefilter_may_declare(false, Some(None)),
            "file never shredded: the store does not cover it, decode"
        );
        assert!(
            member_prefilter_may_declare(false, Some(Some(true))),
            "a matching row: the decode is warranted"
        );
        assert!(
            !member_prefilter_may_declare(false, Some(Some(false))),
            "covered and provably absent: the one skip"
        );
    }

    /// Post-shred plugin emissions beat everything: their symbols exist only
    /// on the materialized resident copy, so the rows' "provably absent" is
    /// a lie for exactly these files.
    #[test]
    fn gated_emissions_fail_open_over_a_provably_absent_row() {
        assert!(member_prefilter_may_declare(true, Some(Some(false))));
        assert!(member_prefilter_may_declare(true, None));
    }
}
