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

/// A function-like macro call in a guard is an opaque term, not a parse
/// failure: the boolean structure around it still decides.
#[test]
fn a_call_shaped_term_is_opaque_inside_its_structure() {
    let guard = t("defined __cplusplus && (__GNUC_PREREQ (4, 4) || __glibc_clang_prereq (3, 5))");
    assert_eq!(
        classify(&[guard.clone()], &cfg(&[], &[])),
        Reachability::Unreachable { reason: "__cplusplus undefined".into() }
    );
    assert!(matches!(classify(&[guard], &cfg(&["__cplusplus"], &[])), Reachability::Unknown { .. }));
}

#[test]
fn comparisons_and_arithmetic_are_opaque_terms() {
    for expr in ["defined(A) && __GNUC__ >= 4", "defined(A) && (X + 1) * 2 != 3", "defined(A) && (B ? 1 : 0)", "defined(A) && -1"] {
        assert_eq!(
            classify(&[t(expr)], &cfg(&[], &[])),
            Reachability::Unreachable { reason: "A undefined".into() },
            "{expr}"
        );
        assert!(matches!(classify(&[t(expr)], &cfg(&["A"], &[])), Reachability::Unknown { .. }), "{expr}");
    }
}

#[test]
fn a_line_splice_continues_the_condition() {
    let r = classify(&[t("defined(A) && \\ defined(B)")], &cfg(&["A"], &[]));
    assert_eq!(r, Reachability::Unreachable { reason: "B undefined".into() });
}

/// A condition that does not parse is no answer, whatever it starts with.
#[test]
fn a_malformed_condition_is_unknown() {
    for expr in ["0 && defined(", "0 && F(1", "0 && )"] {
        assert!(matches!(classify(&[t(expr)], &cfg(&[], &[])), Reachability::Unknown { .. }), "{expr}");
    }
}

fn sites(defs: &[(&str, &[CondTerm])]) -> DefineSites {
    let mut s = DefineSites::default();
    for (name, trail) in defs {
        s.add(name, trail);
    }
    s
}

fn defined(name: &str) -> CondTerm {
    CondTerm { test: CondTest::Defined(name.to_string()), negated: false }
}

/// A knob `#define`d only where the toolchain rules it out cannot be on, so
/// what it guards is unreachable — and the label names the root cause.
#[test]
fn a_knob_defined_only_under_a_false_guard_settles_out() {
    let only_cpp = [t("defined __cplusplus && __GNUC_PREREQ (4, 4)")];
    let c = KnownConfig::from_defines(sites(&[("KNOB", &only_cpp)]), Vec::<String>::new());
    assert_eq!(
        classify(&[defined("KNOB")], &c),
        Reachability::Unreachable { reason: "KNOB undefined (__cplusplus undefined)".into() }
    );
    let not_knob = CondTerm { test: CondTest::Defined("KNOB".into()), negated: true };
    assert_eq!(classify(&[not_knob], &c), Reachability::Active);
    // Under C++ the knob may be on: no answer.
    let cpp = KnownConfig::from_defines(sites(&[("KNOB", &only_cpp)]), ["__cplusplus"]);
    assert!(matches!(classify(&[defined("KNOB")], &cpp), Reachability::Unknown { .. }));
}

/// Settling is a fixed point: a knob defined only under a settled knob
/// settles too, citing the same root.
#[test]
fn settling_chains_to_the_root_cause() {
    let c = KnownConfig::from_defines(
        sites(&[("KNOB2", &[defined("KNOB")]), ("KNOB", &[defined("__cplusplus")])]),
        Vec::<String>::new(),
    );
    assert_eq!(
        classify(&[defined("KNOB2")], &c),
        Reachability::Unreachable { reason: "KNOB2 undefined (__cplusplus undefined)".into() }
    );
}

/// One site that may hold keeps a knob: settling never guesses.
#[test]
fn a_knob_with_one_possible_site_stays() {
    let c = KnownConfig::from_defines(
        sites(&[("KNOB", &[defined("__cplusplus")]), ("KNOB", &[defined("OTHER")]), ("OTHER", &[defined("X")]), ("X", &[])]),
        Vec::<String>::new(),
    );
    assert!(matches!(classify(&[defined("KNOB")], &c), Reachability::Unknown { .. }));
    // An unconditional site anywhere makes it ON, whatever order the sites came in.
    let on = KnownConfig::from_defines(sites(&[("K", &[defined("__cplusplus")]), ("K", &[])]), Vec::<String>::new());
    assert_eq!(classify(&[defined("K")], &on), Reachability::Active);
}
