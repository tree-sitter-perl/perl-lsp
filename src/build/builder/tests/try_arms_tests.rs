//! The arms of `try`/`catch`/`finally` and `eval`, and the joins past them
//! (`docs/adr/cfg-tier.md` §Try).

use super::*;

fn class(n: &str) -> InferredType {
    InferredType::ClassName(n.into())
}

fn either(a: &str, b: &str) -> Option<InferredType> {
    crate::model::file_analysis::join_all(&[class(a), class(b)], &crate::model::file_analysis::Unordered)
}

fn completes(fa: &FileAnalysis, name: &str) -> bool {
    let sid = fa.symbols().iter().find(|s| s.name == name && matches!(s.kind, SymKind::Sub)).unwrap().id;
    fa.symbol_ends(sid).contains(crate::model::witnesses::Ends::COMPLETES)
}

/// Past a `try`, a variable holds what the tried body leaves it, or what a
/// handler does: a handler that rebinds it shadows the value coming in.
#[test]
fn a_try_joins_its_body_and_its_handler() {
    let fa = build_fa(
        "use feature 'try';\nsub f {\nmy $x = Foo->new;\ntry { $x = Bar->new; g() } catch ($e) { $x = Baz->new }\nreturn $x;\n}\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 0)), either("Bar", "Baz"));
}

/// A handler starts from any point the tried body could have died at: the
/// value coming in, or any write the body made before.
#[test]
fn a_handler_starts_from_where_the_body_could_stop() {
    let fa = build_fa(
        "use feature 'try';\nsub f {\nmy $x = Foo->new;\ntry { $x = Bar->new; g() } catch ($e) {\nmy $y = $x;\n}\n}\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 0)), either("Foo", "Bar"));
}

/// A `finally` runs on every path out, so its write is what comes past.
#[test]
fn a_finally_write_is_what_comes_past() {
    let fa = build_fa(
        "use feature 'try';\nsub f {\nmy $x = Foo->new;\ntry { $x = Bar->new } catch ($e) { } finally { $x = Baz->new }\nreturn $x;\n}\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 0)), Some(class("Baz")));
}

/// An `eval` swallows a throw, so past it comes the body's end or any point
/// the body could have died at.
#[test]
fn an_eval_joins_its_end_and_where_it_could_stop() {
    let fa = build_fa("sub f {\nmy $x = Foo->new;\neval { $x = Bar->new; g(); 1 };\nreturn $x;\n}\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(3, 0)), either("Foo", "Bar"));
}

/// A throw a `try` or an `eval` catches does not leave the sub.
#[test]
fn a_caught_throw_does_not_end_the_sub() {
    let fa = build_fa(
        "use feature 'try';\nsub f { try { die 'x' } catch ($e) { } return 1 }\nsub g { eval { die 'x' }; 1 }\nsub h { die 'x' }\n",
    );
    assert!(completes(&fa, "f"));
    assert!(completes(&fa, "g"));
    assert!(!completes(&fa, "h"));
}

/// A throw out of a handler is not caught by its own `try`.
#[test]
fn a_handler_throw_leaves_the_sub() {
    let fa = build_fa("use feature 'try';\nsub f { try { 1 } catch ($e) { die $e } finally { } die 'y' }\n");
    assert!(!completes(&fa, "f"));
}

/// A body stops only where something in it may die: a write nothing that
/// may die follows reaches no handler, and in a loop, what runs again
/// follows it.
#[test]
fn a_handler_starts_only_from_writes_a_raise_follows() {
    let at_handler = |body: &str| {
        let fa = build_fa(&format!(
            "use feature 'try';\nsub f {{\nmy $x = Foo->new;\ntry {{ {body} }} catch ($e) {{\nmy $y = $x;\n}}\n}}\n"
        ));
        fa.inferred_type_via_bag("$x", Point::new(4, 0))
    };
    assert_eq!(at_handler("$x = Bar->new;"), Some(class("Foo")), "the call is the write's own right side");
    assert_eq!(at_handler("$x = Bar->new; my $n = 1 / $_[0];"), either("Foo", "Bar"), "a division may die");
    assert_eq!(at_handler("g(); $x = Bar->new;"), Some(class("Foo")));
    assert_eq!(at_handler("my $b = Bar->new; for (1..2) { $x = $b; } my $n = 1;"), Some(class("Foo")));
    assert_eq!(at_handler("for (1..2) { $x = Bar->new; }"), either("Foo", "Bar"), "the next pass constructs again");
    assert_eq!(at_handler("for (1..2) { g(); $x = Bar->new; }"), either("Foo", "Bar"), "the next pass calls again");
    assert_eq!(at_handler("$x = Bar->new; if ($_[0]) { die 'x' }"), either("Foo", "Bar"));
    // An `eval` lands past itself where the body completes or stops.
    let fa = build_fa("sub f {\nmy $x = Foo->new;\neval { $x = Bar->new; 1 };\nreturn $x;\n}\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(3, 0)), either("Foo", "Bar"));
}

/// Only the paths that complete go on past a `finally`; one that unwinds
/// runs it and keeps unwinding, so a handler's value never comes past it
/// unless the handler completes.
#[test]
fn only_completing_paths_go_on_past_a_finally() {
    let past = |body: &str, fin: &str| {
        let fa = build_fa(&format!(
            "use feature 'try';\nsub f {{\nmy $x = Foo->new;\ntry {{ {body} }} finally {{ {fin} }}\nreturn $x;\n}}\n"
        ));
        fa.inferred_type_via_bag("$x", Point::new(4, 0))
    };
    let body = "$x = Bar->new; g();";
    assert_eq!(past(body, "if ($_[0]) { $x = Baz->new }"), either("Bar", "Baz"), "the entry value unwinds");
    assert_eq!(past(body, "if ($_[0]) { $x = Baz->new } log_it();"), either("Bar", "Baz"));
    assert_eq!(past(body, "log_it();"), Some(class("Bar")));
    assert_eq!(past(body, "$x = Baz->new;"), Some(class("Baz")));
}
