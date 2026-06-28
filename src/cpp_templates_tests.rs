//! PoC measurements: templates as witnesses — collect concrete
//! instantiations, project the body, watch dependent types dissolve, chase
//! the worklist to a fixpoint, terminate on recursion.

use super::*;

fn cpp_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    p
}

fn run(src: &str) -> (HashMap<String, Template>, Vec<Instantiation>) {
    let mut p = cpp_parser();
    let tree = p.parse(src, None).unwrap();
    (collect_templates(&tree, src.as_bytes()), seed_instantiations(&tree, src.as_bytes()))
}

fn concrete(name: &str) -> TypeArg {
    TypeArg::Concrete(name.into())
}

#[test]
fn witnesses_are_collected_not_executed() {
    // The concrete type-args that actually appear are the witnesses — no
    // template body is run.
    let src = "template <typename T> void process(T x) { x.foo(); }\n\
               void use(){ process<Widget>(w); process<Gadget>(g); }\n";
    let (tmpls, seeds) = run(src);
    assert!(tmpls.contains_key("process"));
    assert_eq!(tmpls["process"].params, vec!["T"]);
    assert!(seeds.contains(&Instantiation { template: "process".into(), args: vec![concrete("Widget")] }));
    assert!(seeds.contains(&Instantiation { template: "process".into(), args: vec![concrete("Gadget")] }));
}

#[test]
fn dependent_types_dissolve_at_the_witness() {
    // `typename T::value_type` is unknowable in the template — but once
    // `T=Widget` it dissolves to `Widget::value_type`. The `typename`
    // disambiguator problem evaporates per witness.
    let src = "template <typename T> void process(T x) { typename T::value_type v; }\n\
               void use(){ process<Widget>(w); process<Gadget>(g); }\n";
    let (tmpls, seeds) = run(src);
    assert_eq!(tmpls["process"].dependent_types, vec![("T".into(), "value_type".into())]);

    let resolved = instantiate_to_fixpoint(&tmpls, &seeds);
    let widget = resolved.iter().find(|r| r.inst.args == vec![concrete("Widget")]).unwrap();
    let gadget = resolved.iter().find(|r| r.inst.args == vec![concrete("Gadget")]).unwrap();
    assert_eq!(widget.concrete_types, vec!["Widget::value_type"]);
    assert_eq!(gadget.concrete_types, vec!["Gadget::value_type"], "different per witness");
}

#[test]
fn nested_instantiation_is_discovered_transitively() {
    // `process<T>`'s body instantiates `helper<T>`. Seeding only
    // `process<Widget>` must DISCOVER `helper<Widget>` through the worklist
    // — the transitive fixpoint, the same shape as the reparse/macro loops.
    let src = "template <typename T> void helper(T x) { typename T::tag t; }\n\
               template <typename T> void process(T x) { helper<T>(x); }\n\
               void use(){ process<Widget>(w); }\n";
    let (tmpls, seeds) = run(src);
    // process's body holds a Param-arg instantiation of helper
    assert_eq!(
        tmpls["process"].body_instantiations,
        vec![Instantiation { template: "helper".into(), args: vec![TypeArg::Param("T".into())] }],
    );

    let resolved = instantiate_to_fixpoint(&tmpls, &seeds);
    let names: Vec<(&str, &Vec<TypeArg>)> = resolved.iter().map(|r| (r.inst.template.as_str(), &r.inst.args)).collect();
    assert!(names.contains(&("process", &vec![concrete("Widget")])), "seed: {names:?}");
    assert!(names.contains(&("helper", &vec![concrete("Widget")])), "transitively discovered: {names:?}");
    // and helper<Widget>'s dependent type dissolved too
    let helper = resolved.iter().find(|r| r.inst.template == "helper").unwrap();
    assert_eq!(helper.concrete_types, vec!["Widget::tag"]);
}

#[test]
fn recursive_template_terminates_via_seen_set() {
    // The Turing-complete trap: `rec<T>`'s body instantiates `rec<T>`
    // again. We never EXECUTE, so the worklist just re-queues the same
    // witness — the seen-set drops it. Bounded, terminates, no divergence.
    let src = "template <typename T> void rec(T x) { rec<T>(x); }\n\
               void use(){ rec<Widget>(w); }\n";
    let (tmpls, seeds) = run(src);
    let resolved = instantiate_to_fixpoint(&tmpls, &seeds);
    // exactly one monomorphization of rec<Widget>, not an infinite stream.
    let count = resolved.iter().filter(|r| r.inst.template == "rec").count();
    assert_eq!(count, 1, "recursive template bounded to one witness: {resolved:?}");
}

#[test]
fn two_witnesses_two_distinct_monomorphizations() {
    // The whole point: one template, two witnesses, two resolved bodies.
    let src = "template <typename T> void process(T x) { typename T::out o; sub<T>(x); }\n\
               template <typename T> void sub(T x) { typename T::in i; }\n\
               void use(){ process<A>(a); process<B>(b); }\n";
    let (tmpls, seeds) = run(src);
    let resolved = instantiate_to_fixpoint(&tmpls, &seeds);
    let got: std::collections::HashSet<(String, String)> = resolved
        .iter()
        .flat_map(|r| r.concrete_types.iter().map(move |t| (r.inst.template.clone(), t.clone())))
        .collect();
    assert!(got.contains(&("process".into(), "A::out".into())));
    assert!(got.contains(&("process".into(), "B::out".into())));
    assert!(got.contains(&("sub".into(), "A::in".into())), "sub<A> via worklist");
    assert!(got.contains(&("sub".into(), "B::in".into())), "sub<B> via worklist");
}
