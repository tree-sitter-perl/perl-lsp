//! perl-lsp data model — layer 4 of the architecture (CLAUDE.md).
//! `FileAnalysis` is the single source of truth; the witness bag is
//! the only source of types; `conventions` is pure-`&str` Perl name
//! semantics. This crate's Cargo manifest IS rule #2: tree-sitter is
//! a types-only dependency (`Point`), no grammar, no `cst` — a tree
//! walk here cannot compile against anything.

pub mod conventions;
pub mod file_analysis;
pub mod witnesses;

// Tests exercise the model through the builder (the only producer of
// real FileAnalyses). Dev-dependency cycles are sanctioned by Cargo;
// they don't enter the main graph.
#[cfg(test)]
pub use perl_lsp_build::builder;
#[cfg(test)]
pub use perl_lsp_index::module_index;
