//! PoC measurement: the same template body resolves to different
//! overloads per witness — the two builds composing.

use super::*;
use crate::cpp_multidispatch::{Signature, Ty};

fn resolve(src: &str) -> Vec<ResolvedCall> {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    let tree = p.parse(src, None).unwrap();
    resolve_template_body_calls(&tree, src.as_bytes())
}

fn resolved_params(calls: &[ResolvedCall], witness: &str) -> Vec<Ty> {
    calls
        .iter()
        .find(|c| c.witness == vec![witness.to_string()] && c.callee == "sink")
        .map(|c| match &c.dispatch {
            Dispatch::Resolved(Signature { params, .. }) => params.clone(),
            other => panic!("expected resolved for {witness}, got {other:?}"),
        })
        .unwrap_or_else(|| panic!("no sink call resolved for witness {witness}"))
}

#[test]
fn same_template_body_different_overload_per_witness() {
    // `process<int>` and `process<Widget>` run the SAME `sink(x)` body, but
    // x's type is the witness's T, so the overload differs.
    let src = "\
void sink(int);
void sink(Widget);
template <typename T> void process(T x) { sink(x); }
void use(){ process<int>(a); process<Widget>(b); }
";
    let calls = resolve(src);
    assert_eq!(resolved_params(&calls, "int"), vec![Ty::Int], "process<int> → sink(int)");
    assert_eq!(
        resolved_params(&calls, "Widget"),
        vec![Ty::Class("Widget".into())],
        "process<Widget> → sink(Widget)",
    );
}

#[test]
fn ranking_applies_through_the_template() {
    // The lattice's ranking shows through projection: only `sink(double)`
    // exists, so `process<int>`'s `sink(x)` binds int→double (rank 1).
    let src = "\
void sink(double);
template <typename T> void process(T x) { sink(x); }
void use(){ process<int>(a); }
";
    let calls = resolve(src);
    assert_eq!(resolved_params(&calls, "int"), vec![Ty::Double], "int promotes to the double overload");
}

#[test]
fn no_viable_overload_surfaces_through_the_template() {
    // `process<Gadget>`'s `sink(x)` has no viable `sink` — a per-witness
    // missing call-graph edge, surfaced honestly (not silently picked).
    let src = "\
void sink(int);
template <typename T> void process(T x) { sink(x); }
void use(){ process<Gadget>(g); }
";
    let calls = resolve(src);
    let c = calls.iter().find(|c| c.witness == vec!["Gadget".to_string()]).unwrap();
    assert_eq!(c.dispatch, Dispatch::NoViableOverload);
}

