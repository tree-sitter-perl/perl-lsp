//! Cross-file capability: `CachedModule`, `SubInfo`, the `CrossFileLookup`
//! trait and `ScopedLookup`, plus the global path-intern table.

use super::*;

// ---- Cross-file lookup capability ----

/// A module in the cache — its filesystem path plus the full FileAnalysis of
/// its source. Shared by reference-count so async handlers don't deep-copy.
#[derive(Debug)]
pub struct CachedModule {
    pub path: std::path::PathBuf,
    pub analysis: std::sync::Arc<FileAnalysis>,
}

impl CachedModule {
    pub fn new(path: std::path::PathBuf, analysis: std::sync::Arc<FileAnalysis>) -> Self {
        CachedModule { path, analysis }
    }

    // Symbol/bag readers deliberately do NOT live on CachedModule: an index
    // copy may be evicted on any axis, so consumers mint the sibling on a
    // present view (`idx.whole_present(&cached).sub_info_view(..)` etc.) —
    // a convenience wrapper here would compile everywhere and silently
    // answer empty at scale.
}

/// A view into a module's metadata for a named sub/method.
///
/// Composed of a primary symbol plus any additional symbols with the same
/// name (for rw accessor setter overloads).
impl FileAnalysis {
    /// The `SubInfo` view over THIS analysis — mint it from a bag-present
    /// copy (`idx.bag_present(&cached)`) when the bag-backed accessors will
    /// be read; an evicted index copy answers those with `None`.
    pub fn sub_info_view(&self, name: &str) -> Option<SubInfo<'_>> {
        // Prefer the first matching Sub/Method symbol. Builder may emit several
        // when rw accessors exist (getter + setter); overloads are collected as
        // additional symbols with the same name.
        let mut syms = self
            .symbols
            .iter()
            .filter(|s| s.name == name && matches!(s.kind, SymKind::Sub | SymKind::Method));
        let primary = syms.next()?;
        let overloads: Vec<&Symbol> = syms.collect();

        // Keys are owned by `Sub { package: primary.package, name }` — the
        // sub's hash keys live under the same package as the sub itself.
        let hash_keys: Vec<String> = self
            .hash_key_defs_for_owner(&HashKeyOwner::Sub {
                package: primary.package.clone(),
                name: name.to_string(),
            })
            .iter()
            .map(|s| s.name.clone())
            .collect();

        Some(SubInfo { analysis: self, primary, overloads, hash_keys })
    }

    /// Locate a package-global variable declaration (`our $x` / `our @arr`
    /// / `our %h`) by its sigil-bearing name within `package`. Powers
    /// cross-file goto-def for a fully-qualified read (`$Foo::Bar::x`).
    /// `name` includes the sigil (`$x`, `@arr`, `%h`) to match how variable
    /// symbols are keyed.
    pub fn package_var_def_line(&self, name: &str, package: &str) -> Option<u32> {
        self.symbols
            .iter()
            .find(|s| {
                matches!(s.kind, SymKind::Variable | SymKind::Field)
                    && s.name == name
                    && s.package.as_deref() == Some(package)
            })
            .map(|s| s.span.start.row as u32)
    }

    /// True if a sub/method with this name is declared in this module
    /// *attributed to `package`* — not merely declared somewhere in the
    /// file. Cross-package typeglob installs
    /// (`*{'DateTime::'.$sub} = …` inside `package DateTime::PP`)
    /// synthesize a symbol whose `package` (DateTime) differs from the
    /// file's own module name (DateTime::PP), so a class-keyed method
    /// lookup must ask by package, not by module-name match.
    pub fn has_sub_in_package(&self, name: &str, package: &str) -> bool {
        self.symbols.iter().any(|s| {
            s.name == name
                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                && s.package.as_deref() == Some(package)
        })
    }

    /// Every package this file attributes a sub/method to — exactly the
    /// key `has_sub_in_package` tests against. Normally the file's own
    /// declared packages; a cross-package typeglob install
    /// (`*{'DateTime::x'} = …` inside `package DateTime::PP`) also
    /// attributes the synthesized sub to the TARGET package, which no
    /// `package` statement in this file names. The cross-file provider
    /// index is fed from this, so "which module declares `m` for class
    /// `C`" is a bucket read keyed by C instead of a scan over every
    /// module declaring a sub named `m`.
    pub fn provided_packages(&self) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut out = Vec::new();
        for s in self.symbols() {
            if !matches!(s.kind, SymKind::Sub | SymKind::Method) {
                continue;
            }
            if let Some(pkg) = s.package.as_deref() {
                if seen.insert(pkg) {
                    out.push(pkg.to_string());
                }
            }
        }
        out
    }

    /// Completion candidates for `use Module qw(|)` — this module's export
    /// surface, `@EXPORT` first (sort tier 10) then `@EXPORT_OK` (tier 20),
    /// deduped. Detail carries the resolved return type when known. The
    /// adapter projects these; the "still indexing" affordance for a
    /// not-yet-cached module is the adapter's (there's no entity to gather).
    pub fn import_list_candidates(&self) -> Vec<CompletionCandidate> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for name in &self.export {
            if seen.insert(name.clone()) {
                let detail = self
                    .sub_info_view(name)
                    .and_then(|s| s.return_type(None))
                    .map(|rt| format!("@EXPORT → {}", format_inferred_type(&rt)))
                    .or_else(|| Some("@EXPORT".to_string()));
                items.push(CompletionCandidate {
                    label: name.clone(),
                    kind: SymKind::Sub,
                    detail,
                    insert_text: None,
                    sort_priority: 10,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                });
            }
        }
        for name in &self.export_ok {
            if seen.insert(name.clone()) {
                let detail = self
                    .sub_info_view(name)
                    .and_then(|s| s.return_type(None))
                    .map(|rt| format!("→ {}", format_inferred_type(&rt)));
                items.push(CompletionCandidate {
                    label: name.clone(),
                    kind: SymKind::Sub,
                    detail,
                    insert_text: None,
                    sort_priority: 20,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                });
            }
        }
        items
    }
}

pub struct SubInfo<'a> {
    analysis: &'a FileAnalysis,
    primary: &'a Symbol,
    #[allow(dead_code)] // retained for the `param_counts` / `return_type_for_arity` API surface
    overloads: Vec<&'a Symbol>,
    hash_keys: Vec<String>,
}

impl<'a> SubInfo<'a> {
    pub fn def_line(&self) -> u32 {
        self.primary.span.start.row as u32
    }

    pub fn params(&self) -> &'a [ParamInfo] {
        match &self.primary.detail {
            SymbolDetail::Sub { params, .. } => params,
            _ => &[],
        }
    }

    pub fn is_method(&self) -> bool {
        if self.primary.kind == SymKind::Method {
            return true;
        }
        matches!(
            self.primary.detail,
            SymbolDetail::Sub { is_method: true, .. }
        )
    }

    /// Pass `module_index` so a return type produced by a cross-file method
    /// chain in the sub body resolves; `None` keeps it single-file.
    pub fn return_type(&self, module_index: Option<&dyn CrossFileLookup>) -> Option<InferredType> {
        match &self.primary.detail {
            SymbolDetail::Sub { .. } => {
                self.analysis.symbol_return_type_via_bag_ctx(self.primary.id, None, module_index)
            }
            _ => None,
        }
    }

    pub fn doc(&self) -> Option<&'a str> {
        match &self.primary.detail {
            SymbolDetail::Sub { doc, .. } => doc.as_deref(),
            _ => None,
        }
    }

    pub fn hash_keys(&self) -> &[String] {
        &self.hash_keys
    }

    /// Arity list covering the primary and overloads, in declaration order.
    #[allow(dead_code)] // public SubInfo accessor; consumed by tooling/future cross-file callers
    pub fn param_counts(&self) -> Vec<usize> {
        std::iter::once(self.primary)
            .chain(self.overloads.iter().copied())
            .map(|s| match &s.detail {
                SymbolDetail::Sub { params, .. } => params.len(),
                _ => 0,
            })
            .collect()
    }

    /// Return type for an overload with the given arity, if any matches.
    #[allow(dead_code)] // public SubInfo accessor; consumed by tooling/future cross-file callers
    pub fn return_type_for_arity(&self, arity: usize, module_index: Option<&dyn CrossFileLookup>) -> Option<InferredType> {
        for sym in std::iter::once(self.primary).chain(self.overloads.iter().copied()) {
            if let SymbolDetail::Sub { params, .. } = &sym.detail {
                if params.len() == arity {
                    return self.analysis.symbol_return_type_via_bag_ctx(sym.id, Some(arity), module_index);
                }
            }
        }
        None
    }

    /// SymbolId of the primary (first matching) sym.
    #[allow(dead_code)] // public SubInfo accessor; consumed by tooling/future cross-file callers
    pub fn primary_id(&self) -> SymbolId {
        self.primary.id
    }

    /// SymbolId of the overload whose param count matches `arity`,
    /// if any.
    #[allow(dead_code)] // public SubInfo accessor; consumed by tooling/future cross-file callers
    pub fn id_for_arity(&self, arity: usize) -> Option<SymbolId> {
        for sym in std::iter::once(self.primary).chain(self.overloads.iter().copied()) {
            if let SymbolDetail::Sub { params, .. } = &sym.detail {
                if params.len() == arity {
                    return Some(sym.id);
                }
            }
        }
        None
    }

    /// Inferred type for a param by name (if the analysis resolved one).
    /// Goes through the canonical bag-aware query so framework rules
    /// (Mojo `$self` etc.) apply consistently across every consumer.
    pub fn param_inferred_type(&self, param_name: &str) -> Option<InferredType> {
        self.analysis
            .inferred_type_via_bag(param_name, self.primary.span.end)
    }
}

/// What query-time cross-file resolution needs from the dependency
/// index. `ModuleIndex` implements this; `file_analysis`/`witnesses`
/// depend on the capability, not the index — the inversion that breaks
/// the FA ↔ index cycle (dependency inversion; the index implements it).
///
/// Object-safe by design: a `&dyn CrossFileLookup` rides
/// `witnesses::BagContext`, hence the `&mut dyn FnMut` callback params.
/// Process-global path interner (`docs/adr/relational-ref-index.md`,
/// residency phases): closure paths repeat across nearly every file in a
/// tree (abseil shares ~90% of its header universe per TU), so resident
/// copies share ONE allocation per unique path instead of one per
/// (file × path). Serialized form stays a plain string sequence — blob
/// layout unchanged, interning happens on the way in.
pub mod path_intern {
    use std::sync::{Arc, OnceLock};

    // ---- Global path-id table (the ClosureList substrate) ----
    //
    // Closures at scale are the largest resident bucket as 16-byte
    // `Arc<str>` pointer vecs (chromium: 2.8 GB / 41% of the floor). A
    // sorted `Arc<[u32]>` over one process-global id table is 4× smaller
    // per entry and turns the hot membership gate into id-compare binary
    // search. IDs are process-local (never serialized — the blob keeps
    // `Vec<String>`), so the table only ever grows within a session.

    use std::collections::HashMap;
    use std::sync::RwLock;

    struct PathIds {
        by_str: HashMap<Arc<str>, u32>,
        by_id: Vec<Arc<str>>,
    }

    static IDS: OnceLock<RwLock<PathIds>> = OnceLock::new();

    fn ids() -> &'static RwLock<PathIds> {
        IDS.get_or_init(|| {
            RwLock::new(PathIds { by_str: HashMap::new(), by_id: Vec::new() })
        })
    }

    /// The id for `s`, minting one if unseen.
    fn id_intern(s: &str) -> u32 {
        {
            let g = ids().read().unwrap();
            if let Some(&id) = g.by_str.get(s) {
                return id;
            }
        }
        let mut g = ids().write().unwrap();
        if let Some(&id) = g.by_str.get(s) {
            return id;
        }
        let a: Arc<str> = Arc::from(s);
        let id = g.by_id.len() as u32;
        g.by_id.push(a.clone());
        g.by_str.insert(a, id);
        id
    }

    /// The id for `s` ONLY if some closure already interned it — a miss
    /// means no closure can contain it (lookups must not grow the table).
    fn id_lookup(s: &str) -> Option<u32> {
        ids().read().unwrap().by_str.get(s).copied()
    }

    fn str_of(id: u32) -> Arc<str> {
        ids().read().unwrap().by_id[id as usize].clone()
    }

    /// Process-wide table cost (counted ONCE, not per file): unique paths
    /// and their string bytes across both the Arc pool and the id table.
    pub fn table_stats() -> (usize, usize) {
        let g = ids().read().unwrap();
        let bytes: usize = g
            .by_id
            .iter()
            .map(|a| a.len() + std::mem::size_of::<Arc<str>>() * 2 + 8)
            .sum();
        (g.by_id.len(), bytes)
    }

    /// The id for `s` if any closure has interned it — `None` means no
    /// closure anywhere contains it. The one-per-query half of the
    /// `contains_id` fast path.
    pub fn lookup_id(s: &str) -> Option<u32> {
        id_lookup(s)
    }

    /// A file's `#include` closure as sorted path-ids over the global
    /// table. Semantically a set of path strings; consumers ask membership
    /// (`contains`) or iterate the strings — the representation is private
    /// so it can keep shrinking (`docs/forks-resolved.md`, closure
    /// representation fork).
    #[derive(Debug, Clone, Default)]
    pub struct ClosureList(Arc<[u32]>);

    impl ClosureList {
        pub fn from_iter<'a>(items: impl Iterator<Item = &'a str>) -> Self {
            let mut v: Vec<u32> = items.map(id_intern).collect();
            v.sort_unstable();
            v.dedup();
            ClosureList(v.into())
        }

        pub fn contains(&self, s: &str) -> bool {
            match id_lookup(s) {
                Some(id) => self.0.binary_search(&id).is_ok(),
                None => false,
            }
        }

        /// Membership by pre-resolved id — hot loops (the backward walk's
        /// visibility gate runs once per candidate file) resolve the query
        /// string to an id ONCE via `lookup_id` and test lock-free here.
        pub fn contains_id(&self, id: u32) -> bool {
            self.0.binary_search(&id).is_ok()
        }

        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }

        /// The member paths as shared strings (save path, visibility sets).
        pub fn iter_strs(&self) -> impl Iterator<Item = Arc<str>> + '_ {
            self.0.iter().map(|&id| str_of(id))
        }

        /// Per-file resident bytes (the id array; the global table is
        /// counted once process-wide, not per file).
        pub fn heap_bytes(&self) -> usize {
            self.0.len() * std::mem::size_of::<u32>()
        }
    }

    impl serde::Serialize for ClosureList {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(self.iter_strs().map(|a| a.as_ref().to_owned()))
        }
    }

    impl<'de> serde::Deserialize<'de> for ClosureList {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let raw = <Vec<String> as serde::Deserialize>::deserialize(d)?;
            Ok(ClosureList::from_iter(raw.iter().map(|s| s.as_str())))
        }
    }
}

pub trait CrossFileLookup {
    /// Monotone validity counter for everything a cross-file walk reads —
    /// registrations, freshness records, cache-slot/loader-shape swaps.
    /// The resolution session memoizes consult ANSWERS against it: any
    /// index mutation moves the counter and the memo drops wholesale.
    /// Default `0` (an index that never mutates, or one whose consumers
    /// don't memoize) — a constant is sound because the memo only ever
    /// widens invalidation, never narrows it, and an immutable index has
    /// nothing to invalidate.
    fn resolution_epoch(&self) -> u64 {
        0
    }

    /// Is a MethodCall re-stamp owed for `path`, last stamped at
    /// `stamped_at`?
    ///
    /// The re-stamp is the dominant driver of cross-file bag rehydration, and
    /// most of it re-derives an answer the build already froze. What licenses
    /// skipping it is knowing that no provider of this file has moved since
    /// the last stamp — which is a question about the FLUSH, not about this
    /// file, and answering it by walking `providers(F)` here would resurrect
    /// the transitive closure the enrichment-key memo exists to contain.
    ///
    /// So the flush PUSHES: enqueuing a consumer stamps that consumer's mark,
    /// and this is an O(1) comparison against it. Every unknown — never
    /// stamped, no mark recorded, no index, a caller with no path — **fails
    /// open to today's behavior**, which is why the gate can land before the
    /// flush is the standing path and simply do nothing until it is.
    ///
    /// The gate is exactly as sound as the freshness edge coverage it rides
    /// on, and no more: a provider whose change never reaches
    /// `dirty_consumers` never marks anyone, and the skip would then be
    /// wrong. New and deleted files are covered because registration and
    /// removal already route through `record_and_dirty`.
    fn restamp_owed(&self, _path: &std::path::Path, _stamped_at: Option<u64>) -> bool {
        true
    }

    /// The monotone flush clock a stamp records itself against. Sessional and
    /// deliberately not persisted — see `FileAnalysis::stamped_at`.
    fn flush_epoch(&self) -> u64 {
        0
    }
    /// Does ANY file bridge plugin-synthesized entities onto `class`?
    ///
    /// The guard on every conclusion form that encodes "everything before the
    /// bridge arm answered None" — trusted absence and `Link`. The live ladder
    /// is local → primary → parents → bridges, so a baked `Value`/`ReturnOf`
    /// came from an arm that beats bridges in every world and needs no guard;
    /// the other two are exactly the set that could be contradicted by a
    /// bridged answer they never saw.
    ///
    /// Asked HERE rather than baked, and that is the whole point. A bake-time
    /// "no file bridges to C" is a negative GLOBAL fact stored in a map whose
    /// invalidation covers the derivation code and the file's own stamp —
    /// foreign registry state is covered by neither, by design. A newly indexed
    /// file that starts bridging to C would change nothing about the consumer,
    /// no re-bake would fire, and the stale map would durably skip the bridged
    /// answer. Asking the index at trust time is self-healing in both
    /// directions with no invalidation machinery at all.
    ///
    /// Defaults to `true` — the conservative answer. An index that cannot say
    /// makes its callers decode rather than trust, which is slow and never
    /// wrong.
    fn class_is_bridged_to(&self, _class: &str) -> bool {
        true
    }

    /// The surface fingerprint the freshness index currently records for
    /// `path`, if any.
    ///
    /// The per-provider half of a closedness certificate's validity key
    /// (`model/witnesses/closedness.rs`). `None` means the index cannot vouch
    /// for this path — never recorded, or an impl with no freshness engine —
    /// and the caller declines to certify. Defaulted for the same reason
    /// `conclusions_for` is: an index without one simply never certifies,
    /// which is the fail-open direction.
    fn surface_fingerprint_of(&self, _path: &std::path::Path) -> Option<u64> {
        None
    }

    /// This class's cached closedness certificate, if one was minted and is
    /// still resident. `None` means "mint it if you want one" — never "this
    /// class is not closed".
    fn closedness_certificate(
        &self,
        _class: &str,
    ) -> Option<std::sync::Arc<crate::model::witnesses::ClosednessCertificate>> {
        None
    }

    /// Remember a freshly minted certificate. Defaulted to a no-op: an impl
    /// with nowhere to put it re-mints, which costs a walk the consult had
    /// already paid for.
    fn store_closedness_certificate(
        &self,
        _class: &str,
        _cert: std::sync::Arc<crate::model::witnesses::ClosednessCertificate>,
    ) {
    }

    /// This file's baked conclusions, if the store has them at the reader's
    /// pinned generation.
    ///
    /// `None` means NOT BAKED — the caller decodes, exactly as before this
    /// layer existed. It emphatically does not mean "no answer": that is what
    /// a key being absent from a returned map means, and conflating the two
    /// turns an unbaked file into a file that concludes nothing.
    ///
    /// Defaulted so an index with no store (tests, pack sub-indexes) is
    /// unaffected and simply keeps decoding.
    fn conclusions_for(
        &self,
        _path: &std::path::Path,
    ) -> Option<std::sync::Arc<crate::model::witnesses::ConclusionMap>> {
        None
    }
    fn get_cached(&self, module_name: &str) -> Option<std::sync::Arc<CachedModule>>;
    /// `get_cached` scoped to a querying file's VISIBILITY set (its own path +
    /// its `#include` closure) — see `ModuleIndex::get_cached_scoped`. Default:
    /// ignore the scope (identical to `get_cached`), so non-index impls and
    /// languages with no include model are unaffected. `ScopedLookup` and the
    /// pack `ModuleIndex` override it to rank same-name candidates by reachability.
    fn get_cached_scoped(
        &self,
        module_name: &str,
        _visible: &std::collections::HashSet<String>,
    ) -> Option<std::sync::Arc<CachedModule>> {
        self.get_cached(module_name)
    }
    /// EVERY cached file defining `name` (the pack index's full candidate
    /// table), not just the one-winner `get_cached` view — for consumers that
    /// must weigh candidates themselves (definition-over-prototype). Default:
    /// the winner alone.
    fn def_candidates(&self, name: &str) -> Vec<std::sync::Arc<CachedModule>> {
        self.get_cached(name).into_iter().collect()
    }
    /// `def_candidates` AS SEEN from the querying scope — the forward-
    /// resolution face of the candidate table. Raw indexes pass the full
    /// relation through; `ScopedLookup` narrows a CLOSURE-scoped origin
    /// (pack: flat linkage, where same-named candidates in unrelated TUs are
    /// different entities) to candidates connected to the asker, degrading
    /// to the scope-ranked winner. A scope with no closure axis (Perl, whose
    /// package relation is name → MANY files by design) passes the relation
    /// through — Perl's own visibility tier plugs into `CandidateSet::scoped`
    /// when it lands, never into a second mechanism here.
    fn visible_def_candidates(&self, name: &str) -> Vec<std::sync::Arc<CachedModule>> {
        self.def_candidates(name)
    }
    /// Among the files declaring `pkg`, the one that DEFINES sub/method
    /// `member` — package-attributed first (a reopened package's sub lives
    /// under `pkg`), then any-package (cross-package typeglob installs,
    /// bridged-helper modules). Candidates arrive path-ordered, so ties
    /// break to the smallest path and repeat runs are byte-identical.
    /// `None` when no candidate defines it — callers keep their own
    /// fallbacks. This is THE symbol-disambiguation rule for a name that
    /// maps to a SET of files; consumers resolving "where does `pkg`'s
    /// `member` live" route here, never through the one-winner
    /// `get_cached`.
    fn candidate_defining_sub(
        &self,
        pkg: &str,
        member: &str,
    ) -> Option<std::sync::Arc<CachedModule>> {
        self.candidate_defining_sub_in_package(pkg, pkg, member)
    }
    /// The two-key form of `candidate_defining_sub`: candidates come from
    /// `module_key`'s registration (a bridging/typeglob-installing module
    /// whose NAME differs from the class), package attribution is tested
    /// against `pkg`. `candidate_defining_sub` is the common
    /// module-IS-package spelling.
    fn candidate_defining_sub_in_package(
        &self,
        module_key: &str,
        pkg: &str,
        member: &str,
    ) -> Option<std::sync::Arc<CachedModule>> {
        let cands = self.visible_def_candidates(module_key);
        cands
            .iter()
            .find(|c| self.symbols_present(c).has_sub_in_package(member, pkg))
            .or_else(|| {
                cands
                    .iter()
                    .find(|c| self.symbols_present(c).sub_info_view(member).is_some())
            })
            .cloned()
    }
    /// A cached module's analysis with its witness bag GUARANTEED present.
    /// Slice 2 evicts the bag from resident pack-index copies; every TYPE
    /// query that reads a foreign file's bag (the `PackageSymbol` / `SlotType`
    /// / `TypeName` cross-file chases, `def_candidates` return-type folds,
    /// cross-file field types) routes through here so the exact persisted bag
    /// rehydrates on demand. Default (Perl hub, tests, non-pack impls): a cheap
    /// `Arc` bump — those copies are never evicted. The pack `ModuleIndex`
    /// overrides it to rehydrate from its `PackBagCache` when the bag is
    /// evicted. See `docs/adr/memory-slice-2-lru.md`.
    fn bag_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        cached.analysis.clone()
    }
    /// A cached WORKSPACE module's analysis with cross-file ENRICHMENT
    /// applied (`docs/adr/storage-engine.md`, the always-enriched
    /// tier): imported return types propagated, synthetic hash-key defs
    /// injected — derived through the overlay, never in-place. Consumers
    /// are FALLBACK-ON-MISS: call this only after the raw bag answered
    /// None (a miss pays one deep-copy+enrich, then the overlay caches by
    /// dep-surface fingerprint). Default: the bag-present view — impls
    /// without an overlay answer unenriched, never wrongly.
    fn enriched_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        self.bag_present(cached)
    }
    /// Can `enriched_present` ever hand back a view DISTINCT from the bag?
    /// The witness seams' fallback-on-miss arms ask this BEFORE calling it:
    /// when the answer is no (one-shot CLI — the overlay is long-lived-only),
    /// the retry is a guaranteed no-op, and skipping it saves a redundant
    /// per-escalation fetch — measured at 706k calls / 5.3% of a cold
    /// `--check` wall on a script-heavy corpus — plus the re-chase hazard
    /// when the LRU evicts between the two fetches. Default `true`: an
    /// implementor that overrides `enriched_present` without this probe
    /// keeps its retries.
    fn serves_enriched(&self) -> bool {
        true
    }
    /// A cached module's analysis whole on EVERY evictable axis — bag, refs,
    /// AND symbols present. Consumers that read more than one axis from the
    /// same copy (the diagnostics sweep, the `refs_to` matcher, `sub_info`
    /// readers, heatmap/parity enumeration) route here: a single-axis view
    /// returns the resident copy when its own axis survived but a sibling
    /// was evicted (the shred-failure degradation path), silently dropping
    /// the other axis's answers.
    fn whole_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        cached.analysis.clone()
    }
    /// The SYMBOLS-axis view: the resident copy whenever its symbols
    /// survived eviction, rehydrated otherwise. The @INC strip is
    /// bag-only, so the import tier — exactly the ancestor set the MRO
    /// existence walks hammer — answers with a cheap `Arc` bump instead of
    /// a whole-blob decode; the workspace tier (symbols-evicted after
    /// persist) rehydrates exactly as `whole_present` would.
    ///
    /// CONTRACT: the returned view's symbols axis is POPULATED — an empty
    /// scan result means the file genuinely declares no matching symbol,
    /// never "the axis was evicted". A reader that could answer
    /// absence-by-eviction as absence-in-fact is the silent-wrong-goto-def
    /// failure mode; implementations must rehydrate, never degrade to the
    /// resident-or-empty copy. For existence/name scans that read `symbols`
    /// plus never-evicted lanes (scopes, packages, plugin) ONLY; a consumer
    /// that also reads the bag or refs takes `whole_present`.
    fn symbols_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        self.whole_present(cached)
    }
    /// A sweep-scoped cross-file consult verdict: "what did candidate `path`
    /// contribute to this (point-free) query". `None` = not remembered.
    /// The default remembers nothing — only an index under an open sweep
    /// (the CLI's whole-corpus diagnostics) serves these. The seam exists
    /// because the SESSION memo is thread-local and per-verb, while a batch
    /// sweep's repeats span files and rayon workers: without a shared tier,
    /// every file's build re-chases every (query, candidate) pair the sweep
    /// already settled — the measured n² on package-main corpora.
    fn sweep_consult_answer(
        &self,
        path: &std::path::Path,
        key: &crate::model::witnesses::ConsultVerdictKey,
    ) -> Option<std::sync::Arc<crate::model::witnesses::ReducedValue>> {
        let _ = (path, key);
        None
    }
    /// Remember a sweep-scoped verdict. No-op by default.
    fn remember_sweep_consult(
        &self,
        path: &std::path::Path,
        key: &crate::model::witnesses::ConsultVerdictKey,
        value: &crate::model::witnesses::ReducedValue,
    ) {
        let _ = (path, key, value);
    }
    /// May `cached`'s file declare a member named `name` attributed to
    /// package `class`? The rows-backed pre-filter for the ancestor walk's
    /// per-candidate existence probe (`docs/prompt-relational-iteration.md`):
    /// `false` licenses skipping the `symbols_present` rehydrate outright, so
    /// it requires POSITIVE evidence of absence — a store that covers the
    /// file and holds no matching sym row. Everything else — no store, file
    /// never shredded, a resident (unevicted) copy, post-shred plugin
    /// emissions — answers `true`, which is this default: an implementor
    /// that cannot prove absence inherits the decode, never the skip.
    fn candidate_may_declare(
        &self,
        cached: &std::sync::Arc<CachedModule>,
        name: &str,
        class: &str,
    ) -> bool {
        let _ = (cached, name, class);
        true
    }
    /// The registry-sweep sibling of `candidate_may_declare`: can `cached`'s
    /// BAG hold any witness for a class-keyed attachment named `name`
    /// (`PackageSymbol{class, name}` when `attributed`, `SlotType{.., key}`
    /// otherwise)? Rows half only — the chase-shape gates (declared
    /// parents, dynamic parents, app-surface membership, the unrowed
    /// residue) live at the call site, which owns the chase semantics.
    /// Same three-valued fail-open contract as `candidate_may_declare`;
    /// default `true` (an impl without a row store cannot speak).
    fn candidate_bag_may_answer(
        &self,
        cached: &std::sync::Arc<CachedModule>,
        name: &str,
        class: &str,
        attributed: bool,
    ) -> bool {
        let _ = (cached, name, class, attributed);
        true
    }
    /// The ROWS-axes view — refs AND symbols populated: the backward-walk
    /// matcher's axes (usage sites + declaration sites). The @INC strip is
    /// bag-only, so import-tier copies answer resident; the workspace strip
    /// (bag + refs + symbols) rehydrates.
    ///
    /// CONTRACT: the returned view's refs and symbols are POPULATED — an
    /// empty match result means the file genuinely holds no matching
    /// site, never "the axis was evicted". Absence-by-eviction here is
    /// `references` silently under-reporting with no error; implementations
    /// must rehydrate, never degrade to the resident-or-empty copy. The BAG
    /// is NOT promised: a consumer whose match needs query-time type
    /// inference — a name-matching ref whose verdict isn't baked
    /// (`Ref::match_verdict_baked`) — upgrades that file to `whole_present`.
    fn refs_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        self.whole_present(cached)
    }
    /// Every indexed file holding at least one ref row keyed by one of
    /// `keys` — the relational reverse index's candidate-file retrieval
    /// (`SELECT DISTINCT path … WHERE name_id IN keys`). The backward walk
    /// rehydrates these and runs the one matcher over them. Default: empty
    /// (impls without a row store contribute no candidates; the resident
    /// sweep still covers their files).
    fn ref_candidate_paths(&self, _keys: &[String]) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
    /// Every path this index has SHREDDED into the relational row store
    /// (the `files` table — the single "rows present" marker). A file in
    /// this set but ABSENT from `ref_candidate_paths(keys)` has no ref or
    /// sym row for those names, so — rows over-approximate references — it
    /// provably matches nothing and the backward walk can skip rehydrating
    /// it. Empty (default, or no row store) ⇒ no narrowing; the resident
    /// sweep whole-views every gate-passing file as before. `docs/adr/
    /// relational-ref-index.md`.
    fn ref_indexed_paths(&self) -> std::collections::HashSet<std::path::PathBuf> {
        std::collections::HashSet::new()
    }
    /// Path-keyed cached-module lookup — the retrieval above hands back
    /// paths; this maps them onto the resident registration (for the
    /// visibility gate + whole-copy rehydration). Default `None`.
    fn cached_by_path(
        &self,
        _path: &std::path::Path,
    ) -> Option<std::sync::Arc<CachedModule>> {
        None
    }
    /// Parent classes of `module_name` — the UNION over every file
    /// declaring the package (any file may push `@ISA` / `use parent`; the
    /// name-slot winner alone hides a losing file's edges). Winner-file
    /// parents come first (candidates are path-ordered), so the DFS MRO
    /// stays deterministic. The `packages` lane is never evicted, so the
    /// raw candidate analyses answer without rehydration.
    fn parents_cached(&self, module_name: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in self.visible_def_candidates(module_name) {
            for p in c.analysis.declared_parents(module_name) {
                if !out.iter().any(|x| x == p) {
                    out.push(p.clone());
                }
            }
        }
        out
    }
    fn modules_with_symbol(&self, name: &str) -> Vec<String>;
    fn find_exporters(&self, func_name: &str) -> Vec<String>;
    fn defining_module_cached(&self, entry: &str, name: &str) -> Option<std::sync::Arc<CachedModule>>;
    fn module_declaring_method_in_package(&self, name: &str, class: &str) -> Option<String>;
    /// The search-path roots this index resolved modules from — the
    /// process-wide `@INC`, most-preferred first, ALREADY CANONICAL
    /// (canonicalized once when the resolver published it, never per
    /// query: this is read on request paths). Shared, so the common origin
    /// — one with no `use lib` of its own — builds its axis with an Arc
    /// clone and no filesystem I/O at all. Default empty (an index with no
    /// module universe), which makes the search-path axis transparent
    /// rather than blind.
    fn inc_roots(&self) -> std::sync::Arc<Vec<std::path::PathBuf>> {
        std::sync::Arc::new(Vec::new())
    }
    /// Is `path` in this lookup's read-only DEPENDENCY tier? Tier
    /// attribution for the masked backward walk: a dependency site is
    /// visible to references but never rewritten by rename. The Perl hub's
    /// whole cache is dependency BY CONSTRUCTION (workspace Perl files live
    /// in the FileStore, so anything here came from `@INC`) — hence the
    /// default. A pack sub-index holds the workspace's own files too, so it
    /// overrides with membership in its registered dependency-root set
    /// (composer's vendor packages).
    fn is_dependency_path(&self, _path: &std::path::Path) -> bool {
        true
    }
    /// The workspace root, for resolving an origin's relative `use lib`
    /// entries — Perl resolves those against the process CWD, which for a
    /// language server is the project root.
    fn workspace_root_path(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// The on-disk path a module name resolves to (Perl module goto-def).
    /// Default `None` for impls without a path map.
    fn module_path_cached(&self, _module_name: &str) -> Option<std::path::PathBuf> {
        None
    }
    /// The querying file's visibility scope when this lookup is bound to one
    /// (`ScopedLookup`): its own canonical path + the visible set (self path ∪
    /// include closure, canonical strings). `None` for unscoped indexes.
    /// The backward reference gate mints a pack target's `def_paths` from this
    /// so def→uses matching runs under the SAME visibility forward resolution
    /// uses (`resolve::pack_def_paths`).
    fn visibility_scope(
        &self,
    ) -> Option<(&std::path::Path, &std::collections::HashSet<String>)> {
        None
    }
    /// Scope-less BY RULE (`VisibilityAxis::Flat` — a name-keyed pack):
    /// consumers that degrade a closure scope to agreement folds admit the
    /// FULL candidate table under it. `false` everywhere else, including
    /// `Transparent` ("no rule known yet") — an unwarmed host origin must
    /// not suddenly sweep the pack candidate table.
    fn flat_scope(&self) -> bool {
        false
    }
    /// The namespace THIS scope's origin means by the unqualified class
    /// `leaf` — its `use` row, its own declaration, or (a name-keyed pack's
    /// rule) its own namespace. `None` = the scope makes no claim, and every
    /// same-leaf gate built on it stands down. Only a use-map axis answers.
    fn pinned_namespace(&self, _leaf: &str) -> Option<String> {
        None
    }
    fn for_each_cached(&self, f: &mut dyn FnMut(&str, &std::sync::Arc<CachedModule>));
    /// Visit every distinct cached FILE exactly once. `for_each_cached` is
    /// keyed by NAME with one winner per key, so a pack file that loses every
    /// name tie (two fixtures both declaring `is_scope`) is invisible there —
    /// any whole-project sweep (find-references, macro variants, include
    /// reverse) must use this instead. Default: path-dedup over
    /// `for_each_cached` (correct for the Perl hub, whose module-name keys
    /// are unique per file); the pack index overrides with its complete
    /// per-file candidate table.
    fn for_each_cached_file(&self, f: &mut dyn FnMut(&std::sync::Arc<CachedModule>)) {
        let mut seen: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        self.for_each_cached(&mut |_n, cached| {
            if seen.insert(cached.path.clone()) {
                f(cached);
            }
        });
    }
    fn for_each_reexport_module(
        &self,
        start: Vec<String>,
        visit: &mut dyn FnMut(&std::sync::Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    );
    /// `visit` returns `Break` to stop the walk — and, more importantly, the
    /// per-candidate rehydrates behind it: entity resolution decodes each
    /// bridging module's symbols, so a first-match caller that cannot stop
    /// the iteration pays a decode per plugin file in the workspace for
    /// answers it already has (same shape as `for_each_reexport_module`).
    fn for_each_entity_bridged_to(
        &self,
        class_name: &str,
        f: &mut dyn FnMut(
            &str,
            &std::sync::Arc<CachedModule>,
            &Symbol,
        ) -> std::ops::ControlFlow<()>,
    );
    /// The bridged walk for a caller that wants one NAMED entity — the
    /// first-match-by-name consumers (the MRO walk's bridged arm, the
    /// opaque-return check, the registry's bridged consult). `name` is a
    /// pre-filter LICENSE, not a semantic change: the visitor still sees
    /// every entity of every candidate it reaches, but an implementor with
    /// a row store may skip candidates that provably declare nothing named
    /// `name` — killing the per-miss decode of every bridging module. The
    /// default ignores the hint (fail-open) and delegates.
    fn for_each_entity_bridged_to_named(
        &self,
        class_name: &str,
        name: &str,
        f: &mut dyn FnMut(
            &str,
            &std::sync::Arc<CachedModule>,
            &Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        let _ = name;
        self.for_each_entity_bridged_to(class_name, f)
    }
    /// Direct children/composers of `class` as (package, module) pairs
    /// — the `children_index` inverse, depth 1 (the graph walker
    /// supplies transitivity).
    fn direct_children_of(&self, class: &str) -> Vec<(String, String)>;
    /// Template specializations of `primary` as (spec, module) pairs — the
    /// cross-file half of the graph's `Specializes` family edge (the local
    /// half reads `FileAnalysis.pack.specializes`). Default: none (the Perl hub
    /// and language-less impls have no specialization index).
    fn direct_specializations_of(&self, _primary: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Registration-time loader-config shapes: every (load_name, shape)
    /// projected from `PluginLoad` facts across the workspace —
    /// INCLUDING packageless entrypoint scripts, which never enter the
    /// module cache.
    fn for_each_loader_shape(&self, f: &mut dyn FnMut(&str, &InferredType));
    /// Loadable module names matching `prefix` for completion, as
    /// (name, is_resolved) — resolved modules have full analysis, the rest
    /// are @INC-scanned availability. Defaults empty so lookups that have
    /// no module universe stay honest without stubbing.
    fn complete_module_names(&self, _prefix: &str) -> Vec<(String, bool)> {
        Vec::new()
    }
    /// Completion-GATHERING mirror of `get_cached_scoped`: every registered
    /// name starting with `prefix` that has a definition candidate inside
    /// `visible` (canonical paths — the querying file's `#include` closure).
    /// No global fallback — a file is never offered symbols from headers it
    /// doesn't include. Defaults empty (the Perl hub has no closure model).
    fn visible_defs_with_prefix(
        &self,
        _prefix: &str,
        _visible: &std::collections::HashSet<String>,
    ) -> Vec<(String, std::sync::Arc<CachedModule>)> {
        Vec::new()
    }
}

/// A `CrossFileLookup` decorator scoped to ONE querying file's include-closure
/// visibility. Every cross-file resolution routed through it ranks same-name
/// candidates by reachability (`get_cached` → `inner.get_cached_scoped`), so a
/// file resolves `class Box` to the `Box` it can actually see — not an unrelated
/// file's same-named class (C's flat linkage). Wrap the pack index once per
/// request at the LSP/CLI entry point; every downstream `get_cached` inherits
/// the scope with no threaded parameter. `visible` empty ⇒ transparent
/// (Perl / unwarmed on-open). `docs/adr/macro-handling.md`.
pub struct ScopedLookup<'a> {
    inner: &'a dyn CrossFileLookup,
    visible: std::collections::HashSet<String>,
    self_path: Option<std::path::PathBuf>,
    /// How THIS origin decides which same-named candidates it can see.
    /// The scope carries its own rule, so the decorator never asks what
    /// language it is serving.
    axis: VisibilityAxis,
}

/// The rule an origin's language uses to decide which of a name's
/// candidates it can see, and in what order.
///
/// This is the ONE place a visibility model is named. A consumer asks the
/// axis, never "is this pack" — a new model (a `use`-closure tier, an
/// explicit module map) is a variant here and every projection over the
/// `CandidateSet` inherits it by construction.
#[derive(Debug, Clone, Default)]
pub enum VisibilityAxis {
    /// No model: every candidate is visible, unranked. An origin whose
    /// language has no visibility rule, or one whose scope is not yet
    /// known (an unwarmed on-open doc).
    #[default]
    Transparent,
    /// Everything visible BY RULE — a name-keyed pack language (PHP,
    /// Python) whose imports name modules, not paths: same-named
    /// workspace candidates are genuine siblings, never closure-scoped.
    /// Distinct from `Transparent` ("no rule known yet"): a Flat scope
    /// deliberately mints NO `def_paths` gate and admits the full
    /// candidate table where a closure scope degrades to agreement
    /// folds. The scope-less base every name-keyed axis shares; an origin
    /// that carries pins gets `UseMap` instead.
    Flat,
    /// Flat linkage narrowed by the asker's OWN use-map: a name-keyed pack
    /// whose file says what each leaf means (`use A\B\Collection;`, its own
    /// `class Collection` under `namespace A\B`). A pinned leaf's candidates
    /// are exactly the declarations under that namespace — a same-leaf
    /// class in a namespace the file never named is NOT visible, however
    /// common it is (three `Request`s, three `Collection`s in one Laravel
    /// tree). An unpinned leaf admits the full table, the file's own
    /// namespace ranked first. Scope-less like `Flat` for every closure
    /// consumer (no `def_paths` gate, `flat_scope` set).
    UseMap(std::sync::Arc<UseMapPins>),
    /// Flat linkage (C): a candidate is visible when the asker's `#include`
    /// closure reaches it, or when it includes the asker back.
    IncludeClosure,
    /// Search-path order (Perl `@INC`): a candidate is visible when it
    /// lives under one of the asker's roots, and ranks by that root's
    /// position — the asker's own `use lib` first, then project libs, then
    /// system. Roots are canonical directory paths, most-preferred first.
    /// Empty ⇒ behaves as `Transparent`: an origin whose roots are unknown
    /// must not have its answers narrowed to nothing.
    SearchPath(std::sync::Arc<Vec<std::path::PathBuf>>),
}

/// A name-keyed origin's leaf→namespace table (`FileAnalysis::
/// leaf_namespace_pins`): what each unqualified class spelling in that
/// file means, and the file's own namespace as the default for a leaf it
/// neither declares nor imports.
#[derive(Debug, Default)]
pub struct UseMapPins {
    /// `leaf → Some(namespace)`; `None` = conflicting evidence, no claim.
    pub pins: std::collections::HashMap<String, Option<String>>,
    pub own_namespace: Option<String>,
    /// The leaves this file writes as class tokens.
    pub spelled: std::collections::HashSet<String>,
}

impl UseMapPins {
    /// The namespace `leaf` means for this origin: its pin, else — for a
    /// leaf the file itself spells — its own namespace (PHP resolves an
    /// unqualified class name in the current namespace; there is no global
    /// fallback for classes). A leaf the file never writes gets no claim:
    /// its refs reach that class through some other class's dispatch, and
    /// the namespace the file lives in says nothing about it.
    fn namespace_of(&self, leaf: &str) -> Option<&str> {
        match self.pins.get(leaf) {
            Some(p) => p.as_deref(),
            None if self.spelled.contains(leaf) => self.own_namespace.as_deref(),
            None => None,
        }
    }
    /// A leaf the file explicitly named (a `use` row or its own class).
    fn pinned(&self, leaf: &str) -> bool {
        matches!(self.pins.get(leaf), Some(Some(_)))
    }
}

/// How an origin's language scopes cross-file visibility — the routing
/// fact `for_origin` consumes. Derived by the registry from the pack's
/// own linkage declaration (`include_path_tokens`), never a
/// language-name branch here or at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackVisibility {
    /// Not a pack: the host's search-path derivation (`use lib` ∪ @INC).
    Host,
    /// Include-path linkage (C/C++): a name is visible through the
    /// asker's `#include` closure.
    IncludePaths,
    /// Name-keyed pack (PHP/Python/R/CMake): imports name modules, not
    /// paths — there is no closure to scope by, and the host's @INC
    /// roots would rank every candidate invisible. Visibility is the
    /// origin's own use-map (`UseMap`), `Flat` when it carries none.
    NameKeyed,
}

impl VisibilityAxis {
    /// THE derivation of an origin's visibility rule. Call sites pass the
    /// origin and its index; none of them decides which model applies, so
    /// a new model reaches every projection by changing this one function.
    ///
    /// `visibility` is the routing fact for the origin's language — the
    /// caller reads it from the registry (`pack_visibility`) rather than
    /// this layer importing the driver.
    pub fn for_origin(
        origin: &FileAnalysis,
        self_path: Option<&std::path::Path>,
        index: &dyn CrossFileLookup,
        visibility: PackVisibility,
    ) -> Self {
        match visibility {
            PackVisibility::IncludePaths => return VisibilityAxis::IncludeClosure,
            PackVisibility::NameKeyed => {
                let pins = origin.use_map_pins();
                if pins.pins.is_empty() && pins.own_namespace.is_none() {
                    return VisibilityAxis::Flat;
                }
                return VisibilityAxis::UseMap(pins);
            }
            PackVisibility::Host => {}
        }
        let inc = index.inc_roots();
        // The overwhelmingly common origin declares no `use lib`, and this
        // runs on every request path — so that case is an Arc clone with
        // ZERO filesystem calls. Only a file that actually names its own
        // roots pays to resolve them.
        if origin.lib_roots.is_empty() {
            return if inc.is_empty() {
                // Roots unknown (no resolver has run yet): narrowing on an
                // empty set would answer "no such module" everywhere.
                VisibilityAxis::Transparent
            } else {
                VisibilityAxis::SearchPath(inc)
            };
        }
        let ws = index.workspace_root_path();
        let self_dir = self_path.and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        let mut push = |p: std::path::PathBuf| {
            let canon = std::fs::canonicalize(&p).unwrap_or(p);
            // `canonicalize` already failed for a path that does not exist,
            // so the survivors are real; the dir test rejects a `use lib`
            // pointing at a FILE.
            if canon.is_dir() && !roots.contains(&canon) {
                roots.push(canon);
            }
        };
        // The origin's OWN roots first — `use lib 't/lib'` is exactly the
        // statement "for me, this root wins". An entry naming no directory
        // (an interpolated `"$FindBin::Bin/../lib"`) drops out here.
        for raw in &origin.lib_roots {
            let p = std::path::Path::new(raw);
            if p.is_absolute() {
                push(p.to_path_buf());
                continue;
            }
            // Relative entries are CWD-relative in Perl; for a server that
            // is the project root, with the file's own directory as the
            // fallback a script run from its own directory would see.
            if let Some(ref ws) = ws {
                push(ws.join(p));
            }
            if let Some(ref d) = self_dir {
                push(d.join(p));
            }
        }
        // The index's roots are already canonical — appended, never re-stat'd.
        for r in inc.iter() {
            if !roots.contains(r) {
                roots.push(r.clone());
            }
        }
        if roots.is_empty() {
            return VisibilityAxis::Transparent;
        }
        VisibilityAxis::SearchPath(std::sync::Arc::new(roots))
    }

    /// The asker's rank for a candidate path: `Some(0)` is most preferred,
    /// `None` means "not visible under this axis". `Transparent` ranks
    /// everything equally visible.
    fn rank(&self, path: &std::path::Path) -> Option<usize> {
        match self {
            VisibilityAxis::Transparent
            | VisibilityAxis::Flat
            | VisibilityAxis::UseMap(_)
            | VisibilityAxis::IncludeClosure => Some(0),
            VisibilityAxis::SearchPath(roots) if roots.is_empty() => Some(0),
            VisibilityAxis::SearchPath(roots) => {
                // Longest-prefix wins, THEN root order: a vendored
                // `local/lib/perl5` nested inside the project root would
                // otherwise be attributed to whichever of the two roots
                // came first, and the nested one is the more specific fact.
                roots
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| path.starts_with(r))
                    .min_by_key(|(i, r)| (std::cmp::Reverse(r.as_os_str().len()), *i))
                    .map(|(i, _)| i)
            }
        }
    }
}

impl VisibilityAxis {
    /// Scope-less BY RULE: a name-keyed pack's axis (with or without
    /// pins) mints no closure gate and admits the full candidate table
    /// wherever a closure scope would degrade to agreement folds.
    fn name_keyed(&self) -> bool {
        matches!(self, VisibilityAxis::Flat | VisibilityAxis::UseMap(_))
    }
}

impl<'a> ScopedLookup<'a> {
    /// Build the visibility set from a querying file's include closure plus its
    /// own path (a file always sees the classes it declares itself). Canonicalize
    /// the self path so it matches the candidates' canonical `CachedModule.path`.
    pub fn new(
        inner: &'a dyn CrossFileLookup,
        include_closure: &path_intern::ClosureList,
        self_path: Option<&std::path::Path>,
        axis: VisibilityAxis,
    ) -> Self {
        let mut visible: std::collections::HashSet<String> =
            include_closure.iter_strs().map(|a| a.as_ref().to_owned()).collect();
        let self_path = self_path.map(|p| {
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            visible.insert(canon.to_string_lossy().into_owned());
            canon
        });
        ScopedLookup { inner, visible, self_path, axis }
    }
}

impl<'a> CrossFileLookup for ScopedLookup<'a> {
    fn resolution_epoch(&self) -> u64 {
        self.inner.resolution_epoch()
    }
    fn is_dependency_path(&self, path: &std::path::Path) -> bool {
        self.inner.is_dependency_path(path)
    }
    fn get_cached(&self, module_name: &str) -> Option<std::sync::Arc<CachedModule>> {
        // A search-path origin's winner is PER-ASKER: the same name means
        // whichever provider this file's own @INC reaches first. Falling
        // back to the global slot keeps a name with no ranked candidate
        // answering exactly as before.
        if matches!(self.axis, VisibilityAxis::SearchPath(_) | VisibilityAxis::UseMap(_)) {
            if let Some(best) = self.visible_def_candidates(module_name).into_iter().next() {
                return Some(best);
            }
        }
        self.inner.get_cached_scoped(module_name, &self.visible)
    }
    fn get_cached_scoped(
        &self,
        module_name: &str,
        _visible: &std::collections::HashSet<String>,
    ) -> Option<std::sync::Arc<CachedModule>> {
        self.get_cached(module_name)
    }
    fn inc_roots(&self) -> std::sync::Arc<Vec<std::path::PathBuf>> {
        self.inner.inc_roots()
    }
    fn workspace_root_path(&self) -> Option<std::path::PathBuf> {
        self.inner.workspace_root_path()
    }
    fn def_candidates(&self, name: &str) -> Vec<std::sync::Arc<CachedModule>> {
        // Unscoped by design: consumers of the full candidate table weigh
        // definition-ness themselves, and a definition legitimately lives
        // OUTSIDE the querying file's closure (a `.c` body nobody includes).
        self.inner.def_candidates(name)
    }
    fn visible_def_candidates(&self, name: &str) -> Vec<std::sync::Arc<CachedModule>> {
        match &self.axis {
            VisibilityAxis::Transparent | VisibilityAxis::Flat => {
                self.inner.def_candidates(name)
            }
            VisibilityAxis::UseMap(pins) => {
                let cands = self.inner.def_candidates(name);
                // One candidate has nothing to disambiguate; the table read
                // below rehydrates symbols, so only a genuinely ambiguous
                // leaf pays it.
                if cands.len() < 2 {
                    return cands;
                }
                let Some(want) = pins.namespace_of(name) else {
                    return cands;
                };
                // A declaration under the pinned namespace is the class this
                // file means; one under another namespace is a stranger
                // sharing the leaf. A pinned leaf (the file NAMED it) keeps
                // only agreeing declarations — an empty answer is the honest
                // one when the named class isn't indexed. The own-namespace
                // default is a RANK, not a filter: the file made no claim, so
                // the table stays whole with its own namespace first.
                let pinned = pins.pinned(name);
                let mut agree: Vec<std::sync::Arc<CachedModule>> = Vec::new();
                let mut rest: Vec<std::sync::Arc<CachedModule>> = Vec::new();
                for c in cands {
                    let declared = self.inner.symbols_present(&c).declared_class_namespace(name);
                    match declared {
                        Some(ns) if ns == want => agree.push(c),
                        Some(_) if pinned => {}
                        _ => rest.push(c),
                    }
                }
                agree.extend(rest);
                agree
            }
            VisibilityAxis::IncludeClosure => {
                // Flat linkage: keep candidates CONNECTED to the asker —
                // visible in its include closure, or including the asker
                // back (a `.c` body defining what the asker's own header
                // declares) — the same connectivity rule
                // `member_def_location` applies. None connected ⇒ the
                // scope-ranked winner, so an indirect resolution never
                // regresses.
                let self_str = self
                    .self_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned());
                let mut out: Vec<std::sync::Arc<CachedModule>> = self
                    .inner
                    .def_candidates(name)
                    .into_iter()
                    .filter(|c| {
                        let p = c.path.to_string_lossy();
                        self.visible.contains(p.as_ref())
                            || self_str
                                .as_ref()
                                .is_some_and(|sp| c.analysis.pack.include_closure.contains(sp))
                    })
                    .collect();
                if out.is_empty() {
                    out.extend(self.get_cached(name));
                }
                out
            }
            // Search-path order: keep the candidates the asker's own @INC
            // reaches, best-ranked root first. A candidate under NO root of
            // the asker's is a file this file could not load — but dropping
            // every candidate would answer "no such module" where the tier
            // simply doesn't know the asker's roots, so an empty result
            // degrades to the full relation rather than to nothing.
            VisibilityAxis::SearchPath(_) => {
                let mut ranked: Vec<(usize, std::sync::Arc<CachedModule>)> = self
                    .inner
                    .def_candidates(name)
                    .into_iter()
                    .filter_map(|c| self.axis.rank(&c.path).map(|r| (r, c)))
                    .collect();
                if ranked.is_empty() {
                    return self.inner.def_candidates(name);
                }
                // Stable within a rank: `def_candidates` arrives path-ordered
                // and every consumer's tie-break leans on that staying so.
                ranked.sort_by_key(|(r, _)| *r);
                ranked.into_iter().map(|(_, c)| c).collect()
            }
        }
    }
    fn bag_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        // MUST delegate: the inner pack index owns the `PackBagCache`. Without
        // this, cpp cross-file type queries (which thread a `ScopedLookup`)
        // hit the trait default and read the evicted bag — silent Slice-2
        // type regressions while goto/refs stay green.
        self.inner.bag_present(cached)
    }
    fn enriched_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        // Same delegation rule as `bag_present` — the inner index owns the
        // enrichment overlay.
        self.inner.enriched_present(cached)
    }
    fn serves_enriched(&self) -> bool {
        self.inner.serves_enriched()
    }
    fn whole_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        // Same delegation rule as `bag_present` — the inner index owns the LRU.
        self.inner.whole_present(cached)
    }
    fn symbols_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        // Same delegation rule as `bag_present` — the inner index owns the
        // residency answer (the default would re-route to OUR whole_present,
        // losing the symbols-resident fast path).
        self.inner.symbols_present(cached)
    }
    fn refs_present(
        &self,
        cached: &std::sync::Arc<CachedModule>,
    ) -> std::sync::Arc<FileAnalysis> {
        // Same delegation rule as `symbols_present`.
        self.inner.refs_present(cached)
    }
    fn candidate_may_declare(
        &self,
        cached: &std::sync::Arc<CachedModule>,
        name: &str,
        class: &str,
    ) -> bool {
        // Same delegation rule as `symbols_present` — the inner index owns
        // the row store; the default would fail open and lose the skip.
        self.inner.candidate_may_declare(cached, name, class)
    }
    fn candidate_bag_may_answer(
        &self,
        cached: &std::sync::Arc<CachedModule>,
        name: &str,
        class: &str,
        attributed: bool,
    ) -> bool {
        // Same delegation rule as `candidate_may_declare` — the default
        // would fail open and silently disarm the consult pre-filter on
        // every scoped (pack) sweep.
        self.inner.candidate_bag_may_answer(cached, name, class, attributed)
    }
    fn ref_candidate_paths(&self, keys: &[String]) -> Vec<std::path::PathBuf> {
        // Unscoped by design, like `def_candidates`: the backward walk applies
        // its own per-file closure gate; pre-narrowing here would hide sites
        // in files the textual-inclusion extension admits.
        self.inner.ref_candidate_paths(keys)
    }
    fn ref_indexed_paths(&self) -> std::collections::HashSet<std::path::PathBuf> {
        self.inner.ref_indexed_paths()
    }
    fn cached_by_path(
        &self,
        path: &std::path::Path,
    ) -> Option<std::sync::Arc<CachedModule>> {
        self.inner.cached_by_path(path)
    }
    // `parents_cached` deliberately NOT delegated: the provided default
    // unions over THIS decorator's `visible_def_candidates`, so the scope
    // (pack closure narrowing) applies to the parent relation too.
    fn modules_with_symbol(&self, name: &str) -> Vec<String> {
        self.inner.modules_with_symbol(name)
    }
    fn find_exporters(&self, func_name: &str) -> Vec<String> {
        self.inner.find_exporters(func_name)
    }
    fn defining_module_cached(&self, entry: &str, name: &str) -> Option<std::sync::Arc<CachedModule>> {
        self.inner.defining_module_cached(entry, name)
    }
    fn module_declaring_method_in_package(&self, name: &str, class: &str) -> Option<String> {
        self.inner.module_declaring_method_in_package(name, class)
    }
    fn module_path_cached(&self, module_name: &str) -> Option<std::path::PathBuf> {
        // Scope-aware: the path must name the same candidate the scoped
        // `get_cached` answers with, or a consumer that pairs this path with a
        // scoped range splices two different files (wrong file at a
        // nonexistent position). Fall back to the raw path map only when no
        // analysis is cached at all.
        self.get_cached(module_name).map(|c| c.path.clone())
            .or_else(|| self.inner.module_path_cached(module_name))
    }
    fn visibility_scope(
        &self,
    ) -> Option<(&std::path::Path, &std::collections::HashSet<String>)> {
        // A Flat axis IS "no scope": answering the path + closure set
        // would let the backward gate (`pack_def_paths`) narrow a
        // name-keyed pack's references to its (empty) include closure —
        // same-file-only answers, measured on WordPress/monolog.
        // (Transparent still answers: the host's "no rule known yet" case
        // predates the axis and its consumers' closure tests no-op there.)
        if self.axis.name_keyed() {
            return None;
        }
        self.self_path.as_deref().map(|p| (p, &self.visible))
    }
    fn flat_scope(&self) -> bool {
        self.axis.name_keyed()
    }
    fn pinned_namespace(&self, leaf: &str) -> Option<String> {
        match &self.axis {
            VisibilityAxis::UseMap(pins) => pins.namespace_of(leaf).map(str::to_string),
            _ => None,
        }
    }
    fn for_each_cached(&self, f: &mut dyn FnMut(&str, &std::sync::Arc<CachedModule>)) {
        self.inner.for_each_cached(f)
    }
    fn for_each_cached_file(&self, f: &mut dyn FnMut(&std::sync::Arc<CachedModule>)) {
        self.inner.for_each_cached_file(f)
    }
    fn for_each_reexport_module(
        &self,
        start: Vec<String>,
        visit: &mut dyn FnMut(&std::sync::Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    ) {
        self.inner.for_each_reexport_module(start, visit)
    }
    fn for_each_entity_bridged_to(
        &self,
        class_name: &str,
        f: &mut dyn FnMut(
            &str,
            &std::sync::Arc<CachedModule>,
            &Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        self.inner.for_each_entity_bridged_to(class_name, f)
    }
    fn for_each_entity_bridged_to_named(
        &self,
        class_name: &str,
        name: &str,
        f: &mut dyn FnMut(
            &str,
            &std::sync::Arc<CachedModule>,
            &Symbol,
        ) -> std::ops::ControlFlow<()>,
    ) {
        // Same delegation rule as the unnamed walk — the inner index owns
        // the row store the name pre-filter reads.
        self.inner.for_each_entity_bridged_to_named(class_name, name, f)
    }
    fn direct_children_of(&self, class: &str) -> Vec<(String, String)> {
        self.inner.direct_children_of(class)
    }
    fn direct_specializations_of(&self, primary: &str) -> Vec<(String, String)> {
        self.inner.direct_specializations_of(primary)
    }
    fn for_each_loader_shape(&self, f: &mut dyn FnMut(&str, &InferredType)) {
        self.inner.for_each_loader_shape(f)
    }
    fn visible_defs_with_prefix(
        &self,
        prefix: &str,
        visible: &std::collections::HashSet<String>,
    ) -> Vec<(String, std::sync::Arc<CachedModule>)> {
        self.inner.visible_defs_with_prefix(prefix, visible)
    }
}

