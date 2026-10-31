//! Goto-definition, `#include` navigation, document highlights, linked editing.

use super::*;

/// A hierarchy projection node → LSP `TypeHierarchyItem`. Pure mapping;
/// unlocatable items (no URL) drop.
pub fn to_type_hierarchy_item(
    it: &crate::index::resolve::HierarchyItem,
) -> Option<TypeHierarchyItem> {
    let uri = it.location.to_url()?;
    let range = span_to_range(it.location.span);
    Some(TypeHierarchyItem {
        name: it.name.clone(),
        kind: fa_sym_kind_to_lsp(&it.kind),
        tags: None,
        detail: it.detail.clone(),
        uri,
        range,
        selection_range: range,
        data: None,
    })
}

/// A hierarchy projection node → LSP `CallHierarchyItem`. Pure mapping.
pub fn to_call_hierarchy_item(
    it: &crate::index::resolve::HierarchyItem,
) -> Option<CallHierarchyItem> {
    let uri = it.location.to_url()?;
    let range = span_to_range(it.location.span);
    Some(CallHierarchyItem {
        name: it.name.clone(),
        kind: fa_sym_kind_to_lsp(&it.kind),
        tags: None,
        detail: it.detail.clone(),
        uri,
        range,
        selection_range: range,
        data: None,
    })
}

/// Goto-definition: the forward projection of the resolution CandidateSet,
/// adapted to LSP types. One location → Scalar; several (stacked handler
/// registrations) → Array so the editor shows a picker. The LSP handler and
/// CLI construct the set themselves (they carry the source/pack routing
/// facts); this adapter serves plain-cursor consumers and tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn find_definition(
    files: &crate::index::open_docs::OpenDocs,
    analysis: &FileAnalysis,
    pos: Position,
    uri: &Url,
    module_index: &dyn crate::model::file_analysis::CrossFileLookup,
) -> Option<GotoDefinitionResponse> {
    let cs = crate::index::resolve::resolve(
        files,
        analysis,
        crate::index::open_docs::FileKey::Url(uri.clone()),
        position_to_point(pos),
        Some(module_index),
        crate::index::resolve::OverrideScope::default(),
    );
    let locs: Vec<Location> = cs
        .definitions()
        .into_iter()
        .filter_map(|l| {
            let uri = l.to_url()?;
            Some(Location { uri, range: span_to_range(l.span) })
        })
        .collect();
    match locs.len() {
        0 => None,
        1 => Some(GotoDefinitionResponse::Scalar(locs.into_iter().next().unwrap())),
        _ => Some(GotoDefinitionResponse::Array(locs)),
    }
}

/// The concrete-leaf DISPLAY for a field/variable whose declared type is a
/// config-variant type macro (`docs/adr/macro-handling.md`, "Typing vs.
/// display"). The type that FLOWS stays the join abstraction (`Numeric`); this
/// recovers the human-facing leaf by walking provenance: pick the
/// reachability-active variant of `spelling` (the SAME ranking goto-def uses),
/// then chase that variant body's `TypeName` alias chain to its terminal
/// concrete spelling (`PERL_BITFIELD16 → U16 → U16TYPE → unsigned short`).
/// `None` when `spelling` isn't a config-variant macro, or the chase doesn't
/// reach a leaf more concrete than the variant body itself — the caller then
/// renders the flow type unchanged.
pub(super) fn config_variant_leaf_display(
    analysis: &FileAnalysis,
    spelling: &str,
    module_index: &dyn CrossFileLookup,
) -> Option<String> {
    // Hover reads only the winning variant's BODY, never its location, so the
    // queried file's own key is immaterial — a placeholder keys its local
    // `macro_defs` without colliding with the real cross-file def paths.
    let local = crate::index::open_docs::FileKey::Path(std::path::PathBuf::from("/__hover_local__"));
    let ranked = crate::index::resolve::ranked_macro_variants(analysis, spelling, &local, module_index);
    // A single-variant (or non-) macro flows to its leaf already; only the
    // config-variant JOIN abstraction needs the display-side variant pick.
    if ranked.len() < 2 {
        return None;
    }
    let body = ranked.first()?.0.body.trim();
    // Walk the chosen variant body's alias chain to its terminal concrete type.
    // Only override when the chase reached a NAMED concrete leaf (`unsigned
    // short`) PAST the body spelling — a bare primitive family (`unsigned` →
    // `Numeric`) carries no richer spelling than the flow abstraction already
    // shows, so leave the flow type in place.
    let leaf = analysis.resolve_type_name(body, Some(module_index))?;
    let display = leaf.class_name()?;
    if display == body {
        return None;
    }
    Some(display.to_string())
}

/// documentHighlight: the set's origin-narrowed references projection
/// (`highlights()`), adapted to LSP types — access classification becomes
/// the highlight kind. Shared by the LSP handler and the
/// `--document-highlight` CLI; both construct the set with their own
/// routing facts and hand it here.
pub fn document_highlights(cs: &crate::index::resolve::CandidateSet<'_>) -> Vec<DocumentHighlight> {
    use crate::model::file_analysis::AccessKind;
    cs.highlights()
        .into_iter()
        .map(|l| DocumentHighlight {
            range: span_to_range(l.span),
            kind: Some(match l.access {
                AccessKind::Write => DocumentHighlightKind::WRITE,
                _ => DocumentHighlightKind::READ,
            }),
        })
        .collect()
}

/// Linked-editing ranges = the set's co-edit projection
/// (`linked_editing_spans()` — the origin-file sites rename would rewrite
/// with the typed text verbatim), surfaced as ranges. Shared by the LSP
/// `linked_editing_range` handler and the `--linked-editing` CLI. None when
/// there's nothing to co-edit (fewer than two occurrences).
pub fn linked_editing_ranges(cs: &crate::index::resolve::CandidateSet<'_>) -> Option<Vec<Range>> {
    let spans = cs.linked_editing_spans();
    if spans.len() < 2 {
        return None;
    }
    Some(spans.into_iter().map(span_to_range).collect())
}
