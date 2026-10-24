//! A comment or POD is a grammar extra: it can sit between any two tokens,
//! so no positional read ("the first argument", "the k-th pair", "the last
//! statement") may count it. Each test puts a comment where a reader once
//! took it for an element (#268).

use super::*;

fn has_method(fa: &FileAnalysis, name: &str) -> bool {
    fa.symbols().iter().any(|s| s.name == name && s.kind == SymKind::Method)
}

fn has_sub(fa: &FileAnalysis, name: &str) -> bool {
    fa.symbols().iter().any(|s| s.name == name && s.kind == SymKind::Sub)
}

fn sub_params(fa: &FileAnalysis, name: &str) -> Vec<String> {
    let sym = fa.symbols().iter().find(|s| s.name == name).expect("sub");
    match &sym.detail {
        SymbolDetail::Sub { params, .. } => params.iter().map(|p| p.name.clone()).collect(),
        other => panic!("{other:?}"),
    }
}

fn class_at(fa: &FileAnalysis, var: &str, row: usize) -> Option<String> {
    fa.inferred_type_via_bag(var, Point::new(row, 80)).and_then(|t| t.class_name().map(str::to_string))
}

// ---- call arguments (`cst::call_args`) ----

#[test]
fn bless_class_after_a_comment() {
    let fa = build_fa("package Foo;\nsub new { my $class = shift; return bless {}, # c\n $class; }\n1;\n");
    assert_eq!(
        fa.sub_return_type_at_arity("new", None).and_then(|t| t.class_name().map(str::to_string)).as_deref(),
        Some("Foo"),
    );
}

#[test]
fn blessed_variable_class_after_a_comment() {
    let fa = build_fa("package Foo;\nsub new {\n    my $self = {};\n    bless $self, # c\n 'Foo';\n    $self;\n}\n1;\n");
    assert_eq!(class_at(&fa, "$self", 5).as_deref(), Some("Foo"));
}

#[test]
fn shift_with_only_a_comment_is_the_invocant() {
    let fa = build_fa("package Foo;\nuse parent 'Bar';\nsub m {\n    my $self = shift( # c\n );\n    $self;\n}\n1;\n");
    assert_eq!(class_at(&fa, "$self", 5).as_deref(), Some("Foo"));
}

#[test]
fn call_arity_ignores_a_comment() {
    let fa = build_fa(
        "package My::DOM;\nuse Mojo::Base -base;\nsub pick {\n  my $self = shift;\n  return 'many' if @_ > 2;\n  return $self;\n}\npackage main;\nmy $d = My::DOM->new;\nmy $v = $d->pick('a', # c\n 'b');\n$v;\n",
    );
    assert_eq!(class_at(&fa, "$v", 11).as_deref(), Some("My::DOM"), "two arguments, not three");
}

// ---- sub parameters ----

#[test]
fn shift_params_past_a_comment_line() {
    let fa = build_fa("sub f {\n    my $self = shift;\n    # the rest\n    my $x = shift;\n}\n");
    assert_eq!(sub_params(&fa, "f"), ["$self", "$x"]);
}

#[test]
fn shift_default_after_a_comment() {
    let fa = build_fa("sub f {\n    my $x = shift || # c\n 'dflt';\n}\n");
    let sym = fa.symbols().iter().find(|s| s.name == "f").unwrap();
    let SymbolDetail::Sub { params, .. } = &sym.detail else { panic!() };
    assert_eq!(params[0].default.as_deref(), Some("'dflt'"));
}

// ---- `has` ----

#[test]
fn has_nested_options_after_a_comment() {
    let fa = build_fa("package Foo;\nuse Moo;\nhas n => # c\n (is => 'ro', isa => 'Int');\n1;\n");
    assert_eq!(fa.sub_return_type_at_arity("n", Some(0)), Some(InferredType::Numeric));
}

#[test]
fn mojo_has_default_after_a_comment() {
    let fa = build_fa("package Foo;\nuse Mojo::Base -base;\nhas n => # c\n 42;\n1;\n");
    assert_eq!(fa.sub_return_type_at_arity("n", Some(0)), Some(InferredType::Numeric));
}

#[test]
fn constraint_parameter_after_a_comment() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nhas thing => (is => 'ro', isa => InstanceOf[ # c\n 'My::Thing']);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        Some(InferredType::ClassName("My::Thing".to_string())),
    );
}

// ---- exports ----

#[test]
fn as_rename_after_a_comment() {
    let fa = build_fa("use ModX always_here => # c\n { -as => 'here' };\nhere();\n");
    let renamed = fa.imports.iter().flat_map(|i| i.imported_symbols.iter()).find(|s| s.local_name == "here");
    assert!(renamed.is_some_and(|s| s.remote() == "always_here"), "{:?}", fa.imports);
}

#[test]
fn literal_list_return_after_a_comment() {
    let fa = build_fa("foreach my $tag (_tags()) {\n  *$tag = sub { 1 };\n}\nsub _tags { return # c\n qw(div span); }\n");
    assert!(has_sub(&fa, "div") && has_sub(&fa, "span"));
}

// ---- accessor generators ----

#[test]
fn postfix_for_list_after_a_comment() {
    let fa = build_fa(
        "package My::App;\nuse base 'Class::Accessor::Grouped';\n__PACKAGE__->mk_classdata($_) # c\n for qw/alpha beta/;\n",
    );
    assert!(has_method(&fa, "alpha") && has_method(&fa, "beta"));
}

// ---- string folding ----

#[test]
fn string_concat_operand_after_a_comment() {
    let src = "package P;\nuse Mojo::Base 'Mojolicious::Plugin';\nsub register {\n    my ($self, $app) = @_;\n    for my $name (qw(user)) {\n        $app->helper('find_' . # c\n $name => sub { 3 });\n    }\n}\n1;\n";
    let fa = build_fa(src);
    assert!(fa.symbols().iter().any(|s| s.name == "find_user"));
}

#[test]
fn parenthesized_string_after_a_comment() {
    let src = "package P;\nuse Mojo::Base 'Mojolicious::Plugin';\nsub register {\n    my ($self, $app) = @_;\n    $app->helper(( # c\n 'single') => sub { 4 });\n}\n1;\n";
    let fa = build_fa(src);
    assert!(fa.symbols().iter().any(|s| s.name == "single"));
}

// ---- returns and statements ----

#[test]
fn return_value_after_a_comment() {
    let fa = build_fa("sub f { return # c\n 'x' }\n");
    assert_eq!(fa.sub_return_type_at_arity("f", None), Some(InferredType::String));
}

#[test]
fn delegated_return_after_a_comment() {
    let fa = build_fa("sub inner { return { host => 1 } }\nsub outer { return # c\n inner() }\nmy $h = outer();\n$h->{host};\n");
    let access = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "host" && matches!(r.kind, RefKind::HashKeyAccess { .. }) && r.span.start.row == 4)
        .expect("access");
    assert!(
        matches!(access.hash_key_owner(), Some(HashKeyOwner::Sub { name, .. }) if name == "inner"),
        "{access:?}"
    );
}

#[test]
fn implicit_return_hash_owner_before_a_trailing_comment() {
    let fa = build_fa("sub f {\n    { host => 1 }\n    # trailing\n}\n");
    let def = fa.symbols().iter().find(|s| s.name == "host").expect("host def");
    assert!(
        matches!(&def.detail, SymbolDetail::HashKeyDef { owner: HashKeyOwner::Sub { name, .. }, .. } if name == "f"),
        "{:?}",
        def.detail
    );
}

#[test]
fn anon_sub_last_statement_before_a_trailing_comment() {
    let fa = build_fa("package Foo;\nuse Mojo::Base -base;\nhas n => sub {\n    42;\n    # trailing\n};\n1;\n");
    assert_eq!(fa.sub_return_type_at_arity("n", Some(0)), Some(InferredType::Numeric));
}

#[test]
fn match_operand_after_a_comment() {
    let fa = build_fa("my $re;\nmy $s;\n$s =~ # c\n $re;\n");
    assert_eq!(fa.inferred_type_via_bag("$re", Point::new(3, 10)), Some(InferredType::Regexp));
}

