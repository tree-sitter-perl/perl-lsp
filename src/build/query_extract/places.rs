//! A pack's places: a member read whose receivers lead down to a variable
//! is a part of that variable (`self.a.b` is `self` → `a` → `b`), keyed by
//! the variable's declaration, never by how the access is spelled
//! (`docs/prompt-cfg-tier.md` §3.3).

use std::collections::HashSet;

use tree_sitter::Point;

use crate::model::conventions::{Invocant, InvocantName};
use crate::model::file_analysis::{Access, Binding, PathStep, Place, PlaceDisturbanceIndex, Span};

use super::{ExprId, MemberRead, PlaceWrite};

/// What a name read denotes: its place (its declaration, or a member of
/// the elided receiver), the read's token, and where its value starts.
pub(crate) struct VarPlace {
    pub(crate) place: Place,
    pub(crate) token: Span,
    pub(crate) at: Point,
}

/// Resolves a node to the place it denotes, from the facts the extractor
/// minted: the place each name read denotes and each member read's
/// receiver. Each column is indexed by `ExprId`.
pub(crate) struct PackPlaces<'a> {
    vars: Vec<Option<VarPlace>>,
    members: Vec<Option<&'a MemberRead>>,
    /// The member read each receiver is read through, by the receiver.
    through: Vec<Option<&'a MemberRead>>,
}

impl<'a> PackPlaces<'a> {
    pub(crate) fn new(vars: Vec<Option<VarPlace>>, member_reads: &'a [MemberRead]) -> Self {
        let mut members = vec![None; vars.len()];
        let mut through = vec![None; vars.len()];
        for m in member_reads {
            members[m.expr.index()] = Some(m);
            through[m.receiver_expr.index()] = Some(m);
        }
        PackPlaces { vars, members, through }
    }

    /// The place `expr` denotes: a variable read, or a member read of a
    /// place. A value (a call, a literal) anywhere down the receivers makes
    /// it no place.
    pub(crate) fn at(&self, expr: ExprId) -> Option<Place> {
        let mut steps = Vec::new();
        let mut at = expr;
        // Each hop steps to a receiver inside the access, so the hops end;
        // the bound holds against a document that names an access as its
        // own receiver.
        for _ in 0..=self.members.len() {
            if let Some(base) = &self.vars[at.index()] {
                steps.reverse();
                let mut place = base.place.clone();
                place.path.extend(steps);
                return Some(place);
            }
            let read = self.members[at.index()]?;
            steps.push(PathStep::Member(read.member.clone()));
            at = read.receiver_expr;
        }
        None
    }

    /// The member token of the member read on `expr`.
    pub(crate) fn member_token(&self, expr: ExprId) -> Option<Span> {
        self.members[expr.index()].map(|m| m.token)
    }

    /// A member access's receiver, carrying its place when it is a part of
    /// a variable that `valued` gives a value, so the receiver's narrowings
    /// reach the access.
    pub(crate) fn receiver(&self, expr: ExprId, invocant: Invocant, valued: &HashSet<Place>) -> Invocant {
        match self.at(expr) {
            Some(place) if !place.is_variable() && valued.contains(&place) => Invocant::Place {
                name: InvocantName::assume_canonical(invocant.text()),
                place: Box::new(place),
            },
            _ => invocant,
        }
    }

    /// The place the member access read through `receiver` denotes, when
    /// `valued` gives it a value.
    pub(crate) fn access(&self, receiver: ExprId, valued: &HashSet<Place>) -> Option<Place> {
        let read = self.through[receiver.index()]?;
        self.at(read.expr).filter(|place| valued.contains(place))
    }

    /// Every access that could disturb a place rooted in `wanted`: a place
    /// write is a write where it lands, so what the write itself reads sees
    /// the place as it held coming in; a variable or member read that no
    /// further member read reads through is handed on whole (an argument, a
    /// call's receiver), an opaque access.
    pub(crate) fn disturbances(&self, wanted: &HashSet<Binding>, writes: &[PlaceWrite]) -> PlaceDisturbanceIndex {
        let mut idx = PlaceDisturbanceIndex::default();
        let mut add = |at: Point, access: Access, place: Place| {
            if wanted.contains(&place.root) {
                idx.add(at, access, place);
            }
        };
        for w in writes {
            if let Some(place) = self.at(w.expr) {
                add(w.lands, Access::Write, place);
            }
        }
        for (ix, base) in self.vars.iter().enumerate() {
            if let (Some(base), None) = (base, self.through[ix]) {
                add(base.at, Access::Opaque, base.place.clone());
            }
        }
        for (ix, read) in self.members.iter().enumerate() {
            if let (Some(read), None) = (read, self.through[ix]) {
                if let Some(place) = self.at(read.expr) {
                    add(read.span.start, Access::Opaque, place);
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
            .enumerate()
            .filter_map(|(ix, base)| Some((ExprId(ix as u32), base.as_ref()?)))
            .filter(|(_, base)| base.place.root == place.root)
            .filter(|(expr, base)| self.read_through(*expr, &base.place).reads_into(place))
            .map(|(_, base)| base.token)
            .collect();
        reads.sort_by_key(|s| s.start);
        reads
    }

    /// The writes that give `place` a value again: of it, or of what holds it.
    pub(crate) fn writes_to(&self, place: &Place, writes: &[PlaceWrite]) -> Vec<Span> {
        writes
            .iter()
            .filter(|w| self.at(w.expr).is_some_and(|written| place.disturbed_by(Access::Write, &written)))
            .map(|w| w.token)
            .collect()
    }

    /// The deepest place a name read reaches through the member reads
    /// made of it (`self` in `self.a.b` reaches `self.a.b`).
    fn read_through(&self, expr: ExprId, base: &Place) -> Place {
        let mut place = base.clone();
        let mut at = expr;
        for _ in 0..=self.through.len() {
            let Some(read) = self.through[at.index()] else { break };
            place.path.push(PathStep::Member(read.member.clone()));
            at = read.expr;
        }
        place
    }
}
