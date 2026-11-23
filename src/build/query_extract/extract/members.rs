//! `@member.*` and `@arity.*`: a member access's receiver and operator, and
//! the argument / parameter counts of calls and callables.

use super::*;
use crate::model::conventions::Invocant;
use crate::model::file_analysis::{MemberOp, ParamArity};

/// The call's arguments in order (the `@arity.arg`s of its match's
/// `@arity.args` list), each with whether it spreads (an `@arity.spread` on
/// the argument's node) and the name it is passed by (an `@arity.label`
/// inside it); `None` when the match wrote no argument list.
pub(super) fn call_args(ad: &Adorned, match_id: usize) -> Option<Vec<crate::build::query_extract::WrittenArg>> {
    let list = ad.in_match(match_id, Capture::Arity(ArityCap::Args))?;
    let spreads = |a: &Event| ad.on(a).any(|s| s.cap == Capture::Arity(ArityCap::Spread));
    let label = |a: &Event| {
        ad.within(a).map(|i| &ad.events()[i]).find(|t| t.cap == Capture::Arity(ArityCap::Label)).map(|t| t.text.clone())
    };
    Some(
        ad.list_items(list)
            .filter(|a| a.cap == Capture::Arity(ArityCap::Arg))
            .map(|a| crate::build::query_extract::WrittenArg { span: a.span(), spread: spreads(a), label: label(a) })
            .collect(),
    )
}

/// How many places a call's arguments fill: a named one fills none.
pub(super) fn places(args: &[crate::build::query_extract::WrittenArg]) -> usize {
    args.iter().filter(|a| a.label.is_none()).count()
}

/// The arguments the call passes by name (`@arity.label`s in its match's
/// `@arity.args` list), each its name and token.
pub(super) fn call_labels(ad: &Adorned, match_id: usize) -> Vec<(String, Span)> {
    let Some(list) = ad.in_match(match_id, Capture::Arity(ArityCap::Args)) else {
        return Vec::new();
    };
    ad.list_items(list)
        .filter(|a| a.cap == Capture::Arity(ArityCap::Label))
        .map(|a| (a.text.clone(), a.span()))
        .collect()
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

/// A signature's parameters, in order. A parameter's name is the def its
/// node declares; how a call reaches it is what the document states on it
/// (`@arity.either` and its narrowings), by place where it states nothing.
/// `None` when a parameter has no name.
fn params_of(ad: &Adorned, sig: &Event) -> Option<Vec<crate::model::file_analysis::ParamInfo>> {
    use crate::model::file_analysis::{ParamInfo, ParamPass};
    let mut params = Vec::new();
    for item in ad.list_items(sig) {
        let required = match item.cap {
            Capture::Arity(ArityCap::Required) => true,
            Capture::Arity(ArityCap::Optional) | Capture::Arity(ArityCap::Variadic) => false,
            _ => continue,
        };
        let stated = |cap| ad.on(item).any(|t| t.cap == Capture::Arity(cap));
        let pass = if ad.on(item).any(|t| t.cap == Capture::Arity(ArityCap::Variadic)) {
            if stated(ArityCap::RestPlaces) {
                ParamPass::RestPositional
            } else if stated(ArityCap::RestNames) {
                ParamPass::RestNamed
            } else {
                ParamPass::Rest
            }
        } else if stated(ArityCap::ByName) {
            ParamPass::Named
        } else if stated(ArityCap::ByPlace) {
            ParamPass::Positional
        } else if stated(ArityCap::Either) {
            ParamPass::Either
        } else {
            ParamPass::Positional
        };
        let name = ad.within(item).map(|i| &ad.events()[i]).find(|t| {
            matches!(t.cap, Capture::Def { part: DefPart::Name, .. })
        });
        // A parameter the list does not name (C's `int f(int)`) leaves the
        // list unstated: no call can name it, and a list missing it lies.
        let name = name?;
        let receiver = ad.on(name).any(|t| t.cap == Capture::Receiver);
        let spelled = ad
            .within(item)
            .map(|i| &ad.events()[i])
            .find(|t| matches!(t.cap, Capture::Def { part: DefPart::Spelled, .. }));
        params.push(ParamInfo {
            name: spelled.unwrap_or(name).text.clone(),
            default: (!required && !pass.is_rest()).then(|| {
                let written = ad.within(item).map(|i| &ad.events()[i]).find(|t| t.cap == Capture::Arity(ArityCap::Default));
                written.map_or_else(|| "…".to_string(), |d| d.text.clone())
            }),
            key: (pass.takes_name() && !pass.is_rest() && !receiver).then(|| name.text.clone()),
            pass,
            is_invocant: receiver,
            binding_site: Some(name.start),
        });
    }
    Some(params)
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
                let sig = ad
                    .within(e)
                    .map(|i| &ad.events()[i])
                    .find(|t| t.cap == Capture::Arity(ArityCap::Sig) && !before(t.start, name_end));
                st.out.symbols[row].arity = sig.map(|sig| arity_of(ad, sig));
                st.out.symbols[row].params = sig.and_then(|sig| params_of(ad, sig));
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
