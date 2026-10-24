//! The expansion pass itself: `preprocess_with`, the two-tier
//! `EffectiveMacros` view, splice computation/substitution, and splice
//! application producing the rewritten source + `SpliceMap`.

use super::*;

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
pub(super) fn preprocess_with_mode(
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
pub(super) fn preprocess_with_mode_inner(
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
pub(super) fn compute_splices(
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

/// Where each file-LOCAL macro is defined and undefined: the line start
/// bytes of its `#define`s and its `#undef`s. A use STRICTLY BEFORE the
/// first `#define` predates the definition and, per the C preprocessor, must
/// NOT expand: `#define Simplify DontCallSimplify` at re2/simplify.cc:201
/// protects the out-of-line def `Regexp* Regexp::Simplify()` at :180 and the
/// call at :31, which both keep the real name. A use after an `#undef` keeps
/// its name the same way.
fn local_macro_lines(tree: &Tree, src: &str) -> HashMap<String, (Vec<usize>, Vec<usize>)> {
    let line_starts = line_start_offsets(src);
    let mut out: HashMap<String, (Vec<usize>, Vec<usize>)> = HashMap::new();
    walk_macro_defs(tree, src.as_bytes(), |name, m, _span| {
        out.entry(name).or_default().0.push(line_starts.get(m.def_line).copied().unwrap_or(0));
    });
    walk_macro_undefs(tree, src.as_bytes(), |name, node| {
        out.entry(name).or_default().1.push(node.start_byte());
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
    let eff = crate::util::timings::phase("cpp.macro_expand", || {
        build_effective_macros(tree, src, external, alias_only, force_slow)
    });
    if eff.is_empty() {
        return Vec::new();
    }
    // Per the C preprocessor, an object-like `#define` applies only to text
    // AT/AFTER its directive and until an `#undef`. Uses of a file-local
    // macro name before its own `#define` (re2 `Simplify` →
    // `DontCallSimplify`) must keep the real name.
    // The same holds for a gathered macro: it is defined from the `#include`
    // line that brings it in, until one that undefines it.
    let local_lines = local_macro_lines(tree, src);
    let include_starts: Vec<usize> = include_lines(src).into_iter().map(|(b, _)| b).collect();
    // `None`: the name is no macro this file or its closure defines.
    let spans: std::cell::RefCell<HashMap<String, Option<Vec<(usize, Option<usize>, ())>>>> = Default::default();
    let defined_at = |word: &str, at: usize| -> Option<bool> {
        let mut spans = spans.borrow_mut();
        if !spans.contains_key(word) {
            let (defs, undefs) = local_lines.get(word).map_or((&[][..], &[][..]), |(d, u)| (&d[..], &u[..]));
            let gathered = external.gathered_spans(word, &include_starts);
            let of = (!defs.is_empty() || gathered.is_some()).then(|| {
                definition_spans(
                    defs.iter().map(|&from| (from, ())),
                    gathered.into_iter().flatten().map(|(from, until, _)| (from, until, ())),
                    undefs,
                    &external.undefined_lines(word, &include_starts),
                )
            });
            spans.insert(word.to_string(), of);
        }
        spans[word].as_ref().map(|of| of.iter().any(|&(from, until, ())| definition_counts_at(from, until, at)))
    };
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
                // A use before its `#define` (or the `#include` bringing it
                // in), or after an `#undef`, is no macro use — leave it
                // unexpanded (C preprocessor position semantics). `start` and
                // the directive bytes are both original coordinates, so the
                // comparison is frame-consistent.
                if defined_at(word, start) == Some(false) {
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
pub(super) fn exclusion_spans(tree: &Tree, expand_region_bodies: bool) -> Vec<(usize, usize)> {
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

pub(super) fn apply(src: &str, splices: &mut [Splice]) -> (String, SpliceMap) {
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
