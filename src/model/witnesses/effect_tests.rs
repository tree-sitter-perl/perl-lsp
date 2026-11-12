//! How a callable ends: the union of what its sources state, from the
//! highest-priority source, and the top when nothing does.

use super::*;

fn at(row: usize) -> Point {
    Point { row, column: 0 }
}

fn push(bag: &mut WitnessBag, attachment: WitnessAttachment, source: WitnessSource, payload: WitnessPayload) {
    bag.push(Witness { attachment, source, payload, span: Span { start: at(1), end: at(1) } });
}

fn ends(e: Ends) -> WitnessPayload {
    WitnessPayload::Effect(Effect::Ends(e))
}

fn builder() -> WitnessSource {
    WitnessSource::Builder("test".into())
}

#[test]
fn nothing_stated_is_the_top() {
    let bag = WitnessBag::new();
    assert_eq!(query_ends(&bag, &WitnessAttachment::SymbolEffects(SymbolId(0)), None), Ends::all());
}

#[test]
fn arms_join_by_union() {
    let mut bag = WitnessBag::new();
    let att = WitnessAttachment::SymbolEffects(SymbolId(0));
    push(&mut bag, att.clone(), builder(), ends(Ends::UNWINDS));
    push(&mut bag, att.clone(), builder(), ends(Ends::EXITS));
    assert_eq!(query_ends(&bag, &att, None), Ends::UNWINDS | Ends::EXITS);
}

#[test]
fn a_stated_source_outranks_inferred_arms() {
    let mut bag = WitnessBag::new();
    let att = WitnessAttachment::SymbolEffects(SymbolId(0));
    push(&mut bag, att.clone(), builder(), ends(Ends::COMPLETES));
    push(&mut bag, att.clone(), WitnessSource::Plugin("p".into()), ends(Ends::UNWINDS));
    assert_eq!(query_ends(&bag, &att, None), Ends::UNWINDS);
}

#[test]
fn a_class_member_reads_through_its_mirror() {
    let mut bag = WitnessBag::new();
    let sym = WitnessAttachment::SymbolEffects(SymbolId(3));
    push(&mut bag, sym.clone(), builder(), ends(Ends::EXITS));
    push(&mut bag, WitnessAttachment::effects_of("Foo", "bail"), builder(), WitnessPayload::Edge(sym));
    assert_eq!(query_ends(&bag, &WitnessAttachment::effects_of("Foo", "bail"), None), Ends::EXITS);
}

/// A type question asked of an expression never answers with its effect:
/// one that may complete answers its value, one that cannot answers `Never`.
#[test]
fn an_expression_effect_never_answers_its_type() {
    let mut bag = WitnessBag::new();
    let may = WitnessAttachment::Expr(Span { start: at(1), end: at(2) });
    let cannot = WitnessAttachment::Expr(Span { start: at(3), end: at(4) });
    push(&mut bag, may.clone(), builder(), ends(Ends::COMPLETES | Ends::UNWINDS));
    push(&mut bag, cannot.clone(), builder(), ends(Ends::UNWINDS));
    let q = |attachment| ReducerQuery {
        attachment,
        point: None,
        framework: FrameworkFact::Plain,
        arity_hint: None,
        receiver: None,
        args: Vec::new(),
        context: None,
    };
    assert_eq!(ReducerRegistry::with_defaults().query(&bag, &q(&may)), ReducedValue::None);
    assert_eq!(ReducerRegistry::with_defaults().query(&bag, &q(&cannot)), ReducedValue::Type(InferredType::Never));
    push(&mut bag, may.clone(), builder(), WitnessPayload::InferredType(InferredType::String));
    push(&mut bag, cannot.clone(), builder(), WitnessPayload::InferredType(InferredType::String));
    assert_eq!(ReducerRegistry::with_defaults().query(&bag, &q(&may)), ReducedValue::Type(InferredType::String));
    assert_eq!(ReducerRegistry::with_defaults().query(&bag, &q(&cannot)), ReducedValue::Type(InferredType::Never));
}

#[test]
fn ends_ride_the_wire_as_their_bits() {
    let e = Ends::UNWINDS | Ends::EXITS;
    let bytes = bincode::serialize(&e).unwrap();
    assert_eq!(bytes, vec![e.bits()]);
    assert_eq!(bincode::deserialize::<Ends>(&bytes).unwrap(), e);
}
