//! Perl's control structure, read from `queries/perl/ctl.scm` into a
//! [`ControlView`]: the same vocabulary every pack document speaks, so the
//! join builder reads one shape for every language (`docs/adr/cfg-tier.md`).

use crate::build::query_extract::{Capture, CtlCap};
use crate::model::file_analysis::{Arm, ArmRole, CallableBody, Construct, ControlKind, ControlView, Leave, LeaveKind, LeaveTarget, Span};
use std::collections::HashMap;
use tree_sitter::{Node, Query};

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

/// The view over one file's matches of a control document, each match its
/// captures as `(capture, node id, span, text)`; only a name or a count
/// needs its text. Arms group by the construct their pattern captures, so
/// a construct several patterns describe (an `if` with an `else`) is one
/// construct. Every language's document reads through this one grouping.
pub(crate) fn control_view<'s>(matches: impl IntoIterator<Item = Vec<(CtlCap, usize, Span, &'s str)>>) -> ControlView {
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
        let construct = m.iter().find_map(|&(cap, id, span, _)| {
            let kind = match cap {
                CtlCap::Branch => ControlKind::Branch,
                CtlCap::Loop => ControlKind::Loop { once: false },
                CtlCap::LoopOnce => ControlKind::Loop { once: true },
                CtlCap::Block => ControlKind::Block { next: false },
                CtlCap::BlockNext => ControlKind::Block { next: true },
                _ => return None,
            };
            Some((id, span, kind))
        });
        let Some((node, at, kind)) = construct else {
            let target = m.iter().find_map(|&(cap, _, _, text)| match cap {
                CtlCap::Label => Some(LeaveTarget::Label(label(&mut view, text))),
                CtlCap::Levels => text.trim().parse().ok().map(LeaveTarget::Levels),
                _ => None,
            });
            for &(cap, _, span, _) in &m {
                match cap {
                    CtlCap::Body => view.bodies.push(CallableBody { span, readable: true }),
                    CtlCap::Gate => view.gates.push(span),
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
        for &(cap, _, span, text) in &m {
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
                _ => continue,
            };
            let arm = Arm { span, role };
            let c = &mut view.constructs[ix];
            if !c.arms.contains(&arm) {
                c.arms.push(arm);
            }
        }
    }
    view.constructs.sort_by_key(|c| (c.span.start.row, c.span.start.column, std::cmp::Reverse((c.span.end.row, c.span.end.column))));
    view
}

/// One match's captures as [`control_view`] reads them.
fn captured<'t, 's>(caps: impl IntoIterator<Item = (CtlCap, Node<'t>)>, source: &'s [u8]) -> Vec<(CtlCap, usize, Span, &'s str)> {
    caps.into_iter()
        .map(|(c, n)| {
            let text = match c {
                CtlCap::Label | CtlCap::Levels => n.utf8_text(source).unwrap_or(""),
                _ => "",
            };
            (c, n.id(), super::node_to_span(n), text)
        })
        .collect()
}

/// One walk's view (`ViewWalk::walk`) with no gates.
#[cfg(test)]
pub(super) fn control_view_with_leaves<'t>(
    nodes: impl IntoIterator<Item = Node<'t>>,
    source: &[u8],
    exits: impl Fn(Node<'t>) -> bool,
) -> ControlView {
    let mut walk = ViewWalk::default();
    walk.walk(nodes, source, &exits, &|_| false);
    walk.finish()
}

/// A control view gathered over several walks: the statements around the
/// writes and tests a file joins, then the bodies of the callables their
/// gates call, each walk adding what it finds.
#[derive(Default)]
pub(super) struct ViewWalk<'s> {
    out: Vec<Vec<(CtlCap, usize, Span, &'s str)>>,
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
    /// file's. `exits(node)` names a call that leaves by its callee's
    /// nature (`TODO(cross-file-noreturn)`). A statement `gate` names is a
    /// call control comes back from only if its callee does
    /// ([`ControlView::gates`]); it is returned, and is no leave whatever
    /// `exits` says.
    pub(super) fn walk<'t>(
        &mut self,
        nodes: impl IntoIterator<Item = Node<'t>>,
        source: &'s [u8],
        exits: &dyn Fn(Node<'t>) -> bool,
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
            } else if exits(node) {
                self.out.push(vec![(CtlCap::LeaveThrow, node.id(), super::node_to_span(node), "")]);
            }
            let mut it = cursor.matches(query, node, source);
            while let Some(m) = it.next() {
                for c in m.captures.iter() {
                    if !matches!(caps[c.index as usize], CtlCap::Then | CtlCap::Else | CtlCap::LoopBody | CtlCap::LoopStep) {
                        continue;
                    }
                    if c.node.kind() == "block" {
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
    pub(super) fn body(&mut self, span: Span, readable: bool) {
        self.bodies.push(CallableBody { span, readable });
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
        if stmt.kind() == "expression_statement" {
            out.extend(crate::cst::nth_element(stmt, 0));
        }
    }
    out
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
