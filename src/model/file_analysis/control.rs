//! Control structure, as each language's document declares it
//! (`docs/adr/cfg-tier.md`): the constructs whose arms the join builder
//! joins, and the statements that leave. Built once per file by the
//! builder that read the document and dropped with it; nothing here is
//! stored on a `FileAnalysis`.

use super::Span;

/// What a construct does with its arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    /// Its arms are alternatives (`if`, `?:`, a short-circuit).
    Branch,
    /// It runs its body while its test holds; `once` when the body runs
    /// before the first test.
    Loop { once: bool },
}

/// Where an arm runs relative to its construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmRole {
    /// Where the condition holds (`true`) or fails (`false`).
    When(bool),
    /// A loop's body.
    Body,
    /// What a loop runs after each pass of its body.
    Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm {
    pub span: Span,
    pub role: ArmRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Construct {
    pub span: Span,
    pub kind: ControlKind,
    pub cond: Option<Span>,
    pub arms: Vec<Arm>,
}

/// Where a leaving statement sends control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveKind {
    Return,
    Throw,
    Exit,
    Break,
    Continue,
    Jump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leave {
    pub span: Span,
    pub kind: LeaveKind,
}

/// One file's control structure: constructs in source order (an outer
/// construct before the ones inside it) and its leaving statements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlView {
    pub constructs: Vec<Construct>,
    pub leaves: Vec<Leave>,
}

impl ControlView {
    /// The innermost arm holding `span` that lies inside `within`, the
    /// region whose own run the answer is relative to (a callable's body,
    /// a block). `None`: `span` runs whenever `within` does.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn arm_around(&self, span: Span, within: Span) -> Option<&Arm> {
        self.constructs
            .iter()
            .flat_map(|c| c.arms.iter())
            .filter(|a| contains(within, a.span) && contains(a.span, span))
            // Arms never overlap partly, so those holding `span` nest: the
            // innermost starts last and, on a tie, ends first.
            .max_by_key(|a| (a.span.start.row, a.span.start.column, std::cmp::Reverse((a.span.end.row, a.span.end.column))))
    }
}

fn contains(outer: Span, inner: Span) -> bool {
    let start = |s: Span| (s.start.row, s.start.column);
    let end = |s: Span| (s.end.row, s.end.column);
    start(outer) <= start(inner) && end(inner) <= end(outer)
}
