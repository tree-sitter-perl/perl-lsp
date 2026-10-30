//! A pack's places: a member read whose receivers lead down to a variable
//! is a part of that variable (`self.a.b` is `self` → `a` → `b`), keyed by
//! the variable's declaration, never by how the access is spelled
//! (`docs/prompt-cfg-tier.md` §3.3).

use std::collections::HashMap;

use crate::model::conventions::{Invocant, InvocantName};
use crate::model::file_analysis::{Binding, PathStep, Place, Span, SymbolId};

use super::MemberRead;

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
}
