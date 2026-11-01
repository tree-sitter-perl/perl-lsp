//! The per-file collection machinery under the walks: `collect_from_analysis`,
//! target matching (`symbol_defines_target`), `references_mask_for`, and the
//! raw-word / pack def-path helpers.
use super::*;
use crate::model::file_analysis::MemberKey;

/// The identifier under `point` in `source`, or `None` if the cursor is not
/// on a `[A-Za-z0-9_]` word. Byte-scan (macros vanish from the analysis under
/// the expand-and-reparse policy, so the raw word is the reliable key).
pub fn word_at_point(source: &str, point: tree_sitter::Point) -> Option<&str> {
    let cursor = crate::build::cursor_sentinel::point_to_byte(source, point);
    let b = source.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if cursor > b.len() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && is_id(b[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < b.len() && is_id(b[end]) {
        end += 1;
    }
    (start < end).then(|| &source[start..end])
}

/// Is the identifier at `point` CALL-SHAPED — its token immediately followed
/// (skipping whitespace) by `(`? The C preprocessor expands a function-like
/// macro ONLY at this shape, so a PARENLESS token (`OP** p`, the `OP` in a
/// `typedef`) is never a function-like macro's use and a `#define OP(p)` must
/// not claim it. Object-like macros claim regardless (they expand at any
/// occurrence). This is the SITE half of the shape gate; the def's
/// `params.is_some()` is the candidate half.
pub(crate) fn token_is_call_shaped(source: &str, point: tree_sitter::Point) -> bool {
    let cursor = crate::build::cursor_sentinel::point_to_byte(source, point);
    let b = source.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if cursor > b.len() {
        return false;
    }
    let mut end = cursor;
    while end < b.len() && is_id(b[end]) {
        end += 1;
    }
    while end < b.len() && b[end].is_ascii_whitespace() {
        end += 1;
    }
    end < b.len() && b[end] == b'('
}

/// The full `Base<...>` spelling at a type ref: `span` covers the base token;
/// when `<` follows immediately, extend to the balanced `>` and canonicalize
/// (`canonical_template_spelling` — the identity key specs are filed under).
/// `None` for plain type refs, unbalanced brackets, or a statement boundary
/// before the close (a stray comparison, not template args).
pub(super) fn template_instance_spelling(source: &str, span: Span) -> Option<String> {
    let start = crate::build::cursor_sentinel::point_to_byte(source, span.start);
    let mut i = crate::build::cursor_sentinel::point_to_byte(source, span.end);
    let b = source.as_bytes();
    if i >= b.len() || b[i] != b'<' {
        return None;
    }
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(crate::model::file_analysis::canonical_template_spelling(
                        &source[start..=i],
                    ));
                }
            }
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Is `s` addressable as `owner::<its name>` — the owner-membership predicate
/// shared by owner-anchored goto-def (`member_def_location`) and the
/// qualified-completion gather (`complete_pack_qualified`), so "resolvable"
/// and "offered" never drift apart. Methods/subs key by package; a data
/// member (or enum constant) must be the owner's OWN content, not a sub-body
/// local carrying the owner as sticky package.
/// Membership of `s` in an owner set already expanded through
/// inline-namespace transparency (`pack_inline_owner_set`). The set is passed
/// in — never a single raw package — so goto-def's owner lookup
/// (`member_def_location`) and completion agree with the references gate that
/// a symbol filed under an `inline namespace head` satisfies a query keyed on
/// its transparent parent `absl`.
pub(super) fn pack_member_of(
    fa: &crate::model::file_analysis::FileAnalysis,
    s: &crate::model::file_analysis::Symbol,
    owners: &[String],
) -> bool {
    let in_owners = |p: Option<&str>| p.is_some_and(|p| owners.iter().any(|o| o == p));
    match s.kind {
        SymKind::Method | SymKind::Sub => in_owners(s.package.as_deref()),
        SymKind::Variable | SymKind::Field | SymKind::Enumerator => {
            if !fa.symbol_is_class_content(s) {
                return false;
            }
            if in_owners(s.package.as_deref()) {
                return true;
            }
            // Unscoped-enum leak: `dynamic::STRING` / `level::info`
            // where the enumerator's enum is nested in a class OR
            // namespace `owner`. C++ makes an unscoped enum's
            // enumerators members of EVERY enclosing named scope,
            // addressable by that scope's name — but extraction files
            // the enumerator under its tightest container (the enum),
            // so the direct package match above misses the outer
            // scope. Bridge it structurally: the enumerator's span
            // lives inside a container symbol named `owner`
            // (span-contained, and not the enumerator itself). Works
            // whether the scope is a struct (`dynamic`) or a namespace
            // (`level`), without depending on how either tags package.
            matches!(s.kind, SymKind::Enumerator)
                && fa.symbols().iter().any(|c| {
                    owners.iter().any(|o| o == &c.name)
                        && c.span != s.span
                        && (c.span.start.row, c.span.start.column)
                            <= (s.span.start.row, s.span.start.column)
                        && (s.span.end.row, s.span.end.column)
                            <= (c.span.end.row, c.span.end.column)
                })
        }
        _ => false,
    }
}

/// `owner` plus every inline namespace nested under it (transitively), per
/// C++'s inline-namespace transparency: `namespace fmt { inline namespace
/// v11 { ... } }` makes `v11`'s members addressable as `fmt::` members.
/// Extraction tags inline namespaces with the "inline" attribute; a plain
/// nested namespace never joins the set (its members need their own
/// qualifier).
pub(super) fn pack_inline_owner_set(fa: &crate::model::file_analysis::FileAnalysis, owner: &str) -> Vec<String> {
    let mut owners = vec![owner.to_string()];
    loop {
        let mut grew = false;
        for s in fa.symbols() {
            if s.kind == SymKind::Package
                && s.flags.contains(crate::model::file_analysis::SymbolFlags::INLINE)
                && s.package.as_deref().is_some_and(|p| owners.iter().any(|o| o == p))
                && !owners.contains(&s.name)
            {
                owners.push(s.name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    owners
}

/// The `::`-qualifier owning the identifier under `point` — `dynamic` for the
/// cursor anywhere in `STRING` of `dynamic::STRING`. Walks back to the token
/// start (like `word_at_point`), then scans a leading `::` scope.
/// `None` when the token has no leading `::` scope.
pub(crate) fn qualifier_at_point(source: &str, point: tree_sitter::Point) -> Option<&str> {
    let cursor = crate::build::cursor_sentinel::point_to_byte(source, point);
    let b = source.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if cursor > b.len() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && is_id(b[start - 1]) {
        start -= 1;
    }
    if start < 2 || !source.is_char_boundary(start) || &source[start - 2..start] != "::" {
        return None;
    }
    let e = start - 2;
    let mut s = e;
    while s > 0 && is_id(b[s - 1]) {
        s -= 1;
    }
    (s < e).then(|| &source[s..e])
}

/// Every `#define` of `word` across the origin file + the cached modules,
/// ranked config-active first by the SAME total order goto-def and hover both
/// consume (`docs/adr/macro-handling.md`): reachability rank, then
/// (path, row, col) so the winner is deterministic across processes (the
/// cache iterates in randomized DashMap order). Empty when `word` names no
/// macro. This is the one place the variant set is gathered +
/// reachability-classified — `definitions()` returns all of them (never
/// pruned), hover walks the top one's alias chain to its leaf.
pub(crate) fn ranked_macro_variants(
    analysis: &FileAnalysis,
    word: &str,
    origin_key: &FileKey,
    module_index: &dyn CrossFileLookup,
) -> Vec<(crate::model::file_analysis::MacroDef, FileKey, crate::build::cpp_macro_model::Reachability)> {
    use crate::build::cpp_macro_model::classify;
    use crate::model::file_analysis::{CondArms, CondTerm, MacroDef};
    use std::collections::HashSet;

    // One pass over every cached module + this file: collect the def sites for
    // `word` (config variants live in different headers — win32.h vs perl.h; we
    // keep them ALL, never the last-writer only) AND the reachability config
    // (the whole macro universe). Enumerating the cache directly is robust to a
    // cold reverse index — `modules_with_symbol` can be empty before it warms.
    let mut sites: Vec<(MacroDef, FileKey, Vec<CondTerm>)> = Vec::new();
    let mut seen: HashSet<(PathBuf, usize, usize)> = HashSet::new();
    let mut defines = crate::build::cpp_macro_model::DefineSites::default();
    let mut push = |m: &MacroDef, k: &FileKey, arms: &CondArms, sites: &mut Vec<(MacroDef, FileKey, Vec<CondTerm>)>| {
        let key = (key_for_sort(k), m.selection_span.start.row, m.selection_span.start.column);
        if seen.insert(key) {
            sites.push((m.clone(), k.clone(), arms.trail(m.arm).to_vec()));
        }
    };
    for m in &analysis.pack.macro_defs {
        defines.add(&m.name, analysis.pack.cond_arms.trail(m.arm));
        if m.name == word {
            push(m, origin_key, &analysis.pack.cond_arms, &mut sites);
        }
    }
    // Per-FILE sweep: the name-keyed cache view both repeats files and hides
    // a file that lost every name tie.
    module_index.for_each_cached_file(&mut |cached| {
        let file_key = FileKey::Path(cached.path.clone());
        for m in &cached.analysis.pack.macro_defs {
            defines.add(&m.name, cached.analysis.pack.cond_arms.trail(m.arm));
            if m.name == word {
                push(m, &file_key, &cached.analysis.pack.cond_arms, &mut sites);
            }
        }
    });

    if sites.is_empty() {
        return Vec::new();
    }

    // Toolchain predefined macros (`__GNUC__`, …) are ON here exactly as they
    // are in build-side variant selection — navigation and minting share the
    // one constructor so they can't disagree on which arm is Active. The
    // asker's own toolchain: a `.c` file ranks with `__cplusplus` undefined.
    let origin_path = match origin_key {
        FileKey::Path(p) => Some(p.clone()),
        FileKey::Url(u) => u.to_file_path().ok(),
    };
    let toolchain = match origin_path {
        Some(p) => crate::build::cpp_includes::unit_toolchain(&p),
        None => crate::build::cpp_toolchain::Toolchain::for_lang(crate::build::cpp_toolchain::Lang::Cpp),
    };
    let mut cfg = crate::build::cpp_reparse::unit_config(defines, &toolchain);
    // `#ifndef X … #define X …` guards a definition on its OWN
    // not-yet-defined-ness, so X's own name must not count as ON when ranking
    // X's variants — else every such arm reads as unreachable. General over
    // the pattern, not a per-name rule.
    cfg.defined.remove(word);

    // Rank, active-first. Never prune — a lower-ranked (e.g. win32) def stays,
    // labeled. The secondary (path, line, col) key is a TOTAL order so the
    // result is deterministic across processes.
    let mut ranked: Vec<(MacroDef, FileKey, _)> = sites
        .into_iter()
        .map(|(m, k, trail)| {
            let r = classify(&trail, &cfg);
            (m, k, r)
        })
        .collect();
    ranked.sort_by(|(ma, ka, ra), (mb, kb, rb)| {
        ra.rank()
            .cmp(&rb.rank())
            .then_with(|| key_for_sort(ka).cmp(&key_for_sort(kb)))
            .then_with(|| ma.selection_span.start.row.cmp(&mb.selection_span.start.row))
            .then_with(|| ma.selection_span.start.column.cmp(&mb.selection_span.start.column))
    });
    ranked
}

/// Resolve a pack-language symbol NAME (a delegate callee, a free function) to
/// its def location — local symbols and the cross-file index, preferring a
/// DEFINITION over a prototype: a definition's body mints a scope spanning
/// the symbol (the universal `(function_definition) @scope`), a declaration
/// doesn't, so `fix_optchain` see-through lands in op.c, not proto.h. Ties
/// break local-first then (path, position) so the pick is deterministic
/// across the cache's randomized iteration order.
pub(super) fn pack_symbol_def_location(
    analysis: &FileAnalysis,
    origin_key: &FileKey,
    name: &str,
    module_index: &dyn CrossFileLookup,
) -> Option<RefLocation> {
    let wanted = |k: &SymKind| matches!(k, SymKind::Sub | SymKind::Variable | SymKind::Class);
    let has_body = |a: &FileAnalysis, s: &crate::model::file_analysis::Symbol| {
        a.scopes.iter().any(|sc| sc.span == s.span)
    };
    // (bodied, local, path, row, col) — the bodied/local flags are inverted
    // in the sort below so `true` ranks first.
    let mut candidates: Vec<(bool, bool, PathBuf, usize, usize, RefLocation)> = Vec::new();
    for sym in analysis.symbols().iter().filter(|s| s.name == name && wanted(&s.kind)) {
        candidates.push((
            has_body(analysis, sym),
            true,
            key_for_sort(origin_key),
            sym.selection_span.start.row,
            sym.selection_span.start.column,
            RefLocation::at(origin_key.clone(), sym.selection_span, AccessKind::Declaration),
        ));
    }
    // The FULL candidate table for `name` — a definition legitimately lives
    // in a file the one-winner `get_cached` view (or the include closure)
    // never serves (`Perl_fix_optchain`'s body is in peep.c; proto.h wins
    // the scoped lookup).
    let mut seen_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for cached in module_index.def_candidates(name) {
        if !seen_paths.insert(cached.path.clone()) {
            continue;
        }
        let whole = module_index.whole_present(&cached);
        for sym in whole.symbols().iter().filter(|s| s.name == name && wanted(&s.kind)) {
            candidates.push((
                has_body(&whole, sym),
                false,
                cached.path.clone(),
                sym.selection_span.start.row,
                sym.selection_span.start.column,
                RefLocation::at(FileKey::Path(cached.path.clone()), sym.selection_span, AccessKind::Declaration),
            ));
        }
    }
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0) // bodied first
            .then_with(|| b.1.cmp(&a.1)) // then local
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| (a.3, a.4).cmp(&(b.3, b.4)))
    });
    candidates.into_iter().next().map(|c| c.5)
}

pub(crate) fn key_for_sort(k: &FileKey) -> PathBuf {
    match k {
        FileKey::Path(p) => p.clone(),
        FileKey::Url(u) => u.to_file_path().unwrap_or_else(|_| PathBuf::from(u.as_str())),
    }
}

pub(crate) fn file_key_eq(a: &FileKey, b: &FileKey) -> bool {
    key_for_sort(a) == key_for_sort(b)
}

/// The classes whose declarations a callable rename should match: the override
/// FAMILY for `Hierarchy` (root + every overrider/inheritor), the dispatch
/// CHAIN for `Dispatch` (cursor class up to the def its dispatch lands on).
pub(super) fn method_classes_for(
    origin: &FileAnalysis,
    class: &str,
    name: &str,
    member_kind: Option<MemberKind>,
    module_index: Option<&dyn CrossFileLookup>,
    scope: OverrideScope,
) -> Vec<String> {
    // A target no cursor or declaration told about walks as a callable.
    let want = member_kind.unwrap_or(MemberKind::Callable);
    match scope {
        OverrideScope::Hierarchy => origin.member_override_family(class, name, want, module_index),
        OverrideScope::Dispatch => origin.member_rename_chain(class, name, want, module_index),
    }
}

/// Does `name` name a `#define` anywhere the origin can reach — its own macro
/// table or any cached def candidate? The macro-identity discriminator for the
/// canonical `FileScopeValue` lane: a function-like macro's occurrences appear
/// as Sub-shaped decls/calls (left unexpanded) AND re-minted Variable reads
/// (expanded-and-erased), and every spelling must mint the SAME target or gr
/// sweeps only its own lane. Perl analyses carry no `macro_defs`, and the Perl
/// hub's name-keyed cache holds no macro tables, so Perl cursors never enter.
pub(super) fn names_visible_macro(
    name: &str,
    origin: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
) -> bool {
    origin.names_macro_def(name, None)
        || idx.is_some_and(|i| {
            i.def_candidates(name)
                .iter()
                .any(|c| c.analysis.names_macro_def(name, None))
        })
}

/// Namespace-aware package agreement. Exact equality is the total rule (Perl
/// packages are absolute). `relative` — true only for closure-carrying (pack)
/// analyses — adds C++'s relative-lookup semantics: a call's qualifier and a
/// def's namespace both carry only their innermost segment, so tails compare;
/// and a side with NO attribution (the macro-guarded-namespace-open gap, or a
/// plain unqualified call) matches rather than silently dropping the site —
/// references bias to recall under partial attribution, and the `def_paths`
/// closure gate has already pinned file connectivity.
pub(super) fn pkg_agrees(relative: bool, a: Option<&str>, b: Option<&str>) -> bool {
    if a == b {
        return true;
    }
    if !relative {
        return false;
    }
    match (a, b) {
        (Some(x), Some(y)) => {
            x.rsplit("::").next().unwrap_or(x) == y.rsplit("::").next().unwrap_or(y)
        }
        _ => true,
    }
}

/// The visibility identity (`TargetRef::def_paths`) of a pack-language target
/// keyed on `name` (a class for member/enum-constant targets, the bare value
/// name for `FileScopeValue`): every def candidate closure-connected to the
/// ORIGIN — the origin file itself when it defines the name, candidates the
/// origin's closure reaches (forward: the included header), and candidates
/// whose own closure reaches back to the origin (reverse: the `.c` TU defining
/// the function whose extern decl the origin header carries — a definition
/// legitimately lives outside every consumer's closure). Empty when the lookup
/// carries no scope (Perl, or an unscoped caller): no gate, exactly the
/// pre-existing behavior.
pub(super) fn pack_def_paths(
    name: &str,
    origin_defines: bool,
    idx: Option<&dyn CrossFileLookup>,
) -> Vec<String> {
    let Some(idx) = idx else { return Vec::new() };
    let Some((self_path, visible)) = idx.visibility_scope() else {
        return Vec::new();
    };
    let self_str = self_path.to_string_lossy().into_owned();
    let mut out: Vec<String> = Vec::new();
    if origin_defines {
        out.push(self_str.clone());
    }
    let mut macro_out_of_sight = false;
    for c in idx.def_candidates(name) {
        let p = c.path.to_string_lossy().into_owned();
        let connected = visible.contains(&p) || c.analysis.pack.include_closure.contains(&self_str);
        // A `#define` of the name anywhere joins unconditionally — config
        // variants of one conceptual macro live in disjoint headers (win32.h
        // vs the unix header) and the forward lane's never-prune rule keeps
        // them one identity. Non-macro values (globals, statics) stay
        // closure-strict: two unrelated `static int counter` TUs are two
        // targets.
        if c.analysis.names_macro_def(name, None) {
            macro_out_of_sight |= !connected;
            out.push(p);
        } else if connected {
            out.push(p);
        }
    }
    // Only out-of-closure defines joined: the origin's closure gate would
    // then refuse the origin's OWN uses — the very sites the identity was
    // minted from (highlights would see them, references not).
    if macro_out_of_sight && !out.iter().any(|p| *p == self_str || visible.contains(p)) {
        out.push(self_str);
    }
    out.sort();
    out.dedup();
    out
}

/// `pack_def_paths` unioned over a Method target's class set (`class` +
/// `method_classes` — typedef aliases like perl5's `OP` ↔ `struct op` each
/// have their own defining header). A member's visibility IS its class's:
/// a file sees `op_type` iff it sees `struct op`'s definition.
pub(super) fn pack_class_def_paths(
    target: &TargetRef,
    origin: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
) -> Vec<String> {
    let TargetKind::Method { class } = &target.kind else {
        return Vec::new();
    };
    let mut classes: Vec<&str> = vec![class.as_str()];
    classes.extend(target.method_classes.iter().map(|c| c.as_str()));
    classes.sort();
    classes.dedup();
    let origin_declares = |c: &str| {
        origin
            .symbols()
            .iter()
            .any(|s| matches!(s.kind, SymKind::Class) && s.name == c)
    };
    let mut out: Vec<String> = Vec::new();
    for c in classes {
        out.extend(pack_def_paths(c, origin_declares(c), idx));
    }
    out.sort();
    out.dedup();
    out
}

/// A dispatch name that is actually spelled by *another* identifier, so the
/// token at `span` is not the literal name and rename must not rewrite it
/// (references still resolve through the fold). A variable fold
/// (`$obj->on($evt)`, `$self->$m()` — a `Variable`/`ContainerAccess` ref covers
/// the span) always counts. A const fold (`$obj->on(EVT)` — a `FunctionCall`
/// ref to the constant covers it) counts only when `include_calls` — for a
/// `Sub`/`Method` target a coinciding `FunctionCall` is the callable's OWN call
/// site (which MUST rename), not a fold; only handlers fold through a call.
/// A literal name (`on('connect')`) sits at its string-content span, uncovered.
///
/// The covering ref must spell a DIFFERENT identifier (`$m`, `EVT`) than the
/// target: a bare enum/global read is itself a `Variable` ref whose token IS
/// the literal name — that's the collected use, not a fold, and it must stay
/// rewritable. (Perl variable names carry their sigil, so they can never
/// coincide with a callable name.)
pub(super) fn span_is_folded_name(
    analysis: &FileAnalysis,
    span: Span,
    include_calls: bool,
    literal_name: &str,
) -> bool {
    analysis.refs().iter().any(|r| {
        (matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess)
            || (include_calls && matches!(r.kind, RefKind::FunctionCall | RefKind::ImportedName)))
            && r.span == span
            && r.target_name != literal_name
    })
}

/// True when `sym` is a declaration of `target` (decl-span match).
/// Shared by `collect_from_analysis` (to emit decl locations) and
/// `mask_for_target` (to decide whether the def lives in editable space).
/// `analysis` is the file the symbol lives in — the structural gates
/// (class-content, macro spans) need its scopes/macro table.
pub(super) fn symbol_defines_target(
    sym: &crate::model::file_analysis::Symbol,
    target: &TargetRef,
    analysis: &FileAnalysis,
) -> bool {
    use crate::model::file_analysis::{HashKeyOwner, SymbolDetail};
    if sym.name != target.name {
        return false;
    }
    // Treat a sub and a method in the same package as the same
    // callable — Perl's only distinction between them is call shape.
    // `Sub { package }` matches exactly that scope (None = top-level
    // script sub); `Method { class }` is `Sub { package: Some(class) }`
    // with stricter intent.
    match &target.kind {
        // The `our` decl in the named package (`our $debug` in `Cfg`). The
        // sigil-bearing name is already matched by the `sym.name == target.name`
        // gate above; `collect_package_var` owns the (sigil-narrowed) span.
        TargetKind::PackageVar { package } => {
            matches!(&sym.detail, SymbolDetail::Variable { decl_kind, .. } if decl_kind.is_package_global())
                && sym.package.as_deref() == Some(package.as_str())
        }
        TargetKind::Sub { package } => {
            // Exact scope, OR — under Hierarchy — any class in the override
            // family (so a base-`sub` rename also rewrites every override's
            // decl). Dispatch keeps the strict single-scope match. Pack
            // analyses compare namespace-aware (`pkg_agrees`), recovering an
            // unattributed def's namespace positionally so a `detail::` def
            // still declares its `detail`-scoped target.
            let relative = !analysis.pack.include_closure.is_empty();
            let recovered = match (sym.package.as_deref(), relative) {
                (None, true) => analysis.enclosing_package_of(&sym.span),
                _ => None,
            };
            let sym_pkg = sym.package.as_deref().or(recovered.as_deref());
            let in_scope = pkg_agrees(relative, sym_pkg, package.as_deref())
                || (target.scope == OverrideScope::Hierarchy
                    && target
                        .method_classes
                        .iter()
                        .any(|c| Some(c.as_str()) == sym_pkg));
            matches!(sym.kind, SymKind::Sub | SymKind::Method) && in_scope
        }
        TargetKind::Method { class } => {
            // A `sub NAME` declaration belongs to this target if it lives in
            // ANY class on the inheritance rename-chain — the parent that
            // actually defines an inherited method, not only the cursor's
            // static class. The chain is precomputed on the target (it can't
            // be re-derived while scanning the base file, which doesn't know
            // its children). Empty chain falls back to the strict class match
            // so a Method built outside `TargetRef::method` still works.
            let on_chain = target
                .method_classes
                .iter()
                .any(|c| Some(c.as_str()) == sym.package.as_deref())
                || sym.package.as_deref() == Some(class.as_str());
            // A data member (cpp `o->field`) or enum constant mints the same
            // by-name uses a method does, so its `Variable`/`Field` decl is
            // the target's declaration too — gated by the structural
            // class-content check, because a pack LOCAL inside an inline
            // method also carries the class as sticky `package` and must
            // never read as a member declaration.
            // The declaring file's member namespace decides whether a
            // member of the other family declares this target.
            (matches!(sym.kind, SymKind::Sub | SymKind::Method)
                || analysis.symbol_is_class_content(sym))
                && on_chain
                && target
                    .member_kind
                    .is_none_or(|k| k.admits_decl(sym.kind, analysis.names().member_namespace))
        }
        TargetKind::Package => matches!(
            sym.kind,
            SymKind::Package | SymKind::Class | SymKind::Module
        ),
        TargetKind::HashKeyOfSub { package, name } => matches!(
            &sym.detail,
            SymbolDetail::HashKeyDef {
                owner: HashKeyOwner::Sub { package: op, name: on },
                ..
            } if op == package && on == name
        ),
        TargetKind::HashKeyOfBridged(wanted) => matches!(
            &sym.detail,
            SymbolDetail::HashKeyDef { owner: HashKeyOwner::Bridged { class: n }, .. } if n == wanted
        ),
        // The slot's def is the group decl (the Method/HashKeyDef pair
        // already collect it) — internal-key members contribute access
        // sites only, no decl matching here.
        TargetKind::InternalHashKey { .. } => false,
        TargetKind::Handler { owner, name: hname } => {
            sym.name == *hname
                && matches!(
                    &sym.detail,
                    SymbolDetail::Handler { owner: o, .. } if o == owner
                )
        }
        // Every `#define` of the name is a declaration (config variants in
        // different headers all surface, matching the forward macro lane's
        // never-prune rule — a `#define`'s symbol can be Variable, Sub, or a
        // member-block role's Class), as is a file-scope global's def. A
        // Sub/Method symbol elsewhere is an unexpanded function-like macro
        // USE parsed as a declaration (`int x ABSL_GUARDED_BY(mu);`) — the
        // preprocessor would expand that token, so it joins the same
        // identity; the `def_paths` gate already pinned this file as one
        // that sees the macro.
        TargetKind::FileScopeValue => {
            analysis.names_macro_def(&sym.name, Some(sym.selection_span))
                || analysis.symbol_is_file_scope_value(sym)
                || ((!analysis.pack.include_closure.is_empty() || !analysis.pack.macro_defs.is_empty())
                    && matches!(sym.kind, SymKind::Sub | SymKind::Method))
        }
    }
}

/// Does any WORKSPACE-tier index copy satisfy `pred`? Copies may be
/// symbol-evicted (an empty vec is "on disk", not "declares nothing"): the
/// resident scan covers whole copies, evicted ones are checked through the
/// row-store candidates for `keys` (a couple of rehydrations, never a
/// whole-tree decode).
fn editable_copy_satisfies(
    module_index: Option<&dyn CrossFileLookup>,
    keys: &[String],
    pred: &dyn Fn(&FileAnalysis) -> bool,
) -> bool {
    let Some(idx) = module_index else { return false };
    let mut found = false;
    idx.for_each_workspace_file(&mut |cached| {
        if !found && !cached.analysis.symbols_are_evicted() {
            found = pred(&cached.analysis);
        }
    });
    if found {
        return true;
    }
    for path in idx.ref_candidate_paths(keys) {
        if idx.is_dependency_path(&path) {
            continue;
        }
        let Some(cm) = idx.cached_by_path(&path) else { continue };
        if !cm.analysis.symbols_are_evicted() {
            continue; // the resident scan already judged it
        }
        if pred(&idx.whole_present(&cm)) {
            return true;
        }
    }
    false
}

/// Pick the role mask for a *references* query: scope to editable space
/// (OPEN + WORKSPACE) when the target is declared in a file we can edit,
/// else widen to VISIBLE so refs into a dependency-defined symbol still
/// surface. "Find references" on a project symbol must not scan CPAN —
/// see the file-store ADR's RoleMask discipline.
pub fn references_mask_for(
    files: &OpenDocs,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
) -> RoleMask {
    let mut found_in_editable = false;
    files.for_each_open(|_url, doc| {
        if doc.analysis.symbols().iter().any(|s| symbol_defines_target(s, target, &doc.analysis)) {
            found_in_editable = true;
        }
    });
    if !found_in_editable {
        found_in_editable = editable_copy_satisfies(
            module_index,
            &retrieval_keys(target, &[]),
            &|fa: &FileAnalysis| fa.symbols().iter().any(|s| symbol_defines_target(s, target, fa)),
        );
    }
    // A class-keyed Method target whose decl we can't see in editable
    // space (cross-file synthesized accessor, parent in @INC) still wins
    // EDITABLE if the *class* is a workspace package — the callers we
    // care about are project files.
    if !found_in_editable {
        if let TargetKind::Method { class } = &target.kind {
            found_in_editable = editable_copy_satisfies(
                module_index,
                &[crate::model::file_analysis::name_match_key(class, &target.names)],
                &|fa: &FileAnalysis| {
                    fa.symbols().iter().any(|s| {
                        matches!(s.kind, SymKind::Package | SymKind::Class) && s.name == *class
                    })
                },
            );
        }
    }
    if found_in_editable {
        RoleMask::EDITABLE
    } else {
        RoleMask::VISIBLE
    }
}

/// Collect the rename/reference locations for an `our` package global in one
/// file: the `our` decl, every qualified `$Pkg::var` access (its span is
/// already the bare tail), and the file's unqualified reads that resolve to the
/// decl. Decl + unqualified spans carry the sigil, so they're narrowed past it
/// — the qualifier/sigil survives, only the name token is rewritten.
pub(super) fn collect_package_var(
    key: &FileKey,
    analysis: &FileAnalysis,
    package: &str,
    name: &str,
    out: &mut Vec<RefLocation>,
) {
    use crate::model::file_analysis::{RefKind, SymbolDetail};
    // Rewrite only the trailing name token, anchored at the span *end* so the
    // sigil and any `Pkg::` qualifier survive — regardless of whether the ref
    // span is the whole `$Pkg::name` (container/element/slice reads span
    // sigil+qualifier+name) or already the bare tail (scalar reads, which the
    // builder pre-narrows). Byte math: sigils are 1 byte, columns are bytes.
    let sigil_len = name.chars().next().map_or(0, char::len_utf8);
    let base_len = name.len() - sigil_len;
    let tail = |s: Span| Span {
        start: tree_sitter::Point::new(s.end.row, s.end.column.saturating_sub(base_len)),
        end: s.end,
    };
    let is_our_decl = |id: crate::model::file_analysis::SymbolId| {
        let s = analysis.symbol(id);
        matches!(&s.detail, SymbolDetail::Variable { decl_kind, .. } if decl_kind.is_package_global())
            && s.package.as_deref() == Some(package)
            && s.name == name
    };
    for sym in analysis.symbols() {
        if matches!(&sym.detail, SymbolDetail::Variable { decl_kind, .. } if decl_kind.is_package_global())
            && sym.package.as_deref() == Some(package)
            && sym.name == name
        {
            out.push(RefLocation::at(key.clone(), tail(sym.selection_span), AccessKind::Declaration));
        }
    }
    for r in analysis.refs() {
        if !matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess) {
            continue;
        }
        if let Some((qpkg, qname)) = r.qualified_var_target(analysis.names()) {
            // Qualified `$Pkg::var` (the sigil is canonicalized to the declared
            // one, so `@arr` element reads `$Pkg::arr[0]` still match `@arr`).
            if qpkg == package && qname == name {
                // A symbolic ref folded through a variable (`${$n}`) sits on
                // the `$n` token, which rename must not rewrite; a
                // single-literal fold's rewrite lands on that literal
                // (rule #9), as for a const-folded method call.
                let folded = span_is_folded_name(analysis, r.span, false, &r.target_name);
                out.push(RefLocation::at(key.clone(), if folded { r.span } else { tail(r.span) }, r.access).with_rewrite(SiteRewrite::literal_if(!folded)));
                if let Some(src) = r.folded_from {
                    out.push(RefLocation::at(key.clone(), src, r.access));
                }
            }
        } else if r.target_name == name && r.resolved_symbol().is_some_and(is_our_decl) {
            // Unqualified — only this package's `our` var (resolved in-file).
            out.push(RefLocation::at(key.clone(), tail(r.span), r.access));
        }
    }
}

/// The per-query half of the visibility gate: each def_path's global path
/// id, resolved ONCE — the per-candidate test is then lock-free binary
/// search (`contains_id`). `None` = that def_path is in no closure at all.
pub(super) fn def_path_ids(target: &TargetRef) -> Vec<Option<u32>> {
    target
        .def_paths
        .iter()
        .map(|d| crate::model::file_analysis::path_intern::lookup_id(d))
        .collect()
}

pub(super) fn file_sees_target_ids(
    target: &TargetRef,
    ids: &[Option<u32>],
    analysis: &FileAnalysis,
    file_str: &str,
) -> bool {
    target.def_paths.is_empty()
        || target.def_paths.iter().zip(ids).any(|(d, id)| {
            d == file_str || id.is_some_and(|id| analysis.pack.include_closure.contains_id(id))
        })
}

/// A `FileKey`'s canonical path string — the spelling the visibility facts
/// (`def_paths`, alias def sites, include closures) are keyed in.
pub(super) fn canonical_file_str(key: &FileKey) -> String {
    let file_path = key_for_sort(key);
    std::fs::canonicalize(&file_path)
        .unwrap_or(file_path)
        .to_string_lossy()
        .into_owned()
}

/// The refs of `analysis` that can match `name` under any matcher arm — the
/// in-file half of the relational narrowing (`RefTable::by_key`), in ref
/// order. Callers iterate this instead of the whole ref vec.
pub(super) fn refs_keyed<'a>(
    analysis: &'a FileAnalysis,
    name: &str,
) -> impl Iterator<Item = &'a crate::model::file_analysis::Ref> + 'a {
    let refs = analysis.refs();
    analysis
        .ref_indices_keyed(&crate::model::file_analysis::name_match_key(name, analysis.names()))
        .iter()
        .map(move |&i| &refs[i])
}

/// Per-walk scratch for facts that depend on the TARGET alone, not on the
/// file being scanned, so a walk over N candidate files derives each once.
/// Created by the walk driver; the matcher fills it lazily.
#[derive(Default)]
pub(super) struct WalkMemo {
    /// `class_member_bare_constant`'s verdict per CANDIDATE file of the
    /// target's class: a whole-copy fetch plus a symbol scan of that
    /// candidate, which reads nothing about the asker. Which candidates an
    /// asker sees stays per-asker (`visible_def_candidates` under its
    /// closure); what each candidate answers is shared across every file
    /// the walk scans.
    pub(super) bare_constant_by_candidate:
        std::cell::RefCell<std::collections::HashMap<PathBuf, Option<bool>>>,
}

pub(super) fn collect_from_analysis(
    key: &FileKey,
    analysis: &FileAnalysis,
    target: &TargetRef,
    aliases: &[DelegationAlias],
    module_index: Option<&dyn CrossFileLookup>,
    file_str: &str,
    memo: &WalkMemo,
    out: &mut Vec<RefLocation>,
) {
    use crate::model::file_analysis::HashKeyOwner;

    // An alias applies in THIS file only if its `#define` is visible here
    // (macro expansion requires inclusion). Files of another language have
    // no pack closure, so they can never match an alias — the cross-language
    // pollution gate.
    let visible_aliases: Vec<&DelegationAlias> = aliases
        .iter()
        .filter(|a| {
            a.def_path == file_str
                || analysis.pack.include_closure.contains(&a.def_path)
        })
        .collect();

    // Pack languages: name lookups during matching (invocant typing, the
    // typedef chase) must resolve against THIS file's include closure — the
    // same visibility goto-def uses at this file's cursors — or a scanned
    // file's `o->op_type` types against a globally-arbitrary same-named
    // candidate and the site silently drops out. Transparent for Perl
    // (empty closure = the plain index).
    let scoped_storage: Option<crate::model::file_analysis::ScopedLookup>;
    let module_index: Option<&dyn CrossFileLookup> = match module_index {
        Some(idx) if !analysis.pack.include_closure.is_empty() => {
            let path = key_for_sort(key);
            // Guarded by a non-empty include closure — a pack-only shape.
            scoped_storage = Some(crate::model::file_analysis::ScopedLookup::new(
                idx,
                &analysis.pack.include_closure,
                Some(path.as_path()),
                crate::model::file_analysis::VisibilityAxis::IncludeClosure,
            ));
            // SAFETY: scoped_storage was just set to Some(..) on the line above,
            // in this same match arm — a lifetime-extension idiom, not a fallible read.
            Some(scoped_storage.as_ref().unwrap() as &dyn CrossFileLookup)
        }
        other => other,
    };

    // Package globals match by package + (qualified) name, not the callable
    // scope machinery below — and their spans need sigil handling — so collect
    // them on a dedicated path.
    if let TargetKind::PackageVar { package } = &target.kind {
        collect_package_var(key, analysis, package, &target.name, out);
        return;
    }

    // The member is constant across all refs in this call (the target's name,
    // or the constructor role for a construction), so the varying key is the
    // invocant class plus which of the two the ref asks for. Cache chains by
    // that pair to avoid an O(refs × ancestor_depth) DFS on large files with
    // many same-method calls against the same class.
    let mut rename_chain_cache: std::collections::HashMap<(String, bool), Vec<String>> =
        std::collections::HashMap::new();
    // The chain walks the target's family (constant per call, so the
    // class-keyed cache stays sound); the scanned file's namespace decides
    // which uses that family reaches.
    let chain_want = target.member_kind.unwrap_or(MemberKind::Callable);
    let namespace = analysis.names().member_namespace;
    // Whether a call on `cn` asking for `member` reaches the target owned by
    // `pkg`. Under Hierarchy the override family is precomputed and a call
    // matches iff its class is in it — so `$child->m` and `$base->m` rename
    // together (every family member descends from the root, so inheriting
    // without overriding is covered by membership). The family walk runs
    // INVERSE edges from the origin file, which can miss aliases declared
    // elsewhere (perl5's `typedef struct op OP` lives in perl.h, so `OP`
    // isn't in `op`'s computed family) — the UPWARD chain from the call's
    // class needs no inverse index, so a class whose chain reaches the
    // family is admitted too. Under Dispatch the call matches only if it
    // dispatches to THIS def: `$child->m` reaches an ancestor target via the
    // per-class chain, unrelated same-named methods stay out.
    let mut reaches_target = |cn: &str, member: MemberKey<'_>, pkg: &str| -> bool {
        let mut chain = || {
            rename_chain_cache
                .entry((cn.to_string(), matches!(member, MemberKey::Constructor)))
                .or_insert_with(|| analysis.member_rename_chain(cn, member, chain_want, module_index))
                .clone()
        };
        if target.scope == OverrideScope::Hierarchy {
            target.method_classes.iter().any(|c| c == cn)
                || chain().iter().any(|c| target.method_classes.iter().any(|f| f == c))
        } else {
            cn == pkg || chain().iter().any(|c| c == pkg)
        }
    };

    // Pack (closure-carrying) files speak C++'s relative name lookup and may
    // carry only partial namespace attribution — `pkg_agrees` reads this.
    let relative_ns = !analysis.pack.include_closure.is_empty();
    // Bare unresolved reads count as uses of a Method target only when the
    // member's name hoists into the enclosing scope (an enumerator's does).
    // Receiver-reached members (fields, methods) are matched through their
    // call sites — a bare same-named token elsewhere is noise (the
    // `formatter::format` 1621-hit sweep). Resolved once per scanned file,
    // under this file's own closure scope.
    let bare_constant_member = match &target.kind {
        TargetKind::Method { class } => {
            class_member_bare_constant(&target.name, class, analysis, module_index, Some(memo))
                .unwrap_or(false)
        }
        _ => false,
    };
    // A FileScopeValue whose name is a macro THIS file can see: any resolved
    // same-named read here is a macro use (the preprocessor would have
    // expanded the token), even when it bound to an unexpanded-use artifact
    // symbol. A GLOBAL-flavored target keeps the strict resolved-symbol
    // check, so a shadowing local named like the global stays out.
    let macro_visible_here = matches!(target.kind, TargetKind::FileScopeValue)
        && names_visible_macro(&target.name, analysis, module_index);

    // A callable/handler name can be FOLDED from another identifier: a variable
    // (`$obj->on($evt)`, `$self->$m()`) or, for handlers, a constant
    // (`on(EVT)`). The folded site is a *reference* to that variable/constant,
    // not a literal name token — rename must skip it (references still list it),
    // or it rewrites the variable/constant and corrupts the dispatch. A
    // `FunctionCall` coincidence is a const-fold for a handler but a Sub's OWN
    // call site otherwise, so only handlers fold through calls. Other kinds
    // (Variable/Package/HashKey) have literal-name spans — always rewritable.
    let (foldable, folds_through_calls) = match target.kind {
        TargetKind::Handler { .. } => (true, true),
        TargetKind::Sub { .. } | TargetKind::Method { .. } => (true, false),
        _ => (false, false),
    };
    let rewritable_at = |span: Span| {
        !(foldable && span_is_folded_name(analysis, span, folds_through_calls, &target.name))
    };

    // Include declaration spans when this file defines the target. Name
    // equality is `symbol_defines_target`'s first gate, so only the
    // same-named symbols can pass (in symbol order, as the vec would).
    for &sid in analysis.symbols_named(&target.name) {
        let sym = analysis.symbol(sid);
        if symbol_defines_target(sym, target, analysis) {
            out.push(RefLocation::at(key.clone(), sym.selection_span, AccessKind::Declaration).with_rewrite(SiteRewrite::literal_if(rewritable_at(sym.selection_span))));
        }
    }

    // Collect usage refs.
    let callable_scope_for_refs: Option<Option<String>> = match &target.kind {
        TargetKind::Sub { package } => Some(package.clone()),
        TargetKind::Method { class } => Some(Some(class.clone())),
        _ => None,
    };
    // Only the key buckets for the target and its visible aliases can hold
    // a match; the union is walked in ref order so `out` keeps the vec's
    // order (the final sort is by start point, and same-start refs — a
    // chain's outer call and its receiver — rely on insertion order).
    let mut keyed: Vec<usize> = Vec::new();
    for k in target.retrieval_keys(visible_aliases.iter().map(|a| a.name.as_str()), analysis.names()) {
        keyed.extend_from_slice(analysis.ref_indices_keyed(&k));
    }
    keyed.sort_unstable();
    keyed.dedup();
    let all_refs = analysis.refs();
    for &ri in &keyed {
        let r = &all_refs[ri];
        // Where calling a class constructs it (python's `Foo()`), a call
        // whose name is a class runs that class's constructor, as php's
        // `new Foo` does: the class it constructs, asked only for a
        // constructor target. An `ImportedName` names the class and runs
        // nothing, so its kind keeps it out.
        let call_constructs = (!target.constructed_classes.is_empty()
            && matches!(r.kind, RefKind::FunctionCall))
        .then(|| analysis.class_named_by_call(r, module_index))
        .flatten();
        let construction = r.constructs() || call_constructs.is_some();
        // A qualified call (`Foo::baz()` / `$o->Foo::Bar::baz()`) keeps its
        // whole path in `target_name`; match it on the bare callable tail (the
        // dispatch-class checks in the call arms below still pin the right
        // package/class). A member value read matches on the same tail;
        // every other ref kind matches by exact name.
        // Where calling a class constructs it, a call-shaped token can name
        // a class too (`class_named_by_call`), so a class target reads it
        // as one.
        let call_names_class = matches!(r.kind, RefKind::FunctionCall | RefKind::ImportedName)
            && matches!(target.kind, TargetKind::Package)
            && analysis.pack.names.class_call_sep().is_some();
        let name_matches = if matches!(r.kind, RefKind::PackageRef) || call_names_class {
            // A class token names the class its file's use-map says it
            // does; the target is that identity.
            analysis.spelled_identity(r) == target.name
        } else if construction {
            // A construction is keyed by its class; whether it runs this
            // constructor is the dispatch check below.
            !target.constructed_classes.is_empty()
        } else if matches!(r.kind, RefKind::FunctionCall | RefKind::ImportedName) || r.member_site().is_some() {
            r.unqualified_target_name(analysis.names()) == target.name
        } else {
            r.target_name == target.name
        };
        // A use spelled through a delegating macro (`IncRef(x)` where
        // `#define IncRef(sv) Perl_Inc(sv)`, or the object-like alias
        // `#define op_prune_chain_head Perl_op_prune_chain_head`) IS a use of
        // the target — the backward see-through. Call-shaped when the use was
        // left unexpanded, a bare Variable read when the expansion erased it
        // (the re-minted read carries the alias's name). Bypasses the package
        // gates below (the delegation edge already pinned the identity) and
        // is never rewritable (the token spells the MACRO's name).
        let alias_matched = !name_matches
            && matches!(
                r.kind,
                RefKind::FunctionCall | RefKind::ImportedName | RefKind::Variable | RefKind::PackageRef
            )
            && visible_aliases
                .iter()
                .any(|a| a.name == r.unqualified_target_name(analysis.names()));
        if !name_matches && !alias_matched {
            continue;
        }
        // Sub + Method both match any call into that scope — function
        // or method shape — per the "same callable, two shapes"
        // invariant. Filter is a single scope comparison.
        let mut also_on: Vec<String> = Vec::new();
        let matches_kind = alias_matched || match (&target.kind, &r.kind) {
            (TargetKind::Method { class }, RefKind::FunctionCall) if call_constructs.is_some() => {
                call_constructs.as_deref().is_some_and(|cn| reaches_target(cn, MemberKey::Constructor, class))
            }
            (TargetKind::Sub { .. } | TargetKind::Method { .. },
             RefKind::FunctionCall | RefKind::ImportedName) => {
                // callable_scope_for_refs is derived from the same target.kind
                // match above; a mismatch means malformed input rather than a
                // real match, so skip this ref instead of asserting the invariant.
                let Some(scope) = callable_scope_for_refs.as_ref() else {
                    continue;
                };
                // Under Hierarchy a bare call into ANY family class matches (the
                // whole override family); Dispatch keeps the strict single
                // scope. A bare imported call the single-file walk couldn't pin
                // (`use Bank;` auto-imports `@EXPORT`, invisible at build) has
                // no `Function` binding — re-derive it here, where the index
                // is in hand.
                // Relative-namespace semantics apply to namespace-scoped Subs
                // only: a Method target's scope is a CLASS, which an
                // unqualified call can't name-look-up into from outside — the
                // tolerance would re-open the bare-name sweep on members.
                let ns_relative = relative_ns && matches!(target.kind, TargetKind::Sub { .. });
                let pkg_matches = |pkg: &Option<String>| {
                    pkg_agrees(ns_relative, pkg.as_deref(), scope.as_deref())
                        // Inline-namespace transparency, BOTH directions. A
                        // qualified `mylib::is_thing` / `absl::X` keys on the
                        // transparent parent while the def sits under an inline
                        // child (`v1`, `head`); an UNQUALIFIED in-namespace use
                        // is the mirror — its enclosing owner is the inline
                        // CHILD (`v1`) while the def is attributed to the parent
                        // (`mylib`) whenever the child namespace was opened by a
                        // macro the sticky context never recorded. Expanding
                        // only one side matches the first but drops the second
                        // (the def-anchored gr asymmetry). Expand BOTH and test
                        // for a shared owner: a parent's set contains its inline
                        // children, so parent↔child agrees whichever side names
                        // the parent. Unrelated namespaces share nothing.
                        || match (pkg.as_deref(), scope.as_deref()) {
                            (Some(named), Some(actual)) => {
                                let a = pack_inline_owner_set(analysis, named);
                                let b = pack_inline_owner_set(analysis, actual);
                                a.iter().any(|o| b.contains(o))
                            }
                            _ => false,
                        }
                        || (target.scope == OverrideScope::Hierarchy
                            && target.method_classes.iter().any(|c| Some(c) == pkg.as_ref()))
                };
                match r.resolved_package() {
                    Some(pinned) => pkg_matches(&Some(pinned.to_string())),
                    None => {
                        // Unqualified + unresolved: derive the caller's own
                        // enclosing namespace positionally (pack) — a plain
                        // `vformat_to(...)` inside `namespace fmt` looks up
                        // fmt's, not detail's — before falling to the
                        // no-package comparison.
                        let derived = analysis.deferred_call_package(r, module_index).or_else(
                            || {
                                relative_ns
                                    .then(|| analysis.enclosing_package_of(&r.span))
                                    .flatten()
                            },
                        );
                        pkg_matches(&derived)
                    }
                }
            }
            (TargetKind::Sub { .. } | TargetKind::Method { .. },
             RefKind::MethodCall { .. } | RefKind::FieldAccess { .. }) => {
                // Prefer the build-time-frozen dispatch edge (the `Method`
                // binding) so a call that resolved at build
                // time stays matched regardless of query-time inference. An
                // absent edge means build-time lacked cross-file info (SUPER
                // into a cross-file parent; enrichment re-stamps OPEN docs
                // only) — re-resolve lazily here, where the index is in hand,
                // rather than silently excluding the site. Either way the
                // class then fans out over `member_rename_chain` so
                // `$child->m` matches an ancestor-defined target while
                // unrelated same-named methods stay out.
                // Same derived-from-the-same-match invariant as the FunctionCall
                // arm above; skip rather than assert if it ever doesn't hold.
                let Some(scope) = callable_scope_for_refs.as_ref() else {
                    continue;
                };
                // The use's family must be one the target's admits under
                // the scanned file's member namespace: a value read never
                // references a method where the families are separate.
                if !target
                    .member_kind
                    .is_none_or(|k| k.admits_ref(MemberKind::of_ref(&r.kind), namespace))
                {
                    continue;
                }
                let member = r.member_key(analysis.names());
                {
                    // A union receiver dispatches on every arm, so the
                    // site references each arm's member.
                    let resolved_classes = analysis.receiver_classes(r, module_index, ReceiverPolicy::Edge);
                    let mut unmatched: Vec<String> = Vec::new();
                    let matched = match scope {
                        Some(pkg) => resolved_classes.into_iter().fold(false, |any, cn| {
                            let hit = reaches_target(&cn, member, pkg);
                            // A class the site may be and the target is not:
                            // only a matched site (some other class hit) keeps it.
                            if !hit {
                                unmatched.push(cn);
                            }
                            any || hit
                        }),
                        None => false,
                    };
                    if matched {
                        also_on = unmatched;
                    }
                    matched
                }
            }
            (TargetKind::Package, RefKind::PackageRef) => true,
            (TargetKind::Package, RefKind::FunctionCall | RefKind::ImportedName) => call_names_class,
            // A class member read by BARE name (`x = OP_SCOPE`,
            // `case OP_SCOPE:`) — a `Variable` ref the generic goto-def
            // resolves to this def by name (the value-read half of the shared
            // Variable/Field DEF). An UNRESOLVED read counts only when the
            // member's name actually hoists into the enclosing scope
            // (`bare_constant_member`) — receiver-reached members (fields,
            // methods) never match bare tokens, or every stray `format` in
            // the workspace joins the set. A resolved read counts only when
            // it binds the target's own class content (a genuinely-local
            // variable — even one carrying the class as sticky package —
            // stays out via the structural gate).
            (TargetKind::Method { class }, RefKind::Variable) => match r.resolved_symbol() {
                None => target.bare_constant || bare_constant_member,
                Some(id) => {
                    let s = analysis.symbol(id);
                    analysis.symbol_is_class_content(s)
                        && (s.package.as_deref() == Some(class.as_str())
                            || target
                                .method_classes
                                .iter()
                                .any(|c| Some(c.as_str()) == s.package.as_deref()))
                }
            },
            // The same bare-constant gate for a TYPE-guessed token: a pack
            // grammar parses a value in a type/value-ambiguous slot (a
            // template argument `MakeError<StatusCode::kNotFound>`) as a type,
            // minting a PackageRef — for an enum-constant member that token is
            // a use (the value hoists, exactly like the unresolved bare read
            // above). Receiver-reached members stay out on the same gate.
            (TargetKind::Method { .. }, RefKind::PackageRef) => {
                target.bare_constant || bare_constant_member
            }
            // A file-scope value's uses, all bare-name-keyed like its forward
            // resolutions: a value read (object-like macro / global / enum
            // constant), a type-position token (a type-alias `#define` used as
            // a declared type), or an unresolved call (function-like macro —
            // a package-pinned call belongs to that package's sub, not here).
            (TargetKind::FileScopeValue, RefKind::Variable) => match r.resolved_symbol() {
                None => true,
                Some(id) => {
                    let s = analysis.symbol(id);
                    macro_visible_here
                        || analysis.names_macro_def(&s.name, Some(s.selection_span))
                        || analysis.symbol_is_file_scope_value(s)
                }
            },
            (TargetKind::FileScopeValue, RefKind::PackageRef) => true,
            (TargetKind::FileScopeValue, RefKind::FunctionCall | RefKind::ImportedName) => {
                r.resolved_package().is_none()
            }
            (
                TargetKind::HashKeyOfSub { package, name },
                RefKind::HashKeyAccess { .. },
            ) => {
                // The owning-sub match, widened across inheritance for
                // CONSTRUCTOR keys: a base attr's ctor key
                // (`HashKeyOfSub{Animal, new}`) is also keyed by a SUBCLASS
                // construction (`Dog->new(name => …)`, owner `Sub{Dog, new}`),
                // since `name` is the inherited attr. So renaming a base attr
                // reaches child constructions.
                let sub_matches = |op: &Option<String>, on: &str| -> bool {
                    if on != name.as_str() {
                        return false;
                    }
                    op == package
                        || (crate::model::conventions::is_constructor_name(on)
                            && match (op.as_deref(), package.as_deref()) {
                                (Some(child), Some(base)) => {
                                    analysis.class_isa(child, base, module_index)
                                }
                                _ => false,
                            })
                };
                match r.hash_key_owner() {
                    Some(HashKeyOwner::Sub { package: op, name: on }) => sub_matches(op, on),
                    // owner `None` (build gate blind) OR `Variable` (the var is
                    // bound to an imported call enrichment didn't reach in this
                    // unenriched workspace file) — re-derive cross-file, the same
                    // lazy seam method dispatch + deferred owners use above. This
                    // is what makes a producer-origin rename reach the consumer's
                    // `$c->{key}` access without depending on open-doc enrichment.
                    _ => analysis
                        .deferred_hash_key_owner(r, module_index)
                        .is_some_and(|o| {
                            matches!(o, HashKeyOwner::Sub { package: op, name: on } if sub_matches(&op, &on))
                        }),
                }
            },
            (TargetKind::HashKeyOfBridged(wanted), RefKind::HashKeyAccess { .. }) => {
                // A DBIC/Class::Accessor column. Its key uses are the
                // condition args (`$rs->search({ col => … })`), owned by the
                // `Column` namespace — NOT `$row->{col}` derefs, which carry a
                // `Class` lookup and so never match here (a column isn't a hash
                // slot). The owner-`None` case is the cross-file deferred arg key.
                let target_owner = HashKeyOwner::Bridged { class: wanted.clone() };
                match r.hash_key_owner() {
                    Some(o) => o.found_by(&target_owner),
                    None => analysis
                        .deferred_hash_key_owner(r, module_index)
                        .is_some_and(|o| o.found_by(&target_owner)),
                }
            }
            (TargetKind::InternalHashKey { class },
             RefKind::HashKeyAccess { .. }) => {
                // STRICT Class-owner shape (see the kind's doc), widened
                // only by ancestry: a subclass poking `$self->{attr}` owns
                // the access as ITS class — `Gadget isa Widget` ties it to
                // Widget's attr. Never `found_by` (Sub-owned arg keys stay
                // out).
                matches!(
                    r.hash_key_owner(),
                    Some(HashKeyOwner::Class(c))
                        if c == class || analysis.class_isa(c, class, module_index)
                )
            }
            (TargetKind::Handler { owner, name: hname },
             RefKind::DispatchCall { .. }) => {
                r.target_name == *hname
                    && matches!(r.handler_owner(), Some(o) if o == owner)
            }
            _ => false,
        };
        if matches_kind {
            // A member access's r.span covers the whole expression; callers
            // (rename, highlight) want just the member-name token so they
            // can replace or underline exactly the right characters.
            let span = r.member_site().map_or(r.span, |m| m.name_span);
            let rewrite = if alias_matched {
                SiteRewrite::ThroughMacro
            } else if construction {
                // A construction's token spells the class, not the member.
                SiteRewrite::Construction
            } else if !also_on.is_empty() {
                SiteRewrite::Shared { also_on: std::mem::take(&mut also_on) }
            } else {
                SiteRewrite::literal_if(rewritable_at(span))
            };
            out.push(RefLocation::at(key.clone(), span, r.access).with_rewrite(rewrite));
            // A call folded from a variable (`my $m = 'process'; $self->$m()`)
            // has a non-rewritable name token above; the rewrite belongs on the
            // source string literal the fold came from (rule #9).
            if let Some(src) = r.folded_from {
                out.push(RefLocation::at(key.clone(), src, r.access).with_rewrite(SiteRewrite::literal_if(rewritable_at(src))));
            }
        }
    }

    // Query-time dispatch resolution: gated candidates (which ride the cache
    // ungated, even in non-open workspace/dependency files) resolve their
    // receiver isa-check NOW against the module index. The `Applies` ones are
    // handler call-sites that enrichment-eager promotion would have missed in
    // any file that's never enriched. `applicable_dispatches` skips sites the
    // emit-hook path already materialized above, so no double-count.
    // See `docs/adr/receiver-gated-dispatch.md`.
    if let TargetKind::Handler { owner, name: hname } = &target.kind {
        for applied in analysis.applicable_dispatches(module_index) {
            if &applied.name == hname && &applied.owner == owner {
                out.push(RefLocation::at(key.clone(), applied.span, AccessKind::Read).with_rewrite(SiteRewrite::literal_if(rewritable_at(applied.span))));
            }
        }
    }
}
