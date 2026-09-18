//! Member-block macros as roles (`docs/adr/macro-handling.md`): planning
//! synthetic bases (`SynMember`/`SyntheticBase`/`MemberBlockPlan`) so a
//! field-block macro pasted into a class body extracts as an inherited role.

use super::*;

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

use crate::model::file_analysis::Span;

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
    pub deref_stack: Vec<crate::model::file_analysis::DerefStep>,
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
    pub span: crate::model::file_analysis::Span,
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
                    span: crate::model::file_analysis::Span {
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
pub(super) fn blank_ranges(src: &str, ranges: impl Iterator<Item = (usize, usize)>) -> String {
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
    cfg: &crate::build::cpp_macro_model::KnownConfig,
) -> Option<SyntheticBase> {
    // The field-block variants of this macro, with their body node's original
    // start (byte + point). Config-active first (reachability rank), ties by
    // source order — the SAME pick the goto-def / hover-leaf lanes use.
    let mut sites = field_block_variant_sites(parser, tree, source, macro_name);
    if sites.is_empty() {
        return None;
    }
    sites.sort_by_key(|s| crate::build::cpp_macro_model::classify(&s.guards, cfg).rank());
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
    // Pointer-ness through the SAME document the plain-field query lane reads:
    // its `@deref` captures over this tree, peeled by the one walk. A shape the
    // peel can't model (a function-pointer field) degrades to no stack, exactly
    // as the query lane does — parity either way.
    let derefs = crate::build::query_extract::DerefCaps::of_tree(
        &synth_tree,
        sbytes,
        &crate::build::packs::cpp_pack(),
    );
    let mut fields: Vec<SynMember> = Vec::new();
    for_each_field_declaration(synth_tree.root_node(), &mut |fd| {
        let Some(name_node) = declarator_field_name(fd, sbytes) else { return };
        let Some(type_node) = fd.child_by_field_name("type") else { return };
        let name = name_node.utf8_text(sbytes).unwrap_or("").to_string();
        let type_text = type_node.utf8_text(sbytes).unwrap_or("").trim().to_string();
        if name.is_empty() || type_text.is_empty() {
            return;
        }
        let deref_stack = fd
            .child_by_field_name("declarator")
            .and_then(|d| derefs.peel(d, sbytes))
            .map(|c| c.stack)
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
fn known_config(variants: &BTreeMap<String, Vec<Macro>>) -> crate::build::cpp_macro_model::KnownConfig {
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
) -> crate::build::cpp_macro_model::KnownConfig {
    if let Some(tc) = toolchain_info() {
        seed_predefined(&mut defined, &mut universe, &tc.predefined_macros);
    }
    crate::build::cpp_macro_model::KnownConfig::new(defined, universe)
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

pub(super) fn descend_declarator_name<'a>(node: tree_sitter::Node<'a>, src: &[u8]) -> Option<tree_sitter::Node<'a>> {
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
pub(super) fn enclosing_aggregate_name(root: tree_sitter::Node, src: &str, byte: usize) -> Option<String> {
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
