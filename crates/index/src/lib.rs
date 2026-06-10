//! perl-lsp cross-file layer: open+workspace FileStore, the module
//! index/resolver/cache trio, and `refs_to` resolution. Async
//! handlers above only ever touch `_cached` methods; the resolver
//! thread owns FS I/O.

pub mod builtins_pod;
pub mod document;
pub mod file_store;
pub mod module_cache;
pub mod module_index;
pub mod module_resolver;
pub mod resolve;
pub mod timings;

pub use perl_lsp_build::{builder, cpanfile, plugin, pod};
pub use perl_lsp_cst as cst;
pub use perl_lsp_model::{conventions, file_analysis, witnesses};

#[cfg(test)]
#[path = "parametric_resultset_tests.rs"]
mod parametric_resultset_tests;
