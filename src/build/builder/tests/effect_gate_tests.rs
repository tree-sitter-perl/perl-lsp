//! How a call to a sub of the file ends, inferred from its body, and the
//! joins whose paths run past such a call (`docs/adr/cfg-tier.md` §Effects).

use super::*;
use crate::model::witnesses::{
    BagContext, Ends, FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry, WitnessAttachment,
};

fn sub_id(fa: &FileAnalysis, name: &str) -> SymbolId {
    fa.symbols().iter().find(|s| s.name == name && matches!(s.kind, SymKind::Sub)).unwrap_or_else(|| panic!("sub {name}")).id
}

fn ends_of(fa: &FileAnalysis, name: &str) -> Ends {
    fa.symbol_ends(sub_id(fa, name))
}

/// The registry's own answer for `att`, before any public wrapper reads
/// "nothing" as the top.
fn raw_effects(fa: &FileAnalysis, att: &WitnessAttachment) -> ReducedValue {
    let ctx = BagContext {
        scopes: &fa.scopes,
        symbols: fa.symbols(),
        package_framework: &fa.packages,
        module_index: None,
        package_parents: &fa.packages,
        app_surface_consumers: &fa.plugin.app_surface_consumers,
        lattice: crate::model::conventions::lattice_for(&fa.language),
        member_spaces: crate::model::conventions::member_spaces_for(&fa.language),
    };
    let q = ReducerQuery {
        args: Vec::new(),
        attachment: att,
        point: None,
        framework: FrameworkFact::Plain,
        arity_hint: None,
        receiver: None,
        context: Some(&ctx),
    };
    ReducerRegistry::with_defaults().query(&fa.witnesses, &q)
}

/// A sub of the file that dies narrows past a call to it, as `die` does.
#[test]
fn a_local_die_helper_narrows_past_its_call() {
    let fa = build_fa(
        "sub fatal { my ($m) = @_; die $m }\nsub f {\nmy ($c) = @_;\nmy $x = $c ? Foo->new : undef;\nfatal('no') unless defined $x;\nreturn $x;\n}\n",
    );
    assert_eq!(ends_of(&fa, "fatal"), Ends::UNWINDS);
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(5, 0)), Some(InferredType::ClassName("Foo".into())));
}

/// A sub of the file named like a dying builtin but that comes back is no
/// leave: past its call the value coming in stands.
#[test]
fn a_local_croak_that_returns_narrows_nothing() {
    let fa = build_fa(
        "sub croak { warn @_ }\nsub f {\nmy ($c) = @_;\nmy $x = $c ? Foo->new : undef;\ncroak('no') unless defined $x;\nreturn $x;\n}\n",
    );
    assert!(ends_of(&fa, "croak").contains(Ends::COMPLETES));
    let foo = InferredType::ClassName("Foo".into());
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(5, 0)), Some(InferredType::Optional(Box::new(foo))));
}

/// A write on a path that runs into a call that never comes back reaches
/// no join.
#[test]
fn a_write_before_a_dying_call_reaches_no_join() {
    let fa = build_fa("sub fatal { die 'x' }\nsub f {\nmy ($c) = @_;\nmy $x = 1;\nif ($c) { $x = 'a'; fatal() }\nreturn $x;\n}\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(5, 0)), Some(InferredType::Numeric));
}

/// A wrapper ends as what it calls does, through any number of hops.
#[test]
fn a_wrapper_of_a_dying_sub_does_not_come_back() {
    let fa = build_fa("sub fatal { die 'x' }\nsub wrap { fatal() }\nsub f {\nmy ($c) = @_;\nmy $x = 1;\nif ($c) { $x = 'a'; wrap() }\nreturn $x;\n}\n");
    assert!(!ends_of(&fa, "wrap").contains(Ends::COMPLETES));
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(6, 0)), Some(InferredType::Numeric));
}

/// Merge condition (1): a three-sub cycle with no way out has no way of
/// ending, whichever sub the question starts from.
#[test]
fn a_cycle_with_no_way_out_never_comes_back_from_any_entry() {
    let fa = build_fa(
        "sub a { b() }\nsub b { c() }\nsub c { a() }\nsub f {\nmy ($k) = @_;\nmy $x = 1;\nif ($k) { $x = 'a'; a() } elsif ($k > 1) { $x = 'b'; b() } else { $x = 'c'; c() }\nreturn $x;\n}\n",
    );
    for name in ["a", "b", "c"] {
        assert_eq!(ends_of(&fa, name), Ends::empty(), "{name}");
    }
}

/// Merge condition (1), the memo half: a sub asked about while another is
/// assumed to have no way of ending is asked again once that one has its
/// answer. `b` comes back through `a`, which comes back on its own; the
/// path past `b()` must reach the join even though `b` was first worked
/// out inside `a`'s question.
#[test]
fn an_answer_made_under_a_cycle_assumption_is_not_reused() {
    let fa = build_fa(
        "sub a { return 1 if $_[0]; b() }\nsub b { a() }\nsub f {\nmy ($k) = @_;\nmy $x = 1;\nif ($k) { $x = 'a'; a() } elsif ($k > 1) { $x = Foo->new; b() }\nreturn $x;\n}\n",
    );
    assert!(ends_of(&fa, "a").contains(Ends::COMPLETES));
    assert!(ends_of(&fa, "b").contains(Ends::COMPLETES));
    let t = fa.inferred_type_via_bag("$x", Point::new(6, 0));
    assert!(format!("{t:?}").contains("Foo"), "the path past `b()` reaches the join: {t:?}");
}

/// Merge condition (2): a cycle with one sub that comes back on its own
/// comes back from every entry.
#[test]
fn a_cycle_with_a_base_case_comes_back() {
    let fa = build_fa(
        "sub a { b() }\nsub b { return 1 if $_[0]; c() }\nsub c { a() }\nsub f {\nmy ($k) = @_;\nmy $x = 1;\nif ($k) { $x = 'a'; a() }\nreturn $x;\n}\n",
    );
    for name in ["a", "b", "c"] {
        assert!(ends_of(&fa, name).contains(Ends::COMPLETES), "{name}");
    }
}

/// Merge condition (3): a chase cut by the depth cap, and a call whose
/// callee is an open set, both answer the top: they come back, whatever
/// the cut-off end of the chain does.
#[test]
fn a_depth_cap_and_an_open_set_answer_the_top() {
    let mut src = String::new();
    for i in 0..600 {
        src.push_str(&format!("sub s{i} {{ s{}() }}\n", i + 1));
    }
    src.push_str("sub s600 { die 'x' }\nsub f {\nmy ($k, $o) = @_;\nmy $x = 1;\nif ($k) { $x = 'a'; s0() }\n$o->m();\nreturn $x;\n}\n");
    let fa = build_fa(&src);
    assert_eq!(ends_of(&fa, "s600"), Ends::UNWINDS);
    assert!(ends_of(&fa, "s0").contains(Ends::COMPLETES), "a cap hit is no evidence, never no way of ending");
    let call = fa.refs().iter().find(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "m").expect("$o->m");
    let span = fa.witnesses.all().iter().find(|w| w.span.start == Point::new(604, 0) && matches!(w.attachment, WitnessAttachment::Expr(_)));
    let at = span.map(|w| w.attachment.clone()).unwrap_or(WitnessAttachment::Expr(call.span));
    let ctx_ends = crate::model::witnesses::query_expr_ends(&fa.witnesses, &at, None);
    assert_eq!(ctx_ends, Ends::all(), "a receiver's dispatch is an open set");
}

/// Merge condition (4): an effect question always gets an effect answer:
/// nothing stated, a cycle member and a capped chase alike.
#[test]
fn no_effect_question_answers_nothing() {
    let fa = build_fa("sub quiet { 1 }\nsub a { b() }\nsub b { a() }\nsub f {\nmy ($k) = @_;\nmy $x = 1;\nif ($k) { $x = 'a'; a() }\nreturn $x;\n}\n");
    for name in ["quiet", "a", "b", "f"] {
        let v = raw_effects(&fa, &WitnessAttachment::SymbolEffects(sub_id(&fa, name)));
        assert!(matches!(v, ReducedValue::Effects(_)), "{name}: {v:?}");
    }
    let v = raw_effects(&fa, &WitnessAttachment::SymbolEffects(sub_id(&fa, "quiet")));
    assert_eq!(v, ReducedValue::Effects(Ends::all()), "nothing stated is the top");
}
