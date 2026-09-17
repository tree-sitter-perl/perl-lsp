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
    files: &crate::index::file_store::FileStore,
    analysis: &FileAnalysis,
    pos: Position,
    uri: &Url,
    module_index: &dyn crate::model::file_analysis::CrossFileLookup,
) -> Option<GotoDefinitionResponse> {
    let cs = crate::index::resolve::resolve(
        files,
        analysis,
        crate::index::file_store::FileKey::Url(uri.clone()),
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
    let local = crate::index::file_store::FileKey::Path(std::path::PathBuf::from("/__hover_local__"));
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

/// Goto-def on an `#include "x.h"` / `<x.h>` path token → the resolved header
/// file (`#include` = `use`; the header is the module). `self_path` is the
/// including file — the search anchor for the walk-up include resolver.
/// Returns a whole-file location (range 0:0); only cpp has an include model.
#[cfg(feature = "cpp")]
pub fn pack_include_definition(
    analysis: &FileAnalysis,
    point: Point,
    self_path: Option<&std::path::Path>,
) -> Option<Location> {
    let raw = analysis
        .pack.include_directives
        .iter()
        .find(|r| crate::model::file_analysis::contains_point(&r.span, point))
        .map(|r| r.raw.clone())?;
    // `"foo.h"` captures the string CONTENT (no quotes); `<sys/x.h>` captures the
    // whole token — strip the angle brackets so the resolver sees a bare path.
    let inc = raw.trim_matches(|c| c == '<' || c == '>' || c == '"');
    let base = self_path?;
    let header = crate::build::cpp_reparse::resolve_include_path(base, inc)?;
    let uri = Url::from_file_path(&header).ok()?;
    Some(Location {
        uri,
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
    })
}
#[cfg(not(feature = "cpp"))]
pub fn pack_include_definition(
    _analysis: &FileAnalysis,
    _point: Point,
    _self_path: Option<&std::path::Path>,
) -> Option<Location> {
    None
}

/// Find-references on an `#include` path token: every `#include` directive —
/// across this file + the cached pack modules — that resolves to the SAME
/// header ("who includes this header"), the backward mirror of the include
/// goto-def on the same key (the resolved header path, so `"x.h"` and a
/// differently-spelled directive reaching the same file group together).
/// `None` when the cursor isn't on an include directive; both references
/// handlers (LSP + CLI) call this before the general resolve so the path
/// token never leaks into name-keyed resolution. Sorted (path, position)
/// for deterministic output.
#[cfg(feature = "cpp")]
pub fn pack_include_references(
    analysis: &FileAnalysis,
    point: Point,
    self_path: Option<&std::path::Path>,
    module_index: &dyn CrossFileLookup,
) -> Option<Vec<(std::path::PathBuf, crate::model::file_analysis::Span)>> {
    let raw = analysis
        .pack.include_directives
        .iter()
        .find(|r| crate::model::file_analysis::contains_point(&r.span, point))
        .map(|r| r.raw.clone())?;
    let trim = |r: &str| r.trim_matches(|c| c == '<' || c == '>' || c == '"').to_string();
    let base = self_path?;
    let header = crate::build::cpp_reparse::resolve_include_path(base, &trim(&raw))?;
    let mut out: Vec<(std::path::PathBuf, crate::model::file_analysis::Span)> = Vec::new();
    let mut collect = |path: &std::path::Path, a: &FileAnalysis| {
        for row in &a.pack.include_directives {
            if crate::build::cpp_reparse::resolve_include_path(path, &trim(&row.raw)).as_deref()
                == Some(header.as_path())
            {
                out.push((path.to_path_buf(), row.span));
            }
        }
    };
    collect(base, analysis);
    // Per-FILE sweep; skip the cursor file (fresh copy already collected).
    module_index.for_each_cached_file(&mut |cached| {
        if cached.path == base {
            return;
        }
        collect(&cached.path, &cached.analysis);
    });
    out.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            (a.1.start.row, a.1.start.column).cmp(&(b.1.start.row, b.1.start.column))
        })
    });
    out.dedup();
    Some(out)
}
#[cfg(not(feature = "cpp"))]
pub fn pack_include_references(
    _analysis: &FileAnalysis,
    _point: Point,
    _self_path: Option<&std::path::Path>,
    _module_index: &dyn CrossFileLookup,
) -> Option<Vec<(std::path::PathBuf, crate::model::file_analysis::Span)>> {
    None
}

/// Re-export: the raw-word key lives with the resolution seam
/// (`resolve::word_at_point`); hover and the sig-help slot share it.
pub use crate::index::resolve::word_at_point;

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
