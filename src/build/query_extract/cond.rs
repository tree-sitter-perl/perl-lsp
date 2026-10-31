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

/// The `@cond.*` captures of one document run, collected in any order.
#[derive(Default)]
pub struct ArmBuilder {
    /// `@cond.if` nodes, by match.
    ifs: Vec<(usize, (Pos, Pos))>,
    /// `@cond.test*` tokens, by match.
    tests: Vec<(usize, Tested)>,
    elses: Vec<(Pos, Pos)>,
    guards: Vec<(Pos, Pos)>,
}

impl ArmBuilder {
    /// Record one capture. `text` is the captured node's source text.
    pub fn capture(&mut self, cap: CondCap, node: tree_sitter::Node<'_>, text: &str, match_id: usize) {
        let span = (pos(node.start_position()), pos(node.end_position()));
        let tested = |test, negated| Tested { test, negated, at: span };
        match cap {
            CondCap::If => self.ifs.push((match_id, span)),
            CondCap::Else => self.elses.push(span),
            CondCap::Guard => self.guards.push(span),
            CondCap::Test => {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                self.tests.push((match_id, tested(CondTest::Expr(text), false)))
            }
            CondCap::TestDefined => self.tests.push((match_id, tested(CondTest::Defined(text.trim().to_string()), false))),
            CondCap::TestUndefined => self.tests.push((match_id, tested(CondTest::Defined(text.trim().to_string()), true))),
        }
    }

    /// Place every arm: each region's trail is the trail of the code around
    /// it plus its own term; an alternative arm's is the trail around the
    /// arm it hangs off plus that arm's negated test (and, for an `#elif`,
    /// its own).
    pub fn finish(self) -> ArmRegions {
        struct Region {
            span: (Pos, Pos),
            /// The arm's own test, as written.
            test: Option<Tested>,
            alternative: bool,
        }
        let mut regions: Vec<Region> = Vec::new();
        let mut index: std::collections::HashMap<(Pos, Pos), usize> = Default::default();
        let mut region_at = |span: (Pos, Pos), regions: &mut Vec<Region>| -> usize {
            *index.entry(span).or_insert_with(|| {
                regions.push(Region { span, test: None, alternative: false });
                regions.len() - 1
            })
        };
        for (m, span) in &self.ifs {
            let r = region_at(*span, &mut regions);
            regions[r].test = self.tests.iter().find(|(tm, _)| tm == m).map(|(_, t)| t.clone());
        }
        for span in &self.elses {
            let r = region_at(*span, &mut regions);
            regions[r].alternative = true;
        }
        // Outermost first, so a region's enclosing one is placed before it.
        regions.sort_by(|a, b| a.span.0.cmp(&b.span.0).then(b.span.1.cmp(&a.span.1)));

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
