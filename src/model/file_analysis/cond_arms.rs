//! Conditional-compilation arms: which `#if` arm a declaration sits in.
//!
//! The extractor mints each declaration's innermost arm as an `ArmId` on the
//! symbol (and on each `MacroDef`); the trails themselves are interned once
//! per file in `PackFacts::cond_arms`. A query ranks a candidate by
//! evaluating its trail against the asker's config — nothing is pruned, and
//! a language whose query document declares no arms mints `ArmId::NONE`
//! everywhere, which is unconditional by construction.
//! `docs/adr/macro-handling.md`.

use super::*;

/// A declaration's innermost conditional arm, an index into its file's
/// `CondArms`. `NONE` is code no conditional encloses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct ArmId(u32);

impl ArmId {
    pub const NONE: ArmId = ArmId(0);

    /// True for code no conditional encloses — the one answer to "is this
    /// directive or declaration conditional".
    pub fn is_none(self) -> bool {
        self == ArmId::NONE
    }
}

/// What one enclosing arm tests.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CondTest {
    /// `#if EXPR` / `#elif EXPR`: the condition as written, whitespace
    /// collapsed. Source text, parsed by the guard evaluator.
    Expr(String),
    /// `#ifdef NAME`; `#ifndef NAME` is its negation.
    Defined(String),
}

/// One term of a trail: an enclosing arm's test, and whether the code sits
/// where that test fails (an `#else`/`#elif` past it, or an `#ifndef`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CondTerm {
    pub test: CondTest,
    pub negated: bool,
}

impl CondTerm {
    pub fn heap_bytes(&self) -> usize {
        match &self.test {
            CondTest::Expr(s) | CondTest::Defined(s) => s.capacity(),
        }
    }
}

/// A file's arm table: one trail per arm, each the AND of its terms,
/// outermost first. Equal trails share one id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CondArms {
    trails: Vec<Vec<CondTerm>>,
}

impl CondArms {
    /// The id of `trail`, added when new. An empty trail is `NONE`.
    pub fn intern(&mut self, trail: Vec<CondTerm>) -> ArmId {
        if trail.is_empty() {
            return ArmId::NONE;
        }
        let at = match self.trails.iter().position(|t| *t == trail) {
            Some(i) => i,
            None => {
                self.trails.push(trail);
                self.trails.len() - 1
            }
        };
        ArmId(at as u32 + 1)
    }

    /// The terms `arm` is guarded by; empty for `NONE`.
    pub fn trail(&self, arm: ArmId) -> &[CondTerm] {
        match arm.0 {
            0 => &[],
            n => self.trails.get(n as usize - 1).map_or(&[], Vec::as_slice),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.trails.is_empty()
    }

    pub fn heap_bytes(&self) -> usize {
        vcap(&self.trails)
            + self
                .trails
                .iter()
                .map(|t| vcap(t) + t.iter().map(CondTerm::heap_bytes).sum::<usize>())
                .sum::<usize>()
    }
}
