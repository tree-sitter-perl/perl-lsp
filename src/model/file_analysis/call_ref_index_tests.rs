//! Pins for the `RefTable` call index — the call spanning exactly an
//! expression, which chain-receiver dispatch in
//! `resolve_receiver` reads. Only call-shaped refs go in, equal
//! spans keep the first ref, and a receiver is the call spanning it
//! exactly, so no lookup can land on the outer call itself.

use super::*;
use tree_sitter::{Parser, Point};

fn parse(source: &str) -> FileAnalysis {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    crate::build::builder::build(&tree, source.as_bytes())
}

/// Find the index of the (last) ref whose `target_name` matches and
/// whose kind matches the predicate. Test helper — multiple refs may
/// share a name across a chain (e.g. `Foo->new->new` would have two
/// `new` MethodCalls), but our fixtures keep names unique.
fn ref_idx<F: Fn(&RefKind) -> bool>(fa: &FileAnalysis, name: &str, kind_pred: F) -> usize {
    fa.refs
        .iter()
        .enumerate()
        .find(|(_, r)| r.target_name == name && kind_pred(&r.kind))
        .map(|(i, _)| i)
        .unwrap_or_else(|| {
            panic!(
                "no ref with target_name={:?} matching predicate; refs: {:?}",
                name,
                fa.refs.iter().map(|r| (&r.target_name, &r.kind)).collect::<Vec<_>>(),
            )
        })
}

fn is_method_call(k: &RefKind) -> bool {
    matches!(k, RefKind::MethodCall { .. })
}

fn is_function_call(k: &RefKind) -> bool {
    matches!(k, RefKind::FunctionCall { .. })
}

/// `Foo->new->bar()`: `bar`'s receiver is exactly the inner `Foo->new`
/// call, never the outer `bar`.
#[test]
fn the_receiver_is_the_call_spanning_it() {
    let fa = parse(
        r#"
package Foo;
sub new { bless {}, shift }
sub bar { 1 }
package main;
Foo->new->bar();
"#,
    );
    let bar = &fa.refs[ref_idx(&fa, "bar", is_method_call)];
    let RefKind::MethodCall { invocant_span: Some(span), .. } = &bar.kind else {
        panic!("bar is a MethodCall");
    };
    assert_eq!(fa.refs.call_spanning(*span), Some(ref_idx(&fa, "new", is_method_call)));
    assert_eq!(fa.receiver_classes(bar, None, ReceiverPolicy::Navigate).single().map(str::to_string).as_deref(), Some("Foo"));
}

/// `make_b()->touch()`: the function ref spans only its name, so no
/// call ref spans the receiver; the receiver's value is the whole call's
/// `Expr`.
#[test]
fn a_function_call_receiver_reads_the_whole_calls_value() {
    let fa = parse(
        r#"
sub make_b { bless {}, 'B' }
make_b()->touch();
"#,
    );
    let touch = &fa.refs[ref_idx(&fa, "touch", is_method_call)];
    let RefKind::MethodCall { invocant_span: Some(span), .. } = &touch.kind else {
        panic!("touch is a MethodCall");
    };
    assert_eq!(fa.refs.call_spanning(*span), None);
    assert_eq!(fa.receiver_classes(touch, None, ReceiverPolicy::Navigate).single().map(str::to_string).as_deref(), Some("B"));
}

/// A receiver no call spans falls through to its own shape: a bareword
/// class, or a variable's bag type.
#[test]
fn a_receiver_no_call_spans_resolves_by_its_shape() {
    let fa = parse(
        r#"
package Bar;
sub new { bless {}, shift }
sub m { 1 }
sub n { 1 }
package main;
Bar->n();
my $x = Bar->new;
$x->m();
"#,
    );
    for name in ["n", "m"] {
        let r = &fa.refs[ref_idx(&fa, name, is_method_call)];
        let RefKind::MethodCall { invocant_span: Some(span), .. } = &r.kind else {
            panic!("{name} is a MethodCall");
        };
        assert_eq!(fa.refs.call_spanning(*span), None);
        assert_eq!(fa.receiver_classes(r, None, ReceiverPolicy::Navigate).single().map(str::to_string).as_deref(), Some("Bar"));
    }
}

/// Two calls at the exact same span (a synthesized shape the parser
/// can't produce): the earlier ref wins.
#[test]
fn equal_span_first_write_wins() {
    let span = Span {
        start: Point::new(0, 0),
        end: Point::new(0, 10),
    };
    let call = |name: &str, inv: &str, col: usize| Ref {
        kind: RefKind::MethodCall {
            invocant: crate::model::conventions::Invocant::assume_canonical(inv),
            invocant_span: Some(Span {
                start: Point::new(0, 0),
                end: Point::new(0, 2),
            }),
            method_name_span: Span {
                start: Point::new(0, col),
                end: Point::new(0, col + 1),
            },
            member_op: None,
            named_by_string: false,
            constructs: false,
        },
        span,
        scope: ScopeId(0),
        target_name: name.into(),
        access: AccessKind::Read,
        binding: None,
        folded_from: None,
        arg_count: None,
    };
    let fa = FileAnalysis::new(crate::model::file_analysis::FileAnalysisParts {
        scopes: vec![Scope {
            id: ScopeId(0),
            parent: None,
            kind: ScopeKind::File,
            span: Span {
                start: Point::new(0, 0),
                end: Point::new(10, 0),
            },
            package: None,
            owner: None,
            implicit_receiver: false,
            closed: false,
            captures: Vec::new(),
            direct: false,
            declaration_rules: Vec::new(),
        }],
        refs: vec![call("first", "$a", 4), call("second", "$b", 6)],
        ..Default::default()
    });
    assert_eq!(fa.refs.call_spanning(span), Some(0));
}

/// A deep chain resolves in finite time: each hop's receiver is a
/// strictly narrower span.
#[test]
fn deep_chain_terminates() {
    let fa = parse(
        r#"
sub a { 1 }
a()->b()->c()->d()->e()->f();
"#,
    );
    let f = &fa.refs[ref_idx(&fa, "f", is_method_call)];
    let _ = fa.receiver_classes(f, None, ReceiverPolicy::Navigate);
}

/// Only call-shaped refs answer: a hash access spanning an expression
/// exactly is not a call.
#[test]
fn only_calls_answer() {
    let fa = parse(
        r#"
my $x = {};
my $k = $x->{key};
"#,
    );
    for r in fa.refs.iter() {
        if let Some(i) = fa.refs.call_spanning(r.span) {
            assert!(
                matches!(fa.refs[i].kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. }),
                "non-call kind in the call index: {:?}",
                fa.refs[i].kind,
            );
        }
    }
}

#[test]
fn longest_call_chain_counts_the_deepest_fluent_chain() {
    let fa = parse(
        r#"package P;
sub new { bless {}, shift }
sub a { $_[0] }
package main;
my $x = P->new->a->a->a;
my $y = P->new->a;
foo(P->new->a->a);
"#,
    );
    // `P->new->a->a->a`: `new` plus three `a` calls, all opening at `P`.
    assert_eq!(fa.refs.longest_call_chain(), 4);
}

/// A chain that starts with a function call (`create_user(5)->name->baz`):
/// `baz`'s receiver is `create_user(5)->name`, so `baz` dispatches on what
/// `name` returns, not on what `create_user` returns.
#[test]
fn a_chain_from_a_function_call_dispatches_on_the_last_hop() {
    let fa = parse(
        "package Bar; sub new { bless {}, shift } sub baz {}
package Foo; sub new { bless {}, shift } sub name { return Bar->new }
package main;
sub create_user { return Foo->new }
create_user(5)->name->baz;
",
    );
    let class = |name: &str| fa.receiver_classes(&fa.refs[ref_idx(&fa, name, is_method_call)], None, crate::model::file_analysis::ReceiverPolicy::Navigate).single().map(str::to_string);
    assert_eq!(class("name").as_deref(), Some("Foo"), "the function call is the whole receiver");
    assert_eq!(class("baz").as_deref(), Some("Bar"), "the receiver is the chain's last hop");
}

/// When the last hop's value is unknown, the receiver is unknown: the
/// call at the chain's start must not stand in for it. `name` has no
/// return the file can see, so `baz` has no class — never `Foo`, which is
/// what `create_user` returns.
#[test]
fn an_untyped_hop_is_never_answered_by_the_chains_first_call() {
    let fa = parse(
        "package Foo; sub new { bless {}, shift } sub name { $_[0]{name} } sub baz {}
package main;
sub create_user { return Foo->new }
create_user(5)->name->baz;
my $u = Foo->new;
$u->name->baz;
",
    );
    let classes: Vec<_> = fa
        .refs
        .iter()
        .filter(|r| is_method_call(&r.kind) && r.target_name == "baz")
        .map(|r| fa.receiver_classes(r, None, crate::model::file_analysis::ReceiverPolicy::Navigate).single().map(str::to_string))
        .collect();
    assert_eq!(classes, vec![None, None], "an untyped hop leaves its receiver untyped");
}
