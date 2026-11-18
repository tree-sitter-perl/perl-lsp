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
    pub(crate) all_defs: DashMap<String, DefCandidates>,
    /// The files declaring a use-map class, keyed by the class's LEAF —
    /// the widening relation (`CrossFileLookup::leaf_def_candidates`). Its
    /// own table, never `all_defs`: a leaf key there would join every
    /// bare-name lookup and compete for the name-slot winner, so the leaf
    /// answers only where nothing indexed declares the identity.
    pub(crate) leaf_defs: DashMap<String, DefCandidates>,
    /// Every held file's record: its names and its role. Shared because
    /// both sides land files — the front doors the project's, the resolver
    /// thread the `@INC` tier's.
    pub(crate) records: FileRecords,
    /// The path home: every held file, keyed by canonical path, whatever its
    /// role — including files that declare NOTHING registrable (a header-only
    /// `#include` shim, a packageless entrypoint), which the name-keyed views
    /// cannot reach. Written beside `records`, record first, so a sweep never
    /// meets a file without its role.
    pub(crate) all_files: DashMap<crate::util::canonical_path::CanonicalPath, Arc<CachedModule>>,
    /// The `@INC` roots this process resolves from, canonical, most-
    /// preferred first. Recorded ONCE by the resolver thread (discovery
    /// shells out to `perl`, so no query path may re-derive it) and read by
    /// every origin's `VisibilityAxis::for_origin`.
    pub(crate) inc_roots: std::sync::RwLock<Arc<Vec<std::path::PathBuf>>>,
    pub(crate) queue: ResolveQueue,
    pub(crate) resolved: ResolveNotify,
    pub(crate) workspace_root: WorkspaceRootChannel,
    /// Monotonic per-path registration generation — the ABA-proof identity
    /// token `enrichment_key` hashes (an Arc pointer can be freed and its
    /// address reused; a counter can't run backwards). Bumped by every
    /// registration front door.
    pub(crate) registration_gen: DashMap<crate::util::canonical_path::CanonicalPath, u64>,
    pub(crate) gen_counter: std::sync::atomic::AtomicU64,
    /// Monotone count of index-shape mutations NOT already visible through
    /// `gen_counter` or the freshness write count (cache-slot swaps on
    /// unregister, loader-shape rewrites). Third leg of the enrichment-key
    /// memo's validity epoch — over-invalidation is always safe here, a
    /// missed bump is silent staleness, so mutators bump even when a gen
    /// mint usually accompanies them.
    pub(crate) shape_bumps: std::sync::atomic::AtomicU64,
    /// Process-unique identity, for sweep stores that key per index: an
    /// `Arc` address can be reused by a later core and inherit its entries.
    pub(crate) core_id: u64,
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
    /// Slice-2 rehydration store, keyed to the served language's DB
    /// (`modules.db` installed by `set_workspace_root`, `modules-{lang}.db`
    /// by the pack indexer). A type query reaching into an evicted file
    /// rehydrates the exact persisted bag through this LRU (`bag_present`).
    /// See `docs/adr/memory-slice-2-lru.md`.
    pub(crate) bag_cache:
        std::sync::RwLock<Option<Arc<crate::index::pack_bag_cache::PackBagCache>>>,
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
            builtins: DashMap::new(),
            available_modules: DashMap::new(),
            all_defs: DashMap::new(),
            leaf_defs: DashMap::new(),
            records: FileRecords::default(),
            all_files: DashMap::new(),
            inc_roots: std::sync::RwLock::new(Arc::new(Vec::new())),
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
            core_id: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            flush_epoch: std::sync::atomic::AtomicU64::new(0),
            provider_diff_gen: DashMap::new(),
            long_lived: std::sync::atomic::AtomicBool::new(false),
            bag_cache: std::sync::RwLock::new(None),
            conclusion_cache: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Record the process `@INC`, canonicalized so the per-asker rank can
    /// prefix-match candidate paths without query-time filesystem I/O.
    pub(crate) fn set_inc_roots(&self, roots: &[std::path::PathBuf]) {
        let canon: Vec<std::path::PathBuf> = roots
            .iter()
            .map(|p| crate::util::canonical_path::CanonicalPath::resolve(p.as_path()).into_path_buf())
            .collect();
        if let Ok(mut g) = self.inc_roots.write() {
            *g = Arc::new(canon);
        }
    }

    /// Mint a fresh monotonic registration generation for `path`. The
    /// enrichment key's ABA-proof identity token: a re-registration (or an
    /// @INC re-resolve) bumps the gen, moving every consumer's key — where a
    /// bare Arc pointer could be freed and its address reused.
    pub(crate) fn mint_registration_gen(&self, path: &crate::util::canonical_path::CanonicalPath) {
        crate::util::ghost_stats::count("epoch.gen_mint");
        let g = self
            .gen_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.registration_gen.insert(path.clone(), g);
    }

    /// A shape mutation landed — invalidate every memo keyed on the
    /// enrichment epoch. Over-invalidation is the safe direction.
    pub(crate) fn note_shape_change(&self) {
        self.shape_bumps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Write the name-keyed winner slot. Every slot writer goes through
    /// here or [`Self::clear_slot`]: a slot write moves `get_cached`
    /// answers without a per-path generation mint, so the enrichment epoch
    /// must move with it, and a writer that forgot left memos keyed on the
    /// old answer (`layering_tests::name_slot_writes_move_the_epoch`). The
    /// epoch moves AFTER the write, so a reader that saw the new epoch
    /// cannot have read the old slot.
    pub(crate) fn set_slot(&self, name: &str, value: Option<Arc<CachedModule>>) {
        self.cache.insert(name.to_string(), value);
        self.note_shape_change();
    }

    /// Empty the name-keyed slot ([`Self::set_slot`]'s removal half).
    pub(crate) fn clear_slot(&self, name: &str) {
        self.cache.remove(name);
        self.note_shape_change();
    }

    /// Offer `cand` for `name`'s slot: it is written when the slot is empty
    /// or holds `None`, or when `replaces(existing)` says it wins.
    pub(crate) fn offer_slot(
        &self,
        name: &str,
        cand: &Arc<CachedModule>,
        replaces: impl FnOnce(&Arc<CachedModule>) -> bool,
    ) {
        use dashmap::mapref::entry::Entry;
        let wrote = match self.cache.entry(name.to_string()) {
            Entry::Vacant(v) => {
                v.insert(Some(cand.clone()));
                true
            }
            Entry::Occupied(mut o) => {
                let replace = match o.get() {
                    None => true,
                    Some(existing) => replaces(existing),
                };
                if replace {
                    o.insert(Some(cand.clone()));
                }
                replace
            }
        };
        if wrote {
            self.note_shape_change();
        }
    }

    /// THE one spelling of "the `@INC` tier landed `module_name`" — the
    /// resolver thread and the CLI both route here. `result` is the WHOLE
    /// copy of every file the map located for the name, in the map's order
    /// (a landing adds to the name's candidates, as every front door does);
    /// `persisted` says their rows landed (the strip licence) and `strip` is
    /// the eviction switch. Each file is landed by path: a fresh registration
    /// generation (moves every consumer's enrichment key), the projections —
    /// edge feeds and loader-config shapes — on the WHOLE analysis (the shape
    /// projection reads the witness bag the strip drops), the strip, then the
    /// record and the path home. The candidate relation and the name slot are
    /// derived from the records. Returns the stored copies (the caller's
    /// memo value).
    ///
    /// A `None` miss never clobbers an already-indexed copy: on-demand @INC
    /// resolution can miss a module the workspace indexer already built (a
    /// project module under a relative `use lib` the resolver's @INC doesn't
    /// cover), and clobbering would leave the reverse index pointing at a
    /// module the cache no longer holds.
    pub(crate) fn land_providers(
        &self,
        module_name: &str,
        result: Option<Providers>,
        persisted: bool,
        strip: bool,
    ) -> Option<Providers> {
        crate::util::ghost_stats::count("epoch.shape.insert_resolved");
        self.note_shape_change();
        let Some(providers) = self.tier_providers(result) else {
            if !matches!(self.cache.get(module_name).as_deref(), Some(Some(_))) {
                self.set_slot(module_name, None);
            }
            return None;
        };
        // One purge, then one push per provider: `record_loader_shapes`
        // clears the contributor's entries before pushing, so calling it
        // per provider would leave only the last one's shapes.
        self.purge_loader_shapes(module_name);
        for m in &providers {
            if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
                bc.invalidate(&m.path);
            }
            self.mint_registration_gen(&m.path);
            self.edges.feed(module_name, &m.path, &m.analysis);
            self.push_loader_shapes(module_name, &m.analysis);
        }
        let stored: Providers = providers
            .iter()
            .map(|m| strip_import_copy_one(m, persisted, strip))
            .collect();
        for (precedence, (whole, m)) in providers.iter().zip(&stored).enumerate() {
            self.records.land_dependency(
                &m.path,
                package_names(&whole.analysis),
                (module_name.to_string(), precedence),
            );
            self.all_files.insert(m.path.clone(), m.clone());
            self.all_defs.entry(module_name.to_string()).or_default().upsert(m.clone());
        }
        // The map's first file takes the slot — what `require` would load —
        // unless a workspace file holds it: project code shadows an
        // installed copy (`derive_slot`'s rank).
        let shadowed = matches!(
            self.cache.get(module_name).as_deref(),
            Some(Some(c)) if self.records.role_of(&c.path) == Some(crate::model::file_analysis::FileRole::Workspace)
        );
        if !shadowed {
            self.set_slot(module_name, stored.first().cloned());
        }
        Some(stored)
    }

    /// The files of a landing the tier may hold: a path a front door
    /// registered is the walk's (project `lib/` is on `@INC` too), and the
    /// tier never lands a second copy of it. `None` when nothing is left.
    pub(crate) fn tier_providers(&self, result: Option<Providers>) -> Option<Providers> {
        let mut providers = result?;
        providers.retain(|m| {
            let owned = self.records.role_of(&m.path) == Some(crate::model::file_analysis::FileRole::Workspace);
            if owned {
                crate::util::ghost_stats::count("dependency.workspace_path_skipped");
            }
            !owned
        });
        (!providers.is_empty()).then_some(providers)
    }

    /// Re-derive `name`'s slot from its candidates, ranked by role then the
    /// map: a workspace file shadows an installed one, and among workspace
    /// files the class-rank/path tie-break decides; among dependencies the
    /// map's precedence does (what `require` would load). No candidate
    /// leaves the slot alone — a remembered miss, or a name nothing landed.
    pub(crate) fn derive_slot(&self, name: &str) {
        use crate::model::file_analysis::FileRole;
        let Some(cands) = self.all_defs.get(name).map(|v| v.to_vec()) else {
            return;
        };
        let (workspace, deps): (Vec<&Arc<CachedModule>>, Vec<&Arc<CachedModule>>) = cands
            .iter()
            .partition(|c| self.records.role_of(&c.path) == Some(FileRole::Workspace));
        let winner = if !workspace.is_empty() {
            best_candidate(&workspace, name, &|m, n| self.defines_class(m, n))
        } else {
            deps.into_iter()
                .min_by_key(|c| (self.records.precedence(&c.path, name).unwrap_or(usize::MAX), c.path.clone()))
                .cloned()
        };
        if let Some(w) = winner {
            let held = matches!(self.cache.get(name).as_deref(), Some(Some(c)) if Arc::ptr_eq(c, &w));
            if !held {
                self.set_slot(name, Some(w));
            }
        }
    }

    /// Undo a dependency landing of `path`: its record and path home, its
    /// candidacy under every name the map placed it, those names' edge feeds
    /// (re-fed from the survivors) and slots (re-derived). A workspace file
    /// at `path` is the front door's and is left alone.
    pub(crate) fn retire_dependency(&self, path: &crate::util::canonical_path::CanonicalPath) {
        let Some(record) = self.records.forget_dependency(path) else {
            return;
        };
        crate::util::ghost_stats::count("dependency.retired");
        self.note_shape_change();
        self.all_files.remove(path);
        for (name, _) in &record.loads_as {
            if let Some(mut v) = self.all_defs.get_mut(name) {
                v.remove_path(path);
            }
            self.all_defs.remove_if(name, |_, v| v.is_empty());
            self.edges.purge_module(name);
            let survivors: Vec<Arc<CachedModule>> =
                self.all_defs.get(name).map(|v| v.to_vec()).unwrap_or_default();
            for c in &survivors {
                self.edges.feed(name, &c.path, &c.analysis);
            }
            if survivors.is_empty() {
                self.purge_loader_shapes(name);
                if matches!(self.cache.get(name).as_deref(), Some(Some(c)) if c.path == *path) {
                    self.clear_slot(name);
                }
            } else {
                self.derive_slot(name);
            }
        }
        self.edges.remove_path_record(path);
        if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
            bc.invalidate(path);
        }
    }

    /// A front door is about to register `path` as the project's: a
    /// dependency landing at the same path (the resolver got there first)
    /// retires, so its names re-derive and the project copy replaces it.
    pub(crate) fn claim_for_workspace(
        &self,
        path: &crate::util::canonical_path::CanonicalPath,
        names: Vec<(String, bool)>,
    ) -> Option<Vec<(String, bool)>> {
        self.retire_dependency(path);
        self.records.register_workspace(path, names)
    }

    /// Does `m` declare a `Class`/type named `name`? Rank source for the
    /// slot tie-break. Reads the record first — the resident copy's
    /// `symbols` may be evicted — and falls back to the symbol scan for
    /// copies registered whole (recovery paths, tests).
    pub(crate) fn defines_class(&self, m: &CachedModule, name: &str) -> bool {
        if let Some(declares) = self
            .records
            .with_names(&m.path, |names| names.iter().any(|(n, is_class)| n == name && *is_class))
        {
            return declares;
        }
        m.analysis
            .symbols()
            .iter()
            .any(|s| matches!(s.kind, crate::model::file_analysis::SymKind::Class) && s.name == name)
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
