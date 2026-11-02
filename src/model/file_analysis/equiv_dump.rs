//! The canonical whole-analysis projection: what "this change altered no
//! output" is checked against.
//!
//! Two readers share it, so they cannot disagree about what counts as a
//! difference: the in-process equivalence switches (`PERL_LSP_WALK_EQUIV`,
//! `PERL_LSP_PD_EQUIV`, the fold-memo check), which compare two builds of one
//! input, and the across-build dump (`PERL_LSP_EQUIV_DUMP_DIR`,
//! `--equiv-dump`), which compares a base binary against a head binary over
//! every input either sees.
//!
//! It is the serde projection, not bincode bytes: `serde_json` renders every
//! map through a `BTreeMap`, so `HashMap` iteration order cannot manufacture a
//! difference, while sequence order (emission order) is kept exactly.

use serde_json::Value;

use super::FileAnalysis;
use crate::model::witnesses::{FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry};

/// `HashSet` fields reachable from `FileAnalysis`. They serialize as arrays in
/// iteration order, which differs between any two builds (each set gets its
/// own `RandomState`), so they are sorted before a compare; every OTHER array
/// keeps its order, because sequence order is part of what is checked.
///
/// Listed by name rather than detected: once serialized, an array of scalars is
/// indistinguishable from a set, and for some (`package_parents` values carry
/// MRO) order is meaning. A new `HashSet` field (or any field serialized in
/// a process-dependent order) belongs here;
/// `build::equiv_dump::tests::canonical_dump_survives_a_rehash` fails when one is
/// missed.
const SET_FIELDS: &[&str] = &[
    "column_keyed_verbs",
    "export_lookup",
    "framework_imports",
    // A `ClosureList` is a set of paths in global intern-id order, which is
    // the order a process happened to intern them in.
    "include_closure",
    "reassigned_vars",
];

/// Sort the arrays named in [`SET_FIELDS`], everywhere they appear.
fn canonicalize_sets(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, child) in map.iter_mut() {
                if SET_FIELDS.contains(&k.as_str()) {
                    if let Value::Array(items) = child {
                        items.sort_by_cached_key(|i| i.to_string());
                        continue;
                    }
                }
                canonicalize_sets(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize_sets),
        _ => {}
    }
}

impl FileAnalysis {
    /// Every serialized fact of the analysis, hash order removed.
    pub fn canonical_value(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("FileAnalysis serializes");
        canonicalize_sets(&mut v);
        v
    }

    /// [`canonical_value`](Self::canonical_value) plus the registry's answer
    /// for every attachment the bag holds, in attachment order.
    ///
    /// The answers are what make a reducer refactor checkable: it leaves the
    /// witnesses alone and changes only what they fold to. Each is asked
    /// context-free in the cross-file sense (no index) and with no point, so it
    /// is a function of this analysis alone.
    pub fn equivalence_dump(&self) -> Value {
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context(None);
        let mut atts: Vec<_> = self.witnesses.attachments().collect();
        atts.sort();
        let answers: Vec<Value> = atts
            .into_iter()
            .map(|att| {
                let q = ReducerQuery {
                    attachment: att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: Some(&ctx),
                };
                let answer = match reg.query(&self.witnesses, &q) {
                    ReducedValue::Type(t) => serde_json::json!({ "type": t }),
                    ReducedValue::FactMap(m) => serde_json::json!({ "facts": m }),
                    ReducedValue::None => Value::Null,
                };
                serde_json::json!([att, answer])
            })
            .collect();
        serde_json::json!({ "analysis": self.canonical_value(), "answers": answers })
    }
}
