//! `@ref.*`: a name used where it is not declared — calls, member
//! accesses, type positions, labels.

use super::*;

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    let Capture::Ref(ref_kind) = e.cap else { return };
    let pack = st.pack;
    if ref_kind == RefCap::Label {
        st.out.label_refs.push(((pack.shape_name)(&e.text), st.cur_scope, e.span()));
        return;
    }
    // A "reference" on a def's own name token is the declaration, not
    // a use. Only the name: a def node's last token is often a use
    // (python's `def f(o): return o.count` ends at `count`). `ref.type`
    // is exempt; its decl-name overlap is suppressed precisely (exact
    // selection-span match) in `into_file_analysis`.
    let inside_def = ref_kind != RefCap::Type && st.around(e).any(|a| st.minted[a].declares);
    if inside_def {
        return;
    }
    let ad = st.adorned;
    let recv = ad.in_match(e.match_id, Capture::Member(MemberCap::Recv));
    let invocant = recv.and_then(|r| match r.payload.as_deref() {
        Some(Payload::Recv(i)) => Some((r.span(), i.clone())),
        _ => None,
    });
    // op-DX applies only to a written-name receiver (its deref_stack
    // resolves by name); a wrapper/chain doesn't.
    let simple = matches!(invocant, Some((_, crate::model::conventions::Invocant::Name(_))));
    let member_op = simple.then(|| super::members::op_of(ad, e.match_id)).flatten();
    let arg_count = super::members::call_args(ad, e.match_id).map(|a| a.len());
    st.out.refs.push(SkelRef {
        kind: ref_kind,
        // `@ref.spelled`: the name as the match spells it, where that is
        // not the reference token's own text.
        name: (pack.shape_name)(ad.in_match(e.match_id, Capture::RefSpelled).map_or(&e.text, |s| &s.text)),
        start: e.start,
        end: e.end,
        scope: st.cur_scope,
        invocant,
        member_op,
        arg_count,
        constructs: false,
    });
}
