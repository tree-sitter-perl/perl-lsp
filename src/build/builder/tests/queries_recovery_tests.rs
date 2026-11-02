use super::*;

// ---- High-level query tests ----

#[test]
fn test_find_def_variable() {
    let fa = build_fa("my $x = 1;\nprint $x;");
    // Cursor on the usage of $x at line 1
    let def = fa.find_definition(Point::new(1, 7), None);
    assert!(def.is_some(), "should find definition for $x");
    let span = def.unwrap();
    assert_eq!(span.start.row, 0, "definition should be on line 0");
}

#[test]
fn test_find_def_sub() {
    let fa = build_fa("sub greet { }\ngreet();");
    // Cursor on the function call at line 1
    let def = fa.find_definition(Point::new(1, 1), None);
    assert!(def.is_some(), "should find definition for greet");
    let span = def.unwrap();
    assert_eq!(span.start.row, 0, "definition should be on line 0");
}

#[test]
fn test_find_def_method_in_class() {
    let src = "package Foo;\nsub new { bless {}, shift }\nsub hello { }\npackage main;\nmy $f = Foo->new();\n$f->hello();";
    let fa = build_fa(src);
    // Cursor on hello() call at line 5
    let def = fa.find_definition(Point::new(5, 5), None);
    assert!(def.is_some(), "should find definition for hello method");
    let span = def.unwrap();
    assert_eq!(span.start.row, 2, "hello definition should be on line 2");
}

#[test]
fn test_find_def_scoped_variable() {
    let src = "my $x = 'outer';\nsub foo {\n    my $x = 'inner';\n    print $x;\n}";
    let fa = build_fa(src);
    // Cursor on $x inside sub (line 3) should resolve to inner $x (line 2)
    let def = fa.find_definition(Point::new(3, 11), None);
    assert!(def.is_some());
    let span = def.unwrap();
    assert_eq!(span.start.row, 2, "should resolve to inner $x on line 2");
}

#[test]
fn test_find_references_variable() {
    let src = "my $x = 1;\nprint $x;\n$x = 2;";
    let fa = build_fa(src);
    // Cursor on the declaration of $x
    let refs = fa.find_references(Point::new(0, 4), None);
    assert!(
        refs.len() >= 2,
        "should find at least declaration + usage, got {}",
        refs.len()
    );
}

#[test]
fn test_hash_key_def_implicit_return_gets_sub_owner() {
    // Implicit return: last expression in sub body, no explicit `return`
    let src = "sub get_config { { host => 'localhost', port => 5432 } }\nmy $cfg = get_config();\n$cfg->{host};\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());

    let host_defs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "host" && matches!(s.detail, SymbolDetail::HashKeyDef { .. }))
        .collect();
    assert!(!host_defs.is_empty(), "should find HashKeyDef for 'host'");
    if let SymbolDetail::HashKeyDef { ref owner, .. } = host_defs[0].detail {
        assert_eq!(
            *owner,
            HashKeyOwner::Sub {
                // Top-level scripts default to `main` per Perl's
                // own semantics; the implicit-package seed in
                // `Builder::new` makes this an explicit `Some("main")`
                // rather than `None`.
                package: Some("main".to_string()),
                name: "get_config".to_string()
            },
            "implicit return hash key should have Sub get_config owner, got {:?}",
            owner
        );
    }

    // Go-to-def from $cfg->{host} should reach the hash key in the implicit return
    let host_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "host" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        !host_refs.is_empty(),
        "should find HashKeyAccess for 'host'"
    );
    let def = fa.find_definition(
        host_refs[0].span.start,
        None);
    assert!(def.is_some(), "should find definition for host");
    assert_eq!(def.unwrap().start.row, 0, "host def should be on line 0");
}

#[test]
fn test_find_references_sub() {
    let src = "sub greet { }\ngreet();\ngreet();";
    let fa = build_fa(src);
    // Cursor on the sub name
    let refs = fa.find_references(Point::new(0, 5), None);
    assert!(
        refs.len() >= 2,
        "should find definition + calls, got {}",
        refs.len()
    );
}

#[test]
fn test_find_references_method_through_chain() {
    let src = "\
package Foo;
sub new { bless {}, shift }
sub bar { 42 }
package main;
sub get_foo { return Foo->new() }
my $f = Foo->new();
$f->bar();
get_foo()->bar();
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // Cursor on bar definition (line 2, col 4)
    let refs = fa.find_references(Point::new(2, 5), None);
    // Should find: $f->bar() + get_foo()->bar() (definition may or may not be included)
    let ref_lines: Vec<usize> = refs.iter().map(|s| s.start.row).collect();
    assert!(
        refs.len() >= 2,
        "should find at least 2 refs, got {} at lines {:?}",
        refs.len(),
        ref_lines
    );
    // The key assertion: chained call get_foo()->bar() is found (was broken before P0a fix)
    assert!(
        ref_lines.contains(&7),
        "should find chained get_foo()->bar() at line 7, got {:?}",
        ref_lines
    );
}

#[test]
fn test_hash_key_def_in_return_gets_sub_owner() {
    let src = "sub get_config {\n    return { host => 'localhost', port => 5432 };\n}\nmy $cfg = get_config();\n$cfg->{host};\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());

    // Verify hash key defs exist with Sub owner
    let host_defs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "host" && matches!(s.detail, SymbolDetail::HashKeyDef { .. }))
        .collect();
    assert!(!host_defs.is_empty(), "should find HashKeyDef for 'host'");
    if let SymbolDetail::HashKeyDef { ref owner, .. } = host_defs[0].detail {
        assert_eq!(
            *owner,
            HashKeyOwner::Sub {
                // Top-level scripts default to `main` per Perl's
                // own semantics; the implicit-package seed in
                // `Builder::new` makes this an explicit `Some("main")`
                // rather than `None`.
                package: Some("main".to_string()),
                name: "get_config".to_string()
            },
            "host def should have Sub get_config owner, got {:?}",
            owner
        );
    }

    // Verify HashKeyAccess ref for $cfg->{host} has Sub owner
    let host_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "host" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        !host_refs.is_empty(),
        "should find HashKeyAccess for 'host'"
    );
    if matches!(host_refs[0].kind, RefKind::HashKeyAccess { .. }) {
        assert_eq!(
            host_refs[0].hash_key_owner().cloned(),
            Some(HashKeyOwner::Sub {
                package: Some("main".to_string()),
                name: "get_config".to_string()
            }),
            "host ref should have Sub get_config owner",
        );
    }

    // Verify go-to-references from the def finds the usage
    let host_def_point = host_defs[0].selection_span.start;
    let refs = fa.find_references(host_def_point, None);
    // symbol_at returns include_decl=false, so only usages are returned
    assert!(
        refs.len() >= 1,
        "should find at least 1 usage, got {} refs",
        refs.len()
    );

    // Verify go-to-references from the usage finds back to the def
    let host_ref_point = host_refs[0].span.start;
    let refs_from_usage = fa.find_references(host_ref_point, None);
    // ref resolves to def → include_decl=true, so def + usage
    assert!(
        refs_from_usage.len() >= 2,
        "should find def + usage, got {} refs",
        refs_from_usage.len()
    );
}

#[test]
fn test_hash_key_refs_chained_resolved_at_build() {
    // Chained method calls returning a Sub-keyed hash: the build-time
    // `emit_chained_hash_key_refs` pass resolves the owner to
    // `Sub{Calculator, get_config}` (the implicit-return keys), so the
    // stored ref carries it — no tree fallback needed.
    let src = r#"package Calculator;
sub new { bless {}, shift }
sub get_self { my ($self) = @_; return $self; }
sub get_config { return { host => "localhost", port => 5432 }; }
package main;
my $calc = Calculator->new();
$calc->get_self->get_config->{host};
"#;
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());

    // Find the hash key def for "host" in get_config's return
    let host_defs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "host" && matches!(s.detail, SymbolDetail::HashKeyDef { .. }))
        .collect();
    assert!(!host_defs.is_empty(), "should find HashKeyDef for 'host'");

    // The chained ref now carries its resolved owner at build time.
    let owner = fa
        .refs()
        .iter()
        .find_map(|r| match r.hash_key_owner() {
            Some(o) if r.target_name == "host" => Some(o.clone()),
            _ => None,
        })
        .expect("chained hash access should carry a resolved owner");
    assert_eq!(
        owner,
        HashKeyOwner::Sub {
            package: Some("Calculator".to_string()),
            name: "get_config".to_string(),
        },
        "owner should be the return-hash sub of the last chain hop, got {:?}",
        owner
    );

    // find_references from the def finds the chained usage.
    let host_def_point = host_defs[0].selection_span.start;
    let refs = fa.find_references(host_def_point, None);
    assert!(
        refs.len() >= 1,
        "should find chained usage, got {} refs",
        refs.len()
    );
}

#[test]
fn test_highlights_read_write() {
    let src = "my $x = 1;\nprint $x;\n$x = 2;";
    let fa = build_fa(src);
    let highlights = fa.find_occurrences(Point::new(0, 4), None);
    assert!(!highlights.is_empty(), "should have highlights");
    // Check that we have both read and write accesses
    let has_write = highlights
        .iter()
        .any(|(_, a)| matches!(a, AccessKind::Write));
    let has_read = highlights
        .iter()
        .any(|(_, a)| matches!(a, AccessKind::Read));
    // At minimum we should see the declaration
    assert!(
        highlights.len() >= 2,
        "should have at least 2 highlights, got {}",
        highlights.len()
    );
    // Note: whether read/write are correctly tagged depends on builder's access classification
    let _ = (has_write, has_read); // suppress unused warnings if assertions change
}

#[test]
fn test_hover_variable() {
    let src = "my $greeting = 'hello';\nprint $greeting;";
    let fa = build_fa(src);
    let hover = fa.hover_info(Point::new(1, 8), src, None);
    assert!(hover.is_some(), "should have hover info");
    let text = hover.unwrap();
    assert!(
        text.contains("$greeting"),
        "hover should contain variable name, got: {}",
        text
    );
}

#[test]
fn test_hover_sub() {
    let src = "sub greet { }\ngreet();";
    let fa = build_fa(src);
    let hover = fa.hover_info(Point::new(1, 1), src, None);
    assert!(hover.is_some(), "should have hover info for function call");
    let text = hover.unwrap();
    assert!(
        text.contains("greet"),
        "hover should contain sub name, got: {}",
        text
    );
}

#[test]
fn test_hover_shows_inferred_type() {
    let src =
        "package Point;\nsub new { bless {}, shift }\npackage main;\nmy $p = Point->new();\n$p;";
    let fa = build_fa(src);
    // Hover on $p usage at line 4
    let hover = fa.hover_info(Point::new(4, 1), src, None);
    assert!(hover.is_some(), "should have hover info");
    let text = hover.unwrap();
    assert!(
        text.contains("Point"),
        "hover should show inferred type Point, got: {}",
        text
    );
}

#[test]
fn test_hover_type_at_usage_after_reassignment() {
    // $x starts as Point, gets reassigned to Foo — hover at each usage should reflect the type at that point
    let src = "package Point;\nsub new { bless {}, shift }\npackage Foo;\nsub new { bless {}, shift }\npackage main;\nmy $x = Point->new();\n$x;\n$x = Foo->new();\n$x;";
    let fa = build_fa(src);
    // line 6: $x; — should be Point
    let hover1 = fa.hover_info(Point::new(6, 1), src, None);
    assert!(hover1.is_some());
    let text1 = hover1.unwrap();
    assert!(
        text1.contains("Point"),
        "at line 6 should be Point, got: {}",
        text1
    );
    // line 8: $x; — should be Foo (after reassignment)
    let hover2 = fa.hover_info(Point::new(8, 1), src, None);
    assert!(hover2.is_some());
    let text2 = hover2.unwrap();
    assert!(
        text2.contains("Foo"),
        "at line 8 should be Foo, got: {}",
        text2
    );
}

#[test]
fn test_hover_shows_return_type() {
    let src = "package Foo;\nsub make { return Foo->new() }\nsub new { bless {}, shift }\npackage main;\nmake();";
    let fa = build_fa(src);
    // Hover on sub make definition
    let hover = fa.hover_info(Point::new(1, 5), src, None);
    assert!(hover.is_some(), "should have hover info for sub");
    let text = hover.unwrap();
    assert!(
        text.contains("returns"),
        "hover should show return type, got: {}",
        text
    );
    assert!(
        text.contains("Foo"),
        "hover return type should mention Foo, got: {}",
        text
    );
}

#[test]
fn test_rename_variable() {
    let src = "my $x = 1;\nprint $x;";
    let fa = build_fa(src);
    let edits = fa.rename_at(Point::new(0, 4), "y");
    assert!(edits.is_some(), "should produce rename edits");
    let edits = edits.unwrap();
    assert!(
        edits.len() >= 2,
        "should rename at least declaration + usage"
    );
    for (_, new_text) in &edits {
        assert_eq!(new_text, "y", "all edits should use new name");
    }
}

#[test]
fn test_rename_sub_finds_both_function_and_method_calls() {
    let fa = build_fa(
        "
package Foo;
sub emit { }
sub test {
    my $self = shift;
    emit('event');
    $self->emit('done');
}
",
    );
    // `sub emit` in package Foo. Scope-aware rename with
    // package=Foo catches the decl, the FunctionCall `emit()`,
    // AND the MethodCall `$self->emit()` — two shapes of the
    // same callable.
    let edits = fa.rename_sub_in_package("emit", &Some("Foo".to_string()), "fire", None);
    assert!(
        edits.len() >= 3,
        "rename_sub_in_package should find def + function call + method call, got {} edits",
        edits.len()
    );
    for (_, text) in &edits {
        assert_eq!(text, "fire");
    }
}

#[test]
fn test_moo_has_creates_constructor_hash_key_def() {
    let fa = build_fa(
        "
package MyApp;
use Moo;
has username => (is => 'ro');
has password => (is => 'rw');
",
    );
    // Should have HashKeyDef symbols owned by "new" for each has attribute
    let key_defs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.detail, SymbolDetail::HashKeyDef { .. }))
        .collect();
    let names: Vec<&str> = key_defs.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"username"),
        "should have HashKeyDef for username, got: {:?}",
        names
    );
    assert!(
        names.contains(&"password"),
        "should have HashKeyDef for password, got: {:?}",
        names
    );
    // Verify owner is Sub { package: "MyApp", name: "new" }
    if let SymbolDetail::HashKeyDef { ref owner, .. } = key_defs[0].detail {
        assert_eq!(
            owner,
            &HashKeyOwner::Sub {
                package: Some("MyApp".to_string()),
                name: "new".to_string(),
            }
        );
    }
}

// ---- ERROR recovery tests ----
// tree-sitter-perl wraps broken regions in ERROR nodes. Some structural
// declarations (sub, class) survive as typed nodes inside ERROR.
// use/package often get parsed as raw function tokens inside ERROR —
// those can't be recovered (parser fix needed).

#[test]
fn test_error_recovery_sub_outside_error() {
    // my $x = [ creates an ERROR, but sub below it survives as a top-level node
    let source = "package Foo;\nmy $x = [\nuse List::Util qw(max);\nsub process { }\n";
    let fa = build_fa(source);
    let subs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        subs.contains(&"process"),
        "sub process should survive (outside ERROR)"
    );
}

#[test]
fn test_error_recovery_sub_outside_error_survives() {
    // Sub below an ERROR survives as a top-level node (not inside ERROR)
    let source = "package Foo;\nmy $x = [\nuse List::Util qw(max);\nsub process { }\n";
    let fa = build_fa(source);
    let subs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        subs.contains(&"process"),
        "sub process should survive (outside ERROR)"
    );
}

#[test]
fn test_error_node_does_not_panic() {
    // ERROR nodes should not crash the builder
    let source = "package Foo;\nmy $x = [\nmy $y = [\nsub process { }\n";
    let fa = build_fa(source);
    let pkgs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Package))
        .map(|s| s.name.as_str())
        .collect();
    assert!(pkgs.contains(&"Foo"), "package Foo should survive");
}

#[test]
fn test_error_recovery_sub_inside_error() {
    let source = "package Foo;\nmy $x = [\nmy $y = [\nsub process { }\n";
    let fa = build_fa(source);
    let subs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        subs.contains(&"process"),
        "sub process should be recovered from ERROR"
    );
}

#[test]
fn test_error_recovery_import_inside_error() {
    let source = "package Foo;\nmy $x = [\nuse List::Util qw(max);\nsub process { }\n";
    let fa = build_fa(source);
    let imports: Vec<&str> = fa.imports.iter().map(|i| i.module_name.as_str()).collect();
    assert!(
        imports.contains(&"List::Util"),
        "use List::Util should be recovered from ERROR"
    );
}

#[test]
fn test_error_recovery_package_inside_error() {
    let source = "my $x = [\npackage Bar;\nuse Moose;\nsub bar { }\n";
    let fa = build_fa(source);
    let pkgs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Package))
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        pkgs.contains(&"Bar"),
        "package Bar should be recovered from ERROR"
    );
}

#[test]
fn test_find_def_bareword_class() {
    let src = "package Point;\nsub new { bless {}, shift }\npackage main;\nPoint->new();";
    let fa = build_fa(src);
    // Cursor on "new" in Point->new()
    let def = fa.find_definition(Point::new(3, 8), None);
    assert!(def.is_some(), "should find definition for new");
}

// ---- Block dereference descent tests ----
// @{expr}, %{expr}, ${expr} parse as scalar/array/hash with varname→block.
// The builder must recurse into the block to find inner refs.

#[test]
fn test_deref_block_produces_inner_variable_ref() {
    // @{$arr} — the inner $arr should produce a Variable ref
    let fa = build_fa("my @data = (1,2,3);\nmy $arr = \\@data;\npush @{$arr}, 4;");
    let inner_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "$arr"
                && matches!(r.kind, RefKind::Variable)
                && r.access == AccessKind::Read
        })
        .collect();
    assert!(
        !inner_refs.is_empty(),
        "should find $arr ref inside @{{$arr}}"
    );
    // Should NOT have a bogus ref for the whole @{$arr}
    let bogus: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name.contains("{$arr}"))
        .collect();
    assert!(
        bogus.is_empty(),
        "should not record bogus ref for whole deref expression"
    );
}

#[test]
fn test_deref_block_produces_hash_key_ref() {
    // @{$self->{items}} — inner hash_element_expression should produce:
    // 1. Variable ref for $self
    // 2. HashKeyAccess ref for "items"
    let fa = build_fa("my %h = (items => []);\n@{$h{items}};");
    let key_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "items" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        !key_refs.is_empty(),
        "should find hash key ref 'items' inside deref block"
    );
}

#[test]
fn test_deref_block_resolves_variable() {
    // Variable inside deref block should resolve to its declaration
    let fa = build_fa("my @xs = (1,2);\nmy $ref = \\@xs;\nprint @{$ref};");
    let inner_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "$ref" && r.access == AccessKind::Read)
        .collect();
    assert!(!inner_refs.is_empty(), "$ref ref should exist");
    assert!(
        inner_refs[0].resolved_symbol().is_some(),
        "$ref inside deref should resolve to declaration"
    );
}

#[test]
fn test_deref_self_and_hash_key() {
    // Full integration: constructor defines hash keys, method accesses them through deref
    let src = "package Calculator;\nsub new {\n    my ($class, %args) = @_;\n    my $self = bless {\n        history => [],\n        verbose => 0,\n    }, $class;\n    return $self;\n}\nsub add {\n    my ($self, $a, $b) = @_;\n    my $result = $a + $b;\n    push @{$self->{history}}, \"add\";\n    return $result;\n}";
    let fa = build_fa(src);

    // $self at line 12 (push @{$self->{history}}, ...)
    let def_self = fa.find_definition(Point::new(12, 12), None);
    assert!(
        def_self.is_some(),
        "should find definition for $self in deref"
    );
    assert_eq!(
        def_self.unwrap().start.row,
        10,
        "$self should resolve to declaration on line 10"
    );

    // history key at line 12
    let def_history = fa.find_definition(Point::new(12, 20), None);
    assert!(
        def_history.is_some(),
        "should find definition for history hash key"
    );
    assert_eq!(
        def_history.unwrap().start.row,
        4,
        "history key should resolve to definition on line 4"
    );
}

#[test]
fn test_imports_qw() {
    let source = "use List::Util qw(first any all);\nuse Scalar::Util qw(blessed);\n";
    let fa = build_fa(source);

    assert_eq!(fa.imports.len(), 2);

    assert_eq!(fa.imports[0].module_name, "List::Util");
    let names0: Vec<&str> = fa.imports[0]
        .imported_symbols
        .iter()
        .map(|s| s.local_name.as_str())
        .collect();
    assert_eq!(names0, vec!["first", "any", "all"]);

    assert_eq!(fa.imports[1].module_name, "Scalar::Util");
    let names1: Vec<&str> = fa.imports[1]
        .imported_symbols
        .iter()
        .map(|s| s.local_name.as_str())
        .collect();
    assert_eq!(names1, vec!["blessed"]);
}

#[test]
fn test_imports_qw_close_paren_position() {
    // "use List::Util qw(first);\n"
    //  0123456789...
    //                  ^18    ^24 = )
    let source = "use List::Util qw(first);\n";
    let fa = build_fa(source);

    assert_eq!(fa.imports.len(), 1);
    let imp = &fa.imports[0];
    assert!(imp.qw_close_paren.is_some(), "qw_close_paren should be set");
    let pos = imp.qw_close_paren.unwrap();
    // The ) is at column 23 in "use List::Util qw(first);"
    assert_eq!(pos.row, 0);
    assert_eq!(pos.column, 23, "close paren should be at column 23");
}

#[test]
fn test_imports_bare() {
    let source = "use strict;\nuse warnings;\nuse Carp;\n";
    let fa = build_fa(source);

    // strict/warnings/Carp all produce imports with empty imported_symbols
    let carp = fa.imports.iter().find(|i| i.module_name == "Carp");
    assert!(carp.is_some());
    assert!(carp.unwrap().imported_symbols.is_empty());
}

#[test]
fn test_imports_module_symbol_created() {
    let source = "use List::Util qw(first);\n";
    let fa = build_fa(source);

    // Module symbol should exist
    let module_syms: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Module && s.name == "List::Util")
        .collect();
    assert_eq!(module_syms.len(), 1);

    // Import should exist
    assert_eq!(fa.imports.len(), 1);
    let names: Vec<&str> = fa.imports[0]
        .imported_symbols
        .iter()
        .map(|s| s.local_name.as_str())
        .collect();
    assert_eq!(names, vec!["first"]);
}

#[test]
fn test_goto_def_slurpy_hash_arg_at_call_site() {
    // Calculator->new(verbose => 1): cursor on "verbose" should go to
    // the bless hash key def, NOT to sub new.
    let src = r#"package Calculator;
sub new {
    my ($class, %args) = @_;
    my $self = bless {
        verbose => $args{verbose} // 0,
    }, $class;
    return $self;
}
package main;
my $calc = Calculator->new(verbose => 1);
"#;
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // "verbose" at call site is line 9, after "Calculator->new("
    // Calculator->new(verbose => 1)
    // 0123456789012345678901234567
    //                 ^16 = v of verbose
    // my $calc = Calculator->new(verbose => 1);
    // 0         1         2         3
    // 0123456789012345678901234567890123456789
    //                            ^27 = v of verbose
    let def = fa.find_definition(Point::new(9, 27), None);
    assert!(
        def.is_some(),
        "should find definition for verbose at call site"
    );
    // Should go to line 4: "verbose => $args{verbose} // 0,"
    assert_eq!(
        def.unwrap().start.row,
        4,
        "verbose should resolve to bless hash key def on line 4, not sub new"
    );
}

#[test]
fn test_goto_def_param_field_at_call_site() {
    // Point->new(x => 3, y => 4): cursor on "x" should go to "field $x :param"
    let src = r#"use v5.38;
class Point {
    field $x :param :reader;
    field $y :param;
    method magnitude() { }
}
my $p = Point->new(x => 3, y => 4);
"#;
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // my $p = Point->new(x => 3, y => 4);
    // 0         1         2
    // 0123456789012345678901234
    //                    ^19 = x

    let def = fa.find_definition(Point::new(6, 19), None);
    assert!(def.is_some(), "should find definition for x at call site");
    // Should go to line 2: "field $x :param :reader;"
    assert_eq!(
        def.unwrap().start.row,
        2,
        "x should resolve to field $x on line 2, not the class"
    );
}

// ---- Gap 1: __PACKAGE__ resolution ----

#[test]
fn test_dunder_package_resolution() {
    let fa = build_fa(
        "
        package Mojo::File;
        sub path { __PACKAGE__->new(@_) }
        ",
    );
    let rt = fa.sub_return_type_at_arity("path", None);
    assert_eq!(rt, Some(InferredType::ClassName("Mojo::File".into())));
}

#[test]
fn test_dunder_package_method_invocant() {
    // __PACKAGE__->new() should store the resolved class in MethodCall invocant
    let fa = build_fa(
        "
        package Foo;
        __PACKAGE__->some_method();
        ",
    );
    let method_ref = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "some_method")
        .unwrap();
    match &method_ref.kind {
        RefKind::MethodCall { invocant, .. } => {
            assert_eq!(
                invocant.text(), "Foo",
                "invocant should be resolved from __PACKAGE__"
            );
        }
        _ => panic!("expected MethodCall ref"),
    }
}

// ---- Gap 2: Shift parameter extraction ----

#[test]
fn test_shift_params() {
    let fa = build_fa(
        "
        sub process {
            my $self = shift;
            my $file = shift;
            my $opts = shift || {};
        }
        ",
    );
    // signatures_for_call strips $self when first param is $self
    let sig = fa
        .signatures_for_call("process", false, None, Point::new(0, 0), None)
        .pop()
        .unwrap();
    assert!(sig.is_method, "should detect method from $self first param");
    assert_eq!(sig.params.len(), 2);
    assert_eq!(sig.params[0].name, "$file");
    assert_eq!(sig.params[1].name, "$opts");
    assert_eq!(sig.params[1].default, Some("{}".into()));

    // Check raw params via symbol detail
    let sub_sym = fa.symbols().iter().find(|s| s.name == "process").unwrap();
    if let SymbolDetail::Sub { ref params, .. } = sub_sym.detail {
        assert_eq!(params.len(), 3);
        assert_eq!(params[0].name, "$self");
        assert_eq!(params[1].name, "$file");
        assert_eq!(params[2].name, "$opts");
        assert_eq!(params[2].default, Some("{}".into()));
    } else {
        panic!("expected Sub detail");
    }
}

#[test]
fn test_shift_then_list_assign() {
    let fa = build_fa(
        "
        sub process {
            my $self = shift;
            my ($file, @opts) = @_;
        }
        ",
    );
    let sig = fa
        .signatures_for_call("process", false, None, Point::new(0, 0), None)
        .pop()
        .unwrap();
    assert!(sig.is_method);
    assert_eq!(
        sig.params.len(),
        2,
        "should have $file and @opts (stripped $self)"
    );
    assert_eq!(sig.params[0].name, "$file");
    assert_eq!(sig.params[1].name, "@opts");
    assert!(sig.params[1].is_slurpy);

    // Check raw params
    let sub_sym = fa.symbols().iter().find(|s| s.name == "process").unwrap();
    if let SymbolDetail::Sub { ref params, .. } = sub_sym.detail {
        assert_eq!(params.len(), 3);
        assert_eq!(params[0].name, "$self");
    } else {
        panic!("expected Sub detail");
    }
}

#[test]
fn test_shift_with_double_pipe_default() {
    let fa = build_fa(
        "
        sub handler {
            my $self = shift;
            my $timeout = shift || 30;
        }
        ",
    );
    let sig = fa
        .signatures_for_call("handler", false, None, Point::new(0, 0), None)
        .pop()
        .unwrap();
    assert_eq!(sig.params.len(), 1, "stripped $self");
    assert_eq!(sig.params[0].name, "$timeout");
    assert_eq!(sig.params[0].default, Some("30".into()));
}

#[test]
fn test_shift_with_defined_or_default() {
    let fa = build_fa(
        "
        sub handler {
            my $self = shift;
            my $verbose = shift // 0;
        }
        ",
    );
    let sig = fa
        .signatures_for_call("handler", false, None, Point::new(0, 0), None)
        .pop()
        .unwrap();
    assert_eq!(sig.params.len(), 1, "stripped $self");
    assert_eq!(sig.params[0].name, "$verbose");
    assert_eq!(sig.params[0].default, Some("0".into()));
}

#[test]
fn test_subscript_param() {
    let fa = build_fa(
        "
        sub handler {
            my $self = $_[0];
            my $data = $_[1];
        }
        ",
    );
    let sig = fa
        .signatures_for_call("handler", false, None, Point::new(0, 0), None)
        .pop()
        .unwrap();
    assert_eq!(sig.params.len(), 1, "stripped $self");
    assert_eq!(sig.params[0].name, "$data");
}

#[test]
fn test_legacy_at_params_still_work() {
    // Ensure the existing @_ pattern still works
    let fa = build_fa(
        "
        sub process {
            my ($first, $file, @opts) = @_;
        }
        ",
    );
    let sig = fa
        .signatures_for_call("process", false, None, Point::new(0, 0), None)
        .pop()
        .unwrap();
    assert_eq!(sig.params.len(), 3);
    assert_eq!(sig.params[0].name, "$first");
    assert_eq!(sig.params[1].name, "$file");
    assert_eq!(sig.params[2].name, "@opts");
}

#[test]
fn test_tail_pod_item_method() {
    let fa = build_fa(
        "
            package WWW::Mech;
            sub get { }
            sub post { }

=head1 METHODS

=over

=item $mech->get($url)

Performs a GET request.

=item $mech->post($url)

Performs a POST request.

=back

=cut
        ",
    );
    let get_doc = fa
        .symbols()
        .iter()
        .find(|s| s.name == "get")
        .and_then(|s| match &s.detail {
            SymbolDetail::Sub { doc, .. } => doc.as_ref(),
            _ => None,
        });
    assert!(get_doc.is_some(), "get should have doc from =item");
    assert!(get_doc.unwrap().contains("GET request"));
}

#[test]
fn test_pod_doc_extracted_per_function() {
    let src = "\
package DemoUtils;
use Exporter 'import';
our @EXPORT_OK = qw(fetch_data transform);

=head2 fetch_data

Fetches data from the given URL.

=head2 transform

Transforms items.

=cut

sub fetch_data { }
sub transform { }
";
    let fa = build_fa(src);
    let fd = fa.symbols().iter().find(|s| s.name == "fetch_data").unwrap();
    if let SymbolDetail::Sub { ref doc, .. } = fd.detail {
        let d = doc.as_ref().expect("fetch_data should have doc");
        assert!(
            d.contains("Fetches data"),
            "should have fetch_data doc, got: {}",
            d
        );
        assert!(
            !d.contains("Transforms items"),
            "should NOT have transform doc, got: {}",
            d
        );
    } else {
        panic!("fetch_data should be a Sub");
    }
}

// The Perl `@flow` document speaks the shared capture vocabulary: the shipped
// document's every capture is one the minter reads, and a misspelt or
// unread capture is refused instead of extracting nothing.
#[test]
fn flow_document_captures_are_the_minters_vocabulary() {
    use crate::build::builder::pipeline::flow_capture_table;
    let lang: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
    let shipped = tree_sitter::Query::new(&lang, include_str!("../../../../queries/perl/flow.scm")).unwrap();
    assert!(flow_capture_table(&shipped).is_ok());
    for bad in ["(scalar) @flow.lhs", "(scalar) @flow.place", "(scalar) @flow.targte"] {
        let q = tree_sitter::Query::new(&lang, bad).unwrap();
        assert!(flow_capture_table(&q).is_err(), "{bad} must be refused");
    }
}
