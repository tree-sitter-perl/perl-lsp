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
    let fa = build_fa("sub quiet { 1 + }\nsub a { b() }\nsub b { a() }\nsub f {\nmy ($k) = @_;\nmy $x = 1;\nif ($k) { $x = 'a'; a() }\nreturn $x;\n}\n");
    for name in ["quiet", "a", "b", "f"] {
        let v = raw_effects(&fa, &WitnessAttachment::SymbolEffects(sub_id(&fa, name)));
        assert!(matches!(v, ReducedValue::Effects(_)), "{name}: {v:?}");
    }
    let v = raw_effects(&fa, &WitnessAttachment::SymbolEffects(sub_id(&fa, "quiet")));
    assert_eq!(v, ReducedValue::Effects(Ends::all()), "a body the parser could not read states nothing: the top");
}

/// A sub another file defines ends as that file alone says, read from the
/// index's resident table: one that dies there narrows past its call here;
/// one that comes back, or whose end runs through a callee elsewhere
/// again, does not. Re-registering the provider replaces its answer.
#[test]
fn a_sub_another_file_defines_ends_as_that_file_says() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let provider = |body: &str| format!("package Util;\nuse Exporter 'import';\nour @EXPORT = ('fatal');\nsub fatal {{ {body} }}\n1;\n");
    let consumer = "package C;\nuse Util;\nsub f {\nmy ($c) = @_;\nmy $x = $c ? Foo->new : undef;\nfatal('no') unless defined $x;\nreturn $x;\n}\n";
    let fa = build_fa(consumer);
    let foo = InferredType::ClassName("Foo".into());
    let optional = Some(InferredType::Optional(Box::new(foo.clone())));
    let idx = ModuleIndex::new_for_test();
    let path = std::path::PathBuf::from("/tmp/Util.pm");
    let at = |idx: &ModuleIndex| fa.inferred_type_via_bag_ctx("$x", crate::model::witnesses::ReadPoint::position(Point::new(6, 0)), Some(idx));
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(6, 0)), optional, "with no index the call may come back");
    idx.register_workspace_module(path.clone(), Arc::new(build_fa(&provider("die @_"))));
    assert_eq!(at(&idx), Some(foo), "the provider's `fatal` dies");
    idx.register_workspace_module(path.clone(), Arc::new(build_fa(&provider("warn @_"))));
    assert_eq!(at(&idx), optional, "re-registered, it comes back");
    idx.register_workspace_module(path, Arc::new(build_fa(&provider("Other::bail(@_)"))));
    assert_eq!(at(&idx), optional, "its end runs through a callee elsewhere: the top");
}

/// A test whose only way out is past a call drops no path when the call
/// comes back, so past it the value coming in stands: the call's arm,
/// which the test proves undef, is no evidence that the value is undef.
#[test]
fn a_guard_left_only_through_a_returning_call_narrows_nothing() {
    let fa = build_fa("sub f {\nmy ($p) = @_;\nmy $x = $p->{x};\nif (!defined $x) { Other::warn_about('x') }\nreturn $x->id;\n}\n");
    let t = fa.inferred_type_via_bag("$x", Point::new(4, 7));
    assert!(!matches!(t, Some(InferredType::Undef) | Some(InferredType::Optional(_))), "{t:?}");
}

/// A method call gates like a function call: the receiver's class
/// resolves the name, and a method that dies narrows past its call.
#[test]
fn a_method_that_dies_narrows_past_its_call() {
    let fa = build_fa(
        "package P;\nsub fail { die $_[1] }\nsub f {\nmy ($self, $c) = @_;\nmy $x = $c ? Foo->new : undef;\n$self->fail('no') unless defined $x;\nreturn $x;\n}\n1;\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(6, 0)), Some(InferredType::ClassName("Foo".into())));
}

/// The receiver may be any subclass of its class: an override that comes
/// back keeps the arm, as does a receiver whose class nothing says.
#[test]
fn an_override_that_returns_or_an_untyped_receiver_keeps_the_arm() {
    let foo = InferredType::ClassName("Foo".into());
    let optional = Some(InferredType::Optional(Box::new(foo)));
    let fa = build_fa(
        "package P;\nsub fail { die $_[1] }\nsub f {\nmy ($self, $c) = @_;\nmy $x = $c ? Foo->new : undef;\n$self->fail('no') unless defined $x;\nreturn $x;\n}\npackage Q;\nuse parent -norequire, 'P';\nsub fail { warn $_[1] }\n1;\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(6, 0)), optional, "Q::fail comes back");
    let fa = build_fa(
        "package P;\nsub fail { die $_[1] }\nsub f {\nmy ($o, $c) = @_;\nmy $x = $c ? Foo->new : undef;\n$o->fail('no') unless defined $x;\nreturn $x;\n}\n1;\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(6, 0)), optional, "$o may be anything");
}

/// Across files the index answers: an inherited method another file
/// defines ends as that file says, and an indexed subclass's override
/// joins the dispatch.
#[test]
fn a_method_another_file_defines_ends_as_that_file_says() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let consumer = "package C;\nuse parent 'Base';\nsub f {\nmy ($self, $c) = @_;\nmy $x = $c ? Foo->new : undef;\n$self->fail('no') unless defined $x;\nreturn $x;\n}\n1;\n";
    let fa = build_fa(consumer);
    let foo = InferredType::ClassName("Foo".into());
    let optional = Some(InferredType::Optional(Box::new(foo.clone())));
    let idx = ModuleIndex::new_for_test();
    let at = |idx: &ModuleIndex| fa.inferred_type_via_bag_ctx("$x", crate::model::witnesses::ReadPoint::position(Point::new(6, 0)), Some(idx));
    idx.register_workspace_module("/tmp/Base.pm".into(), Arc::new(build_fa("package Base;\nsub fail { die $_[1] }\n1;\n")));
    assert_eq!(at(&idx), Some(foo), "Base::fail dies");
    idx.register_workspace_module(
        "/tmp/D.pm".into(),
        Arc::new(build_fa("package D;\nuse parent -norequire, 'C';\nsub fail { warn $_[1] }\n1;\n")),
    );
    assert_eq!(at(&idx), optional, "D overrides it with one that comes back");
}

/// A file's answer for a name joins every callable of it the file
/// defines, in any package: one that comes back keeps the gate open.
#[test]
fn a_name_one_package_of_a_file_returns_from_keeps_its_gate_open() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let consumer = "package C;\nuse Util;\nsub f {\nmy ($c) = @_;\nmy $x = $c ? Foo->new : undef;\nfatal('no') unless defined $x;\nreturn $x;\n}\n";
    let fa = build_fa(consumer);
    let optional = Some(InferredType::Optional(Box::new(InferredType::ClassName("Foo".into()))));
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        "/tmp/Util.pm".into(),
        Arc::new(build_fa("package Util;\nuse Exporter 'import';\nour @EXPORT = ('fatal');\nsub fatal { die @_ }\npackage Util::Soft;\nsub fatal { warn @_ }\n1;\n")),
    );
    assert_eq!(fa.inferred_type_via_bag_ctx("$x", crate::model::witnesses::ReadPoint::position(Point::new(6, 0)), Some(&idx)), optional);
}
