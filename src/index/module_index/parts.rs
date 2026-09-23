//! Reverse-edge index bundle plus the registration tokens: `ModuleEdgeIndexes`,
//! the pre-strip Pack/Workspace registration parts, and the surface-write types.

use super::*;

/// Concurrent module cache with background resolution.
///
/// The reverse-edge maps over the module cache, bundled so every feed
/// site updates all of them in lockstep. Every map answers "which
/// modules…" for a different edge:
///
/// - `names`: symbol/export name → holders declaring or exporting it: a
///   Perl module name, or a pack file by path (a pack file is its own
///   holder, so a classless file is reachable by the names it declares).
///   The single generic "find me holders with symbol X" primitive —
///   hover, signature help, goto-def, auto-import, and the
///   unimported-completion path all route through it instead of
///   reinventing per-feature cache walks. Covers every module-visible
///   symbol kind (Sub, Method, Package, Class, Module, HashKeyDef,
///   Handler) plus the export/export_ok lists (XS exporters name
///   functions with no Perl body). Callers wanting narrower semantics
///   filter via per-holder inspection.
/// - `rails`: rail → the handler names declared on it, for the rail-keyed
///   question the name-keyed map would need a scan to answer.
/// - `bridges`: class → modules declaring a `PluginNamespace` whose
///   `bridges` list contains `Bridge::Class(class)`. The one reverse
///   index for plugin-synthesized content; queried through
///   `for_each_entity_bridged_to`.
/// - `children`: parent class/role → modules containing a package
///   that `isa`/composes it (inverse `PackageFacts::parents`). The
///   long-distance primitive: "who composes this role" /
///   "who subclasses this class" in O(1).
///
/// The bundle exists because the feeds must never diverge across the
/// resolve insert path, the SQLite warm rebuild, and workspace
/// registration — a map fed on insert but not on rebuild serves cold
/// sessions and starves warm ones (the twice-paid B6 lesson). One
/// `feed()` per site makes a missed map unrepresentable.
/// One reverse-index bucket: the member list readers iterate, plus the
/// membership test that keeps insertion O(1).
///
/// `seen` is not a cache of `members` — it IS the uniqueness test. A linear
/// scan per insert makes a bulk feed quadratic in bucket size, and the
/// worst case is also the common one: `new` is declared by every module in
/// the workspace, so its bucket IS the workspace. Measured at 8k synthetic
/// modules, the scan cost `rebuild_reverse_index` 4,108 ms of a 15 s CLI
/// startup and grew as ~n^2.5; without it, 137 ms and linear.
///
/// The set materializes only once a bucket is big enough for the scan to
/// cost more than the hash — a size threshold on the data, not a branch on
/// what the data means, so behavior is identical either way. Buckets are
/// overwhelmingly tiny (most sub names are declared once), so the extra
/// strings land only on the few buckets that were the whole problem.
pub struct Bucket<T> {
    members: Vec<T>,
    seen: Option<std::collections::HashSet<T>>,
}

/// The module-name buckets of the class-keyed maps.
pub type ModuleBucket = Bucket<String>;

impl<T> Default for Bucket<T> {
    fn default() -> Self {
        Bucket { members: Vec::new(), seen: None }
    }
}

impl<T: Clone> Clone for Bucket<T> {
    fn clone(&self) -> Self {
        Bucket { members: self.members.clone(), seen: self.seen.clone() }
    }
}

/// Below this a scan is cheaper than a hash and the set is pure overhead.
const BUCKET_SET_THRESHOLD: usize = 32;

impl<T: Clone + Eq + std::hash::Hash> Bucket<T> {
    /// Add `member` if absent. Idempotent per (bucket, member) — every
    /// feed path relies on re-feeding never growing a bucket.
    pub fn insert<Q>(&mut self, member: &Q)
    where
        T: std::borrow::Borrow<Q>,
        Q: ?Sized + Eq + std::hash::Hash + ToOwned<Owned = T>,
    {
        if let Some(seen) = &mut self.seen {
            if !seen.contains(member) {
                seen.insert(member.to_owned());
                self.members.push(member.to_owned());
            }
            return;
        }
        if self.members.iter().any(|m| m.borrow() == member) {
            return;
        }
        self.members.push(member.to_owned());
        if self.members.len() >= BUCKET_SET_THRESHOLD {
            self.seen = Some(self.members.iter().cloned().collect());
        }
    }

    /// Drop `member` from this bucket. Left O(bucket): removal happens per
    /// re-registration, not per bulk feed, so it is not on the hot path.
    pub fn remove<Q>(&mut self, member: &Q)
    where
        T: std::borrow::Borrow<Q>,
        Q: ?Sized + Eq + std::hash::Hash,
    {
        self.members.retain(|m| m.borrow() != member);
        if let Some(seen) = &mut self.seen {
            seen.remove(member);
        }
    }

    /// Test-only: keep the members `keep` admits — the sweep reference's
    /// removal, for members it can only name by predicate.
    #[cfg(test)]
    pub fn retain_members(&mut self, keep: impl Fn(&T) -> bool) {
        self.members.retain(|m| keep(m));
        if let Some(seen) = &mut self.seen {
            seen.retain(|m| keep(m));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The members, in first-fed order.
    pub fn as_slice(&self) -> &[T] {
        &self.members
    }
}

impl<'a, T> IntoIterator for &'a Bucket<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.members.iter()
    }
}

pub struct ModuleEdgeIndexes {
    pub(super) names: DashMap<String, Bucket<Holder>>,
    /// rail → the (handler name, holder) pairs declared on it — the
    /// rail-keyed question ("every route name") that the name-keyed map
    /// cannot answer without a scan. Pairs, not bare names, so a holder's
    /// purge removes exactly its own declarations and a name another
    /// holder also declares survives.
    pub(super) rails: DashMap<String, Bucket<(String, Holder)>>,
    pub(super) bridges: DashMap<String, ModuleBucket>,
    pub(super) children: DashMap<String, ModuleBucket>,
    /// primary template → modules declaring a specialization of it (inverse
    /// `FileAnalysis.pack.specializes`). The `Specializes` family edge's
    /// cross-file half; member resolution never reads it.
    pub(super) specs: DashMap<String, ModuleBucket>,
    /// package → modules with a sub/method ATTRIBUTED to it
    /// (`FileAnalysis::provided_packages`). Distinct from `names`, which is
    /// keyed by the member's own name: the class-keyed question ("who
    /// declares a method for class C") has no answer in a name-keyed index,
    /// which is why `module_declaring_method_in_package` used to fetch every
    /// module declaring a sub of that name — for `new`, most of a corpus.
    /// Cross-package typeglob installs are the reason the key can differ
    /// from the module's registration name.
    pub(super) providers: DashMap<String, ModuleBucket>,
    /// The indexable-name list each FILE last fed — the symbols-derived
    /// half of `feed`, recorded from the WHOLE analysis so a re-feed over
    /// symbol-EVICTED cache copies (`rebuild_reverse_index*` after the
    /// workspace indexer strips, sibling replay after a same-name purge)
    /// replays the names instead of reading empty vecs and silently
    /// blinding `modules_with_symbol`/`find_exporters`. Keyed by PATH, not
    /// module name: several files can feed under one package name (Perl
    /// reopens packages anywhere), and a name-keyed record would replay one
    /// file's names for its siblings. `clear()` and `purge_module` keep it
    /// (re-feeds are exactly when it's needed); `remove_path_record` drops
    /// it when the file itself goes.
    name_records: DashMap<std::path::PathBuf, FedNames>,
    /// Every holder `feed` / `feed_file` has published edges under, and — the point
    /// — WHICH bucket keys it was published under in each map, so
    /// `purge_module` touches only its own edges.
    ///
    /// The guard alone is not enough. Its old comment claimed a cold bulk
    /// index never re-feeds a name; that is false on any corpus with
    /// duplicate package names, and being false silently is what hid this.
    /// An installed `@INC` tree is full of them — bundled `inc/Module/Install`
    /// alone appears in hundreds of distributions — and each duplicate pays a
    /// sweep over EVERY bucket of all four maps. Applications with unique
    /// module names skip every sweep, which is why the term is invisible on
    /// Koha and only bites on a CPAN-shaped tree.
    fed_modules: DashMap<Holder, FedKeys>,
}

/// The bucket keys one module was fed under, per map — the reverse record
/// that makes retraction O(own edges) instead of O(all buckets).
///
/// `ModuleBucket` reused rather than a fresh set type: it is exactly a
/// deduplicated string collection that scans while small and promotes to a
/// hash when it is not, and one name's key list has the same shape as one
/// bucket's member list. A second implementation of that threshold would be
/// a thing to keep in sync for no gain.
#[derive(Default, Clone)]
struct FedKeys {
    names: ModuleBucket,
    /// (rail, handler name) — the holder's own entries in `rails`.
    rails: Bucket<(String, String)>,
    bridges: ModuleBucket,
    children: ModuleBucket,
    specs: ModuleBucket,
    providers: ModuleBucket,
}

/// The name-keyed half of one FILE's feed: every name it can be looked up
/// by, and the (rail, name) pairs its rail handlers declare. Read from
/// `symbols()`, so a registration extracts it from the WHOLE analysis
/// before any strip; the pack tier carries it on its registration token
/// and its warm stub.
#[derive(Default, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NameFeed {
    pub(crate) names: Vec<String>,
    pub(crate) rails: Vec<(String, String)>,
}

impl NameFeed {
    /// Every name a module or file might be located by — declared
    /// module-visible symbols plus the export/export_ok lists — and every
    /// rail handler's (rail, name). Variables and fields are skipped:
    /// file-local, not queryable across files.
    pub(crate) fn of(analysis: &FileAnalysis) -> Self {
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut rails: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for sym in analysis.symbols() {
            if matches!(
                sym.kind,
                SymKind::Sub | SymKind::Method | SymKind::Package | SymKind::Class
                    | SymKind::Module | SymKind::HashKeyDef | SymKind::Handler,
            ) {
                names.insert(sym.name.clone());
            }
            if let crate::model::file_analysis::SymbolDetail::Handler {
                owner: crate::model::file_analysis::HandlerOwner::Rail(rail),
                ..
            } = &sym.detail
            {
                rails.insert((rail.clone(), sym.name.clone()));
            }
        }
        names.extend(analysis.export.iter().cloned());
        names.extend(analysis.export_ok.iter().cloned());
        // Sorted so a persisted stub is byte-stable across runs.
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        let mut rails: Vec<(String, String)> = rails.into_iter().collect();
        rails.sort();
        NameFeed { names, rails }
    }
}

/// The symbols-derived halves of one FILE's feed, recorded together so a
/// replay over a symbol-evicted copy can never carry one and miss the
/// other. Both are read from `analysis.symbols()`, the only evictable axis
/// `feed` touches.
#[derive(Default, Clone)]
struct FedNames {
    names: NameFeed,
    providers: Vec<String>,
}

impl FedKeys {
    /// Union `other`'s keys into this record.
    fn merge(&mut self, other: &FedKeys) {
        for k in other.rails.as_slice() {
            self.rails.insert(k);
        }
        for (dst, src) in [
            (&mut self.names, &other.names),
            (&mut self.bridges, &other.bridges),
            (&mut self.children, &other.children),
            (&mut self.specs, &other.specs),
            (&mut self.providers, &other.providers),
        ] {
            for k in src.as_slice() {
                dst.insert(k);
            }
        }
    }
}

impl ModuleEdgeIndexes {
    pub fn new() -> Self {
        ModuleEdgeIndexes {
            names: DashMap::new(),
            rails: DashMap::new(),
            bridges: DashMap::new(),
            children: DashMap::new(),
            specs: DashMap::new(),
            providers: DashMap::new(),
            name_records: DashMap::new(),
            fed_modules: DashMap::new(),
        }
    }

    /// Register every edge `analysis` contributes under `module_name` — the
    /// Perl tier's write path. With `feed_file` (the pack tier's) and the
    /// two single-edge publishers, the ONLY write paths besides
    /// `purge_holder`/`clear` — new edge maps get their extraction added
    /// here and nowhere else. Eviction-
    /// aware: a symbol-stripped copy replays `path`'s recorded name list;
    /// a whole copy recomputes and re-records it. Idempotent per
    /// (bucket, module_name): re-feeding never grows a bucket, so the
    /// candidate-set rebuilds (purge + one feed per candidate) and the
    /// warm rebuild can overlap without accumulation.
    pub fn feed(&self, module_name: &str, path: &std::path::Path, analysis: &FileAnalysis) {
        let FedNames { names, providers } = if analysis.symbols_are_evicted() {
            match self.name_records.get(path) {
                Some(rec) => rec.clone(),
                // No record (a stripped copy fed without ever being fed
                // whole — shouldn't happen, but degrade to the pinned
                // export names rather than nothing).
                None => Self::symbol_derived_names(analysis),
            }
        } else {
            let rec = Self::symbol_derived_names(analysis);
            self.name_records.insert(path.to_path_buf(), rec.clone());
            rec
        };
        let holder = Holder::Module(module_name.to_string());
        // Built locally and merged once: holding a `fed_modules` entry across
        // the edge-map writes would nest two DashMap locks on the bulk path
        // for no reason.
        let mut fed = self.name_edges(&holder, &names);
        let push_unique =
            |map: &DashMap<String, ModuleBucket>, key: String, seen: &mut ModuleBucket| {
                seen.insert(&key);
                map.entry(key).or_default().insert(module_name);
            };
        for class in Self::bridge_classes(analysis) {
            push_unique(&self.bridges, class, &mut fed.bridges);
        }
        for parent in Self::parent_classes(analysis) {
            push_unique(&self.children, parent, &mut fed.children);
        }
        for primary in Self::spec_primaries(analysis) {
            push_unique(&self.specs, primary, &mut fed.specs);
        }
        for pkg in providers {
            push_unique(&self.providers, pkg, &mut fed.providers);
        }
        // UNION, not replace: several files can feed under one package name
        // (Perl reopens packages anywhere), and `rebuild_name_registration`
        // feeds every candidate after one purge. Replacing would leave the
        // earlier siblings' keys unrecorded and their edges unpurgeable.
        let mut rec = self.fed_modules.entry(holder).or_default();
        rec.merge(&fed);
    }

    /// Register one pack FILE's names under its own holder — the pack
    /// tier's write path. The feed comes from the registration token (read
    /// from the whole analysis pre-strip), never from the stored copy, so
    /// no per-file replay record is kept: every pack registration arrives
    /// with its names in hand. REPLACES the holder's previous edges, so a
    /// re-registration that dropped a name drops its entry.
    pub fn feed_file(&self, holder: &Holder, feed: &NameFeed) {
        crate::util::ghost_stats::count_by("edges.file_names_fed", feed.names.len() as u64);
        self.purge_holder(holder);
        let fed = self.name_edges(holder, feed);
        self.fed_modules.entry(holder.clone()).or_default().merge(&fed);
    }

    /// Publish the name-keyed edges (`names` + `rails`) of one holder and
    /// return the keys they went under — the half both tiers share.
    fn name_edges(&self, holder: &Holder, feed: &NameFeed) -> FedKeys {
        let mut fed = FedKeys::default();
        for name in &feed.names {
            fed.names.insert(name.as_str());
            self.names.entry(name.clone()).or_default().insert(holder);
        }
        for pair in &feed.rails {
            fed.rails.insert(pair);
            let (rail, name) = pair;
            self.rails.entry(rail.clone()).or_default().insert(&(name.clone(), holder.clone()));
        }
        fed
    }

    /// Publish ONE specialization edge (primary → spec). The pack path
    /// records these outside `feed`, and every publication must mark its
    /// member fed or `purge_module`'s guard will skip a module that does
    /// have edges.
    pub fn publish_spec(&self, primary: &str, spec: &str) {
        self.fed_modules
            .entry(Holder::Module(spec.to_string()))
            .or_default()
            .specs
            .insert(primary);
        self.specs.entry(primary.to_string()).or_default().insert(spec);
    }

    /// Publish ONE inverse-inheritance edge (parent → child). Same
    /// marking contract as `publish_spec`.
    pub fn publish_child(&self, parent: &str, child: &str) {
        self.fed_modules
            .entry(Holder::Module(child.to_string()))
            .or_default()
            .children
            .insert(parent);
        self.children.entry(parent.to_string()).or_default().insert(child);
    }

    /// Test-only REFERENCE implementation: the whole-map sweep the reverse
    /// record replaced. Kept so the record-driven purge can be checked
    /// against it directly rather than against hand-written expectations —
    /// the failure mode is a stale edge that survives, which no
    /// spot-assertion reliably catches.
    #[cfg(test)]
    pub fn purge_module_by_sweep(&self, module_name: &str) {
        let holder = Holder::Module(module_name.to_string());
        if self.fed_modules.remove(&holder).is_none() {
            return;
        }
        self.names.retain(|_key, bucket| {
            bucket.remove(&holder);
            !bucket.is_empty()
        });
        self.rails.retain(|_key, bucket| {
            bucket.retain_members(|(_, h)| h != &holder);
            !bucket.is_empty()
        });
        for map in [&self.bridges, &self.children, &self.specs, &self.providers] {
            map.retain(|_key, bucket| {
                bucket.remove(module_name);
                !bucket.is_empty()
            });
        }
    }

    /// Test-only: the whole edge state, canonically ordered, for comparing
    /// two purge implementations.
    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<(&'static str, String, Vec<String>)> {
        let mut out = Vec::new();
        for e in self.names.iter() {
            let mut members: Vec<String> =
                e.value().as_slice().iter().map(|h| h.to_string()).collect();
            members.sort();
            out.push(("names", e.key().clone(), members));
        }
        for e in self.rails.iter() {
            let mut members: Vec<String> =
                e.value().as_slice().iter().map(|(n, h)| format!("{n}@{h}")).collect();
            members.sort();
            out.push(("rails", e.key().clone(), members));
        }
        for (label, map) in [
            ("bridges", &self.bridges),
            ("children", &self.children),
            ("specs", &self.specs),
            ("providers", &self.providers),
        ] {
            for e in map.iter() {
                let mut members = e.value().as_slice().to_vec();
                members.sort();
                out.push((label, e.key().clone(), members));
            }
        }
        out.sort();
        out
    }

    /// Test-only bucket readers: the maps are `pub(super)`, and these
    /// contracts are what the perf rewrite rests on.
    #[cfg(test)]
    pub fn specs_for(&self, primary: &str) -> Vec<String> {
        self.specs.get(primary).map(|b| b.as_slice().to_vec()).unwrap_or_default()
    }
    #[cfg(test)]
    pub fn children_of(&self, parent: &str) -> Vec<String> {
        self.children.get(parent).map(|b| b.as_slice().to_vec()).unwrap_or_default()
    }
    #[cfg(test)]
    pub fn providers_of(&self, pkg: &str) -> Vec<String> {
        self.providers.get(pkg).map(|b| b.as_slice().to_vec()).unwrap_or_default()
    }

    /// Record `path`'s indexable-name list from a WHOLE analysis so a later
    /// `feed` of its stripped copy replays it — the pre-strip half of the
    /// split workspace registration, where the feed itself waits for the
    /// blob COMMIT but only the whole analysis can spell the names.
    pub fn record_names(&self, path: &std::path::Path, analysis: &FileAnalysis) {
        debug_assert!(!analysis.symbols_are_evicted());
        self.name_records
            .insert(path.to_path_buf(), Self::symbol_derived_names(analysis));
    }

    /// Remove `holder` from every bucket of every map. Runs before
    /// re-registration so stale edges from a prior version of the same
    /// module or file don't accumulate (phantom lookups).
    /// KEEPS `name_records` — they are per-PATH, and a same-name sibling
    /// file's replay source must survive this file's re-registration.
    pub fn purge_holder(&self, holder: &Holder) {
        // Never fed ⇒ no bucket can hold it.
        let Some((_, fed)) = self.fed_modules.remove(holder) else {
            crate::util::ghost_stats::count("edges.purge_skipped_never_fed");
            return;
        };
        crate::util::ghost_stats::count("edges.purge_by_record");
        for key in fed.names.as_slice() {
            let now_empty = match self.names.get_mut(key) {
                Some(mut bucket) => {
                    bucket.remove(holder);
                    bucket.is_empty()
                }
                None => false,
            };
            if now_empty {
                self.names.remove_if(key, |_, b| b.is_empty());
            }
        }
        for (rail, name) in fed.rails.as_slice() {
            let entry = (name.clone(), holder.clone());
            let now_empty = match self.rails.get_mut(rail) {
                Some(mut bucket) => {
                    bucket.remove(&entry);
                    bucket.is_empty()
                }
                None => false,
            };
            if now_empty {
                self.rails.remove_if(rail, |_, b| b.is_empty());
            }
        }
        let Holder::Module(module_name) = holder else {
            // The class-keyed maps' members are module names; a file holder
            // publishes only name-keyed edges.
            return;
        };
        let module_name = module_name.as_str();
        for (map, keys) in [
            (&self.bridges, &fed.bridges),
            (&self.children, &fed.children),
            (&self.specs, &fed.specs),
            (&self.providers, &fed.providers),
        ] {
            for key in keys.as_slice() {
                let now_empty = match map.get_mut(key) {
                    Some(mut bucket) => {
                        bucket.remove(module_name);
                        bucket.is_empty()
                    }
                    None => false,
                };
                if now_empty {
                    // Keep "emptied" indistinguishable from "never fed", so no
                    // reader needs an empty-bucket arm — the shape the whole-map
                    // `retain` produced by dropping the entry.
                    map.remove_if(key, |_, b| b.is_empty());
                }
            }
        }
    }

    /// Drop `path`'s recorded name list (the file itself is gone).
    pub fn remove_path_record(&self, path: &std::path::Path) {
        self.name_records.remove(path);
    }

    /// Wipe the edge maps for a rebuild. Deliberately KEEPS `name_records`
    /// — the rebuild re-feeds from cache copies that may be symbol-evicted,
    /// and the records are their only complete name source.
    pub fn clear(&self) {
        self.names.clear();
        self.rails.clear();
        self.bridges.clear();
        self.children.clear();
        self.specs.clear();
        self.providers.clear();
        // The marks describe the maps just emptied; keeping them would let
        // a later purge take the sweep for a module with no edges left,
        // and — worse — a re-feed would find its mark already set.
        self.fed_modules.clear();
    }

    /// One walk of `symbols()` for both symbol-derived feed halves — the
    /// record they land in is what a stripped copy replays.
    fn symbol_derived_names(analysis: &FileAnalysis) -> FedNames {
        FedNames {
            names: NameFeed::of(analysis),
            providers: analysis.provided_packages(),
        }
    }

    /// Every handler name declared on the string rail `rail`, across every
    /// holder — the rail-name completion source. Sorted, deduped.
    pub fn rail_names(&self, rail: &str) -> Vec<String> {
        let mut out: Vec<String> = match self.rails.get(rail) {
            Some(bucket) => bucket.as_slice().iter().map(|(n, _)| n.clone()).collect(),
            None => Vec::new(),
        };
        out.sort();
        out.dedup();
        out
    }

    /// The bridge classes an analysis' plugin namespaces declare, deduped.
    fn bridge_classes(analysis: &FileAnalysis) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ns in &analysis.plugin.namespaces {
            for crate::model::file_analysis::Bridge::Class(c) in &ns.bridges {
                seen.insert(c.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Every primary a specialization in the analysis names — the values of
    /// `specializes`, deduped.
    fn spec_primaries(analysis: &FileAnalysis) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for primary in analysis.pack.specializes.values() {
            seen.insert(primary.clone());
        }
        seen.into_iter().collect()
    }

    /// Every parent class/role any package in the analysis records —
    /// the values of `PackageFacts::parents`, deduped. `use parent`/`use
    /// base`/`@ISA`/`class :isa`/`:does`/`with` all land here, so the
    /// `children` map covers inheritance and role composition alike.
    fn parent_classes(analysis: &FileAnalysis) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_pkg, parents) in analysis.package_parent_edges() {
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
    pub(super) arc: Arc<FileAnalysis>,
    pub(super) feed: Vec<(String, bool)>,
    /// Every name the file is found by, fed under its own holder.
    pub(super) names: NameFeed,
    pub(super) specs: Vec<(String, String)>,
    pub(super) surface: Option<crate::model::surface::Surface>,
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
    pub(crate) fn names(&self) -> &NameFeed {
        &self.names
    }
    /// The projected surface — valid only BEFORE `record_surface` takes it.
    /// Panics after, rather than handing back an empty one: the caller that
    /// reads this encodes a warm stub, and an empty surface baked into a
    /// persisted stub is served as valid on every later warm start. A loud
    /// ordering failure beats a stub that is quietly wrong across sessions.
    pub(crate) fn surface(&self) -> &crate::model::surface::Surface {
        self.surface
            .as_ref()
            .expect("read the surface before record_surface takes it")
    }

    /// A whole-copy token minted from an already-`Arc`'d analysis: the feed
    /// reads the whole `symbols`, the surface projects from the whole bag.
    /// The deliberate whole-copy front door (`register_symbols`) — bounded,
    /// tripwire-counted at its call sites.
    pub(crate) fn whole(arc: Arc<FileAnalysis>) -> Self {
        let (feed, names, specs) = ModuleIndex::prepare_pack_feed(&arc);
        let surface = crate::model::surface::Surface::project(&arc);
        PackRegistrationParts { arc, feed, names, specs, surface: Some(surface) }
    }

    /// Rehydrate a token from a warm stub — the persisted form of a prior
    /// `prepare_pack_parts` output (`encode_stub` was fed exactly these
    /// halves). The proof-of-strip is the persistence itself: a stub only
    /// exists because a fully-stripped copy was written.
    pub(crate) fn from_warm_stub(stub: crate::index::module_cache::WarmStub) -> Self {
        PackRegistrationParts {
            arc: Arc::new(stub.skeleton),
            feed: stub.feed,
            names: stub.names,
            specs: stub.specs,
            surface: Some(stub.surface),
        }
    }

    /// Record this file's span-free surface (the freshness write half).
    /// Separate from registration so the deferred-writer path can record
    /// pre-COMMIT (session-local) while the residency half waits for the
    /// commit; the sync front doors record then register in sequence.
    ///
    /// TAKES the surface rather than cloning it. Registration discards it
    /// (`surface: _`), so a token that rides the bounded persist queue would
    /// otherwise carry a payload whose only remaining use is to be dropped.
    /// Calling this twice records an empty surface the second time — the one
    /// caller shape is record-then-hand-off, and every call site does that.
    pub(crate) fn record_surface(
        &mut self,
        idx: &ModuleIndex,
        path: &std::path::Path,
    ) -> crate::model::surface::SurfaceVerdict {
        idx.record_surface_value(path, self.surface.take().unwrap_or_default())
    }
}

/// The workspace registration TOKEN — the Perl twin of
/// `PackRegistrationParts`. Same private-field / choke-point-mint discipline:
/// minted only by `prepare_workspace_parts` (strip) in this module.
pub(crate) struct WorkspaceRegistrationParts {
    pub(super) arc: Arc<FileAnalysis>,
    /// EVERY package name the file declares (name, is-class), extracted
    /// pre-strip — Perl allows any number of packages per file, and each
    /// one must be reachable by name (`docs/adr/file-store-and-resolve.md`).
    pub(super) names: Vec<(String, bool)>,
    pub(super) surface: Option<crate::model::surface::Surface>,
}

impl WorkspaceRegistrationParts {
    pub(crate) fn arc(&self) -> &Arc<FileAnalysis> {
        &self.arc
    }

    /// Replace the projection this token carries with a PERSISTED one.
    ///
    /// The warm lane's analysis is bag-evicted and `Surface::project` reads
    /// the bag, so the projection minted here describes a smaller file than
    /// the one on disk — and nothing about the result says it is partial. The
    /// cold lane's projection was stored beside the blob precisely so the warm
    /// lane can record what the cold lane recorded instead of re-deriving a
    /// degraded twin (`docs/adr/storage-engine.md`).
    pub(crate) fn adopt_surface(&mut self, surface: crate::model::surface::Surface) {
        self.surface = Some(surface);
    }

    /// See `PackRegistrationParts::record_surface` — takes, does not clone.
    pub(crate) fn record_surface(
        &mut self,
        idx: &ModuleIndex,
        path: &std::path::Path,
    ) -> crate::model::surface::SurfaceVerdict {
        idx.record_surface_value(path, self.surface.take().unwrap_or_default())
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
    pub verdict: crate::model::surface::SurfaceVerdict,
    pub dirty: std::collections::HashSet<std::path::PathBuf>,
}
