//! The reference table: every ref this file records, together with the
//! indices derived from them, their eviction axis, and their enrichment
//! baseline.
//!
//! Refs and their indices are one axis, so they live in one owner: an
//! `evict()` that forgot an index, or a rebuild that missed one, is not
//! expressible from outside. `FileAnalysis` holds exactly one of these and
//! delegates its ref-shaped queries to it.

use super::*;
use std::collections::BTreeMap;

/// Every ref in one file, plus the derived lookups over them.
///
/// Serialized fields are the refs themselves and the enrichment baseline;
/// the three indices are `serde(skip)` and rebuilt after load, and the
/// eviction flag is `serde(skip)` so a rehydrated table is refs-present
/// (an empty `refs` on a evicted table means "on disk", never "no
/// references" — see `docs/adr/relational-ref-index.md`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefTable {
    refs: Vec<Ref>,

    /// Enrichment baseline: `enrich_imported_types_with_keys` re-derives
    /// synthetic refs (imported hash keys) on every run, so it truncates
    /// back to this length first and stays idempotent.
    #[serde(default)]
    base_count: usize,

    #[serde(skip, default)]
    evicted: bool,

    /// Refs bucketed by `Ref::match_key` — the SAME key the relational
    /// `refs` rows carry, so an in-file probe narrows exactly as the
    /// cross-file retrieval does: every matcher arm compares either the
    /// exact `target_name` or its unqualified tail, and equal names have
    /// equal keys, so the bucket for a target's key is a superset of what
    /// any arm can match. The backward walk reads this instead of scanning
    /// the whole vec per (query, candidate file).
    #[serde(skip, default)]
    by_key: HashMap<String, Vec<usize>>,

    /// Refs indexed by the SymbolId they resolve to — "refs to symbol X"
    /// is an O(1) lookup.
    #[serde(skip, default)]
    by_target: HashMap<SymbolId, Vec<usize>>,

    /// Call-shaped refs (MethodCall / FunctionCall) sorted by
    /// `(start, end, index)`. Answers both chain lookups by binary search:
    /// the narrowest call at a start point (`Foo->new->m`'s outer `->m`
    /// `invocant_span` starts at the inner `Foo->new` call's start), and
    /// the call spanning exactly an expression.
    #[serde(skip, default)]
    calls_by_span: Vec<usize>,
    /// Most calls sharing one start point: a fluent chain's calls all open
    /// where its first receiver does, so this is the file's deepest chain.
    #[serde(skip, default)]
    longest_call_chain: usize,

    /// Macro delegation edges, minted by the build per used macro name:
    /// `IncRef → [B, Perl_Inc]` when this file's preprocessor view sees
    /// `#define IncRef(sv) B(sv)` and `#define B(x) Perl_Inc(x)`. A use
    /// spelled through the macro IS a use of every name in its chain, so
    /// the use is indexed (and row-shredded) under each delegate's key too
    /// — the backward half of goto-def's see-through. Empty outside C.
    #[serde(default)]
    delegations: BTreeMap<String, Box<[String]>>,
}

impl RefTable {
    /// Adopt a builder's walk-time ref vec. Indices are built by
    /// `rebuild_indices`, which the owning `FileAnalysis` runs.
    pub fn from_vec(refs: Vec<Ref>) -> Self {
        RefTable { refs, ..Default::default() }
    }

    pub fn as_slice(&self) -> &[Ref] {
        &self.refs
    }

    pub fn as_mut_slice(&mut self) -> &mut [Ref] {
        &mut self.refs
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Ref> {
        self.refs.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Ref> {
        self.refs.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&Ref> {
        self.refs.get(i)
    }

    /// Append a ref. Callers that append post-build (enrichment, gated
    /// emissions) refresh the indices afterwards.
    pub fn push(&mut self, r: Ref) {
        self.refs.push(r);
    }

    /// Ref indexes whose `match_key` is `key`, in ref order.
    pub fn by_key(&self, key: &str) -> &[usize] {
        self.by_key.get(key).map_or(&[], |v| v.as_slice())
    }

    /// Ref indexes resolving to `sym_id`.
    pub fn to_symbol(&self, sym_id: SymbolId) -> &[usize] {
        self.by_target.get(&sym_id).map_or(&[], |v| v.as_slice())
    }

    /// The call-shaped ref anchored at `start` — the chain-receiver hop.
    /// The narrowest span wins (closest to the actual receiver: in
    /// `Foo->new->m` the outer `m` and inner `Foo->new` share a start), a
    /// tie the earlier ref.
    pub fn call_at_start(&self, start: &Point) -> Option<usize> {
        let at = self.calls_by_span.partition_point(|&i| self.refs[i].span.start < *start);
        self.calls_by_span.get(at).copied().filter(|&i| self.refs[i].span.start == *start)
    }

    /// Calls in the file's deepest fluent chain (`a->b->c` is 2, the
    /// receiver `a` is no call).
    pub fn longest_call_chain(&self) -> usize {
        self.longest_call_chain
    }

    /// The first call-shaped ref whose span is exactly `span`.
    pub fn call_spanning(&self, span: Span) -> Option<usize> {
        let key = |i: usize| (self.refs[i].span.start, self.refs[i].span.end);
        let at = self.calls_by_span.partition_point(|&i| key(i) < (span.start, span.end));
        self.calls_by_span.get(at).copied().filter(|&i| self.refs[i].span == span)
    }

    /// Every `(anchor point, ref index)` `call_at_start` can answer — the
    /// shape pins read it to assert the tiebreak and that only call-shaped
    /// kinds are indexed.
    pub fn call_index_entries(&self) -> impl Iterator<Item = (Point, usize)> + '_ {
        self.calls_by_span
            .iter()
            .enumerate()
            .filter(|&(k, &i)| {
                k == 0 || self.refs[self.calls_by_span[k - 1]].span.start != self.refs[i].span.start
            })
            .map(|(_, &i)| (self.refs[i].span.start, i))
    }

    /// Every ref's relational row seed, in ref order. A delegated use adds
    /// one seed per delegate key, so retrieval for the delegate finds it.
    pub fn row_seeds(&self, names: &NameSpellings) -> Vec<RefRowSeed> {
        let mut out = Vec::with_capacity(self.refs.len());
        for (i, r) in self.refs.iter().enumerate() {
            let seed = r.row_seed(names);
            for d in self.delegates_at(i, names) {
                let key = name_match_key(d, names);
                if key != seed.key {
                    out.push(RefRowSeed { key, ..seed.clone() });
                }
            }
            out.push(seed);
        }
        out
    }

    /// Adopt the build's macro delegation edges. The owning `FileAnalysis`
    /// rebuilds the indices afterwards.
    pub fn set_delegations(&mut self, delegations: BTreeMap<String, Box<[String]>>) {
        self.delegations = delegations;
    }

    /// The names ref `i` reaches through macro delegation — empty unless
    /// the ref can spell a macro use and names a delegating macro.
    pub fn delegates_at(&self, i: usize, names: &NameSpellings) -> &[String] {
        if self.delegations.is_empty() {
            return &[];
        }
        let r = &self.refs[i];
        if !r.can_spell_macro_use() {
            return &[];
        }
        self.delegations.get(r.unqualified_target_name(names)).map_or(&[], |d| d)
    }

    /// Bucket ref `i` under its own match key and every delegate's.
    fn index_keys(&mut self, i: usize, names: &NameSpellings) {
        let own = self.refs[i].match_key(names);
        let extra: Vec<String> = self
            .delegates_at(i, names)
            .iter()
            .map(|d| name_match_key(d, names))
            .filter(|k| *k != own)
            .collect();
        for k in extra {
            self.by_key.entry(k).or_default().push(i);
        }
        self.by_key.entry(own).or_default().push(i);
    }

    /// Seal the enrichment baseline at the current length.
    pub fn seal_baseline(&mut self) {
        self.base_count = self.refs.len();
    }

    /// Drop everything enrichment appended, restoring the build-time prefix.
    pub fn truncate_to_baseline(&mut self) {
        self.refs.truncate(self.base_count);
    }

    /// Rebuild every index from the current refs. Both the name/target
    /// lookups and the start-anchored call index.
    pub fn rebuild_indices(&mut self, names: &NameSpellings) {
        self.by_key.clear();
        self.by_target.clear();
        self.calls_by_span.clear();
        for i in 0..self.refs.len() {
            self.index_keys(i, names);
            let r = &self.refs[i];
            if let Some(sym_id) = r.resolved_symbol() {
                self.by_target.entry(sym_id).or_default().push(i);
            }
            if matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. }) {
                self.calls_by_span.push(i);
            }
        }
        let refs = &self.refs;
        self.calls_by_span.sort_by_key(|&i| (refs[i].span.start, refs[i].span.end, i));
        self.longest_call_chain = self
            .calls_by_span
            .chunk_by(|&a, &b| refs[a].span.start == refs[b].span.start)
            .map(<[usize]>::len)
            .max()
            .unwrap_or(0);
    }

    /// Refresh the name/target lookups only — the enrichment refresh.
    /// `calls_by_span` is left alone on purpose: every entry it holds
    /// points into the baseline prefix (it is built before the baseline is
    /// sealed, and no post-build pass mints call refs), and enrichment
    /// restores that prefix verbatim before appending only synthetic
    /// key refs.
    pub fn refresh_name_target_indices(&mut self, names: &NameSpellings) {
        self.by_key.clear();
        self.by_target.clear();
        for i in 0..self.refs.len() {
            self.index_keys(i, names);
            if let Some(sym_id) = self.refs[i].resolved_symbol() {
                self.by_target.entry(sym_id).or_default().push(i);
            }
        }
    }

    /// Strip the resident refs and every index over them — the refs
    /// eviction axis. Lossless: the on-disk analysis keeps the full vec,
    /// and the backward walk retrieves candidates from the relational
    /// index and rehydrates through `whole_present`. Idempotent.
    pub fn evict(&mut self) {
        self.refs = Vec::new();
        self.by_key = HashMap::new();
        self.by_target = HashMap::new();
        self.calls_by_span = Vec::new();
        self.longest_call_chain = 0;
        self.delegations = BTreeMap::new();
        self.evicted = true;
    }

    /// True when `evict` stripped this copy.
    pub fn is_evicted(&self) -> bool {
        self.evicted
    }

    /// Add this table's footprint to a heap probe: the refs bucket (vec +
    /// deep target-name strings) and the table's share of the rebuilt
    /// indices bucket. See [`HeapBreakdown`].
    pub fn heap_add(&self, h: &mut HeapBreakdown) {
        h.refs += self.refs.capacity() * std::mem::size_of::<Ref>()
            + self
                .refs
                .iter()
                .map(|r| r.target_name.capacity() + std::mem::size_of::<String>())
                .sum::<usize>()
            + self
                .delegations
                .iter()
                .map(|(k, v)| {
                    k.capacity()
                        + 64
                        + v.iter().map(|d| d.capacity() + std::mem::size_of::<String>()).sum::<usize>()
                })
                .sum::<usize>();

        let mut b = mcap(&self.by_target)
            + mcap(&self.by_key)
            + self.calls_by_span.capacity() * std::mem::size_of::<usize>();
        for (k, v) in &self.by_key {
            b += k.capacity() + v.capacity() * std::mem::size_of::<usize>();
        }
        for v in self.by_target.values() {
            b += v.capacity() * std::mem::size_of::<usize>();
        }
        h.rebuilt_indices += b;
    }
}

impl std::ops::Index<usize> for RefTable {
    type Output = Ref;

    fn index(&self, i: usize) -> &Ref {
        &self.refs[i]
    }
}

impl std::ops::IndexMut<usize> for RefTable {
    fn index_mut(&mut self, i: usize) -> &mut Ref {
        &mut self.refs[i]
    }
}

impl<'a> IntoIterator for &'a RefTable {
    type Item = &'a Ref;
    type IntoIter = std::slice::Iter<'a, Ref>;

    fn into_iter(self) -> Self::IntoIter {
        self.refs.iter()
    }
}

impl<'a> IntoIterator for &'a mut RefTable {
    type Item = &'a mut Ref;
    type IntoIter = std::slice::IterMut<'a, Ref>;

    fn into_iter(self) -> Self::IntoIter {
        self.refs.iter_mut()
    }
}

impl FileAnalysis {
    /// Every ref this file records (rule #7: every meaningful token gets
    /// one). Empty on an evicted copy — see `refs_are_evicted`.
    pub fn refs(&self) -> &[Ref] {
        self.refs.as_slice()
    }

    /// Mutable ref view for the post-build stamping passes. Bindings only —
    /// the vec's shape is fixed here, so the indices stay valid.
    // Its one caller is the pack lane's implicit-`this` pass, compiled behind
    // the pack language features; a Perl-only build has no mutator outside
    // `model/`.
    #[allow(dead_code)]
    /// Adopt build-time refs minted after assembly (the driver's text
    /// rails): appended, indexed, and sealed into the enrichment baseline —
    /// they are facts of the build, not enrichment, so a re-enrichment
    /// truncating to the baseline keeps them.
    pub fn adopt_text_refs(&mut self, refs: Vec<Ref>) {
        if refs.is_empty() {
            return;
        }
        for r in refs {
            self.refs.push(r);
        }
        self.refs.rebuild_indices(&self.pack.names);
        self.refs.seal_baseline();
    }

    pub fn refs_mut(&mut self) -> &mut [Ref] {
        self.refs.as_mut_slice()
    }

    /// All refs that resolve to this symbol — O(1) lookup via the index.
    /// Callers typically combine this with a kind filter.
    pub fn refs_to_symbol(&self, sym_id: SymbolId) -> &[usize] {
        self.refs.to_symbol(sym_id)
    }

    /// Project every ref into its relational row seed
    /// (`docs/adr/relational-ref-index.md`) — the shredder's input, twin
    /// of `sym_row_seeds`.
    pub fn ref_row_seeds(&self) -> Vec<RefRowSeed> {
        self.refs.row_seeds(&self.pack.names)
    }

    /// Adopt the build's macro delegation edges (`RefTable::delegations`)
    /// and re-index, so every delegated use sits in its delegates' buckets.
    pub fn adopt_macro_delegations(&mut self, delegations: BTreeMap<String, Box<[String]>>) {
        if delegations.is_empty() {
            return;
        }
        self.refs.set_delegations(delegations);
        self.refs.rebuild_indices(&self.pack.names);
    }

    /// The names ref `ri` reaches through macro delegation.
    pub fn ref_delegates(&self, ri: usize) -> &[String] {
        self.refs.delegates_at(ri, &self.pack.names)
    }
}
