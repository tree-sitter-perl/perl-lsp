//! What a caller join reads from a caller file, cut out of its analysis
//! while the analysis is whole: for one call name, the file's call sites of
//! that name and the part of its bag their receivers and arguments reach
//! through local edges. The join runs the same registry query over the slice
//! that it would run over the whole bag; only witnesses those call sites do
//! not reach are left out.
//!
//! An attachment the query MINTS from a value instead of reading an edge
//! (`PackageSymbol{class, m}` for a receiver typed cross-file, `SlotType`)
//! can fall outside the cut. The cut bag remembers every attachment its
//! source held, so such a read is recorded as a miss and the reader widens
//! the cut and asks again (`take_cut_misses`). `PERL_LSP_SLICE_EQUIV=1`
//! also answers over the whole bag and counts disagreements.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{CallArg, Callee, Witness, WitnessAttachment, WitnessBag, WitnessPayload};
use crate::model::file_analysis::{FileAnalysis, PackageFacts, RefKind, Scope};

/// The per-file context every slice of a file shares.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CallerFrame {
    pub scopes: Vec<Scope>,
    pub packages: HashMap<String, PackageFacts>,
    pub app_surface_consumers: Vec<String>,
    /// `attachment_hash` of every attachment the file's bag holds.
    #[serde(with = "shared_set")]
    pub held: Arc<HashSet<u64>>,
}

mod shared_set {
    use std::collections::HashSet;
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Arc<HashSet<u64>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<HashSet<u64>>, D::Error> {
        HashSet::<u64>::deserialize(d).map(Arc::new)
    }
}

impl CallerFrame {
    pub fn of(fa: &FileAnalysis) -> CallerFrame {
        CallerFrame {
            scopes: fa.scopes.clone(),
            packages: fa.packages.clone(),
            app_surface_consumers: fa.plugin.app_surface_consumers.clone(),
            held: Arc::new(fa.witnesses.attachments().map(super::attachment_hash).collect()),
        }
    }

    pub fn heap_bytes(&self) -> usize {
        self.scopes.len() * std::mem::size_of::<Scope>() + self.packages.len() * 256 + self.held.len() * 16
    }
}

/// A slice with the frame it reads under.
#[derive(Debug, Clone)]
pub struct CallerRead {
    pub frame: Arc<CallerFrame>,
    pub slice: Arc<CallerSlice>,
}

impl CallerRead {
    /// Cut from `fa` directly, keeping nothing.
    pub fn cut(fa: &FileAnalysis, key: &str) -> CallerRead {
        let frame = CallerFrame::of(fa);
        let slice = CallerSlice::of(fa, &frame, key, Vec::new());
        CallerRead { frame: Arc::new(frame), slice: Arc::new(slice) }
    }

    pub fn widened(&self, fa: &FileAnalysis, key: &str, misses: Vec<WitnessAttachment>) -> CallerRead {
        CallerRead {
            frame: Arc::clone(&self.frame),
            slice: Arc::new(CallerSlice::widened(fa, &self.frame, key, &self.slice, misses)),
        }
    }

    /// The equivalence net's control: the whole bag.
    pub fn whole(fa: &FileAnalysis, key: &str) -> CallerRead {
        CallerRead { frame: Arc::new(CallerFrame::of(fa)), slice: Arc::new(CallerSlice::whole(fa, key)) }
    }
}

/// One call name's slice of a file.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CallerSlice {
    /// The call refs of the name: (ref index, is a method call).
    pub calls: Vec<(u32, bool)>,
    pub witnesses: WitnessBag,
    /// Attachments a query minted that the call sites' edges do not reach,
    /// found by an earlier read and cut in since.
    pub extra_roots: Vec<WitnessAttachment>,
}

impl CallerSlice {
    /// The slice of `fa` (refs and bag present) for calls keyed `key`.
    pub fn of(fa: &FileAnalysis, frame: &CallerFrame, key: &str, extra_roots: Vec<WitnessAttachment>) -> CallerSlice {
        let refs = fa.refs();
        let calls: Vec<(u32, bool)> = fa
            .refs_table()
            .by_key(key)
            .iter()
            .filter_map(|&ri| match refs[ri].kind {
                RefKind::MethodCall { .. } => Some((ri as u32, true)),
                RefKind::FunctionCall { .. } => Some((ri as u32, false)),
                _ => None,
            })
            .collect();
        let mut roots: Vec<WitnessAttachment> =
            calls.iter().map(|&(ri, _)| WitnessAttachment::Expression(super::RefIdx(ri))).collect();
        roots.extend(extra_roots.iter().cloned());
        let mut witnesses = reachable(&fa.witnesses, &fa.scopes, roots);
        witnesses.mark_cut(Arc::clone(&frame.held));
        CallerSlice { calls, witnesses, extra_roots }
    }

    /// Does the slice hold a plain (non-method) call of its name?
    pub fn has_plain_call(&self) -> bool {
        self.calls.iter().any(|&(_, method)| !method)
    }

    /// The slice of a file with no call of the name.
    pub fn empty(frame: &CallerFrame) -> CallerSlice {
        let mut s = CallerSlice::default();
        s.attach(frame);
        s
    }

    /// Every call name's slice of `fa`, keyed by `match_key`: what the
    /// indexer cuts while the whole analysis is still in hand.
    pub fn all_of(fa: &FileAnalysis, frame: &CallerFrame) -> Vec<(String, CallerSlice)> {
        let refs = fa.refs();
        fa.refs_table()
            .keyed()
            .filter(|(_, ixs)| {
                ixs.iter().any(|&ri| matches!(refs[ri].kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. }))
            })
            .map(|(key, _)| (key.clone(), Self::of(fa, frame, key, Vec::new())))
            .collect()
    }

    /// `from`, cut again from `fa` to also hold `misses`.
    pub fn widened(
        fa: &FileAnalysis,
        frame: &CallerFrame,
        key: &str,
        from: &CallerSlice,
        misses: Vec<WitnessAttachment>,
    ) -> CallerSlice {
        let mut extra = from.extra_roots.clone();
        for m in misses {
            if !extra.contains(&m) {
                extra.push(m);
            }
        }
        Self::of(fa, frame, key, extra)
    }

    /// The same shape over the whole bag: the equivalence net's control.
    pub fn whole(fa: &FileAnalysis, key: &str) -> CallerSlice {
        let frame = CallerFrame::of(fa);
        let mut s = Self::of(fa, &frame, key, Vec::new());
        s.witnesses = fa.witnesses.clone();
        s
    }

    /// Reattach the frame's held set after a decode (it is not serialized
    /// with each slice).
    pub fn attach(&mut self, frame: &CallerFrame) {
        self.witnesses.mark_cut(Arc::clone(&frame.held));
    }

    pub fn heap_bytes(&self) -> usize {
        let (v, i) = self.witnesses.heap_bytes_estimate();
        v + i + self.calls.capacity() * 8 + 64
    }
}

/// Every witness on an attachment `roots` reach through the edges their
/// payloads name. A variable is read through its scope chain, so reaching
/// it in one scope reaches it in that scope's ancestors.
fn reachable(bag: &WitnessBag, scopes: &[Scope], roots: Vec<WitnessAttachment>) -> WitnessBag {
    let parent: HashMap<_, _> = scopes.iter().map(|s| (s.id, s.parent)).collect();
    let mut seen: HashSet<WitnessAttachment> = HashSet::new();
    let mut work = roots;
    let mut out = WitnessBag::new();
    while let Some(att) = work.pop() {
        if !seen.insert(att.clone()) {
            continue;
        }
        if let WitnessAttachment::Variable { name, scope } = &att {
            if let Some(Some(up)) = parent.get(scope) {
                work.push(WitnessAttachment::Variable { name: name.clone(), scope: *up });
            }
        }
        for w in bag.iter_attachment(&att) {
            payload_targets(w, &mut work);
            out.push(w.clone());
        }
    }
    out
}

fn payload_targets(w: &Witness, out: &mut Vec<WitnessAttachment>) {
    match &w.payload {
        WitnessPayload::Edge(a) => out.push(a.clone()),
        WitnessPayload::Projected { base, .. } => out.push(base.clone()),
        WitnessPayload::Tuple(v) => out.extend(v.iter().cloned()),
        WitnessPayload::Invoke { callee, receiver, args } => {
            if let Callee::Value(a) = callee {
                out.push(a.clone());
            }
            out.extend(receiver.iter().cloned());
            for arg in args.iter().flatten() {
                match arg {
                    CallArg::One(a) | CallArg::Spread(a) | CallArg::Word { at: a, .. } => out.push(a.clone()),
                    CallArg::Forward { .. } => {}
                }
            }
        }
        WitnessPayload::ReturnExpr(re) => {
            re.map_of(&mut |a| {
                out.push(a.clone());
                None
            });
        }
        WitnessPayload::InferredType(_)
        | WitnessPayload::Observation(_)
        | WitnessPayload::Fact { .. }
        | WitnessPayload::Derivation
        | WitnessPayload::Custom { .. }
        | WitnessPayload::DomainCompare { .. }
        | WitnessPayload::Reset
        | WitnessPayload::BrandOverlay { .. } => {}
    }
}
