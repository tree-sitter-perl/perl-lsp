//! Witness bag + reducers — the type-inference engine.
//!
//! A **witness** is typed evidence about a specific code location
//! (variable, expression, symbol, hash key...), tagged with its source
//! (which builder pass / plugin emitted it). A bag of witnesses is
//! folded into concrete answers by **reducers** — pure projections that
//! claim the witnesses they care about.
//!
//! A few API surfaces carry `#[allow(dead_code)]` (`WitnessBag::all`,
//! `filter`, `is_empty`; `ReducedValue::FactMap`; `WitnessReducer::name`):
//! they're the bag's stable contract for plugins and future reducers,
//! held in the public surface deliberately rather than chased dead.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::model::file_analysis::{
    HashKeyOwner, InferredType, ParametricType, Scope, ScopeId, Span, SymbolId,
};

use tree_sitter::Point;

pub mod tags;

mod types;
pub use types::*;
mod reducers;
pub use reducers::*;
pub(crate) mod registry;
pub use registry::*;
mod mutation_extension;
pub(crate) use mutation_extension::*;
mod query;
pub use query::*;
mod session;
pub use session::*;
mod conclusions;
pub use conclusions::*;
mod closedness;
pub use closedness::*;

// ---- Witness bag ----

/// Attachment-indexed bag. Kept separate from the raw witness vec so
/// callers can iterate all witnesses for one attachment without scanning.
///
/// `index` is `serde(skip)` (cheap to recompute, redundant on disk). The
/// custom `Deserialize` impl below rebuilds it — without that, every
/// consumer that loads a `FileAnalysis` from bincode (SQLite cache,
/// dump-package, cross-file enrichment) would have reducers claim empty
/// witness slices and the bag would silently return `None`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct WitnessBag {
    witnesses: Vec<Witness>,
    #[serde(skip)]
    index: HashMap<WitnessAttachment, Vec<usize>>,
    /// Every scope a variable name has witnesses on: the one variable's
    /// facts, wherever its writes were made.
    #[serde(skip)]
    var_scopes: HashMap<String, Vec<crate::model::file_analysis::ScopeId>>,
    /// Each temporal attachment's witnesses in source order, so a read at a
    /// point finds the writes in force there without walking the rest
    /// (`live_at`). Only for an attachment of `SPAN_ORDER_MIN` witnesses or
    /// more: below that the whole read is as cheap as the window, and the
    /// order would be memory every small variable pays.
    #[serde(skip)]
    spans: HashMap<WitnessAttachment, SpanOrder>,
    /// Names this bag's content: every mutation draws a fresh value from
    /// [`next_generation`], so a memo that outlives one query can tell
    /// whether the bag it answered from is still the bag being asked.
    #[serde(skip)]
    generation: u64,
    /// While the fold tracks it, the sum of every witness's hash: kept as
    /// witnesses come and go, so the fixed-point check reads it in O(1).
    #[serde(skip)]
    content: Option<u64>,
}

/// One process-wide counter for every generation stamp, so two stamped
/// stores never share a value unless their content is the same.
pub(crate) fn next_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One witness's hash, over its serialized form (the payload types carry
/// no `Hash`).
fn witness_hash(w: &Witness) -> u64 {
    use std::hash::Hasher;
    struct HashWriter(std::collections::hash_map::DefaultHasher);
    impl std::io::Write for HashWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = HashWriter(std::collections::hash_map::DefaultHasher::new());
    let _ = bincode::serialize_into(&mut h, w);
    h.0.finish()
}

/// Does `w` hold at `point` of its attachment's lifetime? A zero-width
/// fact is a point fact that stands from its site on; a region (a fact made
/// in a nested block) holds only inside its span.
pub(crate) fn reaches(w: &Witness, point: Point) -> bool {
    w.span.start == w.span.end || (w.span.start <= point && point <= w.span.end)
}

const SPAN_ORDER_MIN: usize = 16;

/// One temporal attachment's witnesses ordered by source position, for
/// `WitnessBag::live_at`. Bag indices throughout.
#[derive(Debug, Default, Clone)]
struct SpanOrder {
    /// Windowed witnesses by `(span.start, bag index)`.
    windowed: Vec<usize>,
    /// The write markers, by the same order.
    resets: Vec<usize>,
    /// Witnesses whose payload is not windowed: always read.
    kept: Vec<usize>,
    /// Point facts, which every read holds wherever it is asked.
    zero_width: usize,
    /// A witness the order can't reason about: a ranged write marker (a
    /// write is a point) or a delta the extension lowering reads whole.
    opaque: bool,
}

impl SpanOrder {
    /// Count the witness at `idx` in, at the end of its lists.
    fn note(&mut self, w: &Witness, idx: usize) {
        if w.span.start == w.span.end {
            self.zero_width += 1;
        }
        match &w.payload {
            WitnessPayload::Extend(_) => self.opaque = true,
            WitnessPayload::Reset if w.span.start != w.span.end => self.opaque = true,
            _ => {}
        }
        if matches!(w.payload, WitnessPayload::Reset) {
            self.resets.push(idx);
        }
        if w.payload.is_windowed() {
            self.windowed.push(idx);
        } else {
            self.kept.push(idx);
        }
    }

    /// `note` for the witness just pushed at `idx`, moved back to its place:
    /// witnesses mostly land in source order, so the move is short.
    fn file(&mut self, ws: &[Witness], idx: usize) {
        self.note(&ws[idx], idx);
        let at = ws[idx].span.start;
        for v in [&mut self.windowed, &mut self.resets] {
            if v.last() != Some(&idx) {
                continue;
            }
            let mut i = v.len() - 1;
            while i > 0 && ws[v[i - 1]].span.start > at {
                v.swap(i - 1, i);
                i -= 1;
            }
        }
    }

    /// The order over `members`, an attachment's witnesses in bag order.
    fn of(ws: &[Witness], members: &[usize]) -> Self {
        let mut order = SpanOrder::default();
        for &i in members {
            order.note(&ws[i], i);
        }
        order.windowed.sort_by_key(|&i| (ws[i].span.start, i));
        order.resets.sort_by_key(|&i| (ws[i].span.start, i));
        order
    }

    fn live_at<'b>(&self, ws: &'b [Witness], point: Point) -> Option<Vec<&'b Witness>> {
        // With fewer than two point facts the full read can hold a sole
        // witness, and only the caller can count it.
        if self.opaque || self.zero_width < 2 {
            return None;
        }
        let start = |i: usize| ws[i].span.start;
        let upto = self.resets.partition_point(|&i| start(i) <= point);
        let latest = upto.checked_sub(1).map(|k| start(self.resets[k]));
        // The earliest write, kept when the latest one retires what precedes
        // it: it is what says something was bound before.
        let first = self.resets.first().copied().filter(|&i| latest.is_some_and(|l| start(i) < l));
        let lo = match (first, latest) {
            (Some(_), Some(l)) => self.windowed.partition_point(|&i| start(i) < l),
            _ => 0,
        };
        let hi = self.windowed.partition_point(|&i| start(i) <= point);
        let mut picked: Vec<usize> = self.windowed[lo..hi].to_vec();
        picked.extend(first);
        picked.extend(self.kept.iter().copied());
        picked.retain(|&i| reaches(&ws[i], point));
        picked.sort_unstable();
        Some(picked.into_iter().map(|i| &ws[i]).collect())
    }

    fn heap_bytes(&self) -> usize {
        (self.windowed.capacity() + self.resets.capacity() + self.kept.capacity()) * std::mem::size_of::<usize>()
    }
}

impl<'de> Deserialize<'de> for WitnessBag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Helper mirrors the on-disk shape (just the witness vec; `index`
        // is recomputed). The derived impl would expect both fields.
        #[derive(Deserialize)]
        struct WitnessBagOnDisk {
            witnesses: Vec<Witness>,
        }
        let on_disk = WitnessBagOnDisk::deserialize(deserializer)?;
        let mut bag = WitnessBag {
            witnesses: on_disk.witnesses,
            index: HashMap::new(),
            var_scopes: HashMap::new(),
            spans: HashMap::new(),
            generation: 0,
            content: None,
        };
        bag.rebuild_index();
        Ok(bag)
    }
}

impl WitnessBag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rough resident-byte estimate `(witness_vec, rebuilt_index)` for the
    /// memory-composition probe (`docs/adr/memory-slice-2-lru.md`). Flat
    /// `size_of` of each element's inline footprint plus the `Vec`/`HashMap`
    /// backing-store capacity; deep `String`s inside attachments/payloads are
    /// NOT drilled (a modest undercount, called out in the ADR methodology).
    /// Not on any query path — a diagnostic only.
    pub fn heap_bytes_estimate(&self) -> (usize, usize) {
        let vec_bytes = self.witnesses.capacity() * std::mem::size_of::<Witness>();
        let idx_entry = std::mem::size_of::<(WitnessAttachment, Vec<usize>)>() + 1;
        let mut idx_bytes = self.index.capacity() * idx_entry;
        for v in self.index.values() {
            idx_bytes += v.capacity() * std::mem::size_of::<usize>();
        }
        let span_entry = std::mem::size_of::<(WitnessAttachment, SpanOrder)>() + 1;
        idx_bytes += self.spans.capacity() * span_entry;
        for o in self.spans.values() {
            idx_bytes += o.heap_bytes();
        }
        (vec_bytes, idx_bytes)
    }

    /// Start keeping a fingerprint of the bag's content: equal fingerprints
    /// mean the same witnesses (as a multiset). The fold's fixed-point check
    /// reads it, since a clear-and-emit pass can replace a witness with a
    /// different one and leave the length where it was.
    pub fn track_content(&mut self) {
        self.content = Some(self.witnesses.iter().fold(0u64, |acc, w| acc.wrapping_add(witness_hash(w))));
    }

    pub fn untrack_content(&mut self) {
        self.content = None;
    }

    /// The fingerprint `track_content` keeps; computed on the spot when the
    /// bag isn't tracking.
    pub fn content_fingerprint(&self) -> u64 {
        let whole = || self.witnesses.iter().fold(0u64, |acc, w| acc.wrapping_add(witness_hash(w)));
        debug_assert!(self.content.is_none_or(|c| c == whole()), "content fingerprint fell out of step");
        self.content.unwrap_or_else(whole)
    }

    /// `Vec::retain` that keeps the content fingerprint in step.
    fn retain_witnesses(&mut self, keep: impl Fn(&Witness) -> bool) {
        let mut gone = 0u64;
        let tracking = self.content.is_some();
        self.witnesses.retain(|w| {
            let k = keep(w);
            if !k && tracking {
                gone = gone.wrapping_add(witness_hash(w));
            }
            k
        });
        if let Some(c) = &mut self.content {
            *c = c.wrapping_sub(gone);
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn push(&mut self, w: Witness) -> usize {
        self.generation = next_generation();
        let idx = self.witnesses.len();
        let slot = self.index.entry(w.attachment.clone()).or_default();
        if slot.is_empty() {
            if let WitnessAttachment::Variable { name, scope } = &w.attachment {
                self.var_scopes.entry(name.clone()).or_default().push(*scope);
            }
        }
        slot.push(idx);
        let ordered = (w.attachment.is_temporal() && slot.len() >= SPAN_ORDER_MIN).then(|| w.attachment.clone());
        if let Some(c) = &mut self.content {
            *c = c.wrapping_add(witness_hash(&w));
        }
        self.witnesses.push(w);
        if let Some(att) = ordered {
            match self.spans.get_mut(&att) {
                Some(order) => order.file(&self.witnesses, idx),
                None => {
                    let order = SpanOrder::of(&self.witnesses, &self.index[&att]);
                    self.spans.insert(att, order);
                }
            }
        }
        idx
    }

    #[allow(dead_code)]
    pub fn all(&self) -> &[Witness] {
        &self.witnesses
    }

    /// Every attachment the bag holds witnesses for — the enumeration the
    /// conclusion bake walks. Reading the index rather than scanning the
    /// witness vec is what makes "absent means None" checkable: the index IS
    /// the set of keys the bag could answer.
    pub fn attachments(&self) -> impl Iterator<Item = &WitnessAttachment> {
        self.index.keys()
    }

    pub fn for_attachment(&self, att: &WitnessAttachment) -> Vec<&Witness> {
        self.index
            .get(att)
            .map(|ixs| ixs.iter().map(|&i| &self.witnesses[i]).collect())
            .unwrap_or_default()
    }

    /// [`iter_attachment`](Self::iter_attachment) with each witness's push index.
    pub fn iter_attachment_indexed<'b>(&'b self, att: &WitnessAttachment) -> impl Iterator<Item = (usize, &'b Witness)> + 'b {
        self.index.get(att).into_iter().flatten().map(|&i| (i, &self.witnesses[i]))
    }

    /// The scopes `name` has witnesses on.
    pub fn variable_scopes(&self, name: &str) -> &[crate::model::file_analysis::ScopeId] {
        self.var_scopes.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// [`for_attachment`](Self::for_attachment) without the allocation, for
    /// a per-hop check that usually finds nothing.
    pub fn iter_attachment<'b>(&'b self, att: &WitnessAttachment) -> impl Iterator<Item = &'b Witness> + 'b {
        self.index.get(att).into_iter().flatten().map(|&i| &self.witnesses[i])
    }

    /// Iterate witnesses matching a predicate. O(n).
    #[allow(dead_code)]
    pub fn filter<P: Fn(&Witness) -> bool>(&self, pred: P) -> Vec<&Witness> {
        self.witnesses.iter().filter(|w| pred(w)).collect()
    }

    pub fn rebuild_index(&mut self) {
        // Every removal path rebuilds, so this is where they all mutate.
        self.generation = next_generation();
        let _t = crate::util::ghost_stats::ScopedNs::start("bag::rebuild_index");
        crate::util::ghost_stats::count_by("bag.rebuild_index_witnesses", self.witnesses.len() as u64);
        self.index.clear();
        self.var_scopes.clear();
        self.spans.clear();
        for (i, w) in self.witnesses.iter().enumerate() {
            let slot = self.index.entry(w.attachment.clone()).or_default();
            if slot.is_empty() {
                if let WitnessAttachment::Variable { name, scope } = &w.attachment {
                    self.var_scopes.entry(name.clone()).or_default().push(*scope);
                }
            }
            slot.push(i);
        }
        for (att, members) in &self.index {
            if att.is_temporal() && members.len() >= SPAN_ORDER_MIN {
                self.spans.insert(att.clone(), SpanOrder::of(&self.witnesses, members));
            }
        }
    }

    /// The witnesses of temporal `att` that can bear on a read at `point`,
    /// in bag order, already narrowed to those reaching `point`
    /// (`reaches`) — or `None` when the order can't prove the rest inert
    /// and the caller must read them all.
    ///
    /// A read at `point` sees the latest write at or before it; a windowed
    /// payload (`WitnessPayload::is_windowed`) before that write is retired
    /// once an earlier write already answers "was anything bound before",
    /// and one after `point` is not there yet. Every other payload is kept
    /// wherever it sits. It answers only for an attachment with two point
    /// facts, so the full read it stands for never holds a sole witness.
    pub(crate) fn live_at(&self, att: &WitnessAttachment, point: Point) -> Option<Vec<&Witness>> {
        self.spans.get(att)?.live_at(&self.witnesses, point)
    }

    /// Rewrite every witness in place, keeping push order.
    pub fn rewrite(&mut self, mut f: impl FnMut(&mut Witness)) {
        self.witnesses.iter_mut().for_each(&mut f);
        if self.content.is_some() {
            self.track_content();
        }
        self.rebuild_index();
    }

    /// Drop every witness past `baseline` and rebuild the index. Lets
    /// enrichment revert post-build additions before re-deriving them,
    /// keeping the bag idempotent across repeat enrichment calls.
    pub fn truncate(&mut self, baseline: usize) {
        if baseline >= self.witnesses.len() {
            return;
        }
        if let Some(c) = &mut self.content {
            let gone = self.witnesses[baseline..].iter().fold(0u64, |acc, w| acc.wrapping_add(witness_hash(w)));
            *c = c.wrapping_sub(gone);
        }
        self.witnesses.truncate(baseline);
        self.rebuild_index();
    }

    /// Does an explicitly DECLARED fact (`WitnessSource::Annotation`) sit on
    /// `att`? The inlay-hint suppression for languages with explicit types
    /// asks this: `int c` needs no `: int` hint, `auto x` does.
    pub fn has_annotation(&self, att: &WitnessAttachment) -> bool {
        self.index.get(att).is_some_and(|idxs| {
            idxs.iter().any(|&i| matches!(&self.witnesses[i].source, WitnessSource::Annotation(_)))
        })
    }

    /// Drop every `Builder(tag)`-sourced witness and rebuild the index;
    /// returns the count removed. Re-emittable builder passes call this
    /// at the start of each fold iteration so the bag stays
    /// duplicate-free no matter how many times the fold runs.
    pub fn remove_by_source_tag(&mut self, tag: &str) -> usize {
        let _t = crate::util::ghost_stats::ScopedNs::start("bag::remove_tag");
        let before = self.witnesses.len();
        self.retain_witnesses(|w| match &w.source {
            WitnessSource::Builder(s) => s != tag,
            _ => true,
        });
        let removed = before - self.witnesses.len();
        if removed > 0 {
            self.rebuild_index();
        }
        removed
    }

    /// Drop `Builder(tag)`-sourced witnesses on ONE attachment. Targeted
    /// sibling of `remove_by_source_tag`: an arity-discriminated sub retracts
    /// its own non-arity `return_arm_chain` fallback (so the arity union is
    /// the authoritative answer) without disturbing any other symbol's
    /// witnesses.
    pub fn remove_attachment_source(
        &mut self,
        att: &WitnessAttachment,
        tag: &str,
    ) -> usize {
        let before = self.witnesses.len();
        self.retain_witnesses(|w| {
            !(&w.attachment == att
                && matches!(&w.source, WitnessSource::Builder(s) if s == tag))
        });
        let removed = before - self.witnesses.len();
        if removed > 0 {
            self.rebuild_index();
        }
        removed
    }

    /// Drop `Builder(tag)`-sourced witnesses at a SET of binding sites —
    /// each (attachment, span.start) pair identifies one binding, so a
    /// binding can refine its published type without touching a sibling
    /// binding of the same variable at another site. One retain + at most
    /// one index rebuild for the whole set: a per-site form costs a full bag
    /// scan AND an index rebuild per call, O(N * bag) for N bindings — 20+
    /// seconds of a 46k-line-file fold.
    pub fn remove_attachment_sources_at(
        &mut self,
        items: &[(WitnessAttachment, Point)],
        tag: &str,
    ) -> usize {
        if items.is_empty() {
            return 0;
        }
        let _t = crate::util::ghost_stats::ScopedNs::start("bag::remove_at_batch");
        let keys: std::collections::HashSet<(&WitnessAttachment, Point)> =
            items.iter().map(|(a, p)| (a, *p)).collect();
        let before = self.witnesses.len();
        self.retain_witnesses(|w| {
            !(matches!(&w.source, WitnessSource::Builder(s) if s == tag)
                && keys.contains(&(&w.attachment, w.span.start)))
        });
        let removed = before - self.witnesses.len();
        if removed > 0 {
            self.rebuild_index();
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.witnesses.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.witnesses.is_empty()
    }
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[cfg(test)]
#[path = "witnesses_tests.rs"]
mod tests;

#[cfg(test)]
mod live_window_tests;
