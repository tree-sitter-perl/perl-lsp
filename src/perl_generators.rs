//! SPIKE / PoC: productive Perl projection — plugin symbol generators
//! projected over witnesses into synthesized symbols, with provenance.
//!
//! The Perl backend of the metaprogram-witness tier (the C++ template
//! sibling is `cpp_templates.rs`). A "generator" is a helper that creates
//! a GROUP of helpers/tasks — `make_crud_helpers('user')` synthesizes a
//! `user_id` accessor, `get_user`/`set_user` methods, etc. The design is
//! STRUCTURAL PROJECTION, not execution: the generator's synthesis rules
//! are declared ABSTRACTLY by a plugin (parameterized over the
//! generator's args); core collects the call sites (the witnesses),
//! substitutes the literal args as if written there, and runs the
//! plugin's synthesis — symbolic, never running Perl.
//!
//! The same spine as C++ templates: witness-collection + substitution +
//! a fixpoint worklist (a generated helper that itself generates) + a
//! seen-set (a generator that generates itself terminates, doesn't hang).
//! The only language-specific piece is the projection function — here,
//! plugin symbol SYNTHESIS instead of C++ name/overload resolution.
//!
//! Provenance is the LSP payoff: every synthesized symbol traces to the
//! witness call site, so goto-def / rename land on the generator
//! invocation that produced it. Not wired into the pipeline; measured by
//! `perl_generators_tests.rs`.

use std::collections::{HashMap, HashSet};
use tree_sitter::{Node, Tree};

/// A plugin-declared generator: its parameters and the abstract synthesis
/// actions, parameterized over those params. The PLUGIN owns this; core
/// stays generic (rule #10 — no per-generator logic in core).
#[derive(Debug, Clone)]
pub struct GeneratorDef {
    pub params: Vec<String>,
    pub actions: Vec<SynthAction>,
}

#[derive(Debug, Clone)]
pub enum SynthAction {
    /// Emit a symbol whose name interpolates the params, e.g. `${name}_id`.
    Emit { name_tmpl: String, kind: &'static str },
    /// Invoke another generator with arg templates — nested generation,
    /// the worklist's fuel.
    Generate { generator: String, arg_tmpls: Vec<String> },
}

impl GeneratorDef {
    pub fn new(params: &[&str]) -> Self {
        GeneratorDef { params: params.iter().map(|s| s.to_string()).collect(), actions: vec![] }
    }
    pub fn emit(mut self, name_tmpl: &str, kind: &'static str) -> Self {
        self.actions.push(SynthAction::Emit { name_tmpl: name_tmpl.into(), kind });
        self
    }
    pub fn generate(mut self, generator: &str, arg_tmpls: &[&str]) -> Self {
        self.actions.push(SynthAction::Generate {
            generator: generator.into(),
            arg_tmpls: arg_tmpls.iter().map(|s| s.to_string()).collect(),
        });
        self
    }
}

/// A generator call site discovered in the source — the witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness {
    pub generator: String,
    pub args: Vec<String>,
    /// Byte span of the call site — where provenance points.
    pub span: (usize, usize),
}

/// A symbol produced by projecting a generator over a witness. `witness`
/// is the ROOT call site (chained through nested generation) — what
/// rename/goto-def target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synthesized {
    pub name: String,
    pub kind: &'static str,
    pub from_generator: String,
    pub witness: (usize, usize),
}

/// Collect generator call sites from real Perl. Both `gen(args)` and
/// `Class->gen(args)` shapes; only names the plugin declares are
/// witnesses.
pub fn collect_witnesses(tree: &Tree, src: &[u8], known: &HashSet<String>) -> Vec<Witness> {
    let mut out = Vec::new();
    walk_calls(tree.root_node(), src, known, &mut out);
    out
}

fn walk_calls(node: Node, src: &[u8], known: &HashSet<String>, out: &mut Vec<Witness>) {
    let name_field = match node.kind() {
        "function_call_expression" => Some("function"),
        "method_call_expression" => Some("method"),
        _ => None,
    };
    if let Some(field) = name_field {
        if let Some(name) = node.child_by_field_name(field).and_then(|n| n.utf8_text(src).ok()) {
            if known.contains(name) {
                let args = string_args(node, src);
                out.push(Witness {
                    generator: name.to_string(),
                    args,
                    span: (node.start_byte(), node.end_byte()),
                });
            }
        }
    }
    let mut cur = node.walk();
    for c in node.children(&mut cur) {
        walk_calls(c, src, known, out);
    }
}

/// Literal string args (in order) under a call's `arguments` field.
fn string_args(call: Node, src: &[u8]) -> Vec<String> {
    let Some(args) = call.child_by_field_name("arguments") else { return vec![] };
    let mut out = Vec::new();
    let mut cur = args.walk();
    let mut stack = vec![args];
    while let Some(n) = stack.pop() {
        if n.kind() == "string_content" {
            if let Ok(t) = n.utf8_text(src) {
                out.push(t.to_string());
            }
        }
        for c in n.children(&mut cur) {
            stack.push(c);
        }
    }
    out
}

/// Project the generators over the witnesses into synthesized symbols.
/// A fixpoint worklist: nested `Generate` actions become new witnesses
/// (carrying the ROOT span for provenance); the seen-set bounds recursive
/// generators — we never execute, so we never diverge.
pub fn synthesize(defs: &HashMap<String, GeneratorDef>, witnesses: &[Witness]) -> Vec<Synthesized> {
    let mut seen: HashSet<(String, Vec<String>)> = HashSet::new();
    let mut queue: Vec<Witness> = witnesses.to_vec();
    let mut out = Vec::new();
    while let Some(w) = queue.pop() {
        if !seen.insert((w.generator.clone(), w.args.clone())) {
            continue;
        }
        let Some(def) = defs.get(&w.generator) else { continue };
        let subst: HashMap<String, String> =
            def.params.iter().cloned().zip(w.args.iter().cloned()).collect();
        for action in &def.actions {
            match action {
                SynthAction::Emit { name_tmpl, kind } => out.push(Synthesized {
                    name: interpolate(name_tmpl, &subst),
                    kind,
                    from_generator: w.generator.clone(),
                    witness: w.span,
                }),
                SynthAction::Generate { generator, arg_tmpls } => queue.push(Witness {
                    generator: generator.clone(),
                    args: arg_tmpls.iter().map(|t| interpolate(t, &subst)).collect(),
                    span: w.span, // provenance chains to the root call site
                }),
            }
        }
    }
    out
}

/// `${param}` interpolation against the substitution.
fn interpolate(tmpl: &str, subst: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(tmpl.len());
    let bytes = tmpl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
            if let Some(end) = tmpl[i + 2..].find('}') {
                let key = &tmpl[i + 2..i + 2 + end];
                out.push_str(subst.get(key).map(String::as_str).unwrap_or(""));
                i += 2 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "perl_generators_tests.rs"]
mod tests;
