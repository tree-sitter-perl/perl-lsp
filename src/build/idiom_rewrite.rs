//! SPIKE (throwaway): naive `sort` → Schwartzian transform.
//!
//! Recognizes a comparator that is a chain of `K(a) OP K(b)` keys joined by
//! `||` / `or`, where each key is the SAME expression with `$a` and `$b`
//! swapped (alpha-equivalence over the CST, not text matching), and rewrites
//! it to `map { $_->[N] } sort { $a->[0] OP $b->[0] || … } map { [K…, $_] } LIST`.
//! Each key is evaluated once per element instead of twice per comparison.

use tree_sitter::Node;

#[derive(Debug, Clone)]
pub struct SortKey {
    /// Key text with the sort variable rewritten to `$_`.
    pub expr: String,
    pub op: String,
    /// `K($b) OP K($a)`: descending on this key.
    pub descending: bool,
    /// The key does real work (a call, a file test, a regex), so caching it
    /// pays. A bare `$a->{k}` costs about what the tuple lookup does.
    pub costly: bool,
    /// The key is a call whose list-context value could differ from its
    /// scalar-context value; the rewrite forces `scalar(...)` to keep the
    /// comparator's semantics.
    pub needs_scalar: bool,
}

#[derive(Debug, Clone)]
pub struct StCandidate {
    pub row: usize,
    pub original: String,
    pub rewritten: String,
    pub keys: Vec<SortKey>,
}

#[derive(Debug, Clone)]
pub struct StReject {
    pub row: usize,
    pub original: String,
    pub reason: &'static str,
}

pub fn find_schwartzian(src: &str) -> (Vec<StCandidate>, Vec<StReject>) {
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
    let mut parser = reg.fallback().make_parser();
    let Some(tree) = parser.parse(src, None) else { return (vec![], vec![]) };
    let mut out = (Vec::new(), Vec::new());
    walk(tree.root_node(), src.as_bytes(), &mut out);
    out
}

fn walk(n: Node, src: &[u8], out: &mut (Vec<StCandidate>, Vec<StReject>)) {
    if n.kind() == "sort_expression" {
        match candidate(n, src) {
            Ok(c) => out.0.push(c),
            Err(reason) => out.1.push(StReject { row: n.start_position().row, original: text(n, src), reason }),
        }
    }
    let mut c = n.walk();
    for ch in n.named_children(&mut c) {
        walk(ch, src, out);
    }
}

fn text(n: Node, src: &[u8]) -> String {
    n.utf8_text(src).unwrap_or("").to_string()
}

fn candidate(n: Node, src: &[u8]) -> Result<StCandidate, &'static str> {
    let cb = n.child_by_field_name("callback").ok_or("no comparator block (named sub or default sort)")?;
    if cb.kind() != "block" {
        return Err("comparator is not a block");
    }
    let list = n.child_by_field_name("list").ok_or("no list")?;
    let stmts: Vec<Node> = { let mut c = cb.walk(); cb.named_children(&mut c).collect() };
    let [stmt] = stmts.as_slice() else { return Err("comparator has more than one statement") };
    let expr = stmt.named_child(0).ok_or("empty comparator")?;
    let mut cmps = Vec::new();
    split_or(expr, src, &mut cmps)?;
    let mut keys = Vec::new();
    for c in cmps {
        keys.push(key_of(c, src)?);
    }
    if !keys.iter().any(|k| k.costly) {
        return Err("every key is a cheap lookup; caching would not pay");
    }
    let n_keys = keys.len();
    let tuple = keys
        .iter()
        .map(|k| if k.needs_scalar { format!("scalar({})", k.expr) } else { k.expr.clone() })
        .chain(std::iter::once("$_".to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    let cmp = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let (l, r) = if k.descending { ("$b", "$a") } else { ("$a", "$b") };
            format!("{l}->[{i}] {} {r}->[{i}]", k.op)
        })
        .collect::<Vec<_>>()
        .join(" || ");
    let rewritten = format!(
        "map {{ $_->[{n_keys}] }} sort {{ {cmp} }} map {{ [{tuple}] }} {}",
        text(list, src)
    );
    Ok(StCandidate { row: n.start_position().row, original: text(n, src), rewritten, keys })
}

/// `A || B` / `A or B` → [A, B], left to right.
fn split_or<'t>(n: Node<'t>, src: &[u8], out: &mut Vec<Node<'t>>) -> Result<(), &'static str> {
    let n = peel(n);
    if matches!(n.kind(), "logical_expression" | "lowprec_logical_expression" | "binary_expression") {
        let op = operator(n, src);
        if op != "||" && op != "or" {
            return Err("comparator joins keys with something other than || / or");
        }
        split_or(n.child_by_field_name("left").ok_or("bad ||")?, src, out)?;
        split_or(n.child_by_field_name("right").ok_or("bad ||")?, src, out)?;
        return Ok(());
    }
    out.push(n);
    Ok(())
}

fn peel(mut n: Node) -> Node {
    while n.kind() == "parenthesized_expression" && n.named_child_count() == 1 {
        n = n.named_child(0).unwrap();
    }
    n
}

/// The unnamed operator token between `left` and `right`.
fn operator(n: Node, src: &[u8]) -> String {
    let mut c = n.walk();
    let op = n
        .children(&mut c)
        .filter(|ch| !ch.is_named())
        .map(|ch| text(ch, src))
        .find(|t| !t.trim().is_empty() && t != "(" && t != ")")
        .unwrap_or_default();
    op
}

fn key_of(c: Node, src: &[u8]) -> Result<SortKey, &'static str> {
    if c.kind() != "equality_expression" {
        return Err("a comparator term is not a <=> / cmp comparison");
    }
    let op = operator(c, src);
    if op != "<=>" && op != "cmp" {
        return Err("a comparator term is not a <=> / cmp comparison");
    }
    let l = c.child_by_field_name("left").ok_or("bad comparison")?;
    let r = c.child_by_field_name("right").ok_or("bad comparison")?;
    let (lu, ru) = (uses(l, src), uses(r, src));
    let descending = match (lu, ru) {
        ((true, false), (false, true)) => false,
        ((false, true), (true, false)) => true,
        _ => return Err("a key mixes $a and $b, or reads neither"),
    };
    let (lv, rv) = if descending { ("b", "a") } else { ("a", "b") };
    let lk = rename(l, src, lv);
    let rk = rename(r, src, rv);
    if squash(&lk) != squash(&rk) {
        return Err("the two sides compute different keys");
    }
    if has_effect(l) {
        return Err("a key has a side effect; running it once per element changes behavior");
    }
    Ok(SortKey { expr: lk, op, descending, costly: costly(l, src), needs_scalar: needs_scalar(peel(l)) })
}

/// (reads `$a`, reads `$b`)
fn uses(n: Node, src: &[u8]) -> (bool, bool) {
    let mut acc = (false, false);
    visit(n, &mut |m| {
        if m.kind() == "scalar" {
            match m.utf8_text(src).unwrap_or("") {
                "$a" => acc.0 = true,
                "$b" => acc.1 = true,
                _ => {}
            }
        }
    });
    acc
}

/// The key's source with the sort variable (`$a` or `$b` scalar nodes only,
/// never `$ab` or `"$a"` text) replaced by `$_`.
fn rename(n: Node, src: &[u8], var: &str) -> String {
    let base = n.start_byte();
    let mut s = text(n, src);
    let mut hits = Vec::new();
    let want = format!("${var}");
    visit(n, &mut |m| {
        if m.kind() == "scalar" && m.utf8_text(src).ok() == Some(want.as_str()) {
            hits.push((m.start_byte() - base, m.end_byte() - base));
        }
    });
    for (a, b) in hits.into_iter().rev() {
        s.replace_range(a..b, "$_");
    }
    s
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn visit<'t>(n: Node<'t>, f: &mut impl FnMut(Node<'t>)) {
    f(n);
    let mut c = n.walk();
    for ch in n.children(&mut c) {
        visit(ch, f);
    }
}

/// Cheap builtins (`lc`, `length`, `defined`) don't pay for a tuple; file
/// tests (`-M`, `-s`) hit the filesystem, and method / user calls run code.
fn costly(n: Node, src: &[u8]) -> bool {
    let mut hit = false;
    visit(n, &mut |m| match m.kind() {
        "method_call_expression" | "match_regexp" | "substitution_regexp" => hit = true,
        "func1op_call_expression" if m.utf8_text(src).unwrap_or("").starts_with('-') => hit = true,
        "function_call_expression" | "ambiguous_function_call_expression" => {
            let name = m.child_by_field_name("function").and_then(|f| f.utf8_text(src).ok()).unwrap_or("");
            if !crate::model::builtins::is_builtin(name) {
                hit = true;
            }
        }
        _ => {}
    });
    hit
}

fn needs_scalar(n: Node) -> bool {
    matches!(n.kind(), "function_call_expression" | "ambiguous_function_call_expression" | "method_call_expression")
}

fn has_effect(n: Node) -> bool {
    let mut hit = false;
    visit(n, &mut |m| {
        if matches!(
            m.kind(),
            "assignment_expression" | "postinc_expression" | "preinc_expression" | "substitution_regexp"
        ) {
            hit = true;
        }
    });
    hit
}
