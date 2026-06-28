//! PoC measurements: productive Perl projection — abstract plugin
//! generators projected over real Perl call sites into synthesized
//! symbols with provenance, via a fixpoint worklist.

use super::*;

fn perl_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    p
}

/// The plugin's declaration: `make_crud_helpers($name)` synthesizes an
/// accessor + a getter + a setter. (In production this is plugin data.)
fn crud_defs() -> HashMap<String, GeneratorDef> {
    HashMap::from([(
        "make_crud_helpers".to_string(),
        GeneratorDef::new(&["name"])
            .emit("${name}_id", "accessor")
            .emit("get_${name}", "method")
            .emit("set_${name}", "method"),
    )])
}

fn synth(src: &str, defs: &HashMap<String, GeneratorDef>) -> (Vec<Synthesized>, String) {
    let mut p = perl_parser();
    let tree = p.parse(src, None).unwrap();
    let known: HashSet<String> = defs.keys().cloned().collect();
    let witnesses = collect_witnesses(&tree, src.as_bytes(), &known);
    (synthesize(defs, &witnesses), src.to_string())
}

#[test]
fn projection_synthesizes_the_group_with_provenance() {
    // `make_crud_helpers('user')` projects to the whole group — and each
    // synthesized symbol traces back to the call site.
    let src = "make_crud_helpers('user');\n";
    let (syms, src) = synth(src, &crud_defs());
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"user_id"), "{names:?}");
    assert!(names.contains(&"get_user"), "{names:?}");
    assert!(names.contains(&"set_user"), "{names:?}");

    // provenance: the witness span points at the generator call site.
    let id = syms.iter().find(|s| s.name == "user_id").unwrap();
    assert_eq!(&src[id.witness.0..id.witness.1], "make_crud_helpers('user')");
    assert_eq!(id.kind, "accessor");
}

#[test]
fn two_witnesses_two_distinct_symbol_groups() {
    // Same generator, two call sites → two groups, each provenance-tagged
    // to its own site. `Class->gen('x')` (method form) is collected too.
    let src = "make_crud_helpers('user');\n__PACKAGE__->make_crud_helpers('post');\n";
    let (syms, _) = synth(src, &crud_defs());
    let names: HashSet<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    for n in ["user_id", "get_user", "set_user", "post_id", "get_post", "set_post"] {
        assert!(names.contains(n), "missing {n}: {names:?}");
    }
    // the two `_id` accessors trace to DIFFERENT witnesses
    let user = syms.iter().find(|s| s.name == "user_id").unwrap().witness;
    let post = syms.iter().find(|s| s.name == "post_id").unwrap().witness;
    assert_ne!(user, post, "distinct provenance per call site");
}

#[test]
fn nested_generation_runs_the_worklist() {
    // A generator that generates: `make_resource('widget')` emits an
    // accessor AND invokes `make_crud_helpers('widget')` — whose symbols
    // are discovered transitively, all tracing to the ORIGINAL call.
    let defs = HashMap::from([
        (
            "make_resource".to_string(),
            GeneratorDef::new(&["thing"])
                .emit("${thing}_table", "accessor")
                .generate("make_crud_helpers", &["${thing}"]),
        ),
        (
            "make_crud_helpers".to_string(),
            GeneratorDef::new(&["name"]).emit("get_${name}", "method").emit("set_${name}", "method"),
        ),
    ]);
    let src = "make_resource('widget');\n";
    let (syms, src) = synth(src, &defs);
    let names: HashSet<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains("widget_table"), "direct: {names:?}");
    assert!(names.contains("get_widget"), "transitive via worklist: {names:?}");
    assert!(names.contains("set_widget"), "transitive via worklist: {names:?}");
    // the transitively-synthesized symbol still traces to the user's call
    let getter = syms.iter().find(|s| s.name == "get_widget").unwrap();
    assert_eq!(&src[getter.witness.0..getter.witness.1], "make_resource('widget')");
}

#[test]
fn recursive_generator_terminates_via_seen_set() {
    // A generator that generates itself with the same args: we never
    // execute, so the worklist just re-queues the same witness and the
    // seen-set drops it. Bounded, no hang.
    let defs = HashMap::from([(
        "loop_gen".to_string(),
        GeneratorDef::new(&["x"]).emit("sym_${x}", "method").generate("loop_gen", &["${x}"]),
    )]);
    let src = "loop_gen('a');\n";
    let (syms, _) = synth(src, &defs);
    assert_eq!(syms.iter().filter(|s| s.name == "sym_a").count(), 1, "bounded: {syms:?}");
}

#[test]
fn non_generator_calls_are_not_witnesses() {
    // A plain function call that isn't a declared generator synthesizes
    // nothing — core stays generic, the plugin's name set is the gate.
    let src = "regular_function('user');\nprint('hi');\n";
    let (syms, _) = synth(src, &crud_defs());
    assert!(syms.is_empty(), "no generator → no synthesis: {syms:?}");
}
