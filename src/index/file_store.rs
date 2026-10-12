//! Unified file store: one home for every `FileAnalysis` the LSP has.
//!
//! Three roles live in one store:
//! - `Open`    — files the user is editing. Carry a full `Document` (tree + text +
//!               stable outline + analysis) because queries routinely need the tree
//!               for cursor-context-sensitive operations.
//! - `Workspace` — every `.pm`/`.pl`/`.t` in the project root. Stored as
//!               `Arc<FileAnalysis>` only; the tree is re-parsed on demand.
//! - `Dependency` — (owned by `ModuleIndex`, viewed here via secondary index)
//!               `@INC` modules. Not duplicated into this store — calls go
//!               through `ModuleIndex` as of phase 3. A future merge moves them
//!               under this same roof.
//!
//! A single path is never represented twice. Opening a workspace file promotes
//! it to `Open` (workspace entry removed); closing demotes it back.
//!
//! Queries that span files (rename, workspace/symbol, cross-file refs) iterate
//! this store uniformly via `for_each_analysis` — no per-role handler code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::lsp_types::Url;

use crate::index::document::Document;
use crate::model::file_analysis::FileAnalysis;

/// Role tag — the only behavioral difference between store entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FileRole {
    /// User is editing; full Document state available.
    Open,
    /// Project file, not currently open.
    Workspace,
    /// @INC module. (Managed by ModuleIndex; this variant is reserved for
    /// the phase 3/4 merge that folds deps into FileStore.)
    Dependency,
    /// Pre-seeded built-in symbols (future).
    BuiltIn,
}

/// Identifier used by callers who want a role-tagged lookup.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FileKey {
    Path(PathBuf),
    Url(Url),
}

/// The unified store.
pub struct FileStore {
    /// Open files, keyed by URL. Each carries a full Document.
    open: DashMap<Url, Document>,
    /// Workspace files, keyed by canonical path. Stored as Arc for cheap clones.
    workspace: DashMap<PathBuf, Arc<FileAnalysis>>,
    /// URL → canonical path, for open files. Lets us demote cleanly and
    /// prevents duplicate workspace entries for open files.
    url_to_path: DashMap<Url, PathBuf>,
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStore {
    pub fn new() -> Self {
        FileStore {
            open: DashMap::new(),
            workspace: DashMap::new(),
            url_to_path: DashMap::new(),
        }
    }

    // ---- Open-file mutation ----

    /// Open a file from text. Parses and builds analysis. If a workspace entry
    /// exists for the same path, it's replaced by the Open entry.
    pub fn open(&self, url: Url, text: String) -> bool {
        // Route by extension, falling back to a content sniff when no driver
        // claims it. A hub-enriched language (and a truly-unrecognized file,
        // which the fallback driver serves) keeps the native constructor —
        // `Document::new` is the reference pipeline the hub's freshness/
        // enrichment lanes are built around; the rest go through the generic
        // driver constructor.
        let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
        let path = url.to_file_path().ok();
        let driver = path
            .as_ref()
            .map(|p| reg.driver_or_fallback(p, &text))
            .unwrap_or_else(|| reg.fallback());
        let doc = if !driver.caps().hub_enrichment {
            match Document::new_routed(text, driver, path.clone()) {
                Some(d) => d,
                None => return false,
            }
        } else {
            match Document::new(text) {
                Some(d) => d,
                None => return false,
            }
        };
        if let Some(path) = path {
            self.workspace.remove(&path);
            self.url_to_path.insert(url.clone(), path);
        }
        self.open.insert(url, doc);
        true
    }

    /// Close an open file. If a path is known, demote to workspace (keeping
    /// the latest analysis snapshot).
    pub fn close(&self, url: &Url) {
        let doc = match self.open.remove(url) {
            Some((_, doc)) => doc,
            None => return,
        };
        if let Some((_, path)) = self.url_to_path.remove(url) {
            // Snapshot analysis into workspace under the CANONICAL spelling —
            // the bulk indexer keys canonically, and a URI spelled through a
            // symlinked root would otherwise coexist with the canonical entry
            // as two store keys for one file (double hits, double edits).
            let canon = std::fs::canonicalize(&path).unwrap_or(path.clone());
            self.workspace.remove(&path);
            self.workspace.insert(canon, doc.analysis);
        }
    }

    /// Immutable access to an open Document.
    ///
    /// The returned `Ref` pins its DashMap shard: holding it across ANY
    /// store mutation (`open`/`close`/`insert_workspace`) deadlocks when
    /// the other key lands on the same shard — a seed-dependent hang, not
    /// a deterministic failure. Snapshot what you need and drop the guard.
    pub fn get_open(&self, url: &Url) -> Option<dashmap::mapref::one::Ref<'_, Url, Document>> {
        self.open.get(url)
    }

    /// Mutable access to an open Document.
    pub fn get_open_mut(&self, url: &Url) -> Option<dashmap::mapref::one::RefMut<'_, Url, Document>> {
        self.open.get_mut(url)
    }

    /// THE writer of an open doc's conditioned view: re-derives, from the
    /// doc's pristine build, the copy with the deferred plugin emissions whose
    /// gate holds through the index right now, swaps it in via a short
    /// ptr-guarded write (a concurrent rebuild wins), and returns the view the
    /// caller should read. Every other cross-file fact is answered at use time
    /// through the index, so this is the whole of what an open doc owes the
    /// index after a build.
    pub fn condition_open(
        &self,
        url: &Url,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
    ) -> Option<Arc<FileAnalysis>> {
        let (built, current) = {
            let doc = self.open.get(url)?;
            (Arc::clone(&doc.built), Arc::clone(&doc.analysis))
        };
        let view = match built.with_holding_gates(idx) {
            Some(applied) => Arc::new(applied),
            None => Arc::clone(&built),
        };
        if let Some(mut doc) = self.open.get_mut(url) {
            if Arc::ptr_eq(&doc.analysis, &current) {
                doc.analysis = Arc::clone(&view);
            }
        }
        Some(view)
    }

    // ---- Workspace population ----

    /// Insert or replace a workspace entry, unless the same path is currently
    /// open (in which case the open entry is canonical).
    pub fn insert_workspace(&self, path: PathBuf, analysis: FileAnalysis) {
        self.insert_workspace_arc(path, Arc::new(analysis));
    }

    /// Pre-Arc'd variant — lets callers share the same analysis with
    /// other systems (e.g. registering the same module into
    /// ModuleIndex under its primary package name) without cloning
    /// the FileAnalysis twice at workspace-index startup.
    pub fn insert_workspace_arc(&self, path: PathBuf, analysis: Arc<FileAnalysis>) {
        // Compare canonically: the indexer passes canonical paths while
        // url_to_path holds URI spellings — under a symlinked root a raw
        // equality check never matches and an OPEN file gets a stale
        // workspace twin.
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let shadowed = self.url_to_path.iter().any(|e| {
            e.value() == &path
                || std::fs::canonicalize(e.value())
                    .map(|c| c == canon)
                    .unwrap_or(false)
        });
        if !shadowed {
            self.workspace.insert(path, analysis);
        }
    }

    /// Remove a workspace entry (e.g. file deletion via watcher).
    pub fn remove_workspace(&self, path: &Path) {
        self.workspace.remove(path);
    }

    /// Direct access to the workspace DashMap (for parallel indexing via Rayon
    /// and CLI tools that pre-populate then iterate). Values are `Arc<FileAnalysis>`.
    pub fn workspace_raw(&self) -> &DashMap<PathBuf, Arc<FileAnalysis>> {
        &self.workspace
    }

    /// Count of workspace entries.
    pub fn workspace_len(&self) -> usize {
        self.workspace.len()
    }

    // ---- Iteration ----

    /// Read-only iteration over open Documents. Query paths (`refs_to`,
    /// `references_mask_for`, CandidateSet projections) use this.
    ///
    /// DEADLOCK HAZARD: a caller must NOT hold a `get_open` read guard while
    /// this runs. `iter()` re-locks every shard, so it reentrantly read-locks
    /// the shard the guard already holds — and if a writer (an edit's rebuild
    /// or `enrich_open`'s swap) has queued on that shard in between,
    /// parking_lot's writer preference blocks the reentrant read behind the
    /// writer, which waits on the first read. Handlers snapshot
    /// (`Arc::clone(&doc.analysis)`) and drop the guard before calling
    /// `resolve()`. See `Document::analysis`.
    pub fn for_each_open<F: FnMut(&Url, &Document)>(&self, mut f: F) {
        for entry in self.open.iter() {
            f(entry.key(), entry.value());
        }
    }

    /// Call `f` for every file-path backed analysis in the store — open files
    /// first (canonical), then workspace files (skipping paths already covered
    /// by an open entry). Borrowed, not cloned.
    pub fn for_each_analysis<F: FnMut(FileKey, &FileAnalysis)>(&self, mut f: F) {
        let mut covered_paths = std::collections::HashSet::new();

        for entry in self.open.iter() {
            let url = entry.key().clone();
            if let Ok(path) = url.to_file_path() {
                // Claim the canonical spelling too (indexer keys canonically).
                if let Ok(canon) = std::fs::canonicalize(&path) {
                    covered_paths.insert(canon);
                }
                covered_paths.insert(path);
            }
            f(FileKey::Url(url), &entry.value().analysis);
        }

        for entry in self.workspace.iter() {
            if covered_paths.contains(entry.key()) {
                continue;
            }
            f(FileKey::Path(entry.key().clone()), entry.value());
        }
    }

    /// Count of open files.
    #[cfg(test)]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Count of workspace files.
    #[cfg(test)]
    pub fn workspace_count(&self) -> usize {
        self.workspace.len()
    }
}

#[cfg(test)]
#[path = "file_store_tests.rs"]
mod tests;
