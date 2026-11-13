//! Perl's control structure, read from `queries/perl/ctl.scm` into a
//! [`ControlView`]: the same vocabulary every pack document speaks, so the
//! join builder reads one shape for every language (`docs/adr/cfg-tier.md`).

use crate::build::query_extract::{Capture, CtlCap};
use crate::model::file_analysis::{Arm, ArmRole, Construct, ControlKind, ControlView, Leave, LeaveKind, Span};
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
/// captures as `(capture, node id, span)`. Arms group by the construct
/// their pattern captures, so a construct several patterns describe (an
/// `if` with an `else`) is one construct. Every language's document reads
/// through this one grouping.
pub(crate) fn control_view(matches: impl IntoIterator<Item = Vec<(CtlCap, usize, Span)>>) -> ControlView {
    let mut view = ControlView::default();
    let mut by_node: HashMap<usize, usize> = HashMap::new();
    for m in matches {
        let construct = m.iter().find_map(|&(cap, id, span)| {
            let kind = match cap {
                CtlCap::Branch => ControlKind::Branch,
                CtlCap::Loop => ControlKind::Loop { once: false },
                CtlCap::LoopOnce => ControlKind::Loop { once: true },
                _ => return None,
            };
            Some((id, span, kind))
        });
        let Some((node, at, kind)) = construct else {
            for &(cap, _, span) in &m {
                if let Some(kind) = leave_kind(cap) {
                    view.leaves.push(Leave { span, kind });
                }
            }
            continue;
        };
        let ix = *by_node.entry(node).or_insert_with(|| {
            view.constructs.push(Construct { span: at, kind, cond: None, arms: Vec::new() });
            view.constructs.len() - 1
        });
        let c = &mut view.constructs[ix];
        // A body that runs before its first test says so in one pattern.
        if kind == (ControlKind::Loop { once: true }) {
            c.kind = kind;
        }
        for &(cap, _, span) in &m {
            let role = match cap {
                CtlCap::Cond => {
                    c.cond.get_or_insert(span);
                    continue;
                }
                CtlCap::Then => ArmRole::When(true),
                CtlCap::Else => ArmRole::When(false),
                CtlCap::LoopBody => ArmRole::Body,
                CtlCap::LoopStep => ArmRole::Step,
                _ => continue,
            };
            let arm = Arm { span, role };
            if !c.arms.contains(&arm) {
                c.arms.push(arm);
            }
        }
    }
    view.constructs.sort_by_key(|c| (c.span.start.row, c.span.start.column, std::cmp::Reverse((c.span.end.row, c.span.end.column))));
    view
}

/// One match's captures as [`control_view`] reads them.
fn captured<'t>(caps: impl IntoIterator<Item = (CtlCap, Node<'t>)>) -> Vec<(CtlCap, usize, Span)> {
    caps.into_iter().map(|(c, n)| (c, n.id(), super::node_to_span(n))).collect()
}

/// The control view of the constructs at `nodes`: each node is tried as a
/// match's root and nothing below it starts one, so the cost is the nodes'
/// own, not the file's. The joins ask it of a write's ancestors, which are
/// the only constructs that can hold the write.
pub(super) fn control_view_at<'t>(nodes: impl IntoIterator<Item = Node<'t>>, source: &[u8]) -> ControlView {
    use tree_sitter::StreamingIterator;
    let (query, caps) = ctl_query();
    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_max_start_depth(Some(0));
    let mut out = Vec::new();
    for node in nodes {
        let mut it = cursor.matches(query, node, source);
        while let Some(m) = it.next() {
            out.push(captured(m.captures.iter().map(|c| (caps[c.index as usize], c.node))));
        }
    }
    control_view(out)
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
