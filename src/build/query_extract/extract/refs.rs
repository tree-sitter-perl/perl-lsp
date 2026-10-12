//! `@ref.*`: a name used where it is not declared — calls, member
//! accesses, type positions, labels.

use super::*;

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    let Capture::Ref(ref_kind) = e.cap else { return };
    let pack = st.pack;
    if ref_kind == RefKind::Label {
        st.out.label_refs.push(((pack.shape_name)(&e.text), st.cur_scope, e.span()));
        return;
    }
    // Generic suppression: a "reference" inside a def's own
    // header is the declaration, not a use. `ref.type` is exempt:
    // a prototype's RETURN type is the def node's first token
    // (`Widget make_widget();` starts at `Widget`), which is a
    // genuine use — its decl-name overlap is suppressed precisely
    // (exact selection-span match) in `into_file_analysis`.
    let inside_def = ref_kind != RefKind::Type
        && st.defs.def_name_spans.iter().any(|&(s, en)| {
            e.start_byte >= s && e.end_byte <= en && {
                // only suppress when it IS the def name region
                // (cheap heuristic: same start)
                s == e.start_byte || en == e.end_byte
            }
        });
    if inside_def {
        return;
    }
    let m = &st.members;
    let simple = m.member_simple.get(&e.match_id).copied().unwrap_or(false);
    let member_op = simple.then(|| m.member_op_raw.get(&e.match_id).copied()).flatten();
    // Reset-via-method: a rebinding method call on a simple-var
    // receiver (`x.clear()`/`.reset()`/`.assign()`) puts a
    // moved-from object back into a known state — a rebind. Mint
    // a Rebind FlowEdge at the RECEIVER position so the moved-from
    // window (and the narrowing cutoff) end there, sparing the
    // receiver read itself. The pack owns which method names
    // rebind (cpp vocab, like its op_map).
    if ref_kind == RefKind::Member && (pack.rebind_method)(&e.text) && simple {
        if let Some((recv_span, recv_text)) = m.member_recv.get(&e.match_id) {
            st.flow.flow_rebinds.push(((pack.shape_name)(recv_text), st.cur_scope, recv_span.start));
        }
    }
    st.out.refs.push(SkelRef {
        kind: ref_kind,
        name: (pack.shape_name)(&e.text),
        start: e.start,
        end: e.end,
        scope: st.cur_scope,
        invocant: m.member_recv.get(&e.match_id).cloned(),
        member_op,
        // A call ref's arg list opens right where its callee /
        // method token ends; plain (uncalled) member/type refs
        // have no adjacent arg list and stay `None`.
        arg_count: matches!(ref_kind, RefKind::Call | RefKind::QCall | RefKind::Member)
            .then(|| m.arg_counts_by_start.get(&(e.end.row, e.end.column)).copied())
            .flatten(),
    });
}
