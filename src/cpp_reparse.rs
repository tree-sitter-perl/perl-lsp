//! SPIKE: the C++ reparse seam — macro expansion before extraction.
//!
//! The C++ instance of `docs/prompt-cpp-reparse.md`'s reparse-hook
//! flavor. The obstacle course proved the worst, most common damage is
//! a declarator-position macro: `class API_EXPORT Widget {...}` reparses
//! as a `function_definition`, so the class evaporates. The fix is not
//! clang — it is *expansion*: replace the macro with its body and
//! re-parse. The probe (`dbg_cpp_attr_probe`) showed tree-sitter-cpp
//! handles the real attribute syntax (`__attribute__((...))`,
//! `__declspec(...)`) fine — the macro was merely hiding it. So the
//! transform is generic: **expand to body, let the parser validate.**
//!
//! Two flavors fall out of one pass:
//!   - object-like declarator macros (`API_EXPORT`) — the reparse-hook:
//!     expansion fixes a corrupted parse.
//!   - function-like declaration macros (`DECLARE_DYNAMIC(cls)`) — the
//!     emit-hook outcome achieved BY expansion: the body's member
//!     declarations become real, extractable symbols. Expansion
//!     subsumes the emit-hook for C++ (the doc's bet).
//!
//! Soundness is the stratified seam: this runs strictly upstream of
//! extraction (and of any witness bag), so it never interleaves with a
//! type fixpoint. A `SpliceMap` (transformed byte → original byte, the
//! Zed-anchor idea) carries every recovered span back to user text.
//!
//! Honest scope (measured, not hidden): single source-level pass with
//! pre-expanded bodies. Macros whose expansion itself contains further
//! macro CALLS (X-macros: `COLOR_LIST(X)` → `X(RED) X(GREEN)`) need
//! iterative source passes — out of scope here; that nested tail is
//! exactly the "amortize full cpp to once" case. Deliberately not wired
//! into the build pipeline; measured by `cpp_reparse_tests.rs`.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use tree_sitter::{Query, QueryCursor, StreamingIterator, Tree};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Macro {
    /// `Some(params)` = function-like; `None` = object-like.
    pub params: Option<Vec<String>>,
    pub body: String,
    /// Enclosing `#if`/`#ifdef`/`#else` conditions at the `#define`, OUTERMOST
    /// first — the config guard trail (`docs`: `cpp_macro_model`). Empty =
    /// unconditional. Rides the expansion-side rep so a config-variant macro
    /// carries WHICH config each body belongs to. `#[serde(default)]` for cache
    /// blobs written before guards existed.
    #[serde(default)]
    pub guards: Vec<String>,
    /// 0-based line of the `#define` — the variant's def site.
    #[serde(default)]
    pub def_line: usize,
}

/// One source replacement: original `[start,end)` → `replacement`.
#[derive(Debug, Clone)]
struct Splice {
    start: usize,
    end: usize,
    replacement: String,
    /// The macro NAME this splice expands — the salvage's grouping key
    /// (a broken body breaks every use, so validation is per-macro).
    name: String,
}

/// Transformed-source ↔ original-source map under arbitrary splices.
/// `to_original(t)` collapses any byte inside a replacement to the
/// splice site (per-region granularity), and otherwise subtracts the
/// net length change of all earlier splices.
///
/// Both lookups run per extracted span in `remap_spans` (O(symbols)), so
/// they must be sub-linear in the edit count. The edits partition the
/// TRANSFORMED axis into ordered, disjoint regions — replacement
/// `[ts_i, ts_i + nlen_i)` interleaved with pass-through gaps — so `ts`
/// (the transformed start of each replacement) is non-decreasing and a
/// binary search over it lands the containing region in O(log E). The
/// prefix state each region needs (`ts`, the shift accumulated *after*
/// each edit) is precomputed in `apply`; see `binary_search` for the
/// exact correspondence to the former linear scan.
#[derive(Debug, Default, Clone)]
pub struct SpliceMap {
    /// (orig_start, orig_end, replacement_len), sorted by orig_start.
    edits: Vec<(usize, usize, usize)>,
    /// `ts[i]` = transformed-axis start of edit `i`'s replacement
    /// (`orig_start + shift_before_i`). Non-decreasing — the search key.
    ts: Vec<usize>,
    /// `shift_after[i]` = cumulative `nlen - (oe - os)` through edit `i`
    /// inclusive (`trans = orig + shift`); the shift that applies in the
    /// pass-through gap *after* edit `i`.
    shift_after: Vec<isize>,
}

/// Where a transformed offset lands relative to the splice regions.
enum Region {
    /// Before every replacement, or in a pass-through gap: `orig = trans - shift`.
    PassThrough(isize),
    /// Inside edit `k`'s replacement — collapses to that macro-call site.
    Inside(usize),
}

impl SpliceMap {
    #[cfg(test)]
    pub(crate) fn edits_for_test(&self) -> &[(usize, usize, usize)] {
        &self.edits
    }

    /// The raw `(orig_start, orig_end, new_len)` edit list, ordered. The
    /// erased-use re-mint walks the BETWEEN-edit segments with it to find
    /// tokens the transform changed outside any recorded splice (the
    /// length-preserving declarator-macro strip).
    pub(crate) fn edits(&self) -> &[(usize, usize, usize)] {
        &self.edits
    }

    /// Locate `transformed`'s region. `partition_point` returns the count
    /// of edits whose replacement starts at or before `transformed`; the
    /// last of them (`pp - 1`) is the only edit that can contain it
    /// (regions are disjoint and ordered). `<=` with `pp - 1` also picks
    /// the LATER of two edits sharing a `ts` — a zero-width replacement
    /// followed by a real one — matching the linear scan's in-order
    /// processing, where the empty region never claims a byte.
    fn region(&self, transformed: usize) -> Region {
        let pp = self.ts.partition_point(|&t| t <= transformed);
        if pp == 0 {
            return Region::PassThrough(0); // before every splice: shift is 0
        }
        let k = pp - 1;
        let (_os, _oe, nlen) = self.edits[k];
        if transformed < self.ts[k] + nlen {
            Region::Inside(k)
        } else {
            Region::PassThrough(self.shift_after[k])
        }
    }

    pub fn to_original(&self, transformed: usize) -> usize {
        match self.region(transformed) {
            Region::PassThrough(shift) => (transformed as isize - shift) as usize,
            Region::Inside(k) => self.edits[k].0, // collapse to the call site
        }
    }

    /// Every expansion's ORIGINAL byte extent, in order. Each edit IS a
    /// macro use the transform erased from the parsed text — the driver
    /// re-mints a reference at each site so an expanded use still answers
    /// find-references (rule #7: every meaningful token gets a ref; rule #9:
    /// derived facts trace to source).
    pub fn expansion_sites(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.edits.iter().map(|&(os, oe, _)| (os, oe))
    }

    /// If `transformed` falls INSIDE a replacement (a macro expansion),
    /// return the replacement's ORIGINAL extent `(orig_start, orig_end)` —
    /// the macro-call site. A symbol/ref that came out of an expansion
    /// (`newThing(5)` → `Perl_newThing(aTHX_ 5)`) collapses to a zero-width
    /// point under `to_original`; callers use this to give it the call
    /// site's span instead, so goto-def/hover land on the macro call.
    pub fn replacement_at(&self, transformed: usize) -> Option<(usize, usize)> {
        match self.region(transformed) {
            Region::Inside(k) => Some((self.edits[k].0, self.edits[k].1)),
            Region::PassThrough(_) => None,
        }
    }
}

const MACRO_DEF_QUERY: &str = r#"
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
const EXCLUDE_QUERY: &str = r#"
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
const EXCLUDE_QUERY_WIDE: &str = r#"
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

const INCLUDE_QUERY: &str = r#"
(preproc_include path: (string_literal (string_content) @p))
(preproc_include path: (system_lib_string) @s)
"#;

/// A function-like macro use that already parses as a call — the "leave" set
/// for the expansion flip (`clean_call_sites`).
const CALL_QUERY: &str = r#"
(call_expression function: (identifier) @f)
"#;

/// Compile-once cache for this pipeline's queries. Every tree here comes
/// from the one `tree_sitter_cpp` grammar (the C/C++ driver), so a single
/// `Query` per source is reused across every reparse instead of rebuilding
/// the automaton per keystroke. `Query` is `Send + Sync`, so a static slot
/// is safe.
static MACRO_DEF_Q: OnceLock<Query> = OnceLock::new();
static EXCLUDE_Q: OnceLock<Query> = OnceLock::new();
static EXCLUDE_Q_WIDE: OnceLock<Query> = OnceLock::new();
static INCLUDE_Q: OnceLock<Query> = OnceLock::new();
static CALL_Q: OnceLock<Query> = OnceLock::new();

fn cached_query(slot: &'static OnceLock<Query>, lang: &tree_sitter::Language, src: &str) -> &'static Query {
    slot.get_or_init(|| Query::new(lang, src).expect("cpp_reparse query"))
}

/// Walk `collect_macros`' query, calling `emit(name, Macro)` per `#define`
/// (object- and function-like). The Macro carries its config guard trail —
/// the enclosing `#if`/`#ifdef`/`#else` conditions — captured from the CST
/// ancestors of the def. Both the dedup'd table (expansion side) and the
/// variant-preserving collection route through here so the guard trail is
/// captured once.
fn walk_macro_defs(
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
            emit(n, Macro { params: None, body: b, guards: guards.clone(), def_line }, name_span);
        }
        // Bodyless `#define FLAG` — the canonical config knob (feature
        // toggles, include guards, `PERL_CORE`-style markers). It must enter
        // the definition universe or reachability ranks `#ifdef FLAG` arms
        // exactly inverted; its empty body is also C-correct for expansion
        // (a bare use of the flag expands to nothing).
        if let Some(n) = bname {
            emit(
                n,
                Macro { params: None, body: String::new(), guards: guards.clone(), def_line },
                name_span,
            );
        }
        if let (Some(n), Some(p), Some(b)) = (fname, fparams, fbody) {
            emit(n, Macro { params: Some(p), body: b, guards, def_line }, name_span);
        }
    }
}

/// A cheap structural signature over a file's first ~1KB, for routing a file
/// whose extension no driver claims (`commands.def`, a 12.7k
/// line C dispatch table with an unowned extension, went entirely dark under
/// the Perl fallback). NOT an extension list — `.def` is ambiguous across
/// ecosystems (a Windows module-definition file is `LIBRARY`/`EXPORTS`
/// stanzas, not C) so the extension alone can't decide; this reads content.
/// Scores C-preprocessor directives and brace/semicolon statement shape
/// against Perl's sigils/keywords, over full lines only (a truncated last
/// line contributes nothing either way).
pub fn looks_like_c_family(prefix: &str) -> bool {
    let mut c_score = 0i32;
    let mut perl_score = 0i32;
    for raw in prefix.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#')
            && (line[1..].trim_start().starts_with("include")
                || line[1..].trim_start().starts_with("define")
                || line[1..].trim_start().starts_with("ifndef")
                || line[1..].trim_start().starts_with("ifdef")
                || line[1..].trim_start().starts_with("if ")
                || line[1..].trim_start().starts_with("endif")
                || line[1..].trim_start().starts_with("pragma"))
        {
            c_score += 3;
        } else if line.starts_with("package ")
            || line.starts_with("use strict")
            || line.starts_with("use warnings")
            || line.starts_with("sub ")
            || line.starts_with('$')
            || line.starts_with('@')
            || line.starts_with('%')
        {
            perl_score += 3;
        } else if line.ends_with(';') || line.ends_with('{') || line == "}" || line.ends_with("};")
        {
            c_score += 1;
        }
    }
    c_score > 0 && c_score > perl_score
}

pub fn collect_macros(tree: &Tree, src: &[u8]) -> BTreeMap<String, Macro> {
    let mut out = BTreeMap::new();
    walk_macro_defs(tree, src, |n, m, _span| {
        out.insert(n, m);
    });
    out
}

/// The macro identity/navigation lane: every `#define` as a `MacroDef` carrying
/// its guard trail, def-site span, and — for a direct-delegation wrapper —
/// the callee it forwards to. Consumed by goto-def (`#define`-preference,
/// reachability-ranked multi-location, see-through). Parses `source` fresh so
/// def spans are in ORIGINAL coordinates (the expansion tree splices usages).
pub fn collect_macro_defs(
    parser: &mut tree_sitter::Parser,
    source: &str,
) -> Vec<crate::file_analysis::MacroDef> {
    use crate::file_analysis::{MacroDef, Span};
    let Some(tree) = parser.parse(source, None) else { return Vec::new() };
    let src = source.as_bytes();
    let mut out = Vec::new();
    walk_macro_defs(&tree, src, |name, m, (start, end)| {
        // Function-like: a whole-body single call `G(args)`. Object-like: a
        // bare-identifier ALIAS (`#define op_prune_chain_head
        // Perl_op_prune_chain_head`, perl5's non-threaded embed.h shape) —
        // the same forwarding edge, spelled without params.
        let delegate = match m.params {
            Some(_) => delegation_target(&m.body),
            None => bare_identifier(&m.body),
        };
        out.push(MacroDef {
            name,
            params: m.params,
            body: m.body,
            guards: m.guards,
            selection_span: Span { start, end },
            delegate,
        });
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
) -> Option<crate::file_analysis::InferredType> {
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
) -> Vec<(crate::file_analysis::Span, Vec<crate::file_analysis::Span>)> {
    use crate::file_analysis::Span;
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
    pub name_refs: Vec<(String, crate::file_analysis::Span)>,
    /// `(field, span)` per member-access token (`->op_next` / `.op_next`)
    /// inside a body. Untyped here — the receiver is a macro parameter with no
    /// type — so it is left as a bare `(field, span)` candidate; the assembly
    /// pass (`into_file_analysis`) resolves it against the file's own field
    /// symbols and mints a class-frozen `MethodCall` ref so references on the
    /// field include the in-body use (perl5 `->op_next` drills are heavy in
    /// bodies like `OP_NAME`/`cUNOPx`; rule #7).
    pub member_refs: Vec<(String, crate::file_analysis::Span)>,
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
    use crate::file_analysis::Span;
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

fn classify_expr_node(node: tree_sitter::Node) -> Option<crate::file_analysis::InferredType> {
    use crate::file_analysis::InferredType;
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
fn guard_trail(node: tree_sitter::Node, src: &[u8]) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
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
                terms.push(if is_alt { negate(&cond) } else { cond });
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
                if ndef && !is_alt && is_self_defining_guard(p, &name, src) {
                    // term suppressed — always-true past this point.
                } else {
                    let base = if ndef {
                        format!("!defined({name})")
                    } else {
                        format!("defined({name})")
                    };
                    terms.push(if is_alt { negate(&base) } else { base });
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

fn negate(cond: &str) -> String {
    if let Some(inner) = cond.strip_prefix("!(").and_then(|s| s.strip_suffix(')')) {
        inner.to_string()
    } else if cond.starts_with("defined(") {
        format!("!{cond}")
    } else if cond.starts_with("!defined(") {
        cond.trim_start_matches('!').to_string()
    } else {
        format!("!({cond})")
    }
}

/// Largest a macro body may grow to during pre-expansion — a backstop
/// against pathological chains (the self-reference case is already cut by
/// the blue-paint guard; this bounds non-self fan-out too).
const MAX_BODY_LEN: usize = 64 * 1024;

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
fn neutralize_directive_comments(source: &str) -> String {
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
fn balanced_paren_end(src: &[u8], open: usize) -> Option<usize> {
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
fn raw_macro_body(source: &str, body_start: usize) -> &str {
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
fn pre_expand_bodies(macros: &BTreeMap<String, Macro>) -> BTreeMap<String, Macro> {
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

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

/// C/C++ reserved words (the union — this grammar parses both). A closed
/// language fact, not a macro-name list: a token spelled like one of these
/// is grammar structure wherever it appears, so the expansion pass must
/// never rewrite it even when a gathered header #defines it.
fn is_reserved_keyword(word: &str) -> bool {
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

/// ERROR + MISSING node count — the parser's own verdict on a parse.
pub fn parse_damage(node: tree_sitter::Node) -> usize {
    let mut n = 0;
    let mut cur = node.walk();
    let mut stack = vec![node];
    while let Some(x) = stack.pop() {
        if x.is_error() || x.is_missing() {
            n += 1;
        }
        for c in x.children(&mut cur) {
            stack.push(c);
        }
    }
    n
}

/// BODIED structure-container count (class/struct/union/enum/namespace) —
/// the damage count's blind spot. tree-sitter's recovery can trade many
/// small ERRORs for one giant ERROR that swallows a whole class: the damage
/// COUNT drops while the file's structure evaporates (abseil's
/// `raw_hash_set` did exactly this under a blanking round). A repair gate
/// that only compares damage adopts that trade; pairing it with "bodied
/// containers must not decrease" rejects it.
fn structure_count(node: tree_sitter::Node) -> usize {
    let mut n = 0;
    let mut cur = node.walk();
    let mut stack = vec![node];
    while let Some(x) = stack.pop() {
        if matches!(
            x.kind(),
            "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
                | "namespace_definition"
        ) && x.child_by_field_name("body").is_some()
        {
            n += 1;
        }
        for c in x.children(&mut cur) {
            stack.push(c);
        }
    }
    n
}

/// Length-preserving blanking of an UNRESOLVED declarator-position macro.
/// `class API_EXPORT Foo {` — an export macro from a GENERATED header (Qt's
/// `Q_CORE_EXPORT`, never in the source tree) the gather can't reach —
/// parses as a corrupt function and the class evaporates. A class/struct
/// head with TWO identifiers before its body has a macro in the first slot:
/// valid C++ names the type once (the exceptions — `class Name final`,
/// brace-init declarations, range-for bindings — are excluded below).
/// Blank the macro token with spaces (same length → every extracted span
/// stays put, no SpliceMap needed) so the class parses. Returns the
/// rewritten source plus the `(class_name, macro_token)` pairs it recovered
/// — the analyze path looks the token up in the attribute-macro manifest to
/// annotate the class with what the macro signals (`exported`/`deprecated`);
/// an unknown token still recovers the class, it just carries no signal.
///
/// The parse-damage gate can't police this repair: the misparse it fixes
/// (`class API_EXPORT Foo { … }` as a bogus function_definition) contains
/// ZERO error nodes, and so does the valid C++11 it must not touch
/// (`struct Point p {1, 2};`). Instead each candidate is gated on **type-
/// position context** from a parse of the untouched source: valid C++ spells
/// `struct ID1 ID2 ⟨head⟩` only when the head token opens a *value* or *loop*
/// construct (a brace initializer, a range-for binding) — a closed grammar
/// fact, so those (plus comment/string text, which is not code at all) are
/// skipped and everything else is the misparse this repair exists for.
fn strip_declarator_macros(
    parser: &mut tree_sitter::Parser,
    src: &str,
) -> (String, Vec<(String, String)>) {
    let bytes = src.as_bytes();
    // (macro span, name span, head-token byte) candidate sites, textually.
    let mut candidates: Vec<((usize, usize), (usize, usize), usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let kwlen = if bytes[i..].starts_with(b"class") {
            5
        } else if bytes[i..].starts_with(b"struct") {
            6
        } else {
            i += 1;
            continue;
        };
        let word_boundary = (i == 0 || !is_ident_byte(bytes[i - 1]))
            && bytes.get(i + kwlen).is_some_and(|b| b.is_ascii_whitespace());
        if !word_boundary {
            i += kwlen;
            continue;
        }
        // IDENT1 (candidate macro), then IDENT2 (candidate name).
        let mut p = i + kwlen;
        let skip_ws = |p: &mut usize| while *p < bytes.len() && bytes[*p].is_ascii_whitespace() { *p += 1; };
        let read_id = |p: &mut usize| { let s = *p; while *p < bytes.len() && is_ident_byte(bytes[*p]) { *p += 1; } (s, *p) };
        skip_ws(&mut p);
        let (id1s, id1e) = read_id(&mut p);
        skip_ws(&mut p);
        let (id2s, id2e) = read_id(&mut p);
        skip_ws(&mut p);
        let head = p < bytes.len() && matches!(bytes[p], b'{' | b':' | b'<');
        if id1e > id1s && id2e > id2s && head {
            let id2 = &src[id2s..id2e];
            if id2 != "final" && id2 != "sealed" {
                candidates.push(((id1s, id1e), (id2s, id2e), p));
            }
        }
        i += kwlen;
    }
    if candidates.is_empty() {
        return (src.to_string(), Vec::new());
    }
    let tree = parser.parse(src, None);
    let valid_context = |head: usize| -> bool {
        let Some(t) = &tree else { return false };
        let Some(n) = t
            .root_node()
            .named_descendant_for_byte_range(head, head + 1)
        else {
            return false;
        };
        matches!(
            n.kind(),
            // `struct Point p {1, 2};` / `struct sockaddr_in addr {};`
            "initializer_list"
            // `for (struct Point p : v)`
            | "for_range_loop"
            // not code — blanking would mint phantom recovered pairs
            | "comment" | "string_literal" | "raw_string_literal"
            | "string_content" | "char_literal"
        )
    };
    let mut out = src.to_string();
    let mut recovered: Vec<(String, String)> = Vec::new();
    // SAFETY: only ASCII spaces are written, over ASCII identifier bytes —
    // length-preserving and UTF-8-valid.
    let ob = unsafe { out.as_bytes_mut() };
    for ((id1s, id1e), (id2s, id2e), head) in candidates {
        if valid_context(head) {
            continue;
        }
        recovered.push((src[id2s..id2e].to_string(), src[id1s..id1e].to_string()));
        for b in &mut ob[id1s..id1e] {
            *b = b' ';
        }
    }
    (out, recovered)
}

/// Expand seeded with EXTERNAL macros from `#include`d headers, then **let
/// the parser validate**: keep the transform only if it does not increase
/// parse damage. An expansion that helps (declarator macros, simple
/// declaration macros) lands; one that hurts (nested macro CALLS like
/// X-macros, `##` token-paste — the tail this single pass does not model)
/// is salvaged per-splice: the good expansions land, only the bad ones are
/// dropped (or blank-degraded), so one bad macro never discards a whole
/// file's recoveries.
pub fn preprocess_validated_with(
    parser: &mut tree_sitter::Parser,
    src: &str,
    external: &PreExpandedExternal,
) -> (String, SpliceMap, Vec<(String, String)>) {
    // Blank unresolved declarator-position macros first (length-preserving,
    // parse-context-gated — recovers `class Q_CORE_EXPORT Foo` even when the
    // macro is unreachable). Spans stay in original coordinates. `recovered`
    // carries each (class_name, macro_token) so the analyze path can annotate
    // the class with the macro's signal — surviving regardless of which return
    // arm fires below, since `src` is the stripped text throughout.
    let (stripped, recovered) = strip_declarator_macros(parser, src);
    // Blank UNRESOLVED structural macros (macro-before-`namespace`, macro
    // before a constructor) — expansion can't repair a token it has no
    // definition for; known names are left for the expansion below.
    let stripped = strip_unresolved_structural_macros(parser, &stripped, external);
    // Repair a conditional directive in DECLARATION position (a ctor-init `#if`)
    // that misparses the enclosing class — blank the directive lines so the
    // declaration parses, gated by the parser's damage/structure verdict.
    let stripped = strip_declaration_position_directives(parser, &stripped);
    let src = stripped.as_str();
    let Some(tree) = parser.parse(src, None) else {
        return (src.to_string(), SpliceMap::default(), recovered);
    };
    let before = parse_damage(tree.root_node());
    let structure = structure_count(tree.root_node());
    // First attempt: narrow exclusion — conditional-region BODIES are
    // expandable, so a macro use inside `#ifdef`/`#if`/`#else` expands
    // (perl5's `pTHX_` context-param convention). See `EXCLUDE_QUERY`.
    let (rewritten, map) = preprocess_with(&tree, src, external);
    if rewritten == src {
        return (rewritten, map, recovered);
    }
    if parser
        .parse(&rewritten, None)
        .is_some_and(|t| parse_damage(t.root_node()) <= before)
    {
        return (rewritten, map, recovered);
    }
    // The widened expansion RAISED damage. Re-exclude conditional-region bodies
    // (the pre-widening WIDE scope) and retry: a huge macro-heavy file
    // (perl.h/op.c) keeps its prior fast expansion and never pays the salvage
    // cliff for the widened scope — small clean files already validated above
    // and kept the win. `docs/adr/config-superposition-declarations.md` slice 1.
    let (rewritten, map) = preprocess_with_mode(&tree, src, external, false, false);
    if rewritten == src {
        return (rewritten, map, recovered);
    }
    match parser.parse(&rewritten, None) {
        Some(after) if parse_damage(after.root_node()) <= before => (rewritten, map, recovered),
        _ => {
            // The full rewrite raised damage — one bad expansion (an
            // unexpanded `##` call inside a namespace-open macro's body)
            // must not discard the file's GOOD expansions. Bisect the
            // splice set against the parser's own damage verdict, keeping
            // every subset that stays at-or-below the baseline; a rejected
            // splice degrades to a length-preserving blank when THAT
            // validates (leaving the raw token glues the next declaration
            // into garbage — the reason it was spliced at all).
            let full = compute_splices(&tree, src, external, false, false);
            let mut budget: u32 = SALVAGE_PARSE_BUDGET;
            let mut good =
                salvage_splices(parser, src, &full, (before, structure), &mut budget);
            if std::env::var_os("PERL_LSP_SALVAGE_DEBUG").is_some() {
                eprintln!(
                    "salvage-debug: before_damage={} splices={} kept={} (blanked={}) budget_left={}",
                    before,
                    full.len(),
                    good.len(),
                    good.iter().filter(|s| s.replacement.bytes().all(|b| b == b' ')).count(),
                    budget
                );
                let mut names: Vec<&str> = full
                    .iter()
                    .map(|s| s.name.as_str())
                    .filter(|n| !good.iter().any(|g| g.name == *n))
                    .collect();
                names.sort_unstable();
                names.dedup();
                eprintln!("salvage-debug: dropped-names={names:?}");
                let mut blanked: Vec<&str> = good
                    .iter()
                    .filter(|s| s.replacement.bytes().all(|b| b == b' '))
                    .map(|s| s.name.as_str())
                    .collect();
                blanked.sort_unstable();
                blanked.dedup();
                eprintln!("salvage-debug: blanked-names={blanked:?}");
                if let Ok(p) = std::env::var("PERL_LSP_SALVAGE_DUMP") {
                    let mut g = good.clone();
                    let (rw, _) = apply(src, &mut g);
                    let _ = std::fs::write(p, rw);
                }
            }
            if !good.is_empty() {
                let (rw, map) = apply(src, &mut good);
                if let Some(t) = parser.parse(&rw, None) {
                    if parse_damage(t.root_node()) <= before
                        && structure_count(t.root_node()) >= structure
                    {
                        return (rw, map, recovered);
                    }
                }
            }
            // Nothing salvageable splice-wise. Keep only the provably-safe
            // IDENTIFIER-ALIAS expansions (`op_prune_chain_head →
            // Perl_op_prune_chain_head`) so macro-name indirection —
            // goto-def + references THROUGH the alias — survives even when
            // the rest is discarded.
            let (alias_rw, alias_map) = preprocess_with_mode(&tree, src, external, true, false);
            match (alias_rw != src).then(|| parser.parse(&alias_rw, None)).flatten() {
                Some(a) if parse_damage(a.root_node()) <= before => (alias_rw, alias_map, recovered),
                _ => (src.to_string(), SpliceMap::default(), recovered),
            }
        }
    }
}

/// Reparse budget for the per-splice salvage: each `validates` probe is one
/// full parse of the file, so the bisection is bounded. Exhaustion degrades
/// to dropping the unprocessed subset — never to keeping an unvalidated one.
const SALVAGE_PARSE_BUDGET: u32 = 48;

/// Bisect `splices` down to a subset whose application keeps parse damage
/// at or below `base`. Returns a VALIDATED subset or an empty vec — never an
/// unvalidated one (the damage-never-rises invariant holds by construction).
///
/// Bisection runs over per-MACRO-NAME groups, not individual splices: a
/// broken body (`##` token paste the single pass doesn't model) breaks
/// EVERY use of that macro, so the group is the natural validation unit —
/// and it keeps the reparse count O(names), not O(uses) (json.hpp: ~500
/// splices, a few dozen names). A rejected group is retried as
/// length-preserving BLANKS of its use tokens: for a statement-position
/// macro (the namespace-open idiom) the blank recovers the region even when
/// the expansion is broken; a blank that breaks an expression fails its own
/// validation and is dropped — the parser's verdict decides, never the
/// macro's shape. Paired open/close macros couple through the whole-file
/// validation: an END whose `}}` lands without its BEGIN raises damage,
/// fails, and degrades to blanks alongside it.
fn salvage_splices(
    parser: &mut tree_sitter::Parser,
    src: &str,
    splices: &[Splice],
    base: (usize, usize),
    budget: &mut u32,
) -> Vec<Splice> {
    // Context-free-safe splices (empty-body byte-deletions — see
    // `is_context_free_safe`) are KEPT without a probe: their expansion can't
    // raise damage in any position, so the budget must not be spent bisecting
    // them (`docs/prompt-macro-salvage-scaling.md`, fix #1 — `pTHX_`/`aTHX_` used
    // across the whole file no longer cost anything). They double as the
    // always-applied BASELINE the ambiguous bisection validates against: since a
    // deletion only lowers damage, keeping them out can never raise a surviving
    // subset's damage, so the remaining groups keep at least as much as before.
    let (safe, ambiguous): (Vec<Splice>, Vec<Splice>) = splices
        .iter()
        .cloned()
        .partition(|s| s.replacement.chars().all(char::is_whitespace));
    let mut by_name: BTreeMap<&str, Vec<Splice>> = BTreeMap::new();
    for s in &ambiguous {
        by_name.entry(&s.name).or_default().push(s.clone());
    }
    let groups: Vec<Vec<Splice>> = by_name.into_values().collect();
    let mut kept = salvage_groups(parser, src, &groups, &safe, base, budget);
    kept.extend(safe);
    kept.sort_by_key(|s| s.start);
    kept
}

fn salvage_validates(
    parser: &mut tree_sitter::Parser,
    src: &str,
    keep_always: &[Splice],
    set: &[Splice],
    base: (usize, usize),
    budget: &mut u32,
) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    // `keep_always` (the context-free-safe deletions) is applied on every probe
    // so the ambiguous groups are judged in the same context they'll ship in.
    let mut v: Vec<Splice> = keep_always.iter().chain(set).cloned().collect();
    let (rw, _) = apply(src, &mut v);
    parser.parse(&rw, None).is_some_and(|t| {
        parse_damage(t.root_node()) <= base.0 && structure_count(t.root_node()) >= base.1
    })
}

fn salvage_groups(
    parser: &mut tree_sitter::Parser,
    src: &str,
    groups: &[Vec<Splice>],
    keep_always: &[Splice],
    base: (usize, usize),
    budget: &mut u32,
) -> Vec<Splice> {
    if groups.is_empty() {
        return Vec::new();
    }
    let all: Vec<Splice> = groups.iter().flatten().cloned().collect();
    if salvage_validates(parser, src, keep_always, &all, base, budget) {
        return all;
    }
    if groups.len() == 1 {
        // The group's expansions hurt — degrade to blanking its use tokens.
        let blanks: Vec<Splice> = all
            .iter()
            .map(|s| Splice {
                start: s.start,
                end: s.end,
                replacement: " ".repeat(s.end - s.start),
                name: s.name.clone(),
            })
            .collect();
        if salvage_validates(parser, src, keep_always, &blanks, base, budget) {
            return blanks;
        }
        return Vec::new();
    }
    let (l, r) = groups.split_at(groups.len() / 2);
    let lk = salvage_groups(parser, src, l, keep_always, base, budget);
    let rk = salvage_groups(parser, src, r, keep_always, base, budget);
    if lk.is_empty() {
        return rk;
    }
    if rk.is_empty() {
        return lk;
    }
    let mut keep = lk.clone();
    keep.extend(rk.iter().cloned());
    if salvage_validates(parser, src, keep_always, &keep, base, budget) {
        return keep;
    }
    // The halves validated separately but interact when combined — keep the
    // larger half, which validated on its own.
    if lk.len() >= rk.len() {
        lk
    } else {
        rk
    }
}

/// Blank (length-preserving) UNRESOLVED macro tokens in the two structural
/// positions expansion cannot repair because no definition exists:
///
///   * **before `namespace`** — `NS_BEGIN\nnamespace d {…}`: the macro token
///     absorbs the keyword (`function_definition` with an `identifier`
///     declarator spelled "namespace") and the whole block's symbols orphan.
///     The grammar's own verdict is the gate: a `namespace` KEYWORD can never
///     parse as an `identifier` node in valid C++ (`using namespace` parses
///     as a using_declaration), so an identifier node spelled "namespace"
///     proves the token before it is a macro.
///   * **before a constructor** — `ATTR_NOINLINE Widget(Widget&& w)…` inside
///     `class Widget`: a member function whose name equals its class can
///     never carry a return type, so the token in the type slot is a macro.
///     (With a ctor-initializer the misparse cascades — the init list becomes
///     a `bitfield_clause` and the rest of the class reparents wrong.)
///
/// KNOWN names (file-local or gathered `#define`s) are skipped — expansion
/// owns those, and blanking a namespace-OPEN macro whose END expands to `}}`
/// would break brace balance. Iterates to a small fixpoint (blanking one
/// macro can expose the next misconsumed `namespace`); each round's blanking
/// must not raise parse damage or it is reverted.
fn strip_unresolved_structural_macros(
    parser: &mut tree_sitter::Parser,
    src: &str,
    external: &PreExpandedExternal,
) -> String {
    let mut cur = src.to_string();
    for _ in 0..4 {
        let Some(tree) = parser.parse(&cur, None) else { return cur };
        let damage = parse_damage(tree.root_node());
        let structure = structure_count(tree.root_node());
        let local = collect_macros(&tree, cur.as_bytes());
        let known = |name: &str| local.contains_key(name) || external.raw.contains_key(name);
        let bytes = cur.as_bytes();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut stack = vec![tree.root_node()];
        let mut walk = tree.root_node().walk();
        while let Some(n) = stack.pop() {
            // An ERROR whose entire content is one bare identifier sitting
            // right AFTER a function_declarator — the post-declarator
            // attribute-macro position (`T m(...) ATTR { ... }`); no valid
            // C++ token can stand there, the parser's own verdict. The
            // sibling gate keeps this away from other single-identifier
            // ERRORs (a namespace/class NAME stranded inside a macro-glued
            // misparse must never be blanked).
            if n.is_error()
                && n.named_child_count() == 1
                && n.prev_named_sibling().is_some_and(|p| p.kind() == "function_declarator")
            {
                if let Some(c) = n.named_child(0) {
                    let txt = c.utf8_text(bytes).unwrap_or("");
                    if c.kind() == "identifier"
                        && n.utf8_text(bytes).map(str::trim) == Ok(txt)
                        && !is_reserved_keyword(txt)
                        && !known(txt)
                    {
                        ranges.push((c.start_byte(), c.end_byte()));
                    }
                }
            }
            match n.kind() {
                "identifier" if n.utf8_text(bytes) == Ok("namespace") => {
                    // The token before the misconsumed keyword: skip
                    // whitespace backward, read the identifier.
                    let mut e = n.start_byte();
                    while e > 0 && bytes[e - 1].is_ascii_whitespace() {
                        e -= 1;
                    }
                    let mut s = e;
                    while s > 0 && is_ident_byte(bytes[s - 1]) {
                        s -= 1;
                    }
                    if s < e && !known(&cur[s..e]) {
                        ranges.push((s, e));
                    }
                }
                "field_declaration" => {
                    let mac = n
                        .child_by_field_name("type")
                        .filter(|t| t.kind() == "type_identifier");
                    let leaf = n
                        .child_by_field_name("declarator")
                        .filter(|d| d.kind() == "function_declarator")
                        .and_then(|d| descend_declarator_name(d, bytes));
                    if let (Some(t), Some(leaf)) = (mac, leaf) {
                        let class = enclosing_aggregate_name(
                            tree.root_node(),
                            &cur,
                            n.start_byte(),
                        );
                        let tt = t.utf8_text(bytes).unwrap_or("");
                        if class.as_deref() == leaf.utf8_text(bytes).ok()
                            && class.as_deref() != Some(tt)
                            && !known(tt)
                        {
                            ranges.push((t.start_byte(), t.end_byte()));
                        }
                    }
                }
                _ => {}
            }
            for c in n.children(&mut walk) {
                stack.push(c);
            }
        }
        if ranges.is_empty() {
            return cur;
        }
        // Per-candidate adopt/revert: each blank must individually keep
        // damage from rising AND keep every bodied container (a blank that
        // trades three small ERRORs for one class-swallowing ERROR lowers
        // the damage COUNT while erasing the structure — reject it).
        let mut adopted = false;
        for (s, e) in ranges {
            let tentative = blank_ranges(&cur, std::iter::once((s, e)));
            let Some(t) = parser.parse(&tentative, None) else { continue };
            if parse_damage(t.root_node()) <= damage
                && structure_count(t.root_node()) >= structure
            {
                if std::env::var_os("PERL_LSP_SALVAGE_DEBUG").is_some() {
                    eprintln!("strip-debug: blanking {:?}", &cur[s..e]);
                }
                cur = tentative;
                adopted = true;
            }
        }
        if !adopted {
            return cur;
        }
    }
    cur
}

/// True when `line` opens with a conditional preprocessor directive
/// (`#if`/`#ifdef`/`#ifndef`/`#elif`/`#else`/`#endif` and the C23 `#elifdef`
/// spellings). Leading whitespace already stripped by the caller.
fn is_conditional_directive(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else { return false };
    let kw: String = rest.trim_start().chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    matches!(
        kw.as_str(),
        "if" | "ifdef" | "ifndef" | "elif" | "elifdef" | "elifndef" | "else" | "endif"
    )
}

/// The `(line_start, newline_exclusive_end)` range of every conditional
/// directive line whose START falls inside `[span_start, span_end)`. Ranges
/// stop before the `\n` so blanking them is newline-preserving (the arm bodies
/// keep their line structure).
fn conditional_directive_line_ranges(
    bytes: &[u8],
    span_start: usize,
    span_end: usize,
) -> Vec<(usize, usize)> {
    let n = bytes.len();
    let mut i = span_start.min(n);
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1; // rewind to the start of span_start's physical line
    }
    let mut ranges = Vec::new();
    let end = span_end.min(n);
    while i < end {
        let ls = i;
        let mut le = i;
        while le < n && bytes[le] != b'\n' {
            le += 1;
        }
        let line = std::str::from_utf8(&bytes[ls..le]).unwrap_or("");
        if is_conditional_directive(line.trim_start()) {
            ranges.push((ls, le));
        }
        i = le + 1;
    }
    ranges
}

/// Repair a conditional preprocessor directive sitting in DECLARATION position
/// — inside a class / struct / union body — that misparses. The ctor-
/// initializer case (`Widget(...) \n #if X : a(), b() #endif { ... }`,
/// nlohmann json.hpp `JSON_DIAGNOSTIC_POSITIONS`): tree-sitter recovers the
/// `#if`-guarded init list as ERROR-wrapped bogus field declarations, minting
/// PHANTOM members (`a`, `b`) and corrupting hover on the real ones. Blanking
/// only the `#if`/`#elif`/`#else`/`#endif` LINES (arm bodies kept, newlines
/// preserved) lets the declaration parse. Config-variant navigation is
/// untouched — `collect_macro_defs` reparses the ORIGINAL source, not this
/// transform.
///
/// Gated exactly like the sibling structural strips: a candidate region is
/// adopted only when blanking it does NOT raise parse damage AND keeps the
/// bodied-structure floor (`structure_count`), so a true `#if`/`#else` twin
/// whose arms don't concatenate cleanly is left alone (its blank raises damage
/// or drops a container → reverted). Candidates are narrowed to preproc regions
/// that (a) misparse and (b) sit under a `field_declaration_list`, so healthy
/// conditionals and file-scope config regions are never touched.
/// `docs/adr/config-superposition-declarations.md` slice 1 (declaration-
/// position repair).
fn strip_declaration_position_directives(parser: &mut tree_sitter::Parser, src: &str) -> String {
    let mut cur = src.to_string();
    for _ in 0..4 {
        let Some(tree) = parser.parse(&cur, None) else { return cur };
        let damage = parse_damage(tree.root_node());
        if damage == 0 {
            return cur;
        }
        let structure = structure_count(tree.root_node());
        let bytes = cur.as_bytes();
        // Candidate directive-line sets: one per misparsing preproc region in
        // declaration position.
        let mut regions: Vec<Vec<(usize, usize)>> = Vec::new();
        let mut walk = tree.root_node().walk();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            for c in n.children(&mut walk) {
                stack.push(c);
            }
            if matches!(n.kind(), "preproc_if" | "preproc_ifdef")
                && parse_damage(n) > 0
                && node_has_field_list_ancestor(n)
            {
                let lines = conditional_directive_line_ranges(bytes, n.start_byte(), n.end_byte());
                if !lines.is_empty() {
                    regions.push(lines);
                }
            }
        }
        if regions.is_empty() {
            return cur;
        }
        // Per-region adopt/revert against the parser's own verdict, so one bad
        // region never discards another's repair.
        let mut adopted = false;
        for lines in regions {
            let tentative = blank_ranges(&cur, lines.into_iter());
            let Some(t) = parser.parse(&tentative, None) else { continue };
            if parse_damage(t.root_node()) <= damage && structure_count(t.root_node()) >= structure {
                cur = tentative;
                adopted = true;
            }
        }
        if !adopted {
            return cur;
        }
    }
    cur
}

/// Whether `n` has a `field_declaration_list` (class/struct/union body)
/// ancestor — the "declaration position" gate for the directive repair.
fn node_has_field_list_ancestor(n: tree_sitter::Node) -> bool {
    let mut p = n.parent();
    while let Some(node) = p {
        if node.kind() == "field_declaration_list" {
            return true;
        }
        p = node.parent();
    }
    false
}

/// Gather macros from a C++ file's transitively `#include`d headers, so a
/// macro `#define`d in another header (the `SPDLOG_NAMESPACE_BEGIN` idiom)
/// can be expanded in this file. Quoted includes resolve relative to the
/// file's dir, walking ancestor dirs as include roots (the classic search
/// path, discovered not configured). Bounded: depth + visited + header
/// caps; best-effort — unresolvable includes are skipped. The file's OWN
/// macros are NOT included here (the caller collects those).
/// Cached transitive-macro table, keyed by (file, its #include set). The
/// gather walks the whole include closure (perl.h reaches ~2000 macros over
/// hundreds of headers — seconds cold), so re-running it per completion
/// keystroke is untenable. The analyze pass warms this on open; completion
/// reuses it for free. Invalidates when the file's `#include` lines change;
/// header *content* edits evict through `evict_analysis_caches` (the
/// did_save / watched-files invalidation path).
type MacroTable = BTreeMap<String, Macro>;

// ============================================================================
// GatherCache — the ONE byte-capped, single-flight memo all four cpp gather
// caches (macro table, pre-expanded external, header parse, include closure)
// instantiate. It replaces the bare `OnceLock<Mutex<HashMap>>` those four used
// to be (unbounded growth, check-release-compute-insert races). Two properties,
// coupled by design (`docs/adr/memory-slice-2-lru.md`, the residency discipline
// in CLAUDE.md, hitlist H9-3):
//
//   * SINGLE-FLIGHT population. The first worker to miss a key CLAIMS it and
//     computes; siblings expanding the same header cone (op.c/sv.c share ~90% of
//     their include closure) BLOCK on the claimant's result via a condvar
//     instead of each recomputing the whole expansion. One spelling, four
//     caches — never hand-rolled per cache (rule #10's spirit).
//   * BYTE-ACCOUNTED LRU cap. Retention is bounded by `cap_bytes`; the LRU tail
//     is evicted on insert, never the just-inserted key (a single oversized
//     entry over the whole cap still resolves the query it was loaded for — the
//     `PackBagCache` rule). A cap of 0 means never retain (compute-and-drop).
//
// The two are coupled: a cap makes eviction real, which makes recompute storms
// possible on an evicted shared cone — single-flight collapses each storm to one
// flight. Explicit invalidation (`evict_gather_caches`) removes matching entries
// AND cancels any in-flight compute for those keys (the claimant's now-stale
// result is dropped on publish; a waiter recomputes fresh). No deadlock: the
// state lock is NEVER held across a compute, and invalidation only touches the
// lock.

/// What a single-flight compute produced. `Store` caches the value (byte-
/// accounted, LRU-evicted); `Transient` returns it to the caller WITHOUT caching
/// (a degraded / incomplete result that must re-derive next call). A compute
/// returning `None` (the `try` variant) is a MISS — cache nothing, yield nothing.
enum Fill<V> {
    Store(V, usize),
    Transient(V),
}

/// How a `resolve` call settled — lets a caller distinguish a cached answer
/// (hit or freshly stored: authoritative/complete) from a transient one.
enum Resolution {
    Cached,
    Transient,
    Missed,
}

struct GatherEntry<S, V> {
    stamp: S,
    value: V,
    bytes: usize,
    last_used: u64,
}

struct GatherState<K, S, V> {
    entries: HashMap<K, GatherEntry<S, V>>,
    /// Keys with a compute currently running (their claimant owns population).
    in_flight: std::collections::HashSet<K>,
    /// In-flight keys an invalidation targeted mid-compute — the claimant drops
    /// its result on publish so a stale table can't land after the invalidate.
    cancelled: std::collections::HashSet<K>,
    total_bytes: usize,
    clock: u64,
}

pub struct GatherCache<K, S, V> {
    state: std::sync::Mutex<GatherState<K, S, V>>,
    ready: std::sync::Condvar,
    cap_bytes: usize,
}

/// Releases an in-flight claim (and clears any cancel marker) even if `compute`
/// panics — otherwise the key would stay `in_flight` forever and every waiter
/// would wedge on the condvar. The success path disarms it after publishing
/// under the lock.
struct FlightGuard<'a, K, S, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    cache: &'a GatherCache<K, S, V>,
    key: &'a K,
    armed: bool,
}

impl<K, S, V> Drop for FlightGuard<'_, K, S, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut st) = self.cache.state.lock() {
            st.in_flight.remove(self.key);
            st.cancelled.remove(self.key);
        }
        self.cache.ready.notify_all();
    }
}

impl<K, S, V> GatherCache<K, S, V>
where
    K: Eq + std::hash::Hash + Clone,
    S: PartialEq + Clone,
    V: Clone,
{
    fn new(cap_bytes: usize) -> Self {
        GatherCache {
            state: std::sync::Mutex::new(GatherState {
                entries: HashMap::new(),
                in_flight: std::collections::HashSet::new(),
                cancelled: std::collections::HashSet::new(),
                total_bytes: 0,
                clock: 0,
            }),
            ready: std::sync::Condvar::new(),
            cap_bytes,
        }
    }

    /// Fetch the stamp-matching cached value or single-flight compute it; the
    /// compute always yields a value (`Store`/`Transient`), so this never misses.
    fn get_or_fill<F>(&self, key: K, stamp: S, compute: F) -> V
    where
        F: FnOnce() -> Fill<V>,
    {
        self.resolve(key, stamp, || Some(compute()))
            .0
            .expect("get_or_fill compute always yields a value")
    }

    /// Fetch or single-flight compute. `compute` returns `None` for a MISS
    /// (cache nothing, yield `None` — e.g. the on-open cached-only skip, or a
    /// header that failed to read).
    fn get_or_try_fill<F>(&self, key: K, stamp: S, compute: F) -> Option<V>
    where
        F: FnOnce() -> Option<Fill<V>>,
    {
        self.resolve(key, stamp, compute).0
    }

    /// The single-flight + byte-cap core. Returns the value plus how it settled.
    fn resolve<F>(&self, key: K, stamp: S, compute: F) -> (Option<V>, Resolution)
    where
        F: FnOnce() -> Option<Fill<V>>,
    {
        // 1. Acquire the key. A stamp-matching entry is a hit; a live in-flight
        //    compute is waited on (the whole point — no duplicate expansion);
        //    otherwise claim the key so siblings coalesce onto our compute.
        {
            let mut st = self.state.lock().expect("gather cache poisoned");
            loop {
                let fresh = st
                    .entries
                    .get(&key)
                    .filter(|e| e.stamp == stamp)
                    .map(|e| e.value.clone());
                if let Some(v) = fresh {
                    st.clock += 1;
                    let c = st.clock;
                    if let Some(e) = st.entries.get_mut(&key) {
                        e.last_used = c;
                    }
                    return (Some(v), Resolution::Cached);
                }
                if st.in_flight.contains(&key) {
                    st = self.ready.wait(st).expect("gather cache poisoned");
                    continue;
                }
                st.in_flight.insert(key.clone());
                break;
            }
        }

        // 2. Compute with NO lock held (siblings block on the condvar meanwhile).
        let mut guard = FlightGuard { cache: self, key: &key, armed: true };
        let outcome = compute();

        // 3. Publish under the lock. An invalidation that landed for this key
        //    mid-compute (recorded in `cancelled`) drops our stale result.
        let mut st = self.state.lock().expect("gather cache poisoned");
        st.in_flight.remove(&key);
        let cancelled = st.cancelled.remove(&key);
        guard.armed = false;
        let out = match outcome {
            Some(Fill::Store(v, bytes)) => {
                if !cancelled && self.cap_bytes > 0 {
                    if let Some(old) = st.entries.remove(&key) {
                        st.total_bytes -= old.bytes;
                    }
                    st.clock += 1;
                    let c = st.clock;
                    st.total_bytes += bytes;
                    st.entries.insert(
                        key.clone(),
                        GatherEntry { stamp, value: v.clone(), bytes, last_used: c },
                    );
                    self.evict_to_cap(&mut st, &key);
                }
                (Some(v), Resolution::Cached)
            }
            Some(Fill::Transient(v)) => (Some(v), Resolution::Transient),
            None => (None, Resolution::Missed),
        };
        drop(st);
        self.ready.notify_all();
        out
    }

    /// Drop LRU-tail entries until resident bytes are within cap. Never evicts
    /// `keep` (the just-inserted key), matching `PackBagCache::evict_to_cap`.
    fn evict_to_cap(&self, st: &mut GatherState<K, S, V>, keep: &K) {
        while st.total_bytes > self.cap_bytes {
            let victim = st
                .entries
                .iter()
                .filter(|&(k, _)| k != keep)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { break };
            if let Some(e) = st.entries.remove(&victim) {
                st.total_bytes -= e.bytes;
            }
        }
    }

    /// Drop every entry whose key satisfies `pred` and cancel any in-flight
    /// compute for such a key (its result is discarded on publish; a waiter
    /// recomputes fresh). Holds only the state lock — never a compute — so it
    /// can't deadlock a worker waiting on the condvar.
    fn invalidate<P: Fn(&K) -> bool>(&self, pred: P) {
        let mut st = self.state.lock().expect("gather cache poisoned");
        let victims: Vec<K> = st.entries.keys().filter(|k| pred(k)).cloned().collect();
        for k in victims {
            if let Some(e) = st.entries.remove(&k) {
                st.total_bytes -= e.bytes;
            }
        }
        let flight: Vec<K> = st.in_flight.iter().filter(|k| pred(k)).cloned().collect();
        for k in flight {
            st.cancelled.insert(k);
        }
        drop(st);
        self.ready.notify_all();
    }

    /// `(entries, resident_bytes)` — the exact accounted footprint (diagnostic).
    fn stats(&self) -> (usize, usize) {
        let st = self.state.lock().expect("gather cache poisoned");
        (st.entries.len(), st.total_bytes)
    }
}

/// Per-cache byte cap. Each of the four gather caches gets its own default
/// (justified at its constructor); `PERL_LSP_GATHER_CACHE_MB` overrides ALL of
/// them to one value (0 ⇒ never retain — the most aggressive footprint, for
/// A/B'ing the cap's cost). Mirrors the `maxCacheMb` / `PERL_LSP_*` precedents.
fn gather_cap_bytes(default_mb: usize) -> usize {
    let mb = std::env::var("PERL_LSP_GATHER_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default_mb);
    mb.saturating_mul(1024 * 1024)
}

fn macro_heap_bytes(m: &Macro) -> usize {
    m.body.capacity()
        + m.params.as_ref().map_or(0, |p| p.iter().map(|s| s.capacity() + 24).sum())
        + m.guards.iter().map(|s| s.capacity() + 24).sum::<usize>()
        + 48
}

fn macro_table_heap_bytes(t: &MacroTable) -> usize {
    t.iter().map(|(k, v)| k.capacity() + macro_heap_bytes(v) + 32).sum()
}

fn strings_heap_bytes<S: AsRef<str>>(v: &[S]) -> usize {
    v.iter().map(|s| s.as_ref().len() + 24).sum()
}

/// `header_cache` default: 128 MiB. Shared across ALL files (deduped by header
/// PATH, not by consuming file) and NOT dropped by the bulk-index
/// `evict_gather_caches_keep_headers` — it lives for the whole session and is
/// the highest-reuse, lowest-cost tier (~2.6 KB/header measured on re2), so 128
/// MiB holds ~50K distinct headers before the LRU trims the cold tail.
const HEADER_CACHE_MB: usize = 128;
/// `macro_table_cache` default: 128 MiB. Per-file raw merged closure table
/// (perl.h ≈ 2000 macros); 128 MiB matches the PackBagCache/enrichment-overlay
/// budget class for the hottest gather tier.
const MACRO_TABLE_CACHE_MB: usize = 128;
/// `pre_expanded_cache` default: 128 MiB. Full+alias mutual pre-expansion ON
/// TOP of the raw table — the biggest per-entry payload; same 128 MiB class.
const PRE_EXPANDED_CACHE_MB: usize = 128;
/// `include_closure_cache` default: 64 MiB. Per-file path-string lists only
/// (~37 KB/file on abseil), so 64 MiB holds ~1700 files' closures — the
/// smallest per-entry tier gets the smaller cap.
const INCLUDE_CLOSURE_CACHE_MB: usize = 64;

fn macro_table_cache() -> &'static GatherCache<std::path::PathBuf, u64, std::sync::Arc<MacroTable>> {
    static C: OnceLock<GatherCache<std::path::PathBuf, u64, std::sync::Arc<MacroTable>>> =
        OnceLock::new();
    C.get_or_init(|| GatherCache::new(gather_cap_bytes(MACRO_TABLE_CACHE_MB)))
}

/// Hash of the file's `#include` directives — the cache key's variable part.
/// Cheap (one line scan); stable across edits that don't touch includes.
fn include_set_hash(src: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('#') && t[1..].trim_start().starts_with("include") {
            t.hash(&mut h);
        }
    }
    h.finish()
}

/// Bump when the persisted macro-table format or the gather's semantics
/// change in a way that invalidates on-disk blobs.
const MACRO_CACHE_VERSION: i64 = 4;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedMacros {
    include_hash: u64,
    version: i64,
    /// The toolchain the gather resolved system includes against — a probe
    /// failure (or a compiler upgrade) changes which headers the closure
    /// reaches, so a table built under one toolchain must not validate
    /// under another.
    toolchain: u64,
    /// Every transitively-#included header + its content stamp — the table
    /// is valid only while none of them changed (cross-session correctness;
    /// the in-memory cache leans on include_hash alone within a session).
    headers: Vec<(std::path::PathBuf, i64)>,
    table: MacroTable,
}

/// On-disk macro-table cache dir, set once at startup (the CLI / LSP know
/// the workspace root → cache dir). `None` ⇒ persistence off (tests).
fn macro_persist_dir() -> &'static std::sync::OnceLock<Option<std::path::PathBuf>> {
    static D: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    &D
}

/// Point the persisted macro cache at a workspace's cache dir (a `macros/`
/// subdir under it). Idempotent; first call wins.
pub fn set_macro_persist_dir(workspace_cache_dir: Option<std::path::PathBuf>) {
    let resolved = workspace_cache_dir.map(|d| {
        let p = d.join("macros");
        let _ = std::fs::create_dir_all(&p);
        p
    });
    let _ = macro_persist_dir().set(resolved);
}

fn persist_path(file_path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = macro_persist_dir().get()?.clone()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file_path.hash(&mut h);
    Some(dir.join(format!("{:016x}.bin", h.finish())))
}

fn load_persisted(file_path: &std::path::Path, inc_hash: u64) -> Option<MacroTable> {
    let p = persist_path(file_path)?;
    let raw = zstd::decode_all(std::fs::read(&p).ok()?.as_slice()).ok()?;
    let pm: PersistedMacros = bincode::deserialize(&raw).ok()?;
    if pm.include_hash != inc_hash
        || pm.version != MACRO_CACHE_VERSION
        || pm.toolchain != toolchain_fingerprint()
    {
        return None;
    }
    if pm.headers.iter().any(|(hp, st)| file_stamp(hp) != *st) {
        return None; // a header changed on disk
    }
    Some(pm.table)
}

fn save_persisted(
    file_path: &std::path::Path,
    inc_hash: u64,
    headers: Vec<(std::path::PathBuf, i64)>,
    table: &MacroTable,
) {
    let Some(p) = persist_path(file_path) else { return };
    let pm = PersistedMacros {
        include_hash: inc_hash,
        version: MACRO_CACHE_VERSION,
        toolchain: toolchain_fingerprint(),
        headers,
        table: table.clone(),
    };
    if let Ok(raw) = bincode::serialize(&pm) {
        if let Ok(z) = zstd::encode_all(raw.as_slice(), 3) {
            let _ = std::fs::write(&p, z);
        }
    }
}

thread_local! {
    /// When set on the current thread, `included_macros*` skip the cold gather.
    static GATHER_CACHED_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// When set on the current thread, `included_macros*` return whatever's cached
/// (in-mem or on-disk) but SKIP the cold gather — yielding an empty external
/// set (degraded) instead of blocking. `did_open` sets this so the first
/// analyze of a macro-heavy file is instant; a background task then runs the
/// real gather and re-analyzes (the async-refresh path).
pub fn set_gather_cached_only(v: bool) {
    GATHER_CACHED_ONLY.with(|c| c.set(v));
}
fn gather_cached_only() -> bool {
    GATHER_CACHED_ONLY.with(|c| c.get())
}

pub fn included_macros(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
) -> std::sync::Arc<MacroTable> {
    included_macros_inner(file_path, src, parser, true)
        .unwrap_or_else(|| std::sync::Arc::new(MacroTable::new()))
}

/// The three-tier lookup. `allow_cold=false` (the on-open path) stops after the
/// two cache tiers, returning `None` rather than paying the cold gather — the
/// caller degrades to an empty external set and lets a background task warm it.
fn included_macros_inner(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
    allow_cold: bool,
) -> Option<std::sync::Arc<MacroTable>> {
    let key = file_path.to_path_buf();
    let inc_hash = include_set_hash(src);
    // Tier 1 (in-memory, this session) IS the GatherCache hit. On a miss the
    // single-flight claimant runs tiers 2+3; siblings on the same key wait for
    // its result rather than re-paying the cold gather.
    macro_table_cache().get_or_try_fill(key, inc_hash, || {
        // Tier 2: on-disk (across sessions) — kills the cold-start gather.
        if let Some(table) = load_persisted(file_path, inc_hash) {
            let arc = std::sync::Arc::new(table);
            let bytes = macro_table_heap_bytes(&arc);
            return Some(Fill::Store(arc, bytes));
        }
        if !allow_cold {
            return None; // on-open: don't block on the cold gather
        }
        // Tier 3: gather cold, warm disk + this cache.
        let (table, headers) = gather_included_macros(file_path, src, parser);
        save_persisted(file_path, inc_hash, headers, &table);
        let arc = std::sync::Arc::new(table);
        let bytes = macro_table_heap_bytes(&arc);
        Some(Fill::Store(arc, bytes))
    })
}

/// One pre-expanded variant of the external table + the identifiers its bodies
/// name. `table` is `pre_expand_bodies`d once; `body_idents` (every identifier
/// in a `table` body) drives the clean-split test — a file-local name in this
/// set means an external expansion would depend on it, so the split can't bake
/// it and the analyze falls to the slow single-tier path.
#[derive(Default)]
struct ExpandedVariant {
    table: MacroTable,
    body_idents: std::collections::HashSet<String>,
}

impl ExpandedVariant {
    fn of(macros: &MacroTable) -> Self {
        let table = pre_expand_bodies(macros);
        let body_idents = body_identifiers(&table);
        ExpandedVariant { table, body_idents }
    }
}

/// The EXTERNAL macro table (from the `#include` closure), mutually pre-expanded
/// ONCE per include-set and cached. External-referencing-external object refs
/// are baked into the variants, so the per-analyze transform never re-fixpoints
/// the huge external set (perl.h ≈ 2000 macros) — it fixpoints only the
/// file-LOCAL macros and resolves external names by lookup here. `raw` is
/// retained for the byte-identical slow fallback, whose single-tier merge +
/// fixpoint needs the un-pre-expanded external bodies.
#[derive(Default)]
pub struct PreExpandedExternal {
    raw: std::sync::Arc<MacroTable>,
    /// Full mutual pre-expansion (the `preprocess_with` path).
    full: ExpandedVariant,
    /// Identifier-alias subset only (the parse-damage `alias_only` fallback):
    /// `is_identifier_alias`-retained BEFORE expansion, matching the old
    /// merge-then-retain-then-fixpoint order.
    alias: ExpandedVariant,
    /// The gather was SKIPPED (cached-only miss on open), not run: this
    /// empty table is a stand-in, not the truth. Analyses built from it
    /// are marked degraded so the persist tier never freezes them.
    pub degraded: bool,
}

impl PreExpandedExternal {
    pub fn empty() -> Self {
        Self::default()
    }

    /// The cached-only miss: empty AND flagged so downstream consumers know
    /// the external table is a placeholder, not a real (possibly empty) gather.
    fn degraded_empty() -> Self {
        PreExpandedExternal { degraded: true, ..Self::default() }
    }

    fn from_raw(raw: std::sync::Arc<MacroTable>) -> Self {
        // This mutual pre-expansion is the O(external) work the two-tier split
        // hoists out of every analyze — paid ONCE per include-set here, then
        // reused warm. Labelled so `PERL_LSP_PHASE_TIMING` shows the per-analyze
        // cost it eliminates.
        let (full, alias) = crate::timings::phase("cpp.external_preexpand", || {
            let full = ExpandedVariant::of(&raw);
            let mut alias_src = (*raw).clone();
            alias_src.retain(|_, m| is_identifier_alias(m));
            (full, ExpandedVariant::of(&alias_src))
        });
        PreExpandedExternal { raw, full, alias, degraded: false }
    }

    fn variant(&self, alias_only: bool) -> &ExpandedVariant {
        if alias_only {
            &self.alias
        } else {
            &self.full
        }
    }

    /// Object-like gathered macros as `(name, body)` — the raw (un-pre-expanded)
    /// bodies, so a `#define X Y` stays an alias EDGE (`X → TypeName(Y)`) the
    /// bag chases, rather than a flattened leaf. The type-alias emission uses
    /// this to carry an include-closure's type macros (`U16TYPE` from a
    /// gitignored generated `config.h`) into every consuming file's bag, where
    /// the cross-file `TypeName` chase can never index the header directly.
    pub fn object_like_macros(&self) -> impl Iterator<Item = (&str, &str)> {
        self.raw
            .iter()
            .filter(|(_, m)| m.params.is_none())
            .map(|(k, m)| (k.as_str(), m.body.as_str()))
    }

    /// Every gathered macro NAME (object- and function-like) — the include
    /// closure's macro universe. The nested-macro-body ref lane unions this
    /// with the file's own `#define`s so a body token naming a header-defined
    /// macro (`SvFLAGS` used inside an `hv.h` macro) still mints a reference.
    pub fn macro_names(&self) -> impl Iterator<Item = &str> {
        self.raw.keys().map(|k| k.as_str())
    }
}

/// Every identifier token appearing in any macro body — the reference
/// candidates. Used to detect an external body that (transitively, since
/// `expanded` bodies are already baked) names a file-local macro.
fn body_identifiers(macros: &MacroTable) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for m in macros.values() {
        let bytes = m.body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if is_ident_byte(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
                let s = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                out.insert(m.body[s..i].to_string());
            } else {
                i += 1;
            }
        }
    }
    out
}

fn pre_expanded_cache() -> &'static GatherCache<std::path::PathBuf, u64, std::sync::Arc<PreExpandedExternal>>
{
    static C: OnceLock<
        GatherCache<std::path::PathBuf, u64, std::sync::Arc<PreExpandedExternal>>,
    > = OnceLock::new();
    C.get_or_init(|| GatherCache::new(gather_cap_bytes(PRE_EXPANDED_CACHE_MB)))
}

/// The full+alias expanded-variant payload ADDED on top of the raw table (the
/// raw `Arc` is shared with `macro_table_cache`, so it is NOT counted here).
fn pre_expanded_heap_bytes(pe: &PreExpandedExternal) -> usize {
    macro_table_heap_bytes(&pe.full.table)
        + pe.full.body_idents.iter().map(|s| s.len() + 24).sum::<usize>()
        + macro_table_heap_bytes(&pe.alias.table)
        + pe.alias.body_idents.iter().map(|s| s.len() + 24).sum::<usize>()
}

/// `included_macros` plus the one-time mutual pre-expansion of the external
/// table, cached by the same (file, include-set) key. Warm analyzes reuse the
/// pre-expanded table for free — the transform then only fixpoints file-local
/// macros. This is the driver's `gather_macros` hook.
pub fn included_macros_pre_expanded(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
) -> std::sync::Arc<PreExpandedExternal> {
    let key = file_path.to_path_buf();
    let inc_hash = include_set_hash(src);
    pre_expanded_cache().get_or_fill(key, inc_hash, || {
        // In cached-only mode (on-open), a raw-table miss yields an EMPTY
        // external set that is deliberately NOT cached (`Transient`) — so the
        // background gather's real table lands cleanly once it warms and this
        // file is re-analyzed.
        match included_macros_inner(file_path, src, parser, !gather_cached_only()) {
            Some(raw) => {
                let pe = std::sync::Arc::new(PreExpandedExternal::from_raw(raw));
                let bytes = pre_expanded_heap_bytes(&pe);
                Fill::Store(pe, bytes)
            }
            None => Fill::Transient(std::sync::Arc::new(PreExpandedExternal::degraded_empty())),
        }
    })
}

thread_local! {
    /// Each Rayon worker keeps its own `Parser` — tree-sitter parsers aren't
    /// `Sync`, so the parallel frontier can't share one. Created once per thread.
    static POOL_PARSER: std::cell::RefCell<Option<tree_sitter::Parser>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with this thread's pooled parser for `lang`.
fn with_pooled_parser<T>(
    lang: &tree_sitter::Language,
    f: impl FnOnce(&mut tree_sitter::Parser) -> T,
) -> T {
    POOL_PARSER.with(|slot| {
        let mut b = slot.borrow_mut();
        if b.is_none() {
            let mut p = tree_sitter::Parser::new();
            p.set_language(lang).expect("cpp grammar for pooled parser");
            *b = Some(p);
        }
        f(b.as_mut().expect("pooled parser present"))
    })
}

/// Walk the `#include` closure and collect every reachable header's macros.
///
/// Parallel + memoized: each BFS LEVEL's headers are parsed concurrently (Rayon,
/// one pooled `Parser` per worker); `header_info` memoizes by `(path, mtime)` so
/// a header shared across the closure — or across FILES (op.c and sv.c share
/// ~90% of perl5's tree) — is parsed exactly once. There is no header cap: the
/// `seen` set alone bounds the walk (cycles + re-visits), and the memoize bounds
/// the cost, so op.c's full closure is collected instead of truncated.
///
/// BREADTH-first, first-wins: the file's DIRECT includes are merged before
/// theirs, so the closest (most relevant) header's definition of a name wins —
/// the abseil `mutex.h`-vs-`thread_annotations.h` invariant. Determinism under
/// parallelism: a level is canonicalized + deduped SERIALLY in queue order, and
/// the parsed results are merged (and their children enqueued) in that same
/// order, so the macro table is deterministic regardless of parallelism.
fn gather_included_macros(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
) -> (BTreeMap<String, Macro>, Vec<(std::path::PathBuf, i64)>) {
    use rayon::prelude::*;
    let mut macros = BTreeMap::new();
    let mut headers: Vec<(std::path::PathBuf, i64)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(p) = file_path.canonicalize() {
        seen.insert(p);
    }
    let Some(lang) = parser.language().map(|l| (*l).clone()) else {
        return (macros, headers);
    };
    let mut frontier: Vec<std::path::PathBuf> = include_paths(src, parser)
        .iter()
        .filter_map(|inc| resolve_include(file_path, inc))
        .collect();
    while !frontier.is_empty() {
        // Canonicalize + dedup this level in queue order (cheap stat) so the
        // parallel parse below can't perturb the first-wins merge order.
        let mut level: Vec<std::path::PathBuf> = Vec::with_capacity(frontier.len());
        for path in frontier.drain(..) {
            let Ok(canon) = path.canonicalize() else { continue };
            if seen.insert(canon.clone()) {
                level.push(canon);
            }
        }
        // header_info is pure per header → parse the level concurrently.
        let infos: Vec<Option<std::sync::Arc<CachedHeader>>> = level
            .par_iter()
            .map(|canon| with_pooled_parser(&lang, |p| header_info(canon, p)))
            .collect();
        let mut next: Vec<std::path::PathBuf> = Vec::new();
        for (canon, info) in level.iter().zip(infos) {
            let Some(info) = info else { continue };
            headers.push((canon.clone(), file_stamp(canon)));
            for (k, v) in &info.macros {
                macros.entry(k.clone()).or_insert_with(|| v.clone());
            }
            for inc in &info.includes {
                if let Some(nx) = resolve_include(canon, inc) {
                    next.push(nx);
                }
            }
        }
        frontier = next;
    }
    (macros, headers)
}

/// The persisted macro table's per-header validation stamp: a hash of
/// (mtime nanos, size). Whole-second mtimes miss two same-length writes
/// within one second (generated headers, rapid saves) — nanosecond
/// precision plus size closes that window. 0 if unreadable.
fn file_stamp(path: &std::path::Path) -> i64 {
    use std::hash::{Hash, Hasher};
    let Ok(meta) = std::fs::metadata(path) else { return 0 };
    let Ok(mtime) = meta.modified() else { return 0 };
    let nanos = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut h);
    meta.len().hash(&mut h);
    h.finish() as i64
}

/// A header's cached (macros + include edges), by (path, mtime). The cache
/// makes the per-edit re-gather cheap (warm hits skip read+parse).
fn header_info(canon: &std::path::Path, parser: &mut tree_sitter::Parser) -> Option<std::sync::Arc<CachedHeader>> {
    let mut build = |canon: &std::path::Path| -> Option<std::sync::Arc<CachedHeader>> {
        let src = std::fs::read_to_string(canon).ok()?;
        let tree = parser.parse(&src, None)?;
        Some(std::sync::Arc::new(CachedHeader {
            macros: collect_macros(&tree, src.as_bytes()),
            includes: include_paths_tree(&tree, &src),
        }))
    };
    // No mtime (metadata failed) ⇒ no stamp: compute uncached, as before.
    let Some(mtime) = std::fs::metadata(canon).and_then(|m| m.modified()).ok() else {
        return build(canon);
    };
    // Single-flight by (path, mtime): sibling TUs including the same header
    // (op.c/sv.c share most of theirs) wait for ONE read+parse, not N.
    header_cache().get_or_try_fill(canon.to_path_buf(), mtime, || {
        build(canon).map(|info| {
            let bytes = header_heap_bytes(&info);
            Fill::Store(info, bytes)
        })
    })
}

fn header_heap_bytes(h: &CachedHeader) -> usize {
    macro_table_heap_bytes(&h.macros) + strings_heap_bytes(&h.includes)
}

/// A header's own #defines + its include edges — cached by (path, mtime)
/// so the per-edit re-gather doesn't re-read + re-parse the same dozens of
/// transitive headers every keystroke (the server is long-lived; headers
/// rarely change mid-edit, and mtime invalidates when they do).
struct CachedHeader {
    macros: BTreeMap<String, Macro>,
    includes: Vec<String>,
}

fn header_cache(
) -> &'static GatherCache<std::path::PathBuf, std::time::SystemTime, std::sync::Arc<CachedHeader>> {
    static C: OnceLock<
        GatherCache<std::path::PathBuf, std::time::SystemTime, std::sync::Arc<CachedHeader>>,
    > = OnceLock::new();
    C.get_or_init(|| GatherCache::new(gather_cap_bytes(HEADER_CACHE_MB)))
}

/// The default C/C++ toolchain's discovered surface (system include roots +
/// predefined macros), probed once via the compiler and cached process-globally
/// (`OnceLock`). `None` when no compiler is on PATH — include resolution then
/// degrades to workspace-only (today's behavior). Probed as C++ so `include_dirs`
/// is the SUPERSET that also resolves the C system headers (`<sys/mman.h>`);
/// `predefined_macros` rides along for the `#if`-eval consumer.
pub fn toolchain_info() -> Option<&'static crate::cpp_toolchain::ToolchainInfo> {
    static INFO: std::sync::OnceLock<Option<crate::cpp_toolchain::ToolchainInfo>> =
        std::sync::OnceLock::new();
    INFO.get_or_init(|| {
        crate::cpp_toolchain::default_compiler(crate::cpp_toolchain::Lang::Cpp)
            .and_then(|c| crate::cpp_toolchain::probe(&c, None))
    })
    .as_ref()
}

/// Identity of the analysis-input toolchain: compiler version + system
/// include roots + predefined macros, or a distinct sentinel when the probe
/// failed. Rides every persist-tier validation key (macro tables, the
/// pack modules DB) so a degraded generation — probe failure silently
/// emptying the system include roots — can never freeze into the cache
/// and be re-served after the toolchain comes back.
pub fn toolchain_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match toolchain_info() {
        Some(t) => {
            t.compiler_version.hash(&mut h);
            t.include_dirs.hash(&mut h);
            t.predefined_macros.hash(&mut h);
        }
        None => "no-toolchain".hash(&mut h),
    }
    h.finish()
}

/// System/stdlib include roots, in compiler search order — the `<...>` fallback
/// for headers no workspace ancestor holds (`<sys/mman.h>`). Empty when no
/// compiler was found.
fn system_include_dirs() -> &'static [std::path::PathBuf] {
    static DIRS: std::sync::OnceLock<Vec<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| toolchain_info().map(|t| t.include_dirs.clone()).unwrap_or_default())
}

/// Resolve an include like `spdlog/common.h` or `<sys/mman.h>` to a real path.
/// Workspace-first: walk up from the file's dir, first ancestor `R` where
/// `R/<inc>` exists wins (project/relative headers, quoted or angle-bracket).
/// Only when no ancestor has it do the toolchain's system roots answer — so a
/// system `<sys/mman.h>` resolves (its subtree was silently lost before), while
/// a project header still shadows a same-named system one.
fn resolve_include(file_path: &std::path::Path, inc: &str) -> Option<std::path::PathBuf> {
    if let Some(mut dir) = file_path.parent() {
        loop {
            let cand = dir.join(inc);
            if cand.is_file() {
                return Some(cand);
            }
            // The conventional `-Iinclude` layout: a test/src file spelling
            // `#include "fmt/format.h"` reaches `<root>/include/fmt/format.h`.
            // Without this the whole test/src tree gets an empty project
            // closure and the visibility gate cuts it off from every target.
            let cand = dir.join("include").join(inc);
            if cand.is_file() {
                return Some(cand);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    for root in system_include_dirs() {
        let cand = root.join(inc);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Resolve one `#include` path token (quoted or angle-bracket) to a real file,
/// workspace-first then toolchain roots. The public seam for goto-def on an
/// `#include` path token (`FileAnalysis::include_directives`).
pub fn resolve_include_path(file_path: &std::path::Path, inc: &str) -> Option<std::path::PathBuf> {
    resolve_include(file_path, inc)
}

/// Every `#include` directive's raw path text, by a cheap per-line scan (no
/// parse) — the header BFS only needs the paths, so this stays far lighter than
/// `header_info`'s full tree parse. Quoted `"x.h"` and angle-bracket `<x.h>`
/// alike (the walk-up resolver finds project headers written either way).
fn scan_include_directives(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix('#') else { continue };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("include") else { continue };
        let rest = rest.trim_start();
        let path = match rest.as_bytes().first() {
            Some(b'"') => rest[1..].split('"').next(),
            Some(b'<') => rest[1..].split('>').next(),
            _ => None,
        };
        if let Some(p) = path {
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn include_closure_cache(
) -> &'static GatherCache<std::path::PathBuf, u64, std::sync::Arc<Vec<String>>> {
    static C: OnceLock<GatherCache<std::path::PathBuf, u64, std::sync::Arc<Vec<String>>>> =
        OnceLock::new();
    C.get_or_init(|| GatherCache::new(gather_cap_bytes(INCLUDE_CLOSURE_CACHE_MB)))
}

/// The transitive `#include` closure of `file_path`, as canonical path strings
/// (sorted, unique) — the cross-file VISIBILITY key (`docs/adr/macro-handling.md`,
/// "the include-closure lie"): a name resolves preferentially to a definition in
/// a file this set reaches. BFS over the include graph via a cheap line scan;
/// memoized by (path, include-set hash) so the per-edit re-analyze is warm.
///
/// Respects the on-open cached-only gate: on open an unwarmed file returns an
/// empty closure — like cross-file macros, the background re-analyze fills it —
/// so the first open never blocks on the cold header walk. An empty closure is
/// safe: the visibility ranking degrades to the global winner (today's behavior).
///
/// The bool is COMPLETENESS: `false` when the closure is a placeholder (the
/// cached-only skip) or was truncated by a header that RESOLVED and exists yet
/// failed to read (non-UTF-8, transient I/O). A truncated closure is the one
/// blind spot of the `deps_stamp` persist key — the stamp is recomputed over
/// the STORED list at load time, so it self-validates whatever subset was
/// frozen and never re-derives (`module_cache::closure_stamp`). The driver
/// folds `!complete` into `degraded` so `save_to_db` refuses the row; a
/// complete gather next session re-derives it. An UNRESOLVED include (a system
/// header off the search path) is NOT incompleteness — it's a legitimate
/// closure boundary, deterministic across runs.
pub fn include_closure(file_path: &std::path::Path, src: &str) -> (Vec<String>, bool) {
    let key = file_path.to_path_buf();
    let inc_hash = include_set_hash(src);
    // The walk runs single-flight on a miss. A hit or a freshly-stored (COMPLETE)
    // closure resolves `Cached`; the cached-only placeholder and a truncated
    // closure resolve `Transient` (returned, never cached) → `complete = false`.
    let (arc, res) = include_closure_cache().resolve(key, inc_hash, || {
        if gather_cached_only() {
            // on-open placeholder: fill on background re-analyze
            return Some(Fill::Transient(std::sync::Arc::new(Vec::new())));
        }
        let mut seen = std::collections::HashSet::new();
        if let Ok(p) = file_path.canonicalize() {
            seen.insert(p);
        }
        let mut out: Vec<String> = Vec::new();
        let mut complete = true;
        let mut frontier: Vec<std::path::PathBuf> = scan_include_directives(src)
            .iter()
            .filter_map(|inc| resolve_include(file_path, inc))
            .collect();
        while !frontier.is_empty() {
            let mut next: Vec<std::path::PathBuf> = Vec::new();
            for path in frontier.drain(..) {
                let Ok(canon) = path.canonicalize() else { continue };
                if !seen.insert(canon.clone()) {
                    continue;
                }
                out.push(canon.to_string_lossy().into_owned());
                match std::fs::read_to_string(&canon) {
                    Ok(hsrc) => {
                        for inc in scan_include_directives(&hsrc) {
                            if let Some(nx) = resolve_include(&canon, &inc) {
                                next.push(nx);
                            }
                        }
                    }
                    // The header canonicalized (exists) but couldn't be read: its
                    // transitive includes are silently dropped, truncating the
                    // closure. Mark incomplete so the analysis isn't frozen.
                    Err(_) => complete = false,
                }
            }
            frontier = next;
        }
        out.sort();
        out.dedup();
        let arc = std::sync::Arc::new(out);
        // Only memoize a COMPLETE closure: a transient truncation must re-gather
        // next call, not stick in the in-session cache.
        if complete {
            let bytes = strings_heap_bytes(&arc);
            Some(Fill::Store(arc, bytes))
        } else {
            Some(Fill::Transient(arc))
        }
    });
    let complete = matches!(res, Resolution::Cached);
    (arc.map(|a| (*a).clone()).unwrap_or_default(), complete)
}

/// Drop every per-file analysis cache entry for the given files (CANONICAL
/// paths): the tier-1 macro table, its pre-expanded variants, the include
/// closure, and the header parse cache. The in-session invalidation seam —
/// a saved/changed pack file evicts itself + every consumer whose closure
/// contains it, so the next analyze re-gathers instead of serving the
/// frozen table (cache keys are whatever path `analyze_with_path` got, so
/// membership is checked on the canonicalized key).
pub fn evict_analysis_caches(files: &std::collections::HashSet<std::path::PathBuf>) {
    evict_gather_caches(files, true);
}

/// Residency-only eviction for the bulk workspace index: drop the per-file
/// merged/expanded macro tables (`macro_table_cache`, `pre_expanded_cache`) +
/// the closure memo for files whose `FileAnalysis` is already built and
/// persisted, but keep `header_cache` warm. The per-file tables are a private
/// memo of each source file's include-closure merge — never read by any other
/// file's gather (that only consults `header_cache`), disk-backed, and cheaply
/// re-derived from the warm shared header table on a later on-edit re-gather.
/// See `docs/adr/memory-slice-2-lru.md`. Content-edit invalidation
/// must NOT use this — a changed header's own `header_cache` entry has to go,
/// so that path calls `evict_analysis_caches` (drops headers too).
pub fn evict_gather_caches_keep_headers(files: &std::collections::HashSet<std::path::PathBuf>) {
    evict_gather_caches(files, false);
}

fn evict_gather_caches(files: &std::collections::HashSet<std::path::PathBuf>, drop_headers: bool) {
    let hit = |key: &std::path::PathBuf| {
        files.contains(key)
            || key
                .canonicalize()
                .map(|c| files.contains(&c))
                .unwrap_or(false)
    };
    // `invalidate` drops matching entries AND cancels any in-flight compute for
    // them (a claimant's stale result is discarded on publish; a waiter
    // recomputes) — no deadlock, it only touches the state lock.
    macro_table_cache().invalidate(&hit);
    pre_expanded_cache().invalidate(&hit);
    include_closure_cache().invalidate(&hit);
    if drop_headers {
        header_cache().invalidate(&hit);
    }
}

/// Measurement aid (gated by callers behind `PERL_LSP_MEM_REPORT`): a rough
/// resident-byte estimate of the four process-global gather caches. Counts the
/// heap payload of each `String`/`Vec` (capacity), not `size_of` overhead, so
/// the numbers track the actual macro-table blow-up. NOT wired into any query
/// path — a diagnostic only.
pub fn cache_size_report() -> String {
    // The caches now byte-account at insert time, so the resident footprint is
    // read straight off each `GatherCache` (`macro_table_heap_bytes` etc. are
    // the same estimators these totals were summed with).
    let (mt_n, mt_b) = macro_table_cache().stats();
    let (hc_n, hc_b) = header_cache().stats();
    // pre_expanded's `raw` Arc is SHARED with macro_table_cache (same
    // allocation) — its total counts only the ADDED full+alias variants.
    let (pe_n, pe_b) = pre_expanded_cache().stats();
    let (ic_n, ic_b) = include_closure_cache().stats();
    let mb = |b: usize| b as f64 / 1_048_576.0;
    format!(
        "cpp gather caches (heap payload est.):\n  header_cache:       {hc_n:>6} headers, {:>8.1} MB (shared across files)\n  macro_table_cache:  {mt_n:>6} files,   {:>8.1} MB (raw merged table, Arc-shared w/ pre_expanded)\n  pre_expanded_cache: {pe_n:>6} files,   {:>8.1} MB (full+alias expanded variants, ON TOP of raw)\n  include_closure:    {ic_n:>6} files,   {:>8.1} MB\n  TOTAL: {:>8.1} MB",
        mb(hc_b), mb(mt_b), mb(pe_b), mb(ic_b), mb(hc_b + mt_b + pe_b + ic_b)
    )
}

fn include_paths(src: &str, parser: &mut tree_sitter::Parser) -> Vec<String> {
    match parser.parse(src, None) {
        Some(tree) => include_paths_tree(&tree, src),
        None => Vec::new(),
    }
}

/// Every include's path, quoted (`"x/y.h"`) and angle-bracket
/// (`<lib/y.h>`) alike — library headers write project includes with
/// `<>`, and the walk-up resolver finds both (true system headers like
/// `<vector>` simply don't resolve in the workspace, and are skipped).
fn include_paths_tree(tree: &Tree, src: &str) -> Vec<String> {
    let q = cached_query(&INCLUDE_Q, &tree.language(), INCLUDE_QUERY);
    let names = q.capture_names().to_vec();
    let mut out = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(q, tree.root_node(), src.as_bytes());
    while let Some(m) = it.next() {
        for c in m.captures {
            let Ok(t) = c.node.utf8_text(src.as_bytes()) else { continue };
            match names[c.index as usize] {
                "p" => out.push(t.to_string()),
                "s" => out.push(t.trim_start_matches('<').trim_end_matches('>').to_string()),
                _ => {}
            }
        }
    }
    out
}

/// An object-like macro whose body is a single bare identifier — a pure
/// rename (`op_prune_chain_head → Perl_op_prune_chain_head`). Expanding it
/// is provably parse-safe (an identifier replaces an identifier; the token
/// structure is unchanged), so it can be kept even when the full
/// expansion's validate gate rejects the file.
fn is_identifier_alias(m: &Macro) -> bool {
    m.params.is_none()
        && !m.body.is_empty()
        && m.body.bytes().all(is_ident_byte)
}

/// The transform: expand macro invocations in `src`, returning the rewritten
/// source and the anchor map. Single source-level pass. The file's own
/// `#define`s win on conflict; `external` (gathered from `#include`d headers)
/// fills in cross-file names like `SPDLOG_NAMESPACE_BEGIN`.
pub fn preprocess_with(
    tree: &Tree,
    src: &str,
    external: &PreExpandedExternal,
) -> (String, SpliceMap) {
    // Default: conditional-region bodies are expandable (narrow exclusion). The
    // damage-raising fallback in `preprocess_validated_with` re-runs with the
    // wide scope when this widening hurts a file.
    preprocess_with_mode(tree, src, external, false, true)
}

/// The two-tier macro view the source-splice pass queries: file-LOCAL
/// macros (fixpointed per analyze) layered over the cached, pre-expanded
/// EXTERNAL table (external-referencing-external already baked). Local wins on
/// a name conflict. On the slow fallback, `local` holds the full merged +
/// fixpointed map and `external` is empty — a single-tier lookup.
struct EffectiveMacros<'a> {
    local: BTreeMap<String, Macro>,
    external: &'a BTreeMap<String, Macro>,
}

impl EffectiveMacros<'_> {
    fn get(&self, name: &str) -> Option<&Macro> {
        self.local.get(name).or_else(|| self.external.get(name))
    }
    fn is_empty(&self) -> bool {
        self.local.is_empty() && self.external.is_empty()
    }
}

fn empty_table() -> &'static BTreeMap<String, Macro> {
    static E: std::sync::OnceLock<BTreeMap<String, Macro>> = std::sync::OnceLock::new();
    E.get_or_init(BTreeMap::new)
}

fn force_slow_path() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("PERL_LSP_CPP_NO_FASTPATH").is_some())
}

/// Build the macro view for one analyze. FAST path (the common case): the file
/// LOCAL macros are the only set fixpointed here; external names resolve by
/// lookup into the cached, already-expanded `external.expanded`. SLOW path
/// (a local shadows an external name, or an external body references a local
/// name — both cheap to detect against the cached `body_idents`): merge the
/// raw external set with the locals and fixpoint the whole thing, exactly as a
/// single-tier expansion would — byte-identical, at the old cost.
fn build_effective_macros<'a>(
    tree: &Tree,
    src: &str,
    external: &'a PreExpandedExternal,
    alias_only: bool,
    force_slow: bool,
) -> EffectiveMacros<'a> {
    let local_all = collect_macros(tree, src.as_bytes());
    let ext = external.variant(alias_only);
    // Conservative clean-split test (ALL local names, pre-retain): if any local
    // name collides with an external def, or is named by any external body, the
    // two tiers interact and the split can't stay byte-identical → slow path.
    let clean = !force_slow
        && local_all
            .keys()
            .all(|k| !ext.table.contains_key(k) && !ext.body_idents.contains(k));
    if clean {
        let mut local = local_all;
        if alias_only {
            local.retain(|_, m| is_identifier_alias(m));
        }
        let local = pre_expand_local(local, &ext.table);
        EffectiveMacros { local, external: &ext.table }
    } else {
        let mut merged = local_all;
        for (k, v) in external.raw.iter() {
            merged.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if alias_only {
            merged.retain(|_, m| is_identifier_alias(m));
        }
        EffectiveMacros { local: pre_expand_bodies(&merged), external: empty_table() }
    }
}

/// Fixpoint-expand only the LOCAL macro bodies (depth-capped, blue-painted),
/// resolving object-like references to file-local names among `local` and to
/// external names via the already-expanded (terminal) `external` table. The
/// external tier is never re-fixpointed or cloned — the whole point.
fn pre_expand_local(
    local: BTreeMap<String, Macro>,
    external: &BTreeMap<String, Macro>,
) -> BTreeMap<String, Macro> {
    let mut out = local;
    for _ in 0..8 {
        let mut changed = false;
        let snapshot = out.clone();
        for (name, m) in out.iter_mut() {
            let expanded = expand_text_layered(&m.body, &snapshot, external, Some(name));
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

/// `expand_text` over two tiers: an object-like reference resolves against
/// `primary` first (the fixpointing local snapshot), then `secondary` (the
/// terminal external table). Blue-paints `exclude` (a macro isn't re-expanded
/// in its own body).
fn expand_text_layered(
    text: &str,
    primary: &BTreeMap<String, Macro>,
    secondary: &BTreeMap<String, Macro>,
    exclude: Option<&str>,
) -> String {
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
            let m = if Some(word) == exclude {
                None
            } else {
                primary.get(word).or_else(|| secondary.get(word))
            };
            match m {
                Some(m) if m.params.is_none() => out.push_str(&m.body),
                _ => out.push_str(word),
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// `alias_only` restricts expansion to identifier-alias macros (the
/// validate-gate-safe subset) — used as the fallback when the full
/// expansion raises parse damage.
fn preprocess_with_mode(
    tree: &Tree,
    src: &str,
    external: &PreExpandedExternal,
    alias_only: bool,
    expand_region_bodies: bool,
) -> (String, SpliceMap) {
    preprocess_with_mode_inner(tree, src, external, alias_only, force_slow_path(), expand_region_bodies)
}

/// The splice pass proper, `force_slow` explicit (env-gate read at the public
/// boundary) so the differential test can drive the fast and slow tiers on the
/// same input and assert byte-identical output.
fn preprocess_with_mode_inner(
    tree: &Tree,
    src: &str,
    external: &PreExpandedExternal,
    alias_only: bool,
    force_slow: bool,
    expand_region_bodies: bool,
) -> (String, SpliceMap) {
    let mut splices =
        compute_splices_inner(tree, src, external, alias_only, force_slow, expand_region_bodies);
    apply(src, &mut splices)
}

/// The splice set the expansion pass would apply — exposed separately so the
/// per-splice salvage can bisect it (`salvage_splices`).
fn compute_splices(
    tree: &Tree,
    src: &str,
    external: &PreExpandedExternal,
    alias_only: bool,
    expand_region_bodies: bool,
) -> Vec<Splice> {
    compute_splices_inner(tree, src, external, alias_only, force_slow_path(), expand_region_bodies)
}

/// The per-name expansion-safety verdict, computed ONCE from the macro's body
/// (a property, never the name — rule #10). `true` means the expansion is
/// **context-independently safe**: it can be spliced in *any* position without
/// raising parse damage, so it need never be stranded in a dropped
/// conditional-region batch or a salvage-budget tail.
///
/// The provable class is an object-like macro with an empty/whitespace body: its
/// expansion is pure byte-DELETION (`pTHX_`/`aTHX_` under a non-multiplicity
/// config), which can only ever REMOVE a token, never introduce a malformed one.
/// A non-empty fragment (`pTHX_ → PerlInterpreter *my_perl,`) is position-
/// dependent (the trailing comma is safe only in a param list), so it stays
/// under the normal exclusion/validation path — the whole-file gate is the
/// backstop either way. See `docs/prompt-macro-salvage-scaling.md`.
fn is_context_free_safe(m: &Macro) -> bool {
    m.params.is_none() && m.body.trim().is_empty()
}

/// Forward-cursor membership: is `pos` inside one of the sorted, disjoint
/// `spans`? `cursor` only advances, so successive calls with non-decreasing
/// `pos` stay O(1) amortized (the same discipline the `excludes` walk uses).
fn span_contains(spans: &[(usize, usize)], cursor: &mut usize, pos: usize) -> bool {
    while *cursor < spans.len() && spans[*cursor].1 <= pos {
        *cursor += 1;
    }
    *cursor < spans.len() && spans[*cursor].0 <= pos
}

/// Byte offset of the start of each source line, indexed by 0-based row.
fn line_start_offsets(src: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// The byte at which each file-LOCAL macro's object-like/function-like
/// `#define` becomes active — the start of its directive line. A use of the
/// name STRICTLY BEFORE this byte predates the definition and, per the C
/// preprocessor, must NOT expand: `#define Simplify DontCallSimplify` at
/// re2/simplify.cc:201 protects the out-of-line def `Regexp* Regexp::Simplify()`
/// at :180 and the call at :31, which both keep the real name. Keyed by the
/// FIRST definition (min row) so a later redefinition never retro-activates
/// earlier uses. External (`#include`d) macros are absent here — they are
/// active from the file's top, since we don't model include ordering.
fn local_macro_activation(tree: &Tree, src: &str) -> HashMap<String, usize> {
    let line_starts = line_start_offsets(src);
    let mut out: HashMap<String, usize> = HashMap::new();
    walk_macro_defs(tree, src.as_bytes(), |name, m, _span| {
        let byte = line_starts.get(m.def_line).copied().unwrap_or(0);
        out.entry(name)
            .and_modify(|b| *b = (*b).min(byte))
            .or_insert(byte);
    });
    out
}

fn compute_splices_inner(
    tree: &Tree,
    src: &str,
    external: &PreExpandedExternal,
    alias_only: bool,
    force_slow: bool,
    expand_region_bodies: bool,
) -> Vec<Splice> {
    let eff = crate::timings::phase("cpp.macro_expand", || {
        build_effective_macros(tree, src, external, alias_only, force_slow)
    });
    if eff.is_empty() {
        return Vec::new();
    }
    // Per the C preprocessor, an object-like `#define` applies only to text
    // AT/AFTER its directive. Uses of a file-local macro name before its own
    // `#define` (re2 `Simplify` → `DontCallSimplify`) must keep the real name.
    let local_activation = local_macro_activation(tree, src);
    let excludes = exclusion_spans(tree, expand_region_bodies);
    // The HARD exclusions (strings/comments/directives) a context-free-safe
    // macro is *never* exempt from — only computed for the wide fallback, where
    // `excludes` additionally holds the conditional-region bodies such a macro
    // MAY expand into. In the default scope the two sets coincide (see the
    // exemption at the exclusion cursor below).
    let narrow = (!expand_region_bodies).then(|| exclusion_spans(tree, true));
    // The expansion-policy flip: leave a use unexpanded when it already parses
    // clean, expand only where leaving it raises `parse_damage` (parse-repair).
    // `error_spans` is that per-use oracle. The alias-salvage mode is exempt —
    // it runs only as the whole-file fallback after the gated expansion still
    // raised damage, and its job is to preserve identifier-alias name
    // indirection on the CLEAN uses the gate would otherwise leave.
    //
    // The expansion-policy flip (`docs/adr/macro-handling.md`, three modes):
    // a function-like macro whose use ALREADY parses as a clean `call_
    // expression` is LEFT unexpanded — the existing sub-return bag path then
    // types the call for free (a function-like macro IS a package-global sub
    // typed from its body). Only function-like uses that DON'T parse as a call
    // (member-block field-slot misparse `DECLARE_DYNAMIC(x)`, statement soup,
    // args-in-declarator) fall through to expansion (parse-repair). Object-like
    // macros are unaffected — their value/type lanes ride edges, and leaving an
    // attribute/declarator macro is a silent misparse the parser doesn't flag.
    let leave_calls = (!alias_only).then(|| clean_call_sites(tree)).unwrap_or_default();
    let bytes = src.as_bytes();
    let mut splices: Vec<Splice> = Vec::new();
    // `excludes` is sorted + disjoint and `start` only advances, so a
    // single cursor over it decides membership in O(1) amortized: drop
    // intervals that end at/before the current word, then the frontier
    // interval is the only one that can contain it.
    let mut ex = 0usize;
    let mut nex = 0usize;
    let mut lc = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_byte(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let word = &src[start..i];
            while ex < excludes.len() && excludes[ex].1 <= start {
                ex += 1;
            }
            let in_exclude = ex < excludes.len() && excludes[ex].0 <= start;
            if in_exclude {
                // A context-independently-safe expansion (empty body → pure byte
                // deletion; see `is_context_free_safe`) stays expandable even
                // inside a conditional-region BODY the wide fallback re-excludes:
                // otherwise a clean `pTHX_` threaded through a `#ifdef` function
                // dies as collateral when a *sibling* macro forced that fallback
                // (`docs/prompt-macro-salvage-scaling.md`). It is still barred
                // from the HARD spans (strings/comments/directives, `narrow`),
                // where no expansion may ever touch bytes. `narrow` is only built
                // for the wide fallback — in the default scope it equals
                // `excludes`, so the exemption is a no-op there.
                let hard = narrow.as_deref().map_or(&excludes[..], |n| n);
                let exempt = eff.get(word).is_some_and(is_context_free_safe)
                    && !span_contains(hard, &mut nex, start);
                if !exempt {
                    continue; // start ∈ [s, e) of the frontier exclude → skip
                }
            }
            // `leave_calls` (sorted, from the same left-to-right tree walk) is
            // consulted with a forward cursor like `excludes`.
            while lc < leave_calls.len() && leave_calls[lc] < start {
                lc += 1;
            }
            let is_clean_call = lc < leave_calls.len() && leave_calls[lc] == start;
            // A reserved keyword is never an expansion candidate, whatever
            // the gathered table says: system headers #define keyword names
            // in config branches this pass doesn't evaluate (`assert.h`'s
            // C-only `static_assert`, lint-era `else`), and rewriting a
            // keyword token corrupts every construct that uses it.
            if is_reserved_keyword(word) {
                continue;
            }
            if let Some(m) = eff.get(word) {
                // A use before its own file-local `#define` predates the
                // definition — leave it unexpanded (C preprocessor position
                // semantics). External macros are absent from the map (always
                // active). `start` and the activation byte are both original
                // coordinates, so the comparison is frame-consistent.
                if local_activation.get(word).is_some_and(|&act| start < act) {
                    continue;
                }
                if m.params.is_some() && is_clean_call {
                    continue; // leave: parses clean as a call → sub-return types it
                }
                match &m.params {
                    None => splices.push(Splice {
                        start,
                        end: i,
                        replacement: m.body.clone(),
                        name: word.to_string(),
                    }),
                    Some(params) => {
                        if let Some((args_end, args)) = scan_call_args(bytes, i) {
                            let replacement = substitute(&m.body, params, &args);
                            splices.push(Splice {
                                start,
                                end: args_end,
                                replacement,
                                name: word.to_string(),
                            });
                            i = args_end;
                        }
                    }
                }
            }
            continue;
        }
        i += 1;
    }
    splices
}

/// From just after a macro name, skip whitespace, require `(`, and scan
/// a balanced paren group; return (end_offset, top-level comma args).
fn scan_call_args(bytes: &[u8], mut j: usize) -> Option<(usize, Vec<String>)> {
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'(' {
        return None;
    }
    let mut depth = 0i32;
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    while j < bytes.len() {
        let c = bytes[j];
        match c {
            b'(' => {
                depth += 1;
                if depth > 1 {
                    cur.push('(');
                }
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    if !cur.trim().is_empty() || !args.is_empty() {
                        args.push(cur.trim().to_string());
                    }
                    return Some((j + 1, args));
                }
                cur.push(')');
            }
            b',' if depth == 1 => {
                args.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c as char),
        }
        j += 1;
    }
    None
}

/// Whole-word substitute each param with its argument in a body.
fn substitute(body: &str, params: &[String], args: &[String]) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_byte(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let word = &body[start..i];
            match params.iter().position(|p| p == word) {
                Some(idx) if idx < args.len() => out.push_str(&args[idx]),
                _ => out.push_str(word),
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Byte ranges the expansion pass must not touch, returned SORTED and
/// COALESCED into maximal disjoint intervals. Query captures arrive in
/// document order but strings/comments nest inside preproc lines, so the
/// raw spans overlap; merging their union lets the caller test membership
/// with a single forward cursor (the words it tests only ever move
/// rightward) instead of scanning every span per word.
/// `expand_region_bodies` selects the exclusion scope: `true` (default) leaves
/// conditional-region BODIES expandable (excluding only the directive/condition
/// tokens); `false` re-excludes the whole region — the pre-widening scope the
/// damage-raising fallback drops back to.
fn exclusion_spans(tree: &Tree, expand_region_bodies: bool) -> Vec<(usize, usize)> {
    let (slot, src) = if expand_region_bodies {
        (&EXCLUDE_Q, EXCLUDE_QUERY)
    } else {
        (&EXCLUDE_Q_WIDE, EXCLUDE_QUERY_WIDE)
    };
    let query = cached_query(slot, &tree.language(), src);
    let mut spans = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), b"" as &[u8]);
    while let Some(m) = it.next() {
        for c in m.captures {
            spans.push((c.node.start_byte(), c.node.end_byte()));
        }
    }
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Start bytes (SORTED) of every function identifier that heads a clean
/// `call_expression` — `f(args)` where the parser committed to a call, not a
/// misparse. This is the per-use "leave" oracle for the expansion flip: a
/// function-like macro use that already parses as a clean call is left
/// unexpanded (the sub-return bag path types it). A function-like macro pasted
/// where a call can't stand — a struct-body field slot (`DECLARE_DYNAMIC(x)` →
/// `field_declaration`), statement soup — never yields a `call_expression`
/// here, so it falls through to expansion (parse-repair). `docs/adr/macro-
/// handling.md`.
fn clean_call_sites(tree: &Tree) -> Vec<usize> {
    let query = cached_query(&CALL_Q, &tree.language(), CALL_QUERY);
    let mut starts = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), b"" as &[u8]);
    while let Some(m) = it.next() {
        for c in m.captures {
            // The capture is the function identifier; its parent is the
            // `call_expression`. A call the parser flagged as broken is not a
            // trustworthy "leave" — let it expand.
            if c.node.parent().is_some_and(|p| !p.has_error()) {
                starts.push(c.node.start_byte());
            }
        }
    }
    starts.sort_unstable();
    starts
}

fn apply(src: &str, splices: &mut [Splice]) -> (String, SpliceMap) {
    splices.sort_by_key(|s| s.start);
    let mut out = String::with_capacity(src.len());
    let mut map = SpliceMap::default();
    let mut prev = 0usize;
    // `shift` tracks `trans = orig + shift` as each applied splice lands,
    // so `ts`/`shift_after` (the binary-search index SpliceMap reads) are
    // built here rather than re-derived on every lookup. Skipped overlaps
    // never touch `shift`, so the index counts only applied edits — exactly
    // what the former linear scan iterated.
    let mut shift: isize = 0;
    for s in splices.iter() {
        if s.start < prev {
            continue; // overlapping (defensive) — skip
        }
        out.push_str(&src[prev..s.start]);
        out.push_str(&s.replacement);
        let nlen = s.replacement.len();
        map.ts.push((s.start as isize + shift) as usize);
        map.edits.push((s.start, s.end, nlen));
        shift += nlen as isize - (s.end - s.start) as isize;
        map.shift_after.push(shift);
        prev = s.end;
    }
    out.push_str(&src[prev..]);
    (out, map)
}

// ===== Member-block macros as roles (`docs/adr/macro-handling.md`) =====
//
// A macro whose body is a field block (`#define BASEOP OP* op_next; …
// op_type:9; …`) pasted STANDALONE into a struct/class body (`struct op {
// BASEOP };`) is a **role** — the shape of a Perl `with`, already modeled as a
// `package_parents` edge. We do NOT expand it: the use is BLANKED in the parse
// view (`struct op { };` parses clean; the original keeps the token, so the
// landed goto-def-on-`BASEOP` macro lane is untouched), ONE synthetic base is
// minted per macro with members parsed from the config-active variant body, and
// a parent edge is added per pasting struct. The existing ancestor walk then
// delivers member resolution / hover / the references splat — no parallel field
// resolution. Roles all the way (rule #10): even a one-member macro is a role.

use crate::file_analysis::Span;

/// One member field parsed from a role macro's body, in ORIGINAL coordinates.
#[derive(Debug, Clone)]
pub struct SynMember {
    pub name: String,
    /// The field-name token span (goto-def target — op.h:55, not the `#define`).
    pub name_span: Span,
    /// The declared type spelling (`PERL_BITFIELD16`) — re-sources the SAME
    /// `TypeName` edge the expanded field would have (hover keeps working).
    pub type_text: String,
    /// The pointer/reference stack (`OP*` → `[Pointer]`), peeled by the SAME
    /// walker the plain-field query lane uses, so a macro-pasted field renders
    /// its `*`s in hover exactly like a directly-declared one (rule #10).
    pub deref_stack: Vec<crate::file_analysis::DerefStep>,
}

/// One synthetic base minted from a member-block macro (`BASEOP`).
#[derive(Debug, Clone)]
pub struct SyntheticBase {
    pub macro_name: String,
    /// Covers the member positions so `scope_at` returns this scope for a
    /// member's point (the type-witness lookup keys on it).
    pub body_scope_span: Span,
    pub members: Vec<SynMember>,
}

/// The member-block analysis for one file: the blanked parse view, the
/// `(struct, macro)` parent edges, and the synthetic bases to inject.
#[derive(Debug, Clone)]
pub struct MemberBlockPlan {
    /// `source` with every confirmed member-block use blanked (length-
    /// preserving, so spans stay in original coordinates). Identical to
    /// `source` when the file has no member-block macros.
    pub blanked_source: String,
    /// `(struct_name, macro_name)` — a `package_parents` edge per pasting
    /// struct. Sorted + deduped for determinism.
    pub edges: Vec<(String, String)>,
    pub bases: Vec<SyntheticBase>,
}

impl MemberBlockPlan {
    fn identity(source: &str) -> Self {
        MemberBlockPlan { blanked_source: source.to_string(), edges: Vec::new(), bases: Vec::new() }
    }
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty() && self.bases.is_empty()
    }
}

/// One class/struct-body member's access region: its own
/// declaration span, and whether it's reachable from OUTSIDE the class
/// (`false` = public). Two-state — `private`/`protected` both fold to
/// non-public; friend declarations and the protected-vs-private distinction
/// are noted as a follow-up, not modeled here.
pub struct AccessRegion {
    pub span: crate::file_analysis::Span,
    pub non_public: bool,
}

/// Walk every `field_declaration_list` in `source`, tracking the current
/// `public:`/`private:`/`protected:` label as a flat linear scan over its
/// direct children (the label applies to every following member until the
/// next label or the body's end — access specifiers are siblings, not
/// nesting, so this can't be a declarative query). `class` bodies default
/// private, `struct` bodies default public — the language's own rule, read
/// off the body's parent node kind rather than guessed.
pub fn access_regions(parser: &mut tree_sitter::Parser, source: &str) -> Vec<AccessRegion> {
    let Some(tree) = parser.parse(source, None) else { return Vec::new() };
    let src = source.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "field_declaration_list" {
            let mut non_public =
                node.parent().is_some_and(|p| p.kind() == "class_specifier");
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                if child.kind() == "access_specifier" {
                    non_public = match child.utf8_text(src) {
                        Ok("public") => false,
                        Ok("private") | Ok("protected") => true,
                        _ => non_public,
                    };
                    continue;
                }
                out.push(AccessRegion {
                    span: crate::file_analysis::Span {
                        start: child.start_position(),
                        end: child.end_position(),
                    },
                    non_public,
                });
            }
        }
        let mut c = node.walk();
        stack.extend(node.children(&mut c));
    }
    out
}

/// Classify member-block macros by struct-body usage, blank the uses, and mint
/// the synthetic bases + parent edges. Runs BEFORE the expansion transform: the
/// blanked source feeds it, so a role macro is never expanded. `parser` is
/// reused sequentially (each `parse(None)` is independent).
pub fn plan_member_blocks(parser: &mut tree_sitter::Parser, source: &str) -> MemberBlockPlan {
    let Some(tree) = parser.parse(source, None) else { return MemberBlockPlan::identity(source) };
    let src = source.as_bytes();

    // Candidates: file-local macros whose body IS a field block (parses clean as
    // `struct _ { body }` with ≥1 NAMED field). The body-parse is the
    // discriminator — an alias body (`#define BASEOP BASEOP_DEFINITION`) has no
    // named field, so it never qualifies. Cheap `;` pre-gate first. A candidate
    // is OBJECT-like (pasted bare: `struct op { BASEOP };`) and/or FUNCTION-like
    // (pasted as a call: `struct sv { _SV_HEAD(void*); }`, perl5's parametric
    // member block); the paste-shape governs use detection + blanking below.
    let variants = collect_macro_variants(&tree, src);
    let mut candidates: Vec<String> = Vec::new();
    let mut func_like: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut obj_like: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, defs) in &variants {
        let is_obj = defs.iter().any(|m| m.params.is_none() && m.body.contains(';') && body_is_field_block(parser, &m.body));
        let is_func = defs.iter().any(|m| m.params.is_some() && m.body.contains(';') && body_is_field_block(parser, &m.body));
        if is_obj || is_func {
            candidates.push(name.clone());
            if is_obj { obj_like.insert(name.clone()); }
            if is_func { func_like.insert(name.clone()); }
        }
    }
    if candidates.is_empty() {
        return MemberBlockPlan::identity(source);
    }
    candidates.sort_unstable(); // determinism: candidate processing order

    // Every member-block paste of a candidate (not inside a string / comment /
    // preprocessor line). An object-like macro pastes bare (`BASEOP`); a
    // function-like one pastes as a call whose argument list is part of the paste
    // (`_SV_HEAD(void*)` — the whole call is blanked). The per-candidate
    // parse-damage gate below reverts a use that (surprisingly) wasn't one.
    let cand_set: std::collections::HashSet<&str> = candidates.iter().map(String::as_str).collect();
    // Member-block macro paste is orthogonal to region-body expansion; keep the
    // wide exclusion so this consumer's behavior is unchanged by slice 1.
    let excludes = exclusion_spans(&tree, false);
    let mut uses: Vec<(usize, usize, String)> = Vec::new(); // (start, end, macro)
    {
        let mut ex = 0usize;
        let mut i = 0usize;
        while i < src.len() {
            if is_ident_byte(src[i]) && (i == 0 || !is_ident_byte(src[i - 1])) {
                let start = i;
                while i < src.len() && is_ident_byte(src[i]) {
                    i += 1;
                }
                let word = &source[start..i];
                while ex < excludes.len() && excludes[ex].1 <= start {
                    ex += 1;
                }
                let excluded = ex < excludes.len() && excludes[ex].0 <= start;
                let mut j = i;
                while j < src.len() && src[j].is_ascii_whitespace() {
                    j += 1;
                }
                let is_call = j < src.len() && src[j] == b'(';
                if !excluded {
                    if let Some(&c) = cand_set.get(word) {
                        // Function-like paste: the use spans through the balanced
                        // argument list so blanking clears `_SV_HEAD(void*)` whole.
                        if is_call && func_like.contains(c) {
                            if let Some(close) = balanced_paren_end(src, j) {
                                uses.push((start, close, c.to_string()));
                            }
                        } else if !is_call && obj_like.contains(c) {
                            uses.push((start, i, c.to_string()));
                        }
                    }
                }
                continue;
            }
            i += 1;
        }
    }

    // Per-candidate blank + validate: blanking a candidate's uses must not raise
    // parse damage. A genuine member-block paste parses corrupt unexpanded and
    // clean blanked → damage drops; a candidate used somewhere as an expression
    // would raise damage → that candidate is rejected (unblanked).
    //
    // Blank atop a comment-neutralized copy: a `\`-continued define with a
    // trailing block comment ends `preproc_arg` at the comment, and tree-sitter
    // reparses the rest of the body as top-level code — corrupting any
    // declaration ADJACENT to the define (an SV struct right after `_SV_HEAD`
    // became an ERROR node, so its member-block parent edge never formed). The
    // neutralization is length-preserving, so `uses` offsets and every span stay
    // in original coordinates; the baseline damage comes from the same view.
    let neutralized = neutralize_directive_comments(source);
    let mut blanked = neutralized.clone();
    let mut current_damage = parser
        .parse(&neutralized, None)
        .map(|t| parse_damage(t.root_node()))
        .unwrap_or_else(|| parse_damage(tree.root_node()));
    let mut confirmed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cand in &candidates {
        let tentative = blank_ranges(&blanked, uses.iter().filter(|(_, _, m)| m == cand).map(|(s, e, _)| (*s, *e)));
        let Some(tt) = parser.parse(&tentative, None) else { continue };
        let dmg = parse_damage(tt.root_node());
        if dmg <= current_damage {
            blanked = tentative;
            current_damage = dmg;
            confirmed.insert(cand.clone());
        }
    }
    if confirmed.is_empty() {
        return MemberBlockPlan::identity(source);
    }

    // Parent edges: reparse the blanked (now clean) source and read each blanked
    // use's enclosing struct/class/union name. The struct → macro edge is
    // `package_parents`; the ancestor walk carries everything from there.
    let mut edges: Vec<(String, String)> = Vec::new();
    if let Some(clean) = parser.parse(&blanked, None) {
        for (start, _end, macro_name) in &uses {
            if !confirmed.contains(macro_name) {
                continue;
            }
            if let Some(struct_name) = enclosing_aggregate_name(clean.root_node(), &blanked, *start) {
                edges.push((struct_name, macro_name.clone()));
            }
        }
    }
    edges.sort();
    edges.dedup();

    // One synthetic base per confirmed macro: members parsed from the config-
    // active field-block variant, positioned at the real body tokens.
    let cfg = known_config(&variants);
    let mut bases: Vec<SyntheticBase> = Vec::new();
    for macro_name in &candidates {
        if !confirmed.contains(macro_name) {
            continue;
        }
        if let Some(base) = synth_base(parser, source, &tree, macro_name, &cfg) {
            bases.push(base);
        }
    }

    MemberBlockPlan { blanked_source: blanked, edges, bases }
}

/// Length-preserving blank (spaces) of each `[start,end)` byte range. Spans
/// elsewhere in the file are untouched, so extraction stays in original coords.
fn blank_ranges(src: &str, ranges: impl Iterator<Item = (usize, usize)>) -> String {
    let mut out = src.to_string();
    // SAFETY: only ASCII spaces written over ASCII identifier bytes — length-
    // preserving and UTF-8-valid.
    let ob = unsafe { out.as_bytes_mut() };
    let n = ob.len();
    for (s, e) in ranges {
        for b in &mut ob[s.min(n)..e.min(n)] {
            *b = b' ';
        }
    }
    out
}

/// The struct-body text for a field-block macro: `\`→space, plus a single
/// normalized trailing `;`. A function-like member-block body omits the final
/// `;` (it comes from the paste — `_SV_HEAD(void*);`), so the last field would
/// otherwise fail to parse. Only trailing bytes change, so field offsets before
/// it map 1:1 back to source for member positioning.
fn field_block_inner(body: &str) -> String {
    let b = body.replace('\\', " ");
    let trimmed = b.trim_end();
    let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed);
    format!("{trimmed}; ")
}

/// Does `body` parse as a struct field block — `struct _ { body }` clean with
/// ≥1 NAMED field? The discriminator that promotes a macro to a member-block
/// role (an alias body like `BASEOP_DEFINITION` has no named field → not one).
fn body_is_field_block(parser: &mut tree_sitter::Parser, body: &str) -> bool {
    let synth = format!("struct __mb__ {{ {} }};", field_block_inner(body));
    let Some(tree) = parser.parse(&synth, None) else { return false };
    let dmg = parse_damage(tree.root_node());
    let src = synth.as_bytes();
    let mut found = false;
    for_each_field_declaration(tree.root_node(), &mut |fd| {
        if declarator_field_name(fd, src).is_some() {
            found = true;
        }
    });
    dmg == 0 && found
}

/// Mint the synthetic base for `macro_name`: its config-active field-block
/// variant's members, positioned at the real `#define` body tokens.
fn synth_base(
    parser: &mut tree_sitter::Parser,
    source: &str,
    tree: &Tree,
    macro_name: &str,
    cfg: &crate::cpp_macro_model::KnownConfig,
) -> Option<SyntheticBase> {
    // The field-block variants of this macro, with their body node's original
    // start (byte + point). Config-active first (reachability rank), ties by
    // source order — the SAME pick the goto-def / hover-leaf lanes use.
    let mut sites = field_block_variant_sites(parser, tree, source, macro_name);
    if sites.is_empty() {
        return None;
    }
    sites.sort_by_key(|s| crate::cpp_macro_model::classify(&s.guards, cfg).rank());
    let site = &sites[0];

    // Map a byte offset within the body back to an original Point by advancing
    // the body's start Point over the intervening bytes (newlines counted). The
    // body text is verbatim from `source`, so offsets are 1:1.
    let body = site.body.as_bytes(); // raw body bytes as they appear in source
    let point_at = |off: usize| advance_point(site.start_point, &body[..off.min(body.len())]);

    let mut members: Vec<SynMember> = Vec::new();
    // Parse `struct _ { body }` and read each named field; `\`→space keeps the
    // body byte-length identical so token offsets map straight back.
    let prefix = "struct __mb__ { ";
    let synth = format!("{prefix}{} }};", field_block_inner(&site.body));
    let synth_tree = parser.parse(&synth, None)?;
    let sbytes = synth.as_bytes();
    let mut fields: Vec<SynMember> = Vec::new();
    for_each_field_declaration(synth_tree.root_node(), &mut |fd| {
        let Some(name_node) = declarator_field_name(fd, sbytes) else { return };
        let Some(type_node) = fd.child_by_field_name("type") else { return };
        let name = name_node.utf8_text(sbytes).unwrap_or("").to_string();
        let type_text = type_node.utf8_text(sbytes).unwrap_or("").trim().to_string();
        if name.is_empty() || type_text.is_empty() {
            return;
        }
        // Pointer-ness via the SAME peel the plain-field query lane runs; a
        // shape peel can't model (function-pointer field) degrades to no stack,
        // exactly as the query lane does — parity either way.
        let deref_stack = fd
            .child_by_field_name("declarator")
            .and_then(|d| crate::query_extract::peel(d, &crate::query_extract::C_FIELD_DECL_PEEL, sbytes))
            .map(|(_, stack, _)| stack)
            .unwrap_or_default();
        // synth byte → body byte (drop the prefix) → original Point.
        let ns = name_node.start_byte().saturating_sub(prefix.len());
        let ne = name_node.end_byte().saturating_sub(prefix.len());
        let name_span = Span { start: point_at(ns), end: point_at(ne) };
        fields.push(SynMember { name, name_span, type_text, deref_stack });
    });
    // Source order (the DFS visit order isn't guaranteed) — deterministic.
    fields.sort_by_key(|m| (m.name_span.start.row, m.name_span.start.column));
    members.append(&mut fields);
    if members.is_empty() {
        return None;
    }
    let body_scope_span = Span {
        start: site.start_point,
        end: advance_point(site.start_point, body),
    };
    Some(SyntheticBase { macro_name: macro_name.to_string(), body_scope_span, members })
}

/// A field-block `#define` variant of a macro: its guard trail plus the body's
/// original bytes and start Point (for positioning members).
struct VariantSite {
    guards: Vec<String>,
    /// The body's ORIGINAL bytes verbatim (offsets map 1:1 for positioning).
    body: String,
    start_point: tree_sitter::Point,
}

/// Every `#define macro_name` whose body is a field block, with its body node's
/// ORIGINAL span (so minted members land on the real tokens).
fn field_block_variant_sites(
    parser: &mut tree_sitter::Parser,
    tree: &Tree,
    source: &str,
    macro_name: &str,
) -> Vec<VariantSite> {
    let src = source.as_bytes();
    let query = cached_query(&MACRO_DEF_Q, &tree.language(), MACRO_DEF_QUERY);
    let names: Vec<&str> = query.capture_names().to_vec();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(query, tree.root_node(), src);
    let mut out = Vec::new();
    while let Some(m) = it.next() {
        let mut name_node: Option<tree_sitter::Node> = None;
        let mut body_node: Option<tree_sitter::Node> = None;
        for c in m.captures {
            // Object- and function-like field-block macros both mint a base; the
            // params of a function-like one (`_SV_HEAD(ptrtype)`) are absorbed by
            // the paste, so only name + body matter for positioning members.
            match names[c.index as usize] {
                "oname" | "fname" => name_node = Some(c.node),
                "obody" | "fbody" => body_node = Some(c.node),
                _ => {}
            }
        }
        let (Some(nn), Some(bn)) = (name_node, body_node) else { continue };
        if nn.utf8_text(src).unwrap_or("") != macro_name {
            continue;
        }
        // Raw logical body, not the comment-truncated `preproc_arg` span, so
        // every field is present and byte offsets still map 1:1 to source.
        let body_text = raw_macro_body(source, bn.start_byte());
        if !body_text.contains(';') || !body_is_field_block(parser, body_text) {
            continue;
        }
        out.push(VariantSite {
            guards: guard_trail(nn, src),
            body: body_text.to_string(),
            start_point: bn.start_position(),
        });
    }
    out
}

/// The reachability config for variant ranking — every `#define` name is a
/// knob (`universe`); unconditional ones are known ON (`defined`) ∪ the
/// toolchain's predefined macros.
fn known_config(variants: &BTreeMap<String, Vec<Macro>>) -> crate::cpp_macro_model::KnownConfig {
    let mut defined = std::collections::HashSet::new();
    let mut universe = std::collections::HashSet::new();
    for (name, defs) in variants {
        universe.insert(name.clone());
        if defs.iter().any(|m| m.guards.is_empty()) {
            defined.insert(name.clone());
        }
    }
    known_config_with_toolchain(defined, universe)
}

/// Fold the toolchain's predefined macros (`__GNUC__`, `__x86_64__`, …) into a
/// reachability config as known-ON knobs. The ONE seeding point for build-side
/// variant selection AND goto-def/hover navigation (`ranked_macro_variants`),
/// so minting and navigation can't disagree on which config arm is Active.
pub fn known_config_with_toolchain(
    mut defined: std::collections::HashSet<String>,
    mut universe: std::collections::HashSet<String>,
) -> crate::cpp_macro_model::KnownConfig {
    if let Some(tc) = toolchain_info() {
        seed_predefined(&mut defined, &mut universe, &tc.predefined_macros);
    }
    crate::cpp_macro_model::KnownConfig::new(defined, universe)
}

/// Predefined macros are unconditional defines: ON in `defined`, known knobs
/// in `universe`. Split from the `toolchain_info()` wrapper so the seeding is
/// unit-testable without a compiler probe.
pub fn seed_predefined(
    defined: &mut std::collections::HashSet<String>,
    universe: &mut std::collections::HashSet<String>,
    predefined: &[(String, String)],
) {
    for (name, _) in predefined {
        defined.insert(name.clone());
        universe.insert(name.clone());
    }
}

/// Advance a Point over `bytes` (newlines bump the row, reset the column).
fn advance_point(mut p: tree_sitter::Point, bytes: &[u8]) -> tree_sitter::Point {
    for &b in bytes {
        if b == b'\n' {
            p.row += 1;
            p.column = 0;
        } else {
            p.column += 1;
        }
    }
    p
}

/// Visit every `field_declaration` node in a tree (DFS).
fn for_each_field_declaration<'a>(root: tree_sitter::Node<'a>, f: &mut impl FnMut(tree_sitter::Node<'a>)) {
    let mut stack = vec![root];
    let mut cur = root.walk();
    while let Some(n) = stack.pop() {
        if n.kind() == "field_declaration" {
            f(n);
        }
        for ch in n.children(&mut cur) {
            stack.push(ch);
        }
    }
}

/// The field-name identifier of a `field_declaration`, descending pointer /
/// reference / function / array / parenthesized declarators to the leaf. Prefers
/// the `declarator` field edge so a function-pointer field's PARAMETER names
/// (`op_ppaddr(pTHX)`) are never mistaken for the field.
fn declarator_field_name<'a>(fd: tree_sitter::Node<'a>, src: &[u8]) -> Option<tree_sitter::Node<'a>> {
    let decl = fd.child_by_field_name("declarator")?;
    descend_declarator_name(decl, src)
}

fn descend_declarator_name<'a>(node: tree_sitter::Node<'a>, src: &[u8]) -> Option<tree_sitter::Node<'a>> {
    match node.kind() {
        "field_identifier" | "identifier" => Some(node),
        _ => {
            if let Some(d) = node.child_by_field_name("declarator") {
                if let Some(n) = descend_declarator_name(d, src) {
                    return Some(n);
                }
            }
            let mut cur = node.walk();
            for ch in node.named_children(&mut cur) {
                if matches!(ch.kind(), "parameter_list" | "argument_list") {
                    continue;
                }
                if let Some(n) = descend_declarator_name(ch, src) {
                    return Some(n);
                }
            }
            None
        }
    }
}

/// The name of the smallest struct/class/union whose body contains `byte` —
/// the struct a blanked member-block use was pasted into.
fn enclosing_aggregate_name(root: tree_sitter::Node, src: &str, byte: usize) -> Option<String> {
    let mut best: Option<(usize, String)> = None; // (span size, name)
    let mut stack = vec![root];
    let mut cur = root.walk();
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "struct_specifier" | "class_specifier" | "union_specifier") {
            if let Some(body) = n.child_by_field_name("body") {
                if body.start_byte() <= byte && byte < body.end_byte() {
                    if let Some(name) = n.child_by_field_name("name") {
                        let size = n.end_byte() - n.start_byte();
                        if best.as_ref().is_none_or(|(bs, _)| size < *bs) {
                            best = Some((size, name.utf8_text(src.as_bytes()).unwrap_or("").to_string()));
                        }
                    }
                }
            }
        }
        for ch in n.children(&mut cur) {
            stack.push(ch);
        }
    }
    best.map(|(_, name)| name).filter(|n| !n.is_empty())
}

#[cfg(test)]
#[path = "cpp_reparse_tests.rs"]
mod tests;
