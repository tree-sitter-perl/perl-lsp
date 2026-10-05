use super::*;

fn build(src: &str) -> (tree_sitter::Tree, FileAnalysis) {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    (tree, analysis)
}

/// The `expected_type()` lock: `ArgPosition` resolves the param type at a
/// call arg through the same witness-bag path sig-help's own param-type
/// rendering uses (`docs/adr/cursor-slots.md`'s stub — consumed by
/// nothing, this test is the guard against silent drift).
#[test]
fn expected_type_resolves_local_param_type_at_call_arg() {
    let src = r#"package Foo;
sub bar {
    my ($self, $x) = @_;
    $x = 5;
    return $x;
}

Foo->bar(1);
"#;
    let (_tree, analysis) = build(src);
    let callee = CalleeCtx {
        name: "bar".to_string(),
        is_method: true,
        invocant: Some("Foo".to_string()),
        active_param: 0,
        at_key_position: false,
        used_keys: Default::default(),
        first_arg_string: None,
    };
    let slot = Slot::ArgPosition { callee: Some(callee), index: 0, expected: None };
    let point = Point::new(0, 0);
    let ty = slot.expected_type(&analysis, point, None);
    assert_eq!(ty, Some(InferredType::Numeric), "param $x's last assignment types it Numeric: {:?}", ty);
}

/// Every other slot answers `None` — the stub has no other consumer yet.
#[test]
fn expected_type_is_none_off_arg_position() {
    let src = "package Foo;\nsub bar { 1 }\n";
    let (_tree, analysis) = build(src);
    let slot = Slot::Identifier { prefix: "fo".to_string() };
    assert_eq!(slot.expected_type(&analysis, Point::new(0, 0), None), None);

    let slot = Slot::ArgPosition { callee: None, index: 0, expected: None };
    assert_eq!(slot.expected_type(&analysis, Point::new(0, 0), None), None);
}

/// A detector that resolved the type eagerly (the pack domain-comparison
/// slot) carries it on `expected`; `expected_type` returns it verbatim
/// without touching a callee — the seam's second producer.
#[test]
fn expected_type_returns_carried_domain() {
    let src = "package Foo;\nsub bar { 1 }\n";
    let (_tree, analysis) = build(src);
    let dom = InferredType::ClassName("opcode".to_string());
    let slot = Slot::ArgPosition { callee: None, index: 0, expected: Some(dom.clone()) };
    assert_eq!(slot.expected_type(&analysis, Point::new(0, 0), None), Some(dom));
}

/// `Slot::sigil` decodes the bare-sigil-trigger fact back out of an
/// `Identifier` slot — the reconstruction `detect_slot`'s Perl fold
/// relies on to stay byte-identical with the `CursorContext::Variable`
/// branch it folds from.
#[test]
fn sigil_decodes_bare_trigger_only() {
    assert_eq!(Slot::Identifier { prefix: "$".into() }.sigil(), Some('$'));
    assert_eq!(Slot::Identifier { prefix: "@".into() }.sigil(), Some('@'));
    assert_eq!(Slot::Identifier { prefix: "%".into() }.sigil(), Some('%'));
    assert_eq!(Slot::Identifier { prefix: "".into() }.sigil(), None);
    assert_eq!(Slot::Identifier { prefix: "foo".into() }.sigil(), None);
    assert_eq!(Slot::Identifier { prefix: "$x".into() }.sigil(), None);
    assert_eq!(
        Slot::Key { owner: OwnerCtx { owner_type: None, var_text: String::new(), source_sub: None, owner: None } }
            .sigil(),
        None
    );
}

/// `detect_slot` on Perl reproduces the exact `CursorContext` verdicts:
/// method position after `->`.
#[test]
fn detect_slot_perl_method_position() {
    let src = "package Foo;\nsub greet { 1 }\npackage main;\nmy $f = bless {}, 'Foo';\n$f->\n";
    let (tree, analysis) = build(src);
    let point = Point::new(4, 4); // right after `$f->`
    let slot = detect_slot(&analysis, &tree, src, point, "perl", None).slot;
    match slot {
        Slot::Member { receiver, op } => {
            assert_eq!(op, MemberOp::Arrow);
            assert_eq!(receiver.receiver_text.as_deref(), Some("$f"));
        }
        other => panic!("expected Member slot, got {:?}", other),
    }
}

/// H7-10(b): a cursor INSIDE a string argument to a method call
/// (`$schema->resultset('Arti|st')`) is not a member-access slot — the
/// method token is fully typed and the cursor sits past it, in the arg
/// list. Pre-fix the loose "cursor after invocant" check reported a bogus
/// `$schema->` Member slot, dumping method candidates into the string.
#[test]
fn detect_slot_string_arg_of_method_call_is_not_member() {
    let src = "package main;\nmy $x = $schema->resultset('Artist');\n";
    let (tree, analysis) = build(src);
    let line = src.lines().nth(1).unwrap();
    let col = line.find("Artist").unwrap() + 2; // inside 'Artist'
    let point = Point::new(1, col);
    let slot = detect_slot(&analysis, &tree, src, point, "perl", None).slot;
    assert!(
        !matches!(slot, Slot::Member { .. }),
        "cursor inside a string call-arg must not be a Member slot, got {:?}",
        slot
    );
}

/// `use |` (typing the module name) is a `ModulePath` slot on the
/// `UseModule` detector arm — the arm, not a local bool, distinguishes it
/// from the qualified-path drill (`docs/forks-resolved.md`).
#[test]
fn detect_slot_perl_use_module_name_is_module_path() {
    use crate::lsp::cursor_slot::DetectorArm;
    let src = "use Sc\n";
    let (tree, analysis) = build(src);
    let point = Point::new(0, 6);
    let detected = detect_slot(&analysis, &tree, src, point, "perl", None);
    assert_eq!(detected.arm, DetectorArm::UseModule);
    match detected.slot {
        Slot::ModulePath { prefix } => assert_eq!(prefix, "Sc"),
        other => panic!("expected ModulePath slot, got {:?}", other),
    }
}

/// A pack-language `::`-qualified cursor (`fmtx::f|`) is a `ModulePath`
/// slot naming the qualifier as owner — the completion consumer gathers
/// that owner's members instead of the global pool. Detection is
/// `resolve::qualifier_at_point`, the same anchor goto-def resolves
/// through.
#[cfg(feature = "cpp")]
#[test]
fn detect_slot_cpp_qualified_path_is_module_path() {
    let src = "namespace fmtx { void format_to(int); }\nvoid caller() {\n    fmtx::f\n}\n";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
    let analysis = reg.for_id("cpp").unwrap().analyze_with_path(src, None);

    use crate::lsp::cursor_slot::DetectorArm;
    // Mid-token: `fmtx::f|`.
    let detected = detect_slot(&analysis, &tree, src, Point::new(2, 11), "cpp", None);
    assert_eq!(detected.arm, DetectorArm::QualifiedPath);
    match detected.slot {
        Slot::ModulePath { prefix } => assert_eq!(prefix, "fmtx"),
        other => panic!("expected ModulePath slot, got {:?}", other),
    }

    // Bare qualifier: `fmtx::|` (nothing typed after the colons yet).
    let src2 = "namespace fmtx { void format_to(int); }\nvoid caller() {\n    fmtx::\n}\n";
    let tree2 = parser.parse(src2, None).unwrap();
    let analysis2 = reg.for_id("cpp").unwrap().analyze_with_path(src2, None);
    let detected = detect_slot(&analysis2, &tree2, src2, Point::new(2, 10), "cpp", None);
    assert_eq!(detected.arm, DetectorArm::QualifiedPath);
    match detected.slot {
        Slot::ModulePath { prefix } => assert_eq!(prefix, "fmtx"),
        other => panic!("expected ModulePath slot, got {:?}", other),
    }
}
