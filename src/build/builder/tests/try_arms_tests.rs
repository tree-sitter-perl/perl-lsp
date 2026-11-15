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
