//! The constant-fold table keys by what a name resolves to, not its
//! spelling: lexicals by their declaration, package variables and
//! `use constant` names by package.

use super::*;

/// `(row, invocant class)` of every method call named `name`.
fn calls(fa: &FileAnalysis, name: &str) -> Vec<(usize, Option<String>)> {
    fa.refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == name)
        .map(|r| (r.span.start.row, fa.method_call_invocant_classes(r, None).single().map(str::to_string)))
        .collect()
}

fn call_rows(fa: &FileAnalysis, name: &str) -> Vec<usize> {
    calls(fa, name).into_iter().map(|(row, _)| row).collect()
}

#[test]
fn lexical_fold_does_not_leak_into_another_sub() {
    let fa = build_fa(
        "package A;\n\
         sub a { my $self = shift; my $m = 'process'; $self->$m() }\n\
         sub b { my $self = shift; my $m = shift; $self->$m() }\n",
    );
    assert_eq!(call_rows(&fa, "process"), vec![1]);
}

#[test]
fn folded_invocant_class_does_not_leak_into_another_sub() {
    let fa = build_fa(
        "package A;\n\
         sub a { my $c = 'B'; $c->go }\n\
         sub b { my $c = shift; $c->go }\n",
    );
    assert_eq!(calls(&fa, "go"), vec![(1, Some("B".into())), (2, None)]);
}

#[test]
fn inner_declaration_shadows_outer_fold() {
    let fa = build_fa(
        "package A;\n\
         sub a { my $self = shift; my $m = 'outer'; { my $m = shift; $self->$m } $self->$m }\n",
    );
    assert_eq!(call_rows(&fa, "outer"), vec![1]);
    assert_eq!(fa.refs().iter().filter(|r| r.target_name == "outer").count(), 1);
}

#[test]
fn loop_variable_fold_ends_with_the_loop() {
    let fa = build_fa(
        "package A;\n\
         sub a { my $self = shift; for my $m (qw(x y)) { $self->$m } }\n\
         sub b { my $self = shift; my $m = shift; $self->$m }\n",
    );
    assert_eq!(call_rows(&fa, "x"), vec![1]);
    assert_eq!(call_rows(&fa, "y"), vec![1]);
}

#[test]
fn unfoldable_write_clears_the_fold() {
    let fa = build_fa(
        "package A;\n\
         sub a { my $s = shift; my $m = 'x'; $m = $s->pick; $s->$m() }\n\
         sub b { my $s = shift; my @l = qw(y); @l = $s->names; $s->$_ for @l }\n\
         sub c { my $s = shift; my $m = 'z'; ($m) = $s->pick; $s->$m }\n",
    );
    assert!(call_rows(&fa, "x").is_empty());
    assert!(call_rows(&fa, "y").is_empty());
    assert!(call_rows(&fa, "z").is_empty());
}

#[test]
fn literal_reassignment_still_folds() {
    let fa = build_fa("package A;\nsub a { my $s = shift; my $m = 'x'; $m = 'y'; $s->$m() }\n");
    assert!(call_rows(&fa, "x").is_empty());
    assert_eq!(call_rows(&fa, "y"), vec![1]);
}

#[test]
fn isa_is_per_package() {
    let fa = build_fa(
        "package A; our @ISA = ('X');\n\
         package B; our @ISA = ('Y');\n\
         package C; our @ISA = ('Z');\n",
    );
    assert_eq!(fa.declared_parents("A"), vec!["X".to_string()]);
    assert_eq!(fa.declared_parents("B"), vec!["Y".to_string()]);
    assert_eq!(fa.declared_parents("C"), vec!["Z".to_string()]);
}

#[test]
fn use_constant_is_per_package_and_replaces() {
    let fa = build_fa(
        "package A; use constant M => 'x';\n\
         package B; use constant M => 'y';\n\
         sub g { my $s = shift; $s->$_ for (M) }\n",
    );
    assert!(call_rows(&fa, "x").is_empty());
    assert_eq!(call_rows(&fa, "y"), vec![2]);
}

#[test]
fn folded_literal_is_not_refolded_as_a_name() {
    let fa = build_fa(
        "package A; use constant FOO => 'go'; use constant go => 'new';\n\
         sub f { my $s = shift; $s->$_ for (FOO) }\n",
    );
    assert_eq!(call_rows(&fa, "go"), vec![1]);
    assert!(call_rows(&fa, "new").is_empty());
}

#[test]
fn qualified_and_our_spellings_are_one_package_variable() {
    let fa = build_fa(
        "package A;\n\
         our @names = qw(x);\n\
         sub f { my $s = shift; $s->$_ for @A::names }\n",
    );
    assert_eq!(call_rows(&fa, "x"), vec![2]);
}
