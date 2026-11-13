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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinArm {
    pub at: Point,
    /// The arm whose end this is, on a path an arm runs.
    pub arm: Option<Span>,
    pub untaken: Option<Untaken>,
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
    /// Each construct's index, by its address.
    index: HashMap<*const Construct, usize>,
    /// Constructs by their span.
    at_span: HashMap<Span, Vec<usize>>,
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
        let index = view.constructs.iter().enumerate().map(|(i, c)| (c as *const Construct, i)).collect();
        let mut at_span: HashMap<Span, Vec<usize>> = HashMap::new();
        for (i, c) in view.constructs.iter().enumerate() {
            at_span.entry(c.span).or_default().push(i);
        }
        let mut plan = JoinPlan { view, leaves: vec![false; arms.len()], arms, around, index, at_span, ends, arm_spans };
        plan.mark_leaving();
        plan
    }

    /// Marks every arm control never comes out of: one a leaving statement
    /// sits in with no arm between them, or one holding a branch that leaves
    /// on every path. Inner constructs are decided before the arms around
    /// them.
    fn mark_leaving(&mut self) {
        let view = self.view;
        for l in &view.leaves {
            let held = self.holding_ix(l.span.start).find(|&i| contains(self.arm_span(i), l.span));
            if let Some(i) = held {
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
            if c.kind != ControlKind::Branch || !(has(true) && has(false)) {
                continue;
            }
            if !(0..c.arms.len()).all(|a| self.leaves[self.arm_ix(k, a)]) {
                continue;
            }
            let holder = self.holding_ix(c.span.start).find(|&i| self.arms[i].0 != k && contains(self.arm_span(i), c.span));
            if let Some(i) = holder {
                self.mark_left(i);
            }
        }
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
    /// is one control never comes out of.
    pub fn drops_a_path(&self, c: &'v Construct) -> bool {
        let mut reached = vec![c];
        self.arm_points(c, &mut Vec::new(), &mut reached);
        reached.iter().any(|k| {
            let ix = self.index[&(*k as *const Construct)];
            (0..k.arms.len()).any(|a| self.leaves[self.arm_ix(ix, a)])
        })
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
    pub fn branch_joins(&self, c: &'v Construct, bound_from: Option<Point>, written_in: Span) -> Vec<BranchJoin> {
        use crate::model::witnesses::JoinRole;
        let bound = |p: &Point| bound_from.is_none_or(|b| b <= *p);
        let (after, reached) = self.after_join(c, bound_from);
        let mut out = vec![after];
        for k in reached {
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
                out.push(BranchJoin { role: JoinRole::ArmStart, of: k.span, lands, arms: vec![JoinArm { at: entry, arm: None, untaken: None }] });
            }
        }
        out
    }

    /// The join past `c` for a place bound from `bound_from`, and every
    /// branch whose paths are `c`'s.
    fn after_join(&self, c: &'v Construct, bound_from: Option<Point>) -> (BranchJoin, Vec<&'v Construct>) {
        use crate::model::witnesses::JoinRole;
        let mut arms = Vec::new();
        let mut reached = vec![c];
        self.arm_points(c, &mut arms, &mut reached);
        arms.retain(|a| bound_from.is_none_or(|b| b <= a.at));
        arms.sort_by_key(|a| (a.at, a.arm.map(|s| (s.start, s.end)), a.untaken.map(|u| (u.tested, u.of.start, u.of.end))));
        arms.dedup();
        // Just past the construct: its last arm leaves at its end, and that
        // arm's read must not meet the join it feeds.
        let past = Point { row: c.span.end.row, column: c.span.end.column + 1 };
        (BranchJoin { role: JoinRole::After, of: c.span, lands: vec![past], arms }, reached)
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
        within: Span,
        bound_from: Option<Point>,
        untaken_proved: impl Fn(Span) -> bool,
        narrowed: impl Fn(Span) -> bool,
    ) -> Vec<(&'v Construct, BranchJoin)> {
        let Some(found) = self.construct_at(of, ControlKind::Branch) else { return Vec::new() };
        let c = self.lifted_into(found);
        if !self.drops_a_path(c) || !contains(within, c.span) {
            return Vec::new();
        }
        let outer = self
            .arms_holding(c.span.start)
            .filter(|(k, _)| !std::ptr::eq(*k, c) && contains(k.span, c.span) && k.span != c.span)
            .take_while(|(_, a)| contains(within, a.span))
            .filter(|(k, _)| k.kind == ControlKind::Branch && !self.is_lifted(k) && contains(within, k.span))
            .map(|(k, _)| k);
        let mut out: Vec<(&'v Construct, BranchJoin)> = Vec::new();
        for k in std::iter::once(c).chain(outer) {
            let (after, _) = self.after_join(k, bound_from);
            let proved = |a: &JoinArm| match (a.untaken, a.arm) {
                (Some(u), _) => untaken_proved(u.of) || narrowed(u.of),
                (None, Some(arm)) => narrowed(arm) || out.iter().any(|(inner, _)| contains(arm, inner.span)),
                (None, None) => false,
            };
            if after.arms.is_empty() || !after.arms.iter().all(proved) {
                break;
            }
            out.push((k, after));
        }
        out
    }

    /// Where each path through `c` comes out of it: the end of every arm
    /// control comes out of (an arm a branch closes contributes that
    /// branch's paths), and, when no arm runs where the test goes one way,
    /// where the construct is entered. Every branch it passes through lands
    /// in `reached`.
    fn arm_points(&self, c: &'v Construct, out: &mut Vec<JoinArm>, reached: &mut Vec<&'v Construct>) {
        let k = self.index[&(c as *const Construct)];
        for (ai, a) in c.arms.iter().enumerate() {
            match self.closing(c, a).filter(|k| reached.len() < 64 && !reached.iter().any(|r| std::ptr::eq(*r, *k))) {
                Some(inner) => {
                    reached.push(inner);
                    self.arm_points(inner, out, reached);
                }
                None if self.leaves[self.arm_ix(k, ai)] => {}
                None => out.push(JoinArm { at: a.span.end, arm: Some(a.span), untaken: None }),
            }
        }
        let has = |b: bool| c.arms.iter().any(|a: &Arm| a.role == ArmRole::When(b));
        if !(has(true) && has(false)) && !c.arms.is_empty() {
            let tested = c.cond.map_or(entry_point(c), |cond| cond.end);
            out.push(JoinArm { at: entry_point(c), arm: None, untaken: Some(Untaken { of: c.span, tested }) });
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
            match proved {
                Some((payload, tested)) => arm(payload, tested),
                None => arm(WitnessPayload::Edge(read.clone()), a.at),
            }
        })
        .collect()
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
    let mut out: Vec<Witness> = lands
        .into_iter()
        .filter_map(|at| cut(Span { start: at, end }))
        .map(|region| Witness {
            attachment: WitnessAttachment::Place(Box::new(place.clone())),
            source: WitnessSource::Builder(JOIN_SOURCE.into()),
            payload: WitnessPayload::Edge(attachment.clone()),
            span: region,
        })
        .collect();
    if !out.is_empty() {
        out.extend(join_arms(&attachment, place, &arms, proofs));
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
    pub(crate) fn assemble_joins(&mut self, view: &ControlView, guards: GuardFacts, place_cut: Option<&super::PlaceDisturbanceIndex>) {
        use super::{Binding, Place};
        use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
        if view.constructs.is_empty() || (self.flow_edges.is_empty() && guards.seeds.is_empty()) {
            return;
        }
        let plan = JoinPlan::new(view);
        let mut minted: HashSet<(Place, Span, JoinRole)> = HashSet::new();
        let mut joins: Vec<Witness> = Vec::new();
        let source = || WitnessSource::Builder(JOIN_SOURCE.into());
        for fe in &self.flow_edges {
            let at = fe.target_at;
            let (binding, home) = self.variable_binding_of(&fe.target_name, at, fe.target_scope);
            let reach = Reach::of(&self.scopes, fe.target_scope, home);
            let place = Place::variable(binding.clone());
            let bound_from = match binding {
                Binding::Lexical(sid) => Some(self.symbols[sid.0 as usize].span.start),
                _ => None,
            };
            // An arm ending where the write lands (`x = a if c else b`'s
            // `b`) is part of the value written, not around the write.
            let written = fe.target_token.unwrap_or(Span { start: fe.source.start, end: at });
            let holding = plan
                .arms_holding(at)
                .filter(|(_, a)| contains(a.span, written))
                .take_while(|(_, a)| contains(reach.within, a.span))
                .filter(|(c, _)| c.kind == ControlKind::Branch && !plan.is_lifted(c) && contains(reach.within, c.span));
            for (c, _) in holding {
                // Held to the scope around the branch, so a sibling block
                // never sees it.
                let scope = reach.holding_scope(&self.scopes, c.span);
                for BranchJoin { role, of, lands, arms } in plan.branch_joins(c, bound_from, self.scopes[fe.target_scope.0 as usize].span) {
                    if !minted.insert((place.clone(), of, role)) {
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
                    joins.extend(join_arms(&join, &place, &arms, &Proofs::new()));
                }
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
                reach.within,
                bound_from,
                |of| proofs.get(&of).is_some_and(|ps| ps.iter().any(|(p, _)| *p == place)),
                |arm| guards.narrowed.iter().any(|(p, over)| p == place && contains(*over, arm)),
            );
            for (k, after) in found {
                if !minted.insert((place.clone(), after.of, after.role)) {
                    continue;
                }
                let held = self.scopes[reach.holding_scope(&self.scopes, k.span).0 as usize].span.end;
                let end = guards.bounds.get(&k.span).map_or(held, |&b| b.min(held));
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
        for w in regions {
            self.witnesses.push(w);
        }
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
