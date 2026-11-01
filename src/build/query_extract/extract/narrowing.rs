//! `@narrow.*`, `@move.*` and the region captures: guard narrowing over
//! the arm its guard enters, and `std::move` sites for the use-after-move check.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// Guard narrowing, joined per match: a guard (`isinstance(x, Foo)`,
    /// `dynamic_cast<Foo*>(b)`, `opt.has_value()`) and the `@narrow.arm` it
    /// enters. The subject is recorded with the scope the guard sits in (the
    /// narrowing attaches there, like the Perl walker's) and its declared
    /// type text, resolved up the guard's own scope chain. The arm can
    /// precede the guard in source (`a if isinstance(x, T) else b`), so the
    /// join waits for `finish`.
    narrow_var: HashMap<usize, (String, ScopeId, Option<String>)>,
    /// The subject's node, by byte range and span: a subject that is a
    /// member read narrows the place it denotes, decided once the reads are
    /// all in (`finish`).
    narrow_node: HashMap<usize, ((usize, usize), Span)>,
    narrow_type: HashMap<usize, String>,
    narrow_guard: HashMap<usize, String>,
    narrow_arm: HashMap<usize, Span>,
    /// (var name, declaring scope) → its declared-type text, joined per
    /// declaration match. Feeds the token-less optional-engagement narrowing
    /// (`if (opt)`): the guard names no type, so the refinement reads the
    /// subject's declared `std::optional<T>`. Keyed by SCOPE (not bare name), so
    /// two functions each declaring an `opt` of a DIFFERENT `optional<T>` peel
    /// the right inner type — resolved by the guard's scope chain at consumption.
    /// Written by the flow family, where a declaration's scope is known.
    pub(super) annot_text_by_var: HashMap<(String, ScopeId), String>,
    /// Unevaluated-operand regions (`noexcept(...)`/`sizeof(...)`/`decltype(...)`):
    /// a `std::move` whose call sits inside one is a type-trait, not a move.
    unevaluated: Vec<(usize, usize)>,
    /// `std::move(x)` halves, joined per match: the qualifier (`std`) + name
    /// (`move`) verify the call IS std::move (no query predicates), the var is
    /// the moved subject, the call span the region start + enclosing scope.
    move_scope_txt: HashMap<usize, String>,
    move_name_txt: HashMap<usize, String>,
    move_var_txt: HashMap<usize, (String, Span)>,
    move_call: HashMap<usize, (Span, ScopeId)>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Unevaluated => st.narrowing.unevaluated.push((e.start_byte, e.end_byte)),
        // Control-flow construct spans: the use-after-move check reads these
        // to decide whether a move is straight-line in its scope.
        Capture::GuardRegion => st.out.control_regions.push(e.span()),
        // Parameter-list spans: the use-after-move check reads these to tell
        // a moved parameter (not flagged) from a moved local.
        Capture::ParamRegion => st.out.param_regions.push(e.span()),
        _ => {}
    }
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    let subject = (st.pack.shape_name)(&e.text);
    let n = &mut st.narrowing;
    match e.cap {
        Capture::Narrow(NarrowCap::Var) => {
            // Innermost declaration first, so a same-named var in a sibling
            // function never supplies the declared type.
            let annot = st
                .scope_stack
                .iter()
                .rev()
                .find_map(|&(_, sid)| n.annot_text_by_var.get(&(subject.clone(), sid)).cloned());
            n.narrow_var.insert(e.match_id, (subject, st.cur_scope, annot));
            n.narrow_node.insert(e.match_id, ((e.start_byte, e.end_byte), e.span()));
        }
        Capture::Narrow(NarrowCap::Arm) => {
            n.narrow_arm.insert(e.match_id, e.span());
        }
        Capture::Narrow(NarrowCap::Type) => {
            n.narrow_type.insert(e.match_id, e.text.clone());
        }
        Capture::Narrow(NarrowCap::Guard) => {
            n.narrow_guard.insert(e.match_id, e.text.clone());
        }
        Capture::Move(MoveCap::Scope) => {
            n.move_scope_txt.insert(e.match_id, e.text.clone());
        }
        Capture::Move(MoveCap::Name) => {
            n.move_name_txt.insert(e.match_id, e.text.clone());
        }
        Capture::Move(MoveCap::Var) => {
            n.move_var_txt.insert(e.match_id, (e.text.clone(), e.span()));
        }
        Capture::Move(MoveCap::Call) => {
            // Drop moves inside an unevaluated operand — they never execute,
            // so nothing is moved-from (rule #10: the property is "does this
            // move run", asked of the region, not a shape-branch downstream).
            let unevaluated_move = n.unevaluated.iter().any(|&(s, en)| e.start_byte >= s && e.end_byte <= en);
            if !unevaluated_move {
                n.move_call.insert(e.match_id, (e.span(), st.cur_scope));
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
    let mut pending = Vec::new();
    let mut arm_mids: Vec<usize> = n.narrow_arm.keys().copied().collect();
    arm_mids.sort_unstable();
    for mid in arm_mids {
        let Some((subject, scope, annot)) = n.narrow_var.get(&mid) else { continue };
        let Some(ty) = n.narrow_type.get(&mid).or(annot.as_ref()) else { continue };
        let guard = n.narrow_guard.get(&mid).map(String::as_str);
        let Some(refined) = (st.pack.narrow_guard)(guard, ty) else { continue };
        // A member subject (`isinstance(self.x, T)`) narrows its place, which
        // only the assembled analysis can root (`PackPlaces`).
        match n.narrow_node.get(&mid) {
            Some((bytes, span)) if st.flow.is_member_read(*bytes) => {
                let refined = written_type_at(out, &st.pack.names, refined, n.narrow_arm[&mid].start);
                out.place_narrowings.push(crate::build::query_extract::PlaceNarrowing {
                    subject: *span,
                    refined,
                    region: n.narrow_arm[&mid],
                });
            }
            _ => pending.push((subject.clone(), refined, n.narrow_arm[&mid], *scope)),
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
        let Some(held) = crate::model::file_analysis::cut_region(region, [rebinds.earliest_in(&name, region)]) else {
            continue; // rebound before the region even opens — nothing holds
        };
        let refined = written_type_at(out, &st.pack.names, refined, region.start);
        narrowed.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-narrow".into()),
            payload: crate::model::witnesses::WitnessPayload::InferredType(refined),
            span: held,
        });
    }
    out.witnesses.extend(narrowed);
    for (name, scope, obs, region) in std::mem::take(&mut st.flow.observations) {
        let Some(held) = crate::model::file_analysis::cut_region(region, [rebinds.earliest_in(&name, region)]) else {
            continue;
        };
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-obs".into()),
            payload: crate::model::witnesses::WitnessPayload::Observation(obs),
            span: held,
        });
    }
    // std::move sites → moved-from facts. The qualifier/name verify the
    // call IS `std::move` here (no query predicates), so the diagnostic never
    // sees the call shape — it reads the recorded subject + span + scope. The
    // subject becomes a place once the assembled analysis can root it.
    let mut move_mids: Vec<&usize> = n.move_call.keys().collect();
    move_mids.sort_unstable();
    for mid in move_mids {
        let (span, scope) = &n.move_call[mid];
        if n.move_scope_txt.get(mid).map(String::as_str) == Some("std")
            && n.move_name_txt.get(mid).map(String::as_str) == Some("move")
        {
            if let Some((v, subject)) = n.move_var_txt.get(mid) {
                out.moved_from.push(crate::build::query_extract::MoveCall {
                    spelling: v.clone(),
                    subject: *subject,
                    call: *span,
                    scope: *scope,
                });
            }
        }
    }
}
