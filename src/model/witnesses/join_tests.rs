//! A read past a join folds what the place holds at the end of every arm,
//! each arm read at its own site.

use super::*;
use crate::model::file_analysis::{Binding, PackageFacts, Place, Scope, ScopeId, ScopeKind};

fn at(row: usize, column: usize) -> Point {
    Point { row, column }
}

fn point_span(p: Point) -> Span {
    Span { start: p, end: p }
}

fn place() -> Place {
    Place::variable(Binding::Package("main".into(), "$x".into()))
}

fn push(bag: &mut WitnessBag, attachment: WitnessAttachment, payload: WitnessPayload, at: Point) {
    bag.push(Witness { attachment, source: WitnessSource::Builder("test".into()), payload, span: point_span(at) });
}

/// `$x = Foo->new;  if (c) { $x = Bar->new }  ...` with the join the
/// `if` mints at its end, at (4,0). The taken arm leaves at (3,9), the
/// bypass at the end of the condition, (2,5).
fn bag_with_join(taken_arm: Option<InferredType>) -> WitnessBag {
    let mut bag = WitnessBag::new();
    let p = WitnessAttachment::Place(Box::new(place()));
    push(&mut bag, p.clone(), WitnessPayload::Reset, at(1, 0));
    push(&mut bag, p.clone(), WitnessPayload::InferredType(InferredType::ClassName("Foo".into())), at(1, 0));
    push(&mut bag, p.clone(), WitnessPayload::Reset, at(3, 0));
    if let Some(t) = taken_arm {
        push(&mut bag, p.clone(), WitnessPayload::InferredType(t), at(3, 0));
    }
    let join = WitnessAttachment::Join {
        place: Box::new(place()),
        of: Span { start: at(2, 0), end: at(4, 0) },
        role: JoinRole::After,
    };
    push(&mut bag, p.clone(), WitnessPayload::Reset, at(4, 0));
    push(&mut bag, p.clone(), WitnessPayload::Edge(join.clone()), at(4, 0));
    push(&mut bag, join.clone(), WitnessPayload::Edge(p.clone()), at(3, 9));
    push(&mut bag, join, WitnessPayload::Edge(p), at(2, 5));
    bag
}

fn read(bag: &WitnessBag, point: Point) -> ReducedValue {
    let scopes = vec![Scope {
        id: ScopeId(0),
        parent: None,
        kind: ScopeKind::File,
        span: Span { start: at(0, 0), end: at(9, 0) },
        package: Some("main".into()),
        owner: None,
        implicit_receiver: false,
        runs_on: None,
        closed: false,
        captures: Vec::new(),
        direct: false,
        hints: Vec::new(),
    }];
    let packages: std::collections::HashMap<String, PackageFacts> = Default::default();
    let ctx = BagContext {
        scopes: &scopes,
        symbols: &[],
        package_framework: &packages,
        module_index: None,
        package_parents: &packages,
        app_surface_consumers: &[],
        lattice: &crate::model::conventions::PERL_CONVENTIONS.lattice,
        member_spaces: &crate::model::file_analysis::MemberSpaces::PER_FAMILY,
    };
    let att = WitnessAttachment::Place(Box::new(place()));
    let q = ReducerQuery {
        attachment: &att,
        point: Some(point),
        framework: FrameworkFact::Plain,
        arity_hint: None,
        receiver: None,
        args: Vec::new(),
        context: Some(&ctx),
    };
    ReducerRegistry::with_defaults().query(bag, &q)
}

#[test]
fn a_read_past_a_join_is_either_arm() {
    let bag = bag_with_join(Some(InferredType::ClassName("Bar".into())));
    let foo = InferredType::ClassName("Foo".into());
    let bar = InferredType::ClassName("Bar".into());
    let either = crate::model::file_analysis::join(&bar, &foo, &crate::model::file_analysis::Unordered);
    assert!(matches!(either, InferredType::Union(_)), "{either:?}");
    assert_eq!(read(&bag, at(5, 0)), ReducedValue::Type(either));
}

#[test]
fn reads_before_the_join_see_their_own_path() {
    let bag = bag_with_join(Some(InferredType::ClassName("Bar".into())));
    assert_eq!(read(&bag, at(2, 1)), ReducedValue::Type(InferredType::ClassName("Foo".into())));
    assert_eq!(read(&bag, at(3, 5)), ReducedValue::Type(InferredType::ClassName("Bar".into())));
}

#[test]
fn an_arm_nothing_types_leaves_the_join_untyped() {
    let bag = bag_with_join(None);
    assert_eq!(read(&bag, at(5, 0)), ReducedValue::Type(InferredType::Unknown));
}

#[test]
fn arms_that_agree_need_no_union() {
    let bag = bag_with_join(Some(InferredType::ClassName("Foo".into())));
    assert_eq!(read(&bag, at(5, 0)), ReducedValue::Type(InferredType::ClassName("Foo".into())));
}
