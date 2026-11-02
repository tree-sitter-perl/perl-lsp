//! Layer 3 — cross-file. Module/workspace indexing, the SQLite cache,
//! the unified `OpenDocs`, and `resolve` — the resolution
//! CandidateSet every cross-file query routes through.

pub mod builtins_pod;
pub mod document;
pub mod open_docs;
pub mod module_cache;
pub mod module_index;
pub mod module_resolver;
pub mod pack_bag_cache;
pub mod cache_policy;
pub mod closedness_store;
pub mod conclusion_cache;
pub mod conclusion_flush;
pub mod pack_invalidator;
pub mod resolve;
pub mod sweep;
