//! `Place`: a storage location named by what its root variable denotes and
//! the steps taken from it, never by how it is spelled (`docs/prompt-cfg-tier.md`
//! §3.3). `$Foo::h{a}` under any package and `our %h; $h{a}` inside `Foo` are
//! one place; `$h{a}{b}` and `$h{a}->{b}` are one place; `{'a'}` and `{a}` are
//! one key.

use serde::{Deserialize, Serialize};

use super::SymbolId;

/// What a variable name denotes. Built only by the builder's binding
/// resolution (`Builder::binding_key_in`), so a raw spelling never keys a
/// fact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Binding {
    /// A `my` / `state` / loop variable: its declaring symbol.
    Lexical(SymbolId),
    /// A package variable or `use constant` name: `(package, sigil+name)`.
    /// `our @ISA` under `package B` and `@B::ISA` are the same binding.
    Package(String, String),
}

/// A root binding plus the steps from it. An empty path is the variable
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Place {
    pub root: Binding,
    pub path: Vec<PathStep>,
}

/// One step from a value to a part of it. Closed: every rule over steps is
/// an exhaustive match, so a new step fails to compile until its rule is
/// written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PathStep {
    /// Through a reference to its referent: the arrow of `$x->{a}`, and the
    /// implicit arrow between subscripts (`$h{a}{b}` is `$h{a}->{b}`).
    Deref,
    /// Into a hash's slots.
    Hash(Slots<String>),
    /// Into an array's slots.
    Array(Slots<i64>),
}

/// Which slots a step reaches: one (an element), several (a slice), or any
/// (a slice whose keys are not all known).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Slots<T> {
    One(Slot<T>),
    Many(Vec<Slot<T>>),
    All,
}

/// A slot named by a constant, or by whatever a variable holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Slot<T> {
    Const(T),
    Var(Binding),
}

/// How an access touches a place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Stores into it: the left side of an assignment.
    Write,
    /// Hands it to code that may change what lies under it: a call on it,
    /// an argument, a whole-container operation. A read that steps further
    /// in (`$x->{a}` reading `$x`) is not an access of `$x` at all.
    Opaque,
}

impl<T: PartialEq> Slot<T> {
    /// Could the two name the same slot? A variable may hold anything.
    fn may_equal(&self, other: &Slot<T>) -> bool {
        match (self, other) {
            (Slot::Const(a), Slot::Const(b)) => a == b,
            (Slot::Var(_), _) | (_, Slot::Var(_)) => true,
        }
    }
}

impl<T: PartialEq> Slots<T> {
    fn meets(&self, other: &Slots<T>) -> bool {
        let one = |s: &Slot<T>, many: &[Slot<T>]| many.iter().any(|m| m.may_equal(s));
        match (self, other) {
            (Slots::All, _) | (_, Slots::All) => true,
            (Slots::One(a), Slots::One(b)) => a.may_equal(b),
            (Slots::One(a), Slots::Many(m)) | (Slots::Many(m), Slots::One(a)) => one(a, m),
            (Slots::Many(a), Slots::Many(b)) => a.iter().any(|s| one(s, b)),
        }
    }

    fn vars(&self) -> impl Iterator<Item = &Binding> {
        let slots: &[Slot<T>] = match self {
            Slots::One(s) => std::slice::from_ref(s),
            Slots::Many(m) => m,
            Slots::All => &[],
        };
        slots.iter().filter_map(|s| match s {
            Slot::Var(b) => Some(b),
            Slot::Const(_) => None,
        })
    }
}

impl PathStep {
    /// Could the two steps reach a common part?
    pub fn overlaps(&self, other: &PathStep) -> bool {
        match (self, other) {
            (PathStep::Deref, PathStep::Deref) => true,
            (PathStep::Hash(a), PathStep::Hash(b)) => a.meets(b),
            (PathStep::Array(a), PathStep::Array(b)) => a.meets(b),
            (PathStep::Deref, PathStep::Hash(_) | PathStep::Array(_))
            | (PathStep::Hash(_), PathStep::Deref | PathStep::Array(_))
            | (PathStep::Array(_), PathStep::Deref | PathStep::Hash(_)) => false,
        }
    }

    /// Reaches exactly one slot known by name: the step a path index can
    /// key on.
    pub fn is_exact(&self) -> bool {
        match self {
            PathStep::Deref => true,
            PathStep::Hash(s) => matches!(s, Slots::One(Slot::Const(_))),
            PathStep::Array(s) => matches!(s, Slots::One(Slot::Const(_))),
        }
    }

    fn key_vars(&self) -> Box<dyn Iterator<Item = &Binding> + '_> {
        match self {
            PathStep::Deref => Box::new(std::iter::empty()),
            PathStep::Hash(s) => Box::new(s.vars()),
            PathStep::Array(s) => Box::new(s.vars()),
        }
    }
}

impl Place {
    pub fn variable(root: Binding) -> Place {
        Place { root, path: Vec::new() }
    }

    /// The variable itself, with no step into it.
    pub fn is_variable(&self) -> bool {
        self.path.is_empty()
    }

    /// The bindings whose value names a slot along the path (`$k` in
    /// `$self->{$k}`): rebinding one moves the place.
    pub fn key_vars(&self) -> impl Iterator<Item = &Binding> {
        self.path.iter().flat_map(PathStep::key_vars)
    }

    /// Does `access` on `other` disturb what this place holds? A write
    /// disturbs the place and everything under it; an opaque access
    /// disturbs only what lies strictly under it (a method call on the
    /// slot's value leaves the slot holding that value).
    pub fn disturbed_by(&self, access: Access, other: &Place) -> bool {
        if self.root != other.root {
            return false;
        }
        let under = match access {
            Access::Write => other.path.len() <= self.path.len(),
            Access::Opaque => other.path.len() < self.path.len(),
        };
        under && self.path.iter().zip(&other.path).all(|(a, b)| a.overlaps(b))
    }
}

/// What a narrowing or a guard is about. A plain variable keeps its
/// scope-walked name until variables move onto their bindings
/// (`docs/prompt-cfg-tier.md` §3.3); a part of one is its `Place`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Subject {
    Variable(String),
    Place(Place),
}
