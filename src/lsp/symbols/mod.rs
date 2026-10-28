//! LSP adapter: FileAnalysis types -> LSP protocol types, one part per verb
//! family. No analysis, no tree walks (CLAUDE.md rule #3).

use std::collections::HashMap;
use tower_lsp::lsp_types::*;
use tree_sitter::{Point, Tree};

use crate::lsp::cursor_context;
use crate::lsp::cursor_slot::Slot;
use crate::model::file_analysis::{
    format_inferred_type, CompletionCandidate, CrossFileLookup, FileAnalysis, FoldKind,
    GuardVerdict, HandlerOwner, InferredType, OutlineSymbol, ParamInfo, RefKind, Span,
    SymKind as FaSymKind, SymbolDetail,
};
use crate::index::module_index::{ModuleIndex, SubInfo};
use crate::index::resolve::ImportResolution;
#[cfg(test)]
use crate::index::resolve::resolve_imported_function;

// ---- Coordinate conversion ----

fn point_to_position(p: Point) -> Position {
    Position {
        line: p.row as u32,
        character: p.column as u32,
    }
}

pub fn position_to_point(pos: Position) -> Point {
    Point::new(pos.line as usize, pos.character as usize)
}

pub fn span_to_range(span: Span) -> Range {
    Range {
        start: point_to_position(span.start),
        end: point_to_position(span.end),
    }
}

// ---- Symbol conversion ----

fn fa_sym_kind_to_lsp(kind: &FaSymKind) -> SymbolKind {
    match kind {
        FaSymKind::Sub => SymbolKind::FUNCTION,
        FaSymKind::Method => SymbolKind::METHOD,
        FaSymKind::Variable => SymbolKind::VARIABLE,
        FaSymKind::Field => SymbolKind::FIELD,
        FaSymKind::Enumerator => SymbolKind::ENUM_MEMBER,
        FaSymKind::Package => SymbolKind::NAMESPACE,
        FaSymKind::Class => SymbolKind::CLASS,
        FaSymKind::Module => SymbolKind::MODULE,
        FaSymKind::HashKeyDef => SymbolKind::KEY,
        // Handler's actual LSP kind depends on the plugin's
        // `display` choice — this fallback only fires for paths
        // that don't carry detail, which is rare. Event is the
        // conservative default.
        FaSymKind::Handler => SymbolKind::EVENT,
        FaSymKind::Namespace => SymbolKind::NAMESPACE,
    }
}

/// Plugin-chosen Handler display → LSP SymbolKind. Called from the
/// outline path where OutlineSymbol carries `handler_display`.
fn handler_display_to_symbol_kind(d: &crate::model::file_analysis::HandlerDisplay) -> SymbolKind {
    use crate::model::file_analysis::HandlerDisplay as H;
    match d {
        H::Event => SymbolKind::EVENT,
        H::Method => SymbolKind::METHOD,
        H::Function => SymbolKind::FUNCTION,
        H::Field => SymbolKind::FIELD,
        H::Property => SymbolKind::PROPERTY,
        H::Constant => SymbolKind::CONSTANT,
        // Helper / Route / Task / Dispatch → FUNCTION. LSP's
        // `SymbolKind` enum is frozen; the distinguishing word lives
        // in `detail` / baked into `name` so client configs can
        // surface it without protocol extension.
        H::Helper | H::Route | H::Task | H::Action => SymbolKind::FUNCTION,
    }
}

fn handler_display_to_completion_kind(d: &crate::model::file_analysis::HandlerDisplay) -> CompletionItemKind {
    use crate::model::file_analysis::HandlerDisplay as H;
    match d {
        H::Event => CompletionItemKind::EVENT,
        H::Method => CompletionItemKind::METHOD,
        H::Function => CompletionItemKind::FUNCTION,
        H::Field => CompletionItemKind::FIELD,
        H::Property => CompletionItemKind::PROPERTY,
        H::Constant => CompletionItemKind::CONSTANT,
        H::Helper | H::Route | H::Task | H::Action => CompletionItemKind::FUNCTION,
    }
}

/// The LSP `SymbolKind` we'd emit for an outline node. Pulled out so
/// tests can pin behavior without reconstructing the conversion.
pub fn outline_lsp_kind(s: &OutlineSymbol) -> SymbolKind {
    match s.handler_display {
        Some(ref d) => handler_display_to_symbol_kind(d),
        None => fa_sym_kind_to_lsp(&s.kind),
    }
}

#[allow(deprecated)]
fn outline_to_document_symbol(s: &OutlineSymbol) -> DocumentSymbol {
    let children: Vec<DocumentSymbol> = s.children.iter().map(outline_to_document_symbol).collect();
    let kind = outline_lsp_kind(s);
    DocumentSymbol {
        name: s.name.clone(),
        detail: s.detail.clone(),
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(s.span),
        selection_range: span_to_range(s.selection_span),
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    }
}

mod outline;
pub use outline::*;
mod navigate;
pub use navigate::*;
mod links;
pub use links::*;
mod completion;
pub use completion::*;
mod hover;
pub use hover::*;
mod signature;
pub use signature::*;
mod tokens;
pub use tokens::*;
mod diagnostics;
pub use diagnostics::*;
pub use diagnostics::DiagnosticOptions;
mod code_actions;
pub use code_actions::*;

#[cfg(test)]
mod tests;
