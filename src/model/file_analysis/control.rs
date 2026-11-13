//! Control structure, as each language's document declares it
//! (`docs/adr/cfg-tier.md`): the constructs whose arms the join builder
//! joins, and the statements that leave. Built once per file by the
//! builder that read the document and dropped with it; nothing here is
//! stored on a `FileAnalysis`.

use super::Span;
use std::collections::{HashMap, HashSet};
use tree_sitter::Point;

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

/// Where a construct is entered, as its arms see it: past its test when
/// the test runs first, else at its start, before any arm.
fn entry_point(c: &Construct) -> Point {
    match c.cond {
        Some(cond) if c.arms.iter().all(|a| cond.end <= a.span.start) => cond.end,
        _ => c.span.start,
    }
}

fn contains(outer: Span, inner: Span) -> bool {
    let start = |s: Span| (s.start.row, s.start.column);
    let end = |s: Span| (s.end.row, s.end.column);
    start(outer) <= start(inner) && end(inner) <= end(outer)
}

/// One join a branch mints for a variable: past the construct (`After`)
/// or the value entering it (`Entry`), landing at each of `lands`, fed by
/// the variable as it stands at each of `arms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchJoin {
    pub role: crate::model::witnesses::JoinRole,
    pub of: Span,
    pub lands: Vec<Point>,
    pub arms: Vec<Point>,
}

/// A [`ControlView`] indexed for minting joins: which arm holds a point,
/// and which branch ends where an arm does.
pub struct JoinPlan<'v> {
    view: &'v ControlView,
    /// Every arm as `(construct, arm)`, outer before inner, with the
    /// nearest arm around it.
    arms: Vec<(usize, usize)>,
    around: Vec<Option<usize>>,
    /// Branches by where they end, for an arm a branch closes.
    ends: HashMap<Point, Vec<usize>>,
    arm_spans: HashSet<Span>,
}

impl<'v> JoinPlan<'v> {
    pub fn new(view: &'v ControlView) -> Self {
        let span = |&(c, a): &(usize, usize)| view.constructs[c].arms[a].span;
        let mut arms: Vec<(usize, usize)> =
            view.constructs.iter().enumerate().flat_map(|(c, k)| (0..k.arms.len()).map(move |a| (c, a))).collect();
        arms.sort_by_key(|k| {
            let s = span(k);
            (s.start, std::cmp::Reverse(s.end))
        });
        // Arms nest (one holds another or they are apart), so the open
        // ones form a stack.
        let mut around = Vec::with_capacity(arms.len());
        let mut open: Vec<usize> = Vec::new();
        for (i, k) in arms.iter().enumerate() {
            let s = span(k);
            while open.last().is_some_and(|&o| !contains(span(&arms[o]), s)) {
                open.pop();
            }
            around.push(open.last().copied());
            open.push(i);
        }
        let mut ends: HashMap<Point, Vec<usize>> = HashMap::new();
        for (i, c) in view.constructs.iter().enumerate() {
            if c.kind == ControlKind::Branch {
                ends.entry(c.span.end).or_default().push(i);
            }
        }
        let arm_spans = view.constructs.iter().flat_map(|c| c.arms.iter().map(|a| a.span)).collect();
        JoinPlan { view, arms, around, ends, arm_spans }
    }

    /// The arms holding `at`, innermost first, as `(construct, arm)`.
    pub fn arms_holding(&self, at: Point) -> impl Iterator<Item = (&'v Construct, &'v Arm)> + '_ {
        let span = |i: usize| {
            let (c, a) = self.arms[i];
            self.view.constructs[c].arms[a].span
        };
        let last = self.arms.partition_point(|&(c, a)| self.view.constructs[c].arms[a].span.start <= at);
        let start = last.checked_sub(1);
        std::iter::successors(start, move |&i| self.around[i])
            .filter(move |&i| span(i).end >= at)
            .map(move |i| {
                let (c, a) = self.arms[i];
                (&self.view.constructs[c], &self.view.constructs[c].arms[a])
            })
    }

    /// A branch that is wholly another's arm (an `elsif`) joins with the
    /// construct it is the arm of.
    pub fn is_lifted(&self, c: &Construct) -> bool {
        c.kind == ControlKind::Branch && self.arm_spans.contains(&c.span)
    }

    /// The joins `c` mints for a variable bound from `bound_from`: past
    /// it, and the value entering it, for it and every branch closing one
    /// of its arms. Each arm after the first begins from the value
    /// entering its construct, so an arm never sees a sibling's write,
    /// whether or not the language makes an arm a scope. A path on which the variable is not yet bound
    /// reaches no read of it.
    pub fn branch_joins(&self, c: &'v Construct, bound_from: Option<Point>) -> Vec<BranchJoin> {
        use crate::model::witnesses::JoinRole;
        let bound = |p: &Point| bound_from.is_none_or(|b| b <= *p);
        let mut arms = Vec::new();
        let mut reached = vec![c];
        self.arm_points(c, &mut arms, &mut reached);
        arms.retain(bound);
        arms.sort();
        arms.dedup();
        // Just past the construct: its last arm leaves at its end, and that
        // arm's read must not meet the join it feeds.
        let past = Point { row: c.span.end.row, column: c.span.end.column + 1 };
        let mut out = vec![BranchJoin { role: JoinRole::After, of: c.span, lands: vec![past], arms }];
        for k in reached {
            let entry = entry_point(k);
            // The first arm starts where the test leaves the variable; each
            // one written after another starts over from the entering value.
            let first = k.arms.iter().map(|a| a.span.start).min();
            let mut lands: Vec<Point> = k.arms.iter().map(|a| a.span.start).filter(|&s| s > entry && Some(s) != first).collect();
            // A test written after what it guards runs first.
            if let Some(cond) = k.cond.filter(|cond| k.arms.iter().all(|a| a.span.end <= cond.start)) {
                lands.push(cond.start);
            }
            lands.sort();
            lands.dedup();
            // Unbound on entry, an arm's read never comes from the entry.
            if !lands.is_empty() && bound(&entry) {
                out.push(BranchJoin { role: JoinRole::Entry, of: k.span, lands, arms: vec![entry] });
            }
        }
        out
    }

    /// Where each path through `c` leaves it: the end of every arm (an arm
    /// a branch closes contributes that branch's paths), and, when no arm
    /// runs where the test goes one way, where the construct is entered.
    /// Every branch it passes through lands in `reached`.
    fn arm_points(&self, c: &'v Construct, out: &mut Vec<Point>, reached: &mut Vec<&'v Construct>) {
        for a in &c.arms {
            match self.closing(c, a).filter(|k| reached.len() < 64 && !reached.iter().any(|r| std::ptr::eq(*r, *k))) {
                Some(inner) => {
                    reached.push(inner);
                    self.arm_points(inner, out, reached);
                }
                None => out.push(a.span.end),
            }
        }
        let has = |b: bool| c.arms.iter().any(|a: &Arm| a.role == ArmRole::When(b));
        if !(has(true) && has(false)) && !c.arms.is_empty() {
            out.push(entry_point(c));
        }
    }

    /// The outermost branch inside arm `a` of `c` that ends where `a`
    /// does: its paths are the arm's, since nothing in the arm runs after
    /// it.
    fn closing(&self, c: &Construct, a: &Arm) -> Option<&'v Construct> {
        self.ends
            .get(&a.span.end)?
            .iter()
            .map(|&i| &self.view.constructs[i])
            .filter(|k| !std::ptr::eq(*k, c) && k.span != c.span && contains(a.span, k.span))
            .min_by_key(|k| k.span.start)
    }
}

impl super::FileAnalysis {
    /// For every write a branch holds, the joins of that branch, from the
    /// write out to the variable's binding (or the callable the write runs
    /// in), for an analysis whose variable facts are bound
    /// (`bind_variable_witnesses`).
    pub(crate) fn assemble_joins(&mut self, view: &ControlView) {
        use super::{Binding, Place, ScopeId};
        use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
        if view.constructs.is_empty() || self.flow_edges.is_empty() {
            return;
        }
        let plan = JoinPlan::new(view);
        let mut minted: HashSet<(Binding, Span, JoinRole)> = HashSet::new();
        let mut joins: Vec<Witness> = Vec::new();
        let source = || WitnessSource::Builder(JOIN_SOURCE.into());
        for fe in &self.flow_edges {
            let at = fe.target_at;
            let (binding, home) = self.variable_binding_of(&fe.target_name, at, fe.target_scope);
            let chain = super::scope_chain_of(&self.scopes, fe.target_scope);
            let pos = |id: ScopeId| chain.iter().position(|&s| s == id);
            let callable = chain
                .iter()
                .copied()
                .find(|&s| self.scopes[s.0 as usize].kind.is_callable())
                .unwrap_or(*chain.last().unwrap_or(&fe.target_scope));
            // The joins stop at whichever is nearer: the variable's binding
            // or the callable the write runs in.
            let home_inside = pos(home).is_some_and(|h| pos(callable).is_some_and(|c| h <= c));
            let stop = if home_inside { home } else { callable };
            let within = self.scopes[stop.0 as usize].span;
            let place = Place::variable(binding.clone());
            let bound_from = match binding {
                Binding::Lexical(sid) => Some(self.symbols[sid.0 as usize].span.start),
                _ => None,
            };
            let holding = plan
                .arms_holding(at)
                .take_while(|(_, a)| contains(within, a.span))
                .filter(|(c, _)| c.kind == ControlKind::Branch && !plan.is_lifted(c) && contains(within, c.span));
            for (c, _) in holding {
                // Held to the scope around the branch, so a sibling block
                // never sees it.
                let scope = chain.iter().copied().find(|&s| contains(self.scopes[s.0 as usize].span, c.span)).unwrap_or(stop);
                for BranchJoin { role, of, lands, arms } in plan.branch_joins(c, bound_from) {
                    if !minted.insert((binding.clone(), of, role)) {
                        continue;
                    }
                    let join = WitnessAttachment::Join { place: Box::new(place.clone()), of, role };
                    for p in lands {
                        for payload in [WitnessPayload::Reset, WitnessPayload::Edge(join.clone())] {
                            joins.push(Witness {
                                attachment: WitnessAttachment::Variable { name: fe.target_name.clone(), scope },
                                source: source(),
                                payload,
                                span: Span { start: p, end: p },
                            });
                        }
                    }
                    let read = WitnessAttachment::Place(Box::new(place.clone()));
                    for p in arms {
                        joins.push(Witness {
                            attachment: join.clone(),
                            source: source(),
                            payload: WitnessPayload::Edge(read.clone()),
                            span: Span { start: p, end: p },
                        });
                    }
                }
            }
        }
        for mut w in joins {
            super::bind_variable_witness(&mut w, &self.scopes, |n, at, sc| self.variable_binding_of(n, at, sc));
            self.witnesses.push(w);
        }
    }
}
