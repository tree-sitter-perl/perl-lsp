//! Perl's control structure, read from `queries/perl/ctl.scm` into a
//! [`ControlView`]: the same vocabulary every pack document speaks, so the
//! join builder reads one shape for every language (`docs/adr/cfg-tier.md`).

use crate::build::query_extract::{Capture, CtlCap};
use crate::cst::{Kind, NodeExt};
use crate::model::file_analysis::{Arm, ArmRole, CallableBody, Construct, ControlKind, ControlView, Leave, LeaveKind, LeaveTarget, Span};
use std::collections::HashMap;
use tree_sitter::{Node, Point, Query};

static CTL_SCM: &str = include_str!("../../../queries/perl/ctl.scm");


/// The compiled control document, compiled once, with each capture id's
/// parsed `Capture`. The document is compiled into the binary, so a capture
/// outside the `@ctl` family is a constant defect and panics on first use.
pub(super) fn ctl_query() -> (&'static Query, &'static [CtlCap]) {
    use std::sync::OnceLock;
    static CTL_QUERY: OnceLock<(Query, Vec<CtlCap>)> = OnceLock::new();
    let (query, caps) = CTL_QUERY.get_or_init(|| {
        let lang: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
        let query = Query::new(&lang, CTL_SCM).expect("queries/perl/ctl.scm compiles");
        let caps = query
            .capture_names()
            .iter()
            .map(|n| match Capture::parse(n) {
                Ok(Capture::Ctl(c)) => c,
                _ => panic!("queries/perl/ctl.scm: `@{n}` is not a `@ctl` capture"),
            })
            .collect();
        (query, caps)
    });
    (query, caps)
}

/// Force the control document to compile now, off the parallel per-file path.
pub(crate) fn warm_ctl_query() {
    let _ = ctl_query();
}

/// The view over one file's matches of a control document, each match its
/// captures as `(capture, node id, span, text)`; only a name or a count
/// needs its text. Arms group by the construct their pattern captures, so
/// a construct several patterns describe (an `if` with an `else`) is one
/// construct. Every language's document reads through this one grouping.
pub(crate) fn control_view<'s>(matches: impl IntoIterator<Item = Vec<(CtlCap, usize, Span, &'s str, Point)>>) -> ControlView {
    let mut view = ControlView::default();
    let mut by_node: HashMap<usize, usize> = HashMap::new();
    let label = |view: &mut ControlView, text: &str| -> u32 {
        match view.labels.iter().position(|l| l == text) {
            Some(i) => i as u32,
            None => {
                view.labels.push(text.to_string());
                (view.labels.len() - 1) as u32
            }
        }
    };
    for m in matches {
        let construct = m.iter().find_map(|&(cap, id, span, _, _)| {
            let kind = match cap {
                CtlCap::Branch => ControlKind::Branch,
                CtlCap::Loop => ControlKind::Loop { once: false },
                CtlCap::LoopOnce => ControlKind::Loop { once: true },
                CtlCap::Block => ControlKind::Block { next: false },
                CtlCap::BlockNext => ControlKind::Block { next: true },
                CtlCap::Try => ControlKind::Try { quiet: false },
                CtlCap::TryQuiet => ControlKind::Try { quiet: true },
                _ => return None,
            };
            Some((id, span, kind))
        });
        let Some((node, at, kind)) = construct else {
            let target = m.iter().find_map(|&(cap, _, _, text, _)| match cap {
                CtlCap::Label => Some(LeaveTarget::Label(label(&mut view, text))),
                CtlCap::Levels => text.trim().parse().ok().map(LeaveTarget::Levels),
                _ => None,
            });
            for &(cap, _, span, _, _) in &m {
                match cap {
                    CtlCap::Body => view.bodies.push(CallableBody { span, readable: true, hoisted: false, made_before: None }),
                    CtlCap::Gate => view.gates.push(span),
                    CtlCap::Raise => view.raises.push(span),
                    _ => {
                        if let Some(kind) = leave_kind(cap) {
                            view.leaves.push(Leave { span, kind, target: target.unwrap_or_default() });
                        }
                    }
                }
            }
            continue;
        };
        let ix = *by_node.entry(node).or_insert_with(|| {
            view.constructs.push(Construct { span: at, kind, cond: None, arms: Vec::new(), label: None });
            view.constructs.len() - 1
        });
        // A body that runs before its first test says so in one pattern.
        if kind == (ControlKind::Loop { once: true }) {
            view.constructs[ix].kind = kind;
        }
        for &(cap, _, span, text, tail) in &m {
            let role = match cap {
                CtlCap::Cond => {
                    view.constructs[ix].cond.get_or_insert(span);
                    continue;
                }
                CtlCap::Label => {
                    let name = label(&mut view, text);
                    view.constructs[ix].label = Some(name);
                    continue;
                }
                CtlCap::Then => ArmRole::When(true),
                CtlCap::Else => ArmRole::When(false),
                CtlCap::LoopBody => ArmRole::Body,
                CtlCap::LoopStep => ArmRole::Step,
                CtlCap::TryBody => ArmRole::Tried,
                CtlCap::TryCatch => ArmRole::Handler,
                CtlCap::TryElse => ArmRole::Settled,
                CtlCap::TryFinally => ArmRole::Finally,
                _ => continue,
            };
            let arm = Arm { span, role, tail };
            let c = &mut view.constructs[ix];
            if !c.arms.contains(&arm) {
                c.arms.push(arm);
            }
        }
    }
    view.constructs.sort_by_key(|c| (c.span.start.row, c.span.start.column, std::cmp::Reverse((c.span.end.row, c.span.end.column))));
    // Only a try's arms unwind to a handler.
    let tried: Vec<Span> = view
        .constructs
        .iter()
        .filter(|c| matches!(c.kind, ControlKind::Try { .. }))
        .flat_map(|c| c.arms.iter().map(|a| a.span))
        .collect();
    view.raises.retain(|r| tried.iter().any(|t| t.start <= r.start && r.end <= t.end));
    view.raises.sort_by_key(|r| (r.start, r.end));
    view.raises.dedup();
    view
}

/// Every place `node` may die at, at any depth, as [`control_view`] reads
/// a capture. It runs the whole document and keeps the raises: a second,
/// raise-only compile of the document costs every process about as much
/// as the first, and a deep search runs only inside a `try` arm.
fn raises_in<'s>(node: Node<'_>, source: &'s [u8]) -> Vec<Vec<(CtlCap, usize, Span, &'s str, Point)>> {
    use tree_sitter::StreamingIterator;
    let (query, caps) = ctl_query();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut out = Vec::new();
    let mut it = cursor.matches(query, node, source);
    while let Some(m) = it.next() {
        for c in m.captures.iter().filter(|c| caps[c.index as usize] == CtlCap::Raise) {
            out.push(captured([(CtlCap::Raise, c.node)], source));
        }
    }
    out
}

/// One match's captures as [`control_view`] reads them.
fn captured<'t, 's>(caps: impl IntoIterator<Item = (CtlCap, Node<'t>)>, source: &'s [u8]) -> Vec<(CtlCap, usize, Span, &'s str, Point)> {
    caps.into_iter()
        .map(|(c, n)| {
            use crate::cst::NodeExt;
            let span = super::node_to_span(n);
            let tail = match n.kind_of() {
                Kind::Block => n.elements().last().map_or(span.end, |s| s.end_position()),
                _ => span.end,
            };
            let text = match c {
                CtlCap::Label | CtlCap::Levels => n.utf8_text(source).unwrap_or(""),
                _ => "",
            };
            (c, n.id(), span, text, tail)
        })
        .collect()
}

/// One walk's view (`ViewWalk::walk`), `gate` naming its gates.
#[cfg(test)]
pub(super) fn control_view_with_gates<'t>(
    nodes: impl IntoIterator<Item = Node<'t>>,
    source: &[u8],
    gate: impl Fn(Node<'t>) -> bool,
) -> ControlView {
    let mut walk = ViewWalk::default();
    walk.walk(nodes, source, &gate);
    walk.finish()
}

/// A control view gathered over several walks: the statements around the
/// writes and tests a file joins, then the bodies of the callables their
/// gates call, each walk adding what it finds.
#[derive(Default)]
pub(super) struct ViewWalk<'s> {
    out: Vec<Vec<(CtlCap, usize, Span, &'s str, Point)>>,
    seen: std::collections::HashSet<usize>,
    gates: Vec<Span>,
    bodies: Vec<CallableBody>,
}

impl<'s> ViewWalk<'s> {
    /// Adds the constructs at `nodes`, and every statement a construct
    /// found there runs in its arms, down to the statements that leave:
    /// what decides whether an arm comes out at its end, and where a loop's
    /// `last` and `next` land. Each node is tried as a match's root and
    /// nothing below it starts one, so the cost is the nodes' own, not the
    /// file's. A statement `gate` names is a call control comes back from
    /// only if its callee does ([`ControlView::gates`]); it is returned.
    pub(super) fn walk<'t>(
        &mut self,
        nodes: impl IntoIterator<Item = Node<'t>>,
        source: &'s [u8],
        gate: &dyn Fn(Node<'t>) -> bool,
    ) -> Vec<Node<'t>> {
        use tree_sitter::StreamingIterator;
        let (query, caps) = ctl_query();
        let mut cursor = tree_sitter::QueryCursor::new();
        cursor.set_max_start_depth(Some(0));
        let mut gates = Vec::new();
        let mut work: Vec<Node<'t>> = nodes.into_iter().collect();
        while let Some(node) = work.pop() {
            if !self.seen.insert(node.id()) {
                continue;
            }
            if gate(node) {
                self.gates.push(super::node_to_span(node));
                gates.push(node);
            }
            let mut it = cursor.matches(query, node, source);
            while let Some(m) = it.next() {
                for c in m.captures.iter() {
                    // A bare block runs its statements where it stands, with
                    // no arm between them and the code around it.
                    if matches!(caps[c.index as usize], CtlCap::Block | CtlCap::BlockNext) {
                        work.extend(statements_of(c.node));
                        continue;
                    }
                    if !caps[c.index as usize].is_arm() {
                        continue;
                    }
                    // A tried arm may die anywhere inside it, not only at a
                    // statement the walk roots a match at.
                    if matches!(caps[c.index as usize], CtlCap::TryBody | CtlCap::TryCatch | CtlCap::TryElse | CtlCap::TryFinally) {
                        self.out.extend(raises_in(c.node, source));
                    }
                    if c.node.is(Kind::Block) {
                        work.extend(statements_of(c.node));
                    } else {
                        work.push(c.node);
                    }
                }
                self.out.push(captured(m.captures.iter().map(|c| (caps[c.index as usize], c.node)), source));
            }
        }
        gates
    }

    /// Records a callable's body the walk follows into.
    pub(super) fn body(&mut self, span: Span, readable: bool, hoisted: bool, made_before: Option<Point>) {
        self.bodies.push(CallableBody { span, readable, hoisted, made_before });
    }

    pub(super) fn finish(self) -> ControlView {
        let mut view = control_view(self.out);
        view.gates = self.gates;
        view.bodies = self.bodies;
        view
    }
}

/// The statements a block runs, each with the expression it holds: where a
/// construct, a leave or a gate can sit with no arm between it and the block.
pub(super) fn statements_of(block: Node<'_>) -> Vec<Node<'_>> {
    use crate::cst::NodeExt;
    let mut out = Vec::new();
    for stmt in block.elements() {
        out.push(stmt);
        if stmt.is(Kind::ExpressionStatement) {
            out.extend(crate::cst::nth_element(stmt, 0));
        }
    }
    out
}

/// How far [`arm_around`] climbs: to the enclosing sub (or file), or only
/// to the enclosing block. A fact that lands on the block's own scope asks
/// `Block`: inside the block, the block has, by construction, run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ArmBound {
    Sub,
    Block,
}

/// The arm the document declares, between `node` and `bound`, that holds
/// `node`: the innermost construct around it whose `@ctl` arm captures
/// lie on the climb. `None` = `node` runs whenever its bound does. Each
/// ancestor is tried as a match's root only, so the cost is the climb's,
/// not the file's. Scope-crossing conditionality (a write inside a closure
/// relative to an outer variable's scope) is the caller's check.
pub(super) fn arm_around<'t>(syn: &crate::cst::SyntaxForest<'t>, source: &[u8], node: Node<'t>, bound: ArmBound) -> Option<Node<'t>> {
    use tree_sitter::StreamingIterator;
    let (query, caps) = ctl_query();
    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_max_start_depth(Some(0));
    let mut path = vec![node.id()];
    for p in syn.ancestors(node) {
        let kind: Kind = p.kind_of();
        if crate::cst::is_callable_boundary(kind) || kind == Kind::SourceFile || (bound == ArmBound::Block && kind == Kind::Block) {
            return None;
        }
        let mut it = cursor.matches(query, p, source);
        while let Some(m) = it.next() {
            if let Some(c) = m.captures.iter().find(|c| caps[c.index as usize].is_arm() && path.contains(&c.node.id())) {
                return Some(c.node);
            }
        }
        path.push(p.id());
    }
    None
}

/// One arm of an `if`/`unless` chain.
#[derive(Clone, Copy)]
pub(super) struct CondArm<'a> {
    /// `None` for the trailing `else`.
    pub cond: Option<Node<'a>>,
    pub block: Node<'a>,
    /// The truth value `cond` must take for `block` to run: `false` only
    /// for the leading arm of an `unless`. An `else` arm carries `true`.
    pub enter: bool,
}

/// A branch statement's arms in order, as the document declares them: the
/// leading arm, each later branch's (an `elsif`), then the trailing `else`.
/// A branch's own arm comes first in source; the arm after it is either the
/// next branch of the chain or the `else`.
pub(super) fn branch_chain<'t>(source: &[u8], stmt: Node<'t>) -> Vec<CondArm<'t>> {
    let mut out = Vec::new();
    let mut at = stmt;
    while let Some((cond, arms)) = branch_at(source, at) {
        let Some(&(enter, block)) = arms.first() else { break };
        out.push(CondArm { cond, block, enter });
        let Some(&(_, next)) = arms.get(1) else { break };
        if branch_at(source, next).is_none() {
            out.push(CondArm { cond: None, block: next, enter: true });
            break;
        }
        at = next;
    }
    out
}

/// The branch rooted at `node`: its condition, and each arm with whether
/// the condition holds there, in source order.
fn branch_at<'t>(source: &[u8], node: Node<'t>) -> Option<(Option<Node<'t>>, Vec<(bool, Node<'t>)>)> {
    use tree_sitter::StreamingIterator;
    let (query, caps) = ctl_query();
    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_max_start_depth(Some(0));
    let mut it = cursor.matches(query, node, source);
    let mut found = None;
    while let Some(m) = it.next() {
        if !m.captures.iter().any(|c| caps[c.index as usize] == CtlCap::Branch && c.node == node) {
            continue;
        }
        let (cond, arms) = found.get_or_insert_with(|| (None, Vec::new()));
        for c in m.captures {
            match caps[c.index as usize] {
                CtlCap::Cond => *cond = cond.or(Some(c.node)),
                CtlCap::Then | CtlCap::Else => {
                    let holds = caps[c.index as usize] == CtlCap::Then;
                    if !arms.iter().any(|&(_, n): &(bool, Node<'t>)| n == c.node) {
                        arms.push((holds, c.node));
                    }
                }
                _ => {}
            }
        }
    }
    let (cond, mut arms) = found?;
    arms.sort_by_key(|&(_, n)| n.start_byte());
    Some((cond, arms))
}

fn leave_kind(c: CtlCap) -> Option<LeaveKind> {
    Some(match c {
        CtlCap::LeaveReturn => LeaveKind::Return,
        CtlCap::LeaveThrow => LeaveKind::Throw,
        CtlCap::LeaveExit => LeaveKind::Exit,
        CtlCap::LeaveBreak => LeaveKind::Break,
        CtlCap::LeaveContinue => LeaveKind::Continue,
        CtlCap::LeaveJump => LeaveKind::Jump,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
