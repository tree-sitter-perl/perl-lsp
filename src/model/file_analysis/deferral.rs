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
    /// A class is declared under the identity (the spelled name, resolved
    /// through the file's use-map at mint) in some indexed file.
    NamesClass(ElsewhereClass),
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

/// What a question reads to decide itself. Both homes of a deferred fact
/// implement it, the analysis (`InFile`) and the bag's query context, so
/// a question decides the same way wherever its fact lives.
pub trait DecideFacts {
    fn parents(&self) -> &dyn LocalParents;
    fn index(&self) -> Option<&dyn CrossFileLookup>;
    /// The class of a call ref's invocant. `None` where the ref table is
    /// not in hand (the bag), which decides the question `Unknown`.
    fn invocant_class(&self, call: RefIdx) -> Option<String>;
}

/// An analysis with the index it is asked under.
pub struct InFile<'a> {
    pub fa: &'a FileAnalysis,
    pub index: Option<&'a dyn CrossFileLookup>,
}

impl DecideFacts for InFile<'_> {
    fn parents(&self) -> &dyn LocalParents {
        &self.fa.packages
    }
    fn index(&self) -> Option<&dyn CrossFileLookup> {
        self.index
    }
    fn invocant_class(&self, RefIdx(i): RefIdx) -> Option<String> {
        let r = self.fa.refs().get(i as usize)?;
        self.fa.method_call_invocant_class(r, self.index)
    }
}

impl DecideFacts for crate::model::witnesses::BagContext<'_> {
    fn parents(&self) -> &dyn LocalParents {
        self.package_parents
    }
    fn index(&self) -> Option<&dyn CrossFileLookup> {
        self.module_index
    }
    fn invocant_class(&self, _: RefIdx) -> Option<String> {
        None
    }
}

impl Question {
    /// The one decider. A facts source without an index answers from its
    /// own file alone.
    pub fn decide(&self, facts: &dyn DecideFacts) -> Verdict {
        let tag = match self {
            Question::ReceiverIsa { .. } => "deferral.decide.receiver_isa",
            Question::PackageIsa { .. } => "deferral.decide.package_isa",
            Question::NamesClass(_) => "deferral.decide.names_class",
        };
        crate::util::ghost_stats::count(tag);
        crate::util::ghost_stats::timed(tag, || self.decide_untimed(facts))
    }

    fn decide_untimed(&self, facts: &dyn DecideFacts) -> Verdict {
        match self {
            Question::ReceiverIsa { receiver, class } => {
                let recv = match receiver {
                    Receiver::Class(c) => Some(c.clone()),
                    Receiver::CallInvocant(call) => facts.invocant_class(*call),
                };
                match recv {
                    Some(c) if !c.is_empty() => isa_verdict(&c, class, facts),
                    _ => Verdict::Unknown,
                }
            }
            Question::PackageIsa { package, class } => match package {
                Some(p) => isa_verdict(p, class, facts),
                None => Verdict::No,
            },
            Question::NamesClass(class) => {
                let identity = class.identity();
                match facts.index() {
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
            Question::NamesClass(_) => Fallback::Otherwise,
        }
    }
}

/// `NamesClass`'s local proof, over a symbol table: the decider reads the
/// analysis's, the extractor the one it is still assembling, so the two
/// halves of the question are one function.
pub fn declares_class(symbols: &[Symbol], identity: &str) -> bool {
    symbols.iter().any(|s| matches!(s.kind, SymKind::Package | SymKind::Class) && s.name == identity)
}

/// A class identity this file does not declare. The only way to build one
/// runs the local proof, so a `NamesClass` question is never asked about a
/// class its own file settles, and a decider without the file's symbols
/// (the bag) decides it whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElsewhereClass {
    identity: String,
}

impl ElsewhereClass {
    /// `None` when `symbols` declare `identity`: the site is settled.
    pub fn unless_local(symbols: &[Symbol], identity: String) -> Option<Self> {
        (!declares_class(symbols, &identity)).then_some(Self { identity })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }
}

fn isa_verdict(class: &str, ancestor: &str, facts: &dyn DecideFacts) -> Verdict {
    if class_isa(class, ancestor, facts.parents(), facts.index()) {
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

    /// Both readings, for a rewrite that must reach whichever is picked
    /// (a span remap).
    pub fn readings_mut(&mut self) -> impl Iterator<Item = &mut T> {
        std::iter::once(&mut self.then).chain(self.otherwise.as_mut())
    }

    pub fn pick(&self, fa: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> Picked<&T> {
        self.pick_in(&InFile { fa, index })
    }

    pub fn pick_in(&self, facts: &dyn DecideFacts) -> Picked<&T> {
        let verdict = self.when.decide(facts);
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
            Question::NamesClass(c) => c.identity(),
        }
    }
}
