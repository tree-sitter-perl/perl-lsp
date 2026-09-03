//! Arity/return classification: which arity branch a `return` guards,
//! agreement folding across arms, and return-delegation chain helpers.

use super::*;

/// Inspect the `return_expression`'s parent. If it's a
/// `postfix_conditional_expression` with `@_` as the condition, we
/// look at the connector keyword (`if` vs `unless`) to decide. If
/// it's a bare expression_statement, this is a default branch.
///
/// Known idioms:
///   - `return X unless @_;`       → Zero
///   - `return X;`                  → Default
///
/// Unknowns (return None and punt):
///   - `return X if @_;`            (arity >= 1 narrowing)
///   - `return X if @_ == N;`       (exact N)
///   - `return X if scalar @_ …;`   (scalar wrapper)
pub(super) fn classify_arity_branch(return_node: tree_sitter::Node, source: &[u8]) -> Option<ArityBranch> {
    let Some(parent) = return_node.parent() else { return None };
    match parent.kind() {
        "expression_statement" => classify_bare_return_or_if_arm(parent, source),
        "postfix_conditional_expression" => {
            let cond = parent.child_by_field_name("condition")?;
            let keyword = connector_keyword_between(return_node, cond, source)?;
            classify_arity_condition(cond, source, &keyword)
        }
        _ => None,
    }
}

/// `expression_statement` parent → either a bare `return X` at sub
/// body level (Default), OR inside an `if (@_ == N) { return X }` arm
/// where the conditional_statement's condition is an arity test (Exact).
pub(super) fn classify_bare_return_or_if_arm(
    expr_stmt: tree_sitter::Node,
    source: &[u8],
) -> Option<ArityBranch> {
    let Some(block) = expr_stmt.parent() else { return None };
    if block.kind() != "block" {
        return None;
    }
    let Some(outer) = block.parent() else { return None };
    match outer.kind() {
        "subroutine_declaration_statement"
        | "method_declaration_statement"
        | "anonymous_subroutine_expression" => Some(ArityBranch::Default),
        "conditional_statement" => {
            // `if (condition) { return X }` — classify by condition.
            // Arity-gated condition → Zero / Exact. Anything else
            // (regular boolean predicate) is still a contributor to
            // the default return shape: the arm runs whenever the
            // condition holds, regardless of arity. Classify as
            // Default so `emit_arity_return_witnesses` folds it into
            // the union's `Any` arm.
            let cond = outer.child_by_field_name("condition")?;
            classify_arity_condition(cond, source, "if").or(Some(ArityBranch::Default))
        }
        _ => None,
    }
}

/// Classify an arity condition node. `keyword` is "if" or "unless".
pub(super) fn classify_arity_condition(
    cond: tree_sitter::Node,
    source: &[u8],
    keyword: &str,
) -> Option<ArityBranch> {
    // Shape 1: bare `@_`.
    if cond.kind() == "array" {
        let text = cond.utf8_text(source).ok()?.trim();
        if text == "@_" {
            return match keyword {
                "unless" => Some(ArityBranch::Zero),
                _ => None, // `if @_` is "arity >= 1" — not expressible as Exact yet.
            };
        }
        return None;
    }
    // Shape 2: `!@_` → unary_expression with operand @_.
    if cond.kind() == "unary_expression" {
        let op_text = raw_leading_op(cond, source);
        if op_text == "!" {
            if let Some(operand) = cond.child_by_field_name("operand") {
                if operand.kind() == "array" {
                    let text = operand.utf8_text(source).ok()?.trim();
                    if text == "@_" {
                        return match keyword {
                            "if" => Some(ArityBranch::Zero),
                            "unless" => None, // `unless !@_` → arity >= 1, skip
                            _ => None,
                        };
                    }
                }
            }
        }
        return None;
    }
    // Shape 3: `@_ == N` / `scalar(@_) == N` → equality_expression.
    if cond.kind() == "equality_expression" {
        let op = raw_mid_op(cond, source);
        if op != "==" && op != "!=" {
            return None;
        }
        let left = cond.child_by_field_name("left")?;
        let right = cond.child_by_field_name("right")?;
        let n = extract_numeric(right, source)?;
        let counts_args = node_is_arity_magnitude(left, source);
        if !counts_args {
            return None;
        }
        return match (keyword, op.as_str()) {
            ("if", "==") => Some(ArityBranch::Exact(n)),
            ("unless", "!=") => Some(ArityBranch::Exact(n)),
            _ => None, // != / >= / etc. — not a single Exact fact.
        };
    }
    // Shape 4: `@_ > N` / `@_ < N` / `@_ >= N` / `@_ <= N` → relational.
    if cond.kind() == "relational_expression" {
        return classify_relational_arity(cond, source, keyword);
    }
    // Shape 5: compound `A || B` / `A && B` — one side is an arity test, the
    // other an unrelated predicate. Keep ONLY the sound constraint:
    //   - `unless (A || B)` fires ⟺ `!A && !B` ⇒ `!A` (arity side classified
    //     under `unless`) is necessary — `!B` only narrows further.
    //   - `if (A && B)` fires ⟺ `A && B` ⇒ the arity conjunct under `if` is
    //     necessary.
    // The other two (`if (A||B)`, `unless (A&&B)`) can fire via the non-arity
    // term at any arity, so no sound arity constraint — punt.
    if cond.kind() == "binary_expression" {
        let op = raw_mid_op(cond, source);
        let sound = matches!(
            (keyword, op.as_str()),
            ("unless", "||") | ("unless", "or") | ("if", "&&") | ("if", "and")
        );
        if !sound {
            return None;
        }
        let left = cond.child_by_field_name("left")?;
        let right = cond.child_by_field_name("right")?;
        // Either operand may carry the arity test; the other is the
        // unrelated predicate. First sound classification wins.
        return classify_arity_condition(left, source, keyword)
            .or_else(|| classify_arity_condition(right, source, keyword));
    }
    None
}

/// Classify `@_ CMP N` (relational). Arity magnitude must be the LEFT
/// operand, a literal the right. `if @_ > 1` fires at arity ≥ 2
/// (`AtLeast(2)`); `unless @_ > 1` fires at arity ≤ 1 (`AtMost(1)`), the
/// Mojo `attr`-style getter guard.
pub(super) fn classify_relational_arity(
    cond: tree_sitter::Node,
    source: &[u8],
    keyword: &str,
) -> Option<ArityBranch> {
    let op = raw_mid_op(cond, source);
    let left = cond.child_by_field_name("left")?;
    let right = cond.child_by_field_name("right")?;
    if !node_is_arity_magnitude(left, source) {
        return None;
    }
    let n = extract_numeric(right, source)?;
    // `keyword`-fires semantics: `if COND` fires when COND holds; `unless
    // COND` fires when it doesn't. Map each to the arity band it guarantees.
    match (keyword, op.as_str()) {
        // @_ > N: if → ≥ N+1; unless → ≤ N
        ("if", ">") => n.checked_add(1).map(ArityBranch::AtLeast),
        ("unless", ">") => Some(ArityBranch::AtMost(n)),
        // @_ >= N: if → ≥ N; unless → ≤ N-1
        ("if", ">=") => Some(ArityBranch::AtLeast(n)),
        ("unless", ">=") => n.checked_sub(1).map(ArityBranch::AtMost),
        // @_ < N: if → ≤ N-1; unless → ≥ N
        ("if", "<") => n.checked_sub(1).map(ArityBranch::AtMost),
        ("unless", "<") => Some(ArityBranch::AtLeast(n)),
        // @_ <= N: if → ≤ N; unless → ≥ N+1
        ("if", "<=") => Some(ArityBranch::AtMost(n)),
        ("unless", "<=") => n.checked_add(1).map(ArityBranch::AtLeast),
        _ => None,
    }
}

/// Do the return arms agree in a way that is the sub's genuine return type —
/// as opposed to the lossy Object-subsumes-HashRef *dominance* that
/// `resolve_return_type` also accepts? Decides whether an arity-discriminated
/// sub keeps its non-arity `return_arm_chain` fallback (agree → the arm-join
/// is the right answer at every arity and at a no-hint query) or retracts it
/// (disagree → a gap arity must answer None, not the fluent-class leak).
pub(super) fn arms_genuinely_agree(types: &[InferredType]) -> Option<InferredType> {
    let first = types.first()?;
    if types.iter().all(|t| t == first) {
        return Some(first.clone());
    }
    if types.iter().all(|t| t.is_hash_shaped()) {
        return Some(InferredType::HashRef);
    }
    if types.iter().all(|t| t.is_array_shaped()) {
        return Some(InferredType::ArrayRef);
    }
    if types.iter().all(|t| matches!(t, InferredType::Bool | InferredType::Numeric)) {
        return Some(InferredType::Numeric);
    }
    None
}

/// True if `node` evaluates to the length of `@_` — either `@_`
/// itself (scalar context in an equality) or `scalar(@_)`.
pub(super) fn node_is_arity_magnitude(node: tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() == "array" {
        return node.utf8_text(source).map(|s| s.trim() == "@_").unwrap_or(false);
    }
    if node.kind() == "func1op_call_expression" {
        let Some(kw) = node.child(0) else { return false };
        let Ok(name) = kw.utf8_text(source) else { return false };
        if name != "scalar" {
            return false;
        }
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                if c.kind() == "array" {
                    return c.utf8_text(source).map(|s| s.trim() == "@_").unwrap_or(false);
                }
            }
        }
    }
    false
}

/// Extract a small unsigned numeric literal from a `number` node.
pub(super) fn extract_numeric(node: tree_sitter::Node, source: &[u8]) -> Option<u32> {
    if node.kind() != "number" {
        return None;
    }
    node.utf8_text(source).ok()?.trim().parse::<u32>().ok()
}


/// Read the raw bytes between two sibling nodes to recover the
/// postfix keyword (`if` / `unless` / `while` / …). Used because
/// tree-sitter-perl shares one node kind for both `if` and `unless`.
pub(super) fn connector_keyword_between(
    left: tree_sitter::Node,
    right: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let start = left.end_byte();
    let end = right.start_byte();
    if end <= start {
        return None;
    }
    let between = std::str::from_utf8(&source[start..end]).ok()?.trim();
    for kw in ["unless", "if", "while", "until"] {
        if between == kw || between.starts_with(kw) && between.trim() == kw {
            return Some(kw.to_string());
        }
        if between.split_whitespace().next() == Some(kw) {
            return Some(kw.to_string());
        }
    }
    None
}

pub(super) fn raw_leading_op(node: tree_sitter::Node, source: &[u8]) -> String {
    // Operator is an anonymous child between start and the first named
    // child (operand). Read the bytes before the operand node.
    let Some(operand) = node.child_by_field_name("operand") else {
        return String::new();
    };
    let start = node.start_byte();
    let end = operand.start_byte();
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(super) fn raw_mid_op(node: tree_sitter::Node, source: &[u8]) -> String {
    let Some(left) = node.child_by_field_name("left") else {
        return String::new();
    };
    let Some(right) = node.child_by_field_name("right") else {
        return String::new();
    };
    let start = left.end_byte();
    let end = right.start_byte();
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(super) fn point_lt(a: tree_sitter::Point, b: tree_sitter::Point) -> bool {
    (a.row, a.column) < (b.row, b.column)
}


/// Strip a `Pkg::Sub::` prefix from a sub-name identifier, returning the bare
/// trailing component. `"Foo::Bar::baz"` → `"baz"`; `"baz"` → `"baz"`. Pure
/// string op — does not consult the symbol table or package state.
pub(super) fn bare_name(s: &str) -> &str {
    crate::model::file_analysis::split_qualified(s).1
}

/// If `return_node` is `return CALL`, where CALL is a simple named function
/// call or method call, return the bare called name. Otherwise None. Used to
/// collect hash-key ownership delegation chains for post-pass resolution.
pub(super) fn extract_delegated_call_name<'a>(return_node: Node<'a>, source: &'a [u8]) -> Option<String> {
    // The `return X` node has the expression as its first named child.
    let expr = return_node.named_child(0)?;
    let call_name = match expr.kind() {
        "function_call_expression" | "ambiguous_function_call_expression" => {
            expr.child_by_field_name("function")?.utf8_text(source).ok()?
        }
        "method_call_expression" => {
            expr.child_by_field_name("method")?.utf8_text(source).ok()?
        }
        _ => return None,
    };
    // Strip package prefix — delegation is stored by bare sub name to match
    // the return_types lookup convention.
    Some(bare_name(call_name).to_string())
}

/// Find the `varname` child of a variable node (`scalar`/`array`/`hash`/etc.).
/// The grammar aliases its `_var_indirob` into a `varname` node whose text
/// is the bare variable name — no sigil, no braces. For `${foo}` the outer
/// `scalar` text is `${foo}` but the `varname` child text is just `foo`;
/// for `$:whatever` it's whatever TSP decided is the name token.
///
/// For `${$hash{k}}` and other nontrivial derefs the varname child is a
/// `block` — callers that only want a simple identifier should check the
/// returned node's kind (`varname` text is only meaningful for the
/// identifier form).
pub(super) fn find_varname_child<'a>(node: Node<'a>) -> Option<Node<'a>> {
    crate::cst::varname_child(node)
}

/// Find a directly-contained `code_deref_expression` in `node` (the grammar
/// wraps it in a `function` node inside a call's `function` field).
pub(super) fn code_deref_in<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if node.kind() == "code_deref_expression" {
        return Some(node);
    }
    for i in 0..node.named_child_count() {
        let child = node.named_child(i)?;
        if child.kind() == "code_deref_expression" {
            return Some(child);
        }
    }
    None
}

/// The deref operand inside `&{ EXPR }` — `code_deref_expression`'s child is a
/// `block` wrapping `expression_statement(EXPR)`. Returns the inner EXPR node
/// (typically a `scalar`) so callers can type/navigate the symbolic target.
pub(super) fn code_deref_operand<'a>(code_deref: Node<'a>) -> Option<Node<'a>> {
    let block = code_deref.named_child(0)?;
    if block.kind() != "block" {
        // Defensive: future grammar may inline the expression.
        return Some(block);
    }
    let stmt = block.named_child(0)?;
    if stmt.kind() == "expression_statement" {
        stmt.named_child(0)
    } else {
        Some(stmt)
    }
}

/// Find the `data_section` node (the region after `__END__` / `__DATA__`)
/// among a `source_file`'s direct children, if any.
pub(super) fn find_data_section<'a>(root: Node<'a>) -> Option<Node<'a>> {
    for i in 0..root.child_count() {
        let child = root.child(i)?;
        if child.kind() == "data_section" {
            return Some(child);
        }
    }
    None
}

/// Collect `subroutine_declaration_statement` nodes from a re-parsed
/// data-section tree. Recurses through wrappers (a second `__END__` inside
/// the section parks its tail in a nested `data_section`, which we don't
/// descend — it isn't Perl). POD blocks parse as `pod` nodes and are
/// skipped by virtue of not being subroutine declarations.
pub(super) fn collect_data_section_subs<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "subroutine_declaration_statement" => out.push(child),
            // Don't mine a nested data_section — its bytes are payload, not code.
            "data_section" => {}
            _ => collect_data_section_subs(child, out),
        }
    }
}

/// Extract positional `ParamInfo`s from a re-parsed data-section sub. Reads
/// the sub's own `(...)` signature when present (Perl 5.20+ signatures); the
/// classic `my ($a, $b) = @_;` idiom is left to the empty-params default —
/// data-section subs only need navigability, not full type inference.
pub(super) fn extract_data_section_params(sub_node: Node, source: &[u8]) -> Vec<ParamInfo> {
    let mut params = Vec::new();
    let mut sig = None;
    for i in 0..sub_node.child_count() {
        if let Some(c) = sub_node.child(i) {
            if c.kind() == "signature" {
                sig = Some(c);
                break;
            }
        }
    }
    let Some(sig) = sig else { return params };
    for i in 0..sig.named_child_count() {
        let Some(p) = sig.named_child(i) else { continue };
        if matches!(p.kind(), "scalar" | "array" | "hash") {
            if let Ok(text) = p.utf8_text(source) {
                params.push(ParamInfo {
                    name: text.to_string(),
                    default: None,
                    is_slurpy: matches!(p.kind(), "array" | "hash"),
                    is_invocant: false,
                });
            }
        }
    }
    params
}

/// Walk the delegation chain starting at `start` until we find a sub that
/// actually owns HashKeyDefs, or run out of links. Cycle-safe via a visited
/// set; caps at a small depth since delegation chains in real code are short.
pub(super) fn walk_return_delegation_chain(
    start: &str,
    delegations: &std::collections::HashMap<String, String>,
    subs_with_own_keys: &std::collections::HashSet<String>,
) -> String {
    let mut current = start.to_string();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..10 {
        if subs_with_own_keys.contains(&current) {
            return current;
        }
        if !seen.insert(current.clone()) {
            return current; // cycle guard
        }
        match delegations.get(&current) {
            Some(next) => current = next.clone(),
            None => return current,
        }
    }
    current
}
