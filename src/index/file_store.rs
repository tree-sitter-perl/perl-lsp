//! The open documents: the files the editor holds, each a full `Document`
//! (tree + text + stable outline + analysis) because cursor-time queries need
//! the tree, and because they are what diagnostics publish to.
//!
//! Every other file — the project's own and its dependencies — lives in the
//! `ModuleIndex`, the one per-path registry. An open document SHADOWS its
//! index copy (it is the fresher text); walks claim open paths first and
//! skip the index entry for them.

use std::path::PathBuf;
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
    /// The driver every opened file is analyzed by — the process's served
    /// language, fixed for the store's life.
    driver: &'static dyn crate::build::language_driver::LanguageDriver,
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStore {
    /// A store for the process's served language.
    pub fn new() -> Self {
        Self::serving(crate::build::language_driver::LanguageRegistry::served())
    }

    /// A store for `driver`'s language. Production stores serve the process
    /// language (`new`); tests reach a pack language through here.
    pub fn serving(driver: &'static dyn crate::build::language_driver::LanguageDriver) -> Self {
        FileStore {
            open: DashMap::new(),
            driver,
        }
    }

    // ---- Open-file mutation ----

    /// Open a file from text. Parses and builds analysis.
    pub fn open(&self, url: Url, text: String) -> bool {
        // A hub-enriched language keeps the native constructor —
        // `Document::new` is the reference pipeline the hub's freshness/
        // enrichment lanes are built around; the rest go through the generic
        // driver constructor.
        let path = url.to_file_path().ok();
        let driver = self.driver;
        if let Some(p) = &path {
            if let Err(why) = crate::build::language_driver::LanguageRegistry::admits(driver, p) {
                log::info!("not opening: {why}");
                return false;
            }
        }
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
        self.open.insert(url, doc);
        true
    }

    /// Install an already-built Document — the CLI stages its query file
    /// this way, with the analysis it built and enriched itself.
    pub fn insert_open(&self, url: Url, doc: Document) {
        self.open.insert(url, doc);
    }

    /// Close an open file. Its index copy (the on-disk text) answers from
    /// here on — an unsaved buffer's edits close with it.
    pub fn close(&self, url: &Url) {
        self.open.remove(url);
    }

    /// Immutable access to an open Document.
    ///
    /// The returned `Ref` pins its DashMap shard: holding it across ANY
    /// store mutation (`open`/`close`) deadlocks when
    /// the other key lands on the same shard — a seed-dependent hang, not
    /// a deterministic failure. Snapshot what you need and drop the guard.
    pub fn get_open(&self, url: &Url) -> Option<dashmap::mapref::one::Ref<'_, Url, Document>> {
        self.open.get(url)
    }

    /// Mutable access to an open Document.
    pub fn get_open_mut(&self, url: &Url) -> Option<dashmap::mapref::one::RefMut<'_, Url, Document>> {
        self.open.get_mut(url)
    }

    /// THE writer of open-doc cross-file enrichment: derives the enriched
    /// analysis as a clone OFF the store lock, swaps it in via a short
    /// ptr-guarded write, and returns it for the caller to read (diagnostics)
    /// without re-locking. Perl-only — pack analyses have no import
    /// enrichment and are returned untouched. Idempotent through
    /// `enrich_imported_types_with_keys`'s base_*_count truncation; a
    /// concurrent rebuild wins the swap (the ptr guard drops this derivation)
    /// and the next publish re-derives against the newer build. The doc's
    /// `baseline_surface` is untouched by construction — freshness records
    /// stay enrichment-invariant no matter when this runs.
    pub fn enrich_open(
        &self,
        url: &Url,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
    ) -> Option<Arc<FileAnalysis>> {
        // The heal runs the SAME cross-file cascade a verb does, on a
        // background thread, and it is the other half of what made
        // `references` never return at 138k files. It is a walk: give it
        // the memo and the budget (`docs/adr/resolution-session.md`).
        let _session = crate::model::witnesses::ResolutionSession::enter(Some(idx));
        let (base, language) = {
            let doc = self.open.get(url)?;
            (Arc::clone(&doc.analysis), doc.language)
        };
        // Enrichment is the hub lane; other languages answer as-built.
        if !crate::build::language_driver::LanguageRegistry::caps(language).hub_enrichment {
            return Some(base);
        }
        crate::util::ghost_stats::count("enrich_open.perl");
        let mut fa = (*base).clone();
        fa.enrich_imported_types_with_keys_for(
            Some(idx),
            url.to_file_path().ok().as_deref(),
        );
        let enriched = Arc::new(fa);
        if let Some(mut doc) = self.open.get_mut(url) {
            if Arc::ptr_eq(&doc.analysis, &base) {
                doc.analysis = Arc::clone(&enriched);
            }
        }
        Some(enriched)
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

    /// Count of open files.
    #[cfg(test)]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

}

#[cfg(test)]
#[path = "file_store_tests.rs"]
mod tests;
