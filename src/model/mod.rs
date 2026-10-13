//! Layer 0 — the data model. `FileAnalysis` is the single source of
//! truth; nothing here imports tree-sitter beyond `Point` or any upper
//! layer (enforced by `layering_tests`).

// A relation dropped unread (`Tracked`) is a read no engine answer
// records, so here that is an error, not a warning.
#![deny(unused_must_use)]

pub mod builtins;
pub mod conventions;
pub mod file_analysis;
pub mod graph;
pub mod surface;
pub mod witnesses;
