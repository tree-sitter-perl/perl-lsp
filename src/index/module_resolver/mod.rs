//! Module resolver: background thread that resolves Perl modules from `@INC`.
//!
//! Discovers `@INC` paths, locates `.pm` files, parses them in-process with
//! tree-sitter-perl, and extracts export metadata for the module index.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{notification, request};
use tower_lsp::Client;
use tree_sitter::Parser;

use crate::build::cpanfile;
use crate::index::module_cache;
use crate::index::module_index::{
    CachedModule, IndexCore, Providers, ResolveQueue, WorkspaceRootChannel,
};

/// Callback invoked after each module is resolved. Used to trigger diagnostic refresh.
pub type OnResolved = Box<dyn Fn() + Send + Sync>;

/// The server-session half of the resolver: the LSP client for progress
/// reporting plus the diagnostics-refresh callback, and the server-only
/// warmup lanes keyed off its presence (builtins hydration, warm-copy
/// strip, stale priority re-resolution, cpanfile pre-scan, dependency
/// descent). `None` ⇒ headless (one-shot CLI, tests): the SAME per-module
/// resolve protocol, none of the warmup.
struct ServerSession {
    handle: tokio::runtime::Handle,
    client: Client,
    on_resolved: OnResolved,
}

/// Spawn the resolver thread for a server session. Returns immediately; the
/// thread runs in the background holding the same `Arc<IndexCore>` the
/// `ModuleIndex` wraps, so every shared-state operation goes through the one
/// `IndexCore` method set.
///
/// The `on_resolved` callback fires after each module is inserted into the cache,
/// allowing the backend to re-publish diagnostics.
pub fn spawn_resolver(core: Arc<IndexCore>, client: Client, on_resolved: OnResolved) {
    let handle = tokio::runtime::Handle::current();
    spawn_loop("module-resolver", core, Some(ServerSession { handle, client, on_resolved }));
}

/// Headless resolver — no Client, no LSP progress. Same @INC scan,
/// project-local lib discovery, SQLite warm/persist, and resolve protocol
/// as the full resolver (one loop body, not a copy). Serves tests AND
/// one-shot CLI sessions (`ModuleIndex::new_for_cli`), which previously had
/// NO resolver at all and could only read what editor sessions had cached.
#[doc(hidden)]
pub fn spawn_test_resolver(core: Arc<IndexCore>) {
    spawn_loop("module-resolver-test", core, None);
}

fn spawn_loop(name: &str, core: Arc<IndexCore>, server: Option<ServerSession>) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || resolver_loop(core, server))
        .expect("failed to spawn module-resolver thread");
}

pub use crate::build::builder::create_parser;

mod dependency;
pub use dependency::*;
mod inc;
pub use inc::*;
mod index_pack;
pub use index_pack::*;
mod index_perl;
pub use index_perl::*;
mod persist;
pub(crate) use persist::*;
mod thread;
pub use thread::uri_to_path;
use thread::*;

#[cfg(test)]
#[path = "module_resolver_tests.rs"]
mod tests;
#[cfg(all(test, feature = "php"))]
#[path = "dependency_tests.rs"]
mod dependency_tests;
#[cfg(all(test, feature = "python"))]
#[path = "dependency_python_tests.rs"]
mod dependency_python_tests;
