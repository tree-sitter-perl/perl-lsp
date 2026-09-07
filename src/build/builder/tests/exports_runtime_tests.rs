use super::*;

// ---- Fix #1: `not` operator ----

/// `not $x` must never produce an unresolved-function diagnostic.
/// Validated in symbols_tests; here we confirm the builder emits a
/// FunctionCall ref (so the name is visible for filtering) whose name
/// is "not" — the builtin-surface guard in collect_diagnostics then
/// suppresses it.
#[test]
fn not_operator_emits_no_function_call_ref() {
    // As of ts-parser-perl 1.1.0, `not` is the low-precedence logical-not
    // OPERATOR (`logical_not_expression`), not a function call. So no `not`
    // FunctionCall ref is emitted at all — which is the correct end state:
    // nothing for the builtin-suppressor to filter, and no unresolved-function
    // diagnostic for `not`.
    let fa = build_fa("my $x = 1;\nmy $y = not $x;\n");
    let not_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "not" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .collect();
    assert!(
        not_refs.is_empty(),
        "`not` is an operator now; no FunctionCall ref should exist; got refs: {:?}",
        fa.refs().iter().map(|r| (&r.target_name, &r.kind)).collect::<Vec<_>>(),
    );
    // The $x operand still gets its read ref.
    assert!(
        fa.refs().iter().any(|r| r.target_name == "$x"),
        "operand $x should still be referenced",
    );
}

// ---- Fix #2: `\&subname` code-ref ----

/// `\&handler` must emit a FunctionCall ref pointing at `handler`
/// so goto-def and references both work.
#[test]
fn refgen_bare_name_emits_function_call_ref() {
    let fa = build_fa("sub handler { 1 }\nmy $cb = \\&handler;\n");
    let refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "handler" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .collect();
    assert_eq!(
        refs.len(),
        1,
        "\\&handler should emit exactly one FunctionCall ref for `handler`; got: {:?}",
        fa.refs()
            .iter()
            .filter(|r| r.target_name == "handler")
            .map(|r| &r.kind)
            .collect::<Vec<_>>(),
    );
}

/// `\&Pkg::handler` (qualified form) must also emit a FunctionCall ref.
#[test]
fn refgen_qualified_name_emits_function_call_ref() {
    let fa = build_fa("my $cb = \\&Foo::handler;\n");
    let refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "handler" || r.target_name == "Foo::handler"
        })
        .collect();
    assert!(
        !refs.is_empty(),
        "\\&Foo::handler should emit a FunctionCall ref; got refs: {:?}",
        fa.refs().iter().map(|r| (&r.target_name, &r.kind)).collect::<Vec<_>>(),
    );
}

/// goto-def on `\&handler` should land on the `sub handler` definition.
/// FunctionCall refs route goto-def through package+name matching, not
/// `resolves_to`, so we check `find_definition` returns the sub's span.
#[test]
fn refgen_goto_def_lands_on_sub_definition() {
    // sub handler on line 0, \&handler on line 1 col 9
    let src = "sub handler { 1 }\nmy $cb = \\&handler;\n";
    let fa = build_fa(src);
    let sub_sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "handler" && matches!(s.kind, SymKind::Sub))
        .expect("handler sub should be defined");
    // Cursor at the `h` of `\&handler` on line 1 (0-indexed row=1, col≈11)
    let def_span = fa.find_definition(
        Point::new(1, 11),
        None);
    assert_eq!(
        def_span,
        Some(sub_sym.selection_span),
        "goto-def on \\&handler should land on the handler sub; sym={:?}",
        sub_sym,
    );
}

// ---- Fully-qualified variable reads → (pkg, basename) ----

#[test]
fn split_qualified_basics() {
    use crate::model::file_analysis::split_qualified;
    assert_eq!(split_qualified("Foo::Bar::baz"), (Some("Foo::Bar"), "baz"));
    assert_eq!(split_qualified("baz"), (None, "baz"));
    assert_eq!(split_qualified("Foo::bar"), (Some("Foo"), "bar"));
    // Leading `::` (main:: shorthand) → empty-string package, preserved.
    assert_eq!(split_qualified("::foo"), (Some(""), "foo"));
}

#[test]
fn fq_scalar_read_resolves_same_file() {
    // `our $x` in package Pkg; `$Pkg::x` read in another package, same file.
    let src = "package Pkg;\nour $x = 1;\npackage Main;\nmy $a = $Pkg::x;\n";
    let fa = build_fa(src);
    let decl = fa
        .symbols()
        .iter()
        .find(|s| s.name == "$x" && s.package.as_deref() == Some("Pkg"))
        .expect("our $x in Pkg should be a symbol");
    // Cursor on the `x` tail of `$Pkg::x` (line 3).
    let read = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "$Pkg::x")
        .expect("$Pkg::x should emit a Variable ref");
    assert_eq!(
        read.resolved_symbol(),
        Some(decl.id),
        "FQ scalar read should resolve to the Pkg::x declaration"
    );
    let def = fa.find_definition(read.span.start, None);
    assert_eq!(def, Some(decl.selection_span));
}

#[test]
fn fq_array_read_resolves_same_file() {
    let src = "package Pkg;\nour @arr = (1, 2);\npackage Main;\nmy @b = @Pkg::arr;\n";
    let fa = build_fa(src);
    let decl = fa
        .symbols()
        .iter()
        .find(|s| s.name == "@arr" && s.package.as_deref() == Some("Pkg"))
        .expect("our @arr in Pkg should be a symbol");
    let read = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "@Pkg::arr")
        .expect("@Pkg::arr should emit a Variable ref");
    assert_eq!(read.resolved_symbol(), Some(decl.id));
}

#[test]
fn fq_var_ref_span_narrowed_to_tail() {
    // rule #7: rename/highlight token is the bare tail, not the whole path.
    let src = "package Pkg;\nour $x = 1;\npackage Main;\nmy $a = $Pkg::x;\n";
    let fa = build_fa(src);
    let read = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "$Pkg::x")
        .expect("$Pkg::x ref");
    // `$Pkg::x` on line 3: `my $a = ` is 8 cols, `$Pkg::` is 6 → `x` at col 14.
    assert_eq!(read.span.start.row, 3);
    assert_eq!(read.span.start.column, 14, "span should start at the `x` tail");
}

#[test]
fn unqualified_var_still_resolves_lexically() {
    // Regression: the FQ fast-path must not break plain lexical resolution.
    let fa = build_fa("my $x = 1;\nprint $x;\n");
    let read = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "$x" && r.access == AccessKind::Read)
        .expect("plain $x read");
    assert!(read.resolved_symbol().is_some(), "unqualified read still resolves");
}

// ---- Fix #3: around/before/after modifier bodies ----

/// In `around foo => sub { my ($orig, $self) = @_; ... }`, `$self` (param index 1)
/// must be typed as the enclosing class so `$self->method` chains resolve.
#[test]
fn around_modifier_second_param_typed_as_class() {
    let src = r#"
package Dog;
use Moo;

sub speak { "woof" }

around speak => sub {
    my ($orig, $self) = @_;
    return $self->speak_loudly();
};

sub speak_loudly { "WOOF" }
"#;
    let fa = build_fa(src);

    // `$self` inside the around body should resolve to `Dog`
    // (row=8 is the `return $self->speak_loudly()` line).
    let ty = fa.inferred_type_via_bag("$self", Point::new(8, 12));
    assert!(
        ty.is_some(),
        "$self inside `around` body should have an inferred type; got None.\
         \nAll TCs: {:?}",
        fa.refs()
            .iter()
            .filter(|r| r.target_name == "$self")
            .collect::<Vec<_>>(),
    );
    match ty.unwrap() {
        InferredType::ClassName(name) => assert_eq!(name, "Dog", "$self should be Dog"),
        InferredType::FirstParam { package } => {
            assert_eq!(package, "Dog", "$self FirstParam should be Dog")
        }
        other => panic!("expected ClassName/FirstParam for $self, got {:?}", other),
    }
}

/// In `before foo => sub { my ($self) = @_; ... }`, `$self` (param index 0)
/// must be typed as the enclosing class.
#[test]
fn before_modifier_first_param_typed_as_class() {
    let src = r#"
package Cat;
use Moo;

sub meow { "mrrp" }

before meow => sub {
    my ($self) = @_;
    $self->hiss();
};

sub hiss { "ssss" }
"#;
    let fa = build_fa(src);

    // Row 8 = `$self->hiss()` line
    let ty = fa.inferred_type_via_bag("$self", Point::new(8, 4));
    assert!(
        ty.is_some(),
        "$self inside `before` body should have an inferred type",
    );
    match ty.unwrap() {
        InferredType::ClassName(name) => assert_eq!(name, "Cat"),
        InferredType::FirstParam { package } => assert_eq!(package, "Cat"),
        other => panic!("expected ClassName/FirstParam, got {:?}", other),
    }
}

// ---- Runtime exporter modeling ----
//
// Static analysis can't run import(); we model the declarative setup
// shapes so exported names land in `export_ok` (same plumbing as
// `@EXPORT_OK`), which then drives goto-def / refs / diagnostics.

#[test]
fn sub_exporter_use_setup_records_exports() {
    let fa = build_fa(
        "package My::Exporter;\n\
         use Sub::Exporter -setup => { exports => [qw/alpha beta/] };\n\
         sub alpha { 1 }\n\
         sub beta { 2 }\n\
         1;\n",
    );
    assert!(fa.export_ok.contains(&"alpha".to_string()),
        "export_ok should contain alpha; got {:?}", fa.export_ok);
    assert!(fa.export_ok.contains(&"beta".to_string()),
        "export_ok should contain beta; got {:?}", fa.export_ok);
}

#[test]
fn sub_exporter_setup_exporter_call_records_exports() {
    let fa = build_fa(
        "package My::Exporter;\n\
         use Sub::Exporter ();\n\
         Sub::Exporter::setup_exporter({ exports => [qw/gamma/] });\n\
         sub gamma { 3 }\n\
         1;\n",
    );
    assert!(fa.export_ok.contains(&"gamma".to_string()),
        "export_ok should contain gamma; got {:?}", fa.export_ok);
}

#[test]
fn sub_exporter_generator_hashref_records_keys() {
    // Generators: best-effort — the hashref keys are the exported names.
    let fa = build_fa(
        "package My::Exporter;\n\
         use Sub::Exporter -setup => { exports => { delta => \\&_gen_delta } };\n\
         sub _gen_delta { sub { 4 } }\n\
         1;\n",
    );
    assert!(fa.export_ok.contains(&"delta".to_string()),
        "export_ok should contain generator name delta; got {:?}", fa.export_ok);
}

/// Sub::Exporter `exports` member collection is separator-agnostic: the
/// fat-comma generator entry (`bar => \&gen`) and its plain-comma equivalent
/// (`'bar', \&gen`) both put `bar` on the surface while skipping the opaque
/// generator value.
#[test]
fn sub_exporter_exports_plain_comma_members_join_surface() {
    for exports in [
        "[ 'foo', bar => \\&_gen ]",
        "[ 'foo', 'bar', \\&_gen ]",
    ] {
        let src = format!(
            "package My::Exp;\n\
             use Sub::Exporter -setup => {{ exports => {exports} }};\n\
             sub foo {{}}\n\
             sub bar {{}}\n\
             sub _gen {{}}\n\
             1;\n",
        );
        let fa = build_fa(&src);
        for name in ["foo", "bar"] {
            assert!(
                fa.exports_name(name),
                "exports `{exports}`: `{name}` must join the surface; export_ok={:?}",
                fa.export_ok,
            );
        }
    }
}

#[test]
fn sub_exporter_setup_array_members_and_groups_join_surface() {
    // `-setup => { exports => [ qw(foo bar), baz => \&_gen ], groups => {...} }`:
    // every member name joins the export surface (incl. the `name => \&gen`
    // generator entry's name and the group member arrays). The group keys
    // (`default`/`extra`) are selectors, not subs — they must NOT join.
    let fa = build_fa(
        "package My::Exp;\n\
         use Sub::Exporter -setup => {\n\
           exports => [ qw(foo bar), baz => \\&_build_baz ],\n\
           groups  => { default => [qw(foo)], extra => [qw(bar baz)] },\n\
         };\n\
         sub foo {}\n\
         sub bar {}\n\
         sub _build_baz {}\n\
         1;\n",
    );
    for name in ["foo", "bar", "baz"] {
        assert!(
            fa.exports_name(name),
            "exports_name({name}) should be true; export_ok={:?}",
            fa.export_ok
        );
    }
    assert!(
        !fa.export_ok.contains(&"default".to_string())
            && !fa.export_ok.contains(&"extra".to_string()),
        "group selector keys must not join the surface; got {:?}",
        fa.export_ok
    );
}

#[test]
fn sub_exporter_member_refs_local_subs() {
    // Each member that names a local sub gets a FunctionCall ref at its
    // export-list mention (rule #7); a member naming no local sub (the public
    // generator name) gets none.
    let fa = build_fa(
        "package My::Exp;\n\
         use Sub::Exporter -setup => {\n\
           exports => [ qw(foo bar), baz => \\&_build_baz ],\n\
           groups  => { extra => [qw(bar baz)] },\n\
         };\n\
         sub foo {}\n\
         sub bar {}\n\
         sub baz {}\n\
         1;\n",
    );
    let count = |name: &str| {
        fa.refs()
            .iter()
            .filter(|r| {
                r.target_name == name
                    && matches!(&r.kind, RefKind::FunctionCall)
                    && r.resolved_package() == Some("My::Exp")
            })
            .count()
    };
    // foo: exports list only = 1. bar: exports + group `extra` = 2.
    // baz: exports + group `extra` = 2.
    assert_eq!(count("foo"), 1, "foo member ref; got refs {:?}", fa.refs().iter().filter(|r| r.target_name=="foo").collect::<Vec<_>>());
    assert_eq!(count("bar"), 2, "bar in exports + group extra");
    assert_eq!(count("baz"), 2, "baz in exports + group extra");
}

#[test]
fn sub_exporter_member_goto_def_and_references() {
    use crate::index::file_store::FileStore;
    use crate::index::resolve::{refs_to, RoleMask, TargetKind, TargetRef};
    use std::path::PathBuf;

    let src = "package My::Exp;\n\
         use Sub::Exporter -setup => { exports => [ qw(foo bar) ] };\n\
         sub foo {}\n\
         sub bar {}\n\
         1;\n";
    let fa = build_fa(src);

    let foo_def_span = fa
        .symbols()
        .iter()
        .find(|s| s.name == "foo")
        .map(|s| s.selection_span)
        .expect("foo sub symbol");
    let export_ref = fa
        .refs()
        .iter()
        .find(|r| {
            r.target_name == "foo"
                && matches!(&r.kind, RefKind::FunctionCall { .. })
                && r.span != foo_def_span
        })
        .expect("an export-list FunctionCall ref for foo");
    let r = fa
        .ref_at(export_ref.span.start)
        .expect("ref_at the export member token");
    assert_eq!(r.target_name, "foo");

    let store = FileStore::new();
    let path = PathBuf::from("/tmp/qa_sub_exporter.pm");
    store.insert_workspace(path.clone(), fa);
    let results = refs_to(
        &store,
        None,
        &TargetRef {
            name: "foo".to_string(),
            kind: TargetKind::Sub {
                package: Some("My::Exp".to_string()),
            },
            method_classes: Vec::new(), scope: crate::index::resolve::OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
        },
        RoleMask::EDITABLE,
    );
    // def + 1 exports-list mention = 2.
    assert_eq!(
        results.len(),
        2,
        "references on foo should list the def and its exports-list mention; got {results:?}"
    );
}

#[test]
fn sub_exporter_setup_exporter_call_with_groups() {
    // The function-call setup form folds exports + groups the same way.
    let fa = build_fa(
        "package My::Exp;\n\
         use Sub::Exporter ();\n\
         Sub::Exporter::setup_exporter({\n\
           exports => [qw/gamma delta/],\n\
           groups  => { all => [qw/gamma delta/] },\n\
         });\n\
         sub gamma {}\n\
         sub delta {}\n\
         1;\n",
    );
    assert!(fa.exports_name("gamma") && fa.exports_name("delta"),
        "setup_exporter exports should join surface; got {:?}", fa.export_ok);
    assert!(!fa.export_ok.contains(&"all".to_string()),
        "group selector `all` must not join the surface");
}

#[test]
fn non_sub_exporter_use_unaffected() {
    // Regression: a plain `use` of an unrelated module with a `-setup`-shaped
    // arg must not record exports (only Sub::Exporter's use is folded).
    let fa = build_fa(
        "package My::Thing;\n\
         use Some::Other -setup => { exports => [qw/leak/] };\n\
         sub leak {}\n\
         1;\n",
    );
    assert!(!fa.export_ok.contains(&"leak".to_string()),
        "non-Sub::Exporter use must not record exports; got {:?}", fa.export_ok);
    // And no spurious export-member ref on the local sub.
    let leak_refs = fa.refs().iter().filter(|r| r.target_name == "leak"
        && matches!(&r.kind, RefKind::FunctionCall { .. })).count();
    assert_eq!(leak_refs, 0, "no member ref for an unrelated use's pseudo-export");
}

#[test]
fn moose_exporter_setup_import_methods_records_exports() {
    let fa = build_fa(
        "package My::Sugar;\n\
         use Moose::Exporter;\n\
         Moose::Exporter->setup_import_methods(\n\
             with_meta => ['has_table'],\n\
             as_is     => [qw/col belongs_to/],\n\
         );\n\
         sub has_table { }\n\
         sub col { }\n\
         sub belongs_to { }\n\
         1;\n",
    );
    for name in ["has_table", "col", "belongs_to"] {
        assert!(fa.export_ok.contains(&name.to_string()),
            "export_ok should contain {}; got {:?}", name, fa.export_ok);
    }
}

#[test]
fn type_library_add_type_records_named_export() {
    let fa = build_fa(
        "package My::Types;\n\
         use Type::Library -base;\n\
         __PACKAGE__->add_type({ name => 'PositiveInt' });\n\
         __PACKAGE__->add_type({ name => 'Email' });\n\
         1;\n",
    );
    assert!(fa.export_ok.contains(&"PositiveInt".to_string()),
        "export_ok should contain PositiveInt; got {:?}", fa.export_ok);
    assert!(fa.export_ok.contains(&"Email".to_string()),
        "export_ok should contain Email; got {:?}", fa.export_ok);
}

#[test]
fn non_exporter_setup_does_not_pollute_exports() {
    // A plain method call named neither setup verb leaves exports empty.
    let fa = build_fa(
        "package My::Thing;\n\
         My::Thing->configure({ name => 'nope', exports => [qw/leak/] });\n\
         1;\n",
    );
    assert!(!fa.export_ok.contains(&"leak".to_string()),
        "unrelated method call must not record exports; got {:?}", fa.export_ok);
    assert!(!fa.export_ok.contains(&"nope".to_string()));
}

#[test]
fn setup_verb_name_without_exporter_use_does_not_pollute_exports() {
    // The verb name matches a real exporter setup call, but the package never
    // `use`d an exporter that defines it — so it's an unrelated method call
    // (`$x->add_type({name=>...})` on some domain object) and must not record
    // exports. Without the package-use gate this would false-positive.
    let fa = build_fa(
        "package My::Registry;\n\
         my $schema = build_schema();\n\
         $schema->add_type({ name => 'Widget' });\n\
         __PACKAGE__->setup_import_methods(as_is => [qw/leak/]);\n\
         1;\n",
    );
    assert!(!fa.export_ok.contains(&"Widget".to_string()),
        "add_type without Type::Library use must not record exports; got {:?}", fa.export_ok);
    assert!(!fa.export_ok.contains(&"leak".to_string()),
        "setup_import_methods without Moose::Exporter use must not record exports; got {:?}", fa.export_ok);
}

#[test]
fn export_ok_array_assignment_unions_with_runtime_exports() {
    // `:Export` attr records `attr_export` at the sub walk; a later
    // `our @EXPORT_OK = (...)` must union, not clobber, so both survive.
    let fa = build_fa(
        "package My::Mixed;\n\
         use Exporter::Extensible;\n\
         sub attr_export :Export { }\n\
         our @EXPORT_OK = ('array_export');\n\
         sub array_export { }\n\
         1;\n",
    );
    assert!(fa.export_ok.contains(&"attr_export".to_string()),
        "runtime :Export attr survives the array assignment; got {:?}", fa.export_ok);
    assert!(fa.export_ok.contains(&"array_export".to_string()),
        "array-assigned name recorded; got {:?}", fa.export_ok);
}

// Moo/Moose non-default has options: predicate, clearer,
// writer, reader, builder, handles
// ============================================================

#[test]
fn test_moo_has_predicate_string() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'name' => (is => 'ro', predicate => 'has_name');
",
    );
    let pred: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "has_name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(pred.len(), 1, "explicit predicate string synthesizes method");
    if let SymbolDetail::Sub { ref params, is_method, .. } = pred[0].detail {
        assert!(is_method);
        assert!(params.is_empty(), "predicate takes no args");
    }
}

#[test]
fn test_moo_has_predicate_shorthand() {
    // `predicate => 1` derives `has_<attr>` for public attrs.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'email' => (is => 'ro', predicate => 1);
",
    );
    let pred: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "has_email" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(pred.len(), 1, "predicate => 1 derives has_<attr>");
}

#[test]
fn test_moo_has_predicate_private_attr_shorthand() {
    // Private attrs (leading `_`) get `_has_<rest>` not `has__<rest>`.
    let fa = build_fa(
        "
package Foo;
use Moo;
has '_token' => (is => 'ro', predicate => 1);
",
    );
    let pred: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "_has_token" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(pred.len(), 1, "predicate => 1 on _attr derives _has_<rest>");
}

#[test]
fn test_moo_has_clearer_string() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'cache' => (is => 'rw', clearer => 'clear_cache');
",
    );
    let clearer: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "clear_cache" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(clearer.len(), 1, "explicit clearer string synthesizes method");
}

#[test]
fn test_moo_has_clearer_shorthand() {
    // `clearer => 1` derives `clear_<attr>`.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'items' => (is => 'rw', clearer => 1);
",
    );
    let clearer: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "clear_items" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(clearer.len(), 1, "clearer => 1 derives clear_<attr>");
}

#[test]
fn test_moo_has_clearer_private_shorthand() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has '_session' => (is => 'rw', clearer => 1);
",
    );
    let clearer: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "_clear_session" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(clearer.len(), 1, "clearer => 1 on _attr derives _clear_<rest>");
}

#[test]
fn test_moo_has_writer_option() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'color' => (is => 'ro', writer => 'set_color');
",
    );
    let writer: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "set_color" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(writer.len(), 1, "writer option synthesizes method");
    if let SymbolDetail::Sub { ref params, .. } = writer[0].detail {
        assert_eq!(params.len(), 1, "writer has one param");
    }
}

#[test]
fn test_moo_has_reader_option() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'size' => (is => 'ro', reader => 'get_size');
",
    );
    let reader: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "get_size" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(reader.len(), 1, "reader option synthesizes method");
    if let SymbolDetail::Sub { ref params, is_method, .. } = reader[0].detail {
        assert!(is_method);
        assert!(params.is_empty(), "reader takes no args");
    }
}

#[test]
fn test_moo_has_builder_shorthand() {
    // `builder => 1` → method symbol `_build_<attr>` so goto-def
    // to the user-written sub resolves.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'items' => (is => 'ro', builder => 1);
sub _build_items { return [] }
",
    );
    let builder_sym: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "_build_items" && s.kind == SymKind::Method)
        .collect();
    // The synthesized placeholder + the real sub definition both exist.
    // At minimum one symbol with that name must be present.
    assert!(
        !builder_sym.is_empty(),
        "_build_items must exist (synthesized or user-written)"
    );
}

#[test]
fn test_moo_has_builder_string() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'cache' => (is => 'lazy', builder => '_make_cache');
",
    );
    let builder_sym: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "_make_cache" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(builder_sym.len(), 1, "explicit builder name synthesizes method");
}

#[test]
fn test_moo_has_auxiliaries_without_is() {
    // predicate/clearer/builder are valid even when `is` is absent.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'flag' => (predicate => 'has_flag', clearer => 'clear_flag');
",
    );
    let pred: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "has_flag" && s.kind == SymKind::Method)
        .collect();
    let clearer: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "clear_flag" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(pred.len(), 1, "predicate synthesized without is");
    assert_eq!(clearer.len(), 1, "clearer synthesized without is");
}

#[test]
fn test_moo_has_auxiliaries_with_bare() {
    // `is => bare` suppresses default accessor but auxiliaries still appear.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'secret' => (is => 'bare', predicate => 'has_secret');
",
    );
    // Default accessor suppressed
    let accessors: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "secret" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(accessors.len(), 0, "bare suppresses default accessor");
    // Predicate still synthesized
    let pred: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "has_secret" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(pred.len(), 1, "predicate synthesized even with is => bare");
}

#[test]
fn test_moo_has_handles_hashref() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'logger' => (is => 'ro', isa => 'Log::Any', handles => { log => 'debug', warning => 'warn' });
",
    );
    let log_sym: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "log" && s.kind == SymKind::Method)
        .collect();
    let warning_sym: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "warning" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(log_sym.len(), 1, "handles hashref synthesizes 'log' method");
    assert_eq!(warning_sym.len(), 1, "handles hashref synthesizes 'warning' method");
}

#[test]
fn test_moose_has_handles_arrayref() {
    let fa = build_fa(
        "
package Foo;
use Moose;
has 'db' => (is => 'ro', isa => 'DBI::db', handles => [qw(prepare execute)]);
",
    );
    let prepare: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "prepare" && s.kind == SymKind::Method)
        .collect();
    let execute: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "execute" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(prepare.len(), 1, "handles arrayref synthesizes 'prepare'");
    assert_eq!(execute.len(), 1, "handles arrayref synthesizes 'execute'");
}

#[test]
fn test_moo_has_handles_instanceof_edges_return_type() {
    // When isa is InstanceOf['X'], handles delegation edges each local
    // method's return through PackageSymbol{X, remote} so type inference
    // chains through.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'logger' => (is => 'ro', isa => \"InstanceOf['Log::Any']\", handles => { log => 'debug' });
",
    );
    let log_sym: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "log" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(log_sym.len(), 1, "handles delegation synthesizes method");
    // Provenance confirms this came from framework synthesis
    match fa.return_type_provenance(log_sym[0].id) {
        TypeProvenance::FrameworkSynthesis { framework, reason } => {
            assert!(
                framework == "Moo" || framework == "Moose",
                "provenance framework should be Moo/Moose, got {}",
                framework
            );
            assert!(reason.contains("handles"), "reason should mention handles");
        }
        TypeProvenance::Inferred => {
            // Acceptable: no witness was pushed if there was no isa type resolution.
        }
        other => panic!("unexpected provenance: {other:?}"),
    }
}

/// Regression: an option keyword that carries DATA, not a method name
/// (`is`/`isa`/`default`/`lazy`/…), must never mint a method named after its
/// string value. The sprint that moved the accessor vocabulary into moo.rhai
/// briefly synthesized phantom `ro`/`rw`/`lazy`/`bare` methods from every
/// option's value — this pins the gate that stopped it.
#[test]
fn test_moo_has_no_phantom_method_from_data_options() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'name' => (is => 'ro', isa => 'Str', default => 'bob', lazy => 1, required => 1);
",
    );
    for phantom in ["ro", "rw", "lazy", "bare", "Str", "bob", "1"] {
        let hits: Vec<_> = fa
            .symbols()
            .iter()
            .filter(|s| s.name == phantom && s.kind == SymKind::Method)
            .collect();
        assert!(
            hits.is_empty(),
            "option value `{phantom}` must not become a method, got {} symbol(s)",
            hits.len()
        );
    }
    // The real accessor still lands.
    let name: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(name.len(), 1, "the real `name` accessor must still synthesize");
}

/// Moose `lazy_build => 1` implies a builder/clearer/predicate trio at runtime.
#[test]
fn test_moose_has_lazy_build_expands_trio() {
    let fa = build_fa(
        "
package Foo;
use Moose;
has 'cache' => (is => 'ro', lazy_build => 1);
",
    );
    for (name, what) in [
        ("_build_cache", "builder"),
        ("clear_cache", "clearer"),
        ("has_cache", "predicate"),
    ] {
        let hits: Vec<_> = fa
            .symbols()
            .iter()
            .filter(|s| s.name == name && s.kind == SymKind::Method)
            .collect();
        assert_eq!(hits.len(), 1, "lazy_build must synthesize the {what} `{name}`");
    }
    // `lazy_build`/`is` themselves are not methods.
    for phantom in ["lazy_build", "ro", "1"] {
        assert!(
            !fa.symbols().iter().any(|s| s.name == phantom && s.kind == SymKind::Method),
            "`{phantom}` must not become a method"
        );
    }
}

/// goto-def on a `has` accessor must land on the attribute name token of the
/// `has` declaration, not an option line (`is => 'ro'`) inside the body.
#[test]
fn test_moo_has_accessor_selection_span_is_attr_name() {
    // `has` on line 3 (0-indexed), attr `name` at col 5; options on line 4.
    let fa = build_fa("package Foo;\nuse Moo;\nhas name => (\n    is => 'ro',\n);\n");
    let name = fa
        .symbols()
        .iter()
        .find(|s| s.name == "name" && s.kind == SymKind::Method)
        .expect("name accessor");
    assert_eq!(
        name.selection_span.start.row, 2,
        "selection_span must point at the `has name` line, not the options line"
    );
}

/// Dancer2::Plugin re-exports Moo's `has`, so consumer plugins get accessor
/// synthesis even though they never literally `use Moo`.
#[test]
fn test_dancer2_plugin_has_synthesizes_accessor() {
    let fa = build_fa(
        "
package My::Plugin;
use Dancer2::Plugin;
has my_setting => (is => 'ro', isa => 'Str');
",
    );
    let acc: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "my_setting" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(acc.len(), 1, "Dancer2::Plugin `has` must synthesize the accessor");
    // And no phantom from the `is`/`isa` data options.
    for phantom in ["ro", "Str"] {
        assert!(
            !fa.symbols().iter().any(|s| s.name == phantom && s.kind == SymKind::Method),
            "`{phantom}` must not become a method under Dancer2::Plugin either"
        );
    }
}

#[test]
fn use_constant_scalar_form_registers_sub_symbol() {
    // `use constant NAME => VAL` declares a package-global sub. Registering
    // it as a Sub symbol silences the unresolved-function hint at callsites
    // and gives goto-def.
    let fa = build_fa("use constant DEBUG => 1;\nmy $y = DEBUG && 2;\n");
    assert!(
        fa.symbols().iter().any(|s| s.name == "DEBUG" && s.kind == SymKind::Sub),
        "DEBUG must be registered as a Sub symbol; got: {:?}",
        fa.symbols().iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>(),
    );
}

#[test]
fn use_constant_block_form_registers_each_name() {
    let fa = build_fa("use constant { A => 1, B => 2, C => 3 };\n");
    for n in ["A", "B", "C"] {
        assert!(
            fa.symbols().iter().any(|s| s.name == n && s.kind == SymKind::Sub),
            "block-form constant `{n}` must be a Sub symbol; got: {:?}",
            fa.symbols().iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn use_constant_block_plain_comma_keys_register() {
    // `=>` is just an autoquoting comma — `{ 'GAMMA', 3 }` is identical to
    // `{ GAMMA => 3 }`. The block walker must pair positionally, so quoted
    // plain-comma keys register as Sub symbols exactly like fat-comma keys.
    let fa = build_fa("use constant { 'GAMMA', 3, 'DELTA', 4, A => 1, B => 2 };\n");
    for n in ["GAMMA", "DELTA", "A", "B"] {
        assert!(
            fa.symbols().iter().any(|s| s.name == n && s.kind == SymKind::Sub),
            "plain-comma block constant `{n}` must be a Sub symbol; got: {:?}",
            fa.symbols().iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        );
    }
}

/// Plain-comma block constants get usage refs + goto-def/references, the same
/// as fat-comma — the registration path is separator-agnostic end to end.
#[test]
fn use_constant_block_plain_comma_goto_def_and_references() {
    use crate::index::file_store::FileStore;
    use crate::index::resolve::{refs_to, RoleMask, TargetKind, TargetRef};
    use std::path::PathBuf;

    let src = r#"package Foo;
use constant { 'GAMMA', 3, DELTA => 4 };
sub go {
    my $a = GAMMA;
    return DELTA;
}
"#;
    let fa = build_fa(src);
    // Usage refs emitted for both spellings' keys.
    for n in ["GAMMA", "DELTA"] {
        assert!(
            fa.refs().iter().any(|r| {
                r.target_name == n
                    && matches!(&r.kind, RefKind::FunctionCall)
                    && r.resolved_package() == Some("Foo")
            }),
            "usage of plain/fat-comma constant `{n}` must get a FunctionCall ref; refs: {:?}",
            fa.refs().iter().filter(|r| r.target_name == n).collect::<Vec<_>>(),
        );
    }
    let store = FileStore::new();
    store.insert_workspace(PathBuf::from("/tmp/qa_const_plain.pm"), fa);
    for name in ["GAMMA", "DELTA"] {
        let results = refs_to(
            &store,
            None,
            &TargetRef {
                name: name.to_string(),
                kind: TargetKind::Sub { package: Some("Foo".to_string()) },
                method_classes: Vec::new(), scope: crate::index::resolve::OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
            },
            RoleMask::EDITABLE,
        );
        assert_eq!(
            results.len(), 2,
            "references on `{name}` should list its def + 1 usage; got {results:?}",
        );
    }
}

/// Regression: positional pairing must not invent keys from a value position.
/// In `use constant { A => 1 }` the `1` is a value, never a constant name — the
/// walker pairs `A`→`1` and stops, so no `1`-named (or numeric) Sub appears.
#[test]
fn use_constant_block_does_not_mispair_values_as_keys() {
    let fa = build_fa("use constant { A => 1, 'B', 2 };\n");
    let const_subs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Sub)
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        const_subs.contains(&"A") && const_subs.contains(&"B"),
        "keys A and B must register; got {:?}",
        const_subs,
    );
    // The value tokens (`1`, `2`) are not keys — no numeric-named Sub symbol.
    assert!(
        !const_subs.iter().any(|n| *n == "1" || *n == "2"),
        "value tokens must never register as constant names; got {:?}",
        const_subs,
    );
}

#[test]
fn use_constant_between_subs_at_file_scope() {
    // Constants declared between subs must still register.
    let src = "sub one {}\nuse constant MID => 'x';\nsub two {}\n";
    let fa = build_fa(src);
    assert!(
        fa.symbols().iter().any(|s| s.name == "MID" && s.kind == SymKind::Sub),
        "MID declared between subs must register as a Sub symbol",
    );
}

#[test]
fn multiple_name_form_use_constants_each_register() {
    // Several separate NAME-form `use constant` statements in one package:
    // each declares its own package-global sub. The use-dedup key carries the
    // statement span, so identical work identity at different spans (the
    // constant name isn't folded into `constant_strings` when `imports` is
    // extracted, so it's empty for all of them) no longer collapses past the
    // first.
    let src = r#"package Foo;
use constant ALPHA => 1;
use constant BETA  => 2;
use constant GAMMA => 3;
sub go {
    my $a = ALPHA;
    my $b = BETA;
    my $c = GAMMA;
}
"#;
    let fa = build_fa(src);
    for n in ["ALPHA", "BETA", "GAMMA"] {
        assert!(
            fa.symbols().iter().any(|s| s.name == n && s.kind == SymKind::Sub),
            "every separate NAME-form constant must register a Sub symbol; `{n}` missing. got: {:?}",
            fa.symbols().iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        );
        // Usages resolve: each name joins `declared_constants`, so the
        // standalone bareword usage gets a FunctionCall ref to the def.
        assert!(
            fa.refs().iter().any(|r| {
                r.target_name == n
                    && matches!(&r.kind, RefKind::FunctionCall)
                    && r.resolved_package() == Some("Foo")
            }),
            "usage of `{n}` must get a FunctionCall ref to its def; refs for {n}: {:?}",
            fa.refs().iter().filter(|r| r.target_name == n).collect::<Vec<_>>(),
        );
    }
}

/// goto-def + references across THREE separate NAME-form `use constant`
/// statements: every def is reachable and every usage lists.
#[test]
fn multiple_name_form_use_constants_goto_def_and_references() {
    use crate::index::file_store::FileStore;
    use crate::index::resolve::{refs_to, RoleMask, TargetKind, TargetRef};
    use std::path::PathBuf;

    let src = r#"package Foo;
use constant ALPHA => 1;
use constant BETA  => 2;
use constant GAMMA => 3;
sub go {
    my $a = ALPHA;
    my $b = BETA;
    return GAMMA;
}
"#;
    let fa = build_fa(src);
    let store = FileStore::new();
    store.insert_workspace(PathBuf::from("/tmp/qa_multi_const.pm"), fa);

    // BETA: def + 1 usage = 2 hits. GAMMA: def + 1 usage = 2. ALPHA: def + 1.
    for name in ["ALPHA", "BETA", "GAMMA"] {
        let results = refs_to(
            &store,
            None,
            &TargetRef {
                name: name.to_string(),
                kind: TargetKind::Sub { package: Some("Foo".to_string()) },
                method_classes: Vec::new(), scope: crate::index::resolve::OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
            },
            RoleMask::EDITABLE,
        );
        assert_eq!(
            results.len(),
            2,
            "references on `{name}` should list its def + 1 usage; got {results:?}"
        );
    }
}

#[test]
fn indirect_object_filehandle_not_a_function_ref() {
    // `print FH LIST` — the bareword filehandle must NOT become a
    // FunctionCall ref (otherwise STDERR/STDOUT/DATA flag as unresolved).
    for src in [
        "print STDERR \"hi\";\n",
        "printf STDERR \"%s\", $x;\n",
        "say STDOUT \"hi\";\n",
    ] {
        let fa = build_fa(src);
        let fh = src.split_whitespace().nth(1).unwrap().trim_matches(|c| c == '"');
        assert!(
            !fa.refs().iter().any(|r|
                matches!(r.kind, RefKind::FunctionCall { .. }) && r.target_name == fh),
            "filehandle `{fh}` must not be a FunctionCall ref for `{}`; refs: {:?}",
            src.trim(),
            fa.refs().iter().filter(|r| matches!(r.kind, RefKind::FunctionCall { .. }))
                .map(|r| r.target_name.clone()).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn print_with_paren_call_still_emits_function_ref() {
    // `print foo("x")` is a real call — foo must keep its FunctionCall ref.
    let fa = build_fa("print foo(\"x\");\n");
    assert!(
        fa.refs().iter().any(|r|
            matches!(r.kind, RefKind::FunctionCall { .. }) && r.target_name == "foo"),
        "parenthesized call `foo(...)` inside print must keep its FunctionCall ref",
    );
}

#[test]
fn shift_invocant_typed_like_at_underscore() {
    // `my $self = shift;` types $self as the enclosing class, exactly like
    // `my ($self) = @_;` — so method calls on $self resolve in-package.
    // Each body is on line 2 (0-indexed); query $self at its use point.
    let at_point = tree_sitter::Point { row: 2, column: 28 };
    let is_class_w = |fa: &FileAnalysis| {
        matches!(
            fa.inferred_type_via_bag("$self", at_point),
            Some(InferredType::ClassName(ref c)) if c == "W"
        )
    };
    let shift_fa =
        build_fa("package W;\nsub go { 1 }\nsub f { my $self = shift; $self->go(); }\n");
    let at_fa =
        build_fa("package W;\nsub go { 1 }\nsub f { my ($self) = @_; $self->go(); }\n");
    assert!(is_class_w(&shift_fa), "shift-extracted $self must type as ClassName(W)");
    assert!(is_class_w(&at_fa), "@_-extracted $self must type as ClassName(W)");
}
