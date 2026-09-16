use super::*;

#[test]
fn a_rail_denotes_strings_unless_its_document_names_classes() {
    let mut pack = PackFacts::default();
    pack.class_named_rails = vec!["event".into()];
    assert_eq!(HandlerOwner::Rail("event".into()).names_are(&pack), RailNames::Classes);
    assert_eq!(HandlerOwner::Rail("route".into()).names_are(&pack), RailNames::Strings);
    // A class-owned handler's name is an event string whatever the rails say.
    assert_eq!(HandlerOwner::Class("App".into()).names_are(&pack), RailNames::Strings);
    // No declarations at all: every rail is a string rail.
    assert!(!HandlerOwner::Rail("event".into()).names_are_classes(&PackFacts::default()));
}
