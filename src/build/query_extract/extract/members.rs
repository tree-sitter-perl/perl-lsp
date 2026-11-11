//! `@member.*` and `@arity.*`: a member access's receiver and operator, and
//! the argument / parameter counts of calls and callables.

use super::*;
use crate::model::conventions::Invocant;
use crate::model::file_analysis::{MemberOp, ParamArity};

/// The call's arguments in order (the `@arity.arg`s of its match's
/// `@arity.args` list), each with whether it spreads (an `@arity.spread` on
/// the argument's node); `None` when the match wrote no argument list.
pub(super) fn call_args(ad: &Adorned, match_id: usize) -> Option<Vec<(Span, bool)>> {
    let list = ad.in_match(match_id, Capture::Arity(ArityCap::Args))?;
    let spreads = |a: &Event| ad.on(a).any(|s| s.cap == Capture::Arity(ArityCap::Spread));
    Some(
        ad.list_items(list)
            .filter(|a| a.cap == Capture::Arity(ArityCap::Arg))
            .map(|a| (a.span(), spreads(a)))
            .collect(),
    )
}

/// The operator a member match wrote, with its span: the
/// `@member.op.<which>` some pattern names on its `@member.op` token. A
/// token no pattern names claims no operator, so op-DX stays silent rather
/// than guessing.
pub(super) fn op_of(ad: &Adorned, match_id: usize) -> Option<(MemberOp, Span)> {
    let op = ad.in_match(match_id, Capture::Member(MemberCap::Op))?;
    let which = ad.on(op).filter_map(|t| match t.cap {
        Capture::MemberOp(which) => Some(which.op()),
        _ => None,
    });
    Some((which.last()?, op.span()))
}

/// `@member.recv.name` / `@member.recv`: a member access receiver, which
/// the document states is a written name or an expression. An expression
/// in a wrapper (`(p)`, `(*p)`, `(&o)`) stays the wrapper: the document's
/// `@expr.wrapper` edge types it as what it holds.
pub(super) fn flatten_recv<'t>(
    node: tree_sitter::Node<'t>,
    cap: MemberCap,
    source: &[u8],
    match_id: usize,
) -> Flattened<'t> {
    let named = cap == MemberCap::RecvName;
    let text = node.utf8_text(source).unwrap_or("").to_string();
    // A written name (`w`, `Widget`) may spell a class; an expression never does.
    let invocant = if named { Invocant::assume_canonical(text.clone()) } else { Invocant::expr(text.clone()) };
    with(Event::at(node, Capture::Member(MemberCap::Recv), text, match_id), Payload::Recv(invocant))
}

/// A signature's arity: the parameters tagged in its list.
fn arity_of(ad: &Adorned, sig: &Event) -> ParamArity {
    let mut arity = ParamArity { total: 0, required: 0, variadic: false };
    for t in ad.list_items(sig) {
        match t.cap {
            Capture::Arity(ArityCap::Required) => {
                arity.total += 1;
                arity.required += 1;
            }
            Capture::Arity(ArityCap::Optional) => arity.total += 1,
            Capture::Arity(ArityCap::Variadic) => arity.variadic = true,
            _ => {}
        }
    }
    arity
}

/// Callable parameter arities by parameter_list span, in match order;
/// associated to the def symbol by span containment in
/// `into_file_analysis` (`@arity.sig` fires a separate match from the def
/// name).
pub(super) fn finish(st: &mut ExtractState) {
    let ad = st.adorned;
    st.out.param_sigs = ad
        .by_match()
        .filter(|e| e.cap == Capture::Arity(ArityCap::Sig))
        .map(|sig| (sig.span(), arity_of(ad, sig)))
        .collect();
}
