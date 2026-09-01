//! Fact families and builder source tags, named once.
//!
//! A tag is a string two sides agree on — an emitter in `build/builder/` and
//! a reducer here — and a typo in either half fails silently: the witness
//! lands, nothing claims it, the fold answers as if the fact were never
//! observed. Naming them here makes the agreement checkable by the compiler
//! at the only cost of a `use`.
//!
//! CLAUDE.md's worklist invariants require new Fact families and re-emittable
//! source tags to land here rather than inline at the push site.

/// A return/branch arm that is provably `undef` — a bare `return;`,
/// `return undef`, `return ()`, or an `undef` ternary arm. Carries no
/// rvalue type, so it is a Fact rather than an edge.
pub const FACT_UNDEF_ARM: &str = "undef_arm";

/// A return arm that yields a VALUE, counted whether or not it typed.
///
/// The all-undef verdict needs "did I see every way out", and an empty
/// materialized-arm list cannot answer it: an arm whose edge never resolves
/// is indistinguishable from an arm that does not exist. This Fact is the
/// difference between "every arm is undef" and "every arm I could type is
/// undef", and only the first may claim [`InferredType::Undef`].
///
/// [`InferredType::Undef`]: crate::model::file_analysis::InferredType::Undef
pub const FACT_VALUE_ARM: &str = "value_arm";

/// Which spelling produced an undef arm.
///
/// All three coerce to undef in SCALAR context, which is all any current
/// consumer asks — so reducers match on the Fact's `family` and ignore this.
/// It rides the Fact's `key` as a dormant payload because the spellings
/// diverge in LIST context and that distinction is only recoverable at the
/// emission site: recovering it later means re-walking.
///
/// The likeliest first consumer is hash-key union work
/// (`prompt-type-inference-residual.md` Part 2): in
/// `my %o = (%defaults, f());` an [`UndefArm::EmptyList`] splices nothing
/// while an [`UndefArm::Scalar`] contributes a lone `undef` and leaves the
/// list odd-length. Context-dispatched returns proper (`wantarray`) are a
/// separate, unscheduled axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndefArm {
    /// `undef` — a ONE-element list in list context.
    Scalar,
    /// `return;` or `()` — the EMPTY list in list context.
    ///
    /// This is why `return;` is the recommended spelling: it does the right
    /// thing in both contexts, where `return undef` hands a caller expecting
    /// a list a one-element list containing undef.
    EmptyList,
}

impl UndefArm {
    /// Encode onto the Fact's `value`.
    ///
    /// The `value` slot, not the `key`: both are on the witness either way,
    /// but `key` is a `String` — so riding it costs a heap allocation per
    /// undef arm and a serialized string in every cache blob, while `value`
    /// was already carrying a meaningless `Bool(true)`. Two variants fit a
    /// bool exactly. Nothing reads the bool directly; these two functions
    /// are the interface.
    pub fn as_fact_value(self) -> super::FactValue {
        super::FactValue::Bool(matches!(self, UndefArm::EmptyList))
    }

    /// Decode from a Fact's `value`. `None` when the fact predates the
    /// distinction or carries some other shape.
    pub fn from_fact_value(v: &super::FactValue) -> Option<Self> {
        match v {
            super::FactValue::Bool(true) => Some(UndefArm::EmptyList),
            super::FactValue::Bool(false) => Some(UndefArm::Scalar),
            _ => None,
        }
    }
}
