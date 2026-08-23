//! Layer 3 — cross-file. Module/workspace indexing, the SQLite cache,
//! the unified `FileStore`, and `resolve` — the resolution
//! CandidateSet every cross-file query routes through.

pub mod builtins_pod;
pub mod document;
pub mod file_store;
pub mod module_cache;
pub mod module_index;
pub mod module_resolver;
pub mod pack_bag_cache;
pub mod conclusion_cache;
pub mod pack_invalidator;
pub mod resolve;
