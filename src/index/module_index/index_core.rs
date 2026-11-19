//! `IndexCore` — the shared index organs as ONE struct.
//!
//! `ModuleIndex` (async side) and the resolver thread (blocking side) hold
//! the same `Arc<IndexCore>`, so every operation on the shared state has
//! exactly one spelling — a method here — and the side-effect set of an
//! operation cannot diverge per entry path (the drift class where an
//! @INC-resolved module fed `edges` but never `loader_config_shapes`
//! because the thread held loose Arcs without the shapes map).

use super::*;

/// What an ask came to at its generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A file the map placed answers it.
    Landed,
    /// Nothing the map places answers it.
    Miss,
}

struct PublishedMap {
    map: Arc<crate::build::dependency_map::DependencyMap>,
    /// Its generation as read: `DependencyMap::generation` re-stats the
    /// manifests, so a later call would already see the move.
    stamp: String,
}

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
    /// Every name the current dependency map places, listed without
    /// parsing (`DependencyMap::for_each_name`): what completion offers as
    /// "available" before anything lands it. Swapped whole per map
    /// generation, so a reader never sees a half-built listing.
    pub(crate) available: std::sync::RwLock<Arc<std::collections::HashSet<String>>>,
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
    /// The dependency map this process locates from: the one owner of the
    /// search roots (the visibility rank reads them, the landing locates
    /// through it, the walk skips its tier roots). Read by the served
    /// language's reader (`dependency_map_for`), and replaced only when a
    /// manifest it was read from moves (`reread_dependency_map`); `None`
    /// until the first read, which every reader takes as an empty map.
    dependency_map: std::sync::RwLock<Option<PublishedMap>>,
    /// Held across a read so concurrent first askers (the walk and the
    /// resolver thread) read the map once.
    map_read: Mutex<()>,
    /// The process's map generation: 0 before the first map, then one more
    /// for every map that moved. A dependency placement carries the
    /// generation that last made it (`FileRecords`), and the memo below is
    /// current only at this one.
    pub(crate) map_gen: std::sync::atomic::AtomicU64,
    /// Between a map move and its sweep: placements of an older generation
    /// may still be held, and the candidate reads leave them out.
    pub(crate) map_moving: std::sync::atomic::AtomicBool,
    /// The resolver's memo: every ask, with the generation it was answered
    /// at and its outcome. An ask current here is not asked again; a miss
    /// lives only here, never in the name slot or the store.
    pub(crate) asked: DashMap<crate::build::dependency_map::Ask, (u64, Outcome)>,
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
    /// List `map`'s names and publish them as the available set.
    pub(crate) fn list_available(&self, map: &crate::build::dependency_map::DependencyMap, workspace: Option<&std::path::Path>, extensions: &[&str]) {
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        crate::util::ghost_stats::timed("resolver.list_names", || {
            map.for_each_name(workspace, extensions, &mut |n| {
                names.insert(n);
            })
        });
        log::info!("Dependency map lists {} names", names.len());
        *self.available.write().unwrap() = Arc::new(names);
    }

    /// The available set as last listed.
    pub(crate) fn available_names(&self) -> Arc<std::collections::HashSet<String>> {
        Arc::clone(&self.available.read().unwrap())
    }

    pub(crate) fn new() -> Self {
        IndexCore {
            cache: DashMap::new(),
            edges: ModuleEdgeIndexes::new(),
            loader_config_shapes: DashMap::new(),
            shapes_by_contributor: DashMap::new(),
            repair_pushed: DashMap::new(),
            builtins: DashMap::new(),
            available: Default::default(),
            all_defs: DashMap::new(),
            leaf_defs: DashMap::new(),
            records: FileRecords::default(),
            all_files: DashMap::new(),
            dependency_map: std::sync::RwLock::new(None),
            map_read: Mutex::new(()),
            map_gen: std::sync::atomic::AtomicU64::new(0),
            map_moving: std::sync::atomic::AtomicBool::new(false),
            asked: DashMap::new(),
            queue: ResolveQueue::new(),
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

    /// The map this process locates from, taken on first ask from the
    /// store when a kept copy still holds, else read by `driver`'s reader
    /// for `root` (it may shell out, so it is never re-read on a query
    /// path). `None` when the language has no dependency tier.
    pub(crate) fn dependency_map_for(
        &self,
        driver: &dyn crate::build::language_driver::LanguageDriver,
        root: Option<&std::path::Path>,
    ) -> Option<Arc<crate::build::dependency_map::DependencyMap>> {
        if let Some(m) = self.dependency_map() {
            return Some(m);
        }
        let _g = self.map_read.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = self.dependency_map() {
            return Some(m);
        }
        let conn = self.map_store(driver);
        let map = Arc::new(
            load_kept_map(driver, root, conn.as_ref()).or_else(|| read_and_keep(driver, root, conn.as_ref()))?,
        );
        name_from_search_roots(driver, &map);
        self.publish_dependency_map(Arc::clone(&map));
        Some(map)
    }

    /// The served language's store, where a map is kept between runs; `None`
    /// for a language with no map, and before a workspace root is known, so
    /// nothing is kept under the global cache by mistake.
    fn map_store(
        &self,
        driver: &dyn crate::build::language_driver::LanguageDriver,
    ) -> Option<rusqlite::Connection> {
        if !driver.has_dependency_map() {
            return None;
        }
        let key = self.workspace_root.root.lock().unwrap_or_else(|e| e.into_inner()).clone()??;
        crate::index::module_cache::open_cache_db(Some(&key), driver.id())
    }

    /// Re-read the map after one of its manifests moved. `true` when the
    /// read is a new map (a move, published); an unchanged read changes
    /// nothing.
    pub(crate) fn reread_dependency_map(
        &self,
        driver: &dyn crate::build::language_driver::LanguageDriver,
        root: Option<&std::path::Path>,
    ) -> bool {
        let _g = self.map_read.lock().unwrap_or_else(|e| e.into_inner());
        match read_and_keep(driver, root, self.map_store(driver).as_ref()) {
            Some(map) => {
                name_from_search_roots(driver, &map);
                self.publish_dependency_map(Arc::new(map))
            }
            None => false,
        }
    }

    /// The map read so far, if any.
    ///
    /// The visibility rank reads it on request paths, from every worker, so
    /// the read is a generation load and a per-thread copy; the lock is
    /// taken only when the generation moved since this thread last looked.
    /// A lock read per call writes the lock word from every worker.
    pub(crate) fn dependency_map(&self) -> Option<Arc<crate::build::dependency_map::DependencyMap>> {
        type Seen = Option<(u64, u64, Arc<crate::build::dependency_map::DependencyMap>)>;
        thread_local! {
            static SEEN: std::cell::RefCell<Seen> = const { std::cell::RefCell::new(None) };
        }
        let gen = self.map_generation();
        if gen == 0 {
            return None;
        }
        SEEN.with(|seen| {
            if let Some((core, at, map)) = seen.borrow().as_ref() {
                if *core == self.core_id && *at == gen {
                    return Some(Arc::clone(map));
                }
            }
            let slot = self.dependency_map.read().unwrap_or_else(|e| e.into_inner());
            let published = slot.as_ref()?;
            let map = Arc::clone(&published.map);
            // Keyed by the generation read under the lock: the publish
            // advances both together.
            *seen.borrow_mut() = Some((self.core_id, self.map_generation(), Arc::clone(&map)));
            Some(map)
        })
    }

    /// Publish `map`. The first is generation 1; a later one whose
    /// generation differs is a move: the generation advances, older
    /// placements stop being current, and the resolver thread is told to
    /// re-ask its memo and sweep. `true` when it moved.
    pub(crate) fn publish_dependency_map(&self, map: Arc<crate::build::dependency_map::DependencyMap>) -> bool {
        use std::sync::atomic::Ordering;
        let stamp = map.generation();
        let mut slot = self.dependency_map.write().unwrap_or_else(|e| e.into_inner());
        let moved = match slot.as_ref() {
            None => {
                self.map_gen.store(1, Ordering::Release);
                false
            }
            Some(old) if old.stamp == stamp => return false,
            Some(_) => {
                self.map_moving.store(true, Ordering::Release);
                self.map_gen.fetch_add(1, Ordering::AcqRel);
                true
            }
        };
        *slot = Some(PublishedMap { map, stamp });
        drop(slot);
        if moved {
            crate::util::ghost_stats::count("dependency.map_moved");
            self.note_shape_change();
            self.queue.map_moved.store(true, Ordering::Release);
            self.queue.wake();
        }
        moved
    }

    /// The map and its generation, read together: a landing stamps what it
    /// located with the generation of the map it located through.
    pub(crate) fn current_map(&self) -> (Arc<crate::build::dependency_map::DependencyMap>, u64) {
        let slot = self.dependency_map.read().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some(p) => (Arc::clone(&p.map), self.map_generation()),
            None => (Arc::new(crate::build::dependency_map::DependencyMap::empty()), self.map_generation()),
        }
    }

    /// The current map generation (0: no map yet).
    pub(crate) fn map_generation(&self) -> u64 {
        self.map_gen.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether `ask` was answered at the current generation.
    pub(crate) fn asked_now(&self, ask: &crate::build::dependency_map::Ask) -> bool {
        let now = self.map_generation();
        self.asked.get(ask).is_some_and(|e| e.0 == now)
    }

    /// THE ask: queue each of `asks` not answered at the current generation
    /// for the resolver thread. Non-blocking; one-shot callers `wait_idle`.
    pub(crate) fn ask(&self, asks: impl IntoIterator<Item = crate::build::dependency_map::Ask>) {
        let fresh: Vec<_> = asks
            .into_iter()
            .filter(|a| {
                let settled = self.asked_now(a);
                if settled {
                    if let crate::build::dependency_map::Ask::Name(n) = a {
                        crate::util::timings::record_cached(n.as_str());
                    }
                }
                !settled
            })
            .collect();
        if !fresh.is_empty() {
            self.queue.ask(fresh);
        }
    }

    /// Whether `path` is a current candidate under `name`: always, unless a
    /// map move left it a placement of an older generation the sweep has not
    /// reached yet. Free outside a move.
    pub(crate) fn placed_now(&self, path: &crate::util::canonical_path::CanonicalPath, name: &str) -> bool {
        !self.map_moving.load(std::sync::atomic::Ordering::Acquire)
            || self.records.placed_at(path, name, self.map_generation())
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
    /// Each placement is stamped with `map_gen`, the generation that located
    /// it. A miss touches nothing: it is the memo's (`IndexCore::asked`),
    /// never the slot's. `door` is the served language's front door, whose
    /// projections a landed file gets, as a project file of that language
    /// does.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn land_providers(
        &self,
        module_name: &str,
        result: Option<Providers>,
        persisted: bool,
        strip: bool,
        map_gen: u64,
        door: FrontDoor,
    ) -> Option<Providers> {
        let providers = self.tier_providers(result)?;
        crate::util::ghost_stats::count("epoch.shape.insert_resolved");
        self.note_shape_change();
        // One purge, then one push per provider: `record_loader_shapes`
        // clears the contributor's entries before pushing, so calling it
        // per provider would leave only the last one's shapes.
        if door == FrontDoor::Hub {
            self.purge_loader_shapes(module_name);
        }
        for m in &providers {
            if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
                bc.invalidate(&m.path);
            }
            self.mint_registration_gen(&m.path);
            match door {
                FrontDoor::Hub => {
                    self.edges.feed(module_name, &m.path, &m.analysis);
                    self.push_loader_shapes(module_name, &m.analysis);
                }
                FrontDoor::Pack => {
                    let (_, specs) = ModuleIndex::prepare_pack_feed(&m.analysis);
                    self.publish_pack_edges(&m.path, &m.analysis, &specs);
                }
            }
        }
        let stored: Providers = providers
            .iter()
            .map(|m| strip_import_copy_one(m, persisted, strip))
            .collect();
        for (precedence, (whole, m)) in providers.iter().zip(&stored).enumerate() {
            self.records.land_dependency(
                &m.path,
                package_names(&whole.analysis),
                file_records::Placement { name: module_name.to_string(), precedence, map_gen },
                crate::index::module_cache::file_stamp(&m.path),
            );
            self.all_files.insert(m.path.clone(), m.clone());
            self.all_defs.entry(module_name.to_string()).or_default().upsert(m.clone());
        }
        // The map's first file takes the slot — what `require` would load —
        // unless a workspace file holds it: project code shadows an
        // installed copy (`derive_slot`'s rank).
        if !self.project_answers(module_name) {
            self.set_slot(module_name, stored.first().cloned());
        }
        Some(stored)
    }

    /// A pack file's edges: its specializations (`specs`, `(spec, primary)`
    /// read off the whole analysis before any strip), its parents (parent →
    /// child, the cross-file leg `direct_children_of` walks) and its
    /// includes (header → includer), both of which survive every strip.
    /// Each entry is a by-name key that self-heals at read, so a
    /// re-registration only adds.
    pub(crate) fn publish_pack_edges(
        &self,
        path: &crate::util::canonical_path::CanonicalPath,
        analysis: &FileAnalysis,
        specs: &[(String, String)],
    ) {
        for (spec, primary) in specs {
            self.edges.publish_spec(primary, spec);
        }
        for (child, parents) in analysis.package_parent_edges() {
            for parent in parents {
                self.edges.publish_child(parent, child);
            }
        }
        let includer = path.to_string_lossy();
        for target in analysis.pack.include_directives.iter().filter_map(|d| d.target.as_deref()) {
            self.edges.publish_includer(target, &includer);
        }
        for (member, _) in analysis.module_members() {
            self.edges.publish_member(&member, &includer);
        }
    }

    /// Whether a project file holds `name`'s slot. Project code shadows an
    /// installed copy, so an ask for such a name is answered without the
    /// tier: locating and parsing a twin the slot would never pick costs a
    /// parse per vendored module and answers nothing.
    pub(crate) fn project_answers(&self, name: &str) -> bool {
        matches!(
            self.cache.get(name).as_deref(),
            Some(Some(c)) if self.records.role_of(&c.path) == Some(crate::model::file_analysis::FileRole::Workspace)
        )
    }

    /// A project file that answered asks went away: those asks are open
    /// again, so the tier is asked for an installed copy next time.
    pub(crate) fn forget_project_answers(&self, names: &[(String, bool)]) {
        for (name, _) in names {
            self.asked.remove(&crate::build::dependency_map::Ask::Name(name.clone()));
        }
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
            .filter(|c| self.placed_now(&c.path, name))
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
        for p in &record.loads_as {
            self.drop_candidate(&p.name, path);
        }
        self.edges.remove_path_record(path);
        if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
            bc.invalidate(path);
        }
    }

    /// `path` is no longer a candidate under `name`: re-feed the name's
    /// edges from the survivors and re-derive its slot.
    fn drop_candidate(&self, name: &str, path: &crate::util::canonical_path::CanonicalPath) {
        if let Some(mut v) = self.all_defs.get_mut(name) {
            v.remove_path(path);
        }
        self.all_defs.remove_if(name, |_, v| v.is_empty());
        self.edges.purge_module(name);
        let survivors: Vec<Arc<CachedModule>> = self.all_defs.get(name).map(|v| v.to_vec()).unwrap_or_default();
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

    /// A map move's restamp: when every file the current map places under
    /// `name` (the project's aside — the walk owns those) is already held
    /// there, unchanged on disk, its placements carry over to this
    /// generation at the new map's precedence — no parse, no row read, no
    /// re-feed. `None` when anything differs; the caller lands the name
    /// afresh. Returns the held copies.
    pub(crate) fn restamp_placed(
        &self,
        located: &[crate::util::canonical_path::CanonicalPath],
        name: &str,
        map_gen: u64,
    ) -> Option<Providers> {
        use file_records::HeldAs;
        let mut held: Vec<(&crate::util::canonical_path::CanonicalPath, Arc<CachedModule>)> = Vec::new();
        for path in located {
            match self.records.placement_of(path, name, || crate::index::module_cache::file_stamp(path)) {
                HeldAs::Project => continue,
                HeldAs::Placed => held.push((path, self.all_files.get(path)?.clone())),
                HeldAs::Other => return None,
            }
        }
        if held.is_empty() {
            return None;
        }
        for (precedence, (path, _)) in held.iter().enumerate() {
            self.records.restamp(path, name, precedence, map_gen);
        }
        crate::util::ghost_stats::count_by("dependency.restamped", held.len() as u64);
        self.derive_slot(name);
        Some(held.into_iter().map(|(_, m)| m).collect())
    }

    /// The members of `module_name` its files declare, placed with them: a
    /// language whose names reach a module's members (python's
    /// `pkg.models.Base` in `pkg/models.py`) asks for the member, and the
    /// map places it at the module's file. Each is a placement of its own,
    /// so a move renews or sweeps it like the module's, and a name the map
    /// places elsewhere (another package in the same Perl file) is left to
    /// the file that spells it.
    pub(crate) fn place_members(
        &self,
        map: &crate::build::dependency_map::DependencyMap,
        module_name: &str,
        located: &[crate::util::canonical_path::CanonicalPath],
        map_gen: u64,
    ) {
        use crate::build::dependency_map::Ask;
        for (precedence, path) in located.iter().enumerate() {
            let Some(names) = self.records.dependency_names(path) else { continue };
            let Some(held) = self.all_files.get(path).map(|e| e.clone()) else { continue };
            for (name, _) in names {
                // A member's name extends its module's (`member_of` strips
                // trailing segments), so no other name needs the map asked.
                if name.len() <= module_name.len() || !name.starts_with(module_name) {
                    continue;
                }
                let here = map
                    .locate(&Ask::Name(name.clone()))
                    .iter()
                    .any(|l| l.path == *path && l.loads_as.as_deref() == Some(module_name));
                if !here {
                    continue;
                }
                self.place_held(path, &held, &name, precedence, map_gen);
            }
        }
    }

    /// Place a held dependency under one more name (`place_members` for a
    /// module's members, a by-path landing for every name its file offers).
    pub(crate) fn place_name(
        &self,
        path: &crate::util::canonical_path::CanonicalPath,
        name: &str,
        precedence: usize,
        map_gen: u64,
    ) {
        if let Some(held) = self.all_files.get(path).map(|e| e.clone()) {
            self.place_held(path, &held, name, precedence, map_gen);
        }
    }

    fn place_held(
        &self,
        path: &crate::util::canonical_path::CanonicalPath,
        held: &Arc<CachedModule>,
        name: &str,
        precedence: usize,
        map_gen: u64,
    ) {
        if self.records.place(path, file_records::Placement { name: name.to_string(), precedence, map_gen }) {
            self.all_defs.entry(name.to_string()).or_default().upsert(held.clone());
            self.derive_slot(name);
        }
    }

    /// The idle sweep after a map move: every placement an older generation
    /// made and the current one did not renew is taken back, a file with no
    /// placement left is retired, and the memo forgets the older asks.
    pub(crate) fn sweep_stale_placements(&self) {
        let now = self.map_generation();
        let stale = self.records.stale_placements(now);
        crate::util::ghost_stats::count_by("dependency.swept", stale.len() as u64);
        for (path, name) in stale {
            match self.records.unplace(&path, &name) {
                Some(true) => {
                    self.note_shape_change();
                    self.all_files.remove(&path);
                    self.drop_candidate(&name, &path);
                    self.edges.remove_path_record(&path);
                    if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
                        bc.invalidate(&path);
                    }
                }
                Some(false) => {
                    self.note_shape_change();
                    self.drop_candidate(&name, &path);
                }
                None => {}
            }
        }
        self.asked.retain(|_, (g, _)| *g == now);
        self.map_moving.store(false, std::sync::atomic::Ordering::Release);
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
/// The registration door a language's files go through: the Perl hub's
/// (`register_workspace_*`, edge feed plus loader-config shapes) or the
/// pack front door (`register_symbols`, the pack edges). A dependency lands
/// through its language's door, the same choice the workspace indexer
/// makes (`index_workspace_with_index` vs `index_pack_language`).
///
/// TODO(single-registration-target): one door for every language
/// (`docs/prompt-unify-language-paths.md`) retires this choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FrontDoor {
    Hub,
    Pack,
}

impl FrontDoor {
    pub(crate) fn of(driver: &dyn crate::build::language_driver::LanguageDriver) -> Self {
        if driver.caps().hub_enrichment {
            FrontDoor::Hub
        } else {
            FrontDoor::Pack
        }
    }
}

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

/// A path-named language names a file the map places from the search root
/// holding it (`six.py` in `site-packages` is `six`), so the map's roots
/// reach the naming before anything can land under them.
fn name_from_search_roots(
    driver: &dyn crate::build::language_driver::LanguageDriver,
    map: &crate::build::dependency_map::DependencyMap,
) {
    if let Some(marker) = driver.module_marker() {
        crate::build::walk_markers::set_search_roots(marker, &map.search_roots());
    }
}

/// The map kept in `conn` for `root`, when every input it was read from
/// still carries the stamp it was kept with. Its `generation()` is a fresh
/// read's, so nothing downstream can tell the two apart.
pub(crate) fn load_kept_map(
    driver: &dyn crate::build::language_driver::LanguageDriver,
    root: Option<&std::path::Path>,
    conn: Option<&rusqlite::Connection>,
) -> Option<crate::build::dependency_map::DependencyMap> {
    let (conn, root) = (conn?, root?);
    crate::util::timings::phase("dependency_map::load", || {
        let bytes = crate::index::module_cache::load_dependency_map(conn, &root.to_string_lossy())?;
        crate::build::dependency_map::DependencyMap::decode(&bytes, driver.dependency_spellings())
    })
}

/// Read the map afresh and keep it in `conn` over whatever was kept for
/// `root`. A map whose reader read something its inputs do not stamp is
/// read, not kept (`DependencyMap::encode`).
pub(crate) fn read_and_keep(
    driver: &dyn crate::build::language_driver::LanguageDriver,
    root: Option<&std::path::Path>,
    conn: Option<&rusqlite::Connection>,
) -> Option<crate::build::dependency_map::DependencyMap> {
    let map = crate::util::timings::phase("dependency_map::read", || driver.dependency_map(root))?;
    if let (Some(conn), Some(root)) = (conn, root) {
        crate::util::timings::phase("dependency_map::keep", || {
            crate::index::module_cache::save_dependency_map(conn, &root.to_string_lossy(), &map)
        });
    }
    Some(map)
}
