//! Document symbols, workspace/symbol search, selection + folding ranges.

use super::*;

// ---- Public LSP adapter functions ----

#[allow(deprecated)]
pub fn extract_symbols(analysis: &FileAnalysis) -> Vec<DocumentSymbol> {
    analysis.document_symbols()
        .iter()
        .map(outline_to_document_symbol)
        .collect()
}

#[allow(deprecated)]
/// Surface a plugin-controlled namespace in `workspace/symbol` results.
/// The namespace isn't a Perl symbol, but users want to jump to "where
/// my Minion tasks live" or "the mojo app for this package" — this
/// puts it on the same search surface as packages/subs.
#[allow(deprecated)]
pub fn plugin_namespace_to_workspace_info(
    ns: &crate::model::file_analysis::PluginNamespace,
    uri: Url,
) -> SymbolInformation {
    SymbolInformation {
        name: format!("[{}] {}", ns.kind, ns.id),
        kind: SymbolKind::NAMESPACE,
        tags: None,
        deprecated: None,
        location: Location {
            uri,
            range: span_to_range(ns.decl_span),
        },
        container_name: Some(ns.plugin_id.clone()),
    }
}

#[allow(deprecated)]
/// The ONE workspace-search visibility rule, shared by the resident sweep
/// (`symbol_to_workspace_info`) and the rows twin — a kind added to one
/// gate cannot silently diverge the other.
fn workspace_search_visible(kind: &crate::model::file_analysis::SymKind, hidden: bool, lexical: bool) -> bool {
    use crate::model::file_analysis::SymKind as FaSymKind;
    matches!(
        kind,
        FaSymKind::Sub | FaSymKind::Method | FaSymKind::Package | FaSymKind::Class
    ) && !hidden
        && !lexical
}

// `SymbolInformation::deprecated` is a deprecated-but-required field of the
// tower-lsp struct; we must supply it to construct the value.
#[allow(deprecated)]
pub fn symbol_to_workspace_info(sym: &crate::model::file_analysis::Symbol, uri: Url) -> Option<SymbolInformation> {
    if !workspace_search_visible(
        &sym.kind,
        sym.hidden_in_outline(),
        matches!(&sym.detail, crate::model::file_analysis::SymbolDetail::Sub { lexical: true, .. }),
    ) {
        return None;
    }
    Some(SymbolInformation {
        name: sym.name.clone(),
        kind: fa_sym_kind_to_lsp(&sym.kind),
        tags: None,
        deprecated: None,
        location: Location {
            uri,
            range: span_to_range(sym.selection_span),
        },
        container_name: sym.package.clone(),
    })
}

/// Collapse workspace/symbol entries that share a full identity tuple
/// (name, kind, file, line, col). Framework accessor synthesis mints twin
/// symbols at ONE span — a getter `Method` and its fluent-writer twin carry
/// the same name/kind/selection_span — and the same symbol can surface from
/// both the resident sweep and the rows pass. Keying on the whole tuple
/// collapses only byte-identical duplicates; two genuinely different symbols
/// that merely share a name keep their distinct spans.
pub fn dedup_workspace_symbols(results: &mut Vec<SymbolInformation>) {
    let mut seen = std::collections::HashSet::new();
    results.retain(|s| {
        seen.insert((
            s.name.clone(),
            format!("{:?}", s.kind),
            s.location.uri.to_string(),
            s.location.range.start.line,
            s.location.range.start.character,
        ))
    });
}

/// `symbol_to_workspace_info`'s row twin — identical kind gate and
/// hidden/lexical suppressions, sourced from the baked row flags.
// `SymbolInformation::deprecated` is a deprecated-but-required field.
#[allow(deprecated)]
pub fn sym_row_to_workspace_info(
    hit: &crate::index::module_cache::SymRowHit,
) -> Option<SymbolInformation> {
    use crate::model::file_analysis::{sym_kind_from_code, SymRowSeed};
    let kind = sym_kind_from_code(hit.kind)?;
    if !workspace_search_visible(
        &kind,
        hit.flags & SymRowSeed::FLAG_HIDDEN_IN_OUTLINE != 0,
        hit.flags & SymRowSeed::FLAG_LEXICAL_SUB != 0,
    ) {
        return None;
    }
    let path = std::path::Path::new(&hit.path);
    let uri = Url::from_file_path(path).ok()?;
    let span = crate::model::file_analysis::Span {
        start: tree_sitter::Point::new(hit.start_row, hit.start_col),
        end: tree_sitter::Point::new(hit.end_row, hit.end_col),
    };
    Some(SymbolInformation {
        name: hit.name.clone(),
        kind: fa_sym_kind_to_lsp(&kind),
        tags: None,
        deprecated: None,
        location: Location { uri, range: span_to_range(span) },
        container_name: hit.container.clone(),
    })
}

pub fn selection_ranges(tree: &Tree, pos: Position) -> SelectionRange {
    let spans = cursor_context::selection_ranges(tree, position_to_point(pos));
    // Build linked list from innermost to outermost
    let mut result: Option<SelectionRange> = None;
    for span in spans.into_iter().rev() {
        result = Some(SelectionRange {
            range: span_to_range(span),
            parent: result.map(Box::new),
        });
    }
    result.unwrap_or(SelectionRange {
        range: Range::default(),
        parent: None,
    })
}

pub fn folding_ranges(analysis: &FileAnalysis) -> Vec<FoldingRange> {
    analysis.fold_ranges
        .iter()
        .map(|f| FoldingRange {
            start_line: f.start_line as u32,
            start_character: None,
            end_line: f.end_line as u32,
            end_character: None,
            kind: Some(match f.kind {
                FoldKind::Region => FoldingRangeKind::Region,
                FoldKind::Comment => FoldingRangeKind::Comment,
            }),
            collapsed_text: None,
        })
        .collect()
}
