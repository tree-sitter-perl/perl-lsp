//! `@ref.*`: a name used where it is not declared — calls, member
//! accesses, type positions, labels.

use super::*;

#[derive(Default)]
pub(super) struct State {
    /// `@ref.spelled`, joined to the match's reference token.
    spelled_by_match: std::collections::HashMap<usize, String>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::RefSpelled => {
            st.refs.spelled_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::AccessWrite => st.out.write_sites.push(e.span()),
        _ => {}
    }
}

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
    let inside_def = ref_kind != RefCap::Type
        && st.defs.def_name_spans.iter().any(|&(s, en)| e.start >= s && e.end <= en);
    if inside_def {
        return;
    }
    let m = &st.members;
    let simple = m.member_simple.get(&e.match_id).copied().unwrap_or(false);
    let member_op = simple.then(|| m.op_of(e.match_id)).flatten();
    let arg_count = m.arg_count(e.match_id);
    st.out.refs.push(SkelRef {
        kind: ref_kind,
        name: (pack.shape_name)(st.refs.spelled_by_match.get(&e.match_id).unwrap_or(&e.text)),
        start: e.start,
        end: e.end,
        scope: st.cur_scope,
        invocant: m.member_recv.get(&e.match_id).cloned(),
        member_op,
        arg_count,
        constructs: false,
    });
}
