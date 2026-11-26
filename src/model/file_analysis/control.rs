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
    /// It tests nothing, but a `break` aimed at it leaves it (a `switch`),
    /// and a `continue` too when `next` (Perl's bare block).
    Block { next: bool },
    /// It runs its body, and where the body unwinds, a handler; `quiet`
    /// when an unwinding body lands past it with no handler (Perl's `eval`).
    Try { quiet: bool },
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
    /// What a try runs first.
    Tried,
    /// What a try runs where its body unwinds.
    Handler,
    /// What a try runs where its body completes, before what follows it
    /// (python's `try … else`).
    Settled,
    /// What a try runs past it on every path.
    Finally,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm {
    pub span: Span,
    pub role: ArmRole,
    /// Where its last statement ends: a construct ending there closes the
    /// arm even when the arm's own closer (a `}`) follows it.
    pub tail: Point,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Construct {
    pub span: Span,
    pub kind: ControlKind,
    pub cond: Option<Span>,
    pub arms: Vec<Arm>,
    /// The name a jump can aim at it by, as a [`ControlView::labels`] index.
    pub label: Option<u32>,
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
    pub target: LeaveTarget,
}

/// Which construct a `break` or `continue` leaves, of those around it that
/// it can leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeaveTarget {
    /// The innermost.
    #[default]
    Nearest,
    /// The innermost carrying this name ([`ControlView::labels`]).
    Label(u32),
    /// The n-th, counting the innermost as one.
    Levels(u32),
}

/// One file's control structure: constructs in source order (an outer
/// construct before the ones inside it) and its leaving statements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlView {
    pub constructs: Vec<Construct>,
    pub leaves: Vec<Leave>,
    /// The names constructs carry and jumps aim at, each once.
    pub labels: Vec<String>,
    /// Statement-position calls whose callee may not come back: control
    /// reaches what follows one only if the call returns, which the effect
    /// lane decides at query time (`docs/adr/cfg-tier.md` §Effects).
    pub gates: Vec<Span>,
    /// The callables' bodies: how one ends is how a call to it ends. What
    /// sits in a body inside another is that body's, not the outer one's.
    pub bodies: Vec<CallableBody>,
    /// Every statement that returns, wherever it sits: one the view cannot
    /// place still says its callable may come back.
    pub returns: Vec<Span>,
    /// Where a tried arm may unwind (`@ctl.raise`): a write none of these
    /// follows inside the arm reaches no handler. Sorted by start; only
    /// those inside a try's arms are kept.
    pub raises: Vec<Span>,
}

/// A callable's body as the view holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallableBody {
    pub span: Span,
    /// Whether the parser read it whole; one it could not states nothing.
    pub readable: bool,
    /// Whether the callable exists from before its text runs (a Perl named
    /// sub), so it can be called anywhere its variables are bound.
    pub hoisted: bool,
    /// Where the callable is made, when that runs before code written ahead
    /// of it: one made in a statement modifier's condition is made before
    /// the statement runs.
    pub made_before: Option<Point>,
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

/// What each leave of `view` aims at ([`Aim`]). Constructs nest, so the
/// ones open around a leave form a stack, swept once in source order.
fn aims(view: &ControlView) -> Vec<Aim> {
    let mut aims = vec![Aim::Out; view.leaves.len()];
    let mut order: Vec<usize> = (0..view.leaves.len()).filter(|&i| matches!(view.leaves[i].kind, LeaveKind::Break | LeaveKind::Continue)).collect();
    if order.is_empty() {
        return aims;
    }
    order.sort_by_key(|&i| view.leaves[i].span.start);
    let mut catching: Vec<usize> =
        (0..view.constructs.len()).filter(|&k| !matches!(view.constructs[k].kind, ControlKind::Branch)).collect();
    catching.sort_by_key(|&k| (view.constructs[k].span.start, std::cmp::Reverse(view.constructs[k].span.end)));
    let span = |k: usize| view.constructs[k].span;
    let mut open: Vec<usize> = Vec::new();
    let mut next = catching.into_iter().peekable();
    for i in order {
        let l = view.leaves[i];
        while let Some(k) = next.next_if(|&k| span(k).start <= l.span.start) {
            while open.last().is_some_and(|&o| !contains(span(o), span(k))) {
                open.pop();
            }
            open.push(k);
        }
        while open.last().is_some_and(|&o| !contains(span(o), l.span)) {
            open.pop();
        }
        let leaves = |k: &&usize| match (view.constructs[**k].kind, l.kind) {
            (ControlKind::Loop { .. }, _) | (ControlKind::Block { .. }, LeaveKind::Break) => true,
            (ControlKind::Block { next }, _) => next,
            (ControlKind::Branch | ControlKind::Try { .. }, _) => false,
        };
        let mut around = open.iter().rev().filter(leaves);
        let found = match l.target {
            LeaveTarget::Nearest => around.next(),
            LeaveTarget::Label(name) => around.find(|&&k| view.constructs[k].label == Some(name)),
            LeaveTarget::Levels(n) => around.nth((n as usize).saturating_sub(1)),
        };
        aims[i] = match (found, l.target) {
            (Some(&k), _) if matches!(view.constructs[k].kind, ControlKind::Loop { .. }) => Aim::Loop(k),
            (Some(&k), _) if matches!(view.constructs[k].kind, ControlKind::Block { .. }) => Aim::Block(k),
            (Some(_), _) | (None, LeaveTarget::Label(_) | LeaveTarget::Levels(_)) => Aim::Through,
            (None, LeaveTarget::Nearest) => Aim::Out,
        };
    }
    aims
}

/// The point just before `p`, where a read sees nothing landing at `p`.
fn before(p: Point) -> Point {
    match p.column {
        0 => Point { row: p.row.saturating_sub(1), column: usize::MAX },
        c => Point { row: p.row, column: c - 1 },
    }
}

/// The place as it stands at `at`, on a path no test decides.
fn at_point(at: Point) -> Span {
    Span { start: at, end: at }
}

fn read(at: Point) -> JoinArm {
    JoinArm { at, arm: None, untaken: None, gates: Vec::new(), mute: false }
}

/// A loop's arms, by index into its construct's, and where its head lands.
struct LoopParts {
    ix: usize,
    body: usize,
    step: Option<usize>,
    other: Option<usize>,
    once: bool,
    heads: Vec<Point>,
}

/// Just past `span`: where a join past a construct lands, so a read at
/// the construct's own end (its last arm's) never meets the join it feeds.
fn past(span: Span) -> Point {
    Point { row: span.end.row, column: span.end.column + 1 }
}

/// One join a branch mints for a place: past the construct (`After`)
/// or the value entering it (`ArmStart`), landing at each of `lands`, fed by
/// each of `arms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchJoin {
    pub role: crate::model::witnesses::JoinRole,
    pub of: Span,
    pub lands: Vec<Point>,
    pub arms: Vec<JoinArm>,
}

/// One path into a join: the place as it stands at `at`, or, where no arm
/// runs because a test went one way (`untaken`), what that test proves of
/// the place, when it proves anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinArm {
    pub at: Point,
    /// The arm whose end this is, on a path an arm runs.
    pub arm: Option<Span>,
    pub untaken: Option<Untaken>,
    /// The calls on the path that control must come back from to reach
    /// `at` ([`ControlView::gates`]).
    pub gates: Vec<Span>,
    /// In a test's region: the path proves nothing of the place once its
    /// gates come back, so where it runs the region answers nothing.
    pub mute: bool,
}

/// The path through a construct on which no arm runs: the construct, and
/// where its test leaves control, the site of what the test proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Untaken {
    pub of: Span,
    pub tested: Point,
}

/// A [`ControlView`] indexed for minting joins: which arm holds a point,
/// and which branch ends where an arm does.
pub struct JoinPlan<'v> {
    view: &'v ControlView,
    /// Every arm as `(construct, arm)`, outer before inner, with the
    /// nearest arm around it.
    arms: Vec<(usize, usize)>,
    around: Vec<Option<usize>>,
    /// Per arm in `arms`: control never comes out of its end.
    leaves: Vec<bool>,
    /// Per arm in `arms`: the gates it holds with no arm between them.
    gates: Vec<Vec<Span>>,
    /// Each construct's index, by its address.
    index: HashMap<*const Construct, usize>,
    /// Constructs by their span.
    at_span: HashMap<Span, Vec<usize>>,
    /// Branches by where they end, for an arm a branch closes.
    ends: HashMap<Point, Vec<usize>>,
    arm_spans: HashSet<Span>,
    /// Per leave in the view: for a `break` or `continue`, whether it
    /// lands somewhere the view models, and the loop it leaves.
    aims: Vec<Aim>,
}

/// Where a `break` or `continue` sends control, as the view sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aim {
    /// Out of the arms around it, to somewhere they do not reach (a
    /// `return`, or a jump with no construct to leave in the view).
    Out,
    /// Out of loop `k`'s body: past it (`break`) or to its next test.
    Loop(usize),
    /// Past block `k` (a bare block, a `switch`), which the view does not
    /// join: what follows it runs, so nothing inside `k` leaves the arm
    /// around it.
    Block(usize),
    /// Past a name or a count that names no construct around it: modelled
    /// as falling through, so no arm around it is said to leave.
    Through,
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
            if !c.arms.is_empty() {
                ends.entry(c.span.end).or_default().push(i);
            }
        }
        let arm_spans = view.constructs.iter().flat_map(|c| c.arms.iter().map(|a| a.span)).collect();
        let index = view.constructs.iter().enumerate().map(|(i, c)| (c as *const Construct, i)).collect();
        let mut at_span: HashMap<Span, Vec<usize>> = HashMap::new();
        for (i, c) in view.constructs.iter().enumerate() {
            at_span.entry(c.span).or_default().push(i);
        }
        let aims = aims(view);
        let n = arms.len();
        let mut plan = JoinPlan { view, leaves: vec![false; n], gates: vec![Vec::new(); n], arms, around, index, at_span, ends, arm_spans, aims };
        plan.mark_leaving();
        for &g in &view.gates {
            let held = plan.holding_ix(g.start).find(|&i| contains(plan.arm_span(i), g));
            if let Some(i) = held {
                plan.gates[i].push(g);
            }
        }
        plan
    }

    /// Marks every arm control never comes out of: one a leaving statement
    /// sits in with no arm between them, or one holding a branch that leaves
    /// on every path. Inner constructs are decided before the arms around
    /// them.
    fn mark_leaving(&mut self) {
        let view = self.view;
        for (i, l) in view.leaves.iter().enumerate() {
            if matches!(self.aims[i], Aim::Through | Aim::Block(_)) {
                continue;
            }
            let held = self.holding_ix(l.span.start).find(|&i| contains(self.arm_span(i), l.span));
            if let Some(i) = held.filter(|&i| !self.jumped_past(i, l.span)) {
                self.mark_left(i);
            }
        }
        let mut order: Vec<usize> = (0..self.view.constructs.len()).collect();
        order.sort_by_key(|&k| {
            let s = self.view.constructs[k].span;
            (std::cmp::Reverse(s.start), s.end)
        });
        for k in order {
            let c = &view.constructs[k];
            let has = |b: bool| c.arms.iter().any(|a| a.role == ArmRole::When(b));
            let gone = match c.kind {
                ControlKind::Branch => has(true) && has(false) && (0..c.arms.len()).all(|a| self.leaves[self.arm_ix(k, a)]),
                // A try is gone when no path comes out of it.
                ControlKind::Try { .. } => {
                    let mut out = Vec::new();
                    self.try_points(c, &mut out, &mut vec![c], &[]);
                    out.is_empty()
                }
                _ => false,
            };
            if !gone {
                continue;
            }
            let holder = self.holding_ix(c.span.start).find(|&i| self.arms[i].0 != k && contains(self.arm_span(i), c.span));
            if let Some(i) = holder.filter(|&i| !self.jumped_past(i, c.span)) {
                self.mark_left(i);
            }
        }
    }

    /// Whether code at `at` in arm `i` sits in a block inside the arm that a
    /// jump leaves before control reaches `at`, so control may come out past
    /// the block and on through the arm.
    fn jumped_past(&self, i: usize, at: Span) -> bool {
        self.jumped_past_in(self.arm_span(i), at)
    }

    /// As [`Self::jumped_past`], for a block inside `region`. A jump at or
    /// past `at` runs only once control is there, so where `at` leaves, the
    /// jump never runs.
    fn jumped_past_in(&self, region: Span, at: Span) -> bool {
        self.view.leaves.iter().zip(&self.aims).any(|(l, aim)| match *aim {
            Aim::Block(k) => {
                let b = self.view.constructs[k].span;
                contains(region, b) && contains(b, at) && l.span.start < at.start
            }
            _ => false,
        })
    }

    /// Marks arm `i` left, and every arm on the same code: an `else` two
    /// constructs both name (an `if` and the `elif` its chain lifts) is one
    /// arm of each.
    fn mark_left(&mut self, i: usize) {
        let span = self.arm_span(i);
        let mut j = i;
        while j > 0 && self.arm_span(j - 1) == span {
            j -= 1;
        }
        while j < self.arms.len() && self.arm_span(j) == span {
            self.leaves[j] = true;
            j += 1;
        }
    }

    fn arm_span(&self, i: usize) -> Span {
        let (c, a) = self.arms[i];
        self.view.constructs[c].arms[a].span
    }

    fn arm_ix(&self, c: usize, a: usize) -> usize {
        let s = self.view.constructs[c].arms[a].span;
        let first = self.arms.partition_point(|&k| {
            let t = self.arm_span_of(k);
            (t.start, std::cmp::Reverse(t.end)) < (s.start, std::cmp::Reverse(s.end))
        });
        (first..self.arms.len()).find(|&i| self.arms[i] == (c, a)).expect("every arm is indexed")
    }

    fn arm_span_of(&self, (c, a): (usize, usize)) -> Span {
        self.view.constructs[c].arms[a].span
    }

    /// The arms holding `at`, innermost first, as indices into `arms`.
    fn holding_ix(&self, at: Point) -> impl Iterator<Item = usize> + '_ {
        let last = self.arms.partition_point(|&k| self.arm_span_of(k).start <= at);
        std::iter::successors(last.checked_sub(1), move |&i| self.around[i]).filter(move |&i| self.arm_span(i).end >= at)
    }

    /// The arms holding `at`, innermost first, as `(construct, arm)`.
    pub fn arms_holding(&self, at: Point) -> impl Iterator<Item = (&'v Construct, &'v Arm)> + '_ {
        self.holding_ix(at).map(move |i| {
            let (c, a) = self.arms[i];
            (&self.view.constructs[c], &self.view.constructs[c].arms[a])
        })
    }

    /// Whether some path through `c` (or a branch closing one of its arms)
    /// is one control never comes out of, or may not be: past a gate.
    pub fn drops_a_path(&self, c: &'v Construct) -> bool {
        self.drops_by(c, true)
    }

    /// As [`Self::drops_a_path`], counting a gate as a way out only when
    /// `gates` says so.
    fn drops_by(&self, c: &'v Construct, gates: bool) -> bool {
        let mut reached = vec![c];
        self.arm_points(c, &mut Vec::new(), &mut reached, &[]);
        reached.iter().any(|k| {
            let ix = self.index[&(*k as *const Construct)];
            (0..k.arms.len()).any(|a| {
                let i = self.arm_ix(ix, a);
                self.leaves[i] || (gates && !self.gates[i].is_empty())
            })
        })
    }

    /// How a call to the callable whose body is `body` can end, as its
    /// statements say: each leave in it, and its end where control comes out
    /// there, each with the calls on its path ([`JoinArm::gates`]). A jump
    /// out of the body to nothing the view models comes back to the caller.
    pub fn body_ends(&self, body: Span) -> Vec<(crate::model::witnesses::Ends, Point, Vec<Span>)> {
        use crate::model::witnesses::Ends;
        let nested: Vec<Span> =
            self.view.bodies.iter().map(|b| b.span).filter(|&b| b != body && contains(body, b)).collect();
        let own = |s: Span| contains(body, s) && !nested.iter().any(|&b| contains(b, s));
        let direct = |s: Span| {
            own(s)
                && !self.jumped_past_in(body, s)
                && !self.holding_ix(s.start).any(|i| contains(self.arm_span(i), s) && contains(body, self.arm_span(i)))
        };
        let mut out = Vec::new();
        let mut cut = false;
        // A throw a try around it in the body catches lands in its handler.
        let caught = |s: Span| {
            self.holding_ix(s.start).filter(|&i| contains(self.arm_span(i), s) && contains(body, self.arm_span(i))).any(|i| {
                let (k, a) = self.arms[i];
                let c = &self.view.constructs[k];
                c.arms[a].role == ArmRole::Tried && Self::catches(c)
            })
        };
        for (l, aim) in self.view.leaves.iter().zip(&self.aims) {
            if !own(l.span) || (l.kind == LeaveKind::Throw && caught(l.span)) {
                continue;
            }
            let ends = match (l.kind, aim) {
                (LeaveKind::Throw, _) => Ends::UNWINDS,
                (LeaveKind::Exit, _) => Ends::EXITS,
                (LeaveKind::Return, _) | (_, Aim::Out) => Ends::COMPLETES,
                _ => continue,
            };
            cut |= direct(l.span);
            out.push((ends, l.span.start, Vec::new()));
        }
        // A branch with both ways covered whose every arm leaves, sitting in
        // the body itself, leaves too.
        cut |= self.view.constructs.iter().enumerate().any(|(k, c)| {
            let has = |b: bool| c.arms.iter().any(|a| a.role == ArmRole::When(b));
            c.kind == ControlKind::Branch
                && has(true)
                && has(false)
                && direct(c.span)
                && (0..c.arms.len()).all(|a| self.leaves[self.arm_ix(k, a)])
        });
        if !cut {
            let gates = self.view.gates.iter().copied().filter(|&g| direct(g)).collect();
            out.push((Ends::COMPLETES, body.end, gates));
        }
        out
    }

    /// How a call to `sid`, whose body is `body`, ends, as `Effect` arms on
    /// its `SymbolEffects`: a way out per leave and, where control reaches
    /// it, the body's end past the calls it makes there ([`Self::body_ends`]).
    /// A return anywhere in it, in a nested body too, comes back: one too
    /// many only says a call may come back.
    pub fn effect_arms(&self, sid: super::SymbolId, body: Span) -> Vec<crate::model::witnesses::Witness> {
        use crate::model::witnesses::{Effect, Ends, Witness, WitnessAttachment, WitnessPayload, WitnessSource, EFFECT_ARM_SOURCE};
        let attachment = WitnessAttachment::SymbolEffects(sid);
        let arm = |payload: WitnessPayload, at: Point| Witness {
            attachment: attachment.clone(),
            source: WitnessSource::Builder(EFFECT_ARM_SOURCE.into()),
            payload,
            span: Span { start: at, end: at },
        };
        let mut out: Vec<Witness> = self
            .body_ends(body)
            .into_iter()
            .map(|(ends, at, gates)| {
                let effect = WitnessPayload::Effect(Effect::Ends(ends));
                let payload = match gates.is_empty() {
                    true => effect,
                    false => WitnessPayload::Gated {
                        gates: gates.into_iter().map(WitnessAttachment::Expr).collect(),
                        arm: Some(Box::new(effect)),
                    },
                };
                arm(payload, at)
            })
            .collect();
        if let Some(ret) = self.view.returns.iter().find(|&&r| contains(body, r)) {
            out.push(arm(WitnessPayload::Effect(Effect::Ends(Ends::COMPLETES)), ret.start));
        }
        out
    }

    /// The construct `c` joins with: the one it is wholly an arm of (an
    /// `elsif`), climbing, else itself.
    pub fn lifted_into(&self, mut c: &'v Construct) -> &'v Construct {
        for _ in 0..64 {
            if !self.is_lifted(c) {
                break;
            }
            let Some((outer, _)) = self.arms_holding(c.span.start).find(|(k, a)| a.span == c.span && !std::ptr::eq(*k, c))
            else {
                break;
            };
            c = outer;
        }
        c
    }

    /// The construct at exactly `span` of `kind`, if the view has one.
    pub fn construct_at(&self, span: Span, kind: ControlKind) -> Option<&'v Construct> {
        let view = self.view;
        self.at_span.get(&span)?.iter().map(|&i| &view.constructs[i]).find(|c| c.kind == kind)
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
    /// whether or not the language makes an arm a scope. A write held to
    /// a scope (`written_in`, an arm that is a block) is already gone where
    /// the next arm begins, and needs no entry there. A path on which the variable is not yet bound
    /// reaches no read of it.
    pub fn branch_joins(&self, c: &'v Construct, bound_from: Option<Point>, written_in: Span, written: Span, writes: &[Point]) -> Vec<BranchJoin> {
        use crate::model::witnesses::JoinRole;
        let bound = |p: &Point| bound_from.is_none_or(|b| b <= *p);
        let (after, reached) = self.after_join(c, bound_from, writes);
        let mut out = vec![after];
        // An arm starts over from the entering value only past a write in an
        // arm spelled before it; a construct around no write reads the same.
        // A try it reaches joins its own arms (`try_joins`).
        for k in reached.into_iter().filter(|k| k.kind == ControlKind::Branch && contains(k.span, written)) {
            let entry = entry_point(k);
            // The first arm starts where the test leaves the variable; each
            // one written after another starts over from the entering value.
            let first = k.arms.iter().map(|a| a.span.start).min();
            let mut lands: Vec<Point> = k
                .arms
                .iter()
                .map(|a| a.span.start)
                .filter(|&s| s > entry && Some(s) != first && contains(written_in, Span { start: s, end: s }))
                .collect();
            // A test written after what it guards runs first.
            if let Some(cond) = k.cond.filter(|cond| k.arms.iter().all(|a| a.span.end <= cond.start)) {
                lands.push(cond.start);
            }
            lands.sort();
            lands.dedup();
            // Unbound on entry, an arm's read never comes from the entry.
            if !lands.is_empty() && bound(&entry) {
                out.push(BranchJoin { role: JoinRole::ArmStart, of: k.span, lands, arms: vec![read(entry)] });
            }
        }
        out
    }

    /// The joins loop `c` mints for a place bound from `bound_from`
    /// (`docs/adr/cfg-tier.md` §Loops). Its `Head`, where each test runs:
    /// the value entering the loop and the value each pass ends with (past
    /// the step when there is one, else at the body's end and at each
    /// `continue`). Its `Exit`, just past it: the value where the test
    /// fails (past a loop's `else`, which runs there) and at each `break`.
    /// A step's entry (`ArmStart`): the body's end and each `continue`.
    ///
    /// A test written before the body lands the head there, so a write in
    /// the test stands for the body; a step between the two (a C-style
    /// `for`) lands it at the body too, where that write is then hidden.
    pub fn loop_joins(&self, c: &'v Construct, bound_from: Option<Point>, writes: &[Point]) -> Vec<BranchJoin> {
        use crate::model::witnesses::JoinRole;
        let Some(parts) = self.loop_parts(c) else { return Vec::new() };
        let LoopParts { ix, body, step, heads, .. } = parts;
        let span = |a: usize| c.arms[a].span;
        let continues: Vec<JoinArm> = self.aimed(ix, LeaveKind::Continue).collect();
        let first = heads[0];
        let mut head = vec![read(before(first))];
        let mut out = Vec::new();
        match step {
            Some(s) => {
                let mut entering = continues;
                self.pass_end(c, body, &mut entering, writes);
                self.arm_end(c, s, &mut head, &mut Vec::new(), writes);
                out.push((JoinRole::ArmStart, vec![span(s).start], entering));
            }
            None => {
                self.pass_end(c, body, &mut head, writes);
                head.extend(continues);
            }
        }
        out.push((JoinRole::Head, heads, head));
        out.push((JoinRole::Exit, vec![past(c.span)], self.exit_points(c, writes)));

        let bound = |p: Point| bound_from.is_none_or(|b| b <= p);
        out.into_iter()
            .map(|(role, mut lands, mut arms)| {
                arms.retain(|a| bound(a.at));
                arms.sort_by_key(|a| (a.at, a.arm.map(|s| (s.start, s.end)), a.untaken.map(|u| (u.tested, u.of.start, u.of.end))));
                arms.dedup();
                lands.sort();
                lands.dedup();
                BranchJoin { role, of: c.span, lands, arms }
            })
            .filter(|j| !j.arms.is_empty())
            .collect()
    }

    /// Whether a variable bound at `born` is bound afresh on each pass of
    /// `c`: bound in the test or per-pass binding (`while (my $x = ...)`,
    /// `for x in xs`), at or after the head and ahead of the body. Nothing
    /// carries such a binding into the next pass, so the loop joins none.
    pub fn binds_each_pass(&self, c: &'v Construct, born: Point) -> bool {
        matches!(c.kind, ControlKind::Loop { .. })
            && self.loop_parts(c).is_some_and(|p| p.heads[0] <= born && born < c.arms[p.body].span.start)
    }

    /// A loop's arms by role, and where its head lands, first first.
    fn loop_parts(&self, c: &'v Construct) -> Option<LoopParts> {
        let ix = self.index[&(c as *const Construct)];
        let role = |r: ArmRole| c.arms.iter().position(|a| a.role == r);
        let (body, step, other) = (role(ArmRole::Body)?, role(ArmRole::Step), role(ArmRole::When(false)));
        let span = |a: usize| c.arms[a].span;
        let once = c.kind == (ControlKind::Loop { once: true });
        let mut heads = vec![span(body).start];
        match c.cond {
            Some(_) if once => {}
            Some(cond) if cond.end <= span(body).start => {
                heads = vec![cond.start];
                if step.is_some_and(|s| span(s).start < span(body).start) {
                    heads.push(span(body).start);
                }
            }
            Some(cond) => heads.push(cond.start),
            None => {}
        }
        heads.extend(other.map(|e| span(e).start));
        heads.sort();
        heads.dedup();
        Some(LoopParts { ix, body, step, other, once, heads })
    }

    /// Where a pass through loop `c`'s body comes out: the body's end (past
    /// the test, for a body that runs before it).
    fn pass_end(&self, c: &'v Construct, body: usize, out: &mut Vec<JoinArm>, writes: &[Point]) {
        match c.cond {
            Some(cond) if c.kind == (ControlKind::Loop { once: true }) => {
                let mut ended = Vec::new();
                self.arm_end(c, body, &mut ended, &mut Vec::new(), writes);
                if !ended.is_empty() {
                    out.push(read(cond.end));
                }
            }
            _ => self.arm_end(c, body, out, &mut Vec::new(), writes),
        }
    }

    /// The paths out of loop `c`: where its test fails (past its `else`,
    /// which runs there) and each `break` aimed at it.
    fn exit_points(&self, c: &'v Construct, writes: &[Point]) -> Vec<JoinArm> {
        let Some(LoopParts { ix, body, step, other, once, heads }) = self.loop_parts(c) else { return Vec::new() };
        let mut out = Vec::new();
        match (other, c.cond) {
            (Some(e), _) => self.arm_end(c, e, &mut out, &mut Vec::new(), writes),
            (None, Some(_)) if once => self.pass_end(c, body, &mut out, writes),
            // Just past the test: what a test holding proves lasts to its
            // end (`!defined($b) and $n < 9` holds `$b` undef over its
            // right side), and here it failed. A step written right after
            // the test (`$i<$n;$i++`) starts there and lands its own join,
            // which the failed test never reaches, so the read stays short.
            // The test failing is no arm of the loop: what it proves there
            // is the loop's untaken path, as for an `if` with no `else`.
            (None, Some(cond)) => {
                let at = past(cond);
                let stepped = step.is_some_and(|s| c.arms[s].span.start <= at);
                let at = if stepped { cond.end } else { at };
                out.push(JoinArm { untaken: Some(Untaken { of: c.span, tested: cond.end }), ..read(at) });
            }
            (None, None) => out.push(read(heads[0])),
        }
        out.extend(self.aimed(ix, LeaveKind::Break));
        out
    }

    /// Each `kind` jump aimed at loop `ix`, as the place stands there.
    fn aimed(&self, ix: usize, kind: LeaveKind) -> impl Iterator<Item = JoinArm> + '_ {
        self.view.leaves.iter().zip(&self.aims).filter(move |(l, aim)| **aim == Aim::Loop(ix) && l.kind == kind).map(|(l, _)| read(l.span.start))
    }

    /// The join past `c` for a place bound from `bound_from`, and every
    /// branch whose paths are `c`'s.
    fn after_join(&self, c: &'v Construct, bound_from: Option<Point>, writes: &[Point]) -> (BranchJoin, Vec<&'v Construct>) {
        use crate::model::witnesses::JoinRole;
        let mut arms = Vec::new();
        let mut reached = vec![c];
        match c.kind {
            ControlKind::Loop { .. } => arms = self.exit_points(c, writes),
            _ => self.arm_points(c, &mut arms, &mut reached, writes),
        }
        arms.retain(|a| bound_from.is_none_or(|b| b <= a.at));
        arms.sort_by_key(|a| (a.at, a.arm.map(|s| (s.start, s.end)), a.untaken.map(|u| (u.tested, u.of.start, u.of.end))));
        arms.dedup();
        (BranchJoin { role: JoinRole::After, of: c.span, lands: vec![past(c.span)], arms }, reached)
    }

    /// The joins a test of a place makes, from the branch at `of` (or the
    /// one it is lifted into) out through each branch around it inside
    /// `within`: one is made only where some path out of its construct is
    /// gone and every path that comes out carries a proof about the place.
    /// A path proves it when no arm runs on it and the test proves something
    /// there (`untaken_proved`), when a test narrows the place over the code
    /// it runs (`narrowed`: its arm, or for a path no arm runs, its
    /// construct, which an arm narrowed around it answers too), or when its
    /// arm holds a construct already joined.
    pub fn guard_joins(
        &self,
        of: Span,
        asserts: bool,
        within: Span,
        bound_from: Option<Point>,
        untaken_proved: impl Fn(Span) -> bool,
        narrowed: impl Fn(Span) -> bool,
    ) -> Vec<BranchJoin> {
        use crate::model::witnesses::JoinRole;
        // A statement that asserts leaves where its test fails and comes out
        // of itself only where it held: one way on, which no arm runs.
        let (start, first, own, left) = if asserts {
            if !untaken_proved(of) {
                return Vec::new();
            }
            let on = JoinArm { untaken: Some(Untaken { of, tested: of.end }), ..read(of.start) };
            (of, Some(BranchJoin { role: JoinRole::After, of, lands: vec![past(of)], arms: vec![on] }), None, true)
        } else {
            // A loop's test holding never leads out of it: only failing does.
            let looped = self.construct_at(of, ControlKind::Loop { once: false });
            let Some(found) = self.construct_at(of, ControlKind::Branch).or(looped) else { return Vec::new() };
            let c = self.lifted_into(found);
            if !(looped.is_some() || self.drops_a_path(c)) {
                return Vec::new();
            }
            (c.span, None, Some(c), looped.is_some() || self.drops_by(c, false))
        };
        if !contains(within, start) {
            return Vec::new();
        }
        let outer = self
            .arms_holding(start.start)
            .filter(|(k, _)| contains(k.span, start) && k.span != start)
            .take_while(|(_, a)| contains(within, a.span))
            // Past a loop the place holds what its exit joins, which no
            // test inside it decides.
            .take_while(|(k, _)| k.kind == ControlKind::Branch)
            .filter(|(k, _)| !self.is_lifted(k) && contains(within, k.span))
            .map(|(k, _)| k);
        let mut out: Vec<BranchJoin> = first.into_iter().collect();
        for k in own.into_iter().chain(outer) {
            let (mut after, _) = self.after_join(k, bound_from, &[]);
            let proved = |a: &JoinArm| match (a.untaken, a.arm) {
                (Some(u), _) => untaken_proved(u.of) || narrowed(u.of),
                (None, Some(arm)) => narrowed(arm) || out.iter().any(|inner| contains(arm, inner.of)),
                (None, None) => false,
            };
            // A path past a gate may still not come back. Where it does, it
            // speaks for nothing (`guard_region_witnesses`) if it proves
            // nothing, or if no path leaves but past a gate: then the test
            // drops no path and the value coming in stands.
            for a in &mut after.arms {
                a.mute = !a.gates.is_empty() && (!left || !proved(a));
            }
            if after.arms.is_empty() || !after.arms.iter().all(|a| a.mute || proved(a)) {
                break;
            }
            out.push(after);
        }
        out
    }

    /// Where each path through `c` comes out of it: the end of every arm
    /// control comes out of (an arm a branch closes contributes that
    /// branch's paths), and, when no arm runs where the test goes one way,
    /// where the construct is entered. Every branch it passes through lands
    /// in `reached`.
    fn arm_points(&self, c: &'v Construct, out: &mut Vec<JoinArm>, reached: &mut Vec<&'v Construct>, writes: &[Point]) {
        if let ControlKind::Try { .. } = c.kind {
            return self.try_points(c, out, reached, writes);
        }
        for ai in 0..c.arms.len() {
            self.arm_end(c, ai, out, reached, writes);
        }
        let has = |b: bool| c.arms.iter().any(|a: &Arm| a.role == ArmRole::When(b));
        if !(has(true) && has(false)) && !c.arms.is_empty() {
            let tested = c.cond.map_or(entry_point(c), |cond| cond.end);
            out.push(JoinArm { at: entry_point(c), arm: None, untaken: Some(Untaken { of: c.span, tested }), gates: Vec::new(), mute: false });
        }
    }

    /// Where control comes out at the end of arm `ai` of `c`: the paths out
    /// of a branch or loop closing it, else the arm's end, unless control
    /// never comes out of it.
    fn arm_end(&self, c: &'v Construct, ai: usize, out: &mut Vec<JoinArm>, reached: &mut Vec<&'v Construct>, writes: &[Point]) {
        let a = &c.arms[ai];
        let k = self.index[&(c as *const Construct)];
        let ix = self.arm_ix(k, ai);
        let from = out.len();
        match self.closing(c, a).filter(|k| reached.len() < 64 && !reached.iter().any(|r| std::ptr::eq(*r, *k))) {
            Some(inner) if matches!(inner.kind, ControlKind::Branch | ControlKind::Try { .. }) => {
                reached.push(inner);
                self.arm_points(inner, out, reached, writes);
            }
            // Its paths out are paths through `a`, so what holds over `a` holds there.
            Some(inner) => out.extend(self.exit_points(inner, writes).into_iter().map(|p| JoinArm { arm: Some(a.span), ..p })),
            None if self.leaves[ix] => {}
            None => out.push(JoinArm { at: a.span.end, arm: Some(a.span), untaken: None, gates: Vec::new(), mute: false }),
        }
        // Every path out of the arm runs past the calls the arm itself makes.
        for p in &mut out[from..] {
            p.gates.extend(self.gates[ix].iter().copied());
        }
    }

    /// Where each path through try `c` comes out of it: past its `finally`
    /// where it has one, else the end of its body (or of what runs where
    /// the body completes) and of each handler, and for a quiet try,
    /// wherever its body may unwind ([`Self::unwound`]). Only the paths
    /// that complete go on past a `finally`: one that unwinds runs it and
    /// goes on unwinding.
    fn try_points(&self, c: &'v Construct, out: &mut Vec<JoinArm>, reached: &mut Vec<&'v Construct>, writes: &[Point]) {
        let mut completions = Vec::new();
        self.try_completions(c, &mut completions, reached, writes);
        match c.arms.iter().position(|a| a.role == ArmRole::Finally) {
            Some(f) if writes.iter().any(|&w| contains(c.arms[f].span, at_point(w))) => {
                let fin = c.arms[f].span;
                let mut through = Vec::new();
                self.arm_end(c, f, &mut through, reached, writes);
                for p in through {
                    self.through_finally(fin, p, &completions, writes, out, 0);
                }
            }
            // One that writes nothing hands each path on as it came in.
            _ => out.extend(completions),
        }
    }

    /// The paths that complete, as they stand at `p`, a point a path
    /// through `finally` arm `fin` reaches: `p` itself where a write of
    /// the `finally` stands on every path to it, the completing paths
    /// where none does on any, and otherwise the paths out of the latest
    /// construct before `p` that writes, each taken the same way. Reading
    /// `p` itself would join the paths that unwind into the `finally` too.
    fn through_finally(&self, fin: Span, p: JoinArm, completions: &[JoinArm], writes: &[Point], out: &mut Vec<JoinArm>, depth: usize) {
        let gated = |a: &JoinArm| JoinArm { gates: a.gates.iter().chain(&p.gates).copied().collect(), ..a.clone() };
        // The innermost arm inside the `finally` around `p`: what runs
        // before `p` on its path is in it, or before its construct.
        let around = self.arms_holding(p.at).filter(|(_, a)| contains(fin, a.span) && a.span != fin).last();
        let region = around.map_or(fin, |(_, a)| a.span);
        let prior: Vec<Point> = writes.iter().copied().filter(|&w| contains(region, at_point(w)) && w <= p.at).collect();
        let level = |w: Point| self.arms_holding(w).filter(|(_, a)| contains(region, a.span) && a.span != region).next().is_none();
        if depth > 32 || prior.iter().any(|&w| level(w)) {
            return out.push(p);
        }
        let latest = self
            .view
            .constructs
            .iter()
            .filter(|k| contains(region, k.span) && k.span != region && k.span.end <= p.at)
            .filter(|k| prior.iter().any(|&w| contains(k.span, at_point(w))))
            .max_by_key(|k| (k.span.end, std::cmp::Reverse(k.span.start)));
        match (latest, around) {
            (Some(k), _) => {
                let (after, _) = self.after_join(k, None, writes);
                for a in after.arms {
                    self.through_finally(fin, gated(&a), completions, writes, out, depth + 1);
                }
            }
            // Nothing before `p` writes: it stands as the `finally` began.
            (None, None) if prior.is_empty() => out.extend(completions.iter().map(gated)),
            // … or as the branch around it began.
            (None, Some((k, _))) if prior.is_empty() && k.kind == ControlKind::Branch => {
                self.through_finally(fin, JoinArm { at: entry_point(k), ..p.clone() }, completions, writes, out, depth + 1)
            }
            // A loop's arm is entered from its own end as well.
            // TODO(finally-loop-paths): read here, the point joins the
            // unwinding entries too; exact is the loop's entry taken back the
            // same way, joined with its body's writes. Owner: 0.11 CFG
            // follow-up.
            _ => out.push(p),
        }
    }

    /// The paths into what follows try `c`'s body and handlers: its
    /// `finally`, or past it.
    fn try_completions(&self, c: &'v Construct, out: &mut Vec<JoinArm>, reached: &mut Vec<&'v Construct>, writes: &[Point]) {
        let k = self.index[&(c as *const Construct)];
        let role = |r: ArmRole| c.arms.iter().position(|a| a.role == r);
        if let Some(t) = role(ArmRole::Tried) {
            match role(ArmRole::Settled) {
                // What runs where the body completes is reached only so.
                Some(s) if !self.leaves[self.arm_ix(k, t)] => self.arm_end(c, s, out, reached, writes),
                Some(_) => {}
                None => self.arm_end(c, t, out, reached, writes),
            }
        }
        for h in (0..c.arms.len()).filter(|&a| c.arms[a].role == ArmRole::Handler) {
            self.arm_end(c, h, out, reached, writes);
        }
        if c.kind == (ControlKind::Try { quiet: true }) {
            out.extend(self.unwound(c, &[ArmRole::Tried], writes));
        }
    }

    /// The place as it may stand where try `c` unwinds out of its arms of
    /// `roles`: as it entered, or past a write in them that a raise
    /// follows ([`ControlView::raises`]).
    fn unwound(&self, c: &'v Construct, roles: &[ArmRole], writes: &[Point]) -> Vec<JoinArm> {
        let spans: Vec<Span> = c.arms.iter().filter(|a| roles.contains(&a.role)).map(|a| a.span).collect();
        std::iter::once(read(entry_point(c)))
            .chain(
                writes
                    .iter()
                    .filter(|&&w| spans.iter().any(|&s| contains(s, at_point(w)) && self.raised_after(s, w)))
                    .map(|&w| read(w)),
            )
            .collect()
    }

    /// Whether arm `arm` may unwind after its write at `w`: a raise in it
    /// ends past `w`, or sits in a loop around `w` inside it, which runs
    /// again. A raise inside the write's own right side ends before it.
    fn raised_after(&self, arm: Span, w: Point) -> bool {
        let rerun = self
            .view
            .constructs
            .iter()
            .filter(|k| matches!(k.kind, ControlKind::Loop { .. }) && contains(arm, k.span) && contains(k.span, at_point(w)))
            .map(|k| k.span.start)
            .min();
        let from = rerun.unwrap_or(w);
        self.view.raises.iter().any(|&r| contains(arm, r) && r.end > from)
    }

    /// Whether try `c` catches what its body throws.
    fn catches(c: &Construct) -> bool {
        c.kind == (ControlKind::Try { quiet: true }) || c.arms.iter().any(|a| a.role == ArmRole::Handler)
    }

    /// The joins try `c` mints for a place bound from `bound_from`, written
    /// at `writes` (`docs/adr/cfg-tier.md` §Try): past it (`After`); where
    /// each handler begins, the place as the body may have unwound it;
    /// where what runs on completion begins, the body's end; and where its
    /// `finally` begins, every path into it, unwinding ones included.
    pub fn try_joins(&self, c: &'v Construct, bound_from: Option<Point>, writes: &[Point]) -> Vec<BranchJoin> {
        use crate::model::witnesses::JoinRole;
        let bound = |a: &JoinArm| bound_from.is_none_or(|b| b <= a.at);
        let arm = |r: ArmRole| c.arms.iter().find(|a| a.role == r);
        let mut out = vec![self.after_join(c, bound_from, writes).0];
        let mut push = |of: Span, lands: Vec<Point>, mut arms: Vec<JoinArm>| {
            arms.retain(bound);
            arms.sort_by_key(|a| (a.at, a.arm.map(|s| (s.start, s.end))));
            arms.dedup();
            if !lands.is_empty() && !arms.is_empty() {
                out.push(BranchJoin { role: JoinRole::ArmStart, of, lands, arms });
            }
        };
        let handlers: Vec<Point> = c.arms.iter().filter(|a| a.role == ArmRole::Handler).map(|a| a.span.start).collect();
        if let Some(t) = arm(ArmRole::Tried) {
            push(t.span, handlers, self.unwound(c, &[ArmRole::Tried], writes));
        }
        if let (Some(t), Some(s)) = (c.arms.iter().position(|a| a.role == ArmRole::Tried), arm(ArmRole::Settled)) {
            let mut ended = Vec::new();
            self.arm_end(c, t, &mut ended, &mut Vec::new(), writes);
            push(s.span, vec![s.span.start], ended);
        }
        if let Some(f) = arm(ArmRole::Finally) {
            let mut into = Vec::new();
            self.try_completions(c, &mut into, &mut Vec::new(), writes);
            into.extend(self.unwound(c, &[ArmRole::Tried, ArmRole::Handler, ArmRole::Settled], writes));
            push(f.span, vec![f.span.start], into);
        }
        out
    }

    /// The outermost branch or loop inside arm `a` of `c` that ends where
    /// `a` does: its paths are the arm's, since nothing in the arm runs
    /// after it.
    fn closing(&self, c: &Construct, a: &Arm) -> Option<&'v Construct> {
        self.ends
            .get(&a.tail)?
            .iter()
            .map(|&i| &self.view.constructs[i])
            .filter(|k| !std::ptr::eq(*k, c) && k.span != c.span && contains(a.span, k.span))
            .min_by_key(|k| k.span.start)
    }
}

/// Where a place's joins land and stop: the scopes from the site out, the
/// region the joins stay inside (the binding's scope, or the callable the
/// site runs in, whichever is nearer), and whether the binding is inside it.
pub struct Reach {
    pub chain: Vec<super::ScopeId>,
    pub stop: super::ScopeId,
    pub within: Span,
    pub home_inside: bool,
    pub callable: super::ScopeId,
}

impl Reach {
    /// The reach of a site in `sid` for a binding living in `home`.
    pub fn of(scopes: &[super::Scope], sid: super::ScopeId, home: super::ScopeId) -> Reach {
        let chain = super::scope_chain_of(scopes, sid);
        let pos = |id: super::ScopeId| chain.iter().position(|&s| s == id);
        let callable = chain.iter().copied().find(|&s| scopes[s.0 as usize].kind.is_callable()).unwrap_or(*chain.last().unwrap_or(&sid));
        let home_inside = pos(home).is_some_and(|h| pos(callable).is_some_and(|c| h <= c));
        let stop = if home_inside { home } else { callable };
        let within = scopes[stop.0 as usize].span;
        Reach { chain, stop, within, home_inside, callable }
    }

    /// The innermost scope of the reach holding `span`.
    pub fn holding_scope(&self, scopes: &[super::Scope], span: Span) -> super::ScopeId {
        self.chain.iter().copied().find(|&s| contains(scopes[s.0 as usize].span, span)).unwrap_or(self.stop)
    }

    /// The scope a construct's joins are held to: the innermost of the
    /// reach holding the construct and the furthest point its joins land
    /// at, short of the reach's own end (a construct closing its scope lands
    /// just past it).
    pub fn joins_scope(&self, scopes: &[super::Scope], of: Span, joins: &[BranchJoin]) -> super::ScopeId {
        let furthest = joins.iter().flat_map(|j| j.lands.iter().copied()).fold(of.end, Point::max);
        let held = |s: super::ScopeId| contains(scopes[s.0 as usize].span, of);
        let reaches = |s: super::ScopeId| scopes[s.0 as usize].span.end >= furthest || s == self.stop;
        self.chain.iter().copied().find(|&s| held(s) && reaches(s)).unwrap_or(self.stop)
    }
}

/// A write to a lexical variable, where it lands.
#[derive(Debug, Clone, Copy)]
pub struct BoundWrite {
    pub sym: super::SymbolId,
    pub at: Point,
    /// The scope the write is made in.
    pub scope: super::ScopeId,
    /// A plain assignment to a variable already bound, not its declaration.
    pub reassigns: bool,
    /// The expression whose whole value the write stores, where it stores
    /// one.
    pub value: Option<Span>,
}

/// Each later-running body by the callable scope it is the body of: the
/// narrowest callable holding it.
fn bodies_by_scope(scopes: &[super::Scope], bodies: &[CallableBody]) -> HashMap<super::ScopeId, CallableBody> {
    let mut out = HashMap::new();
    for b in bodies {
        let callable = scopes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind.is_callable() && contains(s.span, b.span))
            .min_by_key(|(_, s)| (std::cmp::Reverse((s.span.start.row, s.span.start.column)), (s.span.end.row, s.span.end.column)));
        if let Some((i, _)) = callable {
            out.insert(super::ScopeId(i as u32), *b);
        }
    }
    out
}

/// The later-running bodies `from` sits in that run apart from where
/// `sym` is bound, innermost first: the callables between `from` and the
/// variable's own scope. A closed body holds what it captures by value as
/// it stood at its making and writes none of it back, so the climb stops at
/// one; a capture by reference is the variable itself, so it climbs on.
fn bodies_around(
    scopes: &[super::Scope],
    sym: &super::Symbol,
    from: super::ScopeId,
    bodies: &HashMap<super::ScopeId, CallableBody>,
) -> Vec<(super::ScopeId, CallableBody)> {
    let mut out = Vec::new();
    for s in super::scope_chain_of(scopes, from) {
        let scope = &scopes[s.0 as usize];
        let by_reference = scope.captures.iter().any(|c| c.name == sym.name && c.by == super::CaptureBy::Reference);
        if s == sym.scope || contains(scope.span, sym.span) || (scope.closed && !by_reference) {
            break;
        }
        if let Some(b) = bodies.get(&s) {
            out.push((s, *b));
        }
    }
    out
}

/// What later-running bodies make of the variables of the code around them
/// (`docs/adr/cfg-tier.md` §Later-running bodies).
///
/// A body reads such a variable as it may stand when it runs: as it stood
/// where the body was made, or as any write from then on leaves it, inside
/// the body or past it (every write, for a body that exists before its
/// text). Where such a write exists the body opens on an `Entry` join of
/// those paths; where none does, nothing can change the variable before the
/// body runs, so the read sees it as made, narrowing included, and nothing
/// is minted.
///
/// A body's write reaches the variable wherever the body is called, which
/// is anywhere from its making on (from the variable's binding, for a body
/// that exists before its text). The variable gets one `MayBe` of a `Writes`
/// join of the body's writes there.
///
/// TODO(closure-call-writes): the write reaches the variable only where the
/// body is called; that needs to know which calls can run it. Owner: the
/// whole-program analysis epic.
pub fn body_joins(
    scopes: &[super::Scope],
    symbols: &[super::Symbol],
    bodies: &[CallableBody],
    refs: &[super::Ref],
    writes: &[BoundWrite],
) -> Vec<crate::model::witnesses::Witness> {
    use super::{Binding, Place, RefKind};
    use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
    if bodies.is_empty() || writes.is_empty() {
        return Vec::new();
    }
    let body_of = bodies_by_scope(scopes, bodies);
    let mut written: HashMap<super::SymbolId, Vec<&BoundWrite>> = HashMap::new();
    for w in writes {
        written.entry(w.sym).or_default().push(w);
    }
    let source = || WitnessSource::Builder(JOIN_SOURCE.into());
    let read_of = |sid| WitnessAttachment::Place(Box::new(Place::variable(Binding::Lexical(sid))));
    // A path through a write is the value it stores, which a write made
    // inside a later-running body holds only inside it (`confined_writes`);
    // through anything else it is the variable as it stands there.
    let arm = |join: &WitnessAttachment, sid, at: Point, value: Option<Span>| Witness {
        attachment: join.clone(),
        source: source(),
        payload: WitnessPayload::Edge(value.map_or_else(|| read_of(sid), WitnessAttachment::Expr)),
        span: Span { start: at, end: at },
    };
    let mut out = Vec::new();

    // Reads: every body a read sits in that some write can reach first.
    let mut seen: HashSet<(super::SymbolId, super::ScopeId)> = HashSet::new();
    for r in refs {
        if !matches!(r.kind, RefKind::Variable) || !matches!(r.access, super::AccessKind::Read) {
            continue;
        }
        let Some(sid) = r.resolved_symbol() else { continue };
        let Some(ws) = written.get(&sid) else { continue };
        let sym = &symbols[sid.0 as usize];
        for (s, body) in bodies_around(scopes, sym, r.scope, &body_of) {
            let made = scopes[s.0 as usize].span;
            // The body opens past where it is made, so the path read at
            // its making never reads the join it feeds.
            if body.span.start <= made.start || !seen.insert((sid, s)) {
                continue;
            }
            let from = body.made_before.unwrap_or(made.start);
            let reaches = |w: &&&BoundWrite| body.hoisted || w.at >= from;
            if !ws.iter().filter(reaches).any(|w| w.reassigns || w.at >= from) {
                continue;
            }
            let join = WitnessAttachment::Join { place: Box::new(Place::variable(Binding::Lexical(sid))), of: body.span, role: JoinRole::Entry };
            for payload in [WitnessPayload::Reset, WitnessPayload::Edge(join.clone())] {
                out.push(Witness { attachment: read_of(sid), source: source(), payload, span: Span { start: body.span.start, end: made.end } });
            }
            out.push(arm(&join, sid, from, None));
            out.extend(ws.iter().filter(reaches).map(|w| arm(&join, sid, w.at, w.value)));
        }
    }

    // Writes: every body a write sits in, joined in past where it is made.
    let mut by_body: HashMap<(super::SymbolId, super::ScopeId), Vec<&BoundWrite>> = HashMap::new();
    let mut order: Vec<(super::SymbolId, super::ScopeId)> = Vec::new();
    for w in writes {
        let sym = &symbols[w.sym.0 as usize];
        for (s, _) in bodies_around(scopes, sym, w.scope, &body_of) {
            let ats = by_body.entry((w.sym, s)).or_insert_with(|| {
                order.push((w.sym, s));
                Vec::new()
            });
            ats.push(w);
        }
    }
    for key in order {
        let (sid, s) = key;
        let body = body_of[&s];
        let join = WitnessAttachment::Join { place: Box::new(Place::variable(Binding::Lexical(sid))), of: body.span, role: JoinRole::Writes };
        let from = match body.hoisted {
            true => symbols[sid.0 as usize].span.start,
            false => scopes[s.0 as usize].span.end,
        };
        out.push(Witness { attachment: read_of(sid), source: source(), payload: WitnessPayload::MayBe(join.clone()), span: Span { start: from, end: from } });
        out.extend(by_body[&key].iter().map(|w| arm(&join, sid, w.at, w.value)));
    }
    out
}

/// Where each write made inside a later-running body stops holding: the
/// end of the innermost such body it sits in, past which the variable
/// reads it through that body's `MayBe`; `None` where the body ends on the
/// write. Keyed by the written place and the write's site, for
/// `WitnessBag::confine_writes`.
pub fn confined_writes(
    scopes: &[super::Scope],
    symbols: &[super::Symbol],
    bodies: &[CallableBody],
    writes: &[BoundWrite],
) -> HashMap<(crate::model::witnesses::WitnessAttachment, Point), Option<Point>> {
    use super::{Binding, Place};
    let body_of = bodies_by_scope(scopes, bodies);
    let mut out = HashMap::new();
    for w in writes {
        let sym = &symbols[w.sym.0 as usize];
        let Some((s, _)) = bodies_around(scopes, sym, w.scope, &body_of).into_iter().next() else { continue };
        let region = super::cut_region(Span { start: w.at, end: scopes[s.0 as usize].span.end }, []);
        let place = crate::model::witnesses::WitnessAttachment::Place(Box::new(Place::variable(Binding::Lexical(w.sym))));
        out.insert((place, w.at), region.map(|r| r.end));
    }
    out
}

/// What each test proves of a place on its construct's untaken path, by
/// construct.
pub type Proofs<'p> = HashMap<Span, Vec<(&'p super::Place, &'p crate::model::witnesses::WitnessPayload)>>;

/// The arms of `join`: the place as it stands where each path comes out,
/// or, on a path no arm runs, what the test there proves of it.
pub fn join_arms(
    join: &crate::model::witnesses::WitnessAttachment,
    place: &super::Place,
    arms: &[JoinArm],
    proofs: &Proofs<'_>,
) -> Vec<crate::model::witnesses::Witness> {
    use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
    let read = WitnessAttachment::Place(Box::new(place.clone()));
    let arm = |payload, at| Witness {
        attachment: join.clone(),
        source: WitnessSource::Builder(JOIN_SOURCE.into()),
        payload,
        span: Span { start: at, end: at },
    };
    arms.iter()
        .map(|a| {
            let proved = a.untaken.and_then(|Untaken { of, tested }| {
                let (_, payload) = proofs.get(&of)?.iter().find(|(p, _)| *p == place)?;
                Some(((*payload).clone(), tested))
            });
            let w = match proved {
                Some((payload, tested)) => arm(payload, tested),
                None => arm(WitnessPayload::Edge(read.clone()), a.at),
            };
            gated(w, &a.gates, Some)
        })
        .collect()
}

/// `w`, holding only where control comes back from every call in `gates`;
/// `inner` decides what it holds when it does.
fn gated(
    w: crate::model::witnesses::Witness,
    gates: &[Span],
    inner: impl FnOnce(crate::model::witnesses::WitnessPayload) -> Option<crate::model::witnesses::WitnessPayload>,
) -> crate::model::witnesses::Witness {
    use crate::model::witnesses::{Witness, WitnessPayload};
    if gates.is_empty() {
        return w;
    }
    let Witness { attachment, source, payload, span } = w;
    let arm = inner(payload).map(Box::new);
    let gates = gates.iter().map(|&g| crate::model::witnesses::WitnessAttachment::Expr(g)).collect();
    Witness { attachment, source, payload: WitnessPayload::Gated { gates, arm }, span }
}

/// A region a guard join holds over, from where it lands to `end`, as
/// `cut` leaves it; `None` where nothing is left.
pub fn guard_region_witnesses(
    place: &super::Place,
    join: BranchJoin,
    end: Point,
    proofs: &Proofs<'_>,
    cut: impl Fn(Span) -> Option<Span>,
) -> Vec<crate::model::witnesses::Witness> {
    use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
    let BranchJoin { role, of, lands, arms } = join;
    let attachment = WitnessAttachment::Join { place: Box::new(place.clone()), of, role };
    // Every path out carries a proof, so the region is one too: under a
    // declared bound it reads by the guard rule, not as a write.
    let mut out: Vec<Witness> = lands
        .into_iter()
        .filter_map(|at| cut(Span { start: at, end }))
        .flat_map(|region| {
            [WitnessPayload::Edge(attachment.clone()), WitnessPayload::Proof].map(|payload| Witness {
                attachment: WitnessAttachment::Place(Box::new(place.clone())),
                source: WitnessSource::Builder(JOIN_SOURCE.into()),
                payload,
                span: region,
            })
        })
        .collect();
    if !out.is_empty() {
        // A path past a gate that comes back is one the test proves nothing
        // on, so where it runs the region answers nothing and the value
        // coming in stands, as it does where no path leaves.
        let gated_arms: Vec<_> = arms.iter().map(|a| (a.gates.clone(), a.mute)).collect();
        out.extend(
            join_arms(&attachment, place, &arms.iter().cloned().map(|a| JoinArm { gates: Vec::new(), ..a }).collect::<Vec<_>>(), proofs)
                .into_iter()
                .zip(gated_arms)
                .map(|(w, (gates, mute))| gated(w, &gates, |inner| (!mute).then_some(inner))),
        );
    }
    out
}

/// What a pack's tests prove, by place, for the joins they make
/// ([`FileAnalysis::assemble_joins`](super::FileAnalysis::assemble_joins)).
#[derive(Debug, Default)]
pub struct GuardFacts {
    /// Each test's construct, the place it tests, and the scope it sits in.
    pub seeds: Vec<(Span, super::Place, super::ScopeId)>,
    /// What a test proves of its place where no arm of its construct runs.
    pub untaken: Vec<(Span, super::Place, crate::model::witnesses::WitnessPayload)>,
    /// The arms a test narrows its place over.
    pub narrowed: Vec<(super::Place, Span)>,
    /// How far past each construct a test's join can hold: the end of the
    /// statements it runs among.
    pub bounds: HashMap<Span, Point>,
}

impl super::FileAnalysis {
    /// For every write a branch holds, the joins of that branch, from the
    /// write out to the variable's binding (or the callable the write runs
    /// in); for every test that proves something of a place, the region
    /// joins it makes past its construct (`JoinPlan::guard_joins`), each cut
    /// where the place is rebound or, for a member place, disturbed
    /// (`place_cut`). For an analysis whose variable facts are bound
    /// (`bind_variable_witnesses`).
    pub(crate) fn assemble_joins(
        &mut self,
        view: &ControlView,
        guards: GuardFacts,
        place_cut: Option<&super::PlaceDisturbanceIndex>,
        stores: Vec<MemberStore>,
    ) {
        use super::{Binding, Place};
        use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
        let mut view = std::borrow::Cow::Borrowed(view);
        // A call gates: its callee's own file says how it ends, read on the
        // effect lane at query time.
        let calls = |g: &Span| {
            self.witnesses
                .for_attachment(&WitnessAttachment::Expr(*g))
                .iter()
                .any(|w| matches!(&w.payload, WitnessPayload::Invoke { .. }))
        };
        if !view.gates.iter().all(calls) {
            view.to_mut().gates.retain(calls);
        }
        let plan = JoinPlan::new(&view);
        let effect_arms: Vec<Witness> = view
            .bodies
            .iter()
            .filter(|b| b.readable)
            .filter_map(|b| Some((self.callable_of_body(b.span)?, b.span)))
            .flat_map(|(sid, body)| plan.effect_arms(sid, body))
            .collect();
        // How a member ends rides the class's member, as its return does,
        // so a call that dispatches on a receiver finds it.
        let mut mirrors: Vec<Witness> = Vec::new();
        for w in &effect_arms {
            let WitnessAttachment::SymbolEffects(sid) = w.attachment else { continue };
            let sym = &self.symbols[sid.0 as usize];
            let Some(class) = sym.package.clone() else { continue };
            let att = WitnessAttachment::effects_of(class, sym.name.clone());
            if mirrors.iter().any(|m| m.attachment == att) || !self.witnesses.for_attachment(&att).is_empty() {
                continue;
            }
            mirrors.push(Witness {
                attachment: att,
                source: WitnessSource::Builder("effect_arm".into()),
                payload: WitnessPayload::Edge(WitnessAttachment::SymbolEffects(sid)),
                span: sym.span,
            });
        }
        for w in effect_arms.into_iter().chain(mirrors) {
            self.witnesses.push(w);
        }
        let writes: Vec<BoundWrite> = self
            .flow_edges
            .iter()
            .filter_map(|fe| match self.variable_binding_of(&fe.target_name, fe.target_at, fe.target_scope).0 {
                Binding::Lexical(sym) => Some(BoundWrite { sym, at: fe.target_at, scope: fe.target_scope, reassigns: fe.reassigns, value: fe.stored_value() }),
                _ => None,
            })
            .collect();
        // The Perl builder makes these regions as it lowers them.
        self.witnesses.confine_writes(&confined_writes(&self.scopes, self.symbols(), &view.bodies, &writes));
        for w in body_joins(&self.scopes, self.symbols(), &view.bodies, self.refs(), &writes) {
            self.witnesses.push(w);
        }
        let (states, stores) = self.member_states(stores, &view.returns);
        for w in states {
            self.witnesses.push(w);
        }
        if view.constructs.is_empty() || (self.flow_edges.is_empty() && guards.seeds.is_empty() && stores.is_empty()) {
            return;
        }
        let mut minted: HashSet<(Place, Span, JoinRole)> = HashSet::new();
        let mut joins: Vec<Witness> = Vec::new();
        let source = || WitnessSource::Builder(JOIN_SOURCE.into());
        let lands_at = |att: &WitnessAttachment, join: &WitnessAttachment, lands: &[Point], out: &mut Vec<Witness>| {
            for &p in lands {
                for payload in [WitnessPayload::Reset, WitnessPayload::Edge(join.clone())] {
                    out.push(Witness { attachment: att.clone(), source: source(), payload, span: Span { start: p, end: p } });
                }
            }
        };
        let bound: Vec<(Place, super::ScopeId)> = self
            .flow_edges
            .iter()
            .map(|fe| {
                let (binding, home) = self.variable_binding_of(&fe.target_name, fe.target_at, fe.target_scope);
                (Place::variable(binding), home)
            })
            .collect();
        // Where each place is written: the points a try's body may unwind
        // from.
        let mut writes_of: HashMap<&Place, Vec<Point>> = HashMap::new();
        for ((place, _), fe) in bound.iter().zip(&self.flow_edges) {
            writes_of.entry(place).or_default().push(fe.target_at);
        }
        for (store, _) in &stores {
            writes_of.entry(&store.place).or_default().push(store.at);
        }
        for ((place, home), fe) in bound.iter().zip(&self.flow_edges) {
            let at = fe.target_at;
            let bound_from = match place.root {
                Binding::Lexical(sid) => Some(self.symbols[sid.0 as usize].span.start),
                _ => None,
            };
            // An arm ending where the write lands (`x = a if c else b`'s
            // `b`) is part of the value written, not around the write.
            let written = fe.target_token.unwrap_or(Span { start: fe.source.start, end: at });
            let site = WriteSite { place, at, scope: fe.target_scope, home: *home, bound_from, written, writes: &writes_of[place] };
            for (scope, join, lands, arms) in self.write_joins(&plan, &site, &mut minted) {
                let att = WitnessAttachment::Variable { name: fe.target_name.clone(), scope };
                lands_at(&att, &join, &lands, &mut joins);
                joins.extend(arms);
            }
        }
        // A member's writes join it as a variable's do; its place is its
        // own attachment, with no name to bind.
        let mut member_joins: Vec<Witness> = Vec::new();
        for (store, home) in &stores {
            let site = WriteSite {
                place: &store.place,
                at: store.at,
                scope: self.scope_at(store.at).unwrap_or(*home),
                home: *home,
                bound_from: None,
                written: store.token,
                writes: &writes_of[&store.place],
            };
            for (_, join, lands, arms) in self.write_joins(&plan, &site, &mut minted) {
                lands_at(&WitnessAttachment::Place(Box::new(store.place.clone())), &join, &lands, &mut member_joins);
                member_joins.extend(arms);
            }
        }

        // A test changes nothing a place holds, so its joins hold as
        // regions, and where one answers nothing the value coming in stands.
        let mut proofs: Proofs<'_> = HashMap::new();
        for (of, place, payload) in &guards.untaken {
            proofs.entry(*of).or_default().push((place, payload));
        }
        let rebinds = self.rebind_index();
        let mut regions: Vec<Witness> = Vec::new();
        let mut seen: HashSet<(Span, &Place)> = HashSet::new();
        for (of, place, scope) in &guards.seeds {
            if !seen.insert((*of, place)) {
                continue;
            }
            let (home, bound_from) = match &place.root {
                Binding::Lexical(sym) => (self.symbols[sym.0 as usize].scope, Some(self.symbols[sym.0 as usize].span.start)),
                Binding::Receiver(sym) => (self.symbols[sym.0 as usize].scope, None),
                Binding::Arguments(callable) => (*callable, None),
                Binding::Package(..) => (super::outermost_scope(&self.scopes, *scope), None),
            };
            let reach = Reach::of(&self.scopes, *scope, home);
            let found = plan.guard_joins(
                *of,
                false,
                reach.within,
                bound_from,
                |of| proofs.get(&of).is_some_and(|ps| ps.iter().any(|(p, _)| *p == place)),
                |arm| guards.narrowed.iter().any(|(p, over)| p == place && contains(*over, arm)),
            );
            for after in found {
                if !minted.insert((place.clone(), after.of, after.role)) {
                    continue;
                }
                let held = self.scopes[reach.holding_scope(&self.scopes, after.of).0 as usize].span.end;
                let end = guards.bounds.get(&after.of).map_or(held, |&b| b.min(held));
                regions.extend(guard_region_witnesses(place, after, end, &proofs, |region| {
                    let cut = match place.is_variable() {
                        true => rebinds.earliest_in(place, region),
                        false => place_cut.and_then(|idx| idx.earliest_in(place, region)),
                    };
                    super::cut_region(region, [cut])
                }));
            }
        }

        for mut w in joins {
            super::bind_variable_witness(&mut w, &self.scopes, |n, at, sc| self.variable_binding_of(n, at, sc));
            self.witnesses.push(w);
        }
        for w in member_joins.into_iter().chain(regions) {
            self.witnesses.push(w);
        }
    }

    /// The branch and loop joins a write makes of its place, out to the
    /// place's binding (or the callable the write runs in): each with the
    /// scope it is held to, the points it lands at, and its arms.
    fn write_joins(
        &self,
        plan: &JoinPlan<'_>,
        site: &WriteSite<'_>,
        minted: &mut HashSet<(super::Place, Span, crate::model::witnesses::JoinRole)>,
    ) -> Vec<(super::ScopeId, crate::model::witnesses::WitnessAttachment, Vec<Point>, Vec<crate::model::witnesses::Witness>)> {
        use crate::model::witnesses::WitnessAttachment;
        let reach = Reach::of(&self.scopes, site.scope, site.home);
        let holding = plan
            .arms_holding(site.at)
            .filter(|(_, a)| contains(a.span, site.written))
            .take_while(|(_, a)| contains(reach.within, a.span))
            .filter(|(c, _)| !plan.is_lifted(c) && contains(reach.within, c.span))
            .filter(|(c, _)| site.bound_from.is_none_or(|b| !plan.binds_each_pass(c, b)));
        let mut out = Vec::new();
        for (c, _) in holding {
            let found = match c.kind {
                ControlKind::Branch => plan.branch_joins(c, site.bound_from, self.scopes[site.scope.0 as usize].span, site.written, site.writes),
                ControlKind::Loop { .. } => plan.loop_joins(c, site.bound_from, site.writes),
                ControlKind::Try { .. } => plan.try_joins(c, site.bound_from, site.writes),
                ControlKind::Block { .. } => continue,
            };
            // Held to the scope around the construct, so a sibling block
            // never sees them.
            let scope = reach.joins_scope(&self.scopes, c.span, &found);
            for BranchJoin { role, of, lands, arms } in found {
                if !minted.insert((site.place.clone(), of, role)) {
                    continue;
                }
                let join = WitnessAttachment::Join { place: Box::new(site.place.clone()), of, role };
                let arms = join_arms(&join, site.place, &arms, &Proofs::new());
                out.push((scope, join, lands, arms));
            }
        }
        out
    }
}

/// A write to a write-once member through its own method's receiver
/// (`FileAnalysis::member_states`).
#[derive(Debug, Clone)]
pub struct MemberStore {
    /// The receiver's member: `Binding::Receiver` of the method, one
    /// `PathStep::Member`.
    pub place: super::Place,
    /// The member token written.
    pub token: Span,
    /// Where the write lands.
    pub at: Point,
    /// The expression whose whole value the write stores, where it stores
    /// one.
    pub value: Option<Span>,
}

impl super::FileAnalysis {
    /// What a write-once member holds as its class's methods run
    /// (`docs/adr/cfg-tier.md` §Write-once members): unset where the
    /// constructor starts, unless a parameter of the constructor sets it;
    /// where any other method starts, as the constructor left it; and past
    /// each write, the value written. The writes it keeps are those made in
    /// their own method's body, each with that body's scope.
    ///
    /// TODO(readonly-construction-calls): a method the constructor calls
    /// runs before the constructor ends, but starts here as an end leaves
    /// the member. Owner: the whole-program analysis epic.
    fn member_states(
        &self,
        stores: Vec<MemberStore>,
        returns: &[Span],
    ) -> (Vec<crate::model::witnesses::Witness>, Vec<(MemberStore, super::ScopeId)>) {
        use super::{Binding, ScopeId};
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
        let mut out = Vec::new();
        let mut kept = Vec::new();
        if stores.is_empty() {
            return (out, kept);
        }
        let body_of = |m: super::SymbolId| {
            self.scopes
                .iter()
                .position(|s| s.kind.is_callable() && s.owner == Some(m))
                .map(|i| ScopeId(i as u32))
        };
        let point = |p: Point| Span { start: p, end: p };
        let fact = |place: &super::Place, payload, span| Witness {
            attachment: WitnessAttachment::Place(Box::new(place.clone())),
            source: WitnessSource::Builder(JOIN_SOURCE.into()),
            payload,
            span,
        };
        let mut seeded: HashSet<super::Place> = HashSet::new();
        for store in stores {
            let Binding::Receiver(m) = store.place.root else { continue };
            let Some(home) = body_of(m) else { continue };
            let Some(runs_in) = self.callable_around(store.at) else { continue };
            if !contains(self.scopes[home.0 as usize].span, self.scopes[runs_in.0 as usize].span) {
                continue;
            }
            self.member_entry(&store.place, m, home, returns, &body_of, &mut seeded, &mut out);
            // A write in a body made inside the method sets the member only
            // if that body runs, so past where it is made the member may
            // hold anything.
            // TODO(readonly-closure-writes): the write reaches the member
            // only where the body is called. Owner: the whole-program
            // analysis epic (closure-call-writes).
            if runs_in != home {
                out.push(fact(&store.place, WitnessPayload::Reset, point(self.scopes[runs_in.0 as usize].span.end)));
                continue;
            }
            out.push(fact(&store.place, WitnessPayload::Reset, point(store.at)));
            if let Some(v) = store.value {
                out.push(fact(&store.place, WitnessPayload::Edge(WitnessAttachment::Expr(v)), point(store.at)));
            }
            kept.push((store, home));
        }
        (out, kept)
    }

    /// The state `place` (a member of method `m`'s receiver) starts `m` in:
    /// the constructor's own seed, or the constructor's end.
    fn member_entry(
        &self,
        place: &super::Place,
        m: super::SymbolId,
        home: super::ScopeId,
        returns: &[Span],
        body_of: &dyn Fn(super::SymbolId) -> Option<super::ScopeId>,
        seeded: &mut HashSet<super::Place>,
        out: &mut Vec<crate::model::witnesses::Witness>,
    ) {
        use super::{Binding, InferredType, Place, SymKind, SymbolFlags};
        use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
        if !seeded.insert(place.clone()) {
            return;
        }
        let sym = &self.symbols[m.0 as usize];
        let (Some(class), [super::PathStep::Member(name)]) = (sym.package.as_deref(), place.path.as_slice()) else { return };
        let Some((ctor, ctor_body)) = self
            .symbols
            .iter()
            .filter(|s| s.kind == SymKind::Method && s.package.as_deref() == Some(class) && s.flags.contains(SymbolFlags::CONSTRUCTOR))
            .find_map(|s| Some((s.id, body_of(s.id)?)))
        else {
            return;
        };
        let fact = |att: WitnessAttachment, payload, at: Point| Witness {
            attachment: att,
            source: WitnessSource::Builder(JOIN_SOURCE.into()),
            payload,
            span: Span { start: at, end: at },
        };
        let held = Place { root: Binding::Receiver(ctor), path: place.path.clone() };
        let ctor_span = self.scopes[ctor_body.0 as usize].span;
        if seeded.insert(held.clone()) || m == ctor {
            // A promoted parameter is the member, set as the call begins.
            let promoted = self
                .symbols
                .iter()
                .filter(|s| s.kind == SymKind::Field && s.name == *name && s.package.as_deref() == Some(class))
                .find_map(|s| {
                    self.declared_together(s)
                        .find(|t| t.scope == ctor_body && t.kind != SymKind::HashKeyDef)
                        .map(|t| t.id)
                });
            let payload = match promoted {
                Some(twin) => WitnessPayload::Edge(WitnessAttachment::Place(Box::new(Place::variable(Binding::Lexical(twin))))),
                None => WitnessPayload::InferredType(InferredType::Undef),
            };
            out.push(fact(WitnessAttachment::Place(Box::new(held.clone())), payload, ctor_span.start));
        }
        // A method its language runs on an object no constructor built
        // starts from nothing the constructor says.
        if m == ctor || sym.flags.contains(SymbolFlags::UNCONSTRUCTED) {
            return;
        }
        let body = self.scopes[home.0 as usize].span;
        let join = WitnessAttachment::Join { place: Box::new(place.clone()), of: body, role: JoinRole::Entry };
        for payload in [WitnessPayload::Reset, WitnessPayload::Edge(join.clone())] {
            out.push(Witness { attachment: WitnessAttachment::Place(Box::new(place.clone())), source: WitnessSource::Builder(JOIN_SOURCE.into()), payload, span: body });
        }
        // The constructor ends where its body does and at each `return`
        // of its own; a throw builds no object to run a method on.
        let ends = returns
            .iter()
            .filter(|r| contains(ctor_span, **r) && self.callable_around(r.start) == Some(ctor_body))
            .map(|r| r.end)
            .chain([ctor_span.end]);
        for end in ends {
            out.push(fact(join.clone(), WitnessPayload::Edge(WitnessAttachment::Place(Box::new(held.clone()))), end));
        }
    }
}

impl super::FileAnalysis {
    /// The innermost callable scope around `at`.
    fn callable_around(&self, at: Point) -> Option<super::ScopeId> {
        let sc = self.scope_at(at)?;
        self.scope_chain(sc).into_iter().find(|s| self.scopes[s.0 as usize].kind.is_callable())
    }
}

/// A write as `FileAnalysis::write_joins` places it among the constructs
/// around it.
struct WriteSite<'p> {
    place: &'p super::Place,
    at: Point,
    /// The scope the write is made in.
    scope: super::ScopeId,
    /// The scope the written place lives in.
    home: super::ScopeId,
    bound_from: Option<Point>,
    /// The tokens written: an arm ending inside them is part of the value.
    written: Span,
    /// Every write to the place, where it lands: the paths a body may
    /// unwind from (`JoinPlan::try_joins`).
    writes: &'p [Point],
}

impl super::FileAnalysis {
    /// The callable whose body `body` is: the narrowest callable scope
    /// holding it that names its owner.
    fn callable_of_body(&self, body: Span) -> Option<super::SymbolId> {
        self.scopes
            .iter()
            .filter(|s| s.kind.is_callable() && contains(s.span, body))
            .min_by_key(|s| (std::cmp::Reverse((s.span.start.row, s.span.start.column)), (s.span.end.row, s.span.end.column)))?
            .owner
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
