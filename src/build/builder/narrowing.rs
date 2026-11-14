//! Flow-sensitive narrowing — guard recognition, span-scoped emission, and
//! the declarative `@flow` value-flow pass (FlowEdge minting + lowering), so
//! `docs/adr/flow-narrowing.md` maps to exactly this part.
//!
//! A child module of `builder` (so it keeps direct access to `Builder`'s
//! private fields) rather than a sibling: narrowing is still part of the
//! single tree-sitter consumer (rule #1) — `build()` drives it, it just
//! lives in its own file. Recognition (`recognize_*`) is pure CST
//! inspection; emission + span/truncation are `Builder` methods.
//! Decisions (engine-is-emission, truncation soundness, polarity +
//! the `Undef` negative lattice): `docs/adr/flow-narrowing.md`.


use tree_sitter::{Node, Point};

use crate::cst::node_to_span;
use crate::model::file_analysis::{Access, Binding, InferredType, Place, PlaceDisturbanceIndex, ScopeId, Span};

use super::extract::ListSlot;
use super::{connector_keyword_between, raw_leading_op, raw_mid_op, Builder};

/// One `@flow` match, read through the capture vocabulary. `source` is
/// optional: a clear (`@flow.clear`) carries no inflowing value.
pub(super) struct FlowMatch<'t> {
    target: Option<Node<'t>>,
    clear: Option<Node<'t>>,
    rebind: Option<Node<'t>>,
    source: Option<Node<'t>>,
}

impl<'t> FlowMatch<'t> {
    /// A match's captures; `None` when it binds nothing.
    pub(super) fn of(caps: impl IntoIterator<Item = (Option<crate::build::query_extract::Capture>, Node<'t>)>) -> Option<Self> {
        use crate::build::query_extract::{Capture, FlowCap};
        let mut m = FlowMatch { target: None, clear: None, rebind: None, source: None };
        for (cap, node) in caps {
            match cap {
                Some(Capture::Flow(FlowCap::Target)) => m.target = Some(node),
                Some(Capture::Flow(FlowCap::Clear)) => m.clear = Some(node),
                Some(Capture::Flow(FlowCap::Rebind)) => m.rebind = Some(node),
                Some(Capture::Flow(FlowCap::Source)) => m.source = Some(node),
                other => unreachable!("flow_query admits only the captures read here, not {other:?}"),
            }
        }
        m.target.or(m.clear).or(m.rebind).is_some().then_some(m)
    }
}

/// The subject a guard narrows — a variable or a part of one (`$x`,
/// `$self->{x}`, `$h{k}`, `$self->{$k}`), named by its `Place`. `key_vars`
/// spell the place's key variables (`$k` in `$self->{$k}`): rebinding one
/// moves the place, so the narrowing ends there. `spelling` is for display.
#[derive(Clone)]
pub(super) enum NarrowSubject {
    Place { place: Place, spelling: String, key_vars: Vec<String> },
    /// A variable holding a `ref`/`reftype`/`blessed` result (`$ref` in
    /// `$ref eq 'ARRAY'`): the guard is on whatever that tag names, which
    /// only the bag can say once `$ref`'s value has folded.
    TagOf(Place),
}

impl NarrowSubject {
    /// The subject's identity, for dedup across arms and the guard record.
    fn subject(&self) -> &Place {
        match self {
            NarrowSubject::Place { place, .. } | NarrowSubject::TagOf(place) => place,
        }
    }

    fn spelling(&self) -> &str {
        match self {
            NarrowSubject::Place { spelling, .. } => spelling,
            NarrowSubject::TagOf(_) => "",
        }
    }
}

/// Binds a place's spellings where the walk stands.
struct WalkBinder<'b, 'a>(&'b Builder<'a>);

impl crate::cst::PlaceBinder for WalkBinder<'_, '_> {
    fn root(&mut self, spelling: &str, at: Point) -> Option<Binding> {
        Some(self.0.binding_key_here(spelling, at))
    }
    fn key(&mut self, spelling: &str, at: Point) -> Binding {
        self.0.binding_key_here(spelling, at)
    }
}

/// Binds a surveyed place's spellings by position, for the roots some
/// narrowing is about: a root no subject names cannot disturb one, so it
/// is never resolved.
struct SurveyBinder<'b, 'a, 's> {
    builder: &'b Builder<'a>,
    scopes: super::infra::ScopeCursor<'s>,
    wanted: &'b std::collections::HashSet<String>,
}

impl crate::cst::PlaceBinder for SurveyBinder<'_, '_, '_> {
    fn root(&mut self, spelling: &str, at: Point) -> Option<Binding> {
        let key = crate::model::conventions::name_match_key(spelling, &crate::model::conventions::PERL_SPELLINGS);
        if !self.wanted.contains(&key) {
            return None;
        }
        Some(self.key(spelling, at))
    }
    fn key(&mut self, spelling: &str, at: Point) -> Binding {
        let scope = self.scopes.at(at);
        self.builder.binding_key_in(spelling, at, scope)
    }
}

/// What a guard does to the subject's type where it holds.
///
/// TODO(perl-guard-proofs): the regions these mint carry no
/// `WitnessPayload::Proof`, which only a bound reads and Perl declares none.
/// The first Perl bound (docs/adr/declared-bounds.md) needs them minted
/// beside each region, as the pack narrowing does, or a guard under it
/// reads by the write rule.
#[derive(Clone)]
pub(super) enum NarrowOp {
    /// `isa` / `ref…eq` prove a concrete type.
    To(InferredType),
    /// `defined` / `blessed` prove the value the guard `read` once set: the
    /// region holds `Projected { base: Expr(read), step: Defined }`, an edge
    /// the registry resolves when asked. The read sits in the guard, outside
    /// every region the guard decides, so it never reads its own narrowing.
    Defined { read: Span },
    /// The tag a [`NarrowSubject::TagOf`] holds equals this literal.
    TagEq(String),
}

impl NarrowOp {
    /// The op for the region where the guard is FALSE. Only `defined`/
    /// `blessed` have a representable complement — the subject is `undef`
    /// there. "Not a class" (`isa`/`ref-eq` negated) has no positive
    /// target, so it self-suppresses (`None` → emit nothing, wide type
    /// wins). See `docs/adr/flow-narrowing.md` negative-polarity rows.
    fn negated(&self) -> Option<NarrowOp> {
        match self {
            NarrowOp::To(_) | NarrowOp::TagEq(_) => None,
            NarrowOp::Defined { .. } => Some(NarrowOp::To(InferredType::Undef)),
        }
    }

    /// The region's value as a witness payload, for an op that states one
    /// at walk time; a tag guard's waits for its tag to fold.
    fn payload(self) -> Option<crate::model::witnesses::WitnessPayload> {
        use crate::model::witnesses::{ProjectionStep, WitnessAttachment, WitnessPayload};
        match self {
            NarrowOp::To(ty) => Some(WitnessPayload::InferredType(ty)),
            NarrowOp::Defined { read } => Some(WitnessPayload::Projected {
                base: WitnessAttachment::Expr(read),
                step: ProjectionStep::Defined,
            }),
            NarrowOp::TagEq(_) => None,
        }
    }
}

/// A recognized guard fact: the subject, what the guard proves, and
/// whether the proof holds where the guard *expression* is TRUE (`!`
/// flips it).
pub(super) struct GuardFact<'a> {
    subject: NarrowSubject,
    op: NarrowOp,
    asserts_when_true: bool,
    /// The subject's read in the guard.
    read: Node<'a>,
}

impl GuardFact<'_> {
    /// The op to emit in a region where the guard holds iff the guard
    /// expression is `holds`: the fact's own op when the polarity matches
    /// (positive narrowing), else its negation (which may be
    /// unrepresentable → `None`, so the region stays wide).
    fn op_for_region(&self, holds: bool) -> Option<NarrowOp> {
        if self.asserts_when_true == holds {
            Some(self.op.clone())
        } else {
            self.op.negated()
        }
    }
}

/// A pending tag guard (`$ref eq 'ARRAY'`): re-derived each fold iteration,
/// since what `var` holds is a fold answer.
#[derive(Clone)]
pub(super) struct TagNarrowing {
    var: Place,
    scope: ScopeId,
    region: Span,
    lit: String,
}

/// What holds over a region of a subject's lifetime: a guard's proof, or
/// a use's evidence. Both share the region and its cutoffs; only the
/// payload's strength differs — a proof outranks every other belief, a
/// use ranks below a value and a class.
pub(super) enum RegionFact {
    Guard(NarrowOp),
    Observe(crate::model::witnesses::TypeObservation),
}

/// A recognized narrowing whose region cutoff is computed AFTER the walk
/// (`apply_narrowing_cutoffs`), against indexes built once per file: the
/// scalar rebinds from the FlowEdges, the place disturbances from one tree
/// pass. A cutoff searched per fact is a scan per use, and every use
/// observes. `scope` is captured at recognition time (the walk's
/// `current_scope`), invisible post-walk.
pub(super) struct PendingNarrow {
    subject: NarrowSubject,
    fact: RegionFact,
    region: Span,
    scope: ScopeId,
}


/// Map a `ref(...) eq STRING` right-hand string to the type it proves.
/// The builtin reftype tokens are a fixed Perl language constant (reading
/// a grammar constant, not a rule-#10 shape-table over an open behavior
/// set); tokens with no lattice variant yield `None` (recognized, not
/// narrowable). Anything else is a class name (a blessed ref).
fn ref_string_to_type(s: &str) -> Option<InferredType> {
    Some(match s {
        "HASH" => InferredType::HashRef,
        "ARRAY" => InferredType::ArrayRef,
        "CODE" => InferredType::CodeRef { return_edge: None },
        "Regexp" => InferredType::Regexp,
        "SCALAR" | "REF" | "GLOB" | "LVALUE" | "FORMAT" | "IO" | "VSTRING" => return None,
        other => InferredType::ClassName(other.to_string()),
    })
}

/// What a tag from `via` equal to `s` proves: `ref` names a reftype token
/// or a class, `reftype` only a token, `blessed` only a class.
fn tag_string_to_type(via: crate::model::file_analysis::RefTagFn, s: &str) -> Option<InferredType> {
    use crate::model::file_analysis::RefTagFn;
    match via {
        RefTagFn::Ref => ref_string_to_type(s),
        RefTagFn::RefType => ref_string_to_type(s).filter(|t| !t.is_object()),
        RefTagFn::Blessed => (!s.is_empty()).then(|| InferredType::ClassName(s.to_string())),
    }
}

/// A narrowable subject node: a plain scalar, or a constant hash/array
/// place element (`$self->{x}`).
fn is_subject_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "scalar" | "hash_element_expression" | "array_element_expression"
    )
}

/// The subject argument of a `func1op_call_expression` (`ref($x)` /
/// `ref $self->{x}`) — the first scalar/place operand, groups peeled.
/// Does NOT descend into a call/deref (`ref(f($x))` is `f`'s return, not
/// `$x`), so the group-peeling child finder is exactly right.
fn func1op_subject_arg<'a>(node: Node<'a>) -> Option<Node<'a>> {
    crate::cst::first_named_child_where(node, is_subject_node_kind)
}

/// A condition's logical connective. Each operator has two spellings, and
/// the low-precedence ones (`not`, `and`, `or`) are nodes of their own.
enum Logical<'a> {
    Not(Node<'a>),
    And(Node<'a>, Node<'a>),
    Or(Node<'a>, Node<'a>),
}

fn logical_shape<'a>(cond: Node<'a>, source: &[u8]) -> Option<Logical<'a>> {
    match cond.kind() {
        "unary_expression" if raw_leading_op(cond, source) == "!" => {
            cond.child_by_field_name("operand").map(Logical::Not)
        }
        "logical_not_expression" => cond.child_by_field_name("operand").map(Logical::Not),
        "binary_expression" | "lowprec_logical_expression" => {
            let (l, r) = (cond.child_by_field_name("left")?, cond.child_by_field_name("right")?);
            match raw_mid_op(cond, source).as_str() {
                "&&" | "and" => Some(Logical::And(l, r)),
                "||" | "or" => Some(Logical::Or(l, r)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The narrowing facts `cond` proves over a region where it evaluates to
/// `holds`. A connective proves its sides only where every side is known:
/// `A && B` true proves A and B, `A || B` false proves not-A and not-B,
/// and the other two outcomes say nothing about either side alone.
/// `builder` is threaded so recognizers can consult constant-fold state (a
/// folded class name in `$x->isa($CLASS)`).
fn facts_for_region<'a>(builder: &Builder<'a>, cond: Node<'a>, holds: bool) -> Vec<(NarrowSubject, NarrowOp, Node<'a>)> {
    // Peel transparent `(...)` / single-element list wrappers up front, so
    // `if (($x->isa('Foo')))` and nested grouping fall through to the real
    // condition — one shared primitive instead of a per-shape arm.
    let cond = crate::cst::peel_groups(cond);
    let both = |l, r, holds| {
        let mut out = facts_for_region(builder, l, holds);
        out.extend(facts_for_region(builder, r, holds));
        out
    };
    match logical_shape(cond, builder.source) {
        Some(Logical::Not(x)) => facts_for_region(builder, x, !holds),
        Some(Logical::And(l, r)) if holds => conjoin(both(l, r, true)),
        Some(Logical::Or(l, r)) if !holds => conjoin(both(l, r, false)),
        Some(Logical::And(..) | Logical::Or(..)) => Vec::new(),
        None => recognize_atom(builder, cond)
            .and_then(|f| f.op_for_region(holds).map(|op| (f.subject, op, f.read)))
            .into_iter()
            .collect(),
    }
}

/// Facts that hold together. A type a conjunct proves is already defined,
/// so it subsumes a `defined` of the same subject (`blessed($x) &&
/// $x->isa('Foo')` holds `Foo`): two regions on one span would otherwise
/// tie, and the reducer keeps whichever came first.
fn conjoin<'a>(mut facts: Vec<(NarrowSubject, NarrowOp, Node<'a>)>) -> Vec<(NarrowSubject, NarrowOp, Node<'a>)> {
    let typed: Vec<Place> = facts
        .iter()
        .filter(|(_, op, _)| matches!(op, NarrowOp::To(t) if !t.may_be_undef()))
        .map(|(s, _, _)| s.subject().clone())
        .collect();
    facts.retain(|(s, op, _)| !(matches!(op, NarrowOp::Defined { .. }) && typed.contains(s.subject())));
    facts
}

/// Every guard literal in `cond` — an atom plus the negations applied
/// directly to it — with the literal's node. A negation over a connective
/// does not reach the atoms inside it (`!(A && B)` says nothing about A
/// alone), so each literal's polarity is its own.
fn guard_literals<'a>(builder: &Builder<'a>, cond: Node<'a>, out: &mut Vec<(GuardFact<'a>, Node<'a>)>) {
    let cond = crate::cst::peel_groups(cond);
    if let Some(fact) = guard_literal(builder, cond) {
        out.push((fact, cond));
        return;
    }
    match logical_shape(cond, builder.source) {
        Some(Logical::Not(x)) => guard_literals(builder, x, out),
        Some(Logical::And(l, r) | Logical::Or(l, r)) => {
            guard_literals(builder, l, out);
            guard_literals(builder, r, out);
        }
        None => {}
    }
}

fn guard_literal<'a>(builder: &Builder<'a>, cond: Node<'a>) -> Option<GuardFact<'a>> {
    let cond = crate::cst::peel_groups(cond);
    match logical_shape(cond, builder.source) {
        Some(Logical::Not(x)) => guard_literal(builder, x).map(|mut f| {
            f.asserts_when_true = !f.asserts_when_true;
            f
        }),
        Some(_) => None,
        None => recognize_atom(builder, cond),
    }
}

/// The guard fact a single (non-connective) condition states, if any.
fn recognize_atom<'a>(builder: &Builder<'a>, cond: Node<'a>) -> Option<GuardFact<'a>> {
    let source = builder.source;
    match cond.kind() {
        "method_call_expression" => recognize_isa_guard(builder, cond, source),
        "equality_expression" => recognize_ref_eq_guard(builder, cond, source),
        "func1op_call_expression" => recognize_defined_guard(builder, cond, source),
        "ambiguous_function_call_expression" | "function_call_expression" => {
            recognize_blessed_guard(builder, cond).or_else(|| recognize_type_check(builder, cond).map(|(f, _)| f))
        }
        _ => None,
    }
}

/// `is_ArrayRef($x)` / `assert_Str($x)` — a plugin-declared type check →
/// narrow `$x` to what a passing check proves. The second answer says
/// whether the call dies when the check fails.
fn recognize_type_check<'a>(builder: &Builder<'a>, call: Node<'a>) -> Option<(GuardFact<'a>, bool)> {
    let at = call.start_position();
    let (check, arg) = crate::cst::subject_call(call, builder.source, |name| builder.type_check_fn(name, at))?;
    Some((
        GuardFact {
            subject: builder.narrow_subject_of(arg)?,
            op: NarrowOp::To(check.proves.clone()),
            asserts_when_true: true,
            read: arg,
        },
        check.asserts,
    ))
}

/// `defined $x` / `defined($x)` → strip `Optional` off the subject.
fn recognize_defined_guard<'a>(builder: &Builder<'a>, call: Node<'a>, source: &[u8]) -> Option<GuardFact<'a>> {
    if call.child(0)?.utf8_text(source).ok()? != "defined" {
        return None;
    }
    let arg = func1op_subject_arg(call)?;
    Some(GuardFact {
        subject: builder.narrow_subject_of(arg)?,
        op: NarrowOp::Defined { read: node_to_span(arg) },
        asserts_when_true: true,
        read: arg,
    })
}

/// `blessed $x` / `blessed($x)` → strip `Optional` off the subject (v1
/// treats `blessed` as `defined`'s strip; the extra "is an object"
/// precision has no lattice target yet).
fn recognize_blessed_guard<'a>(builder: &Builder<'a>, call: Node<'a>) -> Option<GuardFact<'a>> {
    let (via, arg) = builder.ref_tag_call(call)?;
    if via != crate::model::file_analysis::RefTagFn::Blessed {
        return None;
    }
    Some(GuardFact {
        subject: builder.narrow_subject_of(arg)?,
        op: NarrowOp::Defined { read: node_to_span(arg) },
        asserts_when_true: true,
        read: arg,
    })
}

/// `$x->isa('Foo')` / `$x->DOES('Role')` → narrow `$x` to `ClassName`.
/// The class argument is a string literal, or a scalar that the builder's
/// constant fold pins to a single class (`my $C = 'Foo'; $x->isa($C)`).
fn recognize_isa_guard<'a>(builder: &Builder<'a>, call: Node<'a>, source: &[u8]) -> Option<GuardFact<'a>> {
    let mc = crate::cst::MethodCall::cast(call)?;
    let method = mc.method()?.utf8_text(source).ok()?;
    if method != "isa" && method != "DOES" {
        return None;
    }
    let read = mc.invocant()?;
    let subject = builder.narrow_subject_of(read)?;
    let arg = call.child_by_field_name("arguments")?;
    let class = crate::cst::plain_string_literal_text(arg, source)
        .or_else(|| builder.folded_class_name_arg(arg))
        .or_else(|| builder.current_package_literal(arg))?;
    Some(GuardFact {
        subject,
        op: NarrowOp::To(InferredType::ClassName(class)),
        asserts_when_true: true,
        read,
    })
}

/// `ref($x) eq 'Foo'` / `reftype($x) eq 'HASH'` / `blessed($x) eq 'Foo'`
/// (either operand order) → narrow `$x` to the proven type;
/// `__PACKAGE__` proves the enclosing class. A scalar compared to a
/// literal (`$ref eq 'ARRAY'`) is a guard on whatever tag it holds —
/// resolved once its value folds (`emit_tag_narrowing_witnesses`).
fn recognize_ref_eq_guard<'a>(builder: &Builder<'a>, eq: Node<'a>, source: &[u8]) -> Option<GuardFact<'a>> {
    if raw_mid_op(eq, source) != "eq" {
        return None;
    }
    let left = eq.child_by_field_name("left")?;
    let right = eq.child_by_field_name("right")?;
    let call = |n: Node<'a>| builder.ref_tag_call(crate::cst::peel_groups(n));
    if let Some(((via, operand), lit)) = call(left).map(|c| (c, right)).or_else(|| call(right).map(|c| (c, left))) {
        let ty = match crate::cst::plain_string_literal_text(lit, source) {
            Some(s) => tag_string_to_type(via, &s)?,
            None => InferredType::ClassName(builder.current_package_literal(lit)?),
        };
        return Some(GuardFact {
            subject: builder.narrow_subject_of(operand)?,
            op: NarrowOp::To(ty),
            asserts_when_true: true,
            read: operand,
        });
    }
    let tagged = |var: Node<'a>, lit: Node<'a>| {
        let var = crate::cst::peel_groups(var);
        if var.kind() != "scalar" {
            return None;
        }
        let name = crate::cst::canonical_var_name(var, source)?;
        let place = Place::variable(builder.binding_key_here(&name, var.start_position()));
        Some((place, crate::cst::plain_string_literal_text(lit, source)?, var))
    };
    let (var, lit, read) = tagged(left, right).or_else(|| tagged(right, left))?;
    Some(GuardFact { subject: NarrowSubject::TagOf(var), op: NarrowOp::TagEq(lit), asserts_when_true: true, read })
}

/// True if an expression is a guaranteed control-flow exit
/// (`return`/`die`/`exit`/`croak`/`last`/`next`/`redo`/`goto`): it never
/// yields a value, so `STMT if/unless G` narrows the rest of the block and
/// `X || EXIT` is worth only `X`. The words are read by spelling, so a sub
/// of the file's own named `croak` counts too.
// TODO(noreturn-callee): a callee that never returns (`croak`, `confess`)
// is an effect on the callee; replace the spelling lookup once effects and
// the CFG exist.
pub(super) fn is_exit_expression(node: Node, source: &[u8]) -> bool {
    const EXITS: [&str; 8] = ["die", "exit", "croak", "confess", "last", "next", "redo", "goto"];
    match node.kind() {
        "return_expression" | "last_expression" | "next_expression" | "redo_expression" => true,
        "function" | "bareword" => node
            .utf8_text(source)
            .map(|s| EXITS.contains(&s.trim()))
            .unwrap_or(false),
        "func1op_call_expression"
        | "function_call_expression"
        | "ambiguous_function_call_expression" => node
            .child_by_field_name("function")
            .or_else(|| node.child(0))
            .and_then(|n| n.utf8_text(source).ok())
            .map(|s| EXITS.contains(&s.trim()))
            .unwrap_or(false),
        _ => false,
    }
}

/// True if `block` never falls through: one of its statements leaves,
/// wherever it sits, so what follows it there is dead.
fn block_leaves(block: Node, source: &[u8]) -> bool {
    use crate::cst::NodeExt;
    block.elements().any(|stmt| statement_leaves(stmt, source))
}

/// True if a statement leaves its block: an exit, a conditional with an
/// `else` whose every arm leaves, or a bare block that leaves.
fn statement_leaves(stmt: Node, source: &[u8]) -> bool {
    match stmt.kind() {
        "expression_statement" => crate::cst::nth_element(stmt, 0).is_some_and(|e| is_exit_expression(e, source)),
        "conditional_statement" => {
            let arms = crate::cst::conditional_arms(stmt);
            arms.last().is_some_and(|arm| arm.cond.is_none()) && arms.iter().all(|arm| block_leaves(arm.block, source))
        }
        // A bare block is a loop that runs once (perlsyn).
        "block_statement" => loop_block_leaves(stmt, source),
        _ => false,
    }
}

/// True if a loop's block never comes back to the loop or lands after it:
/// it leaves, and no `last`/`next`/`redo` in it can reach the loop
/// (conservatively, even one an inner loop owns, or a label names).
// TODO(cfg-exit-join): a loop-control edge to the loop's exit is the CFG's.
fn loop_block_leaves(block: Node, source: &[u8]) -> bool {
    use crate::cst::NodeExt;
    fn has_loop_control(node: Node) -> bool {
        node.elements().any(|c| c.kind() == "loopex_expression" || has_loop_control(c))
    }
    block_leaves(block, source) && !has_loop_control(block)
}

impl<'a> Builder<'a> {
    /// Classify a guard's operand into the subject it narrows: a plain
    /// scalar (`Variable`), else a part of a variable whose every
    /// subscript names its slot (`Place`).
    fn narrow_subject_of(&self, node: Node<'a>) -> Option<NarrowSubject> {
        if let Some(v) = crate::cst::canonical_var_name(node, self.source) {
            let place = Place::variable(self.binding_key_here(&v, node.start_position()));
            return Some(NarrowSubject::Place { place, spelling: v, key_vars: Vec::new() });
        }
        let read = self.place_read(node)?;
        Some(NarrowSubject::Place {
            place: read.place,
            spelling: node.utf8_text(self.source).ok()?.to_string(),
            key_vars: read.key_vars,
        })
    }

    /// The part of a variable `node` names, when every subscript names its
    /// slot: a place a narrowing can be about.
    pub(super) fn place_subject(&self, node: Node<'a>) -> Option<Place> {
        self.place_read(node).map(|r| r.place)
    }

    fn place_read(&self, node: Node<'a>) -> Option<crate::cst::PlaceRead> {
        let read = crate::cst::read_place(self.syntax, node, self.source, &mut WalkBinder(self))?;
        (read.exact && !read.place.is_variable()).then_some(read)
    }

    /// `__PACKAGE__` as a class operand: the enclosing package.
    fn current_package_literal(&self, node: Node<'a>) -> Option<String> {
        self.token_class(node)
    }

    /// A scalar `isa`/`DOES` argument (`$x->isa($C)`) that the constant
    /// fold pins to exactly one class name. A multi-valued fold can't name
    /// a single dispatch class, so it doesn't narrow. `None` for non-scalar
    /// args (literals are handled by `plain_string_literal_text`).
    fn folded_class_name_arg(&self, arg: Node<'a>) -> Option<String> {
        if arg.kind() != "scalar" {
            return None;
        }
        let key = crate::cst::canonical_var_name(arg, self.source)?;
        match self.resolve_constant_strings(&key, arg).as_deref() {
            Some([class]) => Some(class.clone()),
            _ => None,
        }
    }

    /// Narrow an `if` / `elsif`* / `else` chain. Each arm runs only when its
    /// own condition holds (positive narrowing) AND every preceding
    /// condition fell through (cumulative negation). Only representable
    /// negations survive — `defined`/`blessed` complement to `Undef`, while
    /// `isa`/`ref-eq` have no positive complement (stay wide). The `else`
    /// arm is pure cumulative negation; an `elsif` arm combines its own
    /// guard with the priors' negation. A chain with no `else` has an
    /// implicit one. Where exactly one arm falls through (`block_leaves`),
    /// the rest of the enclosing block runs where that arm does; where none
    /// does it never runs, and two or more join (`TODO(cfg-exit-join)`).
    pub(super) fn narrow_block_guard(&mut self, cond_stmt: Node<'a>) {
        let arms: Vec<(Option<Node<'a>>, Node<'a>, bool)> = crate::cst::conditional_arms(cond_stmt)
            .into_iter()
            .map(|arm| (arm.cond, arm.block, arm.enter))
            .collect();

        for k in 0..arms.len() {
            let (own_cond, blk, enter) = arms[k];
            // Record the arm's own guard once (not the priors re-walked
            // for cumulative negation) — D3/D4 want each condition once.
            if let Some(c) = own_cond {
                self.record_guard_sites(c);
            }
            self.narrow_chain_arm(own_cond.map(|c| (c, enter)), &arms[..k], node_to_span(blk));
        }
        let mut falling: Vec<Option<usize>> =
            (0..arms.len()).filter(|&k| !block_leaves(arms[k].1, self.source)).map(Some).collect();
        if arms.last().is_some_and(|arm| arm.0.is_some()) {
            falling.push(None);
        }
        if let [arm] = falling[..] {
            let (own, priors) = match arm {
                Some(k) => (arms[k].0.map(|c| (c, arms[k].2)), &arms[..k]),
                None => (None, &arms[..]),
            };
            if let Some(region) = self.block_remainder(cond_stmt) {
                self.narrow_chain_arm(own, priors, region);
            }
        }
    }

    /// Narrow `region`, which runs where the arm's own condition is as
    /// given and every prior arm was not taken.
    fn narrow_chain_arm(
        &mut self,
        own: Option<(Node<'a>, bool)>,
        priors: &[(Option<Node<'a>>, Node<'a>, bool)],
        region: Span,
    ) {
        // The arm's own guard is the more direct statement about a
        // subject; a prior arm's negation must not override it.
        let mut covered: Vec<Place> = Vec::new();
        if let Some((c, enter)) = own {
            for (subject, op, read) in facts_for_region(self, c, enter) {
                covered.push(subject.subject().clone());
                self.emit_narrowing_fact(&subject, op, read, region);
            }
        }
        for &(prior_cond, _, prior_enter) in priors {
            let Some(prior_cond) = prior_cond else { continue };
            // A prior arm not taken means its condition held at the
            // opposite of its enter polarity.
            for (subject, op, read) in facts_for_region(self, prior_cond, !prior_enter) {
                let key = subject.subject().clone();
                if covered.contains(&key) {
                    continue;
                }
                covered.push(key);
                self.emit_narrowing_fact(&subject, op, read, region);
            }
        }
    }

    /// Record each guard literal in `condition` as a `GuardSite` for the
    /// D3/D4 redundancy diagnostics — subject, predicate, the literal's own
    /// span and polarity (a verdict on one conjunct is not a verdict on the
    /// condition), and the point at which to read the subject's prior
    /// (un-narrowed) type (the condition's own location, before any
    /// narrowed region). Recognition is shared with emission.
    fn record_guard_sites(&mut self, condition: Node<'a>) {
        use crate::model::file_analysis::{GuardPredicate, GuardSite};
        let scope = self.current_scope();
        let before = condition.start_position();
        let mut literals = Vec::new();
        guard_literals(self, condition, &mut literals);
        for (fact, literal) in literals {
            let predicate = match &fact.op {
                NarrowOp::To(ty) => GuardPredicate::IsType(ty.clone()),
                NarrowOp::Defined { .. } => GuardPredicate::Defined,
                // What it proves is a fold answer; D3/D4 read the prior
                // type at the guard, which a tag guard has no subject for.
                NarrowOp::TagEq(_) => continue,
            };
            self.guard_sites.push(GuardSite {
                subject: fact.subject.subject().clone(),
                spelling: fact.subject.spelling().to_string(),
                scope,
                predicate,
                asserts_when_true: fact.asserts_when_true,
                span: node_to_span(literal),
                before_point: before,
            });
        }
    }

    /// `STMT if/unless G;` — STMT runs only where G says, so it is a one-arm
    /// region the guard narrows; when STMT is a control-flow exit, the rest
    /// of the enclosing block is the fall-through, which holds the opposite.
    /// `STMT while/until G;` tests G before every run of STMT, so it narrows
    /// STMT the same way — except `do BLOCK while G`, whose first run comes
    /// before any test. A loop's fall-through is not narrowed: `last` leaves
    /// with G unchanged.
    pub(super) fn narrow_postfix(&mut self, postfix: Node<'a>) {
        let Some(condition) = postfix.child_by_field_name("condition") else { return };
        let Some(modified) = postfix.named_child(0) else { return };
        let Some(kw) = connector_keyword_between(modified, condition, self.source) else { return };
        let (runs_when_true, is_loop) = match kw.as_str() {
            "if" => (true, false),
            "unless" => (false, false),
            "while" => (true, true),
            "until" => (false, true),
            _ => return,
        };
        // TODO(do-loop-node): the grammar gives `do BLOCK while G` the same
        // node as `STMT while G`; a node of its own would make this a kind.
        if is_loop && modified.kind() == "do_expression" {
            return;
        }
        self.narrow_arm(condition, modified, runs_when_true);
        if !is_loop
            && is_exit_expression(modified, self.source)
            && self.narrow_block_remainder(postfix, condition, !runs_when_true)
        {
            self.record_guard_sites(condition);
        }
    }

    /// `while (G) BLOCK` / `for (…; G; …) BLOCK` — G is tested before every
    /// run of the body, so the body narrows by G (`until` by its negation),
    /// and the rebind cutoff ends it at a write. A body that always leaves
    /// never comes back to the test, so the loop is left only where its
    /// first test fails, and the rest of the block narrows as after an `if`
    /// with no `else`; any other body is left with G as the last test or a
    /// `last` left it (`TODO(cfg-exit-join)`).
    pub(super) fn narrow_loop(&mut self, stmt: Node<'a>) {
        let (Some(condition), Some(body)) = (stmt.child_by_field_name("condition"), stmt.child_by_field_name("block"))
        else {
            return;
        };
        // The C-style condition is an `expression_statement` around the test.
        let condition = match condition.kind() {
            "expression_statement" => match crate::cst::nth_element(condition, 0) {
                Some(test) => test,
                None => return,
            },
            _ => condition,
        };
        let enter = stmt.child(0).is_none_or(|k| k.kind() != "until");
        self.narrow_arm(condition, body, enter);
        if loop_block_leaves(body, self.source) {
            if let Some(region) = self.rest_of_block(stmt) {
                self.record_guard_sites(condition);
                self.narrow_chain_arm(Some((condition, !enter)), &[], region);
            }
        }
    }

    /// `G ? A : B` — A runs where G holds, B where it fails.
    pub(super) fn narrow_ternary(&mut self, expr: Node<'a>) {
        let Some(condition) = expr.child_by_field_name("condition") else { return };
        if let Some(a) = expr.child_by_field_name("consequent") {
            self.narrow_arm(condition, a, true);
        }
        if let Some(b) = expr.child_by_field_name("alternative") {
            self.narrow_arm(condition, b, false);
        }
    }

    /// `L && R` / `L and R` — R runs where L holds; `||` / `or` where it
    /// fails.
    pub(super) fn narrow_logical_rhs(&mut self, expr: Node<'a>) {
        let (left, right, holds) = match logical_shape(expr, self.source) {
            Some(Logical::And(l, r)) => (l, r, true),
            Some(Logical::Or(l, r)) => (l, r, false),
            _ => return,
        };
        self.narrow_arm(left, right, holds);
    }

    /// Narrow `arm`, which runs only where `condition` is `holds`.
    fn narrow_arm(&mut self, condition: Node<'a>, arm: Node<'a>, holds: bool) {
        let region = node_to_span(arm);
        for (subject, op, read) in facts_for_region(self, condition, holds) {
            self.emit_narrowing_fact(&subject, op, read, region);
        }
    }

    /// `G or EXIT;` / `G and EXIT;` — the bare-logical early-exit idiom.
    pub(super) fn narrow_logical_exit(&mut self, expr: Node<'a>) {
        // `G or EXIT` → fall-through holds when G TRUE; `G and EXIT` → FALSE.
        let (left, right, holds_when_true) = match logical_shape(expr, self.source) {
            Some(Logical::Or(l, r)) => (l, r, true),
            Some(Logical::And(l, r)) => (l, r, false),
            _ => return,
        };
        if !is_exit_expression(right, self.source) {
            return;
        }
        if self.narrow_block_remainder(expr, left, holds_when_true) {
            self.record_guard_sites(left);
        }
    }

    /// `assert_X($x);` — a statement that dies unless its check passes is
    /// `die unless is_X($x)`: the rest of the block runs where it held.
    pub(super) fn narrow_assertion(&mut self, call: Node<'a>) {
        if self.syntax.parent(call).is_none_or(|p| p.kind() != "expression_statement") {
            return;
        }
        if !recognize_type_check(self, call).is_some_and(|(_, asserts)| asserts) {
            return;
        }
        if self.narrow_block_remainder(call, call, true) {
            self.record_guard_sites(call);
        }
    }

    /// Narrow `[statement end .. enclosing-block end]` for a statement-level
    /// guard: the statement `node` is in leaves the block unless
    /// `condition` is `holds_when_true`. False when that statement is no
    /// block's; the caller records the guard's sites, once per condition.
    fn narrow_block_remainder(
        &mut self,
        node: Node<'a>,
        condition: Node<'a>,
        holds_when_true: bool,
    ) -> bool {
        let Some(region) = self.block_remainder(node) else { return false };
        for (subject, op, read) in facts_for_region(self, condition, holds_when_true) {
            self.emit_narrowing_fact(&subject, op, read, region);
        }
        true
    }

    /// `[statement end .. enclosing-block end]` for the statement `node` is
    /// in, when that statement is a block's.
    fn block_remainder(&self, node: Node<'a>) -> Option<Span> {
        let mut stmt = node;
        while !matches!(stmt.kind(), "expression_statement" | "conditional_statement") {
            stmt = self.syntax.parent(stmt)?;
        }
        self.rest_of_block(stmt)
    }

    /// `[stmt end .. block end]` when `stmt` is a statement of a block, a
    /// bare block, or the file's top level.
    // TODO(cfg-exit-join): narrowing past the enclosing block needs the CFG's block-exit join (docs/prompt-cfg-tier.md §3.5).
    fn rest_of_block(&self, stmt: Node<'a>) -> Option<Span> {
        let block = self.syntax.parent(stmt).filter(|b| matches!(b.kind(), "block" | "block_statement" | "source_file"))?;
        Some(Span { start: stmt.end_position(), end: block.end_position() })
    }

    /// A deref observes its operand as the reference its sigil names:
    /// `@$x` / `@{$x}` / `$$x[0]` an array, `%$x` / `$$x{k}` a hash,
    /// `&$x` / `&{$x}` code. Every deref spelling reaches here, keyed on
    /// the deref node, whichever visitor then walks it.
    pub(super) fn observe_deref(&mut self, node: Node<'a>) {
        use crate::model::conventions::Sigil;
        use crate::model::witnesses::TypeObservation;
        let Some(sigil) = crate::cst::deref_sigil(self.syntax, node) else { return };
        let Some(operand) = crate::cst::deref_operand_expr(node) else { return };
        let obs = match sigil {
            Sigil::Array => TypeObservation::ArrayRefAccess,
            Sigil::Hash => TypeObservation::HashRefAccess,
            Sigil::Code => TypeObservation::CodeRefInvocation,
            // No reference type in the lattice to observe.
            Sigil::Scalar | Sigil::Glob => return,
        };
        self.observe(operand, node, obs);
    }

    /// Record what `operand`'s use at `context` reveals about it. A use
    /// says what the operand holds on the paths that reach it and no others:
    /// it holds from `context` to the end of the conditionally-run arm
    /// around it (`cst::conditional_arm`), or else to the end of the block
    /// it sits in, and like any narrowing it ends at the subject's next
    /// rebind. Past an arm the value is a join of the arm and its bypass,
    /// which `JoinFold` will answer; until then the arm says nothing
    /// outside itself.
    pub(super) fn observe(
        &mut self,
        operand: Node<'a>,
        context: Node<'a>,
        obs: crate::model::witnesses::TypeObservation,
    ) {
        let operand = crate::cst::peel_groups(operand);
        if !is_subject_node_kind(operand.kind()) {
            return;
        }
        let Some(subject) = self.narrow_subject_of(operand) else { return };
        let extent = crate::cst::conditional_arm(self.syntax, context, crate::cst::ArmBound::Block)
            .or_else(|| crate::cst::enclosing_block(self.syntax, context));
        let Some(extent) = extent else { return };
        let region = Span { start: context.start_position(), end: extent.end_position() };
        self.emit_region_fact(&subject, RegionFact::Observe(obs), region);
    }

    /// Emit a guard's narrowing into `region`. `op` is the polarity-
    /// resolved operation (`GuardFact::op_for_region`); the subject is
    /// keyed and the region truncated at the first disturbance, exactly
    /// as for a positive narrowing. A `defined` region's value is the
    /// subject's `read` in the guard, so that read carries its witness.
    fn emit_narrowing_fact(&mut self, subject: &NarrowSubject, op: NarrowOp, read: Node<'a>, region: Span) {
        if matches!(op, NarrowOp::Defined { .. }) {
            self.emit_expr_witness(read);
        }
        self.emit_region_fact(subject, RegionFact::Guard(op), region);
    }

    fn emit_region_fact(&mut self, subject: &NarrowSubject, fact: RegionFact, region: Span) {
        self.pending_narrowings.push(PendingNarrow {
            subject: subject.clone(),
            fact,
            region,
            scope: self.current_scope(),
        });
    }

    /// Re-emittable: tag guards. A guard `$ref eq 'LIT'` narrows the
    /// variable `$ref`'s tag names, when `$ref` holds a `RefTagOf` computed
    /// in the same sub and its subject is not rebound between the tag and
    /// the guard. Clear-and-emit on tag `tag_narrowing`: what `$ref` holds
    /// is a fold answer.
    pub(super) fn emit_tag_narrowing_witnesses(&mut self) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        self.bag.remove_by_source_tag("tag_narrowing");
        let mut emits: Vec<(String, ScopeId, InferredType, Span)> = Vec::new();
        for g in &self.tag_narrowings {
            let Some(InferredType::RefTagOf { subject: Some(subject), site, via, .. }) =
                self.bag_query_place(&g.var, g.scope, g.region.start)
            else {
                continue;
            };
            if self.enclosing_sub_scope_at(site.start) != self.enclosing_sub_scope_at(g.region.start) {
                continue;
            }
            let since_tag = Span { start: site.end, end: g.region.start };
            let rebound = Place::variable(self.binding_of(&subject, g.region.start, g.scope).0);
            if self.rebinds.earliest_in(&rebound, since_tag).is_some() {
                continue;
            }
            let Some(ty) = tag_string_to_type(via, &g.lit) else { continue };
            let cut = self.rebinds.earliest_in(&rebound, g.region);
            if let Some(region) = crate::model::file_analysis::cut_region(g.region, [cut]) {
                emits.push((subject, g.scope, ty, region));
            }
        }
        for (name, scope, ty, region) in emits {
            self.push_witness(Witness {
                attachment: WitnessAttachment::Variable { name, scope },
                source: WitnessSource::Builder("tag_narrowing".into()),
                payload: WitnessPayload::InferredType(ty),
                span: region,
            });
        }
    }

    /// The innermost sub or method scope holding `at` — the extent a
    /// variable's tag means anything in.
    fn enclosing_sub_scope_at(&self, at: Point) -> Option<ScopeId> {
        use crate::model::file_analysis::ScopeKind;
        self.scopes
            .iter()
            .filter(|s| matches!(s.kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. }))
            .filter(|s| crate::model::file_analysis::contains_point(&s.span, at))
            .max_by_key(|s| (s.span.start.row, s.span.start.column))
            .map(|s| s.id)
    }

    /// Post-walk: resolve every recognized narrowing's region cutoff against
    /// the now-minted FlowEdges, then emit each region's value as a scoped
    /// witness; a tag guard's waits for the fold (`TagNarrowing`).
    /// Deferred from the walk so the scalar-rebind truncation can read edges
    /// (minted post-walk). The place index walks the injected `s///e` trees
    /// too: their code runs, so a write there disturbs a place.
    pub(super) fn apply_narrowing_cutoffs(&mut self) {
        use crate::model::witnesses::{Witness, WitnessPayload, WitnessSource};
        let pending = std::mem::take(&mut self.pending_narrowings);
        let wanted: std::collections::HashSet<String> = pending
            .iter()
            .filter_map(|pn| match &pn.subject {
                NarrowSubject::Place { place, .. } if !place.is_variable() => Some(self.binding_name(&place.root).to_string()),
                NarrowSubject::Place { .. } | NarrowSubject::TagOf(_) => None,
            })
            .collect();
        let places = if wanted.is_empty() {
            None
        } else {
            let (idx, sites) = self.place_disturbances(self.syntax.roots(), &wanted);
            self.arrow_deref_sites.extend(sites);
            Some(idx)
        };
        self.rebinds = self.rebind_index();
        let mut facts = Vec::new();
        for pn in pending {
            if let (NarrowSubject::TagOf(var), RegionFact::Guard(NarrowOp::TagEq(lit))) = (&pn.subject, &pn.fact) {
                // The guard reads the tag once, at the region's start; what
                // truncates is a write to the subject it names, known only
                // once the tag resolves.
                self.tag_narrowings.push(TagNarrowing {
                    var: var.clone(),
                    scope: pn.scope,
                    region: pn.region,
                    lit: lit.clone(),
                });
                continue;
            }
            // TODO(narrowing-cutoff-read-order): a write in a modified
            // statement runs after the modifier's head, so the cut should not
            // end the region ahead of the head's reads.
            let rebound = |var: &str| {
                let var = Place::variable(self.binding_of(var, pn.region.start, pn.scope).0);
                self.rebinds.earliest_in(&var, pn.region)
            };
            let cuts: Vec<Option<Point>> = match &pn.subject {
                NarrowSubject::TagOf(var) => vec![self.rebinds.earliest_in(var, pn.region)],
                NarrowSubject::Place { place, .. } if place.is_variable() => vec![self.rebinds.earliest_in(place, pn.region)],
                NarrowSubject::Place { place, key_vars, .. } => {
                    std::iter::once(places.as_ref().and_then(|idx| idx.earliest_in(place, pn.region)))
                        .chain(key_vars.iter().map(|kv| rebound(kv)))
                        .collect()
                }
            };
            let Some(region) = crate::model::file_analysis::cut_region(pn.region, cuts) else {
                continue; // truncated to nothing
            };
            let subject = pn.subject.subject().clone();
            match pn.fact {
                RegionFact::Guard(op) => {
                    if let Some(payload) = op.payload() {
                        facts.push(Witness {
                            attachment: crate::model::witnesses::WitnessAttachment::Place(Box::new(subject.clone())),
                            source: WitnessSource::Builder("narrowing".into()),
                            payload,
                            span: region,
                        });
                    }
                }
                RegionFact::Observe(obs) => {
                    facts.push(Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Place(Box::new(subject.clone())),
                        source: WitnessSource::Builder("usage_observation".into()),
                        payload: WitnessPayload::Observation(obs),
                        span: region,
                    });
                }
            }
        }
        for w in facts {
            self.push_witness(w);
        }
    }

    /// Every FlowEdge target as a write of the whole variable it binds:
    /// what cuts a fact about that variable short.
    fn rebind_index(&self) -> PlaceDisturbanceIndex {
        let mut idx = PlaceDisturbanceIndex::default();
        for fe in &self.flow_edges {
            let (binding, _) = self.binding_of(&fe.target_name, fe.target_at, fe.target_scope);
            idx.add(fe.target_at, Access::Write, Place::variable(binding));
        }
        idx.seal();
        idx
    }

    /// The unqualified, sigiled name a binding was declared or written
    /// with: what every spelling of it shares.
    pub(super) fn binding_name<'b>(&'b self, binding: &'b Binding) -> &'b str {
        match binding {
            Binding::Lexical(sid) => &self.symbols[sid.0 as usize].name,
            Binding::Package(_, name) => name,
            Binding::Arguments(_) => crate::model::conventions::ARGUMENT_ARRAY,
            // Perl has no elided receiver: `$self` is a lexical.
            Binding::Receiver(_) => "",
        }
    }

    /// Every access of a place rooted at a `wanted` name, over the file's
    /// trees: one survey, with each name bound by position.
    ///
    /// The same survey mints the deref sites whose receiver is a part of a
    /// variable (`$self->{cb}->()`): only a narrowed place can type one,
    /// and only a wanted root is read here.
    fn place_disturbances(
        &self,
        roots: impl IntoIterator<Item = Node<'a>>,
        wanted: &std::collections::HashSet<String>,
    ) -> (PlaceDisturbanceIndex, Vec<crate::model::file_analysis::ArrowDerefSite>) {
        let mut idx = PlaceDisturbanceIndex::default();
        let mut sites = Vec::new();
        for root in roots {
            // Each tree runs in document order; an injected tree restarts it.
            let mut binder = SurveyBinder { builder: self, scopes: super::infra::ScopeCursor::new(&self.scopes), wanted };
            let mut read_through = std::collections::HashSet::new();
            super::pipeline::survey(root, |node| {
                let kind = node.kind();
                if !matches!(
                    kind,
                    "hash_element_expression"
                        | "array_element_expression"
                        | "slice_expression"
                        | "keyval_expression"
                        | "scalar"
                        | "array"
                        | "hash"
                        | "coderef_call_expression"
                ) {
                    return;
                }
                if let Some((receiver, form)) = crate::cst::deref_of_part(node) {
                    let read = crate::cst::read_place(self.syntax, receiver, self.source, &mut binder)
                        .filter(|r| r.exact && !r.place.is_variable());
                    if let (Some(read), Ok(text)) = (read, receiver.utf8_text(self.source)) {
                        let dies_on_undef = self.deref_dies_on_undef(node, &form);
                        let span = node_to_span(node);
                        sites.push(crate::model::file_analysis::ArrowDerefSite {
                            receiver: text.to_string(),
                            place: Box::new(read.place),
                            span,
                            form,
                            dies_on_undef,
                            evaluated_before: self.evaluated_before(node),
                        });
                    }
                }
                if kind == "coderef_call_expression" {
                    return;
                }
                // Preorder reaches an access before what it reads through,
                // so the outer access marks the inner one: a deref's operand
                // sits below the access, not as its direct child.
                if let Some(inner) = crate::cst::place_inner(node) {
                    read_through.insert(inner.id());
                }
                if read_through.remove(&node.id()) {
                    return;
                }
                // Binding first: most accesses hang off a root no subject
                // names, and classifying the access is the costly half.
                let Some(read) = crate::cst::read_place(self.syntax, node, self.source, &mut binder) else { return };
                let access = match kind {
                    "hash_element_expression" | "array_element_expression" => {
                        let written = self.element_write_kind(node).is_some()
                            || crate::cst::is_value_reset_target(self.syntax, node, self.source);
                        if written { Access::Write } else { Access::Opaque }
                    }
                    // A slice can write every slot it names, read or not.
                    "slice_expression" | "keyval_expression" => Access::Write,
                    _ => {
                        if crate::cst::is_assignment_target(self.syntax, node) { Access::Write } else { Access::Opaque }
                    }
                };
                idx.add(node.start_position(), access, read.place);
            });
        }
        idx.seal();
        (idx, sites)
    }

    /// Run the declarative `@flow` query (`queries/perl/flow.scm`) and mint a
    /// FlowEdge per `(target, source)` with the builder's OWN scope. The
    /// assignment shapes are captured in the `.scm`; the minting + scope live
    /// here — the same FlowEdge concept the cpp pack produces. The forcing-
    /// function start of Perl-on-the-query-engine. Provenance-only for now
    /// (no lowering): the shapes' types still come from the walk.
    pub(super) fn mint_flow_edges_via_query(&mut self) {
        // The plugin walk carries the document's matches when it ran; a
        // build with no walk patterns traverses for them here.
        let pending = match self.flow_matches.take() {
            Some(m) => m,
            None => self.collect_flow_matches(),
        };
        for caps in pending {
            // Bind shapes: no inflowing value. A bare `my`/`local` CLEARS to
            // undef (`Cleared`); a `foreach` var rebinds per element (`Rebind`,
            // type TBD). Both record the rebind for the narrowing cutoff.
            if let Some(bare) = caps.clear {
                let at = bare.start_position();
                for name in self.bare_bind_names(bare) {
                    // `my %h;` is an empty hash, a closed shape its element
                    // writes extend — the same value as `my %h = ()`.
                    if name.starts_with('%') && bare.kind() == "variable_declaration" {
                        self.push_type_constraint(crate::model::file_analysis::TypeConstraint {
                            variable: name.clone(),
                            scope: self.scope_at_point(at),
                            constraint_span: node_to_span(bare),
                            inferred_type: crate::model::file_analysis::InferredType::HashWithKeys {
                                keys: crate::model::file_analysis::SharedKeys::new(Vec::new()),
                                open: false,
                            },
                        });
                    }
                    // Record the rebind (for the narrowing cutoff). A scalar
                    // clears to undef — but that `Undef` is a REGION assertion
                    // truncated at the next rebind (`my $x; $x->[0]` autoviv
                    // ends it), so it lands with the narrowing tier (where
                    // region+cutoff compose), not as a plain bag witness here.
                    self.push_flow_edge(
                        name,
                        at,
                        node_to_span(bare),
                        crate::model::file_analysis::Extraction::Rebind,
                        false,
                    );
                }
                continue;
            }
            if let Some(loopvar) = caps.rebind {
                if let (Ok(name), Some(src)) = (loopvar.utf8_text(self.source), caps.source) {
                    self.push_flow_edge(
                        name.to_string(),
                        loopvar.start_position(),
                        node_to_span(src),
                        crate::model::file_analysis::Extraction::Rebind,
                        false,
                    );
                }
                continue;
            }
            let Some(src) = caps.source else { continue };
            let mut source_span = node_to_span(src);
            // The write lands where `assignment_write_point` says, and a
            // compound write stores the assignment's value, not the RHS.
            let mut write_at = None;
            if let Some(assign) = self.syntax.parent(src).filter(|p| p.kind() == "assignment_expression") {
                write_at = Some(crate::cst::assignment_write_point(assign));
                if crate::cst::assign_op(assign) != Some(crate::cst::AssignOp::Plain) {
                    source_span = node_to_span(assign);
                }
            }
            if let Some(lhs_node) = caps.target {
                if let Some(slots) = self.lhs_list_slots(lhs_node) {
                    // List/destructuring: each slot edges to its literal element
                    // (Whole) or a Positional projection; an element target
                    // records its write with the same value.
                    let elem_nodes = self.list_element_nodes(src);
                    let at = write_at.unwrap_or_else(|| lhs_node.start_position());
                    let reassigns = lhs_node.kind() != "variable_declaration";
                    let plain = self
                        .syntax
                        .parent(src)
                        .filter(|p| p.kind() == "assignment_expression")
                        .map_or(true, |a| crate::cst::assign_op(a) == Some(crate::cst::AssignOp::Plain));
                    for slot in slots {
                        let (vt, extraction) = match slot {
                            ListSlot::Var(vt, extraction) => (vt, extraction),
                            ListSlot::Element(elem, n) => {
                                let scope = self.scope_at_point(elem.start_position());
                                match (n.filter(|_| plain), &elem_nodes) {
                                    (Some(n), Some(nodes)) => {
                                        self.record_key_write(elem, nodes.get(n).copied(), None, scope)
                                    }
                                    (Some(n), None) => {
                                        self.record_key_write(elem, Some(src), Some(n as i32), scope)
                                    }
                                    (None, _) => self.record_key_write(elem, None, None, scope),
                                }
                                continue;
                            }
                        };
                        let (source, extraction) = match (&elem_nodes, &extraction) {
                            (Some(nodes), crate::model::file_analysis::Extraction::Positional(n))
                                if *n < nodes.len() =>
                            {
                                self.emit_expr_witness(nodes[*n]);
                                (node_to_span(nodes[*n]), crate::model::file_analysis::Extraction::Whole)
                            }
                            _ => (source_span, extraction),
                        };
                        self.push_flow_edge(vt, at, source, extraction, reassigns);
                    }
                } else if let Some(vt) = self.get_var_text_from_lhs(lhs_node) {
                    let reassigns = lhs_node.kind() != "variable_declaration";
                    self.push_flow_edge(
                        vt,
                        write_at.unwrap_or_else(|| lhs_node.start_position()),
                        source_span,
                        crate::model::file_analysis::Extraction::Whole,
                        reassigns,
                    );
                }
            }
        }
    }

    /// The `@flow` document's matches, by its own traversal.
    fn collect_flow_matches(&self) -> Vec<FlowMatch<'a>> {
        crate::util::ghost_stats::count("flow.own_walk");
        let (query, caps_of) = super::flow_query();
        let (matches, _) = self.file_matches(query, |_| true);
        matches
            .into_iter()
            .filter_map(|(_, caps)| FlowMatch::of(caps.into_iter().map(|(i, n)| (Some(caps_of[i as usize]), n))))
            .collect()
    }

    /// The variable name(s) a clear (`@flow.clear`) targets — a
    /// `variable_declaration` (single or paren list) or a `localization`
    /// scalar.
    pub(super) fn bare_bind_names(&self, bare: Node<'a>) -> Vec<String> {
        if bare.kind() == "variable_declaration" {
            if let Some(targets) = self.lhs_list_targets(bare) {
                return targets.into_iter().map(|(n, _)| n).collect();
            }
            return self.get_var_text_from_lhs(bare).into_iter().collect();
        }
        bare.utf8_text(self.source)
            .ok()
            .map(|s| s.to_string())
            .into_iter()
            .collect()
    }

    /// Mint a FlowEdge and lower it to the variable's value edge. Every
    /// write lowers, declaration or reassignment: the edge is the value the
    /// fold reads at the write. The single mint+lower for the query pass.
    pub(super) fn push_flow_edge(
        &mut self,
        name: String,
        at: Point,
        source: Span,
        extraction: crate::model::file_analysis::Extraction,
        reassigns: bool,
    ) {
        let scope = self.scope_at_point(at);
        // The write's reset marker, idempotent with the walk's own — the two
        // lanes agree on every write site by construction.
        self.push_reset_marker(name.clone(), scope, at);
        let fe = crate::model::file_analysis::FlowEdge {
            target_name: name,
            target_scope: scope,
            target_at: at,
            target_token: None,
            source,
            extraction,
            reassigns,
        };
        if let Some(w) = fe.lower_to_witness() {
            // The walk mints a single-target write's edge as it passes;
            // the `@flow` pass reaches the same write again.
            let w = self.bound_witness(w);
            if self.bag.iter_attachment(&w.attachment).any(|o| o.span == w.span && o.payload == w.payload) {
                return;
            }
            self.bag.push(w);
        }
        self.flow_edges.push(fe);
    }
}
