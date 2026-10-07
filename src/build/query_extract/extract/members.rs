//! `@member.*` and `@arity.*`: a member access's receiver and operator, and
//! the argument / parameter counts of calls and callables.

use super::*;
use crate::model::file_analysis::{MemberOp, ParamArity};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// match_id → was the IMMEDIATE member-access receiver a simple variable?
    /// Recorded at construction (the un-peeled node); gates op-DX at the mint.
    pub(super) member_simple: HashMap<usize, bool>,
    /// A call's written arg count, keyed by its argument_list's START point
    /// (the `(`), which is adjacent to the callee/method name token's END — so
    /// a ref finds its arity by `ref.end == arglist.start` without a match join
    /// (member calls fire a separate match from their arg list). One entry per
    /// `@arity.args`.
    pub(super) arg_counts_by_start: HashMap<(usize, usize), usize>,
    /// A callable's declared parameter arity, keyed by the parameter_list span.
    /// Associated to its def symbol by span containment in `into_file_analysis`
    /// (`@arity.sig` fires a separate match from the def name).
    param_sigs: Vec<(Span, ParamArity)>,
    /// `@member.recv` → the receiver span of a `recv.field` access, joined to
    /// its `@ref.member` by match_id so the minted MethodCall ref carries it.
    pub(super) member_recv: HashMap<usize, (Span, String)>,
    /// `@member.op` → the written operator mapped through `pack.op_map` + its
    /// span; joined to `@ref.member` so op-DX rides the minted ref.
    pub(super) member_op_raw: HashMap<usize, (MemberOp, Span)>,
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
    let inner = peel(node, &pack.recv_peel, source).map(|(leaf, _, _)| leaf).unwrap_or(node);
    let text = inner.utf8_text(source).unwrap_or("").to_string();
    events.push(Event::at(inner, Capture::Member(MemberCap::Recv), text, match_id));
}

/// `@arity.args`: a call's argument_list — count its arguments (the named
/// children; the C `...` at a CALL site never appears here). Keyed by the
/// list's start so the callee ref finds it by adjacency.
pub(super) fn count_args(members: &mut State, node: tree_sitter::Node<'_>) {
    members
        .arg_counts_by_start
        .insert((node.start_position().row, node.start_position().column), node.named_child_count());
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

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Member(MemberCap::Recv) => {
            st.members.member_recv.insert(e.match_id, (e.span(), e.text.clone()));
        }
        Capture::Member(MemberCap::Op) => {
            // Map the operator token's KIND (== its text, an anonymous
            // token) to a MemberOp via the pack's open op_map. Unmapped
            // (`.*`) → no entry → no op-DX. No source-text re-decision.
            if let Some((_, op)) = st.pack.op_map.iter().find(|(k, _)| *k == e.text) {
                st.members.member_op_raw.insert(e.match_id, (*op, e.span()));
            }
        }
        _ => {}
    }
}

pub(super) fn finish(st: &mut ExtractState) {
    st.out.param_sigs = std::mem::take(&mut st.members.param_sigs);
}
