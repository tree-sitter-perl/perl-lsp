//! `@narrow.*`, `@move.*` and the region captures: guard narrowing over
//! the arm its guard enters, and `std::move` sites for the use-after-move check.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// Guard subjects as the walk reaches them: (match, subject, the scope
    /// the guard sits in, its declared type text). The narrowing attaches
    /// in the guard's scope, like the Perl walker's, and the declared type
    /// resolves up the guard's own scope chain, both known only while the
    /// walk is there. The arm can precede the guard in source (`a if
    /// isinstance(x, T) else b`), so the join waits for `finish`.
    narrow_subjects: Vec<(usize, String, ScopeId, Option<String>)>,
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
        Capture::Narrow(NarrowCap::Var) => {
            let subject = (st.pack.shape_name)(&e.text);
            let n = &mut st.narrowing;
            // Innermost declaration first, so a same-named var in a sibling
            // function never supplies the declared type.
            let annot = st
                .scope_stack
                .iter()
                .rev()
                .find_map(|&(_, sid)| n.annot_text_by_var.get(&(subject.clone(), sid)).cloned());
            n.narrow_subjects.push((e.match_id, subject, st.cur_scope, annot));
        }
        Capture::Move(MoveCap::Call) => {
            // Drop moves inside an unevaluated operand
            // (`noexcept(...)`/`sizeof(...)`/`decltype(...)`) — they never
            // execute, so nothing is moved-from (rule #10: the property is
            // "does this move run", asked of the region, not a shape-branch
            // downstream).
            let ad = st.adorned;
            if !st.around(e).any(|a| ad.events()[a].cap == Capture::Unevaluated) {
                st.narrowing.move_calls.push((e.match_id, e.span(), st.cur_scope));
            }
        }
        _ => {}
    }
}

pub(super) fn finish(st: &mut ExtractState) {
    let n = &mut st.narrowing;
    let out = &mut st.out;
    // A guard narrows the arm it enters. Its type text is the guard's own
    // `@narrow.type` when it names one (`dynamic_cast<Derived*>`), else the
    // subject's declared type (the optional-engagement form peels `T` from
    // it); the guard token is absent for the bare `if (opt)` form.
    // TODO(pack-exit-guard): no pack guard narrows a false test, so the
    // early-exit fall-through (`if not isinstance(x, T): return`) waits on a
    // pack-side negation.
    let ad = st.adorned;
    let mut subjects = std::mem::take(&mut n.narrow_subjects);
    subjects.sort_by_key(|s| s.0);
    let mut pending = Vec::new();
    for (mid, subject, scope, annot) in subjects {
        let Some(arm) = ad.in_match(mid, Capture::Narrow(NarrowCap::Arm)) else { continue };
        let Some(ty) = ad.in_match(mid, Capture::Narrow(NarrowCap::Type)).map(|t| &t.text).or(annot.as_ref()) else {
            continue;
        };
        let guard = ad.in_match(mid, Capture::Narrow(NarrowCap::Guard)).map(|g| g.text.as_str());
        if let Some(refined) = (st.pack.narrow_guard)(guard, ty) {
            pending.push((subject, refined, arm.span(), scope));
        }
    }
    // Narrowing cutoffs (THE cross-language lift): truncate each guarded region
    // at the first FlowEdge that rebinds the subject — the SAME edge-driven
    // cutoff the Perl narrowing uses (`RebindIndex`). Deferred to here so
    // the edges exist. The witness gets a REAL region span [start, cutoff], so
    // point-containment ends the narrowing at the rebind — the soundness Perl
    // got from its cutoff, now generic. Every LangPack that narrows (python
    // isinstance, cpp dynamic_cast + optional engagement) gets it free.
    let rebinds = crate::model::file_analysis::RebindIndex::new(&out.flow_edges);
    let mut narrowed = Vec::new();
    for (name, refined, region, scope) in pending {
        let end = rebinds.earliest_in(&name, region).unwrap_or(region.end);
        if (region.start.row, region.start.column) >= (end.row, end.column) {
            continue; // rebound before the region even opens — nothing holds
        }
        let refined = written_type_at(out, &st.pack.names, refined, region.start);
        narrowed.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-narrow".into()),
            payload: crate::model::witnesses::WitnessPayload::InferredType(refined),
            span: Span { start: region.start, end },
        });
    }
    out.witnesses.extend(narrowed);
    for (name, scope, obs, region) in std::mem::take(&mut st.flow.observations) {
        let end = rebinds.earliest_in(&name, region).unwrap_or(region.end);
        if (region.start.row, region.start.column) >= (end.row, end.column) {
            continue;
        }
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-obs".into()),
            payload: crate::model::witnesses::WitnessPayload::Observation(obs),
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
