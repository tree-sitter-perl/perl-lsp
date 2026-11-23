//! `@ref.*`: a name used where it is not declared — calls, member
//! accesses, type positions, labels.

use super::*;

/// A reference its match names by `@ref.spelled`, placed at the token the
/// `spelled` walk reaches from the node the document tagged. The scopes the
/// walk passed are its qualifier, minted here, where the levels are in hand.
pub(super) fn flatten_spelled<'t>(
    node: tree_sitter::Node<'t>,
    cap: Capture,
    links: &super::links::Links<'t>,
    pack: &LangPack,
    source: &[u8],
    match_id: usize,
) -> Flattened<'t> {
    let Some((leaf, scopes)) = links.spelled_leaf(node, source) else {
        return Event::at(node, cap, node.utf8_text(source).unwrap_or("").to_string(), match_id);
    };
    let event = Event::at(leaf, cap, leaf.utf8_text(source).unwrap_or("").to_string(), match_id);
    match scopes {
        Some(scopes) => {
            let qualifier = scopes.join(pack.names.sep().unwrap_or_default());
            with(event, Payload::Qualifier((pack.shape_name)(&qualifier)))
        }
        None => event,
    }
}

pub(super) fn handle(st: &mut ExtractState, ix: usize, e: &Event) {
    let Capture::Ref(ref_kind) = e.cap else { return };
    let pack = st.pack;
    if ref_kind == RefCap::Label {
        st.out.label_refs.push(((pack.shape_name)(&e.text), st.cur_scope, e.span()));
        return;
    }
    // A "reference" on a def's own name token is the declaration, not
    // a use. Only the name: a def node's last token is often a use
    // (python's `def f(o): return o.count` ends at `count`). `ref.type`
    // is exempt: one on the very token a kept def is named by is dropped
    // once the defs are settled (`drop_declarations`).
    let inside_def = ref_kind != RefCap::Type && st.around(e).any(|a| st.minted[a].declares);
    if inside_def {
        return;
    }
    let ad = st.adorned;
    let recv = ad.in_match(e.match_id, Capture::Member(MemberCap::Recv));
    let invocant = recv.and_then(|r| match r.payload.as_deref() {
        Some(Payload::Recv(i)) => Some((r.span(), i.clone(), expr_on(ad, &mut st.minted, &mut st.out, r))),
        _ => None,
    });
    // op-DX applies only to a written-name receiver (its deref_stack
    // resolves by name); a wrapper/chain doesn't.
    let simple = matches!(invocant, Some((_, crate::model::conventions::Invocant::Name(_), _)));
    let member_op = simple.then(|| super::members::op_of(ad, e.match_id)).flatten();
    let arg_count = super::members::call_args(ad, e.match_id).map(|a| super::members::places(&a));
    let qualifier = match e.payload.as_deref() {
        Some(Payload::Qualifier(q)) => Some(q.clone()),
        _ => None,
    };
    let spelled = ad.in_match(e.match_id, Capture::RefSpelled);
    st.minted[ix].ref_row = Some(st.out.refs.len());
    st.out.refs.push(SkelRef {
        kind: ref_kind,
        // `@ref.spelled`: the name as the match spells it, where that is
        // not the reference token's own text.
        name: (pack.shape_name)(spelled.map_or(&e.text, |s| &s.text)),
        start: e.start,
        end: e.end,
        scope: st.cur_scope,
        invocant,
        member_op,
        arg_count,
        constructs: false,
        spaces: space_ask(ad.on(e)),
        qualifier,
        spelled: spelled.map(Event::span),
        access: crate::model::file_analysis::AccessKind::Read,
        arg_keys: super::members::call_labels(ad, e.match_id),
    });
}

/// A `@ref.type` on a def's own name token (a class, enum or typedef
/// declaring itself) is the declaration, not a use: the symbol stays the
/// token's only claimant.
pub(super) fn drop_declarations(st: &mut ExtractState) {
    for ix in 0..st.minted.len() {
        if let (Some(_), Some(name)) = (st.minted[ix].def_row, st.minted[ix].def_name) {
            st.minted[name].names_symbol = true;
        }
    }
    let ad = st.adorned;
    let mut keep = vec![true; st.out.refs.len()];
    for (ix, e) in ad.events().iter().enumerate() {
        let Some(row) = st.minted[ix].ref_row else { continue };
        if st.out.refs[row].kind == RefCap::Type && ad.on_ixs(e).any(|on| st.minted[on].names_symbol) {
            keep[row] = false;
        }
    }
    let mut it = keep.iter();
    st.out.refs.retain(|_| *it.next().unwrap());
}

/// The namespaces the `@space.*` captures on a member token state.
pub(super) fn space_ask<'a>(on: impl Iterator<Item = &'a Event>) -> Option<crate::model::file_analysis::SpaceAsk> {
    use crate::model::file_analysis::SpaceAsk;
    on.fold(None, |acc: Option<SpaceAsk>, t| match t.cap {
        Capture::Space { id, mark } => {
            let a = acc.unwrap_or_default();
            Some(SpaceAsk {
                search: a.search.with(id),
                illegal: if mark == Some(SpaceMark::Illegal) { a.illegal.with(id) } else { a.illegal },
            })
        }
        _ => acc,
    })
}
