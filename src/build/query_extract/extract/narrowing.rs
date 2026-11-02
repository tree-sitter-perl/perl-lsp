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
        let Some(refined) = (st.pack.narrow_guard)(guard, ty) else { continue };
        // A member subject (`isinstance(self.x, T)`) narrows its place, which
        // only the assembled analysis can root (`PackPlaces`). So may a bare
        // name: only assembly knows whether it is a local or a member of an
        // elided receiver, and it keeps the place only for the member.
        let node = ad.in_match(mid, Capture::Narrow(NarrowCap::Var));
        let member = node.is_some_and(|v| ad.on_ixs(v).any(|i| st.minted[i].projects.is_some()));
        if let Some(v) = node {
            let refined = written_type_at(out, &st.pack.names, refined.clone(), arm.span().start);
            out.place_narrowings.push(crate::build::query_extract::PlaceNarrowing {
                subject: v.span(),
                refined,
                region: arm.span(),
            });
        }
        if !member {
            pending.push((subject, refined, arm.span(), scope));
        }
    }
    // Each region ends at the first rebind of its subject's binding, which
    // only the assembled analysis knows (`bind_variable_regions`).
    for (name, refined, region, scope) in pending {
        let refined = written_type_at(out, &st.pack.names, refined, region.start);
        out.variable_regions.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-narrow".into()),
            payload: crate::model::witnesses::WitnessPayload::InferredType(refined),
            span: region,
        });
    }
    for (name, scope, obs, region) in std::mem::take(&mut st.flow.observations) {
        out.variable_regions.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-obs".into()),
            payload: crate::model::witnesses::WitnessPayload::Observation(obs),
            span: region,
        });
    }
    // std::move sites → moved-from facts. The qualifier/name verify the
    // call IS `std::move` here (no query predicates), so the diagnostic never
    // sees the call shape — it reads the recorded subject + span + scope. The
    // subject becomes a place once the assembled analysis can root it.
    let ad = st.adorned;
    let text = |mid, cap| ad.in_match(mid, Capture::Move(cap)).map(|t| t.text.as_str());
    for &(mid, span, scope) in &n.move_calls {
        if text(mid, MoveCap::Scope) == Some("std") && text(mid, MoveCap::Name) == Some("move") {
            if let Some(v) = ad.in_match(mid, Capture::Move(MoveCap::Var)) {
                out.moved_from.push(crate::build::query_extract::MoveCall {
                    spelling: v.text.clone(),
                    subject: v.span(),
                    call: span,
                    scope,
                });
            }
        }
    }
}
