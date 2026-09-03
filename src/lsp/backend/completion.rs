//! Pack-language completion gathering: member/qualified-path/enum-domain slots
//! plus the macro + include-closure cross-file identifier universes.

use super::*;

/// Pack-language completion: member access (sentinel reparse → receiver
/// span → type → members) with an in-scope-symbol fallback. Shared by the
/// LSP completion handler and the CLI/--batch mirror so the editor and
/// gold agree. Perl completion stays in `cursor_context`.
///
/// The returned flag is LSP `is_incomplete`: true when the bare-identifier
/// half consulted prefix-gated cross-file sources (macros / include-closure
/// symbols) — those are filtered server-side by the typed prefix, so the
/// client must re-request as the prefix changes instead of trusting its
/// cached list. Member completion and closure-less languages return a
/// complete list (false).
pub fn pack_completion(
    files: &crate::index::file_store::FileStore,
    analysis: &crate::model::file_analysis::FileAnalysis,
    source: &str,
    tree: &tree_sitter::Tree,
    point: tree_sitter::Point,
    language: &str,
    path: Option<&std::path::Path>,
    module_index: &ModuleIndex,
) -> (Vec<CompletionItem>, bool) {
    // Cross-file resolves against THIS language's sub-index (its own
    // cache — no cross-language overlap), falling back to the hub when
    // none is attached.
    let routed = module_index.lookup_for(language);
    let base_idx = routed.as_lookup();
    // Scope member/type resolution to the file's include closure.
    let scoped = crate::model::file_analysis::ScopedLookup::new(
        base_idx,
        &analysis.pack.include_closure,
        path,
        crate::model::file_analysis::VisibilityAxis::for_origin(
            analysis,
            path,
            base_idx,
            crate::build::language_driver::LanguageRegistry::pack_visibility(language),
        ),
    );
    let xidx: &dyn crate::model::file_analysis::CrossFileLookup = &scoped;
    // The slot verdict — Member (sentinel reparse → receiver span →
    // type) or the bare-identifier fallback (no registered driver / no
    // LangPack / no dangling member access) — comes from the one
    // cursor-tier entry (`docs/adr/cursor-slots.md`); this adapter only
    // projects it onto LSP items.
    let crate::lsp::cursor_slot::DetectedSlot { slot, .. } =
        crate::lsp::cursor_slot::detect_slot(analysis, tree, source, point, language, Some(xidx));
    if let crate::lsp::cursor_slot::Slot::Member { receiver, .. } = &slot {
        if let Some(class) =
            receiver.receiver_type.as_ref().and_then(|ty| ty.class_name().map(|s| s.to_string()))
        {
            // Mode A: the member items carry the operator-swap edit
            // (`p.` → `p->`) when the receiver's pointer depth wants
            // a different operator than was typed. The diagnostic
            // path (Mode B) is the universal fallback.
            if let Some(items) = symbols::member_completion_for_class(
                analysis, &class, xidx, receiver.op_fix.clone(), point, receiver.scoped,
            ) {
                return (items, false);
            }
            // Typed receiver, gather declined. The deliberate fall-through
            // below serves a class the analysis KNOWS (cpp's
            // self-access-sees-private gold case — the class is local).
            // A class nothing declares anywhere (a vendor type with no
            // vendor/ present — guzzle's PromiseInterface, round 3) has
            // no honest members to offer, and the identifier universe
            // after `->` is noise wearing confidence: answer EMPTY.
            let class_known = !analysis.symbols_named(&class).is_empty()
                || !xidx.def_candidates(&class).is_empty();
            if !class_known {
                return (Vec::new(), true);
            }
        }
        // An UNTYPEABLE receiver's member slot answers EMPTY, never the
        // file-scope identifier universe: after `->`/`.` only the
        // receiver's members are valid, and ~200 unrelated locals is noise
        // wearing confidence (measured on guzzle/laravel, round 1).
        // `isIncomplete` so the client re-asks as typing narrows the
        // receiver. A TYPED receiver whose member gather declined falls
        // through on purpose — the self-access-sees-private cpp path is
        // served by the in-scope fallback (gold:
        // cpp-completion-access-specifier-self-access-sees-private).
        if receiver.receiver_type.is_none() {
            return (Vec::new(), true);
        }
    }
    // `fmtx::|` — a qualified path completes to the OWNER's members
    // (workspace + dependency roles), never the global pool: the qualifier
    // is a hard filter by meaning. The gather is the CandidateSet's
    // qualified-path projection (pack lane), anchored on the same qualifier
    // detection goto-def uses. Falls through to the bare-identifier universe
    // when the owner resolves nothing (e.g. a macro-guarded namespace open
    // left members unattributed), mirroring gd's owner-anchored
    // fall-through.
    if let crate::lsp::cursor_slot::Slot::ModulePath { ref prefix, .. } = slot {
        let cs = crate::index::resolve::resolve(
            files,
            analysis,
            crate::index::file_store::FileKey::Path(
                path.map(|p| p.to_path_buf()).unwrap_or_default(),
            ),
            point,
            Some(base_idx),
            crate::index::resolve::OverrideScope::default(),
        );
        let candidates = cs.complete_qualified_path(xidx, prefix);
        if !candidates.is_empty() {
            return (
                candidates.into_iter().map(symbols::candidate_to_completion_item).collect(),
                false,
            );
        }
    }
    // `o->op_type == |` — the equality's field operand types the slot to
    // an enum DOMAIN. Rank that enum's members first (never prune the
    // bare-identifier universe): the type-constrained-completion payoff of
    // the `Slot::expected_type` seam (`docs/adr/cursor-slots.md`).
    if let crate::lsp::cursor_slot::Slot::ArgPosition { .. } = &slot {
        if let Some(crate::model::file_analysis::InferredType::ClassName(enum_name)) =
            slot.expected_type(analysis, point, Some(xidx))
        {
            let members = analysis.enum_members(&enum_name, Some(xidx));
            if !members.is_empty() {
                let mut items = symbols::in_scope_completion(analysis, point);
                let macros_live = macro_completion(source, point, language, path, &mut items);
                let closure_live = closure_symbol_completion(
                    files, analysis, source, point, language, path, module_index, &mut items);
                rank_domain_members(&mut items, &members, &enum_name);
                return symbols::finish_completion(
                    items, &slot, false, macros_live || closure_live,
                );
            }
        }
    }
    let mut items = symbols::in_scope_completion(analysis, point);
    let macros_live = macro_completion(source, point, language, path, &mut items);
    let closure_live = closure_symbol_completion(
        files, analysis, source, point, language, path, module_index, &mut items);
    // The shared finisher — the same cap, prefix rule, and flag
    // composition the Perl assembly exits through.
    symbols::finish_completion(items, &slot, false, macros_live || closure_live)
}

/// Move a domain's enum members to the front of the completion list with a
/// leading sort_text so the client ranks them first, without pruning the
/// bare-identifier universe already gathered. Members keep declaration order
/// (their numeric enum order) via a fixed-width index, and any copy already
/// present in the gathered list (an in-scope enumerator) is de-duplicated so
/// the ranked entry is the only one.
fn rank_domain_members(items: &mut Vec<CompletionItem>, members: &[String], enum_name: &str) {
    let member_set: std::collections::HashSet<&str> =
        members.iter().map(String::as_str).collect();
    items.retain(|i| !member_set.contains(i.label.as_str()));
    // "000" leads: '0' (0x30) sorts before every identifier first char.
    let mut ranked: Vec<CompletionItem> = members
        .iter()
        .enumerate()
        .map(|(i, m)| CompletionItem {
            label: m.clone(),
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            detail: Some(enum_name.to_string()),
            sort_text: Some(format!("000{:04}{}", i, m)),
            ..Default::default()
        })
        .collect();
    ranked.append(items);
    *items = ranked;
}

/// Bare-identifier cross-file completion: the file-scope symbols of every
/// header in the file's `#include` closure — C's import surface ("C = Perl,
/// everything exported": the closure is the import list, so enum constants,
/// free functions, typedefs and globals from included headers are candidates
/// exactly like imported subs are for Perl). Enumeration is gated to
/// closure-member files (`visible_defs_with_prefix` — a file that doesn't
/// include a header never sees its names) and prefix-gated like macros (no
/// bare-cursor dump of a large closure). Own-file symbols win dedup; closure
/// items sort after them (`~` sorts past every identifier char). Cross-file
/// `#define`s arrive via `macro_completion`, which also reaches headers the
/// workspace index never parsed; the dedup order makes its richer
/// `#define`-body detail win for names both sources know.
///
/// Returns whether this source is live for the file (a non-empty closure) —
/// the `is_incomplete` signal, independent of whether the current prefix
/// matched anything.
fn closure_symbol_completion(
    files: &crate::index::file_store::FileStore,
    analysis: &crate::model::file_analysis::FileAnalysis,
    source: &str,
    point: tree_sitter::Point,
    language: &str,
    path: Option<&std::path::Path>,
    module_index: &ModuleIndex,
    items: &mut Vec<CompletionItem>,
) -> bool {
    if analysis.pack.include_closure.is_empty() {
        return false;
    }
    let cursor = crate::build::cursor_sentinel::point_to_byte(source, point);
    let prefix = identifier_prefix(source, cursor);
    if prefix.is_empty() {
        return true; // live source, waiting for a prefix
    }
    let routed = module_index.lookup_for(language);
    let seen: std::collections::HashSet<String> =
        items.iter().map(|i| i.label.clone()).collect();
    // The closure-gated identifier universe is the set's completion
    // projection (the cpp instance of `complete(prefix)`); this adapter
    // owns slot detection (the typed prefix), dedup against in-scope
    // items, and presentation (the past-`z` sort tier).
    let cs = crate::index::resolve::resolve(
        files,
        analysis,
        crate::index::file_store::FileKey::Path(path.map(|p| p.to_path_buf()).unwrap_or_default()),
        point,
        Some(routed.as_lookup()),
        crate::index::resolve::OverrideScope::default(),
    );
    let candidates =
        crate::util::timings::phase("completion.closure_symbols", || cs.complete(prefix, false));
    for c in candidates {
        if seen.contains(&c.label) {
            continue;
        }
        items.push(CompletionItem {
            label: c.label.clone(),
            kind: Some(symbols::fa_completion_kind(&c.kind)),
            detail: c.detail,
            sort_text: Some(format!("~{}", c.label)),
            ..Default::default()
        });
    }
    true
}

/// The file's OWN `#define`s are already symbols (in `items`); this adds the
/// cross-file ones. Prefix-filtered server-side (a macro-heavy include
/// closure reaches thousands — perl.h alone is ~2000), and the header cache
/// is warm from analyze, so the re-gather is cheap.
///
/// Returns whether this source is live for the file (C preprocessor + a
/// path to gather from) — the `is_incomplete` signal, independent of
/// whether the current prefix matched anything.
fn macro_completion(
    source: &str,
    point: tree_sitter::Point,
    language: &str,
    path: Option<&std::path::Path>,
    items: &mut Vec<CompletionItem>,
) -> bool {
    if !crate::build::language_driver::LanguageRegistry::has_preprocessor_macros(language) {
        return false; // the pack declares its preprocessor; asked, never named
    }
    let Some(p) = path else { return false };
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
    let Some(driver) = reg.for_id(language) else { return false };
    let cursor = crate::build::cursor_sentinel::point_to_byte(source, point);
    let prefix = identifier_prefix(source, cursor);
    if prefix.is_empty() {
        return true; // no bare-cursor dump of the whole macro table
    }
    let mut parser = driver.make_parser();
    let macros = crate::build::cpp_reparse::included_macros(p, source, &mut parser);
    let seen: std::collections::HashSet<String> =
        items.iter().map(|i| i.label.clone()).collect();
    for (name, m) in macros.iter() {
        if !name.starts_with(prefix) || seen.contains(name) {
            continue;
        }
        let (kind, detail) = match &m.params {
            Some(params) => (
                CompletionItemKind::FUNCTION,
                format!("#define {}({})", name, params.join(", ")),
            ),
            None => (
                CompletionItemKind::CONSTANT,
                format!("#define {} {}", name, m.body.trim()),
            ),
        };
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(kind),
            detail: Some(detail),
            // Cross-file candidates rank after own-file symbols (which
            // carry no sort_text, so clients sort them by bare label).
            sort_text: Some(format!("~{}", name)),
            ..Default::default()
        });
    }
    true
}
