//! Construction plus the zero-I/O cached-lookup accessors async handlers call.

use super::*;

impl ModuleIndex {
    /// Wrap a shared core. Everything OUTSIDE the core is per-`ModuleIndex`
    /// serving state the resolver thread never touches.
    fn from_core(core: Arc<IndexCore>) -> Self {
        ModuleIndex {
            core,
            loaded_modules: Arc::new(DashMap::new()),
            open_doc_paths: Arc::new(DashMap::new()),
            closedness: Arc::new(Default::default()),
            all_files: Arc::new(DashMap::new()),
            freshness: Arc::new(crate::model::surface::FreshnessIndex::default()),
            enriched: Arc::new(DashMap::new()),
            enriched_order: Arc::new(std::sync::Mutex::new(Default::default())),
            gated_views: Arc::new(Default::default()),
            enriched_ghost: crate::util::ghost_stats::GhostStats::new_if_enabled(
                "enriched-overlay".into(),
            ),
            enrichment_key_memo: Arc::new(DashMap::new()),
            ref_rows_opener: std::sync::RwLock::new(None),
            ref_rows_conn: crate::index::module_cache::RetainedReader::new(),
            workspace_modules: Arc::new(DashMap::new()),
        }
    }

    pub fn new(client: Client, on_diagnostics_refresh: impl Fn() + Send + Sync + 'static) -> Self {
        let core = Arc::new(IndexCore::new());
        module_resolver::spawn_resolver(Arc::clone(&core), client, Box::new(on_diagnostics_refresh));
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
        // Everything below reads `modules.db`. A pack process's store is
        // installed by its indexer (`index_pack_language`) instead.
        if !serves_perl() {
            return;
        }
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
        // rehydrate through this, same as a pack language's. Fixed
        // 128 MiB cap (Perl analyses are 10-100x smaller than cpp ones).
        let ckey = key.clone();
        // Retained across rehydrates — a per-call open was the same disease
        // the conclusion loader had (`RetainedReader`); the LRU's whole job
        // is to make misses rare, and each miss was paying a fresh open on
        // top of its decode.
        let bag_retained = Arc::new(crate::index::module_cache::RetainedReader::new());
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
            crate::index::module_cache::open_and_load_diag_retained(
                &bag_retained,
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
        // One retained connection for the whole cache's lifetime, not an
        // open per miss: the consult path misses once per unique path, and
        // a fresh open (WAL handshake + the CANTOPEN retry ladder) measured
        // 5.1 ms against the ~µs SELECT it serves — 360 s of a cold
        // `--check` at 70k paths. `RetainedReader` reopens on inode change,
        // so `--clear-cache` mid-session is still picked up.
        let retained = Arc::new(crate::index::module_cache::RetainedReader::new());
        self.set_conclusion_cache(Arc::new(
            crate::index::conclusion_cache::ConclusionCache::new(concl_cap, move |path| {
                let dir = crate::index::module_cache::cache_dir_for_workspace(ckey.as_deref())?;
                let db = crate::index::module_cache::db_path_for(&dir, "perl");
                retained
                    .with(
                        || crate::index::module_cache::open_reader_retrying(&db).ok(),
                        |conn| {
                            let at = crate::index::module_cache::current_generation(conn);
                            crate::index::module_cache::load_conclusions_stamped(
                                conn,
                                &path.to_string_lossy(),
                                at,
                            )
                        },
                    )
                    .flatten()
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
            crate::util::timings::record_cached(module_name);
            return; // fresh and cached
        }
        if !is_stale {
            // The one dedup'ing enqueue: a name asked per keystroke is
            // queued once between drains.
            self.core.queue.ask([module_name.to_string()]);
            return;
        }
        {
            let mut priority = self.core.queue.priority.lock().unwrap();
            if !priority.contains(&module_name.to_string()) {
                priority.push(module_name.to_string());
            }
        }
        // The guard is out of scope here — `notify_new_work` takes
        // `pending`, and the drain's lock order forbids holding `priority`
        // across it.
        self.core.queue.notify_new_work();
    }

    /// `request_resolve` for everything the project's own files use
    /// (`dependency_demand`).
    pub fn request_dependency_demand(&self) {
        for name in self.dependency_demand() {
            self.request_resolve(&name);
        }
    }

    /// `request_resolve` for everything one file uses (`used_identities`).
    pub fn request_used(&self, analysis: &FileAnalysis) {
        for name in analysis.used_identities() {
            self.request_resolve(&name);
        }
    }

    /// Block until the resolver has read its map and has nothing queued or
    /// in flight — for one-shot callers that answer from a settled index.
    /// Every landing happens on the resolver thread, so this is the only
    /// way such a caller sees one.
    pub fn wait_idle(&self) {
        use std::sync::atomic::Ordering::Acquire;
        let q = &self.core.queue;
        let mut guard = self.core.resolved.mu.lock().unwrap();
        loop {
            // Read under `pending`: the drain takes a batch and marks the
            // queue busy under that lock, so no batch is between the two.
            let idle = {
                let pending = q.pending.lock().unwrap();
                q.started.load(Acquire)
                    && pending.is_empty()
                    && q.priority.lock().unwrap().is_empty()
                    && !q.map_moved.load(Acquire)
                    && !q.busy.load(Acquire)
            };
            if idle {
                return;
            }
            // The timeout only bounds a wake the thread's notify could
            // race; every state change above is followed by one.
            guard = self
                .core
                .resolved
                .cv
                .wait_timeout(guard, std::time::Duration::from_millis(50))
                .unwrap()
                .0;
        }
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
                if let Some(best) = best_candidate(&reachable, module_name, &|m, n| self.core.module_defines_class(m, n)) {
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
            if let Some(best) = best_candidate(&reachable, entry.key(), &|m, n| self.core.module_defines_class(m, n)) {
                out.push((entry.key().clone(), best));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Breadth-first walk over re-export edges (each package's
    /// `reexport_modules`), starting from `start` and visiting each reachable
    /// (module name, candidate file) — the start modules first, then whatever
    /// they re-export. `visit` returns
    /// `ControlFlow::Break` to stop early. Bounded by a seen-set (cycles) and a
    /// fan-out cap; never does I/O. The single place the re-export edge
    /// traversal lives — `defining_module_cached` (def location) and
    /// `FileAnalysis::export_surface_with_index` (transitive surface) both ride
    /// it instead of hand-copying the BFS.
    pub fn for_each_reexport_module<F>(&self, start: impl IntoIterator<Item = String>, mut visit: F)
    where
        F: FnMut(&str, &Arc<CachedModule>) -> std::ops::ControlFlow<()>,
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
                if visit(&module, &cached).is_break() {
                    return;
                }
                let next_hops = cached.analysis.package_exports(&module).map_or(&[][..], |e| &e.reexport_modules);
                for next in next_hops {
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
        self.for_each_reexport_module(std::iter::once(entry.to_string()), |_, cached| {
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

        // Tier 2: what the dependency map can place (name only, no analysis yet)
        let available = Arc::clone(&self.core.available.read().unwrap_or_else(|e| e.into_inner()));
        for name in available.iter() {
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

    /// Find all cached modules that *export* the given function name: the
    /// modules the symbol index names (it covers every named symbol, not
    /// just exports) whose surface carries the name, plus every descendant
    /// of one whose own surface carries it — a class exporting nothing of
    /// its own exports what its inherited `import` does
    /// (`export_surface_with_index`).
    pub fn find_exporters(&self, func_name: &str) -> Vec<String> {
        use crate::model::file_analysis::CrossFileLookup;
        use crate::model::graph::{EdgeKindMask, GraphView, Node, WalkControl};
        // ANY candidate file of the module may carry the export list.
        let exports = |m: &str| {
            self.def_candidates(m)
                .iter()
                .any(|c| c.analysis.export_surface_with_index(m, self).exports(func_name))
        };
        let mut result: Vec<String> = Vec::new();
        for m in self.modules_with_symbol(func_name) {
            if !exports(&m) {
                continue;
            }
            if let Some(seed) = self.def_candidates(&m).first().cloned() {
                GraphView::new(&seed.analysis, Some(self)).walk(
                    Node::Class(m.clone()),
                    EdgeKindMask::INHERITS_INV,
                    &mut |node| {
                        if let Node::Class(d) = node {
                            if exports(d) {
                                result.push(d.clone());
                            }
                        }
                        WalkControl::Continue
                    },
                );
            }
            result.push(m);
        }
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
                    // Rows-backed pre-filter, the same probe as the MRO
                    // walk's (`docs/prompt-relational-iteration.md` ladder 2):
                    // `has_sub_in_package` tests exactly the (name, package)
                    // attribution the syms rows carry, and this runs on the
                    // walk's MISSES, where every candidate scan is wasted.
                    // Fail-open everywhere the store cannot speak.
                    if !self.candidate_may_declare(c, name, class) {
                        crate::util::ghost_stats::count("mdmp.candidate_prefiltered");
                        return false;
                    }
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
    /// How many paths consults have pushed to the repair lane but the
    /// resolver has not yet adopted. Test-only: the drain is on the resolver
    /// thread, so a unit test asserting the push half needs to read the
    /// handoff without one.
    #[cfg(test)]
    pub fn repair_pushed_len_for_test(&self) -> usize {
        self.core.repair_pushed.len()
    }

    pub fn new_for_cli() -> Self {
        let core = Arc::new(IndexCore::new());
        module_resolver::spawn_test_resolver(Arc::clone(&core));
        Self::from_core(core)
    }

    /// Mark this process LONG-LIVED (the server): the witness seams'
    /// enriched retries turn on (the overlay amortizes them; one-shot CLI
    /// modes never recoup the deep-copies — bisected at 2x warm-harness
    /// wall), and the resolver strips warm-loaded @INC copies (their
    /// rehydration cost amortizes the same way). Both are Perl's: a pack
    /// language's index never served the overlay (its wiring is
    /// `prompt-unify-semantic-tiers.md` Step 3c), so a pack process stays
    /// short-lived here rather than start deep-copying its analyses.
    pub fn mark_long_lived(&self) {
        if !serves_perl() {
            return;
        }
        self.core.long_lived
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// `PERL_LSP_LONG_LIVED=1` forces the long-lived behaviors in one-shot
    /// CLI processes — the harness lane that keeps the server-only paths
    /// (enriched retries, warm @INC strip) under a regression net.
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

    /// Direct access to the raw cache DashMap (tests seeding a warm load).
    #[cfg(test)]
    pub fn cache_raw(&self) -> &DashMap<String, Option<Arc<CachedModule>>> {
        &self.core.cache
    }

}

/// The served language reads and writes the Perl hub's store
/// (`modules.db`); a pack process's is installed by its indexer.
fn serves_perl() -> bool {
    crate::build::language_driver::LanguageRegistry::served().caps().hub_enrichment
}
