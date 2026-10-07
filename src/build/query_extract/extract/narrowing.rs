//! `@narrow.*`, `@move.*` and the region captures: guard narrowing scoped
//! to its block, and `std::move` sites for the use-after-move check.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// guard narrowing: match_id → (var, narrowed-type-text). A guard
    /// (`isinstance(x, Foo)`, `ref $x eq 'Foo'`, `dynamic_cast<Foo>`) and
    /// its guarded `@scope` share a match; when that scope is pushed we
    /// emit a Variable witness scoped to the block — so the narrowed type
    /// holds INSIDE the guard and nowhere else (scope = the refinement's
    /// extent). The condition's captures precede the block in source, so
    /// by the time the `@scope` event fires these are populated.
    narrow_var: HashMap<usize, String>,
    narrow_type: HashMap<usize, String>,
    narrow_guard: HashMap<usize, String>,
    /// A guard narrowing tags its consequence block `@narrow.block` (NOT `@scope`)
    /// so the block mints exactly one scope from the general arm-scope pattern;
    /// this maps a block's start byte → the narrow match, so when that arm @scope
    /// is pushed we recover the guard's var/type/token by block position.
    narrow_block_start: HashMap<usize, usize>,
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
    /// Unevaluated-operand regions (`noexcept(...)`/`sizeof(...)`/`decltype(...)`):
    /// a `std::move` whose call sits inside one is a type-trait, not a move.
    unevaluated: Vec<(usize, usize)>,
    /// `std::move(x)` halves, joined per match: the qualifier (`std`) + name
    /// (`move`) verify the call IS std::move (no query predicates), the var is
    /// the moved subject, the call span the region start + enclosing scope.
    move_scope_txt: HashMap<usize, String>,
    move_name_txt: HashMap<usize, String>,
    move_var_txt: HashMap<usize, String>,
    move_call: HashMap<usize, (Span, ScopeId)>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Narrow(NarrowCap::Block) => {
            st.narrowing.narrow_block_start.entry(e.start_byte).or_insert(e.match_id);
        }
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
    let n = &mut st.narrowing;
    match e.cap {
        Capture::Narrow(NarrowCap::Var) => {
            n.narrow_var.insert(e.match_id, e.text.clone());
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
            n.move_var_txt.insert(e.match_id, e.text.clone());
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

/// A guard narrowing whose block is the scope `id` just pushed → the refined
/// type holds within `id` (invisible outside it). Two join shapes: the narrow
/// condition either shares this @scope's match (python: `consequence: (block)
/// @scope`), or tagged the block by position (`@narrow.block`) so the
/// refinement rides a general arm @scope without a fragile duplicate (cpp
/// if/else arms).
pub(super) fn on_scope_push(st: &mut ExtractState, e: &Event, id: ScopeId) {
    let n = &mut st.narrowing;
    let narrow_mid = n
        .narrow_block_start
        .get(&e.start_byte)
        .copied()
        .filter(|nmid| n.narrow_var.contains_key(nmid))
        .or_else(|| n.narrow_var.contains_key(&e.match_id).then_some(e.match_id));
    let Some((nmid, var)) = narrow_mid.and_then(|nmid| n.narrow_var.get(&nmid).map(|v| (nmid, v.clone())))
    else {
        return;
    };
    let subject = (st.pack.shape_name)(&var);
    // Type text: the guard's own `@narrow.type` when it names one
    // (`dynamic_cast<Derived*>`), else the subject's declared type
    // (the optional-engagement form peels `T` from it). The guard
    // token is absent for the bare `if (opt)` truthiness form.
    // Resolve the subject's declared type up the guard's scope
    // chain (innermost first), so a same-named var in a sibling
    // function never supplies the inner type — the nearest
    // enclosing declaration of `subject` wins.
    let ty = n.narrow_type.get(&nmid).cloned().or_else(|| {
        st.scope_stack
            .iter()
            .rev()
            .find_map(|&(_, sid)| n.annot_text_by_var.get(&(subject.clone(), sid)).cloned())
    });
    let guard = n.narrow_guard.get(&nmid).map(String::as_str);
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
    for (mid, (span, scope)) in &n.move_call {
        if n.move_scope_txt.get(mid).map(String::as_str) == Some("std")
            && n.move_name_txt.get(mid).map(String::as_str) == Some("move")
        {
            if let Some(v) = n.move_var_txt.get(mid) {
                out.moved_from.push(((st.pack.shape_name)(v), *span, *scope));
            }
        }
    }
}
