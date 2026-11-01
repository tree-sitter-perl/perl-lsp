//! The open documents: the files the editor holds, each a full `Document`
//! (tree + text + stable outline + analysis) because cursor-time queries need
//! the tree, and because they are what diagnostics publish to.
//!
//! Every other file — the project's own and its dependencies — lives in the
//! `ModuleIndex`, the one per-path registry. An open document SHADOWS its
//! index copy (it is the fresher text); walks claim open paths first and
//! skip the index entry for them.

use crate::util::canonical_path::CanonicalPath;
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
    /// the phase 3/4 merge that folds deps into OpenDocs.)
    Dependency,
    /// Pre-seeded built-in symbols (future).
    BuiltIn,
}

/// Identifier used by callers who want a role-tagged lookup.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FileKey {
    Path(CanonicalPath),
    /// A document addressed by URL, with the canonical path it names (empty
    /// when it names no file). Build it with `OpenDocs::key_of`, so a Url
    /// key and a Path key for one file compare by the same spelling.
    Url(Url, CanonicalPath),
}

impl FileKey {
    /// The canonical file this key names; `None` for a URL that names no
    /// file.
    pub fn canonical_path(&self) -> Option<&CanonicalPath> {
        match self {
            FileKey::Path(p) => Some(p),
            FileKey::Url(_, p) => (!p.as_os_str().is_empty()).then_some(p),
        }
    }
}

/// A document URL's canonical spelling: the file it names. A URL that
/// names no file (`untitled:`) gets the empty path, which no index fact is
/// filed under — resolving its text would ask the filesystem about a
/// relative path that means nothing.
fn canonical_spelling(url: &Url) -> CanonicalPath {
    url.to_file_path().map(CanonicalPath::resolve).unwrap_or_default()
}

/// The unified store.
pub struct OpenDocs {
    /// Open files, keyed by URL. Each carries a full Document.
    open: DashMap<Url, Document>,
    /// The driver every opened file is analyzed by — the process's served
    /// language, fixed for the store's life.
    driver: &'static dyn crate::build::language_driver::LanguageDriver,
}

impl Default for OpenDocs {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenDocs {
    /// A store for the process's served language.
    pub fn new() -> Self {
        Self::serving(crate::build::language_driver::LanguageRegistry::served())
    }

    /// A store for `driver`'s language. Production stores serve the process
    /// language (`new`); tests reach a pack language through here.
    pub fn serving(driver: &'static dyn crate::build::language_driver::LanguageDriver) -> Self {
        OpenDocs {
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
        let mut doc = if !driver.caps().hub_enrichment {
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
        doc.canonical_path = canonical_spelling(&url);
        self.open.insert(url, doc);
        true
    }

    /// Install an already-built Document — the CLI stages its query file
    /// this way, with the analysis it built and enriched itself.
    pub fn insert_open(&self, url: Url, mut doc: Document) {
        doc.canonical_path = canonical_spelling(&url);
        self.open.insert(url, doc);
    }

    /// The canonical file path a URL names: an open document's stamp,
    /// minted when it entered the store, else resolved here — a closed
    /// file still names itself (an origin queried by URL need not be
    /// open). `None` only when the URL names no file.
    ///
    /// Takes a shard read lock: the `for_each_open` deadlock rule applies.
    /// The key a URL-addressed request resolves under.
    ///
    /// Takes a shard read lock: the `for_each_open` deadlock rule applies.
    pub fn key_of(&self, url: &Url) -> FileKey {
        FileKey::Url(url.clone(), self.file_path_of(url).unwrap_or_default())
    }

    pub fn file_path_of(&self, url: &Url) -> Option<CanonicalPath> {
        let path = url.to_file_path().ok()?;
        if let Some(stamp) = self.open.get(url).map(|doc| doc.canonical_path.clone()) {
            return Some(stamp);
        }
        Some(CanonicalPath::resolve(path))
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
#[path = "open_docs_tests.rs"]
mod tests;
