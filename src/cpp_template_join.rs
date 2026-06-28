//! SPIKE / PoC: the JOIN of the two C++-moat builds — template witness
//! projection (`cpp_templates`) feeding the dispatch lattice
//! (`cpp_multidispatch`). The research's build-order payoff: "templates
//! DEPEND ON the lattice — build order: lattice first, witnesses on top."
//!
//! A template body calls an overloaded function with a dependent arg —
//! `template<T> process(T x) { sink(x); }`. The call `sink(x)` has no
//! single meaning: its overload is chosen by `x`'s type, which is the
//! template param `T`, which only goes concrete at a witness. So the SAME
//! body resolves to a DIFFERENT overload per instantiation:
//!   `process<int>`    → `sink(int)`
//!   `process<Widget>` → `sink(Widget)`
//!
//! This is the call-graph kicker (research §1a): an unresolved overload
//! inside a template is a wrong/missing call-graph edge — and the edge is
//! per-witness. Joining the two builds is what makes the call graph (and
//! thus the heatmap / dead-code answer) correct on generic C++. Not wired
//! into the pipeline; measured by `cpp_template_join_tests.rs`.

use crate::cpp_multidispatch::{collect_overloads, dispatch, Dispatch, Ty};
use crate::cpp_templates::{
    collect_templates, instantiate_to_fixpoint, seed_instantiations, TypeArg,
};
use std::collections::HashMap;
use tree_sitter::Tree;

/// One body call resolved at one witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCall {
    pub template: String,
    /// The instantiation's concrete type args (the witness).
    pub witness: Vec<String>,
    pub callee: String,
    pub dispatch: Dispatch,
}

/// Resolve every overloaded body call, at every template witness, through
/// the dispatch lattice. Reuses `cpp_templates` (collect + project + the
/// fixpoint worklist) and `cpp_multidispatch` (overload sets + ranked
/// dispatch) — the join is the connective tissue, not new machinery.
pub fn resolve_template_body_calls(tree: &Tree, src: &[u8]) -> Vec<ResolvedCall> {
    let templates = collect_templates(tree, src);
    let overloads = collect_overloads(tree, src);
    let witnesses = instantiate_to_fixpoint(&templates, &seed_instantiations(tree, src));

    let mut out = Vec::new();
    for w in &witnesses {
        let Some(def) = templates.get(&w.inst.template) else { continue };
        // concrete type args of this instantiation
        let concrete: Vec<String> = w
            .inst
            .args
            .iter()
            .filter_map(|a| match a {
                TypeArg::Concrete(c) => Some(c.clone()),
                TypeArg::Param(_) => None,
            })
            .collect();
        // type-param → concrete
        let subst: HashMap<&str, &str> =
            def.params.iter().map(String::as_str).zip(concrete.iter().map(String::as_str)).collect();
        // value-param var → its type-param
        let var_type: HashMap<&str, &str> =
            def.value_params.iter().map(|(v, t)| (v.as_str(), t.as_str())).collect();

        for (callee, arg_vars) in &def.body_calls {
            let Some(oset) = overloads.get(callee) else { continue };
            // each arg var's type = its type-param substituted concrete
            let arg_tys: Vec<Ty> = arg_vars
                .iter()
                .map(|v| match var_type.get(v.as_str()).and_then(|tp| subst.get(tp)) {
                    Some(c) => Ty::parse(c),
                    None => Ty::Other(v.clone()),
                })
                .collect();
            out.push(ResolvedCall {
                template: w.inst.template.clone(),
                witness: concrete.clone(),
                callee: callee.clone(),
                dispatch: dispatch(oset, &arg_tys),
            });
        }
    }
    out
}

#[cfg(test)]
#[path = "cpp_template_join_tests.rs"]
mod tests;
