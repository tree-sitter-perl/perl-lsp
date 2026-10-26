//! Query-delayed facts: a site whose reading needs other files records the
//! question that picks it, and the reading stays unreadable until the
//! question is decided with the index in hand
//! (`docs/adr/query-delayed-facts.md`).

use super::*;
use crate::model::witnesses::RefIdx;

/// What a deferred site waits on. Closed: every input is a typed fact the
/// producer minted, so a decider never searches the analysis for the site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Question {
    /// The receiver's class descends from `class`.
    ReceiverIsa { receiver: Receiver, class: String },
    /// `package` descends from `class` (local parents ∪ cross-file).
    /// `None` = the site sits in no package, which never descends.
    PackageIsa { package: Option<String>, class: String },
    /// A class is declared under `identity` (the spelled name, resolved
    /// through the file's use-map at mint), here or in any indexed file.
    NamesClass { identity: String },
}

/// How a `ReceiverIsa` question names its receiver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Receiver {
    /// The producer typed it locally (`My::Minion->new`, a typed `has`).
    Class(String),
    /// The invocant of this file's `MethodCall` ref, typed at decide time
    /// through the dispatch ladder (`method_call_invocant_class`).
    CallInvocant(RefIdx),
}

/// A question's answer. `Unknown` is a typing gap, not a negative: the
/// unresolved-dispatch diagnostic reports exactly these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Yes,
    No,
    Unknown,
}

/// Which reading an `Unknown` verdict stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    Then,
    Otherwise,
    Neither,
}

impl Question {
    /// The one decider. `index: None` answers from this file alone.
    pub fn decide(&self, fa: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> Verdict {
        match self {
            Question::ReceiverIsa { receiver, class } => {
                let recv = match receiver {
                    Receiver::Class(c) => Some(c.clone()),
                    Receiver::CallInvocant(RefIdx(i)) => fa
                        .refs()
                        .get(*i as usize)
                        .and_then(|r| fa.method_call_invocant_class(r, index)),
                };
                match recv {
                    Some(c) if !c.is_empty() => isa_verdict(&c, class, fa, index),
                    _ => Verdict::Unknown,
                }
            }
            Question::PackageIsa { package, class } => match package {
                Some(p) => isa_verdict(p, class, fa, index),
                None => Verdict::No,
            },
            Question::NamesClass { identity } => {
                if declares_class(fa.symbols(), identity) {
                    return Verdict::Yes;
                }
                match index {
                    Some(ix) => {
                        let elsewhere = ix
                            .def_candidates(identity)
                            .iter()
                            .any(|c| declares_class(ix.symbols_present(c).symbols(), identity));
                        if elsewhere { Verdict::Yes } else { Verdict::No }
                    }
                    None => Verdict::Unknown,
                }
            }
        }
    }

    /// What an `Unknown` reads as.
    pub fn when_unknown(&self) -> Fallback {
        match self {
            Question::ReceiverIsa { .. } | Question::PackageIsa { .. } => Fallback::Neither,
            Question::NamesClass { .. } => Fallback::Otherwise,
        }
    }
}

/// `NamesClass`'s local proof, over a symbol table: the decider reads the
/// analysis's, the extractor the one it is still assembling, so the two
/// halves of the question are one function.
pub fn declares_class(symbols: &[Symbol], identity: &str) -> bool {
    symbols.iter().any(|s| matches!(s.kind, SymKind::Package | SymKind::Class) && s.name == identity)
}

fn isa_verdict(class: &str, ancestor: &str, fa: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> Verdict {
    if class_isa(class, ancestor, &fa.packages, index) {
        Verdict::Yes
    } else {
        Verdict::No
    }
}

/// A reading that holds only when `when` says so. The fields are private
/// and [`pick`](Self::pick) is the only reader, so no consumer can see a
/// reading without deciding its question first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deferred<T> {
    when: Question,
    then: T,
    otherwise: Option<T>,
}

/// A decided [`Deferred`]: the verdict, and the reading it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picked<U> {
    pub verdict: Verdict,
    pub reading: Option<U>,
}

impl<T> Deferred<T> {
    /// A reading that holds under `when` and has no alternative.
    pub fn new(when: Question, then: T) -> Self {
        Self { when, then, otherwise: None }
    }

    /// A choice between two readings of one site.
    pub fn fork(when: Question, then: T, otherwise: Option<T>) -> Self {
        Self { when, then, otherwise }
    }

    /// The reading an `Unknown` verdict selects: what the site reads as
    /// before anyone decides it.
    pub fn undecided(&self) -> Option<&T> {
        self.reading(self.when.when_unknown())
    }

    pub fn pick(&self, fa: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> Picked<&T> {
        let verdict = self.when.decide(fa, index);
        let side = match verdict {
            Verdict::Yes => Fallback::Then,
            Verdict::No => Fallback::Otherwise,
            Verdict::Unknown => self.when.when_unknown(),
        };
        Picked { verdict, reading: self.reading(side) }
    }

    fn reading(&self, side: Fallback) -> Option<&T> {
        match side {
            Fallback::Then => Some(&self.then),
            Fallback::Otherwise => self.otherwise.as_ref(),
            Fallback::Neither => None,
        }
    }
}

/// A plugin-declared dispatch verb call (`$x->enqueue('T')`), recorded
/// before anyone knows whether the receiver is the verb's target class.
/// Decided at QUERY time (`applicable_dispatches`), never materialized, so
/// candidates in closed files surface like open ones
/// (`docs/adr/receiver-gated-dispatch.md`).
pub type ProvisionalDispatch = Deferred<DispatchCandidate>;

impl ProvisionalDispatch {
    /// The whole-call span: site LOCATION, not gated content, so cursor
    /// pre-filters and the untyped-dispatch diagnostic read it undecided.
    pub(super) fn call_span(&self) -> Span {
        self.then.call_span
    }
    pub(super) fn dispatcher(&self) -> &str {
        &self.then.dispatcher
    }
    /// The class the receiver must descend from.
    pub fn gate(&self) -> &str {
        match &self.when {
            Question::ReceiverIsa { class, .. } | Question::PackageIsa { class, .. } => class,
            Question::NamesClass { identity } => identity,
        }
    }
}
