//! Layer 0 — the data model. `FileAnalysis` is the single source of
//! truth; nothing here imports tree-sitter beyond `Point` or any upper
//! layer (enforced by `layering_tests`).

pub mod builtins;
pub mod conventions;
pub mod file_analysis;
pub mod graph;
pub mod surface;
pub mod trusted_decode;
pub mod witnesses;
