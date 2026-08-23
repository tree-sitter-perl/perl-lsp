//! Conclusions: the registry chase partially evaluated over one file's bag.
//!
//! `docs/prompt-conclusion-layer.md` owns the design. The short version: a
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
            WitnessAttachment::MethodOnClass { class, name } => Some(Self::MethodOnClass {
                class: class.clone(),
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
    Link {
        target: ConclusionKey,
        arity: Option<u32>,
        receiver: ReceiverRule,
    },
    /// Present, and deliberately unbakeable: decode the blob and run the real
    /// chase for THIS key. Distinct from absent, which means "None".
    OpenNone,
}

/// Per-file map, persisted beside the blob and invalidated with it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConclusionMap(pub HashMap<ConclusionKey, Conclusion>);

impl ConclusionMap {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
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
            // ABSENT. Sound only because the bake enumerates every key the bag
            // could answer; a missed key would silently become None here.
            return Outcome::None;
        };
        match c {
            Conclusion::Value(t) => Outcome::Answer(t.clone()),
            Conclusion::ReturnOf(re) => {
                // The live evaluator, not a copy of it. A second spelling of
                // `ReturnExpr` semantics is a place for the baked answer to
                // drift from the answer it is supposed to equal.
                let att = WitnessAttachment::MethodOnClass {
                    class: String::new(),
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
                target,
                arity: link_arity,
                receiver: rule,
            } => Outcome::Follow {
                target: target.clone(),
                arity: link_arity.or(arity),
                receiver: match rule {
                    ReceiverRule::Thread => receiver.cloned(),
                    ReceiverRule::Dispatch(class) => Some(fresh_receiver(receiver, class)),
                },
            },
            Conclusion::OpenNone => Outcome::Decode,
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
    /// Unbakeable here. Decode the blob and run the real chase for this key.
    Decode,
    /// The answer is in another file; follow with these binders.
    Follow {
        target: ConclusionKey,
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
pub fn bake(
    bag: &WitnessBag,
    registry: &ReducerRegistry,
    local_packages: &std::collections::HashSet<String>,
) -> ConclusionMap {
    let mut map = HashMap::new();
    for att in bag.attachments() {
        let Some(key) = ConclusionKey::from_attachment(att) else {
            continue;
        };
        let conclusion = bake_one(bag, registry, att, local_packages);
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
                    o.insert(Conclusion::OpenNone);
                }
            }
        }
    }
    ConclusionMap(map)
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
) -> Conclusion {
    // A sole edge to a class this file does not declare is the cross-file
    // hop, and it residualizes rather than resolving. The bake runs with no
    // module index precisely so this case CANNOT accidentally materialize:
    // the provider can change without this file changing, and a baked value
    // would outlive the truth it copied.
    if let Some(target) = sole_foreign_edge(bag, att, local_packages) {
        crate::util::ghost_stats::count("bake.link");
        return Conclusion::Link {
            target,
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
                // No context: the bake CANNOT reach another file, so every
                // cross-file fallback residualizes instead of materializing.
                context: None,
            },
        )
    };

    let bare = probe(None, None);
    crate::util::ghost_stats::count("bake.attempted");
    let ReducedValue::Type(t) = bare else {
        crate::util::ghost_stats::count("bake.no_bare_answer");
        // No answer without binders. That is either a genuine None or a
        // receiver-dependent shape this function failed to recognize as
        // `ReturnExpr`; the two are indistinguishable here, so decode.
        return Conclusion::OpenNone;
    };

    // A receiver the file cannot have produced. If the answer moves, the
    // conclusion is binder-dependent and must not be a constant.
    let probed = probe(
        Some(InferredType::ClassName(
            "Perl::LSP::ConclusionProbe".to_string(),
        )),
        Some(1),
    );
    let demote = |why: &'static str| {
        crate::util::ghost_stats::count(why);
        Conclusion::OpenNone
    };
    match probed {
        ReducedValue::Type(t2) if t2 == t => {
            crate::util::ghost_stats::count("bake.value");
            Conclusion::Value(t)
        }
        // Answered differently under a different receiver/arity. That IS
        // binder-dependence, and not in a shape we can store — decode rather
        // than freeze whichever of the two answers we happened to see first.
        ReducedValue::Type(_) => demote("bake.demoted_by_binder_probe"),
        // Facts, not a type, where the bare probe gave a type. The two probes
        // disagree about the KIND of answer, which no conclusion form
        // represents.
        ReducedValue::FactMap(_) => demote("bake.demoted_kind_disagreement"),
        // The answer vanished once a receiver was supplied — binder-dependent
        // in the most direct way there is.
        ReducedValue::None => demote("bake.demoted_by_binder_probe"),
    }
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
            super::types::WitnessPayload::Edge(WitnessAttachment::MethodOnClass {
                class,
                name,
            }) if !local_packages.contains(class) => {
                if found.is_some() {
                    return None;
                }
                found = Some(ConclusionKey::MethodOnClass {
                    class: class.clone(),
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
