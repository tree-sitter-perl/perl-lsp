//! A pack's places: a member read whose receivers lead down to a variable
//! is a part of that variable (`self.a.b` is `self` → `a` → `b`), keyed by
//! the variable's declaration, never by how the access is spelled
//! (`docs/prompt-cfg-tier.md` §3.3).

use std::collections::{HashMap, HashSet};

use tree_sitter::Point;

use crate::model::conventions::{Invocant, InvocantName};
use crate::model::file_analysis::{Access, Binding, PathStep, Place, PlaceDisturbanceIndex, Span};

use super::MemberRead;

/// Resolves an expression span to the place it denotes, from the facts the
/// extractor minted: the place each name read denotes (its declaration, or
/// a member of the elided receiver) and each member read's receiver.
pub(crate) struct PackPlaces<'a> {
    /// The place each name read denotes and the read's token, by the
    /// read's value span — the span a member read names its receiver by.
    vars: HashMap<Span, (Place, Span)>,
    members: HashMap<Span, &'a MemberRead>,
    /// The member read each receiver is read through, by the receiver.
    through: HashMap<Span, &'a MemberRead>,
}

impl<'a> PackPlaces<'a> {
    pub(crate) fn new(vars: HashMap<Span, (Place, Span)>, member_reads: &'a [MemberRead]) -> Self {
        PackPlaces {
            vars,
            members: member_reads.iter().map(|m| (m.span, m)).collect(),
            through: member_reads.iter().map(|m| (m.receiver, m)).collect(),
        }
    }

    /// The place `span` denotes: a variable read, or a member read of a
    /// place. A value (a call, a literal) anywhere down the receivers makes
    /// it no place.
    pub(crate) fn at(&self, span: Span) -> Option<Place> {
        let mut steps = Vec::new();
        let mut at = span;
        // Each hop steps to a receiver inside the access; a macro remap can
        // fold spans onto one another, so the hops are bounded.
        for _ in 0..=self.members.len() {
            if let Some((base, _)) = self.vars.get(&at) {
                steps.reverse();
                let mut place = base.clone();
                place.path.extend(steps);
                return Some(place);
            }
            let read = self.members.get(&at)?;
            steps.push(PathStep::Member(read.member.clone()));
            at = read.receiver;
        }
        None
    }

    /// A member access's receiver, carrying its place when it is a part of
    /// a variable that `valued` gives a value, so the receiver's narrowings
    /// reach the access.
    pub(crate) fn receiver(&self, span: Span, invocant: Invocant, valued: &HashSet<Place>) -> Invocant {
        match self.at(span) {
            Some(place) if !place.is_variable() && valued.contains(&place) => Invocant::Place {
                name: InvocantName::assume_canonical(invocant.text()),
                place: Box::new(place),
            },
            _ => invocant,
        }
    }

    /// The place the member access read through `receiver` denotes, when
    /// `valued` gives it a value.
    pub(crate) fn access(&self, receiver: Span, valued: &HashSet<Place>) -> Option<Place> {
        let read = self.through.get(&receiver)?;
        self.at(read.span).filter(|place| valued.contains(place))
    }

    /// Every access that could disturb a place rooted in `wanted`: a place
    /// write is a write where it lands, so what the write itself reads sees
    /// the place as it held coming in; a variable or member read that no
    /// further member read reads through is handed on whole (an argument, a
    /// call's receiver), an opaque access.
    pub(crate) fn disturbances(&self, wanted: &HashSet<Binding>, writes: &[(Span, Point)]) -> PlaceDisturbanceIndex {
        let mut idx = PlaceDisturbanceIndex::default();
        let mut add = |at: Point, access: Access, place: Place| {
            if wanted.contains(&place.root) {
                idx.add(at, access, place);
            }
        };
        for &(w, lands) in writes {
            if let Some(place) = self.at(w) {
                add(lands, Access::Write, place);
            }
        }
        for (&span, (base, _)) in &self.vars {
            if !self.through.contains_key(&span) {
                add(span.start, Access::Opaque, base.clone());
            }
        }
        for &span in self.members.keys() {
            if !self.through.contains_key(&span) {
                if let Some(place) = self.at(span) {
                    add(span.start, Access::Opaque, place);
                }
            }
        }
        idx.seal();
        idx
    }

    /// The token of every name read that reads what `place` holds: the
    /// read, extended through the member reads made of it, runs through
    /// `place`. A read of what holds it (`p` after `p.msg` is moved) hands
    /// the whole on and reads none of the moved state itself.
    pub(crate) fn reads_of(&self, place: &Place) -> Vec<Span> {
        let mut reads: Vec<Span> = self
            .vars
            .iter()
            .filter(|(_, (base, _))| base.root == place.root)
            .filter(|(span, (base, _))| self.read_through(**span, base).reads_into(place))
            .map(|(_, &(_, token))| token)
            .collect();
        reads.sort_by_key(|s| s.start);
        reads
    }

    /// The writes that give `place` a value again: of it, or of what holds it.
    pub(crate) fn writes_to(&self, place: &Place, writes: &[(Span, Point)]) -> Vec<Span> {
        writes
            .iter()
            .map(|&(w, _)| w)
            .filter(|&w| self.at(w).is_some_and(|written| place.disturbed_by(Access::Write, &written)))
            .collect()
    }

    /// The deepest place a name read reaches through the member reads
    /// made of it (`self` in `self.a.b` reaches `self.a.b`).
    fn read_through(&self, span: Span, base: &Place) -> Place {
        let mut place = base.clone();
        let mut at = span;
        for _ in 0..=self.through.len() {
            let Some(read) = self.through.get(&at) else { break };
            place.path.push(PathStep::Member(read.member.clone()));
            at = read.span;
        }
        place
    }
}
