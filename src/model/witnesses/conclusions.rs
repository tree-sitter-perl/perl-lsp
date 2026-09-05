//! Conclusions: the registry chase partially evaluated over one file's bag.
//!
//! `docs/adr/conclusion-layer.md` owns the design. The short version: a
//! cross-file consult decodes a whole provider bag to answer one question, and
//! 78% of that cost is the chase rather than the fetch. A conclusion is that
//! chase run ONCE at persist time with the three query binders
//! (`ReducerQuery.point` / `.receiver` / `.arity_hint`) and the cross-file
//! world left free, so the answer can be served without the bag.
//!
//! Two rules the whole thing rests on:
//!
//! **Edges, not values.** A cross-file answer residualizes as `Link`, never as
//! a materialized type. Baking the value would freeze the world — the provider
//! it names can change without this file changing. The constraint is hops
//! cheaper, never fewer.
//!
//! **Absence is not an answer.** A key absent from the map means "the bag
//! deterministically answers None here"; a key present as `OpenNone` means
//! "unbakeable, go decode". Those are different, and collapsing them makes a
//! silent wrong answer out of a key the enumeration missed. `OpenNone` is the
//! degradation for everything uncertain — a depth cap, an unrecognized
//! payload, a truncated chase.

use super::registry::ReducerRegistry;
use super::reducers::{ReducedValue, ReducerQuery};
use super::types::{FrameworkFact, ReturnExpr, WitnessAttachment};
use super::WitnessBag;
use crate::model::file_analysis::InferredType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A portable key: strings only, so it can be looked up in a file that has
/// never heard of this file's `SymbolId`s or `RefIdx`es.
///
/// The internal attachments (`Symbol`, `Expression`, `Expr`, `Variable`,
/// `SymbolReturnArm`, `BranchArm`) are deliberately absent. They index into
/// one analysis's tables, so a cross-file consumer cannot resolve them, and
/// baking them would produce keys that only their own file can read — the
/// storage cost of a map with none of the benefit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConclusionKey {
    MethodOnClass { class: String, name: String },
    SubByName(String),
    SlotType { class: String, key: String },
    TypeName(String),
}

impl ConclusionKey {
    /// The portable projection of an attachment, or `None` when the
    /// attachment is file-internal.
    pub fn from_attachment(att: &WitnessAttachment) -> Option<Self> {
        match att {
            WitnessAttachment::PackageSymbol { package, name } => Some(Self::MethodOnClass {
                class: package.clone(),
                name: name.clone(),
            }),
            WitnessAttachment::SlotType { class, key } => Some(Self::SlotType {
                class: class.clone(),
                key: key.clone(),
            }),
            WitnessAttachment::TypeName(n) => Some(Self::TypeName(n.clone())),
            _ => None,
        }
    }
}

/// How a `Link` treats the evaluating query's receiver as it hops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReceiverRule {
    /// Pass it through unchanged — the inheritance hop, where the call is
    /// still on the original object.
    Thread,
    /// `fresh_dispatch_receiver` semantics: keep the incoming receiver iff its
    /// class IS this class or a subclass, else substitute `ClassName(class)`.
    Dispatch(String),
}

/// One key's partially-evaluated answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Conclusion {
    /// Constant in all three binders. The bake has PROVEN that, which is why
    /// receiver/arity/point are ignored on evaluation rather than merely
    /// unused.
    Value(InferredType),
    /// Depends on receiver and/or args; the existing `ReturnExpr` is exactly
    /// that dependence, so it is stored and evaluated by the same code the
    /// live path uses.
    ReturnOf(ReturnExpr),
    /// The answer is in another file. Never a value — see "Edges, not values".
    ///
    /// `targets` is ORDERED and first-answer-wins, because Perl's DFS-MRO is an
    /// ordered ladder and that is precisely what this form encodes: "whichever
    /// of these rungs answers first". A chase that COMBINED frames — an arm
    /// fold, a splice into a populated witness list — has an answer this cannot
    /// express, and is poisoned to `OpenNone` rather than squeezed into a
    /// vector that would silently mean something else.
    Link {
        targets: Vec<ConclusionKey>,
        arity: Option<u32>,
        receiver: ReceiverRule,
    },
    /// Present, and deliberately unbakeable: decode the blob and run the real
    /// chase for THIS key. Distinct from absent, which means "None".
    ///
    /// The reason is carried rather than counted at the bake, because the
    /// question a widening has to answer is which cause drives DECODES, and a
    /// bake-side tally counts KEYS. Those differ by however often each key is
    /// consulted, which is exactly the kind of unweighted total that has
    /// mis-sized every step of this arc.
    OpenNone(OpenReason),
}

/// Why a key could not be baked. Measurement-bearing, not behaviour-bearing:
/// every variant evaluates to `Outcome::Decode`, and a consumer that branched
/// on one would be reading a bake-time accident as a semantic distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenReason {
    /// No answer without binders, and the chase named no portable exit — it
    /// was poisoned, or it recorded nothing at all. Nothing a richer
    /// conclusion FORM could carry; this is the bag's own openness.
    NoAnswerOpaque,
    /// No answer, and the only rung the chase named was the key being baked.
    /// A `Link` here would walk to where the consult already is.
    NoAnswerSelfOnly,
    /// Self-only, AND the fold reaches a binder-dependent shape through its
    /// edges — so a residualizing form (`ReturnOf` evaluated with the
    /// consult's own binders) could serve it where `OpenNone` cannot.
    ///
    /// Split out from `NoAnswerSelfOnly` for one reason: the share that
    /// residualization could convert is a question about DECODES, and a
    /// bake-side tally counts KEYS. Carrying the verdict on the row is what
    /// lets the consult-side `.wasted`/`.paid` weighting answer it. Behaves
    /// identically — both are `Outcome::Decode`.
    NoAnswerSelfOnlyResidualizable,
    /// No answer, and the chase named rungs a `Link` could carry. This is the
    /// population a widening would convert, and the only one that is.
    NoAnswerLinkable,
    /// The answer moved, or vanished, under a different receiver or arity.
    /// `Value` may not represent it and `ReturnOf` did not claim it.
    BinderDependent,
    /// The bare and probed queries disagreed about the KIND of answer — a type
    /// against a fact map — which no conclusion form represents.
    KindDisagreement,
    /// Two attachments projected onto one key with different conclusions.
    /// Cannot fire while `from_attachment` is injective; counted so that
    /// "cannot fire" is something the numbers say rather than a comment.
    KeyCollision,
    /// Not a stored conclusion at all: the key is ABSENT and the class cannot
    /// be proven closed, so absence proves nothing and the chase must run.
    ///
    /// Carried alongside the stored reasons because from the consult's side it
    /// is the same event — a decode — and separating them by provenance is
    /// what made the first attribution describe a quarter of the population
    /// while reading as if it described all of it: 25,897 counted against
    /// 92,559 measured, and the gap WAS the answer.
    ///
    /// Measured at 70.3% of all decodes, and 97.6% of those on a class the map
    /// DOES conclude about — it simply inherits from somewhere this file
    /// cannot see.
    AbsentNotClosed,
}

impl OpenReason {
    /// Stable tag for the counters. A `Debug` projection would rename every
    /// series the day someone renames a variant.
    pub fn tag(self) -> &'static str {
        match self {
            OpenReason::NoAnswerOpaque => "concl.open.no_answer_opaque",
            OpenReason::NoAnswerSelfOnly => "concl.open.no_answer_self_only",
            OpenReason::NoAnswerSelfOnlyResidualizable => {
                "concl.open.no_answer_self_only_residualizable"
            }
            OpenReason::NoAnswerLinkable => "concl.open.no_answer_linkable",
            OpenReason::BinderDependent => "concl.open.binder_dependent",
            OpenReason::KindDisagreement => "concl.open.kind_disagreement",
            OpenReason::KeyCollision => "concl.open.key_collision",
            OpenReason::AbsentNotClosed => "concl.open.absent_not_closed",
        }
    }
}

/// Per-file map, persisted beside the blob and invalidated with it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConclusionMap(
    pub HashMap<ConclusionKey, Conclusion>,
    /// Classes whose absence is CONCLUSIVE — every ancestor is declared in
    /// this file, so a key missing from the map really is a proven `None`.
    ///
    /// The property belongs to the class, not the key. A class with a
    /// cross-file parent can be asked about a method it inherits, which has no
    /// attachment in this file's bag and therefore no key here — and reading
    /// that absence as `None` is wrong 633 times per substrate check. We
    /// cannot enumerate those keys (they are the PARENT's method names, which
    /// this file does not know), so the answerable question is which classes
    /// can be reasoned about at all.
    #[serde(default)]
    pub std::collections::HashSet<String>,
    /// Classes this file DECLARES — the ones whose local keys the bake
    /// enumerated. A superset of the closed set.
    ///
    /// The difference between the two sets is the whole of a third absence
    /// verdict. For a class in here but not closed, a missing key proves
    /// something weaker than "no answer" and much stronger than nothing: this
    /// file does not answer it LOCALLY. It may still inherit it — which is
    /// exactly what the live ladder goes on to check, so the honest reply is
    /// "not mine, keep walking" rather than a decode that discovers the same
    /// thing 98.7% of the time.
    ///
    /// Soundness rests on one property, and it is the only one: absent from
    /// this map ⇒ this file has no local answer. The bake enumerates every
    /// declared sub and method (`bake_with_symbols`) precisely so that holds.
    /// Today's decode silently covers any gap in it; this verdict does not,
    /// which is why it ships with `PERL_LSP_CONCL_EQUIV`.
    #[serde(default)]
    pub std::collections::HashSet<String>,
    /// Each enumerated class's DECLARED parents, in MRO order.
    ///
    /// Needed because a key's absence is not the same question as a class's.
    /// The live chase composes: asked for `C::m` with no witnesses, it walks
    /// C's parents and can answer from `Parent::m` — and this file may well
    /// hold a conclusion for `Parent::m` even when the parent lives elsewhere,
    /// because its own bag carries witnesses about it.
    ///
    /// `Mojo::Server::Daemon::app` is the case: no local symbol, no attachment,
    /// no app-surface edge, and an index-less chase still answers
    /// `ClassName("Mojolicious")` — from `Mojo::Server::app`, whose key this
    /// same map holds. Reading the child's absence as "not local" served a
    /// grandparent's answer over it, 40 times per substrate check.
    ///
    /// So absence walks these before concluding anything. Cross-file parents
    /// the map has nothing for are the outer ladder's business, unchanged.
    #[serde(default)]
    pub HashMap<String, Vec<String>>,
);

impl ConclusionMap {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    #[allow(dead_code)] // read by tests; the evaluator goes through `evaluate`
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    #[allow(dead_code)] // read by tests; the evaluator goes through `evaluate`
    pub fn get(&self, key: &ConclusionKey) -> Option<&Conclusion> {
        self.0.get(key)
    }

    /// Evaluate one key against a call site.
    ///
    /// Returns `Outcome::Answer` for a resolved value, `Outcome::None` when
    /// the map proves there is no answer, `Outcome::Decode` when the key is
    /// `OpenNone` or evaluation could not complete, and `Outcome::Follow` when
    /// the answer lives in another file.
    pub fn evaluate(
        &self,
        key: &ConclusionKey,
        receiver: Option<&InferredType>,
        arity: Option<u32>,
        args: &[InferredType],
    ) -> Outcome {
        let Some(c) = self.0.get(key) else {
            // ABSENT. Conclusive only for a class this file can reason about
            // completely — see the `closed` field. For anything else the honest
            // answer is "I do not know", which is a decode.
            // The key is absent, which is a question about the KEY. Before
            // answering it, ask the question about the CLASS: does an
            // enumerated parent hold this member? The live chase composes that
            // way, so a verdict that skipped it would be answering a narrower
            // question than the one it replaces.
            if let ConclusionKey::MethodOnClass { class, name } = key {
                if let Some(t) = self.inherited(class, name, receiver, arity, args, 0) {
                    return t;
                }
            }
            return match key {
                ConclusionKey::MethodOnClass { class, .. } => {
                    if self.1.contains(class) {
                        // Closed: every ancestor is declared here, so there is
                        // nowhere else an answer could have come from.
                        Outcome::None
                    } else if self.2.contains(class) {
                        Outcome::NotLocal
                    } else {
                        // A class this file never declared. Absence says
                        // nothing at all — the bake never looked.
                        Outcome::Decode(OpenReason::AbsentNotClosed)
                    }
                }
                _ => Outcome::None,
            };
        };
        match c {
            Conclusion::Value(t) => Outcome::Answer(t.clone()),
            Conclusion::ReturnOf(re) => {
                // The live evaluator, not a copy of it. A second spelling of
                // `ReturnExpr` semantics is a place for the baked answer to
                // drift from the answer it is supposed to equal.
                let att = WitnessAttachment::PackageSymbol {
                    package: String::new(),
                    name: String::new(),
                };
                let q = ReducerQuery {
                    attachment: &att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: arity,
                    receiver: receiver.cloned(),
                    args: args.to_vec(),
                    context: None,
                };
                match super::reducers::eval_return_expr(re, &q) {
                    Some(t) => Outcome::Answer(t),
                    // `Receiver` with no receiver is a legitimate None (the
                    // live path returns None rather than guessing), so this is
                    // not a decode.
                    None => Outcome::None,
                }
            }
            Conclusion::Link {
                targets,
                arity: link_arity,
                receiver: rule,
            } => Outcome::Follow {
                targets: targets.clone(),
                arity: link_arity.or(arity),
                receiver: match rule {
                    ReceiverRule::Thread => receiver.cloned(),
                    ReceiverRule::Dispatch(class) => Some(fresh_receiver(receiver, class)),
                },
            },
            Conclusion::OpenNone(reason) => Outcome::Decode(*reason),
        }
    }
}

impl ConclusionMap {
    /// Resolve `class::name` through the class's declared parents, within this
    /// map only. `None` when no parent chain in this file has anything to say.
    ///
    /// Depth-capped rather than cycle-guarded: a declared-parent chain inside
    /// one file is short, and a cap is one comparison against a `HashSet`
    /// allocation per lookup on a path taken tens of thousands of times per
    /// check. A cycle therefore truncates instead of looping, which degrades
    /// to the verdict the absence would have produced anyway.
    fn inherited(
        &self,
        class: &str,
        name: &str,
        receiver: Option<&InferredType>,
        arity: Option<u32>,
        args: &[InferredType],
        depth: usize,
    ) -> Option<Outcome> {
        const MAX_LOCAL_MRO: usize = 8;
        if depth >= MAX_LOCAL_MRO {
            crate::util::ghost_stats::count("concl.local_mro_cap");
            return None;
        }
        for parent in self.3.get(class)? {
            let key = ConclusionKey::MethodOnClass {
                class: parent.clone(),
                name: name.to_string(),
            };
            if self.0.contains_key(&key) {
                crate::util::ghost_stats::count("concl.inherited_hit");
                return Some(self.evaluate(&key, receiver, arity, args));
            }
            if let Some(o) = self.inherited(parent, name, receiver, arity, args, depth + 1) {
                return Some(o);
            }
        }
        None
    }
}

/// One file's answers, evaluated against the CURRENT store.
///
/// This — not the persisted map — is the driver's diff artifact, and the
/// distinction is the entire soundness of the propagation cutoff.
///
/// A persisted map is index-free by construction: the bake withholds the
/// module index so a cross-file answer residualizes as a `Link` instead of
/// freezing a world that can change without this file changing. That is what
/// makes the map durable, and it is exactly what makes it useless as a change
/// signal. When C changes, B's map is BYTE-IDENTICAL — B's `Link` still points
/// at the same key — while B's *answers*, chased through to C, have moved. A
/// driver that cuts propagation on map equality stops the wave at B and
/// starves B's consumers.
///
/// It fails silently, and it passes every two-file fixture: with one hop there
/// is no B for the wave to die at. Only a chain distinguishes the two, which
/// is why `a_chain_needs_the_evaluated_surface_not_the_map` exists and why it
/// asserts the map's byte-identity as a PRECONDITION rather than assuming it.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedSurface(pub Vec<(ConclusionKey, EvaluatedAnswer)>);

/// One key's answer after evaluation — map lookups only, never a decode.
#[derive(Debug, Clone, PartialEq)]
pub enum EvaluatedAnswer {
    Answer(InferredType),
    /// The map proves no answer.
    None,
    /// No LOCAL answer; the live ladder continues past this file.
    NotLocal,
    /// Unbakeable here, or a `Link` the store cannot complete. The consumer's
    /// answer is whatever the chase finds, so this file cannot cut a chain on
    /// it — it compares equal only to another `Opaque`, which is the honest
    /// conservative direction.
    Opaque,
}

impl ConclusionMap {
    /// Evaluate every key this map holds against the current store.
    ///
    /// `resolve` is the same map-lookup resolver `follow_link_with` takes:
    /// class name → the candidate files' maps. No decodes, so this is cheap
    /// enough to run per file per flush round.
    ///
    /// Sorted by key, because the driver compares these for equality and a
    /// `HashMap` walk would make the comparison depend on iteration order —
    /// the same defect that made `--dump-package` answer a coin flip.
    pub fn evaluated_surface(
        &self,
        resolve: &dyn Fn(&str) -> Vec<(String, Option<std::sync::Arc<ConclusionMap>>)>,
    ) -> EvaluatedSurface {
        let mut out: Vec<(ConclusionKey, EvaluatedAnswer)> = self
            .0
            .keys()
            .map(|k| (k.clone(), self.evaluate_for_surface(k, resolve)))
            .collect();
        out.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)));
        EvaluatedSurface(out)
    }

    fn evaluate_for_surface(
        &self,
        key: &ConclusionKey,
        resolve: &dyn Fn(&str) -> Vec<(String, Option<std::sync::Arc<ConclusionMap>>)>,
    ) -> EvaluatedAnswer {
        // No binders: the surface is the file's EXPORT face, and a
        // receiver-dependent answer is not part of it — `ReturnOf` evaluates
        // to `None` without a receiver, which is the same thing every consumer
        // sees until it supplies one.
        match self.evaluate(key, None, None, &[]) {
            Outcome::Answer(t) => EvaluatedAnswer::Answer(t),
            Outcome::None => EvaluatedAnswer::None,
            Outcome::NotLocal => EvaluatedAnswer::NotLocal,
            Outcome::Decode(_) => EvaluatedAnswer::Opaque,
            Outcome::Follow { targets, arity, receiver } => {
                match super::registry::follow_link_with(
                    resolve, &targets, &receiver, arity, &[],
                ) {
                    Some(t) => EvaluatedAnswer::Answer(t),
                    None => EvaluatedAnswer::Opaque,
                }
            }
        }
    }
}

/// What one map lookup produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Answer(InferredType),
    /// The map proves no answer — the ladder moves on (parents, next
    /// candidate) exactly as a local-reducer miss does today.
    None,
    /// The map proves no LOCAL answer: this file declares the class and
    /// enumerated its own members, and the key is not among them. It may still
    /// be inherited or bridged.
    ///
    /// Distinct from `None` because it licenses less: `None` can end the whole
    /// resolution, this may only skip the file that said it. Distinct from
    /// `Decode` because it licenses more: the chase this would have paid for
    /// answers nothing locally by construction.
    ///
    /// Deliberately NOT a constructed `Follow` at the parents. A `Follow`
    /// returned from candidate 1's map would short-circuit candidates 2..n,
    /// and a reopened package's method lives in a later candidate
    /// (`PPI::XSAccessor` reopening `PPI::Token` is the case that has already
    /// cost this layer 75 equivalence breaks once). Continuing the loop keeps
    /// candidates-before-parents and the bridge guard correct by construction
    /// rather than by re-derivation.
    NotLocal,
    /// Unbakeable here. Decode the blob and run the real chase for this key.
    ///
    /// Carries WHY, because the consult is where the cost lands and therefore
    /// where the attribution has to be taken. Counting causes at the bake
    /// counts keys; counting them at the consult weights each by how often it
    /// is actually asked, and the two differ by orders of magnitude.
    Decode(OpenReason),
    /// The answer is in another file; follow with these binders. Ordered,
    /// first-answer-wins.
    Follow {
        targets: Vec<ConclusionKey>,
        arity: Option<u32>,
        receiver: Option<InferredType>,
    },
}

/// `fresh_dispatch_receiver`'s rule, in the one place the map needs it: keep a
/// receiver that IS this class (or names it), else substitute the class.
///
/// The subclass test the live path does needs an index; without one the
/// conservative move is to substitute, which is what a receiver-less call
/// would have produced anyway.
fn fresh_receiver(incoming: Option<&InferredType>, class: &str) -> InferredType {
    match incoming {
        Some(t) if t.class_name().as_deref() == Some(class) => t.clone(),
        _ => InferredType::ClassName(class.to_string()),
    }
}

/// Whether an absent key may be trusted as a proven `None`.
///
/// ON, but only for a class with no ancestors — and the caller enforces that,
/// not this flag.
///
/// Absence is the layer's win: it fires ~127k times per warm substrate check
/// against ~1.3k served answers, so a version that cannot trust it is worth
/// under 1% of consults. Trusting it soundly takes the consult chase from
/// 2,375.9 ms to 1,817.3 ms and its calls from 109,003 to 63,768.
///
/// The soundness rule took three tries and is worth stating so nobody
/// relitigates it:
///
///   1. Trust every absence — 633 breaks per check. An inherited method has no
///      attachment in the child's bag, so no key is enumerated for it.
///   2. Trust it for classes whose declared parents are all local — 75 breaks.
///      Perl packages are OPEN: `PPI::XSAccessor` reopens `package PPI::Token`
///      without repeating its `@ISA`, so that file's bake sees a parentless
///      class which in fact inherits from `PPI::Element`.
///   3. Ask the INDEX, through the same `parents_of` every other ancestor walk
///      uses — 0 breaks. Closedness is a property of the class across ALL
///      files, and no per-file bake can establish it.
///
/// `PERL_LSP_CONCL_EQUIV` is the standing proof rather than a one-off: it runs
/// the chase anyway and reports any answer an absence claimed did not exist.
/// Green over the whole substrate. `PERL_LSP_NO_TRUST_ABSENT` reverts to
/// deferring every absence to a decode.
pub fn trust_absent_conclusions() -> bool {
    static TRUST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRUST.get_or_init(|| std::env::var("PERL_LSP_NO_TRUST_ABSENT").is_err())
}

/// Verify each trusted absence against the path it replaces.
///
/// Same shape as `PERL_LSP_PD_EQUIV` for pattern dispatch: a fast path whose
/// justification is empirical ships WITH the means to re-check it, rather than
/// with a note asking the reader to believe the measurement. Under this flag
/// an absent key still runs the decode and the chase, and a disagreement is
/// reported — so the gold harness can assert the equivalence over a real
/// corpus instead of the four packages a person happened to dump.
pub fn verify_absent_conclusions() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("PERL_LSP_CONCL_EQUIV").is_ok())
}

/// Mint `Link`s from a chase's recorded residuals.
///
/// OFF until the ladder-frame rule's poisoning half lands — see the comment at
/// the mint site for the measured error rate.
/// Escape hatch and A/B control for the not-local verdict, same shape as
/// `PERL_LSP_NO_BAKE`: a change that removes work from the ladder must be
/// switchable off without a rebuild, or its correctness can only be argued
/// rather than measured.
pub fn not_local_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("PERL_LSP_NO_NOT_LOCAL").is_ok())
}

/// Is baking switched off?
///
/// ONE speller, because there are TWO producers of a conclusions row — the
/// persist path (`encode_analysis`) and the background repair lane
/// (`repair_conclusions_slice`, seeded from `paths_needing_repair`) — and
/// a control that gates only the first is not a control at all. Measured on
/// the substrate before this existed: an A/B whose OFF arm was primed cold
/// answered 72,305 provider fetches on its first run and **57,481 on its
/// third**, byte-identical to the ON arm, because the repair lane had quietly
/// baked the entire frontier in between. The flag disabled itself after one
/// warm run and nothing said so.
///
/// Read through `conclusions::bake_disabled()` from every producer;
/// `layering_tests::the_bake_gate_has_one_reader` pins that there is no
/// second `std::env::var` for this name.
pub fn bake_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("PERL_LSP_NO_BAKE").is_ok())
}

pub fn mint_links_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PERL_LSP_MINT_LINKS").is_ok())
}

/// How many map-to-map hops a `Follow` may take before giving up.
///
/// A `Link` chain is a graph walk over files, so it needs a bound of its own —
/// the live chase's `VisitedKey` guards the live chase, not this projection. A
/// cycle is caught by the visited set; the cap catches a long-but-acyclic
/// chain, where continuing costs more than the decode it is replacing.
pub const MAX_FOLLOW_HOPS: usize = 8;

/// Marks the calling thread as running a BAKE rather than a live query.
///
/// The exit sites below cannot tell the difference on their own — both see
/// `module_index: None` — and only a bake's misses are candidates for
/// residualization. A live query with no index is an ordinary degraded lookup
/// and must not be counted.
pub struct BakeScope;

thread_local! {
    static IN_BAKE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl BakeScope {
    pub fn enter() -> Self {
        IN_BAKE.with(|f| f.set(true));
        BakeScope
    }
}

impl Drop for BakeScope {
    fn drop(&mut self) {
        IN_BAKE.with(|f| f.set(false));
    }
}

/// Record that a bake reached a point where it would have consulted the index.
///
/// `nameable` says whether the would-be target has a portable `ConclusionKey`.
/// The split is the whole decision: only a nameable exit can become a `Link`,
/// so the ratio says how much of the `OpenNone` population this line of work
/// can actually reach, and whether it is worth building the machinery for.
pub fn note_bake_exit(site: &'static str, nameable: bool) {
    if !IN_BAKE.with(|f| f.get()) {
        return;
    }
    crate::util::ghost_stats::count(if nameable {
        "residual.nameable"
    } else {
        "residual.poisoned"
    });
    crate::util::ghost_stats::count(&format!("residual.site.{site}"));
}

/// The process-wide default registry.
///
/// The bake runs per file on the persist path, and `with_defaults()` allocates
/// a boxed reducer per entry. Query sites build one per call because they are
/// answering one question; a bulk index answers thousands and would pay for
/// the same immutable table every time.
pub fn shared_registry() -> &'static ReducerRegistry {
    static REG: std::sync::OnceLock<ReducerRegistry> = std::sync::OnceLock::new();
    REG.get_or_init(ReducerRegistry::with_defaults)
}

/// Bake one analysis's bag into a map.
///
/// Runs with `module_index: None` by construction — the caller passes a
/// registry query that cannot reach another file, so every cross-file
/// fallback residualizes rather than materializing. That is not an
/// optimization; a materialized cross-file value would freeze a world that
/// can change without this file changing.
#[allow(dead_code)] // defaulting wrapper over `bake_in_context`; production passes its own context
pub fn bake(
    bag: &WitnessBag,
    registry: &ReducerRegistry,
    local_packages: &std::collections::HashSet<String>,
) -> ConclusionMap {
    bake_with_symbols(bag, registry, local_packages, &[])
}

/// `bake`, plus the keys the file's SYMBOLS imply.
///
/// The attachment index alone is not the set of keys the bag can answer, and
/// the difference is the whole soundness of "absent means None". A method the
/// live chase resolves through an inheritance edge or a reducer's synthesis
/// may carry no witnesses of its own, so it never appears in the index — and a
/// key missing from the map is read as a proven `None`.
///
/// The declared symbols close most of that gap: `bake_one` runs the REGISTRY
/// on the attachment, so a witness-less `MethodOnClass` still gets whatever
/// answer the live path would give it, including one composed entirely from
/// edges.
#[allow(dead_code)] // see `bake`
pub fn bake_with_symbols(
    bag: &WitnessBag,
    registry: &ReducerRegistry,
    local_packages: &std::collections::HashSet<String>,
    symbols: &[(Option<String>, String, bool)],
) -> ConclusionMap {
    bake_full(bag, registry, local_packages, symbols, &[])
}

/// `bake_with_symbols`, plus each class's declared parents.
///
/// Parents decide which classes are CLOSED — reasoned about completely from
/// this file — and only a closed class may have its absences read as proofs.
#[allow(dead_code)] // see `bake`
pub fn bake_full(
    bag: &WitnessBag,
    registry: &ReducerRegistry,
    local_packages: &std::collections::HashSet<String>,
    symbols: &[(Option<String>, String, bool)],
    parents: &[(String, Vec<String>)],
) -> ConclusionMap {
    bake_in_context(bag, registry, local_packages, symbols, parents, None)
}

/// `bake_full` with the file's OWN context — scopes, frameworks, parents.
///
/// Withholding the module index is the design (a materialized cross-file value
/// would freeze a world that can change without this file changing). Withholding
/// everything else was an accident: passing `context: None` also denies the bake
/// the file's scopes, per-package frameworks and LOCAL parent edges, so it could
/// not walk a parent chain that lives entirely in this file. Some of
/// `bake.no_bare_answer` was that rather than genuine cross-file dependence.
///
/// A local-only context is also what makes "where would the chase have gone"
/// well-posed: with no context at all, the chase stops before it reaches the
/// point where it would have asked the index.
pub fn bake_in_context(
    bag: &WitnessBag,
    registry: &ReducerRegistry,
    local_packages: &std::collections::HashSet<String>,
    symbols: &[(Option<String>, String, bool)],
    parents: &[(String, Vec<String>)],
    ctx: Option<&super::reducers::BagContext<'_>>,
) -> ConclusionMap {
    let _bake = BakeScope::enter();
    let mut map = HashMap::new();
    let enumerate = |att: WitnessAttachment, map: &mut HashMap<_, _>| {
        let Some(key) = ConclusionKey::from_attachment(&att) else {
            return;
        };
        if map.contains_key(&key) {
            return;
        }
        let c = bake_one(bag, registry, &att, local_packages, ctx);
        map.insert(key, c);
    };
    // Declared subs and methods first, so the attachment-index pass below
    // sees them already present and does not re-derive.
    for (package, name, is_callable) in symbols {
        if !*is_callable {
            continue;
        }
        if let Some(pkg) = package {
            enumerate(
                WitnessAttachment::PackageSymbol {
                    package: pkg.clone(),
                    name: name.clone(),
                },
                &mut map,
            );
        }
    }
    for att in bag.attachments() {
        let Some(key) = ConclusionKey::from_attachment(att) else {
            continue;
        };
        let conclusion = bake_one(bag, registry, att, local_packages, ctx);
        match map.entry(key) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(conclusion);
            }
            // Two attachments projecting to one key. `from_attachment` is
            // injective today so this cannot fire, but "cannot fire" is not
            // something the code says anywhere — and first-wins over
            // `attachments()`, which is `HashMap::keys()`, would make the
            // bake depend on iteration order. That is not merely untidy: the
            // diff-propagation driver cuts its worklist on an EMPTY conclusion
            // diff, so an order-dependent bake produces spurious diffs that
            // never cut the chain, and spuriously-empty ones that cut a chain
            // which should have propagated.
            //
            // Disagreement degrades to `OpenNone` — order-independent by
            // construction, and the safe direction.
            std::collections::hash_map::Entry::Occupied(mut o) => {
                if *o.get() != conclusion {
                    crate::util::ghost_stats::count("bake.key_collision");
                    o.insert(Conclusion::OpenNone(OpenReason::KeyCollision));
                }
            }
        }
    }
    // A class is closed when every ancestor, transitively, is declared here.
    // Transitively matters: a local parent with a foreign grandparent inherits
    // methods this file has never seen, so treating the child as closed would
    // reintroduce exactly the bug this field exists to stop.
    let parent_of: HashMap<&str, &Vec<String>> =
        parents.iter().map(|(c, ps)| (c.as_str(), ps)).collect();
    let mut closed = std::collections::HashSet::new();
    for class in local_packages {
        let mut stack = vec![class.as_str()];
        let mut seen = std::collections::HashSet::new();
        let mut ok = true;
        while let Some(c) = stack.pop() {
            if !seen.insert(c.to_string()) {
                continue;
            }
            if !local_packages.contains(c) {
                ok = false;
                break;
            }
            if let Some(ps) = parent_of.get(c) {
                for p in ps.iter() {
                    stack.push(p.as_str());
                }
            }
        }
        if ok {
            closed.insert(class.clone());
        }
    }
    // The ENUMERATED set: classes for which "absent from this map" really does
    // imply "no local answer". That is not every class this file declares.
    //
    // The bake enumerates keys from the bag's attachments and the file's
    // declared symbols. A member that arrives by an edge NEITHER of those
    // follows leaves no key, and its absence then says the opposite of the
    // truth. The synthetic app-surface parent is exactly such an edge:
    // `parents_of` composes it, `declared_parents` never reports it, so
    // `Mojo::Server::Daemon::app` resolves locally through it and has no key
    // here. Measured: 40 not-local breaks per substrate check, every one this
    // shape.
    //
    // Asked as "does this class have a parent the DECLARED list does not
    // carry" rather than "is this class an app-surface consumer", so a second
    // synthetic edge kind disqualifies its classes without anyone remembering
    // to add it here. The index is withheld — a cross-file parent already
    // costs closedness, and this question is about edges the bake itself could
    // not follow.
    let consumers = ctx.map(|c| c.app_surface_consumers).unwrap_or(&[]);
    let enumerated: std::collections::HashSet<String> = local_packages
        .iter()
        .filter(|class| {
            let declared = parent_of.get(class.as_str()).map(|v| v.len()).unwrap_or(0);
            let composed = crate::model::file_analysis::app_surface_parent(class, consumers)
                .map_or(declared, |_| declared + 1);
            composed == declared
        })
        .cloned()
        .collect();
    ConclusionMap(map, closed, enumerated, parent_of
        .iter()
        .map(|(c, ps)| ((*c).to_string(), (*ps).clone()))
        .collect())
}

/// One attachment's conclusion.
///
/// Everything uncertain lands on `OpenNone`. The temptation is to bake the
/// best guess and let the consumer sort it out, but a consumer cannot tell a
/// guess from a proof, and this map exists precisely so consumers can stop
/// checking.
fn bake_one(
    bag: &WitnessBag,
    registry: &ReducerRegistry,
    att: &WitnessAttachment,
    local_packages: &std::collections::HashSet<String>,
    ctx: Option<&super::reducers::BagContext<'_>>,
) -> Conclusion {
    // A sole edge to a class this file does not declare is the cross-file
    // hop, and it residualizes rather than resolving. The bake runs with no
    // module index precisely so this case CANNOT accidentally materialize:
    // the provider can change without this file changing, and a baked value
    // would outlive the truth it copied.
    if let Some(target) = sole_foreign_edge(bag, att, local_packages) {
        crate::util::ghost_stats::count("bake.link");
        return Conclusion::Link {
            targets: vec![target],
            arity: None,
            // An inheritance hop keeps the original object as receiver — the
            // call is still on it, the method just lives up the chain.
            receiver: ReceiverRule::Thread,
        };
    }
    // A witness carrying a ReturnExpr IS the receiver/arity dependence, so it
    // is stored as syntax rather than evaluated. Checked before the constant
    // probe below, because evaluating one of these with no receiver yields
    // None and would bake as absent — a fluent accessor silently answering
    // "no type" to every caller.
    if let Some(re) = sole_return_expr(bag, att) {
        crate::util::ghost_stats::count("bake.return_of");
        return Conclusion::ReturnOf(re);
    }

    // Constant probe. Two queries differing only in their binders must agree
    // before a `Value` is licensed: an answer that moves with the receiver is
    // exactly what `Value` may not represent.
    let probe = |receiver: Option<InferredType>, arity: Option<u32>| -> ReducedValue {
        registry.query(
            bag,
            &ReducerQuery {
                attachment: att,
                point: None,
                framework: FrameworkFact::Plain,
                arity_hint: arity,
                receiver,
                args: Vec::new(),
                // The file's own context, with `module_index: None` — see
                // `bake_in_context`. The index stays withheld by design; the
                // rest is what lets a local parent chain resolve at all.
                context: ctx,
            },
        )
    };

    let bare = probe(None, None);
    crate::util::ghost_stats::count("bake.attempted");
    let ReducedValue::Type(t) = bare else {
        crate::util::ghost_stats::count("bake.no_bare_answer");
        // No answer without binders. Before settling for `OpenNone`, ask
        // whether the chase told us WHERE it would have gone: an un-poisoned
        // chase whose residuals are all ladder rungs is exactly a `Link`.
        //
        // MINTING IS OFF BY DEFAULT, and the reason is COST, not soundness.
        // With the ladder-frame rule's poisoning half in place the follow
        // breaks go to zero over the substrate — but the decodes do not move
        // (4103 -> 4104). `follow_one` abandons at the first rung whose map
        // says `Decode`, and with ~84k `OpenNone` still in the maps that is
        // nearly every walk, so the consult falls through to the decode it
        // would have done anyway. The `Link` cannot pay off while `OpenNone`
        // dominates the rungs; the leverage is in shrinking that population.
        // `docs/adr/conclusion-layer.md` ("Widening `Link`: rejected") carries the reasoning.
        // Where the chase would have gone, read UNCONDITIONALLY — not behind
        // the minting flag. The composition of this population is what decides
        // whether widening the `Link` form is worth building, and it cannot be
        // measured behind the flag the widening would turn on.
        //
        // The self-rung is dropped first. The cross-file primary records the
        // key being baked as its own first rung, which is true of the ladder
        // and useless as a `Link`: the consult reached this map by doing
        // exactly that, so a walk back to it is a walk to where we already are.
        let residual = registry.residuals_of_last_query().map(|targets| {
            let self_key = ConclusionKey::from_attachment(att);
            targets
                .into_iter()
                .filter(|t| Some(t) != self_key.as_ref())
                .collect::<Vec<_>>()
        });
        let open_reason = match &residual {
            // Poisoned, or nothing recorded: no portable exit exists, so no
            // richer conclusion FORM would reach this key. It is the bag's own
            // openness, and it is the population a widening cannot touch.
            None => OpenReason::NoAnswerOpaque,
            Some(t) if t.is_empty() => {
                census_self_only(bag, att);
                if self_only_residualizable(bag, att) {
                    OpenReason::NoAnswerSelfOnlyResidualizable
                } else {
                    OpenReason::NoAnswerSelfOnly
                }
            }
            Some(_) => OpenReason::NoAnswerLinkable,
        };
        if mint_links_enabled() {
            if let Some(targets) = residual {
                if !targets.is_empty() {
                    crate::util::ghost_stats::count("bake.link_from_residual");
                    return Conclusion::Link {
                        targets,
                        arity: None,
                        // The live candidate/parent hops thread the receiver
                        // unchanged; the call is still on the original object.
                        receiver: ReceiverRule::Thread,
                    };
                }
                crate::util::ghost_stats::count("bake.link_was_self_only");
            }
        }

        // No answer, and no nameable exit — `OpenNone` (decode) rather than
        // absent (a proven None) — measured, not assumed.
        //
        // Treating these as absent looks free: the bake got None, so the live
        // path surely gets None too. It does not. 56 equivalence breaks per
        // substrate check, all the same shape — `Log::Log4perl::get_logger`
        // answering `ClassName("Log::Log4perl::Logger")`, `URI::new` answering
        // `ClassName("URI::_foreign")`. The bake runs with no module index, so
        // a chase that EXITS cross-file returns None here and has a real
        // answer at query time.
        //
        // These want to be `Link`, not `OpenNone`. The bake cannot mint one
        // because the edge it holds is `Edge(Symbol(sid))` — a LOCAL symbol
        // whose own chase leaves the file — so nothing local names the target.
        // Widening `Link` past `sole_foreign_edge` needs the registry to
        // report "I would have consulted the index here, for key K" instead of
        // returning None: a residualizing mode, which is a design step rather
        // than a fix. Until then these cost one decode each.
        return Conclusion::OpenNone(open_reason);
    };

    // A receiver the file cannot have produced. If the answer moves, the
    // conclusion is binder-dependent and must not be a constant.
    let probed = probe(
        Some(InferredType::ClassName(
            "Perl::LSP::ConclusionProbe".to_string(),
        )),
        Some(1),
    );
    let demote = |why: &'static str, reason: OpenReason| {
        crate::util::ghost_stats::count(why);
        Conclusion::OpenNone(reason)
    };
    match probed {
        ReducedValue::Type(t2) if t2 == t => {
            crate::util::ghost_stats::count("bake.value");
            Conclusion::Value(t)
        }
        // Answered differently under a different receiver/arity. That IS
        // binder-dependence, and not in a shape we can store — decode rather
        // than freeze whichever of the two answers we happened to see first.
        ReducedValue::Type(_) => {
            demote("bake.demoted_by_binder_probe", OpenReason::BinderDependent)
        }
        // Facts, not a type, where the bare probe gave a type. The two probes
        // disagree about the KIND of answer, which no conclusion form
        // represents.
        ReducedValue::FactMap(_) => {
            demote("bake.demoted_kind_disagreement", OpenReason::KindDisagreement)
        }
        // The answer vanished once a receiver was supplied — binder-dependent
        // in the most direct way there is.
        ReducedValue::None => {
            demote("bake.demoted_by_binder_probe", OpenReason::BinderDependent)
        }
    }
}

/// Shape breakdown of the self-only population, one hop: does the attachment
/// itself carry a binder-dependent `ReturnExpr`, and if not, what is the
/// floor made of?
///
/// `sole_return_expr` already bakes the single-witness case, so what lands
/// here with witnesses is the SEVERAL case — which is why the shapes matter:
/// several branches of one `UnionOnArgs` are combinable, several unrelated
/// shapes may not be.
///
/// Counters only. The verdict that rides the row comes from
/// `self_only_residualizable`, which asks the same question of the whole
/// fold rather than of one attachment.
fn census_self_only(bag: &WitnessBag, att: &WitnessAttachment) {
    if !crate::util::ghost_stats::enabled() {
        return;
    }
    let mut n = 0usize;
    for w in bag.for_attachment(att) {
        let super::types::WitnessPayload::ReturnExpr(re) = &w.payload else {
            continue;
        };
        n += 1;
        crate::util::ghost_stats::count(match re {
            ReturnExpr::Receiver => "selfonly.shape_receiver",
            ReturnExpr::ReceiverOr(_) => "selfonly.shape_receiver_or",
            ReturnExpr::UnionOnArgs { .. } => "selfonly.shape_union_on_args",
            ReturnExpr::Concrete(_) => "selfonly.shape_concrete",
            ReturnExpr::Operator(_) => "selfonly.shape_operator",
            _ => "selfonly.shape_other",
        });
    }
    crate::util::ghost_stats::count(if n == 0 {
        // No binder-dependent shape to store. This is the floor — and what
        // the attachment DOES carry says what the floor is made of, which is
        // the difference between "nothing to store" and "something we have
        // not thought of a form for".
        for w in bag.for_attachment(att) {
            crate::util::ghost_stats::count(match &w.payload {
                super::types::WitnessPayload::InferredType(_) => "selfonly.floor_inferred_type",
                super::types::WitnessPayload::Edge(t) => {
                    // WHERE the edge points decides whether this floor is the
                    // documented residualizing-registry gap or something new.
                    crate::util::ghost_stats::count(match t {
                        WitnessAttachment::Symbol(_) => "selfonly.edge_to_symbol",
                        WitnessAttachment::PackageSymbol { .. } => "selfonly.edge_to_pkgsym",
                        WitnessAttachment::Variable { .. } => "selfonly.edge_to_variable",
                        WitnessAttachment::Expression(_) => "selfonly.edge_to_expression",
                        _ => "selfonly.edge_to_other",
                    });
                    // Does the target project onto the SAME conclusion key —
                    // the writeback's mirror pointing back at the key being
                    // baked? Only answerable for a target that HAS a portable
                    // key: `Edge(Symbol(_))` projects to `None`, so a zero
                    // here is a property of the projection and not evidence
                    // about the edge. Read it against `edge_to_pkgsym`.
                    if let (Some(a), Some(b)) = (
                        ConclusionKey::from_attachment(t),
                        ConclusionKey::from_attachment(att),
                    ) {
                        if a == b {
                            crate::util::ghost_stats::count("selfonly.edge_is_self_mirror");
                        }
                    }
                    "selfonly.floor_edge"
                }
                super::types::WitnessPayload::Observation(_) => "selfonly.floor_observation",
                super::types::WitnessPayload::Fact { .. } => "selfonly.floor_fact",
                _ => "selfonly.floor_other",
            });
        }
        if bag.for_attachment(att).is_empty() {
            crate::util::ghost_stats::count("selfonly.floor_no_witnesses_at_all");
        }
        "selfonly.not_residualizable"
    } else {
        "selfonly.residualizable"
    });
    if n > 1 {
        crate::util::ghost_stats::count("selfonly.residualizable_several");
    }
    census_reachable_shapes(bag, att);
}

/// Does the fold behind this self-only key terminate in a binder-dependent
/// shape? That is the whole of §6l's gate: `ReturnOf(Receiver)` can be stored
/// and evaluated with the consult's own binders, where `OpenNone` can only
/// say "go decode".
///
/// The question is about the FOLD, not about this attachment — a fold ends
/// wherever the edge chase ends — so the walk follows the edges the bag
/// holds. Bounded by a seen-set and a depth cap because those edges are a
/// graph, not a tree: the writeback's `PackageSymbol -> Edge(Symbol)` mirrors
/// make cycles. A truncated walk answers `false`, so the verdict
/// under-approximates rather than over-claims.
///
/// Runs unconditionally, not behind the stats gate: the answer is stored on
/// the row so the consult side can weight it by decodes. It is confined to
/// the self-only branch — 3.3% of bakes on the substrate, ~3.6 attachments
/// each.
fn self_only_residualizable(bag: &WitnessBag, att: &WitnessAttachment) -> bool {
    walk_reachable(bag, att, &mut |_re| {}).0
}

/// Shape breakdown of what the walk reaches. Gated: this is the census, not
/// the verdict.
fn census_reachable_shapes(bag: &WitnessBag, att: &WitnessAttachment) {
    if !crate::util::ghost_stats::enabled() {
        return;
    }
    let (found, truncated, visited) = walk_reachable(bag, att, &mut |re| {
        crate::util::ghost_stats::count(match re {
            ReturnExpr::Receiver => "selfonly.reach_receiver",
            ReturnExpr::ReceiverOr(_) => "selfonly.reach_receiver_or",
            ReturnExpr::UnionOnArgs { .. } => "selfonly.reach_union_on_args",
            ReturnExpr::Concrete(_) => "selfonly.reach_concrete",
            ReturnExpr::Operator(_) => "selfonly.reach_operator",
            _ => "selfonly.reach_other",
        });
    });
    crate::util::ghost_stats::count(if found {
        "selfonly.residualizable_via_edges"
    } else if truncated {
        "selfonly.floor_unproven_depth_capped"
    } else {
        "selfonly.floor_confirmed"
    });
    crate::util::ghost_stats::count_by("selfonly.reach_attachments", visited as u64);
}

/// The shared walk: `(reached a ReturnExpr, truncated, attachments visited)`.
/// One speller so the verdict and the census cannot disagree about what
/// "reachable" means.
fn walk_reachable(
    bag: &WitnessBag,
    att: &WitnessAttachment,
    on_shape: &mut dyn FnMut(&ReturnExpr),
) -> (bool, bool, usize) {
    use super::types::WitnessPayload;
    const MAX_DEPTH: usize = 8;
    let mut seen: Vec<WitnessAttachment> = vec![att.clone()];
    let mut frontier: Vec<WitnessAttachment> = vec![att.clone()];
    let mut found = false;
    let mut depth = 0usize;
    while !frontier.is_empty() {
        if depth == MAX_DEPTH {
            return (found, !found, seen.len());
        }
        depth += 1;
        let mut next = Vec::new();
        for a in frontier.drain(..) {
            for w in bag.for_attachment(&a) {
                match &w.payload {
                    WitnessPayload::ReturnExpr(re) => {
                        found = true;
                        on_shape(re);
                    }
                    WitnessPayload::Edge(t) => {
                        if !seen.contains(t) {
                            seen.push(t.clone());
                            next.push(t.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        frontier = next;
    }
    (found, false, seen.len())
}

/// The single `ReturnExpr` an attachment carries, if it carries exactly one.
///
/// Exactly one, deliberately: several would need the fold's agreement rules to
/// combine them, and re-deriving those here is a second spelling of
/// `resolve_return_type`. More than one is `OpenNone`'s job.
/// The one `MethodOnClass` edge pointing at a class this file does not
/// declare, if there is exactly one and nothing else competes with it.
///
/// Exactly one, and nothing else: several edges, or an edge alongside a local
/// answer, need the registry's ordering rules to arbitrate, and re-deriving
/// those here is a second spelling of the chase. Those stay `OpenNone`.
fn sole_foreign_edge(
    bag: &WitnessBag,
    att: &WitnessAttachment,
    local_packages: &std::collections::HashSet<String>,
) -> Option<ConclusionKey> {
    let ws = bag.for_attachment(att);
    let mut found: Option<ConclusionKey> = None;
    for w in &ws {
        match &w.payload {
            super::types::WitnessPayload::Edge(WitnessAttachment::PackageSymbol {
                package,
                name,
            }) if !local_packages.contains(package) => {
                if found.is_some() {
                    return None;
                }
                found = Some(ConclusionKey::MethodOnClass {
                    class: package.clone(),
                    name: name.clone(),
                });
            }
            // Anything else on this attachment means the edge is not the whole
            // story, and the registry decides between them.
            _ => return None,
        }
    }
    found
}

fn sole_return_expr(bag: &WitnessBag, att: &WitnessAttachment) -> Option<ReturnExpr> {
    let mut found: Option<ReturnExpr> = None;
    for w in bag.for_attachment(att) {
        if let super::types::WitnessPayload::ReturnExpr(re) = &w.payload {
            if found.is_some() {
                return None;
            }
            found = Some(re.clone());
        }
    }
    found
}

#[cfg(test)]
#[path = "conclusions_tests.rs"]
mod conclusions_tests;
