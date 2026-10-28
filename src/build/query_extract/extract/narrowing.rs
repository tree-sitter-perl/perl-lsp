//! `@narrow.*`, `@move.*` and the region captures: guard narrowing scoped
//! to its block, and `std::move` sites for the use-after-move check.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// Recognized narrowings deferred to after flow-edge minting, so the region
    /// cutoff can read the edges (`finish`) — (subject, refined type, FULL
    /// guarded-block region, block scope).
    pending_narrow: Vec<(String, InferredType, Span, ScopeId)>,
    /// (var name, declaring scope) → its declared-type text, joined per
    /// declaration match. Feeds the token-less optional-engagement narrowing
    /// (`if (opt)`): the guard names no type, so the refinement reads the
    /// subject's declared `std::optional<T>`. Keyed by SCOPE (not bare name), so
    /// two functions each declaring an `opt` of a DIFFERENT `optional<T>` peel
    /// the right inner type — resolved by the guard's scope chain at consumption.
    /// Written by the flow family, where a declaration's scope is known.
    pub(super) annot_text_by_var: HashMap<(String, ScopeId), String>,
    /// `std::move(x)` calls that run, in source order, with the scope each
    /// sits in: (match, call span, scope).
    move_calls: Vec<(usize, Span, ScopeId)>,
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        // Control-flow construct spans: the use-after-move check reads these
        // to decide whether a move is straight-line in its scope.
        Capture::GuardRegion => st.out.control_regions.push(e.span()),
        // Parameter-list spans: the use-after-move check reads these to tell
        // a moved parameter (not flagged) from a moved local.
        Capture::ParamRegion => st.out.param_regions.push(e.span()),
        Capture::Move(MoveCap::Call) => {
            // Drop moves inside an unevaluated operand
            // (`noexcept(...)`/`sizeof(...)`/`decltype(...)`) — they never
            // execute, so nothing is moved-from (rule #10: the property is
            // "does this move run", asked of the region, not a shape-branch
            // downstream).
            let ad = st.adorned;
            if !st.around(e).any(|a| ad.events[a].cap == Capture::Unevaluated) {
                st.narrowing.move_calls.push((e.match_id, e.span(), st.cur_scope));
            }
        }
        _ => {}
    }
}

/// A guard narrowing whose block is the scope `id` just pushed → the refined
/// type holds within `id` (invisible outside it). Two join shapes: the narrow
/// condition either shares this @scope's match (python: `consequence: (block)
/// @scope`), or tagged the block node (`@narrow.block`) so the refinement
/// rides a general arm @scope without a fragile duplicate (cpp if/else arms).
pub(super) fn on_scope_push(st: &mut ExtractState, e: &Event, id: ScopeId) {
    let ad = st.adorned;
    let var_of = |mid: usize| ad.in_match(mid, Capture::Narrow(NarrowCap::Var)).map(|v| (mid, &v.text));
    let tagged = ad.on_range(e.range()).find(|t| t.cap == Capture::Narrow(NarrowCap::Block));
    let Some((nmid, var)) = tagged.and_then(|t| var_of(t.match_id)).or_else(|| var_of(e.match_id)) else {
        return;
    };
    let n = &mut st.narrowing;
    let subject = (st.pack.shape_name)(var);
    // Type text: the guard's own `@narrow.type` when it names one
    // (`dynamic_cast<Derived*>`), else the subject's declared type
    // (the optional-engagement form peels `T` from it). The guard
    // token is absent for the bare `if (opt)` truthiness form.
    // Resolve the subject's declared type up the guard's scope
    // chain (innermost first), so a same-named var in a sibling
    // function never supplies the inner type — the nearest
    // enclosing declaration of `subject` wins.
    let ty = ad.in_match(nmid, Capture::Narrow(NarrowCap::Type)).map(|t| t.text.clone()).or_else(|| {
        st.scope_stack
            .iter()
            .rev()
            .find_map(|&(_, sid)| n.annot_text_by_var.get(&(subject.clone(), sid)).cloned())
    });
    let guard = ad.in_match(nmid, Capture::Narrow(NarrowCap::Guard)).map(|g| g.text.as_str());
    if let Some(refined) = ty.and_then(|t| (st.pack.narrow_guard)(guard, &t)) {
        // Defer: the region cutoff (first rebind edge) needs the
        // FlowEdges, minted after the walk. Carry the FULL
        // guarded-block region [start, end]; `finish` truncates it at
        // the earliest rebind.
        n.pending_narrow.push((subject, refined, e.span(), id));
    }
}

pub(super) fn finish(st: &mut ExtractState) {
    let n = &mut st.narrowing;
    let out = &mut st.out;
    // Narrowing cutoffs (THE cross-language lift): truncate each guarded region
    // at the first FlowEdge that rebinds the subject — the SAME edge-driven
    // cutoff the Perl narrowing uses (`earliest_rebind_in`). Deferred to here so
    // the edges exist. The witness gets a REAL region span [start, cutoff], so
    // point-containment ends the narrowing at the rebind — the soundness Perl
    // got from its cutoff, now generic. Every LangPack that narrows (python
    // isinstance, cpp dynamic_cast + optional engagement) gets it free.
    for (name, refined, region, scope) in std::mem::take(&mut n.pending_narrow) {
        let refined = written_type_at(out, &st.pack.names, refined, region.start);
        let end = crate::model::file_analysis::earliest_rebind_in(&out.flow_edges, &name, region)
            .unwrap_or(region.end);
        if (region.start.row, region.start.column) >= (end.row, end.column) {
            continue; // rebound before the region even opens — nothing holds
        }
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-narrow".into()),
            payload: crate::model::witnesses::WitnessPayload::InferredType(refined),
            span: Span { start: region.start, end },
        });
    }
    // std::move sites → moved-from facts. The qualifier/name verify the
    // call IS `std::move` here (no query predicates), so the diagnostic never
    // sees the call shape — it reads the recorded var + span + scope.
    let ad = st.adorned;
    let text = |mid, cap| ad.in_match(mid, Capture::Move(cap)).map(|t| t.text.as_str());
    for &(mid, span, scope) in &n.move_calls {
        if text(mid, MoveCap::Scope) == Some("std") && text(mid, MoveCap::Name) == Some("move") {
            if let Some(v) = text(mid, MoveCap::Var) {
                out.moved_from.push(((st.pack.shape_name)(v), span, scope));
            }
        }
    }
}
