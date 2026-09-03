//! The reducer vocabulary (`ReducerQuery` / `BagContext` /
//! `WitnessReducer`) and the built-in reducer implementations.

use super::*;

// ---- Reducers ----

/// Input to a reducer query: the attachment plus an optional point of
/// interest (so narrowing-scoped reducers pick the closest containing
/// witness).
#[derive(Clone)]
pub struct ReducerQuery<'a> {
    pub attachment: &'a WitnessAttachment,
    pub point: Option<tree_sitter::Point>,
    pub framework: FrameworkFact,
    /// Arity hint for arity-dispatch reducers. `Some(N)` = caller passed
    /// N additional args; `None` = unknown (reducer returns the default
    /// branch).
    pub arity_hint: Option<u32>,
    /// Receiver type for `ReturnExpr::Receiver` substitution. Set by the
    /// chain typer's coderef/dynamic-method-call arms and by
    /// `PackageSymbol{...}` chases originating from a method call with a
    /// known invocant. `None` for build-time symbol probes — `Receiver`
    /// then evaluates to `None` rather than guessing.
    pub receiver: Option<InferredType>,
    /// Call-site argument types for `ReturnExpr::Arg(n)` substitution — the
    /// positional mirror of `receiver`. `Arg(n)` evaluates to `args[n]`,
    /// `None` when empty (build-time symbol probe / no call site), exactly
    /// as `Receiver` returns `None` without a receiver. Threaded through
    /// edge chases like `receiver`.
    pub args: Vec<InferredType>,
    /// Optional scope topology + per-package framework. Lets the
    /// registry chase `Edge(Variable{...})` with `query_variable_type`
    /// semantics (scope-chain walk + framework fold) instead of a flat
    /// lookup. `None` for context-free queries (tests, self-contained
    /// targets).
    pub context: Option<&'a BagContext<'a>>,
}

/// File-scope context the registry needs to chase edges correctly.
/// Carries the scope tree and per-package framework so materialization
/// can run `query_variable_type` for `Variable` targets — the only edge
/// target whose resolution needs more than the bag itself.
///
/// `module_index` lets materialization recurse into cached modules' bags
/// when a `PackageSymbol{package,...}` names a class in another file.
/// `package_parents` is the per-class inheritance graph (Perl DFS-MRO);
/// the registry walks it for `PackageSymbol{C, m}` queries the local bag
/// can't answer, chasing `PackageSymbol{P, m}` per parent. Both are
/// `None`/empty for in-file callers.
pub struct BagContext<'a> {
    pub scopes: &'a [Scope],
    pub package_framework: &'a dyn crate::model::file_analysis::PackageFrameworks,
    pub module_index: Option<&'a dyn crate::model::file_analysis::CrossFileLookup>,
    pub package_parents: &'a dyn crate::model::file_analysis::LocalParents,
    /// Manifest-declared app-surface consumer classes — threaded so the
    /// `PackageSymbol` inheritance walk injects the synthetic surface
    /// parent via `parents_of`, matching the FA-side ancestor walks.
    /// Empty for in-file callers that don't carry consumer state.
    pub app_surface_consumers: &'a [String],
}

/// A reducer's answer.
///
/// **Adding a variant here is not additive** — so every `match` on it names
/// its variants explicitly, no `_` catch-all, the way
/// `FileAnalysis::surface_feed` destructures with no `..`. A new variant is
/// then a compile error at each site that must decide what it means, instead
/// of compiling everywhere and silently answering `None` — which is how type
/// inference would go dark with nothing to say so. Keep it that way: a
/// catch-all re-introduced here buys one line and costs the enforcement.
///
/// `if let ReducedValue::Type(t) = …` is the same hole wearing a different
/// hat — it falls through silently and no exhaustiveness check reaches it —
/// so those sites are spelled as matches too, even where the extra arm is
/// empty. The payoff is that a variant carrying richer payload (the resolved
/// owner alongside the type — see `docs/adr/skipping-cross-file-work.md`)
/// arrives as a list of compile errors naming every site that must decide.
///
/// `FactMap` is the reserved payload-bearing shape and is deliberately
/// unproduced and unread; reaching for it does not avoid the above, because a
/// reducer that starts returning it stops returning `Type` for those callers.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // FactMap reserved for payload-bearing reducers
pub enum ReducedValue {
    Type(InferredType),
    FactMap(Vec<(String, FactValue)>),
    None,
}

pub trait WitnessReducer: Send + Sync {
    #[allow(dead_code)] // identity for tracing/debug; see module-level note
    fn name(&self) -> &str;

    fn claims(&self, w: &Witness) -> bool;

    fn reduce(&self, ws: &[&Witness], q: &ReducerQuery) -> ReducedValue;
}

// ---- Built-in: framework-aware type-fold reducer ----

/// Folds class / rep / scalar observations into a type:
///
/// 1. `ClassAssertion(Foo)` dominates.
/// 2. `FirstParamInMethod { package }` under a matching framework's
///    backing rep is NOT dethroned by rep observations matching that rep
///    (the Mojo `sub name` bug fix).
/// 3. `BlessTarget(Rep)` pins the rep axis.
/// 4. Rep observations with no class evidence project to flat
///    `HashRef` / `ArrayRef` / `CodeRef`.
/// 5. `NumericUse` / `StringUse` / `RegexpUse` project to their types.
pub struct FrameworkAwareTypeFold;

impl WitnessReducer for FrameworkAwareTypeFold {
    fn name(&self) -> &str {
        "framework_aware_type_fold"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(
            w.attachment,
            WitnessAttachment::Variable { .. } | WitnessAttachment::Expression(_)
        ) && matches!(
            w.payload,
            WitnessPayload::InferredType(_) | WitnessPayload::Observation(_)
        )
    }

    fn reduce(&self, ws: &[&Witness], q: &ReducerQuery) -> ReducedValue {
        // Point-narrowing is variable-lifetime semantics: at the query point,
        // which assignment of `$x` is live. It only makes sense for `Variable`
        // attachments. This reducer also claims `Expression` (a method call's
        // resolved return type) — those carry one witness spanning the call,
        // and filtering them by a point inherited from a chasing variable's
        // scope wrongly discards them (`my $g = X->new->m` chased at `$g`'s
        // scope-end, where the call span doesn't contain it). So only narrow
        // for variables; expressions fold every witness.
        let narrow_point = q
            .point
            .filter(|_| matches!(q.attachment, WitnessAttachment::Variable { .. }));
        // Narrowing: with a `point`, pick the narrowest-span
        // InferredType witness containing it (already post-narrowing).
        // Falls through to the full fold otherwise.
        if let Some(point) = narrow_point {
            // Source priority first (an annotation outranks a flow guess
            // sharing the same class-wide extent), narrowest span second.
            let mut narrow: Option<(&Witness, u64)> = None;
            for w in ws {
                if let WitnessPayload::InferredType(_) = w.payload {
                    if span_contains(&w.span, point) && !span_is_zero(&w.span) {
                        let area = span_area(&w.span);
                        let prio = w.source.priority();
                        let better = narrow.map_or(true, |(nw, a)| {
                            let np = nw.source.priority();
                            prio > np || (prio == np && area < a)
                        });
                        if better {
                            narrow = Some((*w, area));
                        }
                    }
                }
            }
            if let Some((w, _)) = narrow {
                if let WitnessPayload::InferredType(t) = &w.payload {
                    return ReducedValue::Type(t.clone());
                }
            }
        }

        // Class assertions break ties on source priority first, then
        // iteration order — a `Plugin`-sourced assertion (the helper-`$c`
        // override) dominates a `Builder` one (`my $c = shift` typed as the
        // enclosing class). Same axis as `PluginOverrideReducer` on Symbols.
        let mut class_assertion: Option<String> = None;
        let mut class_assertion_priority: u8 = 0;
        let mut first_param_class: Option<String> = None;
        // A `BrandedRoute` is a class identity that carries extra
        // inherited-default data. It must dominate the bare
        // `ClassName(base)` companion that the same assignment also
        // pushes (so a partial route target reads the brand, not the
        // brandless class). Track the latest brand separately and
        // return it ahead of the class axis.
        let mut branded: Option<InferredType> = None;
        let mut rep_obs: Option<Rep> = None;
        let mut bless_rep: Option<Rep> = None;
        let mut num = false;
        let mut str_ = false;
        let mut re = false;
        let mut plain_type: Option<InferredType> = None;
        let mut plain_type_priority: u8 = 0;
        // An `Unknown` ANNOTATION (`@var A|B`, a declared union) is a
        // known-untypable value at its source's priority: it beats a
        // lower-priority guess on the same attachment (a flow default, a
        // constructor write) the way a concrete annotation beats one.
        let mut unknown_priority: u8 = 0;

        // A REASSIGNMENT (`REASSIGN_FLOW_SOURCE`, zero-width at its site —
        // materialized to what it produced, or to `InferredType::Unknown`
        // when its source could not be typed) is a temporal RESET: at the
        // query point, every binding strictly before the latest one is dead
        // — the class axis included, which otherwise wins in any order, so
        // `$r = new WP_Error; $r = json_decode(..)` reads as the array.
        // Companions minted at the same site survive, and observations
        // after it accrue as usual; with nothing typed after it the answer
        // IS `Unknown`, so a chase that reads the variable carries the
        // reset on instead of falling back.
        let reset_at = ws
            .iter()
            .filter(|w| {
                w.span.start == w.span.end
                    && matches!(&w.source, WitnessSource::Builder(t) if t == REASSIGN_FLOW_SOURCE)
            })
            .map(|w| w.span.start)
            .filter(|s| narrow_point.map_or(true, |p| *s <= p))
            .max();

        for w in ws {
            // Temporal ordering: only consider witnesses emitted at or
            // before the query point — a later reassignment shouldn't
            // influence a lookup at an earlier line.
            if let Some(point) = narrow_point {
                if w.span.start > point {
                    continue;
                }
            }
            if reset_at.is_some_and(|r| w.span.start < r) {
                continue;
            }
            // Skip scoped InferredType witnesses that don't contain the
            // query point — narrowing facts for a different slice of the
            // variable's lifetime.
            if let (Some(point), WitnessPayload::InferredType(_)) = (narrow_point, &w.payload) {
                if !span_is_zero(&w.span) && !span_contains(&w.span, point) {
                    continue;
                }
            }
            let prio = w.source.priority();
            match &w.payload {
                WitnessPayload::InferredType(t) => match t {
                    InferredType::ClassName(name) => {
                        if prio >= class_assertion_priority {
                            class_assertion = Some(name.clone());
                            class_assertion_priority = prio;
                        }
                    }
                    InferredType::FirstParam { package } => {
                        first_param_class = Some(package.clone())
                    }
                    b @ InferredType::BrandedRoute { .. } => branded = Some(b.clone()),
                    // The reassignment reset is handled by `reset_at` (it
                    // yields to the observations that follow it); an
                    // annotated `Unknown` competes on priority.
                    InferredType::Unknown => {
                        let reset = matches!(&w.source, WitnessSource::Builder(t) if t == REASSIGN_FLOW_SOURCE);
                        if !reset {
                            unknown_priority = unknown_priority.max(prio);
                        }
                    }
                    // Source priority breaks ties first (an EXPLICIT
                    // annotation — `ANNOT_SOURCE`, priority 20 — governs over
                    // an inferred flow type, priority 10, whatever the order
                    // they land in): the C++ `T x = {…}` braced-init case,
                    // where the initializer's `Numeric` flow witness would
                    // otherwise clobber the declared container type. This is
                    // the same annotation-dominates rule the `ClassName`/
                    // `ClassAssertion` axis above already applies, extended to
                    // every `InferredType` flavor (`Parametric`, `HashRef`,
                    // …). Within equal priority: latest wins UNLESS the
                    // standing answer subsumes the newcomer — structure
                    // dominates rep (`HashWithKeys` is not downgraded by a
                    // deref's re-derived `HashRef`), mirroring class-over-rep.
                    other => {
                        let subsumed = plain_type
                            .as_ref()
                            .is_some_and(|have| have.subsumes_narrowing(other));
                        if prio > plain_type_priority || (prio == plain_type_priority && !subsumed) {
                            plain_type = Some(other.clone());
                            plain_type_priority = prio;
                        }
                    }
                },
                WitnessPayload::Observation(obs) => match obs {
                    TypeObservation::ClassAssertion(name) => {
                        if prio >= class_assertion_priority {
                            class_assertion = Some(name.clone());
                            class_assertion_priority = prio;
                        }
                    }
                    TypeObservation::FirstParamInMethod { package } => {
                        first_param_class = Some(package.clone())
                    }
                    TypeObservation::HashRefAccess => rep_obs = merge_rep(rep_obs, Rep::Hash),
                    TypeObservation::ArrayRefAccess => rep_obs = merge_rep(rep_obs, Rep::Array),
                    TypeObservation::CodeRefInvocation => rep_obs = merge_rep(rep_obs, Rep::Code),
                    TypeObservation::BlessTarget(r) => bless_rep = Some(*r),
                    TypeObservation::NumericUse => num = true,
                    TypeObservation::StringUse => str_ = true,
                    TypeObservation::RegexpUse => re = true,
                },
                _ => {}
            }
        }

        // A branded route dominates the bare-class companion: the
        // brand IS the class identity plus inherited defaults.
        if let Some(b) = branded {
            return ReducedValue::Type(b);
        }
        if unknown_priority > class_assertion_priority.max(plain_type_priority) {
            return ReducedValue::Type(InferredType::Unknown);
        }

        // Class axis wins when consistent with the rep axis. On
        // contradiction or unknown rep, still return the class — the
        // user's intent is object-typed use; a rep mismatch is a
        // separate diagnostic.
        if let Some(name) = class_assertion.clone().or(first_param_class.clone()) {
            let backing = bless_rep.or_else(|| q.framework.backing_rep());
            match (rep_obs, backing) {
                (None, _) => return ReducedValue::Type(InferredType::ClassName(name)),
                (Some(obs), Some(b)) if obs == b => {
                    return ReducedValue::Type(InferredType::ClassName(name));
                }
                (Some(obs), None) => {
                    let _ = obs;
                    return ReducedValue::Type(InferredType::ClassName(name));
                }
                (Some(obs), Some(b)) => {
                    let _ = (obs, b);
                    return ReducedValue::Type(InferredType::ClassName(name));
                }
            }
        }

        // Explicit assignments dominate rep observations — `my $x = []`
        // overrides an earlier `$x->{k}` inference because reassignment
        // breaks the binding. Latest by iteration order (source order).
        if let Some(t) = plain_type {
            return ReducedValue::Type(t);
        }

        // No class evidence, no plain type — project rep observations.
        if let Some(r) = rep_obs.or(bless_rep) {
            return ReducedValue::Type(match r {
                Rep::Hash => InferredType::HashRef,
                Rep::Array => InferredType::ArrayRef,
                Rep::Code => InferredType::CodeRef { return_edge: None },
                Rep::Scalar => InferredType::String,
            });
        }

        // Scalar-context observations.
        if re {
            return ReducedValue::Type(InferredType::Regexp);
        }
        if num {
            return ReducedValue::Type(InferredType::Numeric);
        }
        if str_ {
            return ReducedValue::Type(InferredType::String);
        }

        if reset_at.is_some() {
            return ReducedValue::Type(InferredType::Unknown);
        }
        ReducedValue::None
    }
}

fn span_contains(span: &Span, point: tree_sitter::Point) -> bool {
    span.start <= point && point <= span.end
}

fn span_is_zero(span: &Span) -> bool {
    span.start == span.end
}

/// "Area" measure — rows * many + cols. Used only for picking the
/// narrowest span; overflow isn't a concern for Perl source.
fn span_area(span: &Span) -> u64 {
    let rows = span.end.row.saturating_sub(span.start.row) as u64;
    if rows == 0 {
        span.end.column.saturating_sub(span.start.column) as u64
    } else {
        rows * 10_000 + (span.end.column as u64)
    }
}

fn merge_rep(existing: Option<Rep>, new: Rep) -> Option<Rep> {
    match existing {
        None => Some(new),
        Some(r) if r == new => Some(r),
        // Conflict shouldn't really fire; prefer the newer observation
        // and leave it for a later diagnostic.
        Some(_) => Some(new),
    }
}

// ---- Branch-arm fold reducer (ternary `$c ? A : B`) ----

/// Folds a ternary's per-arm types on the `BranchArm(span)` attachment.
/// Agreement across ≥2 arms → that type; a single arm → None (ternaries
/// always carry both arms syntactically, so one witness means inference
/// for the other arm failed). Claims by attachment shape — the ternary's
/// `Expr(span)` carries one `Edge(BranchArm(span))` so a query on the
/// expression materializes this fold's answer. Symbol-attached return
/// arms go through `SymbolReturnArmFold` instead (1+ arms rule).
pub struct BranchArmFold;

impl WitnessReducer for BranchArmFold {
    fn name(&self) -> &str {
        "branch_arm_fold"
    }

    fn claims(&self, w: &Witness) -> bool {
        if !matches!(w.attachment, WitnessAttachment::BranchArm(_)) {
            return false;
        }
        match &w.payload {
            WitnessPayload::InferredType(_) => true,
            WitnessPayload::Fact { family, .. } => family == "undef_arm",
            _ => false,
        }
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        let mut typed: Vec<InferredType> = Vec::new();
        // `||` / `//` fallback (RHS) arms — the guaranteed floor, tagged with
        // a distinct source at emission so this fold can prefer them.
        let mut fallback: Vec<InferredType> = Vec::new();
        let mut undef_arms = 0usize;
        for w in ws {
            let is_fallback =
                matches!(&w.source, WitnessSource::Builder(t) if t == "fallback_arm");
            match &w.payload {
                WitnessPayload::InferredType(t) if is_fallback => fallback.push(t.clone()),
                WitnessPayload::InferredType(t) => typed.push(t.clone()),
                WitnessPayload::Fact { family, .. } if family == "undef_arm" => undef_arms += 1,
                _ => {}
            }
        }
        // `||` / `//`: the RHS floor is returned whenever the LHS is
        // falsy/undef, so the expression's type is at least the fallback's.
        // Prefer agreement across all known arms; else the known floor; else
        // the known LHS. This is what lets `$ENV{X} || 10` type to `Numeric`
        // even when the LHS hash access can't be resolved — an honest,
        // reachable type beats the entry vanishing.
        if !fallback.is_empty() {
            let all: Vec<&InferredType> = typed.iter().chain(fallback.iter()).collect();
            if let Some((first, rest)) = all.split_first() {
                if rest.iter().all(|t| *t == *first) {
                    return ReducedValue::Type((*first).clone());
                }
            }
            if let Some(fb) = fallback.into_iter().next() {
                return ReducedValue::Type(fb);
            }
            if let Some(l) = typed.into_iter().next() {
                return ReducedValue::Type(l);
            }
            return ReducedValue::None;
        }
        // Both arms must have contributed — the ≥2 rule guards a single
        // materialized arm from masquerading as agreement.
        if typed.len() + undef_arms < 2 {
            return ReducedValue::None;
        }
        // Strict agreement among the typed arms (a ternary wants exact
        // agreement, NOT the loose hash/object subsumption the return-arm
        // join uses). An `undef` arm then lifts the agreed `T` to
        // `Optional<T>`.
        let agreed = match typed.split_first() {
            Some((first, rest)) if rest.iter().all(|t| t == first) => Some(first.clone()),
            _ => None,
        };
        match agreed {
            Some(t) if undef_arms > 0 && !matches!(t, InferredType::Optional(_)) => {
                ReducedValue::Type(InferredType::Optional(Box::new(t)))
            }
            Some(t) => ReducedValue::Type(t),
            None => ReducedValue::None,
        }
    }
}

// ---- Symbol-attached return-arm fold ----
//
// Claims `SymbolReturnArm(sub_id)` attachments carrying `InferredType`
// payloads (Edges materialized into types). Each witness is one return
// arm; `resolve_return_type` agrees them (1 arm → that type, agreeing →
// that type, disagreeing → None, HashRef subsumed by Object).
// `Symbol(sub_id)` carries an `Edge(SymbolReturnArm(sub_id))` chain so
// consumers querying the symbol see the arm-fold answer via standard
// edge materialization.

pub struct SymbolReturnArmFold;

impl WitnessReducer for SymbolReturnArmFold {
    fn name(&self) -> &str {
        "symbol_return_arm_fold"
    }

    fn claims(&self, w: &Witness) -> bool {
        if !matches!(w.attachment, WitnessAttachment::SymbolReturnArm(_)) {
            return false;
        }
        match &w.payload {
            WitnessPayload::InferredType(_) => true,
            // The `return undef` arm marker (no rvalue type to materialize).
            WitnessPayload::Fact { family, .. } => family == "undef_arm",
            _ => false,
        }
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        let mut arms: Vec<InferredType> = Vec::new();
        let mut has_undef_arm = false;
        for w in ws {
            match &w.payload {
                WitnessPayload::InferredType(t) => arms.push(t.clone()),
                WitnessPayload::Fact { family, .. } if family == "undef_arm" => {
                    has_undef_arm = true
                }
                _ => {}
            }
        }
        match crate::model::file_analysis::join_return_arms(&arms, has_undef_arm) {
            Some(t) => ReducedValue::Type(t),
            None => ReducedValue::None,
        }
    }
}

// ---- Typed-slot fold ----
//
// Claims `SlotType{class, key}` attachments carrying `InferredType`
// payloads (per-write `Edge(Expr(rhs_span))` witnesses, materialized to
// types by the registry). Each witness is one `$obj->{key} = <rhs>`
// WRITE; the per-write arms agree via `resolve_return_type` (1+ agree →
// that type, HashRef subsumed by an Object) with one added guard: two
// DISTINCT concrete classes are honest disagreement → None. The guard
// matters here because `resolve_return_type`'s Object/HashRef
// subsumption was tuned for return arms (one Object absorbs sibling
// HashRefs) and otherwise picks the last of two different classes —
// the wrong answer when `$self->{h} = A->new` in one method and
// `= B->new` in another genuinely conflict. Nothing consumes this
// attachment yet — it's the typed half of the hash-key-write seed,
// paired with the untyped `mutation` Fact.

pub struct SlotTypeFold;

impl WitnessReducer for SlotTypeFold {
    fn name(&self) -> &str {
        "slot_type_fold"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(w.attachment, WitnessAttachment::SlotType { .. })
            && matches!(w.payload, WitnessPayload::InferredType(_))
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        let arms: Vec<InferredType> = ws
            .iter()
            .filter_map(|w| match &w.payload {
                WitnessPayload::InferredType(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        // Two distinct class identities never agree: a slot can't be
        // both `A` and `B`. `class_name()` is the value's own "what
        // class am I" answer (rule #10) — distinct answers → None.
        let mut seen_class: Option<String> = None;
        for t in &arms {
            if let Some(cn) = t.class_name() {
                match &seen_class {
                    None => seen_class = Some(cn.to_string()),
                    Some(prev) if prev != cn => return ReducedValue::None,
                    _ => {}
                }
            }
        }
        match crate::model::file_analysis::resolve_return_type(&arms) {
            Some(t) => ReducedValue::Type(t),
            None => ReducedValue::None,
        }
    }
}

// Sub-return delegation chains (`return other()`, `shift->method(...)`)
// are `Edge(Symbol(...))` / `Edge(PackageSymbol{...})` payloads; registry
// materialization chases them, so no procedural delegation pass remains.
// `TypeProvenance::Delegation` is recorded at synthesis time by the
// emitter that pushes the Edge, and preserved across worklist iterations.

// ---- Expression reducer ----
//
// Claims `InferredType` payloads on `Expr(_)` — the unified
// expression-result attachment every rvalue publishes through. The
// walker pushes `Type(t)` directly or `Edge(target)` for resolution;
// edges are materialized by the registry before any reducer claims, so
// this reducer always sees plain types. Latest-wins: `emit_expr_witness`
// runs from multiple walk sites, so the same node may receive several
// witnesses; reading from the back picks the most recent.

pub struct ExprReturn;

impl WitnessReducer for ExprReturn {
    fn name(&self) -> &str {
        "expr_return"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(w.attachment, WitnessAttachment::Expr(_))
            && matches!(w.payload, WitnessPayload::InferredType(_))
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        for w in ws.iter().rev() {
            if let WitnessPayload::InferredType(t) = &w.payload {
                return ReducedValue::Type(t.clone());
            }
        }
        ReducedValue::None
    }
}

// ---- Symbol return reducer ----
//
// Claims plain `InferredType` payloads on `Symbol(_)` — the id-keyed
// "what does THIS sym return?" answer. Pushed by writeback (local
// subs/methods) and hand-crafted test FileAnalyses. Class-scoped
// multi-overload dispatch goes through `PackageSymbol{package, name}`
// instead. Latest wins, so a later writeback re-publish dominates;
// registered AFTER every more-precise reducer (plugin override,
// ReturnExpr arity dispatch) so those claim first. Per-arm answers route
// through `SymbolReturnArm(_)`; `Symbol(sub_id)` carries an
// `Edge(SymbolReturnArm(_))` that materializes the arm-fold answer here.

pub struct SubReturnReducer;

impl WitnessReducer for SubReturnReducer {
    fn name(&self) -> &str {
        "sub_return"
    }

    fn claims(&self, w: &Witness) -> bool {
        // `Symbol(_) + InferredType`, latest-wins. No source-tag filter
        // — the claim discriminator lives on the attachment shape.
        matches!(w.attachment, WitnessAttachment::Symbol(_))
            && matches!(w.payload, WitnessPayload::InferredType(_))
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        for w in ws.iter().rev() {
            if let WitnessPayload::InferredType(t) = &w.payload {
                return ReducedValue::Type(t.clone());
            }
        }
        ReducedValue::None
    }
}

// ---- Class-keyed method-on-class reducer ----
//
// Claims `PackageSymbol{package, name}` carrying a plain `InferredType` —
// the class-scoped, name-keyed default return. `write_back_sub_return_types`
// publishes the primary as `Edge(Symbol(sid))` (materialized to a type
// before this reducer sees it). `ReturnExprReducer` runs first and
// handles arity-aware / receiver-relative dispatch; this fires only when
// no symbol-declarative ReturnExpr answers. Latest-wins: writeback clears
// its `local_return` / `plugin_bridge` / `inheritance` witnesses each
// fold iteration and re-publishes from current state.

pub struct PackageSymbolReducer;

impl WitnessReducer for PackageSymbolReducer {
    fn name(&self) -> &str {
        "method_on_class"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(w.attachment, WitnessAttachment::PackageSymbol { .. })
            && matches!(w.payload, WitnessPayload::InferredType(_))
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        for w in ws.iter().rev() {
            if let WitnessPayload::InferredType(t) = &w.payload {
                return ReducedValue::Type(t.clone());
            }
        }
        ReducedValue::None
    }
}

// Claims `TypeName(name)` carrying a plain `InferredType` — a typedef/using
// alias's resolved underlying type. The typedef site pushes the leaf value
// (`ClassName("unsigned short")`, `Numeric`) or an `Edge(TypeName(_))` for an
// alias chain; the edge is materialized before this reducer sees the list, so
// a chain resolves through the generic chase. Latest-wins. The terminal
// `ClassName(name)` fallback (unresolved alias IS a type of that name) lives
// in `query_rec_body`, not here — `reduce` only runs when a witness matched.

pub struct TypeNameReducer;

impl WitnessReducer for TypeNameReducer {
    fn name(&self) -> &str {
        "type_name"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(w.attachment, WitnessAttachment::TypeName(_))
            && matches!(w.payload, WitnessPayload::InferredType(_))
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        for w in ws.iter().rev() {
            if let WitnessPayload::InferredType(t) = &w.payload {
                return ReducedValue::Type(t.clone());
            }
        }
        ReducedValue::None
    }
}

// ---- Domain-coherence fold (int-used-as-enum) ----
//
// Claims `Field{owner, name}` carrying `DomainCompare{enum_type}` — the
// per-site evidence that a storage slot is *used as* a value of some enum.
// Folds every site project-wide: a slot mostly compared/assigned against
// one enum HAS that enum as its domain; a truly-mixed slot has none. The
// `FrameworkAwareTypeFold` pattern retargeted (observations → a folded
// verdict), here as a majority vote. Deterministic: counts collect into a
// `BTreeMap` (sorted keys) and ties break by enum name ascending, so the
// verdict never depends on witness-push order or HashMap iteration.
//
// The domain is defeasible — it refines the human surfaces (hover / the
// navigation bridge), never the storage type that flows. Nothing on the
// flow axis (Variable/Expr/Symbol/PackageSymbol) queries `Field`, so
// returning the domain as a `ClassName` here can't leak into flow typing.

pub struct DomainCoherenceFold;

impl WitnessReducer for DomainCoherenceFold {
    fn name(&self) -> &str {
        "domain_coherence_fold"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(w.attachment, WitnessAttachment::Field { .. })
            && matches!(w.payload, WitnessPayload::DomainCompare { .. })
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        match domain_coherence(ws) {
            Some((domain, _count, _total)) => {
                ReducedValue::Type(InferredType::ClassName(domain))
            }
            None => ReducedValue::None,
        }
    }
}

/// The coherence vote over `DomainCompare` witnesses: the dominant enum,
/// its site count, and the total, when the dominant share is a strict
/// majority over ≥2 sites; else `None`. The total counts EVERY site,
/// including `enum_type: None` counter-evidence — the denominator is the
/// slot's whole interaction story, so the enum must be a majority of all
/// uses, not of the enum-shaped subset. Deterministic (sorted counts,
/// name-ascending tie-break). Shared by the reducer and the query method
/// (which reports `confidence = count / total`).
pub fn domain_coherence(ws: &[&Witness]) -> Option<(String, usize, usize)> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total: usize = 0;
    for w in ws {
        if let WitnessPayload::DomainCompare { enum_type } = &w.payload {
            total += 1;
            if let Some(e) = enum_type {
                *counts.entry(e.clone()).or_default() += 1;
            }
        }
    }
    if total < 2 {
        return None;
    }
    // Dominant enum: BTreeMap iterates keys ascending, and we keep the
    // incumbent on ties (`v <= best`), so the smallest-named enum wins a
    // tie — a stable, source-order-independent choice.
    let mut best: Option<(&String, usize)> = None;
    for (k, v) in &counts {
        if best.is_none_or(|(_, bv)| *v > bv) {
            best = Some((k, *v));
        }
    }
    let (dom, dom_count) = best?;
    // Mostly-agree: a strict majority. op_type shows ~99.9% coherence
    // on real op_type, so any threshold in (0.5, 0.99) fires cleanly;
    // majority is the simplest defensible line.
    if dom_count * 2 > total {
        Some((dom.clone(), dom_count, total))
    } else {
        None
    }
}

// ---- Plugin-override priority reducer ----
//
// Claims `Symbol(_)` attachments with an `InferredType` from a
// high-priority source (Plugin). When such a witness exists it
// short-circuits the symbol-return fold — overrides dominate inference.
// Registered first so its short-circuit beats every inferred fold.
//
// Kept a distinct, named reducer (rather than a branch in
// FrameworkAwareTypeFold) so (a) the `claims` predicate only fires on a
// priority>10 witness, not every Symbol+InferredType fact, and (b)
// dump-package can attribute the answer to `plugin_override` specifically.

pub struct PluginOverrideReducer;

impl WitnessReducer for PluginOverrideReducer {
    fn name(&self) -> &str {
        "plugin_override"
    }

    fn claims(&self, w: &Witness) -> bool {
        matches!(w.attachment, WitnessAttachment::Symbol(_))
            && matches!(w.payload, WitnessPayload::InferredType(_))
            && w.source.priority() > 10
    }

    fn reduce(&self, ws: &[&Witness], _q: &ReducerQuery) -> ReducedValue {
        // Highest priority wins; ties go to the last pushed.
        let mut best: Option<(&Witness, u8)> = None;
        for w in ws {
            let pr = w.source.priority();
            match best {
                None => best = Some((*w, pr)),
                Some((_, prev)) if pr >= prev => best = Some((*w, pr)),
                _ => {}
            }
        }
        if let Some((w, _)) = best {
            if let WitnessPayload::InferredType(t) = &w.payload {
                return ReducedValue::Type(t.clone());
            }
        }
        ReducedValue::None
    }
}

// ---- Return-expression reducer ----
//
// Claims `Symbol(_)` and `PackageSymbol{...}` attachments carrying a
// `ReturnExpr(_)` payload — the symbol-declarative return machinery.
// Substitutes `q.receiver` for `Receiver`, dispatches `UnionOnArgs`
// against `q.arity_hint`, and evaluates `Operator(RowOf(_))`. Registered
// before `PackageSymbolReducer` / `SubReturnReducer` so declarative
// answers dominate primary-sym writeback.
//
// Per CLAUDE.md #10: no peeking at method names, classes, or payloads
// beyond `q.receiver` / `q.arity_hint` — the sub's policy lives entirely
// in the pushed `ReturnExpr`. Highest-priority source wins, ties to
// latest-pushed, so a plugin re-publish dominates a build-time inference.

pub struct ReturnExprReducer;

impl WitnessReducer for ReturnExprReducer {
    fn name(&self) -> &str {
        "return_expr"
    }

    fn claims(&self, w: &Witness) -> bool {
        // The policy lives on the payload, not the attachment: a
        // `ReturnExpr(_)` is a deferred (receiver/arity)-relative return
        // wherever it's pushed. `Symbol(_)` / `PackageSymbol{..}` carry
        // the class-keyed declarations; `Expr(_)` carries a method body's
        // own deferred return (`sub me { return $_[0] }` → `Receiver` on
        // the `$_[0]` body span), reached through the `SymbolReturnArm`
        // edge chase; `Variable{..}` carries a statement-position
        // receiver bless (`bless $obj, $class; ...; return $obj` — the
        // deferred class rides the VARIABLE so the return-arm chase
        // substitutes the call site's receiver). Claiming all four lets a
        // self-returning method substitute the call's receiver at
        // arbitrary chain depth.
        matches!(
            w.attachment,
            WitnessAttachment::Symbol(_)
                | WitnessAttachment::PackageSymbol { .. }
                | WitnessAttachment::Expr(_)
                | WitnessAttachment::Variable { .. }
        ) && matches!(w.payload, WitnessPayload::ReturnExpr(_))
    }

    fn reduce(&self, ws: &[&Witness], q: &ReducerQuery) -> ReducedValue {
        // Highest-priority source wins; ties resolve to latest-pushed.
        // Variable-attached witnesses are temporal like the framework
        // fold: a bless mid-body types the variable only PAST the bless
        // site — a query before it keeps the rep answer (HashRef pre-,
        // instance post-bless).
        let temporal_gate = |w: &&Witness| match (&w.attachment, q.point) {
            (WitnessAttachment::Variable { .. }, Some(p)) => {
                let s = w.span.start;
                (s.row, s.column) <= (p.row, p.column)
            }
            _ => true,
        };
        let mut best: Option<(&Witness, u8)> = None;
        for w in ws.iter().rev().filter(|w| temporal_gate(w)) {
            let pr = w.source.priority();
            match best {
                None => best = Some((*w, pr)),
                Some((_, prev)) if pr > prev => best = Some((*w, pr)),
                _ => {}
            }
        }
        let Some((w, _)) = best else {
            return ReducedValue::None;
        };
        let WitnessPayload::ReturnExpr(re) = &w.payload else {
            return ReducedValue::None;
        };
        match eval_return_expr(re, q) {
            Some(t) => ReducedValue::Type(t),
            None => ReducedValue::None,
        }
    }
}

/// Evaluate a `ReturnExpr` against a query. Pure substitution — the only
/// context read is `q.receiver` / `q.arity_hint`. Returns `None` when:
///   - `Receiver` is encountered but `q.receiver` is `None`.
///   - No `UnionOnArgs` branch matches `q.arity_hint`.
///   - An operator's sub-expression evaluates to `None`, or to a type
///     the operator can't project (`RowOf(NotAResultSet)` → `None`).
pub(super) fn eval_return_expr(re: &ReturnExpr, q: &ReducerQuery) -> Option<InferredType> {
    match re {
        ReturnExpr::Concrete(t) => Some(t.clone()),
        ReturnExpr::Receiver => q.receiver.clone(),
        ReturnExpr::Arg(i) => q.args.get(*i as usize).cloned(),
        ReturnExpr::ReceiverOr(fallback) => {
            Some(q.receiver.clone().unwrap_or_else(|| fallback.clone()))
        }
        ReturnExpr::Operator(op) => match op {
            ParametricOp::RowOf(inner) => {
                // Project eagerly: `RowOf` only has meaning over a
                // `ResultSet` (→ its row class). Any other operand has no
                // row dimension, so → `None`. A projected row is a plain
                // class with no further row dimension, so nested
                // `RowOf<RowOf<…>>` correctly bottoms out at `None`.
                match eval_return_expr(inner, q)? {
                    InferredType::Parametric(ParametricType::ResultSet { row, .. }) => {
                        Some(InferredType::ClassName(row))
                    }
                    _ => None,
                }
            }
            ParametricOp::ParamOf { index, of } => {
                // The receiver's i-th type argument. Only a template
                // `Instance` carries the positional-arg axis; anything
                // else has no i-th parameter, so → `None` (a bare
                // `ClassName(Box)` receiver honestly can't answer what
                // `T` is).
                match eval_return_expr(of, q)? {
                    InferredType::Parametric(ParametricType::Instance { args, .. }) => {
                        args.get(*index as usize).cloned()
                    }
                    _ => None,
                }
            }
            ParametricOp::InstanceOf { base, args } => {
                let mut out = Vec::with_capacity(args.len());
                for a in args {
                    out.push(eval_return_expr(a, q)?);
                }
                Some(InferredType::Parametric(ParametricType::Instance {
                    base: base.clone(),
                    args: out,
                }))
            }
        },
        ReturnExpr::UnionOnArgs { branches } => {
            // First-match wins when the hint is concrete.
            if q.arity_hint.is_some() {
                for (guard, sub) in branches {
                    if guard.matches(q.arity_hint) {
                        return eval_return_expr(sub, q);
                    }
                }
                return None;
            }
            // Hint-less query (introspection / class-keyed lookup with no
            // call site): prefer the `Any` arm (the union's catch-all),
            // else fall back to `Empty` (the typical primary for Mojo
            // `has` getter+writer pairs and DBIC accessors). Keeps
            // per-call-site dispatch strict while answering introspection.
            for (guard, sub) in branches {
                if matches!(guard, ArgGuard::Any) {
                    return eval_return_expr(sub, q);
                }
            }
            for (guard, sub) in branches {
                if matches!(guard, ArgGuard::Empty) {
                    return eval_return_expr(sub, q);
                }
            }
            None
        }
    }
}
