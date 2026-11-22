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

/// What each kept def holds inside its own node, read off the tree where
/// a separate match captured it:
///
/// - a callable's arity is its own parameter list's (`@arity.sig`): the
///   first one inside the def after the name token, a nested
///   function-pointer declarator's or a lambda's coming later;
/// - a class's inner scope is the first scope opened inside its def;
/// - an enumerator's enum is the nearest class def around it. Its body
///   opens no scope, so the enumerators stay in the scope around the enum.
pub(super) fn finish(st: &mut ExtractState) {
    use crate::model::file_analysis::SymKind;
    let ad = st.adorned;
    let before = |a: Point, b: Point| (a.row, a.column) < (b.row, b.column);
    for (ix, e) in ad.events().iter().enumerate() {
        let Some(row) = st.minted[ix].def_row else { continue };
        let kind = st.out.symbols[row].kind;
        match kind.sym_kind() {
            _ if matches!(kind, DefCap::Sub | DefCap::Method) => {
                let name_end = st.out.symbols[row].name_end;
                st.out.symbols[row].arity = ad
                    .within(e)
                    .map(|i| &ad.events()[i])
                    .find(|t| t.cap == Capture::Arity(ArityCap::Sig) && !before(t.start, name_end))
                    .map(|sig| arity_of(ad, sig));
            }
            SymKind::Class => {
                st.out.symbols[row].inner =
                    ad.within(e).filter(|&i| ad.events()[i].site != e.site).find_map(|i| st.minted[i].opens);
            }
            SymKind::Enumerator => {
                st.out.symbols[row].enum_of = ad.around(e).find_map(|i| {
                    let class = &st.out.symbols[st.minted[i].def_row?];
                    (class.kind.sym_kind() == SymKind::Class).then(|| class.name.clone())
                });
            }
            _ => {}
        }
    }
}
