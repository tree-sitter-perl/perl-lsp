//! Perl's control structure, read from `queries/perl/ctl.scm` into a
//! [`ControlView`]: the same vocabulary every pack document speaks, so the
//! join builder reads one shape for every language (`docs/adr/cfg-tier.md`).

// TODO(cfg-joins): the branch joins (design PR 5) are the first reader; until
// then only the cross-check against `cst::conditional_arm` runs the view.
#![cfg_attr(not(test), allow(dead_code))]

use crate::build::query_extract::{Capture, CtlCap};
use crate::model::file_analysis::{Arm, ArmRole, Construct, ControlKind, ControlView, Leave, LeaveKind};
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

/// The view over one file's matches of [`ctl_query`]. Arms group by the
/// construct their pattern captures, so a construct several patterns
/// describe (an `if` with an `else`) is one construct.
pub(super) fn control_view<'t>(matches: impl IntoIterator<Item = Vec<(u32, Node<'t>)>>) -> ControlView {
    let (_, caps) = ctl_query();
    let span = super::node_to_span;
    let mut view = ControlView::default();
    let mut by_node: HashMap<usize, usize> = HashMap::new();
    for m in matches {
        let cap = |i: u32| caps[i as usize];
        let construct = m.iter().find_map(|&(i, n)| {
            let kind = match cap(i) {
                CtlCap::Branch => ControlKind::Branch,
                CtlCap::Loop => ControlKind::Loop { once: false },
                CtlCap::LoopOnce => ControlKind::Loop { once: true },
                _ => return None,
            };
            Some((n, kind))
        });
        let Some((node, kind)) = construct else {
            for &(i, n) in &m {
                if let Some(kind) = leave_kind(cap(i)) {
                    view.leaves.push(Leave { span: span(n), kind });
                }
            }
            continue;
        };
        let ix = *by_node.entry(node.id()).or_insert_with(|| {
            view.constructs.push(Construct { span: span(node), kind, cond: None, arms: Vec::new() });
            view.constructs.len() - 1
        });
        let c = &mut view.constructs[ix];
        // A body that runs before its first test says so in one pattern.
        if kind == (ControlKind::Loop { once: true }) {
            c.kind = kind;
        }
        for &(i, n) in &m {
            let role = match cap(i) {
                CtlCap::Cond => {
                    c.cond.get_or_insert(span(n));
                    continue;
                }
                CtlCap::Then => ArmRole::When(true),
                CtlCap::Else => ArmRole::When(false),
                CtlCap::LoopBody => ArmRole::Body,
                CtlCap::LoopStep => ArmRole::Step,
                _ => continue,
            };
            let arm = Arm { span: span(n), role };
            if !c.arms.contains(&arm) {
                c.arms.push(arm);
            }
        }
    }
    view.constructs.sort_by_key(|c| (c.span.start.row, c.span.start.column, std::cmp::Reverse((c.span.end.row, c.span.end.column))));
    view
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
