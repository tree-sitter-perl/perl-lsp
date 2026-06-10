//! perl-lsp builder layer — the ONLY tree-sitter consumer (rule #1).
//! All CST traversal lives in `builder::build()` and its sanctioned
//! plugins; everything below gets a finished `FileAnalysis`.

pub mod builder;
pub mod cpanfile;
pub mod plugin;
pub mod pod;
pub mod query_cache;

// Lower-layer modules under their single-crate names, so `crate::…`
// paths inside this crate read identically to the pre-split tree.
pub use perl_lsp_cst as cst;
pub use perl_lsp_model::{conventions, file_analysis, witnesses};

#[cfg(test)]
pub use perl_lsp_index::{file_store, module_index, resolve};

#[cfg(test)]
#[path = "call_ref_index_tests.rs"]
mod call_ref_index_tests;
#[cfg(test)]
#[path = "return_expr_tests.rs"]
mod return_expr_tests;
#[cfg(test)]
#[path = "type_inference_invariants_tests.rs"]
mod type_inference_invariants_tests;
