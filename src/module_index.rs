//! Module index: public API for cross-file Perl module intelligence.
//!
//! Wraps a concurrent cache (`DashMap`) backed by a background resolver thread.
//! Async LSP handlers only read from the cache (zero I/O). The resolver thread
//! handles @INC discovery, in-process parsing, SQLite persistence, and cpanfile
//! pre-scanning.
//!
//! The cache stores the full `FileAnalysis` (not a lossy summary), so
//! cross-file refs, type constraints, call bindings, and framework context
//! all survive the module boundary.
//!
//! See also:
//! - `module_resolver.rs` — resolver thread, in-process parsing
//! - `module_cache.rs` — SQLite persistence (schema v9, bincode+zstd blobs)
//! - `cpanfile.rs` — cpanfile parsing

#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use dashmap::DashMap;
use tower_lsp::Client;

use crate::file_analysis::{CrossFileLookup, FileAnalysis, SymKind};
#[cfg(test)]
use crate::file_analysis::InferredType;
use crate::module_resolver;

// ---- Public types ----

// `CachedModule` / `SubInfo` are pure views over `FileAnalysis` and live
// there (the index depends on the model, not vice versa); re-exported so
// index consumers keep one import site.
pub use crate::file_analysis::{CachedModule, SubInfo};

type InferredTypeOwned = crate::file_analysis::InferredType;

/// Rehydration misses on evicted copies this process served degraded
/// (`rehydrate_or_resident`'s invariant-break arm). Process-global: the
/// residency story spans the hub and every pack sub-index, and the flake
/// this polices ("inputs vanished" cold runs) is a per-process verdict.
static REHYDRATION_MISSES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// How many evicted copies failed to rehydrate this process (each was
/// served as a stripped resident — quietly incomplete answers). Zero in a
/// healthy session; the strict gate (`PERL_LSP_STRICT_RESIDENCY`) panics
/// at the first miss instead of counting. Observability hook read by the
/// residency tests; production reacts via the strict gate, not this reader.
#[cfg(test)]
pub fn rehydration_miss_count() -> usize {
    REHYDRATION_MISSES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Mint a fresh monotonic registration generation for `path`. The enrichment
/// key's ABA-proof identity token: a re-registration (or an @INC re-resolve)
/// bumps the gen, moving every consumer's key — where a bare Arc pointer
/// could be freed and its address reused. The resolver THREAD holds the raw
/// Arcs (not a `&ModuleIndex`), so this is a free fn both the thread and the
/// `ModuleIndex` methods route through.
pub(crate) fn mint_registration_gen(
    registration_gen: &DashMap<std::path::PathBuf, u64>,
    gen_counter: &std::sync::atomic::AtomicU64,
    path: &std::path::Path,
) {
    let g = gen_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    registration_gen.insert(path.to_path_buf(), g);
}

/// Stamp a generation for every name-keyed cache entry that lacks one. The
/// @INC warm scan (`warm_cache`) writes blobs straight into the cache
/// without a registration front door, so those providers would otherwise
/// read gen 0 in `enrichment_key`. `or_insert` so a warm entry racing a
/// workspace front-door registration keeps the front-door generation.
pub(crate) fn stamp_missing_import_gens(
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    registration_gen: &DashMap<std::path::PathBuf, u64>,
    gen_counter: &std::sync::atomic::AtomicU64,
) {
    for entry in cache.iter() {
        if let Some(ref cm) = *entry.value() {
            registration_gen.entry(cm.path.clone()).or_insert_with(|| {
                gen_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            });
        }
    }
}

/// The linkage-visible feed a registration extracts from a WHOLE analysis:
/// (name, declares-a-Class) per visible symbol. Collected before any strip
/// so the feeds and tie-breaks never read an emptied `symbols`.
fn collect_linkage_feed(analysis: &FileAnalysis) -> Vec<(String, bool)> {
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut feed: Vec<(String, bool)> = Vec::new();
    for sym in &analysis.symbols {
        // The C-linkage surface (`FileAnalysis::is_linkage_visible`) —
        // the same predicate completion gathering uses, so every name
        // registered here is also offerable and vice versa.
        if !analysis.is_linkage_visible(sym) {
            continue;
        }
        let is_class = matches!(sym.kind, SymKind::Class);
        match index.get(sym.name.as_str()) {
            // A file declaring both a value AND a Class under one name
            // ranks as a Class.
            Some(&i) => feed[i].1 |= is_class,
            None => {
                index.insert(sym.name.as_str(), feed.len());
                feed.push((sym.name.clone(), is_class));
            }
        }
    }
    // Class rank is visibility-INDEPENDENT (the old occupant scan matched
    // any Class symbol): a non-linkage-visible Class sharing a visible
    // value's name still ranks the file as declaring that Class.
    for sym in &analysis.symbols {
        if matches!(sym.kind, SymKind::Class) {
            if let Some(&i) = index.get(sym.name.as_str()) {
                feed[i].1 = true;
            }
        }
    }
    feed
}

/// Pick the winner among same-name candidates by the SAME total order
/// `register_symbols` uses for the global cache slot: a TYPE (Class) beats a
/// Sub/value, then the smallest canonical path breaks the tie (order-independent
/// — no reliance on registration order). Factored so the scoped lookup and the
/// registration winner agree by construction.
fn best_candidate<'c>(
    cands: &[&'c Arc<CachedModule>],
    name: &str,
    defines_class: &dyn Fn(&CachedModule, &str) -> bool,
) -> Option<Arc<CachedModule>> {
    cands
        .iter()
        .copied()
        .max_by(|a, b| {
            let (ac, bc) = (defines_class(a, name), defines_class(b, name));
            // Class beats non-class; then SMALLER path wins (reverse for max_by).
            ac.cmp(&bc).then_with(|| b.path.cmp(&a.path))
        })
        .cloned()
}

// ---- Internal sync primitives (pub(crate) for resolver thread) ----

/// Thread-safe queue: Mutex<Vec> + Condvar.
pub(crate) struct ResolveQueue {
    /// High priority: stale modules from open files. Drained first.
    pub priority: Mutex<Vec<String>>,
    /// Normal priority: missing modules.
    pub pending: Mutex<Vec<String>>,
    pub condvar: Condvar,
}

/// Signaled after each module is resolved.
pub(crate) struct ResolveNotify {
    pub mu: Mutex<()>,
    pub cv: Condvar,
}

/// Channel for workspace root from initialize() → resolver thread.
pub(crate) struct WorkspaceRootChannel {
    pub root: Mutex<Option<Option<String>>>,
    pub condvar: Condvar,
}

/// Concurrent module cache with background resolution.
///
/// The reverse-edge maps over the module cache, bundled so every feed
/// site updates all of them in lockstep. Every map answers "which
/// modules…" for a different edge:
///
/// - `names`: symbol/export name → modules declaring or exporting it.
///   The single generic "find me modules with symbol X" primitive —
///   hover, signature help, goto-def, auto-import, and the
///   unimported-completion path all route through it instead of
///   reinventing per-feature cache walks. Covers every module-visible
///   symbol kind (Sub, Method, Package, Class, Module, HashKeyDef,
///   Handler) plus the export/export_ok lists (XS exporters name
///   functions with no Perl body). Callers wanting narrower semantics
///   filter via per-module inspection.
/// - `bridges`: class → modules declaring a `PluginNamespace` whose
///   `bridges` list contains `Bridge::Class(class)`. The one reverse
///   index for plugin-synthesized content; queried through
///   `for_each_entity_bridged_to`.
/// - `children`: parent class/role → modules containing a package
///   that `isa`/composes it (inverse `package_parents`). The
///   long-distance primitive: "who composes this role" /
///   "who subclasses this class" in O(1).
///
/// The bundle exists because the feeds must never diverge across the
/// resolve insert path, the SQLite warm rebuild, and workspace
/// registration — a map fed on insert but not on rebuild serves cold
/// sessions and starves warm ones (the twice-paid B6 lesson). One
/// `feed()` per site makes a missed map unrepresentable.
pub struct ModuleEdgeIndexes {
    names: DashMap<String, Vec<String>>,
    bridges: DashMap<String, Vec<String>>,
    children: DashMap<String, Vec<String>>,
    /// primary template → modules declaring a specialization of it (inverse
    /// `FileAnalysis.specializes`). The `Specializes` family edge's
    /// cross-file half; member resolution never reads it.
    specs: DashMap<String, Vec<String>>,
    /// The indexable-name list each module last fed — the symbols-derived
    /// half of `feed`, recorded from the WHOLE analysis so a rebuild over
    /// symbol-EVICTED cache copies (`rebuild_reverse_index*` after the
    /// workspace indexer strips) replays the names instead of reading empty
    /// vecs and silently blinding `modules_with_symbol`/`find_exporters`
    /// for every workspace module. `clear()` keeps it (rebuilds are exactly
    /// when it's needed); `purge_module` drops it with the edges.
    name_records: DashMap<String, Vec<String>>,
}

impl ModuleEdgeIndexes {
    pub fn new() -> Self {
        ModuleEdgeIndexes {
            names: DashMap::new(),
            bridges: DashMap::new(),
            children: DashMap::new(),
            specs: DashMap::new(),
            name_records: DashMap::new(),
        }
    }

    /// Register every edge `analysis` contributes under `module_name`.
    /// The ONLY write path besides `purge_module`/`clear` — new edge
    /// maps get their extraction added here and nowhere else. Eviction-
    /// aware: a symbol-stripped copy replays its recorded name list; a
    /// whole copy recomputes and re-records it.
    pub fn feed(&self, module_name: &str, analysis: &FileAnalysis) {
        let names: Vec<String> = if analysis.symbols_are_evicted() {
            match self.name_records.get(module_name) {
                Some(rec) => rec.clone(),
                // No record (a stripped copy fed without ever being fed
                // whole — shouldn't happen, but degrade to the pinned
                // export names rather than nothing).
                None => Self::indexable_names(analysis),
            }
        } else {
            let names = Self::indexable_names(analysis);
            self.name_records
                .insert(module_name.to_string(), names.clone());
            names
        };
        for name in names {
            self.names
                .entry(name)
                .or_default()
                .push(module_name.to_string());
        }
        for class in Self::bridge_classes(analysis) {
            self.bridges
                .entry(class)
                .or_default()
                .push(module_name.to_string());
        }
        for parent in Self::parent_classes(analysis) {
            self.children
                .entry(parent)
                .or_default()
                .push(module_name.to_string());
        }
        for primary in Self::spec_primaries(analysis) {
            self.specs
                .entry(primary)
                .or_default()
                .push(module_name.to_string());
        }
    }

    /// Remove `module_name` from every bucket of every map. Runs
    /// before re-registration so stale edges from a prior version of
    /// the same module don't accumulate (phantom-module lookups).
    pub fn purge_module(&self, module_name: &str) {
        for map in [&self.names, &self.bridges, &self.children, &self.specs] {
            map.retain(|_key, mods| {
                mods.retain(|m| m != module_name);
                !mods.is_empty()
            });
        }
        self.name_records.remove(module_name);
    }

    /// Wipe the edge maps for a rebuild. Deliberately KEEPS `name_records`
    /// — the rebuild re-feeds from cache copies that may be symbol-evicted,
    /// and the records are their only complete name source.
    pub fn clear(&self) {
        self.names.clear();
        self.bridges.clear();
        self.children.clear();
        self.specs.clear();
    }

    /// Every name `find_exporters` might need to locate a module by:
    /// declared module-visible symbols plus the export/export_ok lists.
    /// Variables and fields are skipped — file-local, not queryable
    /// across files.
    fn indexable_names(analysis: &FileAnalysis) -> Vec<String> {
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for sym in &analysis.symbols {
            if matches!(
                sym.kind,
                SymKind::Sub | SymKind::Method | SymKind::Package | SymKind::Class
                    | SymKind::Module | SymKind::HashKeyDef | SymKind::Handler,
            ) {
                names.insert(sym.name.clone());
            }
        }
        names.extend(analysis.export.iter().cloned());
        names.extend(analysis.export_ok.iter().cloned());
        names.into_iter().collect()
    }

    /// The bridge classes an analysis' plugin namespaces declare, deduped.
    fn bridge_classes(analysis: &FileAnalysis) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ns in &analysis.plugin_namespaces {
            for crate::file_analysis::Bridge::Class(c) in &ns.bridges {
                seen.insert(c.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Every primary a specialization in the analysis names — the values of
    /// `specializes`, deduped.
    fn spec_primaries(analysis: &FileAnalysis) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for primary in analysis.specializes.values() {
            seen.insert(primary.clone());
        }
        seen.into_iter().collect()
    }

    /// Every parent class/role any package in the analysis records —
    /// the values of `package_parents`, deduped. `use parent`/`use
    /// base`/`@ISA`/`class :isa`/`:does`/`with` all land here, so the
    /// `children` map covers inheritance and role composition alike.
    fn parent_classes(analysis: &FileAnalysis) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for parents in analysis.package_parents.values() {
            for p in parents {
                seen.insert(p.clone());
            }
        }
        seen.into_iter().collect()
    }
}

/// Async LSP handlers read from `cache` (zero I/O). The background resolver
/// thread populates the cache by parsing `.pm` files in-process.
/// The pack registration TOKEN: the (possibly stripped) arc to register
/// plus the whole-analysis halves — feed, specialization edges, projected
/// surface — all extracted BEFORE the strip. Fields are PRIVATE and the
/// struct is minted ONLY by the choke points in this module
/// (`prepare_pack_parts` = the reads-whole-before-evict strip, `whole` =
/// a deliberate whole-copy door, `from_warm_stub` = a persisted token
/// rehydrated). Holding one is the compile-time proof that a resident
/// `FileAnalysis` reached registration through one of those seams — a new
/// caller cannot hand `register_symbols_inner` a loose whole arc.
pub(crate) struct PackRegistrationParts {
    arc: Arc<FileAnalysis>,
    feed: Vec<(String, bool)>,
    specs: Vec<(String, String)>,
    surface: crate::surface::Surface,
}

impl PackRegistrationParts {
    /// The arc registration stores (read for persistence — `include_closure`
    /// — and for stub encoding).
    pub(crate) fn arc(&self) -> &Arc<FileAnalysis> {
        &self.arc
    }
    pub(crate) fn feed(&self) -> &[(String, bool)] {
        &self.feed
    }
    pub(crate) fn specs(&self) -> &[(String, String)] {
        &self.specs
    }
    pub(crate) fn surface(&self) -> &crate::surface::Surface {
        &self.surface
    }

    /// A whole-copy token minted from an already-`Arc`'d analysis: the feed
    /// reads the whole `symbols`, the surface projects from the whole bag.
    /// The deliberate whole-copy front door (`register_symbols`) — bounded,
    /// tripwire-counted at its call sites.
    pub(crate) fn whole(arc: Arc<FileAnalysis>) -> Self {
        let (feed, specs) = ModuleIndex::prepare_pack_feed(&arc);
        let surface = crate::surface::Surface::project(&arc);
        PackRegistrationParts { arc, feed, specs, surface }
    }

    /// Rehydrate a token from a warm stub — the persisted form of a prior
    /// `prepare_pack_parts` output (`encode_stub` was fed exactly these
    /// halves). The proof-of-strip is the persistence itself: a stub only
    /// exists because a fully-stripped copy was written.
    pub(crate) fn from_warm_stub(stub: crate::module_cache::WarmStub) -> Self {
        PackRegistrationParts {
            arc: Arc::new(stub.skeleton),
            feed: stub.feed,
            specs: stub.specs,
            surface: stub.surface,
        }
    }

    /// Record this file's span-free surface (the freshness write half).
    /// Separate from registration so the deferred-writer path can record
    /// pre-COMMIT (session-local) while the residency half waits for the
    /// commit; the sync front doors record then register in sequence.
    pub(crate) fn record_surface(
        &self,
        idx: &ModuleIndex,
        path: &std::path::Path,
    ) -> crate::surface::SurfaceVerdict {
        idx.record_surface_value(path, self.surface.clone())
    }
}

/// The workspace registration TOKEN — the Perl twin of
/// `PackRegistrationParts`. Same private-field / choke-point-mint discipline:
/// minted only by `prepare_workspace_parts` (strip) in this module.
pub(crate) struct WorkspaceRegistrationParts {
    arc: Arc<FileAnalysis>,
    module_name: Option<String>,
    surface: crate::surface::Surface,
}

impl WorkspaceRegistrationParts {
    pub(crate) fn arc(&self) -> &Arc<FileAnalysis> {
        &self.arc
    }

    /// See `PackRegistrationParts::record_surface`.
    pub(crate) fn record_surface(
        &self,
        idx: &ModuleIndex,
        path: &std::path::Path,
    ) -> crate::surface::SurfaceVerdict {
        idx.record_surface_value(path, self.surface.clone())
    }
}

/// Who is recording a surface. While a doc is OPEN, cross-file consumers
/// read its BUFFER analysis (query priority: open docs shadow the indexed
/// disk copy), so the freshness baseline must track the buffer: a
/// `Background` write (bulk indexer, watcher tick, save re-register) for an
/// open path describes a disk state consumers cannot see and is SUPPRESSED
/// — otherwise an edit reverting the buffer to the disk state reads
/// Unchanged against the wrong baseline and skips the consumer refresh.
/// `did_close` reconciles: consumers flip back to the disk copy, so the
/// close path re-records it (and refreshes whoever the flip dirtied).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceWrite {
    /// The open-doc editor path — owns the record while the doc is open.
    OpenDoc,
    /// Everything else (indexers, watcher, warm lanes) — yields to an open
    /// doc's record, wins otherwise.
    Background,
}

/// The freshness gate's answer: the surface verdict plus, on `Changed`, the
/// transitive dirty consumer set. Returned by `ModuleIndex::record_and_dirty`
/// (and by `register_workspace_resident`, which routes through it) so a
/// caller that records a surface always holds the consumer answer from the
/// same path.
pub struct SurfaceDirty {
    /// Rides the answer for callers that gate on FirstSeen vs Unchanged vs
    /// Changed; today's consumers act only on `dirty` (empty ⇒ nothing to do).
    #[allow(dead_code)]
    pub verdict: crate::surface::SurfaceVerdict,
    pub dirty: std::collections::HashSet<std::path::PathBuf>,
}

pub struct ModuleIndex {
    cache: Arc<DashMap<String, Option<Arc<CachedModule>>>>,
    /// See `ModuleEdgeIndexes` — names + bridges + children reverse maps.
    edges: Arc<ModuleEdgeIndexes>,
    /// Modules imported (literally or via SyntheticUse) by ANY
    /// workspace file, entrypoint scripts included. Powers the
    /// entrypoint-scan helper lint's "does anything load M" question.
    /// Fed by `register_workspace_module` only — the workspace scan
    /// re-runs every startup, so no warm-rebuild feed is needed.
    loaded_modules: Arc<DashMap<String, ()>>,
    /// Primary package names of workspace-registered files. The lint
    /// fires only for WORKSPACE plugin modules (in-project plugins you
    /// forgot to load); installed CPAN plugins keep the generous
    /// "downloaded = intended" resolution.
    workspace_modules: Arc<DashMap<String, ()>>,
    /// Loader-config shapes projected at registration: load-name →
    /// (contributor, shape) pairs from each file's `PluginLoad` facts.
    /// Projected HERE because lite entrypoints are PACKAGELESS — they
    /// never enter the cache, so enrichment can't reach their bags;
    /// the config value is a literal, so its shape is final at the
    /// contributor's own build. Fed by register_workspace_module
    /// (before the packageless early-return) AND insert_cache.
    loader_config_shapes: Arc<DashMap<String, Vec<(String, InferredTypeOwned)>>>,
    /// Modules loaded from cache with an old extract_version.
    /// Eligible for priority re-resolution when requested.
    stale_modules: Arc<DashMap<String, ()>>,
    /// Perl builtins hover docs, name → rendered markdown. Hydrated
    /// from SQLite by the resolver thread at startup (parsed from
    /// `perlfunc.pod` on first cold-cache miss). Empty until the
    /// resolver has run its warmup path.
    builtins: Arc<DashMap<String, String>>,
    /// Known module names from @INC scan. Name → path. No exports until resolved.
    available_modules: Arc<DashMap<String, std::path::PathBuf>>,
    queue: Arc<ResolveQueue>,
    resolved: Arc<ResolveNotify>,
    workspace_root: Arc<WorkspaceRootChannel>,
    /// Per-language sub-indexes (`"cpp"`, `"python"`, …) — kept SEPARATE
    /// (own cache, own `modules-{lang}.db`) so names never comingle across
    /// languages. The Perl index is the hub; query routing picks the right
    /// one by the queried file's language. Generic: any pack language.
    pack_indexes: Arc<DashMap<String, Arc<ModuleIndex>>>,
    /// Canonical paths of currently-open docs whose surface record the
    /// open-doc path owns (`SurfaceWrite` — background writes yield).
    /// Marked by the backend on didOpen, cleared + reconciled on didClose.
    /// Perl hub only today: pack languages have no open-doc surface
    /// recorder yet, so guarding their background writes would freeze
    /// records staleward.
    open_doc_paths: Arc<DashMap<std::path::PathBuf, ()>>,
    /// ALL cross-file candidates per name (not just the single winner in
    /// `cache`) — pack languages only. C linkage is globally flat, so two
    /// unrelated files can each define `class Box`; `cache` picks one
    /// deterministic winner, but a query from file F wants the candidate F can
    /// actually SEE (its `#include` closure). `get_cached_scoped` ranks these by
    /// reachability. `docs/adr/macro-handling.md`, "the include-closure lie".
    all_defs: Arc<DashMap<String, Vec<Arc<CachedModule>>>>,
    /// Every pack file registered, keyed by canonical path — including files
    /// that declare NOTHING registrable (a header-only `#include` shim). The
    /// name-keyed views can't reach those, but whole-project sweeps
    /// (`for_each_cached_file`) must.
    all_files: Arc<DashMap<std::path::PathBuf, Arc<CachedModule>>>,
    /// The freshness engine (`docs/adr/storage-engine.md`):
    /// per-file span-free surface records + the reverse-dependency index.
    /// Fed at registration (whole copy, pre-strip) and on open-doc
    /// rebuilds; `dirty_consumers` names who must re-enrich after a
    /// surface CHANGE, and an Unchanged verdict is the early-cutoff.
    freshness: Arc<crate::surface::FreshnessIndex>,
    /// The enrichment overlay (R4): derived enriched copies keyed by the
    /// surface fingerprints of the file + its providers. Bounded FIFO —
    /// `enriched_order` is the eviction queue.
    /// `None` payload = a DECLINED build (byte-cap giant / cycle-tainted)
    /// at this key: repeat queries skip the deep-copy entirely until a
    /// provider change moves the key.
    enriched: Arc<DashMap<std::path::PathBuf, (u64, Option<Arc<FileAnalysis>>, usize)>>,
    /// Monotonic per-path registration generation — the ABA-proof identity
    /// token `enrichment_key` hashes (an Arc pointer can be freed and its
    /// address reused; a counter can't run backwards). Bumped by every
    /// registration front door.
    registration_gen: Arc<DashMap<std::path::PathBuf, u64>>,
    gen_counter: Arc<std::sync::atomic::AtomicU64>,
    /// The witness seams' fallback-on-miss enriched retries only pay off
    /// when the process lives long enough to amortize the overlay (each
    /// miss is a whole-analysis deep copy + enrich). Off by default; the
    /// SERVER enables it at initialize. One-shot CLI query modes leave it
    /// off — the bisected cost was 2x warm-gold wall for answers no
    /// one-shot invocation reuses. (`--check`/`--dump-package` consume
    /// `enriched_snapshot` directly and are unaffected by this gate.)
    long_lived: Arc<std::sync::atomic::AtomicBool>,
    enriched_order: Arc<std::sync::Mutex<std::collections::VecDeque<std::path::PathBuf>>>,
    /// The linkage-visible (name, declares-a-Class) pairs each file
    /// registered — the exact inverse list `unregister_file` walks AND the
    /// class-rank source for the cache-slot tie-break. Recorded at
    /// registration (pre-strip) because the resident copy's `symbols` may be
    /// evicted, and rehydration after an edit persists would fetch the NEW
    /// generation's names.
    registered_names: Arc<DashMap<std::path::PathBuf, Vec<(String, bool)>>>,
    /// Slice-2 rehydration store. Pack sub-indexes get theirs at
    /// construction (keyed to `modules-{lang}.db`); the Perl hub gets its
    /// own in `set_workspace_root` (keyed to `modules.db` — workspace
    /// copies are refs/bag-evicted once persisted). A type query reaching
    /// into an evicted file rehydrates the exact persisted bag through this
    /// LRU (`bag_present`). See `docs/adr/memory-slice-2-lru.md`.
    bag_cache: Arc<std::sync::RwLock<Option<Arc<crate::pack_bag_cache::PackBagCache>>>>,
    /// The SIBLING tier's rehydration store, for copies this index does not
    /// own. Sweeps mint `CachedModule`s from FileStore entries and ask
    /// whatever index the query routed to — a cpp query's workspace sweep
    /// hands PERL paths to the cpp sub-index, whose own loader (keyed to
    /// `modules-{lang}.db`) can never serve them. `attach_pack_index`
    /// shares the hub's `bag_cache` cell here so a foreign path routes to
    /// its owner instead of degrading to the stripped resident. The hub's
    /// converse route (a pack path asked of the hub) walks `pack_indexes`.
    foreign_bag_cache: std::sync::RwLock<
        Option<Arc<std::sync::RwLock<Option<Arc<crate::pack_bag_cache::PackBagCache>>>>>,
    >,
    /// Read-connection opener for the relational ref index
    /// (`docs/adr/relational-ref-index.md`) — set once per index onto the
    /// per-language DB (`modules.db` for the Perl hub, `modules-{lang}.db`
    /// for pack sub-indexes). Opened per retrieval (WAL readers are cheap
    /// and `rusqlite::Connection` isn't `Sync`); `None` (tests, no cache
    /// dir) contributes no candidates and the resident sweep still covers.
    ref_rows_opener:
        std::sync::RwLock<Option<Arc<dyn Fn() -> Option<rusqlite::Connection> + Send + Sync>>>,
    /// The retained read connection the opener fills lazily — one per index,
    /// so the statement cache amortizes across queries (a heatmap projects
    /// references once per symbol; per-call opens would re-prepare every
    /// statement). WAL readers see each write txn that committed before
    /// their own read txn begins, so retaining it never serves stale rows.
    /// Paired with the DB file's inode at open: `--clear-cache` UNLINKS the
    /// file, and an fd pinning the dead inode would serve frozen rows
    /// forever — an inode change (or missing file) drops the conn so the
    /// next query reopens the recreated DB.
    ref_rows_conn: std::sync::Mutex<Option<(rusqlite::Connection, u64)>>,
}

impl ModuleIndex {
    pub fn new(client: Client, on_diagnostics_refresh: impl Fn() + Send + Sync + 'static) -> Self {
        let cache: Arc<DashMap<String, Option<Arc<CachedModule>>>> = Arc::new(DashMap::new());
        let edges = Arc::new(ModuleEdgeIndexes::new());
        let stale_modules: Arc<DashMap<String, ()>> = Arc::new(DashMap::new());
        let available_modules: Arc<DashMap<String, std::path::PathBuf>> = Arc::new(DashMap::new());
        let builtins: Arc<DashMap<String, String>> = Arc::new(DashMap::new());
        let queue = Arc::new(ResolveQueue {
            priority: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            condvar: Condvar::new(),
        });
        let resolved = Arc::new(ResolveNotify {
            mu: Mutex::new(()),
            cv: Condvar::new(),
        });
        let workspace_root = Arc::new(WorkspaceRootChannel {
            root: Mutex::new(None),
            condvar: Condvar::new(),
        });

        let refresh_clone = Arc::new(on_diagnostics_refresh);
        let long_lived = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let bag_cache: Arc<
            std::sync::RwLock<Option<Arc<crate::pack_bag_cache::PackBagCache>>>,
        > = Arc::new(std::sync::RwLock::new(None));
        // Hoisted before the spawn so the resolver thread stamps @INC
        // generations on the SAME maps the enrichment key reads.
        let registration_gen: Arc<DashMap<std::path::PathBuf, u64>> = Arc::new(DashMap::new());
        let gen_counter = Arc::new(std::sync::atomic::AtomicU64::new(1));

        module_resolver::spawn_resolver(
            Arc::clone(&cache),
            Arc::clone(&edges),
            Arc::clone(&stale_modules),
            Arc::clone(&available_modules),
            Arc::clone(&builtins),
            Arc::clone(&queue),
            Arc::clone(&resolved),
            Arc::clone(&workspace_root),
            client,
            Box::new(move || refresh_clone()),
            Arc::clone(&long_lived),
            Arc::clone(&bag_cache),
            Arc::clone(&registration_gen),
            Arc::clone(&gen_counter),
        );

        ModuleIndex {
            cache,
            edges,
            loaded_modules: Arc::new(DashMap::new()),
            pack_indexes: Arc::new(DashMap::new()),
            open_doc_paths: Arc::new(DashMap::new()),
            all_defs: Arc::new(DashMap::new()),
            all_files: Arc::new(DashMap::new()),
            registered_names: Arc::new(DashMap::new()),
            freshness: Arc::new(crate::surface::FreshnessIndex::default()),
            enriched: Arc::new(DashMap::new()),
            registration_gen,
            gen_counter,
            long_lived,
            enriched_order: Arc::new(std::sync::Mutex::new(Default::default())),
            bag_cache,
            foreign_bag_cache: std::sync::RwLock::new(None),
            ref_rows_opener: std::sync::RwLock::new(None),
            ref_rows_conn: std::sync::Mutex::new(None),
            workspace_modules: Arc::new(DashMap::new()),
            loader_config_shapes: Arc::new(DashMap::new()),
            stale_modules,
            available_modules,
            builtins,
            queue,
            resolved,
            workspace_root,
        }
    }

    /// Hover markdown for a Perl builtin (e.g. `push`, `scalar`).
    /// Returns `None` for unknown names or before the resolver has
    /// hydrated the index from SQLite.
    pub fn builtin_doc(&self, name: &str) -> Option<String> {
        self.builtins.get(name).map(|e| e.clone())
    }

    /// Notify the resolver thread of the workspace root (from LSP initialize).
    pub fn set_workspace_root(&self, root: Option<&str>) {
        let mut guard = self.workspace_root.root.lock().unwrap();
        if root.is_none() {
            log::warn!("No workspace root from client; using global module cache");
        }
        *guard = Some(root.map(String::from));
        self.workspace_root.condvar.notify_one();
        drop(guard);
        // The hub's relational-ref-index reader: the SAME cache key the
        // resolver thread writes under (both spell it as this root string),
        // so retrieval and shred always address one DB.
        let key = root.map(String::from);
        {
            let key = key.clone();
            self.set_ref_rows_opener(Arc::new(move || {
                crate::module_cache::open_cache_db_readonly(key.as_deref(), "perl")
            }));
        }
        // The hub's rehydration LRU: Perl workspace copies are refs/bag-
        // evicted once persisted; queries that need the whole analysis
        // rehydrate through this, same as the pack sub-indexes. Fixed
        // 128 MiB cap (Perl analyses are 10-100x smaller than cpp ones).
        let loader = move |path: &std::path::Path| {
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
            crate::module_cache::open_and_load_diag(key.as_deref(), "perl", &spellings)
        };
        self.set_bag_cache(Arc::new(crate::pack_bag_cache::PackBagCache::new(
            128 * 1024 * 1024,
            loader,
        )));
    }

    /// Get the workspace root URI if set.
    pub fn workspace_root(&self) -> Option<String> {
        self.workspace_root.root.lock().ok()
            .and_then(|guard| guard.as_ref().and_then(|opt| opt.clone()))
    }

    /// Request background resolution for a module. Non-blocking.
    /// Stale modules (old extract version) are queued with priority.
    pub fn request_resolve(&self, module_name: &str) {
        let is_stale = self.stale_modules.contains_key(module_name);
        if self.cache.contains_key(module_name) && !is_stale {
            return; // fresh and cached
        }
        if is_stale {
            let mut priority = self.queue.priority.lock().unwrap();
            if !priority.contains(&module_name.to_string()) {
                priority.push(module_name.to_string());
            }
        } else {
            let mut pending = self.queue.pending.lock().unwrap();
            pending.push(module_name.to_string());
        }
        self.queue.condvar.notify_one();
    }

    /// Return the cached CachedModule for a module name. Never does I/O.
    pub fn get_cached(&self, module_name: &str) -> Option<Arc<CachedModule>> {
        self.cache.get(module_name).and_then(|entry| entry.clone())
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
            if let Some(cands) = self.all_defs.get(module_name) {
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
        for entry in self.all_defs.iter() {
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
            let Some(cached) = self.get_cached(&module) else { continue };
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
            use crate::file_analysis::CrossFileLookup;
            if self.whole_present(cached).sub_info_view(name).is_some() {
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
        self.cache
            .get(module_name)
            .and_then(|entry| entry.as_ref().map(|m| m.path.clone()))
    }

    /// Return cached parent classes for a module's primary package.
    pub fn parents_cached(&self, module_name: &str) -> Vec<String> {
        let cached = match self.get_cached(module_name) {
            Some(c) => c,
            None => return Vec::new(),
        };
        primary_package_parents(&cached.analysis, module_name)
    }

    /// Iterate all cached modules. Callback receives (module_name, CachedModule).
    pub fn for_each_cached<F: FnMut(&str, &Arc<CachedModule>)>(&self, mut f: F) {
        for entry in self.cache.iter() {
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
        for entry in self.cache.iter() {
            if entry.value().is_some() {
                let name = entry.key();
                if name.to_lowercase().starts_with(&prefix_lower) && seen.insert(name.clone()) {
                    results.push((name.clone(), true));
                }
            }
        }

        // Tier 2: @INC scan (name only, no analysis yet)
        for entry in self.available_modules.iter() {
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
        use crate::file_analysis::CrossFileLookup;
        let modules = self.edges.names.get(func_name)?;
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
                self.get_cached(m)
                    .map(|c| c.analysis.export.iter().any(|e| e == func_name)
                        || c.analysis.export_ok.iter().any(|e| e == func_name))
                    .unwrap_or(false)
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
        match self.edges.names.get(name) {
            Some(modules) => {
                let mut result = modules.clone();
                result.sort();
                result.dedup();
                result
            }
            None => Vec::new(),
        }
    }

    /// Find the module that declares method `name` *attributed to class*
    /// `class` in a file whose own module name differs (cross-package
    /// typeglob install). Returns the registration key for a follow-up
    /// `get_cached`. The reverse index (keyed by symbol name) scopes the
    /// scan; the per-module `has_sub_in_package` filter pins the package.
    /// `None` when no such cross-package symbol exists — callers fall
    /// back to the class's own module / bridges.
    pub fn module_declaring_method_in_package(
        &self,
        name: &str,
        class: &str,
    ) -> Option<String> {
        use crate::file_analysis::CrossFileLookup;
        self.modules_with_symbol(name)
            .into_iter()
            .find(|mod_name| {
                self.get_cached(mod_name)
                    .map(|c| self.whole_present(&c).has_sub_in_package(name, class))
                    .unwrap_or(false)
            })
    }

    /// Create a minimal ModuleIndex for CLI mode (no resolver thread, no @INC scan).
    pub fn new_for_cli() -> Self {
        // A real (headless) resolver thread: one-shot CLI sessions used
        // to carry NO resolver, so they could never resolve a module the
        // editor hadn't already cached — framework-implied imports
        // (DefaultHelpers) stayed invisible to every CLI probe and the
        // gold harness. The thread blocks until `set_workspace_root`
        // fires in `cli_full_startup`.
        let cache: Arc<DashMap<String, Option<Arc<CachedModule>>>> = Arc::new(DashMap::new());
        let edges = Arc::new(ModuleEdgeIndexes::new());
        let stale_modules: Arc<DashMap<String, ()>> = Arc::new(DashMap::new());
        let available_modules: Arc<DashMap<String, std::path::PathBuf>> = Arc::new(DashMap::new());
        let builtins: Arc<DashMap<String, String>> = Arc::new(DashMap::new());
        let queue = Arc::new(ResolveQueue {
            priority: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            condvar: Condvar::new(),
        });
        let resolved = Arc::new(ResolveNotify {
            mu: Mutex::new(()),
            cv: Condvar::new(),
        });
        let workspace_root = Arc::new(WorkspaceRootChannel {
            root: Mutex::new(None),
            condvar: Condvar::new(),
        });

        let registration_gen: Arc<DashMap<std::path::PathBuf, u64>> = Arc::new(DashMap::new());
        let gen_counter = Arc::new(std::sync::atomic::AtomicU64::new(1));
        module_resolver::spawn_test_resolver(
            Arc::clone(&cache),
            Arc::clone(&edges),
            Arc::clone(&stale_modules),
            Arc::clone(&available_modules),
            Arc::clone(&queue),
            Arc::clone(&resolved),
            Arc::clone(&workspace_root),
            Arc::clone(&registration_gen),
            Arc::clone(&gen_counter),
        );

        ModuleIndex {
            cache,
            edges,
            loaded_modules: Arc::new(DashMap::new()),
            pack_indexes: Arc::new(DashMap::new()),
            open_doc_paths: Arc::new(DashMap::new()),
            all_defs: Arc::new(DashMap::new()),
            all_files: Arc::new(DashMap::new()),
            registered_names: Arc::new(DashMap::new()),
            freshness: Arc::new(crate::surface::FreshnessIndex::default()),
            enriched: Arc::new(DashMap::new()),
            registration_gen,
            gen_counter,
            long_lived: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            enriched_order: Arc::new(std::sync::Mutex::new(Default::default())),
            bag_cache: Arc::new(std::sync::RwLock::new(None)),
            foreign_bag_cache: std::sync::RwLock::new(None),
            ref_rows_opener: std::sync::RwLock::new(None),
            ref_rows_conn: std::sync::Mutex::new(None),
            workspace_modules: Arc::new(DashMap::new()),
            loader_config_shapes: Arc::new(DashMap::new()),
            stale_modules,
            available_modules,
            builtins,
            queue,
            resolved,
            workspace_root,
        }
    }

    /// Mark this process LONG-LIVED (the server): the witness seams'
    /// enriched retries turn on (the overlay amortizes them; one-shot CLI
    /// modes never recoup the deep-copies — bisected at 2x warm-harness
    /// wall), and the resolver strips warm-loaded @INC copies (their
    /// rehydration cost amortizes the same way).
    pub fn mark_long_lived(&self) {
        self.long_lived
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// `PERL_LSP_LONG_LIVED=1` forces the long-lived behaviors in one-shot
    /// CLI processes — the harness lane that keeps the server-only paths
    /// (enriched retries, warm @INC strip) under a regression net.
    pub fn is_long_lived(&self) -> bool {
        self.long_lived.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn mark_long_lived_from_env(&self) {
        if std::env::var("PERL_LSP_LONG_LIVED").as_deref() == Ok("1") {
            self.mark_long_lived();
        }
    }

    // ---- Test-only methods ----

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let cache: Arc<DashMap<String, Option<Arc<CachedModule>>>> = Arc::new(DashMap::new());
        let edges = Arc::new(ModuleEdgeIndexes::new());
        let stale_modules: Arc<DashMap<String, ()>> = Arc::new(DashMap::new());
        let available_modules: Arc<DashMap<String, std::path::PathBuf>> = Arc::new(DashMap::new());
        let builtins: Arc<DashMap<String, String>> = Arc::new(DashMap::new());
        let queue = Arc::new(ResolveQueue {
            priority: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            condvar: Condvar::new(),
        });
        let resolved = Arc::new(ResolveNotify {
            mu: Mutex::new(()),
            cv: Condvar::new(),
        });
        let workspace_root = Arc::new(WorkspaceRootChannel {
            root: Mutex::new(None),
            condvar: Condvar::new(),
        });

        let registration_gen: Arc<DashMap<std::path::PathBuf, u64>> = Arc::new(DashMap::new());
        let gen_counter = Arc::new(std::sync::atomic::AtomicU64::new(1));
        module_resolver::spawn_test_resolver(
            Arc::clone(&cache),
            Arc::clone(&edges),
            Arc::clone(&stale_modules),
            Arc::clone(&available_modules),
            Arc::clone(&queue),
            Arc::clone(&resolved),
            Arc::clone(&workspace_root),
            Arc::clone(&registration_gen),
            Arc::clone(&gen_counter),
        );

        let idx = ModuleIndex {
            cache,
            edges,
            loaded_modules: Arc::new(DashMap::new()),
            pack_indexes: Arc::new(DashMap::new()),
            open_doc_paths: Arc::new(DashMap::new()),
            all_defs: Arc::new(DashMap::new()),
            all_files: Arc::new(DashMap::new()),
            registered_names: Arc::new(DashMap::new()),
            freshness: Arc::new(crate::surface::FreshnessIndex::default()),
            enriched: Arc::new(DashMap::new()),
            registration_gen,
            gen_counter,
            long_lived: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            enriched_order: Arc::new(std::sync::Mutex::new(Default::default())),
            bag_cache: Arc::new(std::sync::RwLock::new(None)),
            foreign_bag_cache: std::sync::RwLock::new(None),
            ref_rows_opener: std::sync::RwLock::new(None),
            ref_rows_conn: std::sync::Mutex::new(None),
            workspace_modules: Arc::new(DashMap::new()),
            loader_config_shapes: Arc::new(DashMap::new()),
            stale_modules,
            available_modules,
            builtins,
            queue,
            resolved,
            workspace_root,
        };
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
        self.builtins.insert(name.to_string(), doc.to_string());
    }

    /// Direct access to the raw cache DashMap (for CLI warm_cache integration).
    pub fn cache_raw(&self) -> &DashMap<String, Option<Arc<CachedModule>>> {
        &self.cache
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
        for entry in self.cache.iter() {
            let Some(cached) = entry.value() else { continue };
            if cached.analysis.gated_emissions.is_empty() {
                continue;
            }
            // Rehydrate the whole view (the resident copy may be
            // symbols-evicted) before appending the synthesized accessors.
            let whole = crate::file_analysis::CrossFileLookup::whole_present(self, cached);
            let mut copy = (*whole).clone();
            copy.materialize_gated_emissions(self);
            updates.push((entry.key().clone(), cached.path.clone(), Arc::new(copy)));
        }
        for (name, path, analysis) in updates {
            let cm = Arc::new(CachedModule::new(path.clone(), analysis));
            self.all_files.insert(path, cm.clone());
            self.cache.insert(name, Some(cm));
        }
    }

    pub fn insert_cache(&self, module_name: &str, cached: Option<Arc<CachedModule>>) {
        if let Some(ref m) = cached {
            self.edges.feed(module_name, &m.analysis);
            self.record_loader_shapes(module_name, &m.analysis);
            // A CLI-resolved @INC provider: mint its generation so the
            // enrichment key reads a real token, and a re-resolve moves it.
            self.mint_import_gen(&m.path);
        }
        self.cache.insert(module_name.to_string(), cached);
    }

    /// Project each `PluginLoad` fact's config value into a stored
    /// shape under its load-name. The value is a literal in the
    /// contributor's file, so `expr_type_at_span` with no index is
    /// already final — this is a registration-time projection of
    /// local facts (the same tier as export names), not a cached
    /// cross-file resolution.
    fn record_loader_shapes(&self, contributor: &str, analysis: &FileAnalysis) {
        // re-registration: drop this contributor's old entries
        self.loader_config_shapes.retain(|_n, v| {
            v.retain(|(c, _)| c != contributor);
            !v.is_empty()
        });
        for f in &analysis.plugin_loads {
            let Some(span) = f.config_span else { continue };
            if let Some(t) = analysis.expr_type_at_span(span, None) {
                self.loader_config_shapes
                    .entry(f.name.clone())
                    .or_default()
                    .push((contributor.to_string(), t));
            }
        }
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
        for f in &analysis.plugin_loads {
            self.loaded_modules.insert(f.name.clone(), ());
        }
        self.record_loader_shapes(&path.display().to_string(), analysis);
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
        let Some(module_name) = first_package_name(&analysis) else {
            return sd;
        };
        self.workspace_modules.insert(module_name.clone(), ());
        self.edges.purge_module(&module_name);
        self.edges.feed(&module_name, &analysis);
        self.cache.insert(module_name, Some(cached));
        sd
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
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let verdict =
            self.record_surface_write(&canon, crate::surface::Surface::project(fa), write);
        let dirty = match verdict {
            crate::surface::SurfaceVerdict::Changed => self.dirty_consumers(&canon),
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
    ) -> crate::surface::SurfaceVerdict {
        self.record_surface_value(path, crate::surface::Surface::project(fa))
    }

    /// Record an ALREADY-projected surface (the warm-stub path decodes the
    /// persisted projection; the fresh worker projects once and shares it
    /// with the stub encoder).
    pub fn record_surface_value(
        &self,
        path: &std::path::Path,
        surface: crate::surface::Surface,
    ) -> crate::surface::SurfaceVerdict {
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
        surface: crate::surface::Surface,
        write: SurfaceWrite,
    ) -> crate::surface::SurfaceVerdict {
        if write == SurfaceWrite::Background && self.open_doc_paths.contains_key(canon) {
            return crate::surface::SurfaceVerdict::Unchanged;
        }
        self.freshness.record(canon, surface)
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
        let whole = crate::file_analysis::CrossFileLookup::whole_present(self, &cm);
        Some(self.record_and_dirty(&canon, &whole, SurfaceWrite::Background))
    }

    /// Every registration bumps this — the enrichment key's freshness
    /// token for the file itself and for providers whose facts aren't
    /// surface-covered (a body edit re-registers with a new generation,
    /// where a surface fingerprint deliberately stands still).
    pub(crate) fn bump_registration_gen(&self, path: &std::path::Path) {
        mint_registration_gen(&self.registration_gen, &self.gen_counter, path);
    }

    /// Mint a generation for an @INC provider the CLI resolved directly
    /// (main.rs's `insert_cache`) — the resolver THREAD mints through the
    /// free `mint_registration_gen` on its own Arcs.
    pub(crate) fn mint_import_gen(&self, path: &std::path::Path) {
        mint_registration_gen(&self.registration_gen, &self.gen_counter, path);
    }

    /// Stamp a generation for every name-keyed cache entry that lacks one —
    /// the warm scan loads @INC blobs straight into the cache without a
    /// registration front door. See `stamp_missing_import_gens`.
    pub(crate) fn stamp_import_generations(&self) {
        stamp_missing_import_gens(&self.cache, &self.registration_gen, &self.gen_counter);
    }

    fn registration_gen_of(&self, path: &std::path::Path) -> u64 {
        self.registration_gen.get(path).map(|g| *g).unwrap_or(0)
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
        self.freshness.remove(&canon);
        // Belt over braces: if the caller's raw spelling was the recorded
        // key (registration itself fell back), remove that too.
        if canon != path {
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
            return None;
        }
        let _entered = Entered(cached.path.clone());
        let declined_before = DECLINED.with(|c| c.get());
        // BYTE-bounded first (enriched copies are whole analyses — 64 of a
        // tree's biggest generated modules would quietly re-pin the
        // gigabytes the eviction axes stripped), entry-bounded second.
        const ENRICHED_CAP: usize = 64;
        const ENRICHED_BYTE_CAP: usize = 128 * 1024 * 1024;
        let path = &cached.path;
        let key = self.enrichment_key(cached);
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
                return hit;
            }
        }
        let whole = crate::file_analysis::CrossFileLookup::whole_present(self, cached);
        // Deep copy via serde — enrichment must never write through the
        // shared Arc (the R4 rule the overlay exists to enforce).
        let mut copy: FileAnalysis = bincode::serialize(&*whole)
            .ok()
            .and_then(|bin| bincode::deserialize(&bin).ok())?;
        copy.after_deserialize();
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
            if tainted || bytes > ENRICHED_BYTE_CAP { None } else { Some(arc) };
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
                && (order.len() > ENRICHED_CAP || total_bytes(&order) > ENRICHED_BYTE_CAP)
            {
                if let Some(evictee) = order.pop_front() {
                    self.enriched.remove(&evictee);
                }
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
    fn enrichment_key(&self, cached: &Arc<CachedModule>) -> u64 {
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
                match self.get_cached(&dep) {
                    None => 0u8.hash(&mut h),
                    Some(cm) => {
                        // Generation ALWAYS on the key, fingerprint too when
                        // recorded: enrichment's ctx-ful passes bake
                        // BODY-dependent provider facts the span-free
                        // fingerprint deliberately ignores, so a provider
                        // re-registration must move every consumer's key
                        // (over-invalidation, never staleness).
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
                                for parents in cm.analysis.package_parents.values() {
                                    next.extend(parents.iter().cloned());
                                }
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

    /// The name/edge feed half of workspace registration, run on the WHOLE
    /// analysis BEFORE any strip (a stripped copy's `symbols` is empty and
    /// would blind the feeds). Returns the module name the residency half
    /// keys the cache slot on.
    pub(crate) fn workspace_feed_prestrip(&self, fa: &FileAnalysis) -> Option<String> {
        let module_name = first_package_name(fa);
        if let Some(ref name) = module_name {
            self.workspace_modules.insert(name.clone(), ());
            self.edges.purge_module(name);
            self.edges.feed(name, fa);
        }
        module_name
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
        let WorkspaceRegistrationParts { arc, module_name, surface: _ } = parts;
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.bump_registration_gen(&path);
        let cached = Arc::new(CachedModule::new(path, arc));
        self.all_files.insert(cached.path.clone(), cached.clone());
        if let Some(name) = module_name {
            self.cache.insert(name, Some(cached));
        }
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
        strip_bag: bool,
        strip_rows: bool,
    ) -> Arc<FileAnalysis> {
        let parts = self.prepare_workspace_parts(fa, strip_bag, strip_rows);
        parts.record_surface(self, &path);
        let arc = Arc::clone(parts.arc());
        self.register_workspace_residency(path, parts);
        arc
    }

    /// Remove a deleted workspace file's registrations — the path-keyed
    /// entry plus its name-keyed cache row and edges (a dead file must not
    /// stay a retrieval candidate or a phantom module).
    pub fn unregister_workspace_path(&self, path: &std::path::Path) {
        self.remove_surface(path);
        self.all_files.remove(path);
        let name = self.cache.iter().find_map(|entry| {
            entry
                .value()
                .as_ref()
                .filter(|cm| cm.path == path)
                .map(|_| entry.key().clone())
        });
        if let Some(name) = name {
            self.edges.purge_module(&name);
            self.cache.remove(&name);
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
            *g = Some(Arc::clone(&self.bag_cache));
        }
        self.pack_indexes.insert(lang.to_string(), idx);
    }

    /// Install this pack sub-index's Slice-2 bag-rehydration LRU before it is
    /// `Arc`-wrapped and registered. Consuming builder so the field is set once
    /// on the owned value (the index is shared immutably thereafter).
    pub fn with_bag_cache(
        self,
        cache: Arc<crate::pack_bag_cache::PackBagCache>,
    ) -> Self {
        self.set_bag_cache(cache);
        self
    }

    /// Post-`Arc` variant for the hub, set alongside the workspace root.
    /// LAST root wins — a re-rooted session must not keep rehydrating from
    /// the first root's DB while the writers moved to the new one.
    pub fn set_bag_cache(&self, cache: Arc<crate::pack_bag_cache::PackBagCache>) {
        if let Ok(mut g) = self.bag_cache.write() {
            *g = Some(cache);
        }
    }

    fn bag_cache_ref(&self) -> Option<Arc<crate::pack_bag_cache::PackBagCache>> {
        self.bag_cache.read().ok().and_then(|g| g.clone())
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
    /// `refs_present`: the miss policy and LRU selection must never diverge
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
    fn rehydrate_or_resident(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        let mut stage = "no bag cache installed on this index".to_string();
        if let Some(bc) = self.bag_cache_ref() {
            match bc.bag_for_diag(&cached.path) {
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
        if let Some(fa) = self.rehydrate_foreign(&cached.path) {
            return fa;
        }

        REHYDRATION_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        log::error!(
            "rehydration miss for evicted copy {:?} ({stage}) — serving stripped \
             resident (references/types for this file are quietly incomplete this \
             session)",
            cached.path
        );
        if crate::module_resolver::strict_residency() {
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
    fn rehydrate_foreign(&self, path: &std::path::Path) -> Option<Arc<FileAnalysis>> {
        // Sub-index → the hub's cell (shared at `attach_pack_index`).
        let hub_cell = self.foreign_bag_cache.read().ok().and_then(|g| g.clone());
        if let Some(cell) = hub_cell {
            let hub_cache = cell.read().ok().and_then(|g| g.clone());
            if let Some(bc) = hub_cache {
                if let Some(fa) = bc.bag_for(path) {
                    return Some(fa);
                }
            }
        }
        // Hub → the pack sibling that registered the path.
        for entry in self.pack_indexes.iter() {
            let sub = entry.value();
            if sub.all_files.contains_key(path) {
                if let Some(bc) = sub.bag_cache_ref() {
                    if let Some(fa) = bc.bag_for(path) {
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
    fn module_defines_class(&self, m: &CachedModule, name: &str) -> bool {
        if let Some(rec) = self.registered_names.get(&m.path) {
            return rec.iter().any(|(n, is_class)| n == name && *is_class);
        }
        m.analysis
            .symbols
            .iter()
            .any(|s| matches!(s.kind, SymKind::Class) && s.name == name)
    }

    /// Run `f` against this index's retained read connection to the
    /// relational row store, opening (or re-opening, if the DB file was
    /// unlinked/recreated) through the installed opener. `None` when no
    /// opener is set (tests, no cache dir) or the open fails. One retained
    /// connection per index so the statement cache amortizes across queries.
    fn with_rows_conn<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> R) -> Option<R> {
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
    pub fn sym_search(&self, query: &str) -> Vec<crate::module_cache::SymRowHit> {
        self.with_rows_conn(|conn| crate::module_cache::sym_rows_matching(conn, query))
            .unwrap_or_default()
    }

    /// The unused-exports view over THIS index's row store — exported syms
    /// with zero cross-file reference rows (`docs/adr/relational-ref-index.md`).
    /// `None` when the row store is unavailable (opener absent, cold cache);
    /// the caller degrades to the references projection.
    pub fn unused_exported_syms(&self) -> Option<Vec<crate::module_cache::DeadExportRow>> {
        self.with_rows_conn(crate::module_cache::unused_exported_syms)
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
                crate::module_cache::names_with_ref_rows(conn),
                crate::module_cache::paths_with_ref_rows(conn),
            )
        })
    }

    /// The sub-index for `lang`, if this distribution indexes it.
    pub fn pack_index(&self, lang: &str) -> Option<Arc<ModuleIndex>> {
        self.pack_indexes.get(lang).map(|e| e.value().clone())
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
        let parts = PackRegistrationParts::whole(analysis);
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
            fa.specializes.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        (feed, specs)
    }

    /// The Perl-workspace twin of `prepare_pack_parts`: the name feed and
    /// the surface project from the WHOLE analysis, THEN the requested axes
    /// evict, then the arc is minted. `register_workspace_stripping` and the
    /// fresh workspace worker both route here so the reads-whole-before-
    /// evict ordering has one speller per tier.
    pub(crate) fn prepare_workspace_parts(
        &self,
        mut fa: FileAnalysis,
        strip_bag: bool,
        strip_rows: bool,
    ) -> WorkspaceRegistrationParts {
        let module_name = self.workspace_feed_prestrip(&fa);
        let surface = crate::surface::Surface::project(&fa);
        fa.evict_axes(strip_bag, strip_rows);
        WorkspaceRegistrationParts { arc: Arc::new(fa), module_name, surface }
    }

    /// The ONE speller of the pack strip ordering: feed + specs + surface
    /// project from the WHOLE analysis, THEN the requested axes evict, then
    /// the arc is minted. Every pack registration that strips (bulk warm,
    /// fresh worker, edit swap) routes here so the "reads-whole-before-
    /// evict" invariant can't drift between separately-spelled copies —
    /// and the stub encoder gets exactly the halves registration used.
    pub(crate) fn prepare_pack_parts(
        mut fa: FileAnalysis,
        strip_bag: bool,
        strip_rows: bool,
    ) -> PackRegistrationParts {
        let (feed, specs) = Self::prepare_pack_feed(&fa);
        let surface = crate::surface::Surface::project(&fa);
        fa.evict_axes(strip_bag, strip_rows);
        PackRegistrationParts { arc: Arc::new(fa), feed, specs, surface }
    }

    pub fn register_symbols_stripping(
        &self,
        path: std::path::PathBuf,
        fa: FileAnalysis,
        strip_bag: bool,
        strip_rows: bool,
    ) -> Arc<FileAnalysis> {
        let parts = Self::prepare_pack_parts(fa, strip_bag, strip_rows);
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
                let mut v = self.all_defs.entry(sym_name.clone()).or_default();
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
            match self.cache.entry(sym_name.clone()) {
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
            let mut v = self.edges.specs.entry(primary.clone()).or_default();
            if !v.iter().any(|m| m == spec) {
                v.push(spec.clone());
            }
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
        // (`evict_axes` leaves it) and rides the warm-stub skeleton, so the arc
        // carries it on the fresh, warm, and whole paths alike.
        for (child, parents) in &analysis.package_parents {
            for parent in parents {
                let mut v = self.edges.children.entry(parent.clone()).or_default();
                if !v.iter().any(|m| m == child) {
                    v.push(child.clone());
                }
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
            if let Some(mut v) = self.all_defs.get_mut(name) {
                v.retain(|c| c.path != canon);
            }
            self.all_defs.remove_if(name, |_, v| v.is_empty());
            let survivor = self
                .all_defs
                .get(name)
                .and_then(|v| best_candidate(&v.iter().collect::<Vec<_>>(), name, &|m, n| {
                    self.module_defines_class(m, n)
                }));
            // Only touch the cache slot if the departing file held it.
            let held = self
                .cache
                .get(name)
                .map(|e| matches!(e.value(), Some(c) if c.path == canon))
                .unwrap_or(false);
            if held {
                match survivor {
                    Some(cand) => {
                        self.cache.insert(name.clone(), Some(cand));
                    }
                    None => {
                        self.cache.remove(name);
                    }
                }
            }
            if !self.all_defs.contains_key(name) {
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
        self.edges.clear();
        for entry in self.cache.iter() {
            if let Some(ref cached) = *entry.value() {
                self.edges.feed(entry.key(), &cached.analysis);
            }
        }
    }

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

    pub fn modules_bridging_to(&self, class_name: &str) -> Vec<String> {
        match self.edges.bridges.get(class_name) {
            Some(mods) => {
                let mut result = mods.clone();
                result.sort();
                result.dedup();
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
        match self.edges.children.get(class_name) {
            Some(mods) => {
                let mut result = mods.clone();
                result.sort();
                result.dedup();
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
                let Some(cached) = self.get_cached(&module_name) else { continue };
                // A module can hold several packages; only the ones
                // actually listing `current` as a parent are children.
                for (pkg, parents) in &cached.analysis.package_parents {
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
        mut visit: impl FnMut(&str, &Arc<CachedModule>, &crate::file_analysis::Symbol),
    ) {
        use crate::file_analysis::CrossFileLookup;
        for mod_name in self.modules_bridging_to(class_name) {
            let Some(cached) = self.get_cached(&mod_name) else { continue };
            // Entities index into `symbols`, which may be evicted on the
            // resident copy — resolve them against the whole view (same
            // generation: the LRU is invalidated on every rewrite).
            let whole = self.whole_present(&cached);
            for ns in &whole.plugin_namespaces {
                let bridges_class = ns.bridges.iter().any(|b|
                    matches!(b, crate::file_analysis::Bridge::Class(c) if c == class_name));
                if !bridges_class { continue; }
                // Namespace membership IS the filter — if this namespace
                // bridges to `class_name`, every entity it owns is
                // visible from `class_name`. No `sym.package` gate: the
                // plugin picks ONE canonical home package and the
                // namespace's bridges control visibility, so no
                // per-bridge Method fan-out is needed.
                for sym_id in &ns.entities {
                    let idx = sym_id.0 as usize;
                    let Some(sym) = whole.symbols.get(idx) else { continue };
                    visit(&mod_name, &cached, sym);
                }
            }
        }
    }

    /// Block until `module_name` appears in the cache, or timeout.
    /// (Used by tests and the one-shot CLI import resolution.)
    #[doc(hidden)]
    pub fn wait_resolved(&self, module_name: &str, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut guard = self.resolved.mu.lock().unwrap();
        loop {
            if self.cache.contains_key(module_name) {
                return true;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (g, result) = self.resolved.cv.wait_timeout(guard, remaining).unwrap();
            guard = g;
            if result.timed_out() && !self.cache.contains_key(module_name) {
                return false;
            }
        }
    }

    /// Get cached module synchronously. WARNING: Does blocking I/O. Only for tests.
    #[cfg(test)]
    pub fn get_cached_blocking(&self, module_name: &str) -> Option<Arc<CachedModule>> {
        if let Some(entry) = self.cache.get(module_name) {
            return entry.clone();
        }
        let inc_paths = module_resolver::discover_inc_paths();
        let mut parser = module_resolver::create_parser();
        let result = module_resolver::resolve_and_parse(&inc_paths, module_name, &mut parser);
        self.cache.insert(module_name.to_string(), result.clone());
        result
    }

    #[cfg(test)]
    fn inc_paths(&self) -> Vec<PathBuf> {
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

    fn refs_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        if !cached.analysis.refs_are_evicted() {
            return cached.analysis.clone();
        }
        self.rehydrate_or_resident(cached)
    }

    fn whole_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        if cached.analysis.is_fully_resident() {
            return cached.analysis.clone();
        }
        self.rehydrate_or_resident(cached)
    }

    fn ref_candidate_paths(&self, keys: &[String]) -> Vec<std::path::PathBuf> {
        self.with_rows_conn(|conn| {
            crate::module_cache::ref_candidate_files(conn, keys)
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
    }

    fn ref_indexed_paths(&self) -> std::collections::HashSet<std::path::PathBuf> {
        self.with_rows_conn(|conn| {
            crate::module_cache::paths_with_ref_rows(conn)
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
        self.cache.iter().find_map(|entry| {
            entry
                .value()
                .as_ref()
                .filter(|cm| cm.path == path)
                .cloned()
        })
    }

    fn enriched_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        if !self.long_lived.load(std::sync::atomic::Ordering::Relaxed) {
            return self.bag_present(cached);
        }
        self.enriched_snapshot(cached)
            .unwrap_or_else(|| self.bag_present(cached))
    }
    fn bag_present(&self, cached: &Arc<CachedModule>) -> Arc<FileAnalysis> {
        // Never-evicted copy (open docs, degraded files kept whole): a cheap
        // Arc bump, no I/O.
        if !cached.analysis.bag_is_evicted() {
            return cached.analysis.clone();
        }
        self.rehydrate_or_resident(cached)
    }

    fn parents_cached(&self, module_name: &str) -> Vec<String> {
        self.parents_cached(module_name)
    }

    fn module_path_cached(&self, module_name: &str) -> Option<std::path::PathBuf> {
        self.module_path_cached(module_name)
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
        match self.all_defs.get(name) {
            Some(cands) if !cands.is_empty() => cands.clone(),
            // Perl hub: `all_defs` is pack-only, fall back to the winner.
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
        for entry in self.cache.iter() {
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
        f: &mut dyn FnMut(&str, &Arc<CachedModule>, &crate::file_analysis::Symbol),
    ) {
        self.for_each_entity_bridged_to(class_name, f)
    }

    fn direct_children_of(&self, class: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for module in self.modules_with_parent(class) {
            let Some(cached) = self.get_cached(&module) else { continue };
            for (pkg, parents) in &cached.analysis.package_parents {
                if parents.iter().any(|p| p == class) {
                    out.push((pkg.clone(), module.clone()));
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
            .edges
            .specs
            .get(primary)
            .map(|v| v.clone())
            .unwrap_or_default();
        for module in modules {
            let Some(cached) = self.get_cached(&module) else { continue };
            for (spec, prim) in &cached.analysis.specializes {
                if prim == primary {
                    out.push((spec.clone(), module.clone()));
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn for_each_loader_shape(&self, f: &mut dyn FnMut(&str, &crate::file_analysis::InferredType)) {
        for entry in self.loader_config_shapes.iter() {
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
}

// ---- Module-level helpers ----

/// Return the parents of the primary package of a module, preferring the
/// package with the same name as `module_name` and falling back to the
/// single-package case if only one package exists in the file.
/// First `package X;` declaration in a FileAnalysis. Used to decide
/// under what name a workspace file should be registered in the
/// module index so cross-file method resolution (which keys on
/// package name, e.g. "Users" for `->to('Users#list')`) can find it.
/// Returns `None` for scripts with no explicit package declaration.
pub fn first_package_name(analysis: &FileAnalysis) -> Option<String> {
    for sym in &analysis.symbols {
        if matches!(sym.kind, SymKind::Package | SymKind::Class) {
            return Some(sym.name.clone());
        }
    }
    None
}

pub fn primary_package_parents(analysis: &FileAnalysis, module_name: &str) -> Vec<String> {
    analysis
        .package_parents
        .get(module_name)
        .cloned()
        .unwrap_or_default()
}


#[cfg(test)]
#[path = "module_index_tests.rs"]
mod tests;
