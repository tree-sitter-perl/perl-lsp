//! `@narrow.*`, `@move.*` and the region captures: guard narrowing over
//! the arms a condition decides, and `std::move` sites for the use-after-move
//! check.

use super::*;
use std::collections::HashMap;

/// A guard subject as the walk reaches it. The narrowing attaches in the
/// guard's scope, like the Perl walker's, and the declared type resolves up
/// the guard's own scope chain, both known only while the walk is there.
/// The arm can precede the guard in source (`a if isinstance(x, T) else
/// b`), so the join waits for `finish`.
struct Subject {
    mid: usize,
    name: String,
    /// The scope the guard sits in.
    scope: ScopeId,
    /// The subject's declared type text.
    annot: Option<String>,
    /// A `@type.<relative>` mark on the guard's type and the class the
    /// guard is written in, which the mark names a class through; outside
    /// every class it names none.
    relative: Option<(RelativeClass, Option<String>)>,
}

#[derive(Default)]
pub(super) struct State {
    narrow_subjects: Vec<Subject>,
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
            let relative = st
                .adorned
                .in_match(e.match_id, Capture::Narrow(NarrowCap::Type))
                .and_then(|t| super::defs::names_relative(st.adorned, t))
                .map(|rel| (rel, super::scopes::enclosing_class(st).map(str::to_string)));
            let n = &mut st.narrowing;
            // Innermost declaration first, so a same-named var in a sibling
            // function never supplies the declared type.
            let annot = st
                .scope_stack
                .iter()
                .rev()
                .find_map(|&(_, sid)| n.annot_text_by_var.get(&(subject.clone(), sid)).cloned());
            n.narrow_subjects.push(Subject { mid: e.match_id, name: subject, scope: st.cur_scope, annot, relative });
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

/// What an atom proves of its subject.
enum Atom {
    /// The subject is this type where the atom holds.
    Is(InferredType),
    Set,
    Unset,
}

impl Atom {
    /// What the subject is where the atom evaluates to `holds`. "Not a
    /// Foo" names no type, so a failed `is` proves nothing.
    fn proves(&self, holds: bool) -> Option<InferredType> {
        match (self, holds) {
            (Atom::Is(t), true) => Some(t.clone()),
            (Atom::Set, false) | (Atom::Unset, true) => Some(InferredType::Undef),
            // TODO(narrow-set-strip): a value that is set is its incoming
            // type without the null, which only a fold over that type knows.
            (Atom::Is(_), false) | (Atom::Set, true) | (Atom::Unset, false) => None,
        }
    }
}

/// A logical step between an atom and a condition above it.
#[derive(Clone, Copy)]
enum Connective {
    Not,
    And,
    Or,
    Group,
}

fn connective(e: &Event) -> Option<Connective> {
    match e.cap {
        Capture::Narrow(NarrowCap::Not) => Some(Connective::Not),
        Capture::Narrow(NarrowCap::And) => Some(Connective::And),
        Capture::Narrow(NarrowCap::Or) => Some(Connective::Or),
        Capture::Narrow(NarrowCap::Group) => Some(Connective::Group),
        _ => None,
    }
}

/// The value an operand under `path` (nearest the operand first) has
/// where the condition at its top is `holds`, if the condition fixes it:
/// `A && B` holding fixes both sides, `A || B` failing fixes both, and the
/// other two outcomes fix neither side alone.
fn through(path: &[Connective], mut holds: bool) -> Option<bool> {
    for c in path.iter().rev() {
        match c {
            Connective::Not => holds = !holds,
            Connective::And if !holds => return None,
            Connective::Or if holds => return None,
            Connective::And | Connective::Or | Connective::Group => {}
        }
    }
    Some(holds)
}

/// The regions match `mid`'s positions cover, each with whether its
/// condition holds there. An exit's region is the rest of the block it
/// sits in.
fn positions(ad: &Adorned<'_, '_>, mid: usize) -> Vec<(Span, bool)> {
    ad.of_match(mid)
        .filter_map(|e| match e.cap {
            Capture::Narrow(NarrowCap::Then) => Some((e.span(), true)),
            Capture::Narrow(NarrowCap::Else) => Some((e.span(), false)),
            Capture::Narrow(NarrowCap::Exit) => {
                // TODO(cfg-exit-join): narrowing past the enclosing block needs the CFG's block-exit join (docs/prompt-cfg-tier.md §3.5).
                let block = ad.tree.node(e.site).enclosing_sites().next()?;
                Some((Span { start: e.end, end: block.ts().end_position() }, false))
            }
            _ => None,
        })
        .collect()
}

/// Every region the atom on `atom`'s node is known in, with its value
/// there: the positions of each condition above it that the climb
/// reaches through connectives alone. An atom under any other node (an
/// argument of a call) is no part of the condition around that node.
fn regions_of(ad: &Adorned<'_, '_>, atom: &Event) -> Vec<(Span, bool)> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    let node = ad.tree.node(atom.site);
    for site in std::iter::once(node).chain(node.enclosing_sites()) {
        let tags = || ad.tags.on(site.id()).map(|ix| &ad.events()[ix]);
        if site != node {
            let Some(c) = tags().find_map(connective) else { break };
            path.push(c);
        }
        for cond in tags().filter(|t| t.cap == Capture::Narrow(NarrowCap::Cond)) {
            out.extend(
                positions(ad, cond.match_id)
                    .into_iter()
                    .filter_map(|(region, holds)| Some((region, through(&path, holds)?))),
            );
        }
    }
    out
}

pub(super) fn finish(st: &mut ExtractState) {
    let n = &mut st.narrowing;
    let out = &mut st.out;
    // An `is` atom's `@narrow.type` names a class, which the language's
    // reading of a type spelling may know better (python's `str` is a
    // string), or one named by where the guard is written (php's `self`),
    // already an identity; with none, the subject is what its declared type
    // holds once the test proves it engaged (the pack's `narrow_guard`: an
    // optional's inner type). An atom is known where the conditions above
    // it put it.
    let ad = st.adorned;
    let mut subjects = std::mem::take(&mut n.narrow_subjects);
    subjects.sort_by_key(|s| s.mid);
    let mut pending = Vec::new();
    for Subject { mid, name: subject, scope, annot, relative } in subjects {
        let Some((kind, node)) = [NarrowCap::Is, NarrowCap::Set, NarrowCap::Unset]
            .into_iter()
            .find_map(|k| ad.in_match(mid, Capture::Narrow(k)).map(|e| (k, e)))
        else {
            continue;
        };
        let atom = match kind {
            NarrowCap::Is => match (ad.in_match(mid, Capture::Narrow(NarrowCap::Type)), &relative) {
                (Some(_), Some((rel, class))) => {
                    class.as_ref().and_then(|c| out.relative_class(*rel, c)).map(InferredType::ClassName)
                }
                (Some(t), None) => {
                    Some((st.pack.annot_type)(&t.text).unwrap_or_else(|| InferredType::ClassName(t.text.clone())))
                }
                (None, _) => annot.as_deref().and_then(st.pack.narrow_guard),
            }
            .map(Atom::Is),
            NarrowCap::Unset => Some(Atom::Unset),
            _ => Some(Atom::Set),
        };
        let Some(atom) = atom else { continue };
        let regions = regions_of(ad, node);
        // A member subject (`isinstance(self.x, T)`) narrows its place, which
        // only the assembled analysis can root (`PackPlaces`). So may a bare
        // name: only assembly knows whether it is a local or a member of an
        // elided receiver, and it keeps the place only for the member.
        let node = ad.in_match(mid, Capture::Narrow(NarrowCap::Var));
        let member = node.is_some_and(|v| ad.on_ixs(v).any(|i| st.minted[i].projects.is_some()));
        for (region, holds) in regions {
            let Some(refined) = atom.proves(holds) else { continue };
            let refined = match relative {
                Some(_) => refined,
                None => written_type_at(out, &st.pack.names, refined, region.start),
            };
            if let Some(v) = node {
                out.place_narrowings.push(crate::build::query_extract::PlaceNarrowing {
                    subject: v.span(),
                    refined: refined.clone(),
                    region,
                });
            }
            if !member {
                pending.push((subject.clone(), refined, region, scope));
            }
        }
    }
    // Each region ends at the first rebind of its subject's binding, which
    // only the assembled analysis knows (`bind_variable_regions`).
    for (name, refined, region, scope) in pending {
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
