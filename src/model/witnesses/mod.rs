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

mod types;
pub use types::*;
mod reducers;
pub use reducers::*;
pub(crate) mod registry;
pub use registry::*;
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
        (vec_bytes, idx_bytes)
    }

    pub fn push(&mut self, w: Witness) -> usize {
        let idx = self.witnesses.len();
        self.index.entry(w.attachment.clone()).or_default().push(idx);
        self.witnesses.push(w);
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

    /// Iterate witnesses matching a predicate. O(n).
    #[allow(dead_code)]
    pub fn filter<P: Fn(&Witness) -> bool>(&self, pred: P) -> Vec<&Witness> {
        self.witnesses.iter().filter(|w| pred(w)).collect()
    }

    pub fn rebuild_index(&mut self) {
        let _t = crate::util::ghost_stats::ScopedNs::start("bag::rebuild_index");
        crate::util::ghost_stats::count_by("bag.rebuild_index_witnesses", self.witnesses.len() as u64);
        self.index.clear();
        for (i, w) in self.witnesses.iter().enumerate() {
            self.index.entry(w.attachment.clone()).or_default().push(i);
        }
    }

    /// Drop every witness past `baseline` and rebuild the index. Lets
    /// enrichment revert post-build additions before re-deriving them,
    /// keeping the bag idempotent across repeat enrichment calls.
    pub fn truncate(&mut self, baseline: usize) {
        if baseline >= self.witnesses.len() {
            return;
        }
        self.witnesses.truncate(baseline);
        self.rebuild_index();
    }

    /// Does `att` carry a witness sourced from `Builder(tag)`? Used to ask
    /// "was this variable's type written EXPLICITLY" (`skeleton-annot`) vs
    /// inferred — the inlay-hint suppression for languages with explicit
    /// types (`int c` needs no `: int` hint; `auto x` does).
    pub fn has_builder_source(&self, att: &WitnessAttachment, tag: &str) -> bool {
        self.index.get(att).is_some_and(|idxs| {
            idxs.iter().any(|&i| {
                matches!(&self.witnesses[i].source, WitnessSource::Builder(s) if s == tag)
            })
        })
    }

    /// Drop every `Builder(tag)`-sourced witness and rebuild the index;
    /// returns the count removed. Re-emittable builder passes call this
    /// at the start of each fold iteration so the bag stays
    /// duplicate-free no matter how many times the fold runs.
    pub fn remove_by_source_tag(&mut self, tag: &str) -> usize {
        let _t = crate::util::ghost_stats::ScopedNs::start("bag::remove_tag");
        let before = self.witnesses.len();
        self.witnesses.retain(|w| match &w.source {
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
        self.witnesses.retain(|w| {
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
        self.witnesses.retain(|w| {
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
