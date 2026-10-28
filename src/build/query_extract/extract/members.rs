//! `@member.*` and `@arity.*`: a member access's receiver and operator, and
//! the argument / parameter counts of calls and callables.

use super::*;
use crate::model::file_analysis::{MemberOp, ParamArity};

/// The call's arguments in order (its `@arity.arg`s), each with whether
/// it spreads (an `@arity.spread` on the argument's node); `None` when the
/// match wrote no argument list.
pub(super) fn call_args(ad: &Adorned, match_id: usize) -> Option<Vec<(Span, bool)>> {
    let mut wrote_list = false;
    let mut args = Vec::new();
    for t in ad.of_match(match_id) {
        match t.cap {
            Capture::Arity(ArityCap::Args) => wrote_list = true,
            Capture::Arity(ArityCap::Arg) => {
                wrote_list = true;
                args.push((t.span(), ad.on_range(t.range()).any(|s| s.cap == Capture::Arity(ArityCap::Spread))));
            }
            _ => {}
        }
    }
    wrote_list.then_some(args)
}

/// The operator a member match wrote, with its span: the
/// `@member.op.<which>` some pattern names on its `@member.op` token. A
/// token no pattern names claims no operator, so op-DX stays silent rather
/// than guessing.
pub(super) fn op_of(ad: &Adorned, match_id: usize) -> Option<(MemberOp, Span)> {
    let op = ad.in_match(match_id, Capture::Member(MemberCap::Op))?;
    let which = ad.on_range(op.range()).filter_map(|t| match t.cap {
        Capture::MemberOp(which) => Some(which.op()),
        _ => None,
    });
    Some((which.last()?, op.span()))
}

/// `@member.recv`: a member access receiver. Peel transparent wrappers
/// (`(*p)`, `(&o)`, `(p)`) to the typed inner where the node is live, so the
/// minted MethodCall ref's invocant_span lands on the inner expression
/// `expr_type_at_span` already types. op-DX applies only to a bare-variable
/// immediate receiver (its deref_stack resolves by name), so whether the
/// un-peeled node was one rides on the event.
pub(super) fn flatten_recv(node: tree_sitter::Node<'_>, pack: &LangPack, source: &[u8], match_id: usize) -> Event {
    let simple = pack.simple_var_kinds.contains(&node.kind());
    let inner = peel_receiver(node, pack.recv_peel);
    let text = inner.utf8_text(source).unwrap_or("").to_string();
    Event::at(inner, Capture::Member(MemberCap::Recv), text, match_id).with(Payload::SimpleRecv(simple))
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
        .map(|sig| {
            let mut arity = ParamArity { total: 0, required: 0, variadic: false };
            for p in ad.of_match(sig.match_id) {
                match p.cap {
                    Capture::Arity(ArityCap::Required) => {
                        arity.total += 1;
                        arity.required += 1;
                    }
                    Capture::Arity(ArityCap::Optional) => arity.total += 1,
                    Capture::Arity(ArityCap::Variadic) => arity.variadic = true,
                    _ => {}
                }
            }
            (sig.span(), arity)
        })
        .collect();
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
