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
);

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
            // ABSENT. Conclusive only for a class this file can reason about
            // completely — see the `closed` field. For anything else the honest
            // answer is "I do not know", which is a decode.
            return match key {
                ConclusionKey::MethodOnClass { class, .. } if !self.1.contains(class) => {
                    Outcome::Decode
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
    let mut enumerate = |att: WitnessAttachment, map: &mut HashMap<_, _>| {
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
                WitnessAttachment::MethodOnClass {
                    class: pkg.clone(),
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
                    o.insert(Conclusion::OpenNone);
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
    ConclusionMap(map, closed)
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
        // No answer without binders, and `OpenNone` (decode) rather than
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
