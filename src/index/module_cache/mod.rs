//! SQLite persistence for the module cache (schema v10).
//!
//! Stores a full `Option<FileAnalysis>` per module, serialized via bincode
//! and compressed with zstd. Validates entries against mtime + file size to
//! detect stale data. Invalidates the entire cache when `@INC` changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;
use rusqlite::{params, Connection};

use crate::model::file_analysis::FileAnalysis;
use crate::index::module_index::CachedModule;

/// The `source` tag for name-keyed rows — the @INC provider tier, whoever
/// resolved it. The resolver thread and the one-shot CLI share one pool:
/// tagging by writer instead of by keying scheme made every CLI-resolved
/// row write-only (nothing read them back), so each CLI verb re-resolved
/// the whole tier. Path-keyed rows use their own tags (`workspace`) and
/// stream through `warm_cache_streaming`; only name-keyed rows belong here.
pub const NAME_KEYED_SOURCE: &str = "import";

mod blob;
pub use blob::*;
mod conn;
pub use conn::*;
mod rows;
pub use rows::*;
mod schema;
pub use schema::*;
mod stubs;
pub use stubs::*;
mod conclusions_store;
pub use conclusions_store::*;
mod warm;
pub use warm::*;

#[cfg(test)]
#[path = "module_cache_tests.rs"]
mod tests;
