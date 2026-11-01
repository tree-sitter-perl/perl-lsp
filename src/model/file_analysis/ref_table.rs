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
/// Serialized fields are the refs themselves, the enrichment baseline and
/// the identities the refs name; the indices are `serde(skip)` and rebuilt
/// after load, and the
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

    /// Every class or package identity the build-time refs name that the
    /// file does not declare, sorted: the reference axis's share of the
    /// dependency tier's demand (`FileAnalysis::used_identities`). Kept
    /// through `evict`, because demand is read off stripped copies and warm
    /// stubs; re-minted with the refs, never beside them.
    #[serde(default)]
    named_identities: Vec<String>,

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
    /// `(start, end, index)`: the call spanning exactly an expression, by
    /// binary search.
    #[serde(skip, default)]
    calls_by_span: Vec<usize>,
    /// Member refs spanning only their member token (the pack shape; a
    /// Perl MethodCall spans the whole call) sorted by `(invocant start,
    /// index)`: the member-chain hop, found by where its receiver opens.
    #[serde(skip, default)]
    member_hops: Vec<usize>,
    /// Most calls sharing one start point: a fluent chain's calls all open
    /// where its first receiver does, so this is the file's deepest chain.
    #[serde(skip, default)]
    longest_call_chain: usize,
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

    /// Calls in the file's deepest fluent chain (`a->b->c` is 2, the
    /// receiver `a` is no call).
    /// Call-shaped refs opening at `start`, sorted by `(end, index)`. A
    /// query-time lookup by position reads an index like this one: a scan
    /// of every ref per query is quadratic per file, since expression
    /// typing asks once per receiver hop.
    pub fn calls_starting_at(&self, start: Point) -> &[usize] {
        let lo = self.calls_by_span.partition_point(|&i| self.refs[i].span.start < start);
        let hi = self.calls_by_span.partition_point(|&i| self.refs[i].span.start <= start);
        &self.calls_by_span[lo..hi]
    }

    /// Member-token refs whose invocant opens at `start`, in ref order.
    pub fn member_hops_at(&self, start: Point) -> &[usize] {
        let lo = self.member_hops.partition_point(|&i| member_hop_key(&self.refs[i]) < Some(start));
        let hi = self.member_hops.partition_point(|&i| member_hop_key(&self.refs[i]) <= Some(start));
        &self.member_hops[lo..hi]
    }

    pub fn longest_call_chain(&self) -> usize {
        self.longest_call_chain
    }

    /// The first call-shaped ref whose span is exactly `span`.
    pub fn call_spanning(&self, span: Span) -> Option<usize> {
        let key = |i: usize| (self.refs[i].span.start, self.refs[i].span.end);
        let at = self.calls_by_span.partition_point(|&i| key(i) < (span.start, span.end));
        self.calls_by_span.get(at).copied().filter(|&i| self.refs[i].span == span)
    }

    /// Every ref's relational row seed, in ref order.
    pub fn row_seeds(&self, names: &NameSpellings) -> Vec<RefRowSeed> {
        self.refs.iter().map(|r| r.row_seed(names)).collect()
    }

    /// Mint `named_identities` from the refs: each package reference's
    /// identity (`identity`, the file's spelling resolved), less what
    /// `declared` says the file declares itself.
    pub fn mint_named_identities(&mut self, identity: impl Fn(&Ref) -> String, declared: impl Fn(&str) -> bool) {
        let mut out: Vec<String> = self
            .refs
            .iter()
            .filter(|r| matches!(r.kind, RefKind::PackageRef))
            .map(identity)
            .filter(|id| !id.is_empty() && !declared(id))
            .collect();
        out.sort();
        out.dedup();
        self.named_identities = out;
    }

    /// The identities the refs name across files (`mint_named_identities`);
    /// present on an evicted table too.
    pub fn named_identities(&self) -> &[String] {
        &self.named_identities
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
    /// lookups and the call index.
    pub fn rebuild_indices(&mut self, names: &NameSpellings) {
        self.by_key.clear();
        self.by_target.clear();
        self.calls_by_span.clear();
        self.member_hops.clear();
        for (i, r) in self.refs.iter().enumerate() {
            self.by_key.entry(r.match_key(names)).or_default().push(i);
            if let Some(sym_id) = r.resolved_symbol() {
                self.by_target.entry(sym_id).or_default().push(i);
            }
            if matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. }) {
                self.calls_by_span.push(i);
            }
            if member_hop_key(r).is_some() {
                self.member_hops.push(i);
            }
        }
        let refs = &self.refs;
        self.calls_by_span.sort_by_key(|&i| (refs[i].span.start, refs[i].span.end, i));
        self.member_hops.sort_by_key(|&i| (member_hop_key(&refs[i]), i));
        self.longest_call_chain = self
            .calls_by_span
            .chunk_by(|&a, &b| refs[a].span.start == refs[b].span.start)
            .map(<[usize]>::len)
            .max()
            .unwrap_or(0);
    }

    /// Refresh the name/target lookups only — the enrichment refresh.
    /// `calls_by_span` and `member_hops` are left alone on purpose: every
    /// entry they hold points into the baseline prefix (they are built
    /// before the baseline is sealed, and no post-build pass mints call or
    /// member refs), and enrichment restores that prefix verbatim before
    /// appending only synthetic key refs.
    pub fn refresh_name_target_indices(&mut self, names: &NameSpellings) {
        debug_assert!(
            self.refs[self.base_count.min(self.refs.len())..].iter().all(|r| {
                !matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. })
                    && member_hop_key(r).is_none()
            }),
            "a ref appended above the baseline would be missing from the positional indices"
        );
        self.by_key.clear();
        self.by_target.clear();
        for (i, r) in self.refs.iter().enumerate() {
            self.by_key.entry(r.match_key(names)).or_default().push(i);
            if let Some(sym_id) = r.resolved_symbol() {
                self.by_target.entry(sym_id).or_default().push(i);
            }
        }
    }

    /// Strip the resident refs and every index over them — the refs
    /// eviction axis. Lossless: the on-disk analysis keeps the full vec,
    /// and the backward walk retrieves candidates from the relational
    /// index and rehydrates through `whole_present`. Idempotent.
    /// `named_identities` stays: it is what demand reads of a stripped copy.
    pub fn evict(&mut self) {
        self.refs = Vec::new();
        self.by_key = HashMap::new();
        self.by_target = HashMap::new();
        self.calls_by_span = Vec::new();
        self.member_hops = Vec::new();
        self.longest_call_chain = 0;
        self.evicted = true;
    }

    /// `r`'s position in this table, when `r` is one of its refs (and not a
    /// copy held elsewhere).
    pub fn index_of(&self, r: &Ref) -> Option<usize> {
        self.refs.as_ptr_range().contains(&(r as *const Ref)).then(|| {
            // In range, so the offset is a whole number of elements.
            (r as *const Ref as usize - self.refs.as_ptr() as usize) / std::mem::size_of::<Ref>()
        })
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
            + self.named_identities.capacity() * std::mem::size_of::<String>()
            + self.named_identities.iter().map(String::capacity).sum::<usize>();

        let mut b = mcap(&self.by_target)
            + mcap(&self.by_key)
            + (self.calls_by_span.capacity() + self.member_hops.capacity()) * std::mem::size_of::<usize>();
        for (k, v) in &self.by_key {
            b += k.capacity() + v.capacity() * std::mem::size_of::<usize>();
        }
        for v in self.by_target.values() {
            b += v.capacity() * std::mem::size_of::<usize>();
        }
        h.rebuilt_indices += b;
    }
}

/// Where a member-token ref's receiver opens: the `member_hops` key, `None`
/// for every other ref.
fn member_hop_key(r: &Ref) -> Option<Point> {
    let site = r.member_site()?;
    (site.name_span == r.span).then_some(site.invocant_span?.start)
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
}
