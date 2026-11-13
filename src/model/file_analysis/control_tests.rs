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
        arms: arms.iter().map(|&(span, holds)| Arm { span, role: ArmRole::When(holds) }).collect(),
    }
}

fn after(plan: &JoinPlan<'_>, c: &Construct) -> Vec<JoinArm> {
    let joins = plan.branch_joins(c, None, sp((0, 0), (99, 0)));
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
        leaves: vec![Leave { span: sp((0, 10), (0, 16)), kind: LeaveKind::Return }],
    };
    let plan = JoinPlan::new(&view);
    assert!(plan.drops_a_path(&view.constructs[0]));
    assert_eq!(after(&plan, &view.constructs[0]), vec![JoinArm { at: els.end, arm: Some(els), untaken: None }]);
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
    };
    let one = mk(vec![Leave { span: sp((0, 22), (0, 28)), kind: LeaveKind::Return }]);
    let plan = JoinPlan::new(&one);
    assert!(!plan.drops_a_path(&one.constructs[0]), "the inner else still comes out");
    let both = mk(vec![
        Leave { span: sp((0, 22), (0, 28)), kind: LeaveKind::Return },
        Leave { span: sp((0, 42), (0, 48)), kind: LeaveKind::Throw },
    ]);
    let plan = JoinPlan::new(&both);
    assert!(plan.drops_a_path(&both.constructs[0]));
    let arms = after(&plan, &both.constructs[0]);
    assert_eq!(arms.len(), 1, "only the untaken path comes out: {arms:?}");
    assert_eq!(arms[0].untaken, Some(Untaken { of: both.constructs[0].span, tested: Point::new(0, 5) }));
}
