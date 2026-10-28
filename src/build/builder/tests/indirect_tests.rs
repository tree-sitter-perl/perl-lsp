use super::*;
use crate::index::module_index::ModuleIndex;
use crate::model::file_analysis::{Deferred, Verdict};
use crate::model::witnesses::{Callee, WitnessPayload};

/// Rows of the `meth Class(ARGS)` sites the file reads as method calls.
fn method_call_rows(fa: &FileAnalysis, method: &str) -> Vec<usize> {
    fa.refs()
        .iter()
        .filter(|r| r.target_name == method && matches!(r.kind, RefKind::MethodCall { .. }))
        .map(|r| r.span.start.row)
        .collect()
}

fn choices(fa: &FileAnalysis) -> Vec<&Deferred<WitnessPayload>> {
    fa.witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Choose(d) => Some(&**d),
            _ => None,
        })
        .collect()
}

/// "method" / "function": which call a picked reading makes.
fn reading_kind(p: Option<&WitnessPayload>) -> &'static str {
    match p {
        Some(WitnessPayload::Invoke { callee: Callee::Method { .. }, .. }) => "method",
        Some(WitnessPayload::Invoke { callee: Callee::Function { .. }, .. }) => "function",
        _ => "other",
    }
}

/// The one deferred site in `src`, decided against an index holding `deps`.
fn decide(src: &str, deps: &[(&str, &str)]) -> (Verdict, &'static str) {
    let fa = build_fa(src);
    let idx = ModuleIndex::new_for_test();
    for (path, dep) in deps {
        idx.insert_workspace_for_test(std::path::PathBuf::from(path), build_fa(dep));
    }
    let cs = choices(&fa);
    assert_eq!(cs.len(), 1, "one deferred site: {cs:?}");
    let picked = cs[0].pick(&fa, Some(&idx));
    (picked.verdict, reading_kind(picked.reading))
}

#[test]
fn indirect_feature_off_makes_every_site_a_function_call() {
    // Perl reads `new Foo(...)` as a function call wherever the indirect
    // feature is off: a 5.36+ bundle, `no feature`, `no indirect`, block-
    // scoped like every pragma.
    let fa = build_fa(
        "package Foo;\nsub new { bless {}, shift }\npackage main;\nmy $a = new Foo(1);\n{\n    no feature 'indirect';\n    my $b = new Foo(2);\n}\nmy $c = new Foo(3);\n{\n    no indirect;\n    my $d = new Foo(4);\n}\nuse v5.36;\nmy $e = new Foo(5);\nuse feature 'indirect';\nmy $f = new Foo(6);\n",
    );
    assert_eq!(method_call_rows(&fa, "new"), vec![3, 8, 16]);
    // The sites where it is on still defer: another file may define a
    // `main::new`.
    assert_eq!(choices(&fa).len(), 3);
}

#[test]
fn a_plugin_declared_bundle_turns_indirect_off() {
    // A module that imports a feature bundle into its caller (a house
    // `My::Base` doing `feature->unimport('indirect')`) is plugin-declared:
    // core holds no list of them.
    let plugin_src = r#"
        fn id() { "house-base" }
        fn triggers() { [ #{ UsesModule: "My::Base" } ] }
        fn feature_makers() { [ #{ "module": "My::Base", disables: ["indirect"] } ] }
    "#;
    let engine = std::sync::Arc::new(crate::build::plugin::rhai_host::make_engine());
    let plugin = crate::build::plugin::rhai_host::RhaiPlugin::from_source(plugin_src, engine)
        .expect("plugin compiles");
    let mut reg = crate::build::plugin::PluginRegistry::new();
    reg.register(Box::new(plugin));

    let source = "package Foo;\nsub new { bless {}, shift }\npackage main;\nmy $a = new Foo(1);\nuse My::Base;\nmy $b = new Foo(2);\n";
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let fa = build_with_plugins(&tree, source.as_bytes(), std::sync::Arc::new(reg));
    assert_eq!(method_call_rows(&fa, "new"), vec![3]);
}

#[test]
fn a_site_the_file_cannot_settle_defers_and_reads_its_file_alone() {
    // `use Foo` names the package, but whether Foo exports a sub named
    // `Foo` or `new` is another file's fact: the site defers, reading as a
    // method call (its refs and its undecided type) until decided. With
    // nothing naming `Bar`, the undecided reading is a function call.
    let fa = build_fa("package main;\nuse Foo;\nmy $a = new Foo(1);\nmy $b = new Bar(2);\n");
    assert_eq!(method_call_rows(&fa, "new"), vec![2]);
    let undecided: Vec<_> = choices(&fa).iter().map(|d| reading_kind(d.undecided())).collect();
    assert_eq!(undecided, vec!["method", "function"]);
}

#[test]
fn a_verb_no_file_defines_is_a_method_call() {
    let thing = "package Thing;\nsub new { bless {}, shift }\n1;\n";
    let site = "package main;\nmy $a = new Thing(1);\n";
    assert_eq!(decide(site, &[("/tmp/qa_ind/Thing.pm", thing)]), (Verdict::Yes, "method"));
}

#[test]
fn an_exported_verb_needs_a_declared_class() {
    // `use Exp` exports `new` into main: Perl then reads `new CLASS` as a
    // method call only on a class it knows.
    let exp = "package Exp;\nuse Exporter 'import';\nour @EXPORT = qw(new);\nsub new { 1 }\n1;\n";
    let thing = "package Thing;\nsub new { bless {}, shift }\n1;\n";
    let deps = [("/tmp/qa_ind/Exp.pm", exp), ("/tmp/qa_ind/Thing.pm", thing)];
    assert_eq!(decide("package main;\nuse Exp;\nmy $a = new Ghost(1);\n", &deps), (Verdict::No, "function"));
    assert_eq!(decide("package main;\nuse Exp;\nmy $a = new Thing(1);\n", &deps), (Verdict::Yes, "method"));
}

#[test]
fn an_exported_class_word_is_a_function_call() {
    // A tag that exports a sub named like the class makes the class word
    // a call, so the whole site is `new(Widget(1))`.
    let exp = "package Exp;\nuse Exporter 'import';\nour %EXPORT_TAGS = (all => [qw(Widget)]);\nsub Widget { 1 }\n1;\n";
    let site = "package main;\nuse Exp ':all';\nmy $a = new Widget(1);\n";
    assert_eq!(decide(site, &[("/tmp/qa_ind/Exp.pm", exp)]), (Verdict::No, "function"));
}

#[test]
fn an_exporter_the_index_cannot_see_leaves_the_site_undecided() {
    let site = "package main;\nuse Missing;\nmy $a = new Thing(1);\n";
    assert_eq!(decide(site, &[]), (Verdict::Unknown, "function"));
}

#[test]
fn a_sub_of_the_package_in_another_file_makes_the_class_word_a_call() {
    // FHEM's `fhem ReadingsVal($n)`: both are subs of `main` defined in
    // another file, so it is `fhem(ReadingsVal($n))`.
    let fhem = "package main;\nsub ReadingsVal { 'v' }\nsub fhem { 's' }\n1;\n";
    let site = "package main;\nmy $n = 1;\nmy $a = fhem ReadingsVal($n);\n";
    assert_eq!(decide(site, &[("/tmp/qa_ind/fhem.pl", fhem)]), (Verdict::No, "function"));
}

#[test]
fn a_decided_site_types_through_its_reading() {
    let thing = "package Thing;\nsub new { bless {}, shift }\n1;\n";
    let src = "package main;\nmy $a = new Thing(1);\n";
    let fa = build_fa(src);
    let idx = ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(std::path::PathBuf::from("/tmp/qa_ind/Thing.pm"), build_fa(thing));
    let call = Span { start: Point::new(1, 8), end: Point::new(1, 20) };
    assert_eq!(fa.expr_type_at_span(call, Some(&idx)), Some(InferredType::ClassName("Thing".into())));
    assert_ne!(fa.expr_type_at_span(call, None), Some(InferredType::ClassName("Thing".into())));
}

#[test]
fn every_spelling_of_a_536_bundle_turns_indirect_off() {
    for (version, method) in [("v5.36", false), ("5.036", false), ("5.36.1", false), ("v5.34", true), ("5.010", true)] {
        let fa = build_fa(&format!("package Foo;\nsub new {{ bless {{}}, shift }}\npackage main;\nuse {version};\nmy $a = new Foo(1);\n"));
        assert_eq!(!method_call_rows(&fa, "new").is_empty(), method, "use {version}");
    }
}

#[test]
fn a_name_the_file_binds_decides_without_its_producer() {
    // An explicit import list binds its names whatever the producer
    // exports, so `use Lib qw(new)` makes `new` a sub with `Lib` unindexed.
    let site = "package main;\nuse Lib qw(new);\nmy $a = new Ghost(1);\n";
    assert_eq!(decide(site, &[]), (Verdict::No, "function"));
    // A sub named like the class, declared before the site, decides it
    // with no index at all.
    let fa = build_fa("package main;\nsub Helper { 1 }\nmy $a = new Helper(1);\n");
    let cs = choices(&fa);
    assert_eq!(cs.len(), 1);
    let picked = cs[0].pick(&fa, None);
    assert_eq!((picked.verdict, reading_kind(picked.reading)), (Verdict::No, "function"));
}

#[test]
fn a_decided_site_navigates_through_its_reading_at_the_same_ids() {
    // `new Thing(1)` reads as `new(Thing(1))` undecided (nothing names
    // `Thing`); an index declaring `Thing` decides it a method call. The
    // view swaps the head refs in place: same table lengths, the
    // function-call slots dormant, the method-call slots live.
    let thing = "package Thing;\nsub new { bless {}, shift }\n1;\n";
    let fa = build_fa("package main;\nmy $a = new Thing(1);\n");
    let idx = ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(std::path::PathBuf::from("/tmp/qa_ind/Thing.pm"), build_fa(thing));
    let verb = Point::new(1, 9);
    assert!(matches!(fa.ref_at(verb).map(|r| &r.kind), Some(RefKind::FunctionCall)));
    assert!(method_call_rows(&fa, "new").is_empty());

    let view = fa.decided_with(&idx).expect("the index moves the site off its fallback");
    assert_eq!(view.refs().len(), fa.refs().len(), "ids are stable across views");
    assert_eq!(view.symbols().len(), fa.symbols().len());
    assert!(matches!(view.ref_at(verb).map(|r| &r.kind), Some(RefKind::MethodCall { .. })));
    assert_eq!(method_call_rows(&view, "new"), vec![1]);
    assert!(
        view.refs().iter().all(|r| !(r.target_name == "Thing" && matches!(r.kind, RefKind::FunctionCall))),
        "the class word is no call in the method reading"
    );
    assert!(fa.decided(&fa.readings.fallbacks()).is_none(), "the fallbacks are the base");
}

#[test]
fn a_held_function_head_resolves_a_call_to_a_sub_defined_below() {
    // `FW_pO FW_textfield(..)` (FHEM's 01_FHEMWEB.pm): `FW_pO` is declared
    // above, `FW_textfield` only below, so the site defers. Its
    // `FW_textfield` call is a forward call, pinned to `main` after the
    // walk, in the analysis as stored and in any view reading the call.
    let src = "package main;\nsub FW_pO(@);\nsub show {\n  FW_pO FW_textfield(\"cmd\", 1, \"m\");\n}\nsub FW_textfield { 1 }\nsub FW_pO { 1 }\n";
    let fa = build_fa(src);
    let call = Point::new(3, 9);
    let pinned = |fa: &FileAnalysis| {
        fa.ref_at(call).filter(|r| matches!(r.kind, RefKind::FunctionCall)).and_then(|r| r.resolved_package()).map(str::to_string)
    };
    assert_eq!(pinned(&fa).as_deref(), Some("main"), "the stored fallback keeps the walk's pin");

    // Fallback a method call (`Thing` is named a package above); the file
    // itself defines `sub Thing`, so the index picks the function reading
    // and the view installs the held call heads.
    let src = "package Thing;\npackage main;\nmy $a = new Thing(1);\nsub Thing { 1 }\n";
    let fa = build_fa(src);
    let class = Point::new(2, 13);
    assert!(matches!(fa.ref_at(class).map(|r| &r.kind), Some(RefKind::PackageRef)));
    let idx = ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(std::path::PathBuf::from("/tmp/qa_ind/main.pl"), build_fa(src));
    let view = fa.decided_with(&idx).expect("the index moves the site off its fallback");
    let r = view.ref_at(class).expect("the class word is a call in the function reading");
    assert!(matches!(r.kind, RefKind::FunctionCall));
    assert_eq!(r.resolved_package(), Some("main"), "the installed head is pinned like a walked one");
}

#[test]
fn a_held_method_head_dispatches_like_a_walked_one() {
    // `new Thing(1)` above `package Thing`: the file alone reads it
    // `new(Thing(1))`; the index decides it a method call. The installed
    // head carries the dispatch target finalize freezes on a walked
    // `Thing->new(1)`: the local `Thing::new`.
    let src = "package main;\nmy $a = new Thing(1);\npackage Thing;\nsub new { bless {}, shift }\n";
    let fa = build_fa(src);
    let verb = Point::new(1, 9);
    assert!(matches!(fa.ref_at(verb).map(|r| &r.kind), Some(RefKind::FunctionCall)));
    let idx = ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(std::path::PathBuf::from("/tmp/qa_ind/thing.pl"), build_fa(src));
    let view = fa.decided_with(&idx).expect("the index moves the site off its fallback");
    let r = view.ref_at(verb).expect("the verb's head");
    assert!(matches!(r.kind, RefKind::MethodCall { .. }));
    let new = view.symbols().iter().find(|s| s.name == "new").expect("Thing::new").id;
    assert!(
        matches!(r.method_target(), Some(MethodTarget::Local { sym_id, invocant_class }) if *sym_id == new && invocant_class == "Thing"),
        "{:?}",
        r.method_target()
    );
}
