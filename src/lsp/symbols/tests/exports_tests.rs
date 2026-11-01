//! Import/export surfaces: Types::Standard vocabulary, unresolved-method ancestry,
//! %EXPORT_TAGS, import bindings, transitive re-exports, workspace-symbol gating.

use super::*;

// ---- Types::Standard / Types::Common import-scoped vocabulary ----

/// `use Types::Standard qw/Str Int/; Str();` — Str is explicitly imported, so
/// calling it as a function (with parens, producing a FunctionCall ref) must not
/// raise an unresolved-function diagnostic. The plugin's on_use hook provides the
/// import-scoped vocabulary; the builder's process_use already creates an Import
/// entry for the qw-list, but this also exercises the plugin path.
#[test]
fn types_standard_explicit_import_suppresses_diagnostic() {
    let source = "use Types::Standard qw/Str Int/;\nStr();\nInt();\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let names: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        names.is_empty(),
        "Str()/Int() explicitly imported from Types::Standard must not produce unresolved-function; got: {:?}",
        names,
    );
}

/// `-all` import flag: the plugin expands to the full vocabulary, so any type
/// constant used as a function call is known even when the module isn't in the
/// module_index. Without the plugin's on_use hook, `-all` would appear literally
/// in imported_symbols and InstanceOf wouldn't match, producing a spurious diagnostic.
#[test]
fn types_standard_all_flag_suppresses_diagnostic() {
    let source = "use Types::Standard '-all';\nInstanceOf(['Foo']);\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let unresolved: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        unresolved.is_empty(),
        "InstanceOf() with '-all' must not produce unresolved-function; got: {:?}",
        unresolved,
    );
}

/// Types::Common::String and Types::Common::Numeric vocabularies are similarly
/// suppressed when explicitly imported.
#[test]
fn types_common_string_numeric_explicit_import_suppresses_diagnostic() {
    let source = concat!(
        "use Types::Common::String qw/NonEmptyStr/;\n",
        "use Types::Common::Numeric qw/PositiveInt/;\n",
        "NonEmptyStr();\nPositiveInt();\n",
    );
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let unresolved: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        unresolved.is_empty(),
        "Types::Common String/Numeric names must not produce unresolved-function; got: {:?}",
        unresolved,
    );
}

/// Regression: the existing InstanceOf['Foo'] constraint typing must still work
/// after adding the on_use hook. The import suppression is additive; it must not
/// disturb the type_constraint_names / type_constraint_inner machinery.
#[test]
fn types_standard_instanceof_constraint_typing_still_works() {
    use crate::model::file_analysis::InferredType;
    let source = concat!(
        "package T;\nuse Moo;\n",
        "use Types::Standard qw/Str Int InstanceOf/;\n",
        "has x => (is => 'ro', isa => InstanceOf['Foo']);\n",
        "my $t = Str;\n1;\n",
    );
    let analysis = parse_analysis(source);
    // Accessor `x` must return Foo (constraint-projection)
    assert_eq!(
        analysis.sub_return_type_at_arity("x", Some(0)),
        Some(InferredType::ClassName("Foo".to_string())),
        "InstanceOf['Foo'] isa must give the accessor a Foo return type",
    );
    // No unresolved-function diagnostics for Str, Int, InstanceOf
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let unresolved: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        unresolved.is_empty(),
        "No unresolved-function expected for Types::Standard imports; got: {:?}",
        unresolved,
    );
}

// ── P1.2: bare-use export_ok suppression ────────────────────────────────────

/// A bare `use Foo;` must suppress unresolved-function for names in export_ok.
/// Runtime exporters (Moose::Exporter->setup_import_methods) write to export_ok,
/// not export, so the suppression must cover both lists.
#[test]
fn bare_use_suppresses_export_ok_names() {
    // Consumer: bare use, no qw list — should auto-suppress subtype/as/where/coerce.
    let source = "use FakeTypeConstraints;\nsubtype('Foo', as => 'Str', where => sub { 1 });\nas('Str');\ncoerce('Foo', from => 'Int', via => sub { \"$_[0]\" });\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    // Seed with a module whose names are ONLY in export_ok (runtime-exporter style).
    let cached = fake_cached("FakeTypeConstraints", "/fake/FakeTypeConstraints.pm", &[], &["subtype", "as", "where", "coerce"]);
    module_index.insert_cache("FakeTypeConstraints", Some(cached));
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let unresolved: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        unresolved.is_empty(),
        "bare use of a runtime-exporter module must not produce unresolved-function for export_ok names; got: {:?}",
        unresolved,
    );
}

/// Genuinely-undefined functions must still produce a diagnostic even when a
/// module seeded with export_ok is in scope via a bare use.
#[test]
fn genuinely_undefined_still_flags_with_export_ok_module_in_scope() {
    let source = "use FakeTypeConstraints;\ntruly_undefined_fn();\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let cached = fake_cached("FakeTypeConstraints", "/fake/FakeTypeConstraints.pm", &[], &["subtype", "as"]);
    module_index.insert_cache("FakeTypeConstraints", Some(cached));
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let unresolved: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        unresolved.iter().any(|m| m.contains("truly_undefined_fn")),
        "truly_undefined_fn must still produce an unresolved-function diagnostic; got: {:?}",
        unresolved,
    );
}

// ── P3: lowercase `does` universal method ───────────────────────────────────

/// `$obj->does(...)` must not be flagged as an unresolved method.
/// Moose adds lowercase `does` to every class alongside UNIVERSAL's uppercase DOES.
#[test]
fn does_method_not_flagged_unresolved() {
    let source = "package M;\nuse Moose;\nsub check {\n    my ($self, $role) = @_;\n    return $self->does($role);\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let unresolved_method: Vec<&str> = diags.iter()
        .filter_map(|d| {
            if matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-method") {
                Some(d.message.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        !unresolved_method.iter().any(|m| m.contains("does")),
        "`does` must be in the universal-methods skip list and not flagged; got: {:?}",
        unresolved_method,
    );
}

// ── Incomplete ISA chain → honest-silent unresolved-method ───────────────────
// A class whose `@ISA` names a parent we can't resolve (not in the workspace,
// not in @INC) has an INCOMPLETE chain: the called method might be inherited
// from the unresolvable parent. Every invocant-typing path must consult the
// SAME `class_has_unresolved_ancestor` predicate so they can't drift (rule #10).

fn unresolved_method_messages(diags: &[Diagnostic]) -> Vec<String> {
    diags.iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-method"))
        .map(|d| d.message.clone())
        .collect()
}

/// THE FIX: `$self = shift; $self->inherited()` where the class `use base`s an
/// unresolvable parent must NOT emit unresolved-method. The `$self`/FirstParam
/// path was the one that leaked before the shared guard.
#[test]
fn self_invocant_unresolvable_parent_no_unresolved_method_use_base() {
    let source = "package Child;\nuse base qw(MyDep);\nsub new { bless {}, shift }\nsub local_thing {\n    my $self = shift;\n    return $self->dep_method();\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let um = unresolved_method_messages(&diags);
    assert!(
        um.is_empty(),
        "$self->dep_method with unresolvable `use base` parent must stay silent; got: {:?}",
        um,
    );
}

/// Same gate via `use parent`.
#[test]
fn self_invocant_unresolvable_parent_no_unresolved_method_use_parent() {
    let source = "package Child;\nuse parent -norequire, 'MyDep';\nsub new { bless {}, shift }\nsub local_thing {\n    my $self = shift;\n    return $self->dep_method();\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let um = unresolved_method_messages(&diags);
    assert!(um.is_empty(), "unresolvable `use parent` parent must stay silent; got: {:?}", um);
}

/// Same gate via `our @ISA =`.
#[test]
fn self_invocant_unresolvable_parent_no_unresolved_method_at_isa() {
    let source = "package Child;\nour @ISA = ('MyDep');\nsub new { bless {}, shift }\nsub local_thing {\n    my $self = shift;\n    return $self->dep_method();\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let um = unresolved_method_messages(&diags);
    assert!(um.is_empty(), "unresolvable `our @ISA` parent must stay silent; got: {:?}", um);
}

/// Regression: the direct-invocant paths (`Pkg->new->m`, `Pkg->m`) on a class
/// with an unresolvable parent must also stay silent — same predicate.
#[test]
fn direct_invocant_unresolvable_parent_no_unresolved_method() {
    let source = "package Child;\nuse base qw(MyDep);\nsub new { bless {}, shift }\nsub callers {\n    Child->dep_method();\n    my $c = Child->new;\n    $c->dep_method();\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let um = unresolved_method_messages(&diags);
    assert!(
        um.is_empty(),
        "direct-invocant calls on a class with an unresolvable parent must stay silent; got: {:?}",
        um,
    );
}

/// No over-suppression: a class with NO parents calling a genuinely missing
/// method STILL flags. The chain is complete, so the FP is a real TP.
#[test]
fn no_parents_missing_method_still_flags() {
    let source = "package Foo;\nsub new { bless {}, shift }\nsub real { 1 }\npackage main;\nmy $f = Foo->new;\n$f->totally_bogus_xyz();\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let um = unresolved_method_messages(&diags);
    assert!(
        um.iter().any(|m| m.contains("totally_bogus_xyz")),
        "a parentless class calling a missing method must still flag; got: {:?}",
        um,
    );
}

/// No over-suppression on a fully-resolved 2-hop chain: an inherited method
/// resolves (silent) AND a genuinely missing one still flags. All packages are
/// local here, so the whole chain is known.
#[test]
fn fully_resolved_two_hop_chain_resolves_inherited_and_flags_missing() {
    let source = "package GrandPa;\nsub new { bless {}, shift }\nsub gp_method { 1 }\npackage Pa;\nuse parent -norequire, 'GrandPa';\npackage Kid;\nuse parent -norequire, 'Pa';\nsub use_things {\n    my $self = shift;\n    $self->gp_method();\n    $self->missing_method();\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let um = unresolved_method_messages(&diags);
    assert!(
        !um.iter().any(|m| m.contains("gp_method")),
        "gp_method is inherited 2 hops on a fully-resolved chain — must NOT flag; got: {:?}",
        um,
    );
    assert!(
        um.iter().any(|m| m.contains("missing_method")),
        "missing_method on a fully-resolved chain must still flag; got: {:?}",
        um,
    );
}


#[test]
fn classic_perl_filehandle_fps_suppressed() {
    // `print FH LIST` / `say FH` / `printf FH` must not flag the bareword
    // filehandle as an unresolved function.
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    for src in [
        "print STDERR \"hi\";\n",
        "my $x; printf STDERR \"%s\", $x;\n",
        "say STDOUT \"hi\";\n",
        "print DATA;\n",
        "STDOUT->autoflush(1);\n",
        "my $t = -t STDIN;\n",
    ] {
        let analysis = parse_analysis(src);
        let diags = collect_diagnostics(&analysis, &module_index, Default::default());
        assert!(
            diags.is_empty(),
            "filehandle FP for `{}`: {:?}",
            src.trim(),
            diags.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn print_with_real_call_in_list_still_flags() {
    // The filehandle suppression must not swallow real calls in the list.
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let analysis = parse_analysis("print STDERR \"a\", frobnicate();\n");
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        diags.iter().any(|d| d.message.contains("frobnicate")),
        "real call `frobnicate` in print list must still flag; got: {:?}",
        diags.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
    );
}

#[test]
fn use_constant_callsites_not_flagged() {
    // Both scalar and block forms register the constant as a local sub, so
    // same-file callsites no longer flag as unresolved functions.
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    for src in [
        "use constant DEBUG => 1;\nmy $y = DEBUG && 2;\n",
        "use constant { A => 1, B => 2 };\nmy $z = A() + B();\n",
    ] {
        let analysis = parse_analysis(src);
        let diags = collect_diagnostics(&analysis, &module_index, Default::default());
        assert!(
            diags.is_empty(),
            "use-constant callsite FP for `{}`: {:?}",
            src.trim(),
            diags.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        );
    }
}

#[test]
fn require_bareword_not_flagged() {
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    for src in ["require Carp;\n", "require Foo::Bar;\n"] {
        let analysis = parse_analysis(src);
        let diags = collect_diagnostics(&analysis, &module_index, Default::default());
        assert!(
            diags.is_empty(),
            "require-bareword FP for `{}`: {:?}",
            src.trim(),
            diags.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        );
    }
}

// ---- NAV-B: %EXPORT_TAGS single export surface, goto-def + diagnostic agree ----

/// Build a cached module from raw Perl source (lets tests seed `%EXPORT_TAGS`
/// producers that `fake_cached` can't express).
fn cached_from_source(path: &str, source: &str) -> std::sync::Arc<crate::index::module_index::CachedModule> {
    std::sync::Arc::new(crate::index::module_index::CachedModule::new(
        std::path::PathBuf::from(path),
        std::sync::Arc::new(parse_analysis(source)),
    ))
}

/// The Perl::Critic::Utils shape: `hashify` exported only via a
/// `Readonly::Hash our %EXPORT_TAGS` member. A consumer importing the
/// `:data_conversion` tag and calling `hashify` must NOT be flagged as
/// unresolved (the diagnostic agrees with goto-def's resolution).
#[test]
fn export_tags_tag_import_diagnostic_agrees() {
    let producer = r#"
package Perl::Critic::Utils;
Readonly::Array our @EXPORT_OK => qw( interpolate );
Readonly::Hash our %EXPORT_TAGS => (
    all             => [ @EXPORT_OK ],
    data_conversion => [ qw{ hashify words_from_string interpolate } ],
);
sub hashify { 1 }
sub words_from_string { 2 }
sub interpolate { 3 }
1;
"#;
    let consumer = "use Perl::Critic::Utils qw(:data_conversion);\nmy %h = hashify(@list);\n";
    let analysis = parse_analysis(consumer);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    module_index.insert_cache(
        "Perl::Critic::Utils",
        Some(cached_from_source("/usr/lib/perl5/Perl/Critic/Utils.pm", producer)),
    );

    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let hashify_diags: Vec<_> = diags.iter().filter(|d| d.message.contains("hashify")).collect();
    assert!(
        hashify_diags.is_empty(),
        "tag-imported `hashify` must not be flagged unresolved; got {:?}",
        hashify_diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );

    // Goto-def reaches the producer file via the same export surface.
    let (_import, path, remote) =
        resolve_imported_function(&analysis, "hashify", &module_index)
            .expect("hashify resolves through the folded export surface");
    assert_eq!(remote, "hashify");
    assert!(path.ends_with("Perl/Critic/Utils.pm"));
}

/// A name in neither `@EXPORT*` nor any tag is still unresolved — the fold
/// widens the surface only for genuine tag members.
#[test]
fn export_tags_non_member_still_unresolved() {
    let producer = r#"
package Util::Tagged;
Readonly::Hash our %EXPORT_TAGS => (
    data_conversion => [ qw{ hashify } ],
);
sub hashify { 1 }
sub private_helper { 2 }
1;
"#;
    let consumer = "use Util::Tagged qw(:data_conversion);\nprivate_helper();\n";
    let analysis = parse_analysis(consumer);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    module_index.insert_cache(
        "Util::Tagged",
        Some(cached_from_source("/usr/lib/perl5/Util/Tagged.pm", producer)),
    );

    assert!(
        resolve_imported_function(&analysis, "private_helper", &module_index).is_none(),
        "non-exported sub must not resolve through the export surface",
    );
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        diags.iter().any(|d| d.message.contains("private_helper")),
        "non-exported sub must still be flagged; got {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

/// TASK B.1 — goto-def on an imported-function CALL SITE one-hops to the
/// DEFINING sub in the provider module, not the consumer's local `use` line.
/// The producer module is cached, so `sub_info(remote).def_line()` resolves;
/// the response must be a Scalar landing on that line in the .pm.
#[test]
fn imported_function_call_goto_def_reaches_module_sub() {
    let provider_src = "package My::Util;\n\
our @EXPORT_OK = qw(helper_fn);\n\
sub helper_fn {\n\
  my ($x) = @_;\n\
  return $x * 2;\n\
}\n\
1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_My_Util.pm"),
        std::sync::Arc::new(parse_analysis(provider_src)),
    );

    let consumer_src = "use My::Util qw(helper_fn);\n\
my $v = helper_fn(21);\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();

    // Cursor on the `helper_fn` token at the call site (line 1, not the use line).
    let byte = consumer_src.find("helper_fn(21)").expect("call site present");
    let prefix = &consumer_src[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };

    let uri = Url::parse("file:///consumer.pl").unwrap();
    let resp = find_definition(&crate::index::open_docs::OpenDocs::new(), &consumer, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        other => panic!("expected a single-hop Scalar to the module sub, got {other:?}"),
    };
    assert!(
        loc.uri.path().ends_with("My_Util.pm"),
        "goto-def should land in the provider file, got {}",
        loc.uri,
    );
    // `sub helper_fn` is the 3rd line (0-based row 2) of the provider source.
    assert_eq!(
        loc.range.start.line, 2,
        "should land on the defining `sub helper_fn` line, not the consumer's use stmt",
    );
}

/// TASK B.2 — goto-def on the INVOCANT class-name token in `Foo->bar()`
/// resolves to the `package Foo` decl (a PackageRef), exactly like `use Foo`.
/// The narrower PackageRef at the bareword span must win over the wider
/// MethodCall ref describing `bar`.
#[test]
fn class_invocant_goto_def_reaches_package_decl() {
    let source = "package Foo;\n\
sub bar { 42 }\n\
sub new { bless {}, shift }\n\
package main;\n\
Foo->bar();\n";
    let analysis = parse_analysis(source);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(source, None).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();

    // Cursor on the `Foo` invocant in `Foo->bar()`.
    let byte = source.find("Foo->bar").expect("invocant present");
    let prefix = &source[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32 + 1,
    };
    let uri = Url::parse("file:///test.pl").unwrap();
    let resp = find_definition(&crate::index::open_docs::OpenDocs::new(), &analysis, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        other => panic!("expected goto-def on class invocant, got {other:?}"),
    };
    assert_eq!(
        loc.range.start.line, 0,
        "`Foo` invocant should resolve to `package Foo;` (line 0), not the constructor or method",
    );
}

/// TASK B.2 regression guard — the METHOD token (`bar`) in `Foo->bar()` must
/// still resolve to the `sub bar` decl (NAV-A's precise method ref), NOT the
/// package. Emitting the invocant PackageRef must not shadow the method token.
#[test]
fn method_token_goto_def_unaffected_by_invocant_package_ref() {
    let source = "package Foo;\n\
sub bar { 42 }\n\
package main;\n\
Foo->bar();\n";
    let analysis = parse_analysis(source);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(source, None).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();

    // Cursor on the `bar` method token.
    let byte = source.rfind("bar()").expect("method token present");
    let prefix = &source[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };
    let uri = Url::parse("file:///test.pl").unwrap();
    let resp = find_definition(&crate::index::open_docs::OpenDocs::new(), &analysis, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        other => panic!("expected goto-def on method token, got {other:?}"),
    };
    assert_eq!(
        loc.range.start.line, 1,
        "`bar` method token should resolve to `sub bar` (line 1), not the package decl",
    );
}

// ---- Consumer import-binding evaluator (ExportSurface / imported_names) ----
//
// One bound set, every consumer reads it: the unresolved-function diagnostic
// and goto-def both route through `imported_names`, so "found by goto-def,
// flagged by diagnostic" is impossible. ModX has @EXPORT=always_here,
// @EXPORT_OK=opt_here, %EXPORT_TAGS=(all=>[both]).

fn modx_index() -> crate::index::module_index::ModuleIndex {
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let src = "package ModX;\n\
our @EXPORT = qw(always_here);\n\
our @EXPORT_OK = qw(opt_here);\n\
our %EXPORT_TAGS = (all => [qw(always_here opt_here)]);\n\
sub always_here { 1 }\n\
sub opt_here { 2 }\n\
1;\n";
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_ModX.pm"),
        std::sync::Arc::new(parse_analysis(src)),
    );
    idx
}

/// Does the unresolved-function diagnostic flag `name` in `source`?
fn flags_fn(source: &str, name: &str, idx: &crate::index::module_index::ModuleIndex) -> bool {
    let analysis = parse_analysis(source);
    collect_diagnostics(&analysis, idx, Default::default())
        .iter()
        .any(|d| {
            matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-function")
                && d.message.contains(&format!("'{}'", name))
        })
}

/// Does goto-def resolve the LAST call to `name` in `source` to ModX.pm?
fn gd_resolves(source: &str, name: &str, idx: &crate::index::module_index::ModuleIndex) -> bool {
    let analysis = parse_analysis(source);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(source, None).unwrap();
    let byte = source.rfind(&format!("{}(", name)).expect("call site present");
    let prefix = &source[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };
    let uri = Url::parse("file:///consumer.pl").unwrap();
    let loc = match find_definition(&crate::index::open_docs::OpenDocs::new(), &analysis, pos, &uri, idx) {
        Some(GotoDefinitionResponse::Scalar(loc)) => Some(loc),
        Some(GotoDefinitionResponse::Array(mut v)) if !v.is_empty() => Some(v.remove(0)),
        _ => None,
    };
    loc.map_or(false, |l| l.uri.path().ends_with("ModX.pm"))
}

#[test]
fn bare_use_binds_export_default_no_fp_and_gd_resolves() {
    let idx = modx_index();
    let src = "use ModX;\nalways_here();\n";
    assert!(!flags_fn(src, "always_here", &idx), "bare use binds @EXPORT — no FP");
    assert!(gd_resolves(src, "always_here", &idx), "goto-def resolves @EXPORT name");
}

/// JSON::PP's exact export header: `use Exporter ()` + BEGIN-block @ISA +
/// top-level `our @EXPORT = qw(...)`. A dual-life core module resolved from
/// @INC rides the DEPENDENCY cache, so the bare-use default surface must
/// bind from a dep-cached analysis too — `use JSON::PP;` +
/// `encode_json(...)` is not an unresolved function.
#[test]
fn bare_use_of_dep_cached_exporter_binds_default_exports() {
    let jsonpp = "package JSON::PP;\n\
use strict;\n\
use Exporter ();\n\
BEGIN { our @ISA = ('Exporter') }\n\
our @EXPORT = qw(encode_json decode_json from_json to_json);\n\
sub encode_json { }\n\
sub decode_json { }\n\
sub from_json { }\n\
sub to_json { }\n\
1;\n";
    let fa = parse_analysis(jsonpp);
    assert_eq!(
        fa.all_export(),
        vec!["encode_json", "decode_json", "from_json", "to_json"],
        "the BEGIN-ISA Exporter spelling must not hide @EXPORT extraction"
    );
    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);
    idx.insert_cache(
        "JSON::PP",
        Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
            std::path::PathBuf::from("/usr/lib/perl5/JSON/PP.pm"),
            std::sync::Arc::new(fa),
        ))),
    );
    let src = "use strict;\nuse JSON::PP;\nmy $s = encode_json({ a => 1 });\nmy $d = decode_json($s);\n";
    assert!(!flags_fn(src, "encode_json", &idx), "bare use JSON::PP binds encode_json");
    assert!(!flags_fn(src, "decode_json", &idx), "bare use JSON::PP binds decode_json");
}

#[test]
fn named_import_still_works_regression() {
    let idx = modx_index();
    let src = "use ModX qw(opt_here);\nopt_here();\n";
    assert!(!flags_fn(src, "opt_here", &idx), "named import binds the name");
    assert!(gd_resolves(src, "opt_here", &idx), "goto-def resolves named import");
}

#[test]
fn tag_selector_binds_members_no_fp_and_gd_resolves() {
    let idx = modx_index();
    let src = "use ModX qw(:all);\nopt_here();\n";
    assert!(!flags_fn(src, "opt_here", &idx), ":all tag binds member opt_here");
    assert!(gd_resolves(src, "opt_here", &idx), "goto-def resolves :tag member");
}

#[test]
fn default_tag_equals_export() {
    let idx = modx_index();
    // :DEFAULT is the Exporter alias for @EXPORT — binds always_here, not opt_here.
    let src = "use ModX qw(:DEFAULT);\nalways_here();\n";
    assert!(!flags_fn(src, "always_here", &idx), ":DEFAULT binds @EXPORT member");
    assert!(gd_resolves(src, "always_here", &idx), "goto-def resolves :DEFAULT member");
}

#[test]
fn as_rename_binds_local_to_origin() {
    let idx = modx_index();
    // local `here` aliases origin `always_here`; goto-def on `here` reaches the origin.
    let src = "use ModX always_here => { -as => 'here' };\nhere();\n";
    let analysis = parse_analysis(src);
    let renamed = analysis
        .imports
        .iter()
        .flat_map(|i| i.imported_symbols.iter())
        .find(|s| s.local_name == "here");
    assert!(
        renamed.map_or(false, |s| s.remote() == "always_here"),
        "the -as rename must bind local `here` to origin `always_here`; imports: {:?}",
        analysis.imports.iter().map(|i| i.imported_symbols.clone()).collect::<Vec<_>>(),
    );
    assert!(!flags_fn(src, "here", &idx), "renamed local `here` is bound — no FP");
    assert!(gd_resolves(src, "here", &idx), "goto-def on `here` reaches origin sub");
}

#[test]
fn as_rename_plain_comma_binds_local_to_origin() {
    let idx = modx_index();
    // `=>` is an autoquoting comma — `'always_here', { '-as', 'here' }` is
    // identical to `always_here => { -as => 'here' }`. The rename parse must
    // pair positionally so the plain-comma spelling binds the alias too.
    let src = "use ModX 'always_here', { '-as', 'here' };\nhere();\n";
    let analysis = parse_analysis(src);
    let renamed = analysis
        .imports
        .iter()
        .flat_map(|i| i.imported_symbols.iter())
        .find(|s| s.local_name == "here");
    assert!(
        renamed.map_or(false, |s| s.remote() == "always_here"),
        "plain-comma -as rename must bind local `here` to origin `always_here`; imports: {:?}",
        analysis.imports.iter().map(|i| i.imported_symbols.clone()).collect::<Vec<_>>(),
    );
    assert!(!flags_fn(src, "here", &idx), "renamed local `here` is bound — no FP");
    assert!(gd_resolves(src, "here", &idx), "goto-def on `here` reaches origin sub");
}

#[test]
fn empty_import_binds_nothing_flags_export_name() {
    let idx = modx_index();
    // `use ModX ();` — explicit empty list suppresses even @EXPORT.
    let src = "use ModX ();\nalways_here();\n";
    assert!(
        flags_fn(src, "always_here", &idx),
        "empty `()` import binds nothing — @EXPORT name must flag (honest)",
    );
}

#[test]
fn export_ok_on_bare_use_is_suppressed_gate5() {
    let idx = modx_index();
    // GATE-5 (load-bearing): a bare `use M;` suppresses unresolved-function for
    // @EXPORT_OK names. The builder cannot distinguish a runtime exporter's
    // defaults (recorded in export_ok) from traditional opt-in, so flagging them
    // would reintroduce the ~684-FP cluster. The honest "binds nothing" path is
    // `use M ();` (empty parens), tested separately.
    let src = "use ModX;\nopt_here();\n";
    assert!(
        !flags_fn(src, "opt_here", &idx),
        "bare use must suppress unresolved-function for @EXPORT_OK names (684-FP guard)",
    );
}

#[test]
fn diagnostic_and_gotodef_agree_on_bound_set() {
    let idx = modx_index();
    // For every case, "flagged by diagnostic" must equal "NOT brought" — and a
    // name the diagnostic stays silent on (brought) must goto-def resolve.
    let cases: &[(&str, &str, bool)] = &[
        // (source, name, should_flag_as_unresolved-function)
        ("use ModX;\nalways_here();\n", "always_here", false),
        ("use ModX qw(opt_here);\nopt_here();\n", "opt_here", false),
        ("use ModX qw(:all);\nopt_here();\n", "opt_here", false),
        ("use ModX ();\nalways_here();\n", "always_here", true),
    ];
    for (src, name, should_flag) in cases {
        let flagged = flags_fn(src, name, &idx);
        assert_eq!(
            flagged, *should_flag,
            "diagnostic verdict mismatch for `{}` in {:?}",
            name, src,
        );
        // Brought names (not flagged) must be navigable.
        if !should_flag {
            assert!(
                gd_resolves(src, name, &idx),
                "a non-flagged (brought) name must goto-def resolve: `{}` in {:?}",
                name, src,
            );
        }
    }
}

// ---- Transitive export surface: re-export edges (forms 1/2/3) ----
//
// A re-exporting module M folds another module's surface into its own. The
// consumer `imported_names` evaluator is UNCHANGED — it binds M's @EXPORT,
// which `ExportSurface` now reports transitively. Each test registers the
// producer(s) and M, then asserts the consumer's `use M;` binds the re-exported
// names (no FP) and goto-def reaches the defining producer file.

/// Register a module under a path derived from its name (`Pkg::Sub` →
/// `/tmp/perl_lsp_re_Pkg_Sub.pm`) so goto-def assertions can pin the file.
fn register_module(idx: &crate::index::module_index::ModuleIndex, pkg: &str, src: &str) {
    let file = format!("/tmp/perl_lsp_re_{}.pm", pkg.replace("::", "_"));
    idx.register_workspace_module(
        std::path::PathBuf::from(file),
        std::sync::Arc::new(parse_analysis(src)),
    );
}

/// Like `gd_resolves`, but pins the resolved file to `target_file`.
fn gd_resolves_to(
    source: &str,
    name: &str,
    idx: &crate::index::module_index::ModuleIndex,
    target_file: &str,
) -> bool {
    let analysis = parse_analysis(source);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(source, None).unwrap();
    let byte = source.rfind(&format!("{}(", name)).expect("call site present");
    let prefix = &source[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };
    let uri = Url::parse("file:///consumer.pl").unwrap();
    let loc = match find_definition(&crate::index::open_docs::OpenDocs::new(), &analysis, pos, &uri, idx) {
        Some(GotoDefinitionResponse::Scalar(loc)) => Some(loc),
        Some(GotoDefinitionResponse::Array(mut v)) if !v.is_empty() => Some(v.remove(0)),
        _ => None,
    };
    loc.map_or(false, |l| l.uri.path().ends_with(target_file))
}

#[test]
fn reexport_static_splice_binds_transitively() {
    // Form 1: `our @EXPORT = ('own_fn', @Base::EXPORT)` re-exports Base's surface.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "Base", "package Base;\nour @EXPORT = qw(base_fn);\nsub base_fn { 1 }\n1;\n");
    register_module(
        &idx,
        "M",
        "package M;\nour @EXPORT = ('own_fn', @Base::EXPORT);\nsub own_fn { 2 }\n1;\n",
    );

    let src = "use M;\nbase_fn();\nown_fn();\n";
    assert!(!flags_fn(src, "base_fn", &idx), "re-exported base_fn binds, no FP");
    assert!(!flags_fn(src, "own_fn", &idx), "own_fn binds, no FP");
    assert!(
        gd_resolves_to(src, "base_fn", &idx, "re_Base.pm"),
        "goto-def base_fn reaches Base",
    );
    assert!(
        gd_resolves_to(src, "own_fn", &idx, "re_M.pm"),
        "goto-def own_fn reaches M",
    );
}

#[test]
fn reexport_loop_push_literal_qw_binds() {
    // Form 2: loop-push over a literal qw module list.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "A", "package A;\nour @EXPORT = qw(a_fn);\nsub a_fn { 1 }\n1;\n");
    register_module(&idx, "B", "package B;\nour @EXPORT = qw(b_fn);\nsub b_fn { 1 }\n1;\n");
    register_module(
        &idx,
        "M",
        "package M;\nour @EXPORT = ();\nfor my $m (qw(A B)) {\n    push @EXPORT, @{\"${m}::EXPORT\"};\n}\n1;\n",
    );

    let src = "use M;\na_fn();\nb_fn();\n";
    assert!(!flags_fn(src, "a_fn", &idx), "loop-push re-exports A::a_fn");
    assert!(!flags_fn(src, "b_fn", &idx), "loop-push re-exports B::b_fn");
    assert!(gd_resolves_to(src, "a_fn", &idx, "re_A.pm"), "gd a_fn → A");
    assert!(gd_resolves_to(src, "b_fn", &idx, "re_B.pm"), "gd b_fn → B");
}

#[test]
fn reexport_loop_push_samefile_array_binds() {
    // Form 2 variant: the loop list is a same-file `my @mods = (...)` we chase.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "A", "package A;\nour @EXPORT = qw(a_fn);\nsub a_fn { 1 }\n1;\n");
    register_module(&idx, "B", "package B;\nour @EXPORT = qw(b_fn);\nsub b_fn { 1 }\n1;\n");
    register_module(
        &idx,
        "M",
        "package M;\nour @EXPORT = ();\nmy @mods = ('A', 'B');\nfor my $m (@mods) {\n    push @EXPORT, @{\"${m}::EXPORT\"};\n}\n1;\n",
    );

    let src = "use M;\na_fn();\nb_fn();\n";
    assert!(!flags_fn(src, "a_fn", &idx), "same-file @mods list re-exports A");
    assert!(!flags_fn(src, "b_fn", &idx), "same-file @mods list re-exports B");
}

#[test]
fn reexport_loop_push_dynamic_list_mints_no_edge() {
    // Form 2 honesty: a dynamic/unresolvable module list mints NO edge — the
    // re-exported name stays unresolved (we don't fabricate).
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "A", "package A;\nour @EXPORT = qw(a_fn);\nsub a_fn { 1 }\n1;\n");
    register_module(
        &idx,
        "M",
        "package M;\nour @EXPORT = ();\nfor my $m (@dynamic_runtime_list) {\n    push @EXPORT, @{\"${m}::EXPORT\"};\n}\n1;\n",
    );
    // Confirm no edge was minted on M.
    let m = idx.get_cached("M").expect("M cached");
    assert!(
        m.analysis.all_reexport_modules().is_empty(),
        "dynamic list must mint no re-export edge, got: {:?}",
        m.analysis.all_reexport_modules(),
    );

    let src = "use M;\na_fn();\n";
    assert!(flags_fn(src, "a_fn", &idx), "dynamic list → a_fn stays unresolved (honest)");
}

#[test]
fn reexport_declarative_also_binds() {
    // Form 3: `setup_import_methods(also => ['Base'])` includes Base's surface.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "Base", "package Base;\nour @EXPORT = qw(base_fn);\nsub base_fn { 1 }\n1;\n");
    register_module(
        &idx,
        "M",
        "package M;\nuse Moose::Exporter;\nMoose::Exporter->setup_import_methods( also => [ 'Base' ] );\n1;\n",
    );
    let m = idx.get_cached("M").expect("M cached");
    assert!(
        m.analysis.all_reexport_modules().contains(&"Base".to_string()),
        "also => ['Base'] mints a re-export edge, got: {:?}",
        m.analysis.all_reexport_modules(),
    );

    let src = "use M;\nbase_fn();\n";
    assert!(!flags_fn(src, "base_fn", &idx), "also-re-exported base_fn binds");
    assert!(gd_resolves_to(src, "base_fn", &idx, "re_Base.pm"), "gd base_fn → Base");
}

#[test]
fn reexport_cycle_resolves_finitely() {
    // A re-exports B, B re-exports A — the seen-set bounds the walk; the
    // consumer's surface query must terminate and bind both names.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(
        &idx,
        "A",
        "package A;\nour @EXPORT = ('a_fn', @B::EXPORT);\nsub a_fn { 1 }\n1;\n",
    );
    register_module(
        &idx,
        "B",
        "package B;\nour @EXPORT = ('b_fn', @A::EXPORT);\nsub b_fn { 1 }\n1;\n",
    );

    let src = "use A;\na_fn();\nb_fn();\n";
    assert!(!flags_fn(src, "a_fn", &idx), "cycle: a_fn binds");
    assert!(!flags_fn(src, "b_fn", &idx), "cycle: b_fn binds via A→B edge");
}

#[test]
fn a_module_exporting_nothing_answers_with_its_parents_surface_at_query_time() {
    // DDP → Data::Printer: the child declares no exports and inherits the
    // parent's `import`. Acquisition stores the child as written; the
    // inherited surface is the query's answer.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "Printer", "package Printer;\nuse Exporter 'import';\nour @EXPORT = qw(p);\nsub p { 1 }\n1;\n");
    register_module(&idx, "Shortcut", "package Shortcut;\nour @ISA = ('Printer');\n1;\n");
    let kid = idx.get_cached("Shortcut").expect("Shortcut cached");
    assert!(kid.analysis.all_export().is_empty(), "the child's own analysis exports nothing");

    let surface = kid.analysis.export_surface_with_index("Shortcut", &idx);
    assert_eq!(surface.default_set(), ["p".to_string()]);
    let src = "use Shortcut;\np();\n";
    assert!(!flags_fn(src, "p", &idx), "the inherited export binds");
    assert!(
        idx.find_exporters("p").contains(&"Shortcut".to_string()),
        "the child exports `p` for auto-import too: {:?}",
        idx.find_exporters("p"),
    );
}

#[test]
fn reexport_imported_names_evaluator_unchanged() {
    // Consumer-unchanged guard: `imported_names` is NOT special-cased for
    // re-exports — it binds whatever the surface reports. A surface built
    // WITHOUT the index walk (own-only) sees just M's own @EXPORT; built WITH
    // the index it sees the transitive set. Same evaluator, different surface.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    register_module(&idx, "Base", "package Base;\nour @EXPORT = qw(base_fn);\nsub base_fn { 1 }\n1;\n");
    register_module(
        &idx,
        "M",
        "package M;\nour @EXPORT = ('own_fn', @Base::EXPORT);\nsub own_fn { 2 }\n1;\n",
    );
    let m = idx.get_cached("M").expect("M cached");

    // A real bare `use M;` import, parsed from source (no Default on Import).
    let consumer = parse_analysis("use M;\n");
    let import = consumer.imports.iter().find(|i| i.module_name == "M").expect("use M import");

    // Own-only surface: just M's own @EXPORT — base_fn NOT visible.
    let own = m.analysis.export_surface("M");
    let bound_own = crate::model::file_analysis::imported_names(import, &own);
    assert!(bound_own.iter().any(|(l, _)| l == "own_fn"));
    assert!(
        !bound_own.iter().any(|(l, _)| l == "base_fn"),
        "own-only surface must not include the re-exported name",
    );

    // Index-walked surface: same evaluator, transitive surface — base_fn binds.
    let walked = m.analysis.export_surface_with_index("M", &idx);
    let bound_walked = crate::model::file_analysis::imported_names(import, &walked);
    assert!(bound_walked.iter().any(|(l, _)| l == "own_fn"));
    assert!(
        bound_walked.iter().any(|(l, _)| l == "base_fn"),
        "transitive surface includes the re-exported name — via the SAME evaluator",
    );
}

/// Consumer-side ctor key navigation through the deferred owner: in a
/// file that only `use`s Point, goto-def on `x` in `Point->new(x => 1)`
/// jumps to the `field $x :param` decl in Point.pm via the index (the
/// build-time gate deferred because the class isn't local; query time
/// re-derives the owner with the index in hand).
#[test]
fn test_goto_def_deferred_ctor_key_cross_file() {
    let point_src = "\
use v5.38;
class Point {
    field $x :param :reader;
}
1;
";
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    module_index.insert_cache(
        "Point",
        Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
            std::path::PathBuf::from("/tmp/sym_defer_point.pm"),
            std::sync::Arc::new(parse_analysis(point_src)),
        ))),
    );

    let consumer_src = "use Point;\nmy $p = Point->new(x => 1);\n";
    let analysis = parse_analysis(consumer_src);
    let uri = Url::parse("file:///tmp/sym_defer_consumer.pl").unwrap();
    // Cursor on `x` (row 1, col 19).
    let resp = find_definition(
        &crate::index::open_docs::OpenDocs::new(),
        &analysis,
        Position { line: 1, character: 19 },
        &uri,
        &module_index,
    );
    let Some(GotoDefinitionResponse::Scalar(loc)) = resp else {
        panic!("expected scalar goto-def, got {:?}", resp);
    };
    assert!(
        loc.uri.path().ends_with("sym_defer_point.pm"),
        "lands in the class file: {:?}",
        loc.uri,
    );
    assert_eq!(loc.range.start.line, 2, "lands on the field decl line");
}

/// Closed-shape hash-key typo diagnostic: a READ of a key the closed
/// literal doesn't define is hinted. An unconditional write EXTENDS
/// the shape (the written key reads silently; other unknowns still
/// hint); a conditional write opens it; an escape (call arg / alias /
/// invocant / sigil deref) opens it AT the escape span — reads before
/// it still hint. Reassignment suppresses (the one remaining gate
/// clause); open shapes and known keys stay silent.
#[test]
fn test_closed_shape_unknown_key_diagnostic() {
    let src = "\
my $config = { host => 'x', port => 1 };
my $bad = $config->{typo};
my $ok = $config->{host};
my $mutv = { host => 'x' };
$mutv->{added} = 1;
my $r0 = $mutv->{added};
my $r1 = $mutv->{other};
my $cond = { host => 'x' };
$cond->{maybe} = 1 if $ENV{X};
my $rc = $cond->{anything};
my $esc = { host => 'x' };
my $pre = $esc->{typo_pre};
process($esc);
my $r2 = $esc->{anything};
my $re = { host => 'x' };
$re = fetch_config() if $ENV{X};
my $r3 = $re->{whatever};
my $base = { a => 1 };
my $open = { %$base, extra => 1 };
my $maybe = $open->{whatever};
";
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(
        &analysis,
        &idx,
        DiagnosticOptions::default(),
    );
    let keys: Vec<&str> = diags
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unknown-hash-key"))
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        keys.len(),
        3,
        "the typo, the post-extension unknown, and the pre-escape typo: {:?}",
        keys,
    );
    assert!(keys[0].contains("'typo'"), "{:?}", keys);
    assert!(keys[0].contains("host"), "message names the known keys: {:?}", keys);
    assert!(keys[1].contains("'other'"), "{:?}", keys);
    assert!(
        keys[1].contains("added"),
        "extended shape names the written key: {:?}",
        keys,
    );
    assert!(
        keys[2].contains("'typo_pre'"),
        "read BEFORE the escape still hints: {:?}",
        keys,
    );
}

/// The literal-hash spelling gets the same diagnostic: container-form
/// reads (`$h{k}`) check against `%h`'s shape. A bare `func(%h)` pass
/// flattens to copies — the callee can't add keys — so it does NOT
/// suppress; `\%h` ref-taking does.
#[test]
fn test_literal_hash_unknown_key_diagnostic() {
    let src = "\
my %config = (host => 'x');
my $bad = $config{typo};
func(%config);
my %taken = (host => 'x');
my $r = \\%taken;
my $silent = $taken{anything};
";
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(
        &analysis,
        &idx,
        DiagnosticOptions::default(),
    );
    let keys: Vec<&str> = diags
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unknown-hash-key"))
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(keys.len(), 1, "only the %config typo: {:?}", keys);
    assert!(keys[0].contains("'typo'"), "{:?}", keys);
    assert!(keys[0].contains("%config"), "names the hash variable: {:?}", keys);
}

/// A slot's shape can live only in a PARENT's file, written through the
/// grandparent's spelling (`my $self = Grand->new; $self->{_area} = {..}`):
/// the parent file states `Field{Grand, _area}`, never `Field{Base, _area}`.
/// The child's `Field{Base, _area}` reaches it through `Base.pm`'s own bag
/// re-keyed to the grandparent — a hop the candidate fan-out's on-path skip
/// must not take with it.
#[test]
fn test_unknown_key_diagnostic_through_parent_file_ancestry() {
    let module = |name: &str, src: &str| {
        Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
            std::path::PathBuf::from(format!("/fake/{name}.pm")),
            std::sync::Arc::new(parse_analysis(src)),
        )))
    };
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.set_workspace_root(None);
    idx.insert_cache(
        "Grand",
        module("Grand", "package Grand;\nsub new { my $class = shift; return bless {}, $class }\n1;\n"),
    );
    idx.insert_cache(
        "Base",
        module(
            "Base",
            "\
package Base;
our @ISA = qw(Grand);
sub new { my $class = shift; my $self = Grand->new(); $self->{_area} = { _layout => 1 }; bless $self, $class; return $self }
1;
",
        ),
    );
    let analysis = parse_analysis(
        "\
package Kid;
our @ISA = qw(Base);
sub draw { my $self = shift; return $self->{_area}->{_typo} }
1;
",
    );
    let diags = collect_diagnostics(&analysis, &idx, DiagnosticOptions::default());
    let keys: Vec<&str> = diags
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unknown-hash-key"))
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(keys.len(), 1, "the typo against Base's slot shape: {:?}", keys);
    assert!(keys[0].contains("'_typo'") && keys[0].contains("_layout"), "{:?}", keys);
}

/// Expression-base spelling: `cfg()->{kye}` hints off the producer's
/// closed return shape — no variable in hand, the drill's Projected
/// witness carries the (base, key) pair. Known keys stay silent, and
/// bare-variable bases stay the gated ref loop's territory.
#[test]
fn test_expression_base_unknown_key_diagnostic() {
    let src = "\
sub cfg { return { host => 'x', port => 1 } }
my $ok = cfg()->{host};
my $bad = cfg()->{hsot};
cfg()->{hsot2};
";
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(
        &analysis,
        &idx,
        DiagnosticOptions::default(),
    );
    let keys: Vec<&str> = diags
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unknown-hash-key"))
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        keys.len(),
        2,
        "assignment-position and bare-statement call-base typos: {:?}",
        keys,
    );
    assert!(keys[0].contains("'hsot'"), "{:?}", keys);
    assert!(
        keys[0].contains("this expression's"),
        "expression-base message form: {:?}",
        keys,
    );
    assert!(
        keys[1].contains("'hsot2'"),
        "bare-statement drill is witnessed too: {:?}",
        keys,
    );
}

/// Role `requires NAMES` declares method contracts: `$self->name`
/// inside the role resolves to the synthesized marker (no
/// unresolved-method hint, goto-def lands on the contract atom); a
/// genuinely unknown method still hints. Both the qw and
/// single-string spellings.
#[test]
fn test_role_requires_suppresses_unresolved_method() {
    let src = "\
package My::Role;
use Moo::Role;
requires qw/fetch source/;
requires 'extra';
sub run {
  my ($self) = @_;
  $self->fetch;
  $self->source;
  $self->extra;
  $self->typo_method;
}
1;
";
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(
        &analysis,
        &idx,
        DiagnosticOptions::default(),
    );
    let unresolved: Vec<&str> = diags
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-method"))
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(unresolved.len(), 1, "only the typo: {:?}", unresolved);
    assert!(unresolved[0].contains("typo_method"), "{:?}", unresolved);
    assert_eq!(
        Some(analysis.role_requires("My::Role").len()),
        Some(3),
        "the contract record carries all three names",
    );
}

/// Anonymous subs are resolvable, not browsable: the synthesized
/// `(anon)` symbol carries hide_in_outline, and the workspace-symbol
/// converter honors it (an empty/broad query must not surface them).
#[test]
fn test_anon_subs_hidden_from_workspace_symbols() {
    let src = "\
my $cb = sub { return 42 };
sub real_sub { 1 }
";
    let analysis = parse_analysis(src);
    let uri = tower_lsp::lsp_types::Url::parse("file:///t.pl").unwrap();
    let names: Vec<String> = analysis
        .symbols()
        .iter()
        .filter_map(|s| symbol_to_workspace_info(s, uri.clone()))
        .map(|i| i.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "real_sub"),
        "real subs surface: {:?}",
        names,
    );
    assert!(
        !names.iter().any(|n| n.contains("anon")),
        "anon subs stay out: {:?}",
        names,
    );
}

/// H7-10(a): an anonymous sub is never a method-completion candidate. With
/// an unresolvable receiver, `complete_methods` falls back to enumerating
/// file subs — but `$obj->(anon)` isn't callable, so the synthetic `(anon)`
/// symbol must be filtered at the source (gated on "has a callable name",
/// not the literal spelling).
#[test]
fn test_anon_sub_not_a_method_completion_candidate() {
    let src = "\
my $cb = sub { return 42 };
sub real_method { 1 }
";
    let analysis = parse_analysis(src);
    // `$unknown` doesn't resolve to a class → the file-subs fallback path.
    let cands = analysis.complete_methods("$unknown", Point::new(2, 0), None);
    let labels: Vec<&str> = cands.iter().map(|c| c.label.as_str()).collect();
    assert!(
        !labels.iter().any(|l| l.contains("anon")),
        "anon sub must not be a method candidate: {:?}",
        labels
    );
    assert!(
        labels.contains(&"real_method"),
        "named subs are still candidates: {:?}",
        labels
    );
}

#[test]
fn test_dedup_workspace_symbols_collapses_twins() {
    use tower_lsp::lsp_types::{Location, Position, Range, SymbolInformation, SymbolKind, Url};
    #[allow(deprecated)]
    let make = |name: &str, line: u32, col: u32| SymbolInformation {
        name: name.to_string(),
        kind: SymbolKind::METHOD,
        tags: None,
        deprecated: None,
        location: Location {
            uri: Url::parse("file:///t.pm").unwrap(),
            range: Range {
                start: Position { line, character: col },
                end: Position { line, character: col + 4 },
            },
        },
        container_name: None,
    };
    // Two byte-identical twins (accessor getter + fluent-writer at one span)
    // plus a same-named symbol at a different line (a real distinct decl).
    let mut results = vec![
        make("connect_timeout", 10, 4),
        make("connect_timeout", 10, 4),
        make("connect_timeout", 42, 4),
    ];
    dedup_workspace_symbols(&mut results);
    assert_eq!(results.len(), 2, "twins collapse, distinct span survives: {:?}",
        results.iter().map(|s| (s.name.clone(), s.location.range.start.line)).collect::<Vec<_>>());
    assert!(results.iter().any(|s| s.location.range.start.line == 10));
    assert!(results.iter().any(|s| s.location.range.start.line == 42));
}

/// `my sub helper { … }` — document symbols keep it (real in-file
/// structure); workspace-symbol search drops it (not addressable
/// outside its block). Plain subs surface in both.
#[test]
fn test_lexical_subs_outline_only() {
    let src = "\
my sub helper_fn { 42 }
sub public_fn { helper_fn() }
";
    let analysis = parse_analysis(src);
    let uri = tower_lsp::lsp_types::Url::parse("file:///t.pl").unwrap();
    let ws: Vec<String> = analysis
        .symbols()
        .iter()
        .filter_map(|s| symbol_to_workspace_info(s, uri.clone()))
        .map(|i| i.name)
        .collect();
    assert!(ws.iter().any(|n| n == "public_fn"), "{:?}", ws);
    assert!(!ws.iter().any(|n| n == "helper_fn"), "lexical stays out of workspace: {:?}", ws);
    let outline = analysis.document_symbols();
    let names: Vec<&str> = outline.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"helper_fn"), "outline keeps the lexical sub: {:?}", names);
    assert!(names.contains(&"public_fn"), "{:?}", names);
}
