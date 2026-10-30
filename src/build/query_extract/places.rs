//! A pack's places: a member read whose receivers lead down to a variable
//! is a part of that variable (`self.a.b` is `self` → `a` → `b`), keyed by
//! the variable's declaration, never by how the access is spelled
//! (`docs/prompt-cfg-tier.md` §3.3).

use std::collections::{HashMap, HashSet};

use tree_sitter::Point;

use crate::model::conventions::{Invocant, InvocantName};
use crate::model::file_analysis::{Access, Binding, PathStep, Place, PlaceDisturbanceIndex, Span, SymbolId};

use super::{MemberRead, MemberWrite};

/// Resolves an expression span to the place it denotes, from the facts the
/// extractor minted: each variable read's declaration and each member read's
/// receiver.
pub(crate) struct PackPlaces<'a> {
    vars: HashMap<Span, SymbolId>,
    members: HashMap<Span, &'a MemberRead>,
}

impl<'a> PackPlaces<'a> {
    pub(crate) fn new(vars: HashMap<Span, SymbolId>, member_reads: &'a [MemberRead]) -> Self {
        PackPlaces { vars, members: member_reads.iter().map(|m| (m.span, m)).collect() }
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
            if let Some(&sym) = self.vars.get(&at) {
                steps.reverse();
                return Some(Place { root: Binding::Lexical(sym), path: steps });
            }
            let read = self.members.get(&at)?;
            steps.push(PathStep::Member(read.member.clone()));
            at = read.receiver;
        }
        None
    }

    /// A member access's receiver, carrying its place when it is a part of
    /// a variable, so the receiver's narrowings reach the access.
    pub(crate) fn receiver(&self, span: Span, invocant: Invocant) -> Invocant {
        match self.at(span) {
            Some(place) if !place.is_variable() => Invocant::Place {
                name: InvocantName::assume_canonical(invocant.text()),
                place: Box::new(place),
            },
            _ => invocant,
        }
    }

    /// Every access that could disturb a place rooted in `wanted`: a member
    /// write is a write of its place; a variable or member read that no
    /// further member read reads through is handed on whole (an argument, a
    /// call's receiver), an opaque access.
    pub(crate) fn disturbances(&self, wanted: &HashSet<Binding>, writes: &[MemberWrite]) -> PlaceDisturbanceIndex {
        let mut idx = PlaceDisturbanceIndex::default();
        let mut add = |at: Point, access: Access, place: Place| {
            if wanted.contains(&place.root) {
                idx.add(at, access, place);
            }
        };
        for w in writes {
            if let Some(mut place) = self.at(w.receiver) {
                place.path.push(PathStep::Member(w.member.clone()));
                add(w.receiver.start, Access::Write, place);
            }
        }
        let read_through: HashSet<Span> = self.members.values().map(|m| m.receiver).collect();
        for (&span, &sym) in &self.vars {
            if !read_through.contains(&span) {
                add(span.start, Access::Opaque, Place::variable(Binding::Lexical(sym)));
            }
        }
        for &span in self.members.keys() {
            if !read_through.contains(&span) {
                if let Some(place) = self.at(span) {
                    add(span.start, Access::Opaque, place);
                }
            }
        }
        idx.seal();
        idx
    }
}
