//! An explicit paren group spells the same list as the bare one: `{ (a => 1) }`
//! is `{ a => 1 }`. Each test writes a list the builder reads in its paren
//! spelling.

use super::*;

fn has_method(fa: &FileAnalysis, name: &str) -> bool {
    fa.symbols().iter().any(|s| s.name == name && s.kind == SymKind::Method)
}

#[test]
fn class_tiny_hash_keys_in_parens() {
    let fa = build_fa("package Foo;\nuse Class::Tiny { (name => undef, age => 0) };\n");
    assert!(has_method(&fa, "name") && has_method(&fa, "age"));
}

#[test]
fn as_alias_in_parens() {
    let fa = build_fa("use ModX always_here => { (-as => 'here') };\nhere();\n");
    let renamed = fa
        .imports
        .iter()
        .flat_map(|i| i.imported_symbols.iter())
        .find(|s| s.local_name == "here");
    assert!(renamed.is_some_and(|s| s.remote() == "always_here"), "{:?}", fa.imports);
}

#[test]
fn hash_literal_type_in_parens() {
    let fa = build_fa("package Foo;\nsub f { my $h = { (a => 1, b => 'x') }; $h }\n1;\n");
    let t = fa.sub_return_type_at_arity("f", None);
    let Some(InferredType::HashWithKeys { keys, .. }) = t else { panic!("{t:?}") };
    let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn array_literal_type_in_parens() {
    let fa = build_fa("package Foo;\nsub f { my $a = [ (1, 'x') ]; $a }\n1;\n");
    assert_eq!(
        fa.sub_return_type_at_arity("f", None),
        Some(InferredType::Sequence(vec![InferredType::Numeric, InferredType::String]))
    );
}

#[test]
fn sub_exporter_generator_keys_in_parens() {
    let fa = build_fa(
        "package My::Exporter;\nuse Sub::Exporter -setup => { exports => { (delta => \\&_gen) } };\nsub _gen { sub { 4 } }\n1;\n",
    );
    assert!(fa.export_ok.contains(&"delta".to_string()), "{:?}", fa.export_ok);
}

#[test]
fn push_exports_in_parens() {
    let fa = build_fa("package Foo;\nuse Exporter 'import';\nour @EXPORT_OK = qw(foo);\npush(@EXPORT_OK, 'bar');\npush((@EXPORT_OK), 'baz');\n");
    assert_eq!(fa.export_ok, vec!["foo", "bar", "baz"]);
}

#[test]
fn importer_menu_single_pair_in_parens() {
    let fa = build_fa("package My::Menu;\nsub IMPORTER_MENU { return (export => [qw/foo/]) }\nsub foo { 1 }\n");
    assert!(fa.export_ok.contains(&"foo".to_string()), "{:?}", fa.export_ok);
}

#[test]
fn constant_hash_in_parens() {
    let fa = build_fa("package Foo;\nuse constant { (ALPHA => 1, BETA => 2) };\n");
    for c in ["ALPHA", "BETA"] {
        assert!(fa.symbols().iter().any(|s| s.name == c), "{c}");
    }
}


// A comment inside the braces sits beside the list, not in it.
#[test]
fn a_comment_in_a_hash_literal_is_not_a_key() {
    let fa = build_fa("package Foo;\nsub f { return {    # note\n    host => 'h',\n    port => 1,\n} }\n1;\n");
    let t = fa.sub_return_type_at_arity("f", None);
    let Some(InferredType::HashWithKeys { keys, .. }) = t else { panic!("{t:?}") };
    let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["host", "port"]);
}

// A string literal is one shape however it is quoted.
#[test]
fn as_alias_on_a_double_quoted_name() {
    let fa = build_fa("use ModX \"always_here\" => { -as => 'here' };\nhere();\n");
    let renamed = fa
        .imports
        .iter()
        .flat_map(|i| i.imported_symbols.iter())
        .find(|s| s.local_name == "here");
    assert!(renamed.is_some_and(|s| s.remote() == "always_here"), "{:?}", fa.imports);
}
