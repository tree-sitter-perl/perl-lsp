//! Conditional-compilation arms from a document's `@cond.*` captures.
//!
//! A query document states its arm shapes as captures (`queries/cpp/cond.scm`):
//! `@cond.if` is an arm that tests (its `@cond.test*` is what it tests),
//! `@cond.else` an arm that runs where the one it hangs off fails, and
//! `@cond.guard` the name of an include guard, whose test holds for the rest
//! of the file. `ArmBuilder` turns those into regions, each with the trail
//! of the code written directly in it; a declaration's arm is the innermost
//! region around it. Language-agnostic: a document with no `@cond.*`
//! captures builds no regions, so every position is `ArmId::NONE`.
//! `docs/adr/macro-handling.md`.

use crate::model::file_analysis::{ArmId, CondArms, CondTerm, CondTest};

use super::CondCap;

/// A source position, as (row, column).
type Pos = (usize, usize);

fn pos(p: tree_sitter::Point) -> Pos {
    (p.row, p.column)
}

/// The test one arm states, before it is placed.
#[derive(Clone)]
struct Tested {
    test: CondTest,
    /// `#ifndef`: the arm runs where the name is NOT defined.
    negated: bool,
    /// The token the test was read from: an include guard names it.
    at: (Pos, Pos),
}

/// The `@cond.test*` capture an arm states, as read from its match.
pub struct ArmTest<'a> {
    pub cap: CondCap,
    pub start: tree_sitter::Point,
    pub end: tree_sitter::Point,
    /// The captured node's source text.
    pub text: &'a str,
}

impl ArmTest<'_> {
    fn tested(&self) -> Option<Tested> {
        let at = (pos(self.start), pos(self.end));
        let (test, negated) = match self.cap {
            CondCap::Test => (CondTest::Expr(self.text.split_whitespace().collect::<Vec<_>>().join(" ")), false),
            CondCap::TestDefined => (CondTest::Defined(self.text.trim().to_string()), false),
            CondCap::TestUndefined => (CondTest::Defined(self.text.trim().to_string()), true),
            CondCap::If | CondCap::Else | CondCap::Guard => return None,
        };
        Some(Tested { test, negated, at })
    }
}

/// One arm before it is placed.
struct Region {
    span: (Pos, Pos),
    /// The arm's own test, as written.
    test: Option<Tested>,
    alternative: bool,
}

/// The `@cond.*` arms of one document run, stated in any order.
#[derive(Default)]
pub struct ArmBuilder {
    regions: Vec<Region>,
    guards: Vec<(Pos, Pos)>,
}

impl ArmBuilder {
    /// An arm over `start..end`: what it tests, if it tests, and whether it
    /// runs where the arm it hangs off fails. One node may be stated twice
    /// (an `#elif` is an arm that tests AND an alternative); it is one arm.
    pub fn arm(&mut self, start: tree_sitter::Point, end: tree_sitter::Point, test: Option<ArmTest<'_>>, alternative: bool) {
        let span = (pos(start), pos(end));
        self.regions.push(Region { span, test: test.and_then(|t| t.tested()), alternative });
    }

    /// An include guard's name token: its test holds for the rest of the
    /// file, so it guards nothing inside it.
    pub fn guard(&mut self, start: tree_sitter::Point, end: tree_sitter::Point) {
        self.guards.push((pos(start), pos(end)));
    }

    /// One raw query match's `@cond.*` captures, for a walk outside the
    /// extractor (the macro gather's `#define` walk): a match states one
    /// arm with its test, or an alternative, or a guard.
    pub fn capture_match<'a>(&mut self, caps: impl IntoIterator<Item = (CondCap, tree_sitter::Node<'a>, &'a str)>) {
        let caps: Vec<_> = caps.into_iter().collect();
        let mut test = caps.iter().find_map(|&(k, n, text)| {
            matches!(k, CondCap::Test | CondCap::TestDefined | CondCap::TestUndefined)
                .then(|| ArmTest { cap: k, start: n.start_position(), end: n.end_position(), text })
        });
        for &(k, n, _) in &caps {
            match k {
                CondCap::If => self.arm(n.start_position(), n.end_position(), test.take(), false),
                CondCap::Else => self.arm(n.start_position(), n.end_position(), None, true),
                CondCap::Guard => self.guard(n.start_position(), n.end_position()),
                CondCap::Test | CondCap::TestDefined | CondCap::TestUndefined => {}
            }
        }
    }

    /// Place every arm: each region's trail is the trail of the code around
    /// it plus its own term; an alternative arm's is the trail around the
    /// arm it hangs off plus that arm's negated test (and, for an `#elif`,
    /// its own).
    pub fn finish(self) -> ArmRegions {
        let mut regions = self.regions;
        // Outermost first, so a region's enclosing one is placed before it.
        regions.sort_by(|a, b| a.span.0.cmp(&b.span.0).then(b.span.1.cmp(&a.span.1)));
        // A node stated twice (an `#elif`: an arm that tests, and an
        // alternative) is one arm.
        regions.dedup_by(|dup, kept| {
            let same = dup.span == kept.span;
            if same {
                kept.test = kept.test.take().or(dup.test.take());
                kept.alternative |= dup.alternative;
            }
            same
        });

        let mut arms = CondArms::default();
        let mut placed: Vec<(Pos, Pos, ArmId)> = Vec::with_capacity(regions.len());
        // (end, trail around the arm, own term) of each open region.
        let mut stack: Vec<(Pos, Vec<CondTerm>, Option<CondTerm>, Option<Tested>)> = Vec::new();
        for r in &regions {
            while stack.last().is_some_and(|(end, ..)| *end <= r.span.0) {
                stack.pop();
            }
            let (around, own_term) = match stack.last() {
                // The arm it hangs off is the region it sits in.
                Some((_, outer, _, Some(parent))) if r.alternative => {
                    let mut t = outer.clone();
                    t.push(CondTerm { test: parent.test.clone(), negated: !parent.negated });
                    (t, r.test.as_ref())
                }
                Some((_, outer, own, _)) => {
                    let mut t = outer.clone();
                    t.extend(own.clone());
                    (t, r.test.as_ref())
                }
                None => (Vec::new(), r.test.as_ref()),
            };
            // An include guard's own test holds for everything inside it.
            let own = own_term
                .filter(|t| !(t.negated && !r.alternative && self.guards.contains(&t.at)))
                .map(|t| CondTerm { test: t.test.clone(), negated: t.negated });
            let mut trail = around.clone();
            trail.extend(own.clone());
            placed.push((r.span.0, r.span.1, arms.intern(trail)));
            stack.push((r.span.1, around, own, r.test.clone()));
        }
        ArmRegions { placed, arms }
    }
}

/// Every arm region of one parse, and the trails they intern.
#[derive(Default)]
pub struct ArmRegions {
    /// (start, end, arm), outermost first.
    placed: Vec<(Pos, Pos, ArmId)>,
    arms: CondArms,
}

impl ArmRegions {
    /// The innermost arm around `at`; `NONE` where no conditional encloses
    /// it. The one answer to "is this conditional" for any position.
    pub fn arm_at(&self, at: tree_sitter::Point) -> ArmId {
        let at = pos(at);
        let upto = self.placed.partition_point(|(start, ..)| *start <= at);
        self.placed[..upto]
            .iter()
            .rev()
            .find(|(_, end, _)| at < *end)
            .map_or(ArmId::NONE, |&(.., arm)| arm)
    }

    /// The trail `arm` is guarded by.
    pub fn trail(&self, arm: ArmId) -> &[CondTerm] {
        self.arms.trail(arm)
    }

    /// The trail at `at`.
    pub fn trail_at(&self, at: tree_sitter::Point) -> &[CondTerm] {
        self.trail(self.arm_at(at))
    }

    /// The arm table the ids from `arm_at` point into.
    pub fn into_arms(self) -> CondArms {
        self.arms
    }
}
