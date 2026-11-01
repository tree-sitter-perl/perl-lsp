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

fn handler(id: u32, name: &str, owner: HandlerOwner) -> Symbol {
    let at = Span { start: Point { row: id as usize, column: 0 }, end: Point { row: id as usize, column: 1 } };
    Symbol {
        id: SymbolId(id),
        name: name.into(),
        kind: SymKind::Handler,
        span: at,
        selection_span: at,
        scope: ScopeId(0),
        package: None,
        detail: SymbolDetail::Handler { owner, dispatchers: vec!["emit".into()], params: vec![] },
        namespace: Namespace::framework("p".to_string()),
        presentation: Default::default(),
        attributes: Vec::new(),
        flags: Default::default(),
        declared_with: None,
        deref_stack: Vec::new(),
        arity: None,
        arm: ArmId::NONE,
    }
}

#[test]
fn owning_a_handler_bridges_it_to_the_owner() {
    let symbols = vec![
        handler(0, "ready", HandlerOwner::Class("App".into())),
        handler(1, "tick", HandlerOwner::Class("Clock".into())),
        handler(2, "home", HandlerOwner::Rail("route".into())),
    ];
    // `tick` is also a namespace entity bridged to its owner: still one visit.
    let namespaces = vec![PluginNamespace {
        id: "clock".into(),
        plugin_id: "p".into(),
        kind: "emitter".into(),
        entities: vec![SymbolId(1)],
        bridges: vec![Bridge::Class("Clock".into())],
        decl_span: symbols[1].span,
    }];
    let mut classes: Vec<&str> = bridge_classes(&symbols, &namespaces).collect();
    classes.sort_unstable();
    classes.dedup();
    assert_eq!(classes, ["App", "Clock"], "a rail-owned handler bridges nothing");

    let visited = |class: &str| {
        let mut names = Vec::new();
        let _ = for_each_entity_bridged_to(&symbols, &namespaces, class, |s| {
            names.push(s.name.clone());
            std::ops::ControlFlow::Continue(())
        });
        names
    };
    assert_eq!(visited("App"), ["ready"], "no namespace needed");
    assert_eq!(visited("Clock"), ["tick"], "owned and bridged: visited once");
    assert!(visited("route").is_empty());
}
