//! SQLite persistence for the module cache (schema v10).
//!
//! Stores a full `Option<FileAnalysis>` per module, serialized via bincode
//! and compressed with zstd. Validates entries against mtime + file size to
//! detect stale data. Invalidates the dependency tier when `@INC` changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;
use rusqlite::{params, Connection};

use crate::model::file_analysis::FileAnalysis;
use crate::index::module_index::CachedModule;

/// The `source` tag for the dependency tier's rows: path-keyed like the
/// workspace's, one row per file whatever names the map placed it under,
/// read back by the landing that locates the file (`load_dependency_row`).
pub const DEPENDENCY_SOURCE: &str = "dependency";

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
pub(crate) mod sql;

#[cfg(test)]
#[path = "module_cache_tests.rs"]
mod tests;
