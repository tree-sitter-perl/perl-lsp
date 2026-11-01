//! Module resolver: the background thread that lands dependency files for
//! every served language — Perl's `@INC`, Composer's `vendor/`, a venv's
//! `site-packages` — through the driver's dependency map (`acquire`), plus
//! the workspace bulk indexers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{notification, request};
use tower_lsp::Client;

use crate::index::module_cache;
use crate::index::module_index::{
    CachedModule, IndexCore, Providers, ResolveQueue,
};

/// Callback invoked after each module is resolved. Used to trigger diagnostic refresh.
pub type OnResolved = Box<dyn Fn() + Send + Sync>;

/// The server-session half of the resolver: the LSP client for progress
/// reporting plus the diagnostics-refresh callback, and the server-only
/// warmup lanes keyed off its presence (builtins hydration, warm-copy
/// strip, stale priority re-resolution, the declared dependencies'
/// progress). `None` ⇒ headless (one-shot CLI, tests): the SAME per-name
/// acquisition, none of the warmup.
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

/// Headless resolver — no Client, no LSP progress. Same map, store warm and
/// acquisition as the full resolver (one loop body, not a copy). Serves
/// tests AND one-shot CLI sessions (`ModuleIndex::new_for_cli`).
#[doc(hidden)]
pub fn spawn_test_resolver(core: Arc<IndexCore>) {
    spawn_loop("module-resolver-test", core, None);
}

fn spawn_loop(name: &str, core: Arc<IndexCore>, server: Option<ServerSession>) {
    // A test's store reaches the resolver it spawns, so its landings persist.
    #[cfg(test)]
    let store = module_cache::test_cache_dir();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            #[cfg(test)]
            if let Some((dir, busy)) = store {
                return module_cache::with_test_cache_dir(&dir, busy, || resolver_loop(core, server));
            }
            resolver_loop(core, server)
        })
        .expect("failed to spawn module-resolver thread");
}

pub use crate::build::builder::create_parser;

mod acquire;
#[cfg(test)]
pub(crate) use acquire::{parse_from_inc, perl_snapshot};
use acquire::*;
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
#[cfg(test)]
#[path = "search_roots_tests.rs"]
mod search_roots_tests;
