//! What a deferred fact waits on: a question minted with the fact and
//! decided where a copy is handed to a reader, with the index in hand.

use super::*;

/// What a deferred site waits on. Closed: every input is a typed fact the
/// producer minted, so a decider never searches the analysis for the site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// `package` descends from a class under any of `prefixes` — a
    /// plugin's `ClassIsa` trigger the file alone did not fire.
    ClassIsaPrefix { package: String, prefixes: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Yes,
    No,
}

/// What a question reads to decide itself.
pub trait DecideFacts {
    fn parents(&self) -> &dyn LocalParents;
    fn index(&self) -> Option<&dyn CrossFileLookup>;
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
}

impl Question {
    /// The one decider.
    pub fn decide(&self, facts: &dyn DecideFacts) -> Verdict {
        match self {
            Question::ClassIsaPrefix { package, prefixes } => {
                let holds = prefixes
                    .iter()
                    .any(|prefix| class_isa_prefix(package, prefix, facts.parents(), facts.index()));
                if holds { Verdict::Yes } else { Verdict::No }
            }
        }
    }
}
