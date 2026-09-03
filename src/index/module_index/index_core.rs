//! `IndexCore` — the shared index organs as ONE struct.
//!
//! `ModuleIndex` (async side) and the resolver thread (blocking side) hold
//! the same `Arc<IndexCore>`, so every operation on the shared state has
//! exactly one spelling — a method here — and the side-effect set of an
//! operation cannot diverge per entry path (the drift class where an
//! @INC-resolved module fed `edges` but never `loader_config_shapes`
//! because the thread held loose Arcs without the shapes map).

use super::*;

pub(crate) struct IndexCore {
    pub(crate) cache: DashMap<String, Option<Arc<CachedModule>>>,
    /// See `ModuleEdgeIndexes` — names + bridges + children reverse maps.
    pub(crate) edges: ModuleEdgeIndexes,
    /// Loader-config shapes projected at registration: load-name →
    /// (contributor, shape) pairs from each file's `PluginLoad` facts.
    /// Projected HERE because lite entrypoints are PACKAGELESS — they
    /// never enter the cache, so enrichment can't reach their bags;
    /// the config value is a literal, so its shape is final at the
    /// contributor's own build. Fed by `record_workspace_projections`
    /// (before the packageless early-return) AND `insert_resolved`.
    pub(crate) loader_config_shapes: DashMap<String, Vec<(String, InferredTypeOwned)>>,
    /// Reverse index: contributor → the load-names it recorded a shape under.
    /// Retracting a contributor used to `retain` over the whole shape map — an
    /// ALL-SHARD write barrier, run once per registered file by the bulk walk.
    /// Almost no file contributes a shape, so the scan was pure overhead
    /// 124k times over at workspace scale; with this, a non-contributor's
    /// retraction is one lookup miss and a contributor's touches only its own
    /// names.
    shapes_by_contributor: DashMap<String, Vec<String>>,
    /// Paths whose persisted derivation a consult found STALE — the repair
    /// lane's push half.
    ///
    /// The frontier query detects ABSENCE (no map, or no surface at this
    /// projection version); it cannot see a row that exists and is simply
    /// wrong, and teaching it a fingerprint join would make an O(corpus) scan
    /// out of a check the consult already performed. So the site that
    /// rejected the row says so, and residual drift self-heals through the
    /// same lane as absence.
    ///
    /// A set, not a queue: a stale path rejected ten thousand times in one
    /// sweep is one repair.
    pub(crate) repair_pushed: DashMap<std::path::PathBuf, ()>,
    /// Modules loaded from cache with an old extract_version.
    /// Eligible for priority re-resolution when requested.
    pub(crate) stale_modules: DashMap<String, ()>,
    /// Perl builtins hover docs, name → rendered markdown. Hydrated
    /// from SQLite by the resolver thread at startup (parsed from
    /// `perlfunc.pod` on first cold-cache miss). Empty until the
    /// resolver has run its warmup path.
    pub(crate) builtins: DashMap<String, String>,
    /// Known module names from @INC scan. Name → the @INC-order-winning
    /// path. No exports until resolved.
    pub(crate) available_modules: DashMap<String, std::path::PathBuf>,
    /// ALL cross-file candidates per name, not just the winner in `cache` —
    /// the honest relation for every tier, because a name maps to a SET of
    /// files. Pack: C's flat linkage lets unrelated files each define
    /// `class Box`. Perl: a package reopens in any file, and @INC is
    /// per-entrypoint, so one module name legitimately has several
    /// providers. The winner in `cache` is DERIVED from this set; the
    /// per-origin visibility rule (`ScopedLookup`) picks among them.
    /// Shared: the resolver thread adopts @INC providers here, the async
    /// side adopts workspace candidates.
    pub(crate) all_defs: DashMap<String, Vec<Arc<CachedModule>>>,
    /// The `@INC` roots this process resolves from, canonical, most-
    /// preferred first. Recorded ONCE by the resolver thread (discovery
    /// shells out to `perl`, so no query path may re-derive it) and read by
    /// every origin's `VisibilityAxis::for_origin`.
    pub(crate) inc_roots: std::sync::RwLock<Arc<Vec<std::path::PathBuf>>>,
    /// The read-only DEPENDENCY tier's root prefixes. `None` = the hub's
    /// construction-time semantics (everything here came from `@INC`, so
    /// every path is dependency); `Some(roots)` = a pack sub-index, whose
    /// cache holds the workspace's own files PLUS declared dependency
    /// roots (composer's vendor packages) — a path is dependency iff a
    /// root prefixes it. Set once by the bulk indexer; canonical, like
    /// `inc_roots`.
    pub(crate) dependency_roots: std::sync::RwLock<Option<Arc<Vec<std::path::PathBuf>>>>,
    pub(crate) queue: ResolveQueue,
    pub(crate) resolved: ResolveNotify,
    pub(crate) workspace_root: WorkspaceRootChannel,
    /// Monotonic per-path registration generation — the ABA-proof identity
    /// token `enrichment_key` hashes (an Arc pointer can be freed and its
    /// address reused; a counter can't run backwards). Bumped by every
    /// registration front door.
    pub(crate) registration_gen: DashMap<std::path::PathBuf, u64>,
    pub(crate) gen_counter: std::sync::atomic::AtomicU64,
    /// Monotone count of index-shape mutations NOT already visible through
    /// `gen_counter` or the freshness write count (cache-slot swaps on
    /// unregister, loader-shape rewrites). Third leg of the enrichment-key
    /// memo's validity epoch — over-invalidation is always safe here, a
    /// missed bump is silent staleness, so mutators bump even when a gen
    /// mint usually accompanies them.
    pub(crate) shape_bumps: std::sync::atomic::AtomicU64,
    /// The flush clock, and the per-file marks compared against it.
    ///
    /// The re-stamp gate's whole mechanism: a flush bumps `flush_epoch` once,
    /// then records that epoch for every consumer it enqueued
    /// (`provider_diff_gen`). A file whose own `stamped_at` is at or past its
    /// mark has had no provider move since it last stamped, so its re-stamp is
    /// re-deriving what it already froze.
    ///
    /// Sessional on purpose: `FileAnalysis::stamped_at` is `#[serde(skip)]`,
    /// so both halves die with the process and a fresh one fails open. A
    /// persisted mark table paired with sessional stamps would be sound; a
    /// persisted STAMP paired with sessional marks would silently skip a
    /// re-stamp that was owed, so neither half may outlive the other.
    ///
    /// **A dedicated clock, and not the conclusion store's generation** — the
    /// obvious-looking consolidation, ruled out on purpose. A comparison clock
    /// must share its lifetime with the operands it orders, and these operands
    /// are sessional. A persistent clock over sessional operands fails the
    /// mirror of the half-persistence case above: a restart resets stamps and
    /// marks while the store generation carries on, so any path that ever
    /// seeded `stamped_at` from a persisted value would compare a new-session
    /// stamp against an old-session clock. This counter cannot express that
    /// bug. If the marks are ever persisted, all three move together.
    pub(crate) flush_epoch: std::sync::atomic::AtomicU64,
    pub(crate) provider_diff_gen: DashMap<std::path::PathBuf, u64>,
    /// The witness seams' fallback-on-miss enriched retries only pay off
    /// when the process lives long enough to amortize the overlay (each
    /// miss is a whole-analysis deep copy + enrich). Off by default; the
    /// SERVER enables it at initialize. One-shot CLI query modes leave it
    /// off — the bisected cost was 2x warm-gold wall for answers no
    /// one-shot invocation reuses. (`--check`/`--dump-package` consume
    /// `enriched_snapshot` directly and are unaffected by this gate.)
    pub(crate) long_lived: std::sync::atomic::AtomicBool,
    /// Slice-2 rehydration store CELL. Pack sub-indexes get theirs at
    /// construction (keyed to `modules-{lang}.db`); the Perl hub gets its
    /// own in `set_workspace_root` (keyed to `modules.db`). A type query
    /// reaching into an evicted file rehydrates the exact persisted bag
    /// through this LRU (`bag_present`). Kept behind its own `Arc` because
    /// `attach_pack_index` shares the cell itself (not its contents) into
    /// sub-indexes' `foreign_bag_cache`, so a later `set_workspace_root`
    /// install stays visible to them. See `docs/adr/memory-slice-2-lru.md`.
    pub(crate) bag_cache:
        Arc<std::sync::RwLock<Option<Arc<crate::index::pack_bag_cache::PackBagCache>>>>,
    /// Resident baked conclusions, byte-bounded. Installed alongside the bag
    /// cache; absent on an index with no store, which simply keeps decoding.
    pub(crate) conclusion_cache:
        Arc<std::sync::RwLock<Option<Arc<crate::index::conclusion_cache::ConclusionCache>>>>,
}

impl IndexCore {
    pub(crate) fn new() -> Self {
        IndexCore {
            cache: DashMap::new(),
            edges: ModuleEdgeIndexes::new(),
            loader_config_shapes: DashMap::new(),
            shapes_by_contributor: DashMap::new(),
            repair_pushed: DashMap::new(),
            stale_modules: DashMap::new(),
            builtins: DashMap::new(),
            available_modules: DashMap::new(),
            all_defs: DashMap::new(),
            inc_roots: std::sync::RwLock::new(Arc::new(Vec::new())),
            dependency_roots: std::sync::RwLock::new(None),
            queue: ResolveQueue {
                priority: Mutex::new(Vec::new()),
                pending: Mutex::new(Vec::new()),
                condvar: Condvar::new(),
            },
            resolved: ResolveNotify { mu: Mutex::new(()), cv: Condvar::new() },
            workspace_root: WorkspaceRootChannel {
                root: Mutex::new(None),
                condvar: Condvar::new(),
            },
            registration_gen: DashMap::new(),
            gen_counter: std::sync::atomic::AtomicU64::new(1),
            shape_bumps: std::sync::atomic::AtomicU64::new(0),
            flush_epoch: std::sync::atomic::AtomicU64::new(0),
            provider_diff_gen: DashMap::new(),
            long_lived: std::sync::atomic::AtomicBool::new(false),
            bag_cache: Arc::new(std::sync::RwLock::new(None)),
            conclusion_cache: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Record the process `@INC`, canonicalized so the per-asker rank can
    /// prefix-match candidate paths without query-time filesystem I/O.
    pub(crate) fn set_inc_roots(&self, roots: &[std::path::PathBuf]) {
        let canon: Vec<std::path::PathBuf> = roots
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();
        if let Ok(mut g) = self.inc_roots.write() {
            *g = Arc::new(canon);
        }
    }

    /// Declare this index a PACK sub-index with the given dependency-root
    /// prefixes (canonicalized). Even an empty list flips the tier
    /// semantics: the index's other files are the workspace's own
    /// (rename-editable), not `@INC`-style read-only modules.
    pub(crate) fn set_dependency_roots(&self, roots: Vec<std::path::PathBuf>) {
        let canon: Vec<std::path::PathBuf> = roots
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();
        if let Ok(mut g) = self.dependency_roots.write() {
            *g = Some(Arc::new(canon));
        }
    }

    /// Mint a fresh monotonic registration generation for `path`. The
    /// enrichment key's ABA-proof identity token: a re-registration (or an
    /// @INC re-resolve) bumps the gen, moving every consumer's key — where a
    /// bare Arc pointer could be freed and its address reused.
    pub(crate) fn mint_registration_gen(&self, path: &std::path::Path) {
        crate::util::ghost_stats::count("epoch.gen_mint");
        let g = self
            .gen_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.registration_gen.insert(path.to_path_buf(), g);
    }

    /// Stamp a generation for every name-keyed cache entry that lacks one.
    /// The @INC warm scan (`warm_cache`) writes blobs straight into the
    /// cache without a registration front door, so those providers would
    /// otherwise read gen 0 in `enrichment_key`. `or_insert` so a warm entry
    /// racing a workspace front-door registration keeps the front-door
    /// generation.
    pub(crate) fn stamp_missing_import_gens(&self) {
        for entry in self.cache.iter() {
            if let Some(ref cm) = *entry.value() {
                self.registration_gen.entry(cm.path.clone()).or_insert_with(|| {
                    crate::util::ghost_stats::count("epoch.gen_stamp_missing");
                    self.gen_counter
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                });
            }
        }
    }

    /// THE one spelling of "a module resolution landed in the name-keyed
    /// cache slot" — the resolver thread and the CLI both route here.
    /// `result` is the WHOLE parsed copy; `persisted` says its blob landed
    /// (the strip license) and `strip` is the eviction switch. On a resolved
    /// copy, in order: stale-pin clear BEFORE the copy is reachable (a
    /// re-resolve replaced the blob; a query racing this insert must not
    /// rehydrate the prior generation), a fresh registration generation
    /// (moves every consumer's enrichment key), then the projections — edge
    /// feeds and loader-config shapes — on the WHOLE analysis (the shape
    /// projection resolves config literals through the witness bag the strip
    /// drops: reads-whole-before-evict), then the registration-owned strip,
    /// then the store. Returns the stored copy (the caller's memo value).
    ///
    /// A `None` miss never clobbers an already-indexed copy: on-demand @INC
    /// resolution can miss a module the workspace indexer already built (a
    /// project module under a relative `use lib` the resolver's @INC doesn't
    /// cover), and clobbering would leave the reverse index pointing at a
    /// module the cache no longer holds (the orphan that broke cross-file
    /// Handler / dispatch lookup).
    /// A shape mutation landed — invalidate every memo keyed on the
    /// enrichment epoch. Over-invalidation is the safe direction.
    pub(crate) fn note_shape_change(&self) {
        self.shape_bumps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn insert_resolved(
        &self,
        module_name: &str,
        result: Option<Providers>,
        persisted: bool,
        strip: bool,
    ) -> Option<Providers> {
        crate::util::ghost_stats::count("epoch.shape.insert_resolved");
        self.note_shape_change();
        if let Some(ref providers) = result {
            // One purge, then one push per provider: `record_loader_shapes`
            // clears the contributor's entries before pushing, so calling it
            // per provider would leave only the last one's shapes.
            self.purge_loader_shapes(module_name);
            for m in providers {
                if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
                    bc.invalidate(&m.path);
                }
                self.mint_registration_gen(&m.path);
                self.edges.feed(module_name, &m.path, &m.analysis);
                self.push_loader_shapes(module_name, &m.analysis);
            }
        } else if matches!(self.cache.get(module_name).as_deref(), Some(Some(_))) {
            return None;
        }
        let stored: Option<Providers> = result.as_ref().map(|providers| {
            providers
                .iter()
                .map(|m| strip_import_copy_one(m, persisted, strip))
                .collect()
        });
        // The relation holds every provider; the name-keyed slot holds the
        // @INC-order winner (what `require` would load) — derived from the
        // set, never the only thing kept.
        if let Some(ref providers) = stored {
            self.adopt_inc_providers(module_name, providers);
        }
        let primary = stored.as_ref().and_then(|p| p.first().cloned());
        self.cache.insert(module_name.to_string(), primary);
        stored
    }

    /// Adopt `providers` as `@INC` candidates for `module_name`, keyed by
    /// path so a re-resolve REPLACES its own entry instead of stacking, and
    /// a workspace candidate for the same name is left untouched (the tiers
    /// share the relation; precedence stays with the cache slot).
    pub(crate) fn adopt_inc_providers(&self, module_name: &str, providers: &[Arc<CachedModule>]) {
        let mut v = self.all_defs.entry(module_name.to_string()).or_default();
        for cand in providers {
            match v.iter().position(|c| c.path == cand.path) {
                Some(i) => v[i] = cand.clone(),
                None => v.push(cand.clone()),
            }
        }
    }

    /// Drop `contributor`'s loader-shape entries. Split from the push so a
    /// name with several providers purges once and accumulates the union.
    pub(crate) fn purge_loader_shapes(&self, contributor: &str) {
        crate::util::ghost_stats::count("epoch.shape.record_loader_shapes");
        // The epoch always moves: this runs from a registration writer, and
        // over-invalidation is the safe direction. It is one relaxed add and
        // was never the cost here — the whole-map `retain` this replaced was,
        // being an ALL-SHARD write barrier run once per registered file.
        self.note_shape_change();
        // Retract only THIS contributor's names. A file that recorded no shape
        // — nearly all of them at workspace scale — is one lookup miss.
        let Some((_, names)) = self.shapes_by_contributor.remove(contributor) else {
            crate::util::ghost_stats::count("epoch.shape.purge_skipped_empty");
            return;
        };
        for name in names {
            let now_empty = match self.loader_config_shapes.get_mut(&name) {
                Some(mut v) => {
                    v.retain(|(c, _)| c != contributor);
                    v.is_empty()
                }
                None => false,
            };
            if now_empty {
                // Keep the "no shapes under this name" state indistinguishable
                // from never-recorded, so readers need no empty-vec arm.
                self.loader_config_shapes.remove_if(&name, |_, v| v.is_empty());
            }
        }
    }

    /// Project each `PluginLoad` fact's config value into a stored
    /// shape under its load-name. The value is a literal in the
    /// contributor's file, so `expr_type_at_span` with no index is
    /// already final — this is a registration-time projection of
    /// local facts (the same tier as export names), not a cached
    /// cross-file resolution.
    pub(crate) fn record_loader_shapes(&self, contributor: &str, analysis: &FileAnalysis) {
        self.purge_loader_shapes(contributor);
        self.push_loader_shapes(contributor, analysis);
    }

    fn push_loader_shapes(&self, contributor: &str, analysis: &FileAnalysis) {
        for f in &analysis.plugin.loads {
            let Some(span) = f.config_span else { continue };
            if let Some(t) = analysis.expr_type_at_span(span, None) {
                self.loader_config_shapes
                    .entry(f.name.clone())
                    .or_default()
                    .push((contributor.to_string(), t));
                self.shapes_by_contributor
                    .entry(contributor.to_string())
                    .or_default()
                    .push(f.name.clone());
            }
        }
    }

    /// Rebuild the edge indexes (`func → modules`, bridges, children, specs)
    /// from the current cache. The warm path writes blobs straight into the
    /// cache without touching the indexes, so a warm start that skips this
    /// leaves every reverse lookup blind (cold/warm attribution, the B6
    /// class).
    pub(crate) fn rebuild_reverse_index(&self) {
        self.edges.clear();
        for entry in self.cache.iter() {
            if let Some(ref cached) = *entry.value() {
                self.edges.feed(entry.key(), &cached.path, &cached.analysis);
            }
        }
        // Path-keyed handler feeds live outside the name-keyed cache; the
        // records are their only source after the clear.
        self.edges.replay_handler_records();
    }
}

/// The @INC tier's registration-owned strip: once the blob is persisted,
/// the resident copy drops its witness bag (the dominant share of a CPAN
/// module's payload; `bag_present` rehydrates through the hub's LRU).
/// Symbols and refs stay resident this slice — their reader routing for
/// the import tier is the follow-up in
/// `docs/prompt-storage-residuals.md`. Degraded
/// analyses keep the bag (their rows never persist).
pub(crate) fn strip_import_copy_one(
    m: &Arc<CachedModule>,
    persisted: bool,
    strip: bool,
) -> Arc<CachedModule> {
    if persisted && strip && !m.analysis.degraded {
        let mut fa = (*m.analysis).clone();
        fa.evict_to(crate::model::file_analysis::Residency::RowsOnly);
        Arc::new(CachedModule::new(m.path.clone(), Arc::new(fa)))
    } else {
        m.clone()
    }
}
