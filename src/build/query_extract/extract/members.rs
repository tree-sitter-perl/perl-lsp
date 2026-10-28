//! `@member.*` and `@arity.*`: a member access's receiver and operator, and
//! the argument / parameter counts of calls and callables.

use super::*;
use crate::model::file_analysis::{MemberOp, ParamArity};
use crate::model::conventions::Invocant;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// match_id → was the IMMEDIATE member-access receiver a simple variable?
    /// Recorded at construction (the un-peeled node); gates op-DX at the mint.
    pub(super) member_simple: HashMap<usize, bool>,
    /// match_id → was the PEELED receiver a written name (`w`, `Widget`)
    /// rather than an expression (`self.x`, `f()`)? Decides whether the
    /// receiver's text may spell a class.
    recv_named: HashMap<usize, bool>,
    /// A call's written arguments (span + byte range), keyed by the match
    /// that captured its callee (`@ref.call` / `@ref.qcall` /
    /// `@ref.member.call`), its argument list (`@arity.args`) and each
    /// argument (`@arity.arg`).
    pub(super) args_by_match: HashMap<usize, Vec<(Span, usize, usize)>>,
    /// `@arity.spread`: the byte ranges of arguments that flatten to any
    /// number of values.
    spreads: std::collections::HashSet<(usize, usize)>,
    /// A callable's declared parameter arity, keyed by the parameter_list span.
    /// Associated to its def symbol by span containment in `into_file_analysis`
    /// (`@arity.sig` fires a separate match from the def name).
    param_sigs: Vec<(Span, ParamArity)>,
    /// `@member.recv` → the receiver span of a `recv.field` access, joined to
    /// its `@ref.member.*` by match_id so the minted MethodCall ref carries it.
    pub(super) member_recv: HashMap<usize, (Span, Invocant)>,
    /// `@member.op` → the operator token's span, joined to `@ref.member.*` by
    /// match_id; which operator it is comes from `ops_at`.
    pub(super) member_op_span: HashMap<usize, Span>,
    /// `@member.op.<which>` → the operator the document names at a token
    /// span. A token no pattern names claims no operator, so op-DX stays
    /// silent rather than guessing.
    ops_at: HashMap<(Point, Point), MemberOp>,
}

impl State {
    /// How many arguments the call in `match_id` wrote.
    pub(super) fn arg_count(&self, match_id: usize) -> Option<usize> {
        self.args_by_match.get(&match_id).map(Vec::len)
    }

    /// The call's arguments in order, each with whether it spreads.
    pub(super) fn call_args(&self, match_id: usize) -> Option<Vec<(Span, bool)>> {
        let args = self.args_by_match.get(&match_id)?;
        Some(args.iter().map(|&(span, s, e)| (span, self.spreads.contains(&(s, e)))).collect())
    }

    /// The operator a member match wrote, with its span.
    pub(super) fn op_of(&self, match_id: usize) -> Option<(MemberOp, Span)> {
        let span = *self.member_op_span.get(&match_id)?;
        Some((*self.ops_at.get(&(span.start, span.end))?, span))
    }
}

/// `@member.recv`: a member access receiver. Peel transparent wrappers
/// (`(*p)`, `(&o)`, `(p)`) to the typed inner where the node is live, so the
/// minted MethodCall ref's invocant_span lands on the inner expression
/// `expr_type_at_span` already types.
pub(super) fn flatten_recv(
    members: &mut State,
    node: tree_sitter::Node<'_>,
    pack: &LangPack,
    source: &[u8],
    match_id: usize,
    events: &mut Vec<Event>,
) {
    // op-DX applies only to a bare-variable immediate receiver
    // (its deref_stack resolves by name); a wrapper/chain doesn't.
    members.member_simple.insert(match_id, pack.simple_var_kinds.contains(&node.kind()));
    let inner = peel_receiver(node, pack.recv_peel);
    members.recv_named.insert(match_id, pack.simple_var_kinds.contains(&inner.kind()));
    let text = inner.utf8_text(source).unwrap_or("").to_string();
    events.push(Event::at(inner, Capture::Member(MemberCap::Recv), text, match_id));
}

/// `@arity.args`: the call wrote an argument list, so its arity is known
/// even when the list is empty. Keyed by the match, which also captured the
/// callee.
pub(super) fn open_args(members: &mut State, match_id: usize) {
    members.args_by_match.entry(match_id).or_default();
}

/// `@arity.arg`: one argument of the match's list, kept in source order. A
/// wildcard in a query also matches the grammar's extras, and a predicate
/// filters a whole match rather than one quantified capture, so a comment
/// between two arguments is dropped here.
pub(super) fn record_arg(members: &mut State, node: tree_sitter::Node<'_>, match_id: usize) {
    if node.is_extra() {
        return;
    }
    let args = members.args_by_match.entry(match_id).or_default();
    let at = args.partition_point(|&(_, s, _)| s < node.start_byte());
    args.insert(at, (Span { start: node.start_position(), end: node.end_position() }, node.start_byte(), node.end_byte()));
}

/// `@arity.sig`: a callable's parameter_list — count declared params
/// structurally. `optional_parameter_declaration` carries a default (counts
/// toward `total`, not `required`); a template pack
/// (`variadic_parameter_declaration`) or a C `...` token makes the signature
/// variadic.
pub(super) fn count_params(members: &mut State, node: tree_sitter::Node<'_>) {
    let mut total = 0usize;
    let mut required = 0usize;
    let mut variadic = false;
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        match ch.kind() {
            "parameter_declaration" => {
                total += 1;
                required += 1;
            }
            "optional_parameter_declaration" => {
                total += 1;
            }
            "variadic_parameter_declaration" | "..." => variadic = true,
            _ => {}
        }
    }
    members.param_sigs.push((
        Span { start: node.start_position(), end: node.end_position() },
        ParamArity { total, required, variadic },
    ));
}

/// The span-keyed member facts, read before any handler asks them.
pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::MemberOp(which) => {
            st.members.ops_at.insert((e.start, e.end), which.op());
        }
        Capture::Arity(ArityCap::Spread) => {
            st.members.spreads.insert((e.start_byte, e.end_byte));
        }
        _ => {}
    }
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Member(MemberCap::Recv) => {
            let named = st.members.recv_named.get(&e.match_id).copied().unwrap_or(false);
            let invocant = if named {
                Invocant::assume_canonical(e.text.clone())
            } else {
                Invocant::expr(e.text.clone())
            };
            st.members.member_recv.insert(e.match_id, (e.span(), invocant));
        }
        Capture::Member(MemberCap::Op) => {
            st.members.member_op_span.insert(e.match_id, e.span());
        }
        _ => {}
    }
}

pub(super) fn finish(st: &mut ExtractState) {
    st.out.param_sigs = std::mem::take(&mut st.members.param_sigs);
}

/// Drop transparent receiver wrappers (`(*p)` → `p`) through each one's
/// first named child; the node itself when it is not a wrapper, or when a
/// wrapper has no named child.
fn peel_receiver<'a>(node: tree_sitter::Node<'a>, wrappers: &[&str]) -> tree_sitter::Node<'a> {
    let mut inner = node;
    for _ in 0..32 {
        if !wrappers.contains(&inner.kind()) {
            return inner;
        }
        let mut c = inner.walk();
        let first = inner.named_children(&mut c).find(|ch| !ch.is_extra());
        match first {
            Some(ch) => inner = ch,
            None => return node,
        }
    }
    node
}
