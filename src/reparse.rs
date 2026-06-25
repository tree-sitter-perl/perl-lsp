//! SPIKE: the stratified reparse seam (Perl prototype reparenthesizer).
//!
//! The thinnest end-to-end proof of `docs/prompt-cpp-reparse.md`: a
//! source transform that runs BEFORE extraction, because a declaration
//! changes how the file parses. Perl prototypes are the local,
//! preprocessor-free instance — `sub sner ($) {...}` makes `sner 1, 2`
//! mean `sner(1), 2`, but tree-sitter-perl greedily grabs both args
//! into the call. Knowing the prototype, we reparenthesize the source
//! and re-parse.
//!
//! Two outputs, both load-bearing for the C++ macro generalization:
//!   - rewritten source (the transform), and
//!   - an `AnchorMap` (transformed byte → original byte) so every span
//!     extracted from the rewritten tree lands on real user text.
//!
//! Stratification is the soundness argument (see the doc): facts here
//! (prototype shapes) are type-INDEPENDENT, so this fixpoint sits
//! strictly upstream of the witness bag — the worklist never sees an
//! intermediate tree, monotonicity untouched. Deliberately not wired
//! into the build pipeline; measured by `reparse_tests.rs`.

use std::collections::HashMap;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

/// A parsed Perl prototype's parse-relevant shape. The full prototype
/// grammar (`\`, `+`, `*`, `;`, `_`) is richer; this slice models the
/// two facts that change grouping: nullary, and the count of leading
/// fixed scalars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proto {
    pub nullary: bool,
    /// Number of leading `$` slots — how many top-level args the call
    /// actually binds (`($)` = 1, `($$)` = 2).
    pub fixed_arity: usize,
}

impl Proto {
    /// `($)` / `()` / `($$)` → shape. `inner` is the text between the
    /// parens.
    fn parse(inner: &str) -> Proto {
        let inner = inner.trim();
        // Stop at the first non-fixed slot (`@`, `%`, `;`, `&`): only
        // the leading run of `$` binds positionally for our purposes.
        let mut fixed = 0;
        for c in inner.chars() {
            match c {
                '$' => fixed += 1,
                '\\' => {} // ref-of-next; still one slot, approximated
                _ => break,
            }
        }
        Proto { nullary: inner.is_empty(), fixed_arity: fixed }
    }
}

/// One source insertion: `bytes` inserted at original offset `at`.
#[derive(Debug, Clone)]
struct Insertion {
    at: usize,
    bytes: &'static str,
}

/// Transformed-source ↔ original-source coordinate map. The Zed-anchor
/// idea in miniature: a transformed byte maps back to original by
/// subtracting the insertions that precede it. Per-region granularity —
/// a byte inside an inserted `(` collapses to the insertion site.
#[derive(Debug, Default, Clone)]
pub struct AnchorMap {
    /// (transformed_offset_of_insertion_start, inserted_len, original_at)
    inserts: Vec<(usize, usize, usize)>,
}

impl AnchorMap {
    /// Map a byte offset in the rewritten source back to the original.
    pub fn to_original(&self, transformed: usize) -> usize {
        let mut shift = 0usize;
        for &(t_start, len, _orig) in &self.inserts {
            if transformed >= t_start + len {
                shift += len; // fully past this insertion
            } else if transformed >= t_start {
                // inside the inserted run → collapse to the site
                return t_start - shift;
            }
        }
        transformed - shift
    }
}

const PROTO_QUERY: &str = r#"
(subroutine_declaration_statement
  name: (bareword) @name
  (prototype) @proto)
"#;

const SITE_QUERY: &str = r#"
(ambiguous_function_call_expression
  function: (function) @callee
  arguments: (list_expression) @args)
(binary_expression left: (bareword) @bareword)
(binary_expression right: (bareword) @bareword)
"#;

/// Collect local prototype facts: sub name → its parse-relevant shape.
pub fn collect_prototypes(tree: &Tree, src: &[u8]) -> HashMap<String, Proto> {
    let lang = tree.language();
    let query = Query::new(&lang, PROTO_QUERY).expect("proto query");
    let names: Vec<&str> = query.capture_names().to_vec();
    let mut out = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = it.next() {
        let mut name = None;
        let mut proto = None;
        for c in m.captures {
            let txt = c.node.utf8_text(src).unwrap_or("");
            match names[c.index as usize] {
                "name" => name = Some(txt.to_string()),
                "proto" => proto = Some(txt.trim_start_matches('(').trim_end_matches(')').to_string()),
                _ => {}
            }
        }
        if let (Some(n), Some(p)) = (name, proto) {
            out.insert(n, Proto::parse(&p));
        }
    }
    out
}

/// Plan the reparenthesizations for one parse, given known prototypes.
fn plan_edits(tree: &Tree, src: &[u8], protos: &HashMap<String, Proto>) -> Vec<Insertion> {
    let lang = tree.language();
    let query = Query::new(&lang, SITE_QUERY).expect("site query");
    let names: Vec<&str> = query.capture_names().to_vec();
    let mut edits: Vec<Insertion> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = it.next() {
        let mut callee: Option<Node> = None;
        let mut args: Option<Node> = None;
        let mut bareword: Option<Node> = None;
        for c in m.captures {
            match names[c.index as usize] {
                "callee" => callee = Some(c.node),
                "args" => args = Some(c.node),
                "bareword" => bareword = Some(c.node),
                _ => {}
            }
        }
        // unary/fixed-arity call greedily grabbed too many args:
        // wrap the first `fixed_arity` args in parens.
        if let (Some(callee), Some(args)) = (callee, args) {
            let name = callee.utf8_text(src).unwrap_or("");
            if let Some(p) = protos.get(name) {
                if p.fixed_arity >= 1 {
                    let n = p.fixed_arity.min(args.named_child_count());
                    if n >= 1 {
                        let last = args.named_child(n - 1).unwrap();
                        edits.push(Insertion { at: callee.end_byte(), bytes: "(" });
                        edits.push(Insertion { at: last.end_byte(), bytes: ")" });
                    }
                }
            }
        }
        // nullary name used as a bareword operand → it's a call: `()`.
        if let Some(bw) = bareword {
            let name = bw.utf8_text(src).unwrap_or("");
            if protos.get(name).is_some_and(|p| p.nullary) {
                edits.push(Insertion { at: bw.end_byte(), bytes: "()" });
            }
        }
    }
    edits.sort_by_key(|e| e.at);
    edits
}

/// Apply insertions, producing rewritten source + the anchor map.
fn apply(src: &str, edits: &[Insertion]) -> (String, AnchorMap) {
    let mut out = String::with_capacity(src.len() + edits.len() * 2);
    let mut map = AnchorMap::default();
    let mut prev = 0usize;
    for e in edits {
        out.push_str(&src[prev..e.at]);
        let t_start = out.len();
        out.push_str(e.bytes);
        map.inserts.push((t_start, e.bytes.len(), e.at));
        prev = e.at;
    }
    out.push_str(&src[prev..]);
    (out, map)
}

/// One reparenthesization pass: parse, learn prototypes, rewrite. A
/// full implementation iterates to a fixpoint (a rewrite can expose a
/// nested site); the local single-pass slice is enough to prove the
/// seam. The caller re-parses the returned source.
pub fn reparenthesize(parser: &mut tree_sitter::Parser, src: &str) -> (String, AnchorMap) {
    let Some(tree) = parser.parse(src, None) else {
        return (src.to_string(), AnchorMap::default());
    };
    let protos = collect_prototypes(&tree, src.as_bytes());
    if protos.is_empty() {
        return (src.to_string(), AnchorMap::default());
    }
    let edits = plan_edits(&tree, src.as_bytes(), &protos);
    apply(src, &edits)
}

#[cfg(test)]
#[path = "reparse_tests.rs"]
mod tests;
