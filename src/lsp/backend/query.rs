//! The single blocking hop for query verbs (`run_query`) and the context
//! (`QueryCx`) a blocking closure resolves through.
//!
//! Set construction/projection, the relational row search, and the
//! rehydration readers all do real I/O (SQLite + zstd decode on LRU miss,
//! `fs` reads) — so the blocking decision rides THIS API, not per-handler
//! memory: a `QueryCx` is minted only inside `run_query`'s
//! `spawn_blocking`, so holding one proves the code is off the reactor.
//! `layering_tests::query_verbs_route_through_run_query` pins the raw
//! spellings out of the handler file.
//!
//! WHY THE DISCIPLINE IS ABSOLUTE: tower-lsp 0.20's `serve()` polls the
//! stdin reader and EVERY handler future inside ONE `join!`ed task —
//! `buffer_unordered(4)` gives concurrency within that single task, not
//! across threads. Synchronous CPU inside any handler future therefore
//! stalls every other in-flight verb AND the message reader until it
//! yields (measured: one inline open-doc enrichment against a 138k-file
//! workspace took 344 s and made every hover/definition/completion time
//! out behind it — the post-cold-index availability hole). The rule this
//! implies: NO synchronous CPU in a handler future, ever — heavy work
//! goes through `run_query`, `spawn_blocking` (diagnostics derivation:
//! `DiagCtx::publish`), or a spawned task.

use super::*;

impl Backend {
    /// The one blocking hop for query verbs: moves the store + hub handles
    /// onto the blocking pool and runs `f` there. Handlers snapshot their
    /// open-doc state (`Arc<FileAnalysis>`, text, language) BEFORE calling —
    /// the guard discipline on `Document::analysis` is unchanged.
    pub(super) async fn run_query<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&QueryCx) -> T + Send + 'static,
        T: Send + 'static,
    {
        let cx = QueryCx {
            open_docs: Arc::clone(&self.open_docs),
            module_index: Arc::clone(&self.module_index),
        };
        tokio::task::spawn_blocking(move || f(&cx))
            .await
            .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())
    }
}

/// What a blocking query closure reaches the stores through. Only
/// `run_query` constructs one.
pub(super) struct QueryCx {
    open_docs: Arc<OpenDocs>,
    module_index: Arc<ModuleIndex>,
}

impl QueryCx {
    pub(super) fn open_docs(&self) -> &OpenDocs {
        &self.open_docs
    }

    pub(super) fn index(&self) -> &ModuleIndex {
        &self.module_index
    }

    /// The process index as the lookup trait `set()` and the slot-shaped
    /// pre-set lanes (include tokens, raw-word fallbacks) consume.
    pub(super) fn lookup(&self) -> &dyn crate::model::file_analysis::CrossFileLookup {
        &*self.module_index
    }

    /// The one set-construction spelling behind the blocking hop.
    pub(super) fn set<'a>(
        &'a self,
        lookup: &'a dyn crate::model::file_analysis::CrossFileLookup,
        analysis: &'a crate::model::file_analysis::FileAnalysis,
        uri: &Url,
        point: tree_sitter::Point,
        scope: crate::index::resolve::OverrideScope,
    ) -> crate::index::resolve::CandidateSet<'a> {
        crate::index::resolve::resolve(
            &self.open_docs,
            analysis,
            FileKey::Url(uri.clone()),
            point,
            Some(lookup),
            scope,
        )
    }

    /// `set()` for a caller that already holds a `FileKey` — the hierarchy
    /// handlers re-anchor at an ITEM's file, which is Path-keyed when closed.
    pub(super) fn set_at<'a>(
        &'a self,
        lookup: &'a dyn crate::model::file_analysis::CrossFileLookup,
        analysis: &'a crate::model::file_analysis::FileAnalysis,
        key: FileKey,
        point: tree_sitter::Point,
        scope: crate::index::resolve::OverrideScope,
    ) -> crate::index::resolve::CandidateSet<'a> {
        crate::index::resolve::resolve(&self.open_docs, analysis, key, point, Some(lookup), scope)
    }

    /// The analysis + key a hierarchy ITEM re-anchors at.
    /// Hierarchy requests (supertypes/subtypes/incoming/outgoing) hand back
    /// an item pointing at a file that need not be open; this resolves it
    /// across the same tiers the reference walk sweeps (open doc, workspace
    /// entry, cached registration — rehydrated when evicted). Closed files
    /// key by Path so tier attribution matches the CLI mirrors.
    pub(super) fn item_anchor(
        &self,
        uri: &Url,
    ) -> Option<(Arc<crate::model::file_analysis::FileAnalysis>, FileKey)> {
        if let Some(doc) = self.open_docs.get_open(uri) {
            return Some((Arc::clone(&doc.analysis), FileKey::Url(uri.clone())));
        }
        let path = uri.to_file_path().ok()?;
        let key = FileKey::Path(path);
        let analysis =
            crate::index::resolve::analysis_for_key(&self.open_docs, Some(&*self.module_index), &key)?;
        Some((analysis, key))
    }

    /// workspace/symbol's relational pass (SQLite over the served language's rows).
    pub(super) fn sym_rows(&self, query: &str) -> Vec<crate::index::module_cache::SymRowHit> {
        self.module_index.sym_search(query)
    }
}

