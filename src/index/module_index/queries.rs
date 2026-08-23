//! Construction plus the zero-I/O cached-lookup accessors async handlers call.

use super::*;

impl ModuleIndex {
    /// Wrap a shared core. Everything OUTSIDE the core is per-`ModuleIndex`
    /// serving state the resolver thread never touches.
    fn from_core(core: Arc<IndexCore>) -> Self {
        ModuleIndex {
            core,
            loaded_modules: Arc::new(DashMap::new()),
            pack_indexes: Arc::new(DashMap::new()),
            open_doc_paths: Arc::new(DashMap::new()),
            all_files: Arc::new(DashMap::new()),
            registered_names: Arc::new(DashMap::new()),
            freshness: Arc::new(crate::model::surface::FreshnessIndex::default()),
            enriched: Arc::new(DashMap::new()),
            enriched_order: Arc::new(std::sync::Mutex::new(Default::default())),
            enriched_ghost: crate::util::ghost_stats::GhostStats::new_if_enabled(
                "enriched-overlay".into(),
            ),
            enrichment_key_memo: Arc::new(DashMap::new()),
            foreign_bag_cache: std::sync::RwLock::new(None),
            ref_rows_opener: std::sync::RwLock::new(None),
            ref_rows_conn: std::sync::Mutex::new(None),
            workspace_modules: Arc::new(DashMap::new()),
        }
    }

    pub fn new(client: Client, on_diagnostics_refresh: impl Fn() + Send + Sync + 'static) -> Self {
        let core = Arc::new(IndexCore::new());
        module_resolver::spawn_resolver(
            Arc::clone(&core),
            client,
            Box::new(on_diagnostics_refresh),
        );
        Self::from_core(core)
    }

    /// Hover markdown for a Perl builtin (e.g. `push`, `scalar`).
    /// Returns `None` for unknown names or before the resolver has
    /// hydrated the index from SQLite.
    pub fn builtin_doc(&self, name: &str) -> Option<String> {
        self.core.builtins.get(name).map(|e| e.clone())
    }

    /// Notify the resolver thread of the workspace root (from LSP initialize).
    pub fn set_workspace_root(&self, root: Option<&str>) {
        let mut guard = self.core.workspace_root.root.lock().unwrap();
        if root.is_none() {
            log::warn!("No workspace root from client; using global module cache");
        }
        *guard = Some(root.map(String::from));
        self.core.workspace_root.condvar.notify_one();
        drop(guard);
        // The hub's relational-ref-index reader: the SAME cache key the
        // resolver thread writes under (both spell it as this root string),
        // so retrieval and shred always address one DB.
        let key = root.map(String::from);
        {
            let key = key.clone();
            self.set_ref_rows_opener(Arc::new(move || {
                crate::index::module_cache::open_cache_db_readonly(key.as_deref(), "perl")
            }));
        }
        // The hub's rehydration LRU: Perl workspace copies are refs/bag-
        // evicted once persisted; queries that need the whole analysis
        // rehydrate through this, same as the pack sub-indexes. Fixed
        // 128 MiB cap (Perl analyses are 10-100x smaller than cpp ones).
        let ckey = key.clone();
        let loader = move |path: &std::path::Path, want_bag: bool| {
            // Raw walk path first (preserves the pre-diag behavior), canonical
            // as a fallback spelling; the discriminated helper survives the
            // readonly-open CANTOPEN/WAL race behind both.
            let raw = path.to_string_lossy().into_owned();
            let canon = path
                .canonicalize()
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            let mut spellings = vec![raw.clone()];
            if let Some(c) = canon {
                if c != raw {
                    spellings.push(c);
                }
            }
            crate::index::module_cache::open_and_load_diag(
                key.as_deref(),
                "perl",
                &spellings,
                want_bag,
            )
        };
        // Measurement-only override for ghost-stats cap experiments
        // (`PERL_LSP_BAG_CACHE_MB`); unset ⇒ the stock 128 MiB.
        let cap_bytes = std::env::var("PERL_LSP_BAG_CACHE_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(128 * 1024 * 1024);
        self.set_bag_cache(Arc::new(crate::index::pack_bag_cache::PackBagCache::new_labeled(
            cap_bytes,
            "perl-hub",
            loader,
        )));

        // Conclusions ride the same cache-key. 16 MiB rather than the bag
        // cache's 128: the whole substrate's maps measured 15.4 MB, so this
        // holds a realistic workspace outright and eviction is the exception.

        let concl_cap = std::env::var("PERL_LSP_CONCL_CACHE_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(16 * 1024 * 1024);
        self.set_conclusion_cache(Arc::new(
            crate::index::conclusion_cache::ConclusionCache::new(concl_cap, move |path| {
                let dir = crate::index::module_cache::cache_dir_for_workspace(ckey.as_deref())?;
                let db = crate::index::module_cache::db_path_for(&dir, "perl");
                let conn = crate::index::module_cache::open_reader_retrying(&db).ok()?;
                let at = crate::index::module_cache::current_generation(&conn);
                crate::index::module_cache::load_conclusions(
                    &conn,
                    &path.to_string_lossy(),
                    at,
                )
            }),
        ));
    }

    /// Get the workspace root URI if set.
    pub fn workspace_root(&self) -> Option<String> {
        self.core.workspace_root.root.lock().ok()
            .and_then(|guard| guard.as_ref().and_then(|opt| opt.clone()))
    }

    /// Request background resolution for a module. Non-blocking.
    /// Stale modules (old extract version) are queued with priority.
    pub fn request_resolve(&self, module_name: &str) {
        let is_stale = self.core.stale_modules.contains_key(module_name);
        if self.core.cache.contains_key(module_name) && !is_stale {
            return; // fresh and cached
        }
        if is_stale {
            let mut priority = self.core.queue.priority.lock().unwrap();
            if !priority.contains(&module_name.to_string()) {
                priority.push(module_name.to_string());
            }
        } else {
            let mut pending = self.core.queue.pending.lock().unwrap();
            pending.push(module_name.to_string());
        }
        // Both guards are out of scope here — `notify_new_work` takes
        // `pending`, and the drain's lock order forbids holding `priority`
        // across it.
        self.core.queue.notify_new_work();
    }

    /// Return the cached CachedModule for a module name. Never does I/O.
    pub fn get_cached(&self, module_name: &str) -> Option<Arc<CachedModule>> {
        self.core.cache.get(module_name).and_then(|entry| entry.clone())
    }

    /// Like `get_cached`, but scoped to a querying file's VISIBILITY set
    /// (`visible` = canonical paths of the file + its `#include` closure). When
    /// two files define the same name (C's flat linkage), prefer the candidate
    /// the querying file can actually SEE; fall back to the global winner when
    /// NONE is reachable (so a legit indirect resolution never regresses).
    /// `visible` empty (Perl, or an unwarmed on-open file) ⇒ identical to
    /// `get_cached`. `docs/adr/macro-handling.md`, "the include-closure lie".
    pub fn get_cached_scoped(
        &self,
        module_name: &str,
        visible: &std::collections::HashSet<String>,
    ) -> Option<Arc<CachedModule>> {
        if !visible.is_empty() {
            if let Some(cands) = self.core.all_defs.get(module_name) {
                let reachable: Vec<&Arc<CachedModule>> = cands
                    .iter()
                    .filter(|c| visible.contains(&c.path.to_string_lossy().into_owned()))
                    .collect();
                if let Some(best) = best_candidate(&reachable, module_name, &|m, n| self.module_defines_class(m, n)) {
                    return Some(best);
                }
            }
        }
        self.get_cached(module_name)
    }

    /// Completion-GATHERING mirror of `get_cached_scoped`: enumerate every
    /// registered name starting with `prefix` that has a definition candidate
    /// inside `visible` (canonical paths — the querying file's `#include`
    /// closure). Unlike resolution there is NO global fallback — an empty or
    /// non-matching closure yields nothing, so a file never gets offered
    /// symbols from headers it doesn't include. Deterministic: sorted by
    /// name; among reachable candidates the tie breaks exactly like
    /// `get_cached_scoped` (class-over-value, then smallest path).
    pub fn visible_defs_with_prefix(
        &self,
        prefix: &str,
        visible: &std::collections::HashSet<String>,
    ) -> Vec<(String, Arc<CachedModule>)> {
        if visible.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(String, Arc<CachedModule>)> = Vec::new();
        for entry in self.core.all_defs.iter() {
            if !entry.key().starts_with(prefix) {
                continue;
            }
            let reachable: Vec<&Arc<CachedModule>> = entry
                .value()
                .iter()
                .filter(|c| c.path.to_str().is_some_and(|p| visible.contains(p)))
                .collect();
            if let Some(best) = best_candidate(&reachable, entry.key(), &|m, n| self.module_defines_class(m, n)) {
                out.push((entry.key().clone(), best));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Breadth-first walk over re-export edges (`reexport_modules`), starting
    /// from `start` and visiting each reachable cached module — the start
    /// modules first, then whatever they re-export. `visit` returns
    /// `ControlFlow::Break` to stop early. Bounded by a seen-set (cycles) and a
    /// fan-out cap; never does I/O. The single place the re-export edge
    /// traversal lives — `defining_module_cached` (def location) and
    /// `FileAnalysis::export_surface_with_index` (transitive surface) both ride
    /// it instead of hand-copying the BFS.
    pub fn for_each_reexport_module<F>(&self, start: impl IntoIterator<Item = String>, mut visit: F)
    where
        F: FnMut(&Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    {
        const MAX: usize = 256;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<String> = start.into_iter().collect();
        let mut visited = 0usize;
        while let Some(module) = queue.pop_front() {
            if !seen.insert(module.clone()) {
                continue;
            }
            visited += 1;
            if visited > MAX {
                break;
            }
            // Every candidate file registered under the name — a split
            // module's re-export edges (and defs) span the set.
            for cached in crate::model::file_analysis::CrossFileLookup::def_candidates(self, &module) {
                if visit(&cached).is_break() {
                    return;
                }
                for next in &cached.analysis.reexport_modules {
                    if !seen.contains(next) {
                        queue.push_back(next.clone());
                    }
                }
            }
        }
    }

    /// Find the cached module that actually defines sub `name`, starting at
    /// `entry` and following re-export edges when `entry` re-exports another
    /// module's surface. The directly-`use`d module is tried first;
    /// re-exporters delegate the def location to whoever they re-export.
    pub fn defining_module_cached(
        &self,
        entry: &str,
        name: &str,
    ) -> Option<Arc<CachedModule>> {
        use std::ops::ControlFlow;
        let mut found = None;
        self.for_each_reexport_module(std::iter::once(entry.to_string()), |cached| {
            use crate::model::file_analysis::CrossFileLookup;
            // Existence probe over symbols only — the caller takes the
            // CachedModule and chooses its own view for detail reads.
            if self.symbols_present(cached).sub_info_view(name).is_some() {
                found = Some(Arc::clone(cached));
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        found
    }

    /// Return cached module path only — never does I/O.
    pub fn module_path_cached(&self, module_name: &str) -> Option<std::path::PathBuf> {
        self.core.cache
            .get(module_name)
            .and_then(|entry| entry.as_ref().map(|m| m.path.clone()))
    }

    /// Iterate all cached modules. Callback receives (module_name, CachedModule).
    pub fn for_each_cached<F: FnMut(&str, &Arc<CachedModule>)>(&self, mut f: F) {
        for entry in self.core.cache.iter() {
            if let Some(ref cached) = *entry.value() {
                f(entry.key(), cached);
            }
        }
    }

    /// Collect module names matching a prefix for completion.
    /// Returns (name, is_resolved) — resolved modules have full analysis.
    pub fn complete_module_names(&self, prefix: &str) -> Vec<(String, bool)> {
        let prefix_lower = prefix.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();

        // Tier 1: resolved modules (have full analysis)
        for entry in self.core.cache.iter() {
            if entry.value().is_some() {
                let name = entry.key();
                if name.to_lowercase().starts_with(&prefix_lower) && seen.insert(name.clone()) {
                    results.push((name.clone(), true));
                }
            }
        }

        // Tier 2: @INC scan (name only, no analysis yet)
        for entry in self.core.available_modules.iter() {
            let name = entry.key();
            if name.to_lowercase().starts_with(&prefix_lower) && seen.insert(name.clone()) {
                results.push((name.clone(), false));
            }
        }

        results
    }

    /// Look up the return type of an imported function. Zero I/O.
    #[cfg(test)]
    pub fn get_return_type_cached(&self, func_name: &str) -> Option<InferredType> {
        use crate::model::file_analysis::CrossFileLookup;
        let modules = self.core.edges.names.get(func_name)?;
        for module_name in modules.value() {
            if let Some(cached) = self.get_cached(module_name) {
                // `sub_return_type_local` walks symbols AND resolves through
                // the bag — two evictable axes, take the whole view.
                let whole = self.whole_present(&cached);
                if let Some(ty) = whole.sub_return_type_local(func_name) {
                    return Some(ty.clone());
                }
            }
        }
        None
    }

    /// Find all cached modules that *export* the given function name.
    /// Starts from the generic symbol index, then filters to modules
    /// whose `export` / `export_ok` list actually contains the name —
    /// the reverse_index covers every named symbol, not just exports.
    pub fn find_exporters(&self, func_name: &str) -> Vec<String> {
        let mut result: Vec<String> = self.modules_with_symbol(func_name)
            .into_iter()
            .filter(|m| {
                // ANY candidate file of the module may carry the export list.
                crate::model::file_analysis::CrossFileLookup::def_candidates(self, m)
                    .iter()
                    .any(|c| c.analysis.export.iter().any(|e| e == func_name)
                        || c.analysis.export_ok.iter().any(|e| e == func_name))
            })
            .collect();
        result.sort();
        result.dedup();
        result
    }

    /// Generic "find modules with a symbol named N" primitive —
    /// O(1) hash + O(matches) scan for name-keyed predicates (never
    /// `for_each_cached` over the whole store). Callers apply their
    /// own kind/detail filter + override/stacking semantics after
    /// picking which specific symbols matter to them.
    pub fn modules_with_symbol(&self, name: &str) -> Vec<String> {
        match self.core.edges.names.get(name) {
            Some(bucket) => {
                // Unique by construction; sort only, for a stable order.
                let mut result = bucket.as_slice().to_vec();
                result.sort();
                result
            }
            None => Vec::new(),
        }
    }

    /// Find the module that declares method `name` *attributed to class*
    /// `class` in a file whose own module name differs (cross-package
    /// typeglob install). Returns the registration key for a follow-up
    /// `get_cached`. `None` when no such cross-package symbol exists —
    /// callers fall back to the class's own module / bridges.
    ///
    /// The candidate set is the CLASS-keyed provider bucket, not the
    /// name-keyed one: this runs as the ancestor walk's last resort, i.e.
    /// on exactly the misses, and a name-keyed narrowing leaves the class
    /// test answerable only after a rehydrate — for `new`, once per module
    /// in the corpus, to conclude nothing. Both buckets are supersets of
    /// the answer set (a module matching the `has_sub_in_package` filter
    /// necessarily attributes a sub to `class`), so the filtered result is
    /// the same module; the provider bucket just makes the miss a bucket
    /// read.
    pub fn module_declaring_method_in_package(
        &self,
        name: &str,
        class: &str,
    ) -> Option<String> {
        use crate::model::file_analysis::CrossFileLookup;
        crate::util::ghost_stats::count("mdmp.call");
        let mods = self.modules_providing_package(class);
        crate::util::ghost_stats::count_by("mdmp.modules_scanned", mods.len() as u64);
        let found = mods
            .into_iter()
            .find(|mod_name| {
                // EVERY file registered under this module name — the definer
                // may be a losing candidate of its own name slot. Symbols-axis
                // existence scan — no bag/refs read.
                self.def_candidates(mod_name).iter().any(|c| {
                    crate::util::ghost_stats::count("mdmp.candidate_fetched");
                    crate::util::ghost_stats::count(if crate::util::ghost_stats::mroc_saw(&c.path) {
                        "mdmp.candidate_seen_by_mroc"
                    } else {
                        "mdmp.candidate_new"
                    });
                    self.symbols_present(c).has_sub_in_package(name, class)
                })
            });
        crate::util::ghost_stats::count(if found.is_some() {
            "mdmp.found"
        } else {
            "mdmp.not_found"
        });
        found
    }

    /// Create a ModuleIndex for CLI mode: a real (headless) resolver
    /// thread — same @INC scan, SQLite warm/persist, and resolve loop as
    /// the server's, without the LSP progress/builtins warmup. One-shot
    /// CLI sessions previously carried NO resolver, so they could never
    /// resolve a module the editor hadn't already cached. The thread
    /// blocks until `set_workspace_root` fires in `cli_full_startup`.
    pub fn new_for_cli() -> Self {
        let core = Arc::new(IndexCore::new());
        module_resolver::spawn_test_resolver(Arc::clone(&core));
        Self::from_core(core)
    }

    /// Mark this process LONG-LIVED (the server): the witness seams'
    /// enriched retries turn on (the overlay amortizes them; one-shot CLI
    /// modes never recoup the deep-copies — bisected at 2x warm-harness
    /// wall), and the resolver strips warm-loaded @INC copies (their
    /// rehydration cost amortizes the same way).
    pub fn mark_long_lived(&self) {
        self.core.long_lived
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// `PERL_LSP_LONG_LIVED=1` forces the long-lived behaviors in one-shot
    /// CLI processes — the harness lane that keeps the server-only paths
    /// (enriched retries, warm @INC strip) under a regression net.
    pub fn is_long_lived(&self) -> bool {
        self.core.long_lived.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn mark_long_lived_from_env(&self) {
        if std::env::var("PERL_LSP_LONG_LIVED").as_deref() == Ok("1") {
            self.mark_long_lived();
        }
    }

    // ---- Test-only methods ----

    /// Test-only: publish an `@INC` without a resolver thread (discovery
    /// shells out to `perl`, and a unit net needs its own roots).
    #[cfg(test)]
    pub fn set_inc_roots_for_test(&self, roots: &[std::path::PathBuf]) {
        self.core.set_inc_roots(roots);
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let idx = Self::new_for_cli();
        // Unit nets exercise the seams' retries; production defaults OFF
        // (the server enables at initialize).
        idx.mark_long_lived();
        idx
    }

    /// Test-only: seed the builtins map directly (bypasses SQLite +
    /// the resolver thread). Used by hover tests so they don't have
    /// to spin up the perlfunc.pod parse pipeline.
    #[cfg(test)]
    pub fn seed_builtin_for_test(&self, name: &str, doc: &str) {
        self.core.builtins.insert(name.to_string(), doc.to_string());
    }

    /// Direct access to the raw cache DashMap (for CLI warm_cache integration).
    pub fn cache_raw(&self) -> &DashMap<String, Option<Arc<CachedModule>>> {
        &self.core.cache
    }

    /// The candidate relation behind the name-slot winner — the warm lane
    /// fills it alongside `cache_raw` so a warm start keeps every provider.
    pub fn all_defs_raw(&self) -> &DashMap<String, Vec<Arc<CachedModule>>> {
        &self.core.all_defs
    }

    /// Insert a module directly into the cache (for CLI and testing).
    /// After indexing completes (cross-file ancestry fully populated),
    /// MATERIALIZE deferred gated plugin emissions (`GatedEmission`) into each
    /// cached copy whose gate now resolves cross-file. A DBIC result class's
    /// column/relationship accessors are recorded but not applied at build
    /// (the `ClassIsa` trigger can't see the cross-file base, rule #1); this
    /// pass applies them once the index knows the ancestry, so `whole_present`
    /// — the view every cross-file goto-def / references reader consults —
    /// sees them WITHOUT a per-query enriched-overlay hop.
    ///
    /// The cheap gate — `gated_emissions` is NOT an eviction axis, so an
    /// evicted resident copy still carries it — decides whether a file needs
    /// materializing; the whole (rehydrated) view is only pulled for those.
    /// The re-registered copy is whole (symbols resident); this is the
    /// one-shot CLI's deterministic path (re-pinning is harmless when the
    /// process is about to answer one query and exit). The warm server never
    /// calls this — it has the enriched-overlay fallback in
    /// `method_resolution_on_class`. Idempotent (`materialize_gated_emissions`
    /// dedups against already-present symbols).
    pub fn materialize_gated_emissions(&self) {
        let mut updates: Vec<(String, std::path::PathBuf, Arc<FileAnalysis>)> = Vec::new();
        for entry in self.core.cache.iter() {
            let Some(cached) = entry.value() else { continue };
            if cached.analysis.plugin.gated_emissions.is_empty() {
                continue;
            }
            // Rehydrate the whole view (the resident copy may be
            // symbols-evicted) before appending the synthesized accessors.
            let whole = crate::model::file_analysis::CrossFileLookup::whole_present(self, cached);
            let mut copy = (*whole).clone();
            copy.materialize_gated_emissions(self);
            updates.push((entry.key().clone(), cached.path.clone(), Arc::new(copy)));
        }
        for (name, path, analysis) in updates {
            let cm = Arc::new(CachedModule::new(path.clone(), analysis));
            self.register_materialized_whole(name, path, cm);
        }
    }
}
