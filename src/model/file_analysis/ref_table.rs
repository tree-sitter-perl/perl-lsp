//! The reference table: every ref this file records, together with the
//! indices derived from them, their eviction axis, and their enrichment
//! baseline.
//!
//! Refs and their indices are one axis, so they live in one owner: an
//! `evict()` that forgot an index, or a rebuild that missed one, is not
//! expressible from outside. `FileAnalysis` holds exactly one of these and
//! delegates its ref-shaped queries to it.

use super::*;

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

    #[serde(skip, default)]
    by_name: HashMap<String, Vec<usize>>,

    /// Refs indexed by the SymbolId they resolve to — "refs to symbol X"
    /// is an O(1) lookup.
    #[serde(skip, default)]
    by_target: HashMap<SymbolId, Vec<usize>>,

    /// Start-point → call-shaped ref index, used by
    /// `method_call_invocant_class` to chase a chain receiver:
    /// `Foo->new->m`'s outer `->m` `invocant_span` starts at the inner
    /// `Foo->new` call's start; `make_b()->touch()`'s outer
    /// `invocant_span` starts at `make_b`'s start. Only MethodCall and
    /// FunctionCall refs go in — the receiver dispatch is keyed on those
    /// two kinds.
    #[serde(skip, default)]
    call_by_start: HashMap<Point, usize>,
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

    /// Ref indexes whose `target_name` is `name`.
    pub fn by_name(&self, name: &str) -> Option<&Vec<usize>> {
        self.by_name.get(name)
    }

    /// Ref indexes resolving to `sym_id`.
    pub fn to_symbol(&self, sym_id: SymbolId) -> &[usize] {
        self.by_target.get(&sym_id).map_or(&[], |v| v.as_slice())
    }

    /// The call-shaped ref anchored at `start` — the chain-receiver hop.
    pub fn call_at_start(&self, start: &Point) -> Option<usize> {
        self.call_by_start.get(start).copied()
    }

    /// Every `(anchor point, ref index)` the call index holds — the shape
    /// pins read it to assert the tiebreak and that only call-shaped kinds
    /// are indexed.
    pub fn call_index_entries(&self) -> impl Iterator<Item = (Point, usize)> + '_ {
        self.call_by_start.iter().map(|(&p, &i)| (p, i))
    }

    /// Every ref's relational row seed, in ref order.
    pub fn row_seeds(&self) -> Vec<RefRowSeed> {
        self.refs.iter().map(Ref::row_seed).collect()
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
    pub fn rebuild_indices(&mut self) {
        self.by_name.clear();
        self.by_target.clear();
        self.call_by_start.clear();
        for (i, r) in self.refs.iter().enumerate() {
            self.by_name
                .entry(r.target_name.clone())
                .or_default()
                .push(i);
            if let Some(sym_id) = r.resolved_symbol() {
                self.by_target.entry(sym_id).or_default().push(i);
            }
            if matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. }) {
                // Smaller span (closer to the actual receiver) wins; a tie
                // keeps the earlier insertion. Method-call refs are visited
                // outer-first, so for a chain like `Foo->new->m` the outer
                // `m` and inner `Foo->new` share a start point — keeping the
                // smaller-span ref points the index at the inner receiver.
                // FunctionCall refs (just the function-name span) are
                // naturally narrower than the enclosing MethodCall, so they
                // win the same way.
                let take = match self.call_by_start.get(&r.span.start).copied() {
                    None => true,
                    Some(prev) => {
                        let prev_span = self.refs[prev].span;
                        (r.span.end.row, r.span.end.column)
                            < (prev_span.end.row, prev_span.end.column)
                    }
                };
                if take {
                    self.call_by_start.insert(r.span.start, i);
                }
            }
        }
    }

    /// Refresh the name/target lookups only — the enrichment refresh.
    /// `call_by_start` is left alone on purpose: every entry it holds
    /// points into the baseline prefix (it is built before the baseline is
    /// sealed, and no post-build pass mints call refs), and enrichment
    /// restores that prefix verbatim before appending only synthetic
    /// key refs.
    pub fn refresh_name_target_indices(&mut self) {
        self.by_name.clear();
        self.by_target.clear();
        for (i, r) in self.refs.iter().enumerate() {
            self.by_name
                .entry(r.target_name.clone())
                .or_default()
                .push(i);
            if let Some(sym_id) = r.resolved_symbol() {
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
        self.by_name = HashMap::new();
        self.by_target = HashMap::new();
        self.call_by_start = HashMap::new();
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
                .sum::<usize>();

        let mut b = mcap(&self.by_target) + mcap(&self.call_by_start) + mcap(&self.by_name);
        for (k, v) in &self.by_name {
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
        self.refs.rebuild_indices();
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
        self.refs.row_seeds()
    }
}
