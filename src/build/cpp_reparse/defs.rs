//! Macro-definition collection: the tree-sitter queries, `#define`
//! walking/classification, body-ref scanning, guard/variant gathering, and
//! the body-cleanup + textual-expansion primitives they feed.

use super::*;
use crate::model::file_analysis::{CondTerm, CondTest};

pub(super) const MACRO_DEF_QUERY: &str = r#"
(preproc_def name: (identifier) @oname value: (preproc_arg) @obody)
(preproc_def name: (identifier) @bname !value)
(preproc_function_def
  name: (identifier) @fname
  parameters: (preproc_params) @fparams
  value: (preproc_arg) @fbody)
"#;

/// Spans to never expand inside: string/char literals, comments, and
/// the preprocessor definition/conditional DIRECTIVE lines themselves.
///
/// Conditional regions (`#ifdef`/`#if`/`#elif`) exclude only their
/// `name:`/`condition:` field — the directive-line tokens — NOT the whole
/// node: the region BODY must stay expandable so a macro use between
/// `#ifdef` and `#endif` still expands (`docs/adr/config-superposition-
/// declarations.md`, slice 1: whole-node exclusion left perl5's `pTHX_`
/// literal inside every conditional function, mistyping the receiver).
/// The condition/name stays excluded so a macro name on the directive line
/// (`#ifdef FOO`, `#if defined(FOO)`) is never rewritten.
pub(super) const EXCLUDE_QUERY: &str = r#"
(string_literal) @x
(char_literal) @x
(comment) @x
(preproc_def) @x
(preproc_function_def) @x
(preproc_call) @x
(preproc_ifdef name: (identifier) @x)
(preproc_if condition: (_) @x)
(preproc_elif condition: (_) @x)
(preproc_include) @x
"#;

/// The pre-widening WIDE exclusion: whole conditional region excluded (body
/// included). Used ONLY as the fallback when the default narrow expansion above
/// RAISES parse damage on a file — a huge macro-heavy source (perl.h/op.c)
/// re-excludes its region bodies and keeps its prior fast expansion instead of
/// paying the salvage cliff for the widened scope. See `EXCLUDE_QUERY` and
/// `docs/adr/config-superposition-declarations.md` slice 1.
pub(super) const EXCLUDE_QUERY_WIDE: &str = r#"
(string_literal) @x
(char_literal) @x
(comment) @x
(preproc_def) @x
(preproc_function_def) @x
(preproc_call) @x
(preproc_ifdef) @x
(preproc_if) @x
(preproc_include) @x
"#;

pub(super) const INCLUDE_QUERY: &str = r##"
(preproc_include path: (string_literal (string_content) @p))
(preproc_include path: (system_lib_string) @s)
(preproc_call directive: (preproc_directive) @_d argument: (preproc_arg) @n (#eq? @_d "#include_next"))
"##;

/// A function-like macro use that already parses as a call — the "leave" set
/// for the expansion flip (`clean_call_sites`).
pub(super) const CALL_QUERY: &str = r#"
(call_expression function: (identifier) @f)
"#;

/// Compile-once cache for this pipeline's queries. Every tree here comes
/// from the one `tree_sitter_cpp` grammar (the C/C++ driver), so a single
/// `Query` per source is reused across every reparse instead of rebuilding
/// the automaton per keystroke. `Query` is `Send + Sync`, so a static slot
/// is safe.
pub(super) static MACRO_DEF_Q: OnceLock<Query> = OnceLock::new();
pub(super) static EXCLUDE_Q: OnceLock<Query> = OnceLock::new();
pub(super) static EXCLUDE_Q_WIDE: OnceLock<Query> = OnceLock::new();
pub(super) static INCLUDE_Q: OnceLock<Query> = OnceLock::new();
pub(super) static CALL_Q: OnceLock<Query> = OnceLock::new();

pub(super) fn cached_query(slot: &'static OnceLock<Query>, lang: &tree_sitter::Language, src: &str) -> &'static Query {
    slot.get_or_init(|| Query::new(lang, src).expect("cpp_reparse query"))
}

/// Walk `collect_macros`' query, calling `emit(name, Macro)` per `#define`
/// (object- and function-like). The Macro carries its config guard trail —
/// the enclosing `#if`/`#ifdef`/`#else` conditions — captured from the CST
/// ancestors of the def. Both the dedup'd table (expansion side) and the
/// variant-preserving collection route through here so the guard trail is
/// captured once.
pub(super) fn walk_macro_defs(
    tree: &Tree,
    src: &[u8],
    mut emit: impl FnMut(String, Macro, (tree_sitter::Point, tree_sitter::Point)),
) {
    let query = cached_query(&MACRO_DEF_Q, &tree.language(), MACRO_DEF_QUERY);
    let names: Vec<&str> = query.capture_names().to_vec();
    // Bodies are re-derived from raw source (comment truncation), not node text.
    let source = std::str::from_utf8(src).unwrap_or("");
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = it.next() {
        let mut oname = None;
        let mut obody = None;
        let mut bname = None;
        let mut fname = None;
        let mut fparams: Option<Vec<String>> = None;
        let mut fbody = None;
        // Any name capture pins the def site (its parent is the preproc_def).
        let mut name_node: Option<tree_sitter::Node> = None;
        for c in m.captures {
            let txt = c.node.utf8_text(src).unwrap_or("");
            match names[c.index as usize] {
                "oname" => {
                    oname = Some(txt.to_string());
                    name_node = Some(c.node);
                }
                "obody" => obody = Some(clean_body(raw_macro_body(source, c.node.start_byte()))),
                "bname" => {
                    bname = Some(txt.to_string());
                    name_node = Some(c.node);
                }
                "fname" => {
                    fname = Some(txt.to_string());
                    name_node = Some(c.node);
                }
                "fparams" => {
                    fparams = Some(
                        txt.trim_start_matches('(')
                            .trim_end_matches(')')
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    )
                }
                "fbody" => fbody = Some(clean_body(raw_macro_body(source, c.node.start_byte()))),
                _ => {}
            }
        }
        let guards = name_node.map(|n| guard_trail(n, src)).unwrap_or_default();
        let def_line = name_node.map(|n| n.start_position().row).unwrap_or(0);
        let name_span = name_node
            .map(|n| (n.start_position(), n.end_position()))
            .unwrap_or_default();
        if let (Some(n), Some(b)) = (oname, obody) {
            emit(n, Macro { params: None, body: b, guards: guards.clone(), def_line, variant_delegates: Vec::new() }, name_span);
        }
        // Bodyless `#define FLAG` — the canonical config knob (feature
        // toggles, include guards, `PERL_CORE`-style markers). It must enter
        // the definition universe or reachability ranks `#ifdef FLAG` arms
        // exactly inverted; its empty body is also C-correct for expansion
        // (a bare use of the flag expands to nothing).
        if let Some(n) = bname {
            emit(
                n,
                Macro { params: None, body: String::new(), guards: guards.clone(), def_line, variant_delegates: Vec::new() },
                name_span,
            );
        }
        if let (Some(n), Some(p), Some(b)) = (fname, fparams, fbody) {
            emit(n, Macro { params: Some(p), body: b, guards, def_line, variant_delegates: Vec::new() }, name_span);
        }
    }
}

pub fn collect_macros(tree: &Tree, src: &[u8]) -> BTreeMap<String, Macro> {
    let mut out: BTreeMap<String, Macro> = BTreeMap::new();
    walk_macro_defs(tree, src, |n, m, _span| insert_folded(&mut out, n, m));
    out
}

/// Last definition wins the body; the shadowed one's delegates survive.
fn insert_folded(table: &mut BTreeMap<String, Macro>, name: String, mut m: Macro) {
    if let Some(prev) = table.remove(&name) {
        m.absorb_variant(&prev);
    }
    table.insert(name, m);
}

impl Macro {
    /// This definition's direct-delegation callee. Function-like: a
    /// whole-body single call `G(args)`. Object-like: a bare-identifier ALIAS
    /// (`#define op_prune_chain_head Perl_op_prune_chain_head`, perl5's
    /// non-threaded embed.h shape) — the same forwarding edge, spelled
    /// without params.
    pub fn delegate(&self) -> Option<String> {
        match self.params {
            Some(_) => delegation_target(&self.body),
            None => bare_identifier(&self.body),
        }
    }

    /// Every callee a use of this name may forward to, across all its
    /// config variants.
    pub fn all_delegates(&self) -> impl Iterator<Item = String> + '_ {
        self.delegate().into_iter().chain(self.variant_delegates.iter().cloned())
    }

    /// Fold a shadowed same-named definition's delegates into this one.
    pub fn absorb_variant(&mut self, other: &Macro) {
        let own = self.delegate();
        for d in other.all_delegates() {
            if own.as_ref() != Some(&d) && !self.variant_delegates.contains(&d) {
                self.variant_delegates.push(d);
            }
        }
    }
}

/// The macro identity/navigation lane: every `#define` as a `MacroDef` carrying
/// its guard trail, def-site span, and — for a direct-delegation wrapper —
/// the callee it forwards to. Consumed by goto-def (`#define`-preference,
/// reachability-ranked multi-location, see-through). Parses `source` fresh so
/// def spans are in ORIGINAL coordinates (the expansion tree splices usages).
pub fn collect_macro_defs(
    parser: &mut tree_sitter::Parser,
    source: &str,
    arms: &mut crate::model::file_analysis::CondArms,
) -> Vec<crate::model::file_analysis::MacroDef> {
    collect_file_macros(parser, source, arms).defs
}

/// One file's own `#define`s, from one parse and one walk: the per-variant
/// identity lane AND the folded table (`collect_macros`' shape), so the
/// delegation lane reads a variant's callees from the same `Macro` the
/// gathered closure does.
#[derive(Default)]
pub struct FileMacros {
    pub defs: Vec<crate::model::file_analysis::MacroDef>,
    pub table: BTreeMap<String, Macro>,
}

pub fn collect_file_macros(
    parser: &mut tree_sitter::Parser,
    source: &str,
    arms: &mut crate::model::file_analysis::CondArms,
) -> FileMacros {
    use crate::model::file_analysis::{MacroDef, Span};
    let mut out = FileMacros::default();
    let Some(tree) = parser.parse(source, None) else { return out };
    walk_macro_defs(&tree, source.as_bytes(), |name, m, (start, end)| {
        out.defs.push(MacroDef {
            name: name.clone(),
            params: m.params.clone(),
            body: m.body.clone(),
            arm: arms.intern(m.guards.clone()),
            selection_span: Span { start, end },
            delegate: m.delegate(),
        });
        insert_folded(&mut out.table, name, m);
    });
    out
}

/// A direct-delegation body — a single call `G(args)` whose whole point is to
/// forward to `G` (`SvREFCNT_inc(sv)` → `Perl_SvREFCNT_inc(MUTABLE_SV(sv))`).
/// Returns the callee identifier `G` when the body IS exactly one such call
/// (a leading identifier immediately followed by a balanced `(...)` that spans
/// to the end), else `None`. General over the shape — no per-name table.
/// A body that is nothing but one identifier — an object-like alias's
/// forwarding target. Digit-leading (a number) is not an identifier.
fn bare_identifier(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty()
        || body.as_bytes()[0].is_ascii_digit()
        || !body.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(body.to_string())
}

fn delegation_target(body: &str) -> Option<String> {
    let body = body.trim();
    let paren = body.find('(')?;
    let callee = body[..paren].trim();
    if callee.is_empty() || !callee.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric()) {
        return None;
    }
    if callee.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    // The call must span the whole body: walk the parens, and nothing but
    // whitespace may follow the matching close (`F(x) + 1` is not delegation).
    let mut depth = 0i32;
    for (i, c) in body.bytes().enumerate().skip(paren) {
        match c {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return body[i + 1..].trim().is_empty().then(|| callee.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The param-INDEPENDENT type a function-like macro body evaluates to — the
/// implied return of `#define F(x) …expr…` when the result type doesn't depend
/// on the argument (`((x)*(x))` is `Numeric` whatever `x` is). Returns `None`
/// for a bare-param body (`(x)`) or anything argument-dependent (PARKED per the
/// ADR: parametric return is a later tier). Delegation bodies (`G(x)`) are
/// handled by the caller via `MacroDef::delegate` — this is the non-delegation
/// expression lane. A tiny recursive classifier over the parsed body, not a
/// full type engine: C's binary/comparison/bitwise/shift/logical operators all
/// yield a numeric value regardless of operand types, so the common wrapper
/// macro types without arg inference.
pub fn classify_body_type(
    parser: &mut tree_sitter::Parser,
    body: &str,
) -> Option<crate::model::file_analysis::InferredType> {
    // Wrap so the body parses as an initializer expression the tree exposes
    // cleanly (a bare `((x)*(x))` alone is a MISSING-`;` statement).
    let wrapped = format!("int __macro_ret__ = {body};");
    let tree = parser.parse(&wrapped, None)?;
    let decl = tree.root_node().named_child(0)?;
    // declaration → declarator: (init_declarator) → value:
    let value = decl
        .child_by_field_name("declarator")
        .filter(|n| n.kind() == "init_declarator")
        .and_then(|n| n.child_by_field_name("value"))?;
    classify_expr_node(value)
}

/// The macro parameter this body reduces to, if any: `#define ID(x) (x)` →
/// `Some(0)`, `#define SEL2(a,b) (b)` → `Some(1)`. Paren and cast wrappers are
/// transparent — `#define CAST(x) ((Widget*)(x))` is still the argument's
/// value (the cast type is not recovered; "record what's cheap" per the ADR).
/// Returns `None` for a body that isn't a bare parameter under wrappers (a
/// literal, an operator expression, `G(x)` delegation, `a + b`). The
/// param-DEPENDENT sibling of `classify_body_type`.
pub fn classify_param_return(
    parser: &mut tree_sitter::Parser,
    body: &str,
    params: &[String],
) -> Option<u32> {
    let wrapped = format!("int __macro_ret__ = {body};");
    let tree = parser.parse(&wrapped, None)?;
    let decl = tree.root_node().named_child(0)?;
    let value = decl
        .child_by_field_name("declarator")
        .filter(|n| n.kind() == "init_declarator")
        .and_then(|n| n.child_by_field_name("value"))?;
    let name = param_identity_node(value, wrapped.as_bytes())?;
    params.iter().position(|p| p == name).map(|i| i as u32)
}

/// Strip paren/cast wrappers to the bare identifier a body evaluates to (the
/// value's identity, not its type); `None` if the peeled core isn't a single
/// identifier.
fn param_identity_node<'a>(node: tree_sitter::Node, src: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        "identifier" => node.utf8_text(src).ok(),
        "parenthesized_expression" => param_identity_node(node.named_child(0)?, src),
        "cast_expression" => param_identity_node(node.child_by_field_name("value")?, src),
        _ => None,
    }
}

/// Per-call-site argument spans for calls whose callee is one of `names`
/// (function-like macros left unexpanded → `call_expression`s). Keyed by the
/// call span so the macro lane can edge a `Param(n)` call to its n-th
/// argument's value witness. Spans are in `source` (original) coordinates —
/// the same frame the extractor's remapped witnesses land in.
pub fn macro_call_arg_spans(
    parser: &mut tree_sitter::Parser,
    source: &str,
    names: &std::collections::HashSet<String>,
) -> Vec<(crate::model::file_analysis::Span, Vec<crate::model::file_analysis::Span>)> {
    use crate::model::file_analysis::Span;
    let Some(tree) = parser.parse(source, None) else { return Vec::new() };
    let src = source.as_bytes();
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
        if node.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = node.child_by_field_name("function") else { continue };
        if callee.kind() != "identifier" {
            continue;
        }
        let Some(callee_name) = callee.utf8_text(src).ok() else { continue };
        if !names.contains(callee_name) {
            continue;
        }
        let Some(arglist) = node.child_by_field_name("arguments") else { continue };
        let mut argc = arglist.walk();
        let arg_spans: Vec<Span> = arglist
            .named_children(&mut argc)
            .filter(|n| n.kind() != "comment")
            .map(|n| Span { start: n.start_position(), end: n.end_position() })
            .collect();
        out.push((
            Span { start: node.start_position(), end: node.end_position() },
            arg_spans,
        ));
    }
    out
}

/// The two reference lanes a `#define` body hides from the code parser (the
/// body is one opaque `preproc_arg` token, so nothing inside it surfaces as a
/// query capture): known-macro NAME uses, and member-access FIELD uses.
#[derive(Default)]
pub struct MacroBodyRefs {
    /// `(name, span)` per token naming a KNOWN macro (`#define IS_OK(x)
    /// (FLAGS(x) & 1)` references `FLAGS`; perl5 `SvFLAGS` inside `SvOK`).
    pub name_refs: Vec<(String, crate::model::file_analysis::Span)>,
    /// `(field, span)` per member-access token (`->op_next` / `.op_next`)
    /// inside a body. Untyped here — the receiver is a macro parameter with no
    /// type — so it is left as a bare `(field, span)` candidate; the assembly
    /// pass (`into_file_analysis`) resolves it against the file's own field
    /// symbols and mints a class-frozen `MethodCall` ref so references on the
    /// field include the in-body use (perl5 `->op_next` drills are heavy in
    /// bodies like `OP_NAME`/`cUNOPx`; rule #7).
    pub member_refs: Vec<(String, crate::model::file_analysis::Span)>,
}

/// Scan every `#define` body in ORIGINAL coordinates (def bodies are never
/// spliced) for the two hidden reference lanes above. A NAME use is minted per
/// token that (a) names a known macro and (b) is not the macro's own parameter;
/// a MEMBER use is minted per identifier immediately following a `->`/`.`
/// operator. Comments, string/char literals, and `#`/`##` stringify/paste
/// operands are skipped — a pasted or stringified token is textual, not a real
/// reference (rule: prefer silence over a wrong ref). Body end is the LOGICAL
/// line end (`logical_body_end`), not the CST node's, so continuation-past-
/// comment tokens are still seen.
pub fn macro_body_name_refs(
    parser: &mut tree_sitter::Parser,
    source: &str,
    known: &std::collections::HashSet<String>,
) -> MacroBodyRefs {
    let mut out = MacroBodyRefs::default();
    let Some(tree) = parser.parse(source, None) else { return out };
    let src = source.as_bytes();
    let query = cached_query(&MACRO_DEF_Q, &tree.language(), MACRO_DEF_QUERY);
    let names: Vec<&str> = query.capture_names().to_vec();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = it.next() {
        let mut body: Option<tree_sitter::Node> = None;
        let mut params: Vec<String> = Vec::new();
        for c in m.captures {
            match names[c.index as usize] {
                "obody" | "fbody" => body = Some(c.node),
                "fparams" => {
                    let txt = c.node.utf8_text(src).unwrap_or("");
                    params = txt
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                _ => {}
            }
        }
        let Some(body) = body else { continue };
        scan_body_name_refs(src, body, known, &params, &mut out);
    }
    out
}

/// Lexically scan `body`'s logical extent (comment/literal-aware) and push a
/// NAME use per known-macro identifier token and a MEMBER use per identifier
/// that immediately follows a `->`/`.` member operator. Point coordinates are
/// tracked from the node's start position; identifiers never cross a newline.
fn scan_body_name_refs(
    src: &[u8],
    body: tree_sitter::Node,
    known: &std::collections::HashSet<String>,
    params: &[String],
    out: &mut MacroBodyRefs,
) {
    use crate::model::file_analysis::Span;
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    let start = body.start_byte();
    let end = logical_body_end(src, start).min(src.len());
    let mut i = start;
    let start_pt = body.start_position();
    let (mut row, mut col) = (start_pt.row, start_pt.column);
    // The most recent non-whitespace byte — for the stringify/paste-right
    // operand check (`#X`, or `X` after the second `#` of `Y ## X`).
    let mut prev_nonspace = 0u8;
    let bump = |b: u8, row: &mut usize, col: &mut usize| {
        if b == b'\n' {
            *row += 1;
            *col = 0;
        } else {
            *col += 1;
        }
    };
    while i < end {
        let b = src[i];
        match (b, src.get(i + 1).copied()) {
            (b'/', Some(b'*')) => {
                while i < end && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                    bump(src[i], &mut row, &mut col);
                    i += 1;
                }
                // consume the closing `*/`
                for _ in 0..2 {
                    if i < end {
                        bump(src[i], &mut row, &mut col);
                        i += 1;
                    }
                }
                prev_nonspace = b'/';
            }
            (b'/', Some(b'/')) => {
                while i < end && src[i] != b'\n' {
                    bump(src[i], &mut row, &mut col);
                    i += 1;
                }
            }
            (q @ (b'"' | b'\''), _) => {
                bump(b, &mut row, &mut col);
                i += 1;
                while i < end {
                    let c = src[i];
                    bump(c, &mut row, &mut col);
                    i += 1;
                    if c == b'\\' {
                        if i < end {
                            bump(src[i], &mut row, &mut col);
                            i += 1;
                        }
                    } else if c == q {
                        break;
                    }
                }
                prev_nonspace = q;
            }
            _ if is_id(b) => {
                let (srow, scol) = (row, col);
                let tok_start = i;
                while i < end && is_id(src[i]) {
                    bump(src[i], &mut row, &mut col);
                    i += 1;
                }
                let name = &src[tok_start..i];
                let span = Span {
                    start: tree_sitter::Point { row: srow, column: scol },
                    end: tree_sitter::Point { row, column: col },
                };
                // A member-access token (`recv->FIELD` / `recv.FIELD`) is a
                // field use, never a macro invocation — look back past inline
                // whitespace for the operator. `->` needs both bytes; a `.` is
                // a member dot unless it's the second `.` of `..`. Digit-led
                // tokens (a float's `.5`) can't be a field, so gate on an
                // identifier START. The receiver is a macro param with no type,
                // so the field's class is resolved downstream, not here.
                let is_member = name.first().is_some_and(|c| *c == b'_' || c.is_ascii_alphabetic())
                    && {
                        let mut k = tok_start;
                        while k > start && matches!(src[k - 1], b' ' | b'\t') {
                            k -= 1;
                        }
                        (k >= start + 2 && src[k - 1] == b'>' && src[k - 2] == b'-')
                            || (k > start
                                && src[k - 1] == b'.'
                                && !(k >= start + 2 && src[k - 2] == b'.'))
                    };
                if is_member {
                    if let Ok(s) = std::str::from_utf8(name) {
                        out.member_refs.push((s.to_string(), span));
                    }
                } else {
                    // Stringify/paste-right operand: `#TOKEN` or `Y ## TOKEN`.
                    let stringified = prev_nonspace == b'#';
                    // Paste-left operand: `TOKEN ## Y` — peek past spaces for `##`.
                    let mut j = i;
                    while j < end && matches!(src[j], b' ' | b'\t') {
                        j += 1;
                    }
                    let pasted = src.get(j) == Some(&b'#') && src.get(j + 1) == Some(&b'#');
                    if !stringified && !pasted {
                        if let Ok(s) = std::str::from_utf8(name) {
                            if known.contains(s) && !params.iter().any(|p| p == s) {
                                out.name_refs.push((s.to_string(), span));
                            }
                        }
                    }
                }
                prev_nonspace = *name.last().unwrap_or(&0);
            }
            _ => {
                if !matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                    prev_nonspace = b;
                }
                bump(b, &mut row, &mut col);
                i += 1;
            }
        }
    }
}

fn classify_expr_node(node: tree_sitter::Node) -> Option<crate::model::file_analysis::InferredType> {
    use crate::model::file_analysis::InferredType;
    match node.kind() {
        "number_literal" | "char_literal" | "true" | "false" | "sizeof_expression" => {
            Some(InferredType::Numeric)
        }
        "string_literal" | "concatenated_string" | "raw_string_literal" => {
            Some(InferredType::String)
        }
        // Every C binary operator (arithmetic / comparison / bitwise / shift /
        // logical) produces a numeric value — the operand types don't change
        // that, so the result is param-independent.
        "binary_expression" => Some(InferredType::Numeric),
        "parenthesized_expression" | "unary_expression" => {
            node.named_child(0).and_then(classify_expr_node)
        }
        // A ternary is param-independent only if both arms agree.
        "conditional_expression" => {
            let a = node.child_by_field_name("consequence").and_then(classify_expr_node);
            let b = node.child_by_field_name("alternative").and_then(classify_expr_node);
            match (a, b) {
                (Some(x), Some(y)) if x == y => Some(x),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The COMPLETE variant set per macro name — every `#define`, not the
/// collection-order winner `collect_macros` keeps. This is the config-variant
/// model input: a macro `#define`d three times under three different `#if`s
/// yields three variants, each with its guard trail + def site.
pub fn collect_macro_variants(
    tree: &Tree,
    src: &[u8],
) -> BTreeMap<String, Vec<Macro>> {
    let mut out: BTreeMap<String, Vec<Macro>> = BTreeMap::new();
    walk_macro_defs(tree, src, |n, m, _span| {
        out.entry(n).or_default().push(m);
    });
    out
}

/// The config guard trail for a `#define` at `node` (a name identifier inside
/// the preproc_def): the enclosing `#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`
/// conditions, OUTERMOST first. An else/elif branch negates the condition it
/// falls under; chained elifs accumulate the negations of preceding arms
/// because each `#elif`/`#else` is the `alternative` child of the arm before
/// it, and ascending through an `alternative` edge negates that arm's own
/// condition.
pub(super) fn guard_trail(node: tree_sitter::Node, src: &[u8]) -> Vec<CondTerm> {
    let mut terms: Vec<CondTerm> = Vec::new();
    let mut prev = node;
    let mut cur = node.parent();
    while let Some(p) = cur {
        let is_alt = p
            .child_by_field_name("alternative")
            .map(|a| a.id())
            == Some(prev.id());
        match p.kind() {
            "preproc_if" | "preproc_elif" => {
                let cond = p
                    .child_by_field_name("condition")
                    .and_then(|c| c.utf8_text(src).ok())
                    .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
                    .unwrap_or_else(|| "1".to_string());
                terms.push(CondTerm { test: CondTest::Expr(cond), negated: is_alt });
            }
            "preproc_ifdef" => {
                // Node kind is shared by #ifdef and #ifndef; the leading
                // directive text disambiguates.
                let name = p
                    .child_by_field_name("name")
                    .and_then(|c| c.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();
                let ndef = src
                    .get(p.start_byte()..)
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .map(|s| s.trim_start().starts_with("#ifndef"))
                    .unwrap_or(false);
                // The header-guard idiom (`#ifndef X` / `#define X` as the
                // FIRST thing inside it) makes X true for the rest of the
                // file from here on — it's not a real config knob, so a
                // descendant nested in the primary branch must not inherit
                // "!defined(X)" as a guard term (every macro
                // in a guarded header would pick up its file's own include
                // guard as a bogus UNKNOWN reachability label).
                if !(ndef && !is_alt && is_self_defining_guard(p, &name, src)) {
                    terms.push(CondTerm { test: CondTest::Defined(name), negated: ndef != is_alt });
                }
            }
            // #else contributes no condition of its own — the negation of the
            // arm it belongs to is applied when we ascend into the parent
            // conditional and see this else as its `alternative` child.
            _ => {}
        }
        prev = p;
        cur = p.parent();
    }
    terms.reverse();
    terms
}

/// True when `p` (a `#ifndef NAME` / `#ifdef NAME` `preproc_ifdef`) directly
/// `#define`s `NAME` as one of its own children — the canonical include-guard
/// idiom (`#ifndef X` / `#define X` / ... / `#endif`). Structural, not a name
/// list: any macro whose enclosing conditional it also defines is self-
/// guarding, regardless of the guard's own spelling.
/// Names `#define`d as their file's own include guard: a BODYLESS object-like
/// `#define X` sitting directly inside `#ifndef X` (the self-guarding idiom
/// `#ifndef X` / `#define X` / … / `#endif`). Such a macro is pure compilation
/// plumbing — no program meaning — so symbol-listing views (outline /
/// workspace-symbol) fold it away while goto-def / references still resolve it
/// (rule #7: the token keeps its ref). Structural, not a name list. The
/// bodyless requirement is the discriminator against a real conditional
/// definition (`#ifndef MIN` / `#define MIN(a,b) …`, or a valued default), which
/// the outline should keep.
pub fn collect_include_guard_names(
    parser: &mut tree_sitter::Parser,
    source: &str,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Some(tree) = parser.parse(source, None) else { return out };
    let src = source.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "preproc_ifdef" {
            let is_ifndef = src
                .get(n.start_byte()..)
                .and_then(|s| std::str::from_utf8(s).ok())
                .map(|s| s.trim_start().starts_with("#ifndef"))
                .unwrap_or(false);
            if is_ifndef {
                if let Some(name) =
                    n.child_by_field_name("name").and_then(|c| c.utf8_text(src).ok())
                {
                    let mut c = n.walk();
                    let is_guard = n.named_children(&mut c).any(|child| {
                        child.kind() == "preproc_def"
                            && child
                                .child_by_field_name("name")
                                .and_then(|x| x.utf8_text(src).ok())
                                == Some(name)
                            && child.child_by_field_name("value").is_none()
                    });
                    if is_guard {
                        out.insert(name.to_string());
                    }
                }
            }
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    out
}

fn is_self_defining_guard(p: tree_sitter::Node, name: &str, src: &[u8]) -> bool {
    let mut c = p.walk();
    let hit = p.named_children(&mut c).any(|child| {
        child.kind() == "preproc_def"
            && child.child_by_field_name("name").and_then(|n| n.utf8_text(src).ok()) == Some(name)
    });
    hit
}

/// Largest a macro body may grow to during pre-expansion — a backstop
/// against pathological chains (the self-reference case is already cut by
/// the blue-paint guard; this bounds non-self fan-out too).
pub(super) const MAX_BODY_LEN: usize = 64 * 1024;

/// Strip line continuations and collapse the multi-line macro body to
/// single-line text suitable for in-place splicing. Callers pass the RAW
/// logical-line bytes (`raw_macro_body`), NOT the CST `preproc_arg` text:
/// tree-sitter-cpp ends `preproc_arg` at the first trailing block comment on a
/// continued line, dropping every field after it (perl5 `_SV_HEAD` kept only
/// `sv_any`). We do the real C translation phases here — splice `\`-newline
/// (phase 2), then remove comments (phase 3) — so a `/* … */` between fields
/// no longer truncates the body.
fn clean_body(raw: &str) -> String {
    let spliced = raw.replace("\\\r\n", " ").replace("\\\n", " ").replace('\\', " ");
    strip_c_comments(&spliced)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The byte at which a macro body's logical line ends: scan from `body_start`
/// over physical lines, following each that ends (ignoring trailing whitespace)
/// in `\` — C phase-2 line splicing, which runs BEFORE comment removal, so a
/// trailing block comment never terminates the splice. Returns the offset of
/// the final newline (or EOF). The CST cannot supply this: tree-sitter stops
/// the whole `preproc_*` def at the first comment-bearing continued line.
fn logical_body_end(src: &[u8], body_start: usize) -> usize {
    let n = src.len();
    let mut i = body_start;
    loop {
        let line_start = i;
        while i < n && src[i] != b'\n' {
            i += 1;
        }
        let mut j = i;
        while j > line_start && matches!(src[j - 1], b' ' | b'\t' | b'\r') {
            j -= 1;
        }
        let continues = j > line_start && src[j - 1] == b'\\';
        if i >= n || !continues {
            return i;
        }
        i += 1;
    }
}

/// Replace comments inside `\`-continued preprocessor directives with spaces
/// (length-preserving; newlines kept). tree-sitter-cpp ends `preproc_arg` at the
/// first block comment on a continued line and reparses the rest of the macro
/// body as top-level code, which corrupts any declaration adjacent to the def.
/// Neutralizing the comments lets the whole directive parse as one def while
/// every byte offset is preserved, so downstream spans stay in original coords.
pub(super) fn neutralize_directive_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < n {
        let line_start = i;
        let end = logical_body_end(bytes, line_start);
        let mut k = line_start;
        while k < end && matches!(bytes[k], b' ' | b'\t') {
            k += 1;
        }
        // Only continued directives truncate; a single-line one parses fine.
        let multiline = bytes[line_start..end].contains(&b'\n');
        if k < end && bytes[k] == b'#' && multiline {
            blank_comments_in_range(&mut out, line_start, end);
        }
        i = if end < n { end + 1 } else { end };
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// Overwrite C comment bytes in `out[start..end)` with spaces (newlines kept),
/// respecting string/char literals. In-place and length-preserving.
fn blank_comments_in_range(out: &mut [u8], start: usize, end: usize) {
    let end = end.min(out.len());
    let mut i = start;
    while i < end {
        let two = (out[i], if i + 1 < end { out[i + 1] } else { 0 });
        match two {
            (b'/', b'*') => {
                let cs = i;
                i += 2;
                while i < end && !(out[i] == b'*' && i + 1 < end && out[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(end);
                for b in &mut out[cs..i] {
                    if *b != b'\n' {
                        *b = b' ';
                    }
                }
            }
            (b'/', b'/') => {
                let cs = i;
                while i < end && out[i] != b'\n' {
                    i += 1;
                }
                for b in &mut out[cs..i] {
                    *b = b' ';
                }
            }
            (q @ (b'"' | b'\''), _) => {
                i += 1;
                while i < end {
                    let c = out[i];
                    i += 1;
                    if c == b'\\' {
                        i += 1;
                    } else if c == q {
                        break;
                    }
                }
            }
            _ => i += 1,
        }
    }
}

/// The byte just past the `)` that closes the `(` at `open` (balanced over
/// nesting). `None` if unbalanced. Used to span a function-like member-block
/// paste (`_SV_HEAD(void*)`) through its argument list so the whole call blanks.
pub(super) fn balanced_paren_end(src: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < src.len() {
        match src[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The raw macro body verbatim from source, from `body_start` to the end of its
/// logical line. Bytes are unmodified (comments, `\`, tabs intact) so member
/// positioning maps 1:1 back to original coordinates; the struct-parse consumer
/// handles comments natively.
pub(super) fn raw_macro_body(source: &str, body_start: usize) -> &str {
    let end = logical_body_end(source.as_bytes(), body_start);
    source.get(body_start..end).unwrap_or("")
}

/// Replace C block (`/* … */`) and line (`//`) comments with a space, leaving
/// string/char-literal contents untouched. Operates on already-spliced text;
/// ASCII delimiters make the byte scan UTF-8-safe (multibyte bytes are ≥ 0x80,
/// never a delimiter).
fn strip_c_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1)) {
            (b'/', Some(b'*')) => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push(b' ');
            }
            (b'/', Some(b'/')) => {
                i += 2;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                out.push(b' ');
            }
            (q @ (b'"' | b'\''), _) => {
                out.push(q);
                i += 1;
                while i < b.len() {
                    let c = b[i];
                    out.push(c);
                    i += 1;
                    if c == b'\\' && i < b.len() {
                        out.push(b[i]);
                        i += 1;
                    } else if c == q {
                        break;
                    }
                }
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Resolve macro refs WITHIN bodies to a fixpoint (depth-capped), so a
/// single source-level pass suffices for non-recursive nesting.
pub(super) fn pre_expand_bodies(macros: &BTreeMap<String, Macro>) -> BTreeMap<String, Macro> {
    let mut out = macros.clone();
    for _ in 0..8 {
        let mut changed = false;
        let snapshot = out.clone();
        for (name, m) in out.iter_mut() {
            // Blue paint: a macro never re-expands itself inside its own
            // body (C's rule) — without it `#define M M M` explodes.
            let expanded = expand_text(&m.body, &snapshot, Some(name));
            if expanded != m.body {
                m.body = expanded;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out
}

pub(super) fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

/// C/C++ reserved words (the union — this grammar parses both). A closed
/// language fact, not a macro-name list: a token spelled like one of these
/// is grammar structure wherever it appears, so the expansion pass must
/// never rewrite it even when a gathered header #defines it.
pub(super) fn is_reserved_keyword(word: &str) -> bool {
    static KW: &[&str] = &[
        "alignas", "alignof", "asm", "auto", "bool", "break", "case", "catch", "char",
        "char16_t", "char32_t", "char8_t", "class", "co_await", "co_return", "co_yield",
        "concept", "const", "const_cast", "consteval", "constexpr", "constinit", "continue",
        "decltype", "default", "delete", "do", "double", "dynamic_cast", "else", "enum",
        "explicit", "export", "extern", "false", "float", "for", "friend", "goto", "if",
        "inline", "int", "long", "mutable", "namespace", "new", "noexcept", "nullptr",
        "operator", "private", "protected", "public", "register", "reinterpret_cast",
        "requires", "restrict", "return", "short", "signed", "sizeof", "static",
        "static_assert", "static_cast", "struct", "switch", "template", "this",
        "thread_local", "throw", "true", "try", "typedef", "typeid", "typename", "union",
        "unsigned", "using", "virtual", "void", "volatile", "wchar_t", "while",
    ];
    KW.binary_search(&word).is_ok()
}

/// Expand object-like macros in a free text fragment (used for body
/// pre-expansion; no arg machinery — function-like refs in bodies are
/// left for the source pass). `exclude` is the macro being expanded (blue
/// paint: it isn't re-expanded in its own body).
fn expand_text(text: &str, macros: &BTreeMap<String, Macro>, exclude: Option<&str>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if out.len() > MAX_BODY_LEN {
            return out;
        }
        if is_ident_byte(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let word = &text[start..i];
            match macros.get(word) {
                Some(m) if m.params.is_none() && Some(word) != exclude => out.push_str(&m.body),
                _ => out.push_str(word),
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
