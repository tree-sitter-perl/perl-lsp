use super::*;
use crate::model::witnesses::JoinRole;

fn sp(a: (usize, usize), b: (usize, usize)) -> Span {
    Span { start: Point::new(a.0, a.1), end: Point::new(b.0, b.1) }
}

fn branch(span: Span, cond: Span, arms: &[(Span, bool)]) -> Construct {
    Construct {
        span,
        kind: ControlKind::Branch,
        cond: Some(cond),
        arms: arms.iter().map(|&(span, holds)| Arm { span, role: ArmRole::When(holds), tail: span.end }).collect(),
        label: None,
    }
}

fn after(plan: &JoinPlan<'_>, c: &Construct) -> Vec<JoinArm> {
    let joins = plan.branch_joins(c, None, sp((0, 0), (99, 0)), c.span);
    joins.into_iter().find(|j| j.role == JoinRole::After).expect("a join past it").arms
}

/// `if (c) { … return } else { … }`: the then arm leaves, so only the
/// else comes out.
#[test]
fn an_arm_holding_a_leave_is_no_arm_of_the_join() {
    let then = sp((0, 8), (0, 20));
    let els = sp((0, 26), (0, 30));
    let view = ControlView {
        constructs: vec![branch(sp((0, 0), (0, 30)), sp((0, 4), (0, 5)), &[(then, true), (els, false)])],
        labels: Vec::new(),
        leaves: vec![Leave { span: sp((0, 10), (0, 16)), kind: LeaveKind::Return, target: LeaveTarget::Nearest }],
        gates: Vec::new(),
        bodies: Vec::new(),
        returns: Vec::new(),
    };
    let plan = JoinPlan::new(&view);
    assert!(plan.drops_a_path(&view.constructs[0]));
    assert_eq!(after(&plan, &view.constructs[0]), vec![JoinArm { at: els.end, arm: Some(els), untaken: None, gates: Vec::new(), mute: false }]);
}

/// A leave under a nested branch's arm leaves that arm, not the outer one,
/// unless the nested branch leaves both ways.
#[test]
fn a_nested_branch_leaves_its_arm_only_when_every_way_leaves() {
    let outer_then = sp((0, 8), (0, 60));
    let inner = sp((0, 10), (0, 58));
    let (it, ie) = (sp((0, 20), (0, 30)), sp((0, 40), (0, 50)));
    let mk = |leaves: Vec<Leave>| ControlView {
        constructs: vec![
            branch(sp((0, 0), (0, 60)), sp((0, 4), (0, 5)), &[(outer_then, true)]),
            branch(inner, sp((0, 14), (0, 15)), &[(it, true), (ie, false)]),
        ],
        leaves,
        labels: Vec::new(),
        gates: Vec::new(),
        bodies: Vec::new(),
        returns: Vec::new(),
    };
    let one = mk(vec![Leave { span: sp((0, 22), (0, 28)), kind: LeaveKind::Return, target: LeaveTarget::Nearest }]);
    let plan = JoinPlan::new(&one);
    assert!(!plan.drops_a_path(&one.constructs[0]), "the inner else still comes out");
    let both = mk(vec![
        Leave { span: sp((0, 22), (0, 28)), kind: LeaveKind::Return, target: LeaveTarget::Nearest },
        Leave { span: sp((0, 42), (0, 48)), kind: LeaveKind::Throw, target: LeaveTarget::Nearest },
    ]);
    let plan = JoinPlan::new(&both);
    assert!(plan.drops_a_path(&both.constructs[0]));
    let arms = after(&plan, &both.constructs[0]);
    assert_eq!(arms.len(), 1, "only the untaken path comes out: {arms:?}");
    assert_eq!(arms[0].untaken, Some(Untaken { of: both.constructs[0].span, tested: Point::new(0, 5) }));
}

fn looped(span: Span, cond: Option<Span>, body: Span, label: Option<u32>) -> Construct {
    Construct { span, kind: ControlKind::Loop { once: false }, cond, arms: vec![Arm { span: body, role: ArmRole::Body, tail: body.end }], label }
}

fn joined(plan: &JoinPlan<'_>, c: &Construct, role: JoinRole) -> Vec<Point> {
    let joins = plan.loop_joins(c, None);
    joins.into_iter().find(|j| j.role == role).map_or_else(Vec::new, |j| j.arms.into_iter().map(|a| a.at).collect())
}

/// `while (c) { switch { break; continue; } break; }`: a `break` inside a
/// `switch` leaves the `switch`, a `continue` there the loop; the loop's
/// own `break` is its exit, and leaves its body.
#[test]
fn a_jump_leaves_the_construct_it_can_leave() {
    let lp = looped(sp((0, 0), (0, 100)), Some(sp((0, 6), (0, 7))), sp((0, 9), (0, 100)), None);
    let switch = Construct { span: sp((0, 10), (0, 40)), kind: ControlKind::Block { next: false }, cond: None, arms: Vec::new(), label: None };
    let jump = |at: usize, kind| Leave { span: sp((0, at), (0, at + 6)), kind, target: LeaveTarget::Nearest };
    let view = ControlView {
        constructs: vec![lp, switch],
        leaves: vec![jump(20, LeaveKind::Break), jump(30, LeaveKind::Continue), jump(50, LeaveKind::Break)],
        labels: Vec::new(),
        gates: Vec::new(),
        bodies: Vec::new(),
        returns: Vec::new(),
    };
    let plan = JoinPlan::new(&view);
    let c = &view.constructs[0];
    assert_eq!(joined(&plan, c, JoinRole::Head), vec![Point::new(0, 5), Point::new(0, 30)], "the entry and the `continue`; the body never ends");
    assert_eq!(joined(&plan, c, JoinRole::Exit), vec![Point::new(0, 8), Point::new(0, 50)], "past the failed test, and the loop's own `break`");
}

/// A named jump leaves the loop carrying the name, a counted one the n-th
/// around it; one naming nothing around it falls through, as modelled.
#[test]
fn a_named_or_counted_jump_leaves_the_loop_it_names() {
    let outer = looped(sp((0, 0), (0, 100)), None, sp((0, 10), (0, 100)), Some(0));
    let inner = looped(sp((0, 20), (0, 90)), None, sp((0, 30), (0, 90)), None);
    let jump = |at: usize, target| Leave { span: sp((0, at), (0, at + 6)), kind: LeaveKind::Break, target };
    let view = ControlView {
        constructs: vec![outer, inner],
        leaves: vec![jump(40, LeaveTarget::Label(0)), jump(60, LeaveTarget::Levels(2)), jump(70, LeaveTarget::Label(1))],
        labels: vec!["OUTER".into(), "NONE".into()],
        gates: Vec::new(),
        bodies: Vec::new(),
        returns: Vec::new(),
    };
    let plan = JoinPlan::new(&view);
    let exits = joined(&plan, &view.constructs[0], JoinRole::Exit);
    assert_eq!(exits, vec![Point::new(0, 10), Point::new(0, 40), Point::new(0, 60)], "the head, then both jumps aimed at it");
    let inner_exits = joined(&plan, &view.constructs[1], JoinRole::Exit);
    assert_eq!(inner_exits, vec![Point::new(0, 30)], "none aims at the inner loop");
}
