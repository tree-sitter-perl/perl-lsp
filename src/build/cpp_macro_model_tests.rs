//! Config-variant macro model: guard eval + reachability classification.

use super::*;

/// A trail term as an `#if` would write it.
fn t(expr: &str) -> CondTerm {
    CondTerm { test: CondTest::Expr(expr.to_string()), negated: false }
}

fn cfg(defined: &[&str], universe: &[&str]) -> KnownConfig {
    KnownConfig::new(
        defined.iter().map(|s| s.to_string()).collect(),
        // the universe always contains everything defined (a defined macro is
        // trivially in the closure's definition set)
        universe.iter().chain(defined.iter()).map(|s| s.to_string()).collect(),
    )
}

#[test]
fn unconditional_define_is_active() {
    assert_eq!(classify(&[], &KnownConfig::default()), Reachability::Active);
}

#[test]
fn defined_guard_active_when_known_on() {
    let r = classify(&[t("defined(HAS)")], &cfg(&["HAS"], &[]));
    assert_eq!(r, Reachability::Active);
}

#[test]
fn platform_macro_never_defined_is_unreachable() {
    // WIN32 is neither predefined nor #defined anywhere in the closure → a
    // platform macro absent on this target → provably false.
    let r = classify(&[t("defined(WIN32)")], &cfg(&[], &["HAS"]));
    assert_eq!(r, Reachability::Unreachable { reason: "WIN32 undefined".into() });
}

#[test]
fn knob_defined_somewhere_but_not_on_is_unknown() {
    // HAS_NON_INT_BITFIELDS is a Configure knob we've SEEN defined in the
    // closure (universe) but can't resolve here → UNKNOWN, not guessed.
    let r = classify(&[t("defined(HAS)")], &cfg(&[], &["HAS"]));
    assert_eq!(r, Reachability::Unknown { guard: "defined(HAS)".into() });
}

#[test]
fn negated_knob_is_unknown_too() {
    // `!defined(HAS)` with HAS a known-but-unresolved knob is equally unknown.
    let r = classify(&[t("!defined(HAS)")], &cfg(&[], &["HAS"]));
    assert!(matches!(r, Reachability::Unknown { .. }));
}

#[test]
fn negated_active_knob_is_unreachable() {
    let r = classify(&[t("!defined(HAS)")], &cfg(&["HAS"], &[]));
    assert_eq!(r, Reachability::Unreachable { reason: "HAS defined".into() });
}

#[test]
fn conjunction_three_valued() {
    // !defined(WIN32) && defined(HAS): WIN32 absent → true; HAS knob → unknown.
    let r = classify(
        &[t("!defined(WIN32)"), t("defined(HAS)")],
        &cfg(&[], &["HAS"]),
    );
    assert!(matches!(r, Reachability::Unknown { .. }));
    // with WIN32 present the whole thing is provably false regardless of HAS.
    let r2 = classify(
        &[t("!defined(WIN32)"), t("defined(HAS)")],
        &cfg(&["WIN32"], &["HAS"]),
    );
    assert_eq!(r2, Reachability::Unreachable { reason: "WIN32 defined".into() });
}

#[test]
fn rank_orders_active_unknown_unreachable() {
    // The sort key goto-def/hover ranking rides on: ACTIVE < UNKNOWN < UNREACHABLE.
    let active = Reachability::Active;
    let unknown = Reachability::Unknown { guard: "defined(CFG)".into() };
    let unreachable = Reachability::Unreachable { reason: "WIN32 undefined".into() };
    assert!(active.rank() < unknown.rank());
    assert!(unknown.rank() < unreachable.rank());
    assert_eq!(unreachable.label().as_deref(), Some("unreachable: WIN32 undefined"));
}
