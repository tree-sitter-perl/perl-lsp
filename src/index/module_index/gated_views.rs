//! The applied copies of gated files — the index half of the one gate seam
//! (`GatedEmission`). A reader of a closed file whose deferred plugin
//! emissions fire gets the copy with them applied; this store keeps those
//! copies so a repeat read is an `Arc` bump.
//!
//! An entry is valid for exactly the copy it was applied to and the verdicts
//! it was applied under: re-registration replaces the cached copy (its
//! generation and address move) and an ancestry change flips a verdict, so
//! either one misses and re-applies. Nothing invalidates an entry.
//!
//! Byte-capped, oldest-touched out first: an applied copy is a whole
//! analysis, and a DBIC schema is hundreds of gated classes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::model::file_analysis::FileAnalysis;

const CAP_BYTES: usize = 64 * 1024 * 1024;

struct Entry {
    generation: u64,
    source: usize,
    verdicts: Vec<bool>,
    analysis: Arc<FileAnalysis>,
    bytes: usize,
    touched: AtomicU64,
}

#[derive(Default)]
pub(crate) struct GatedViews {
    entries: DashMap<PathBuf, Entry>,
    bytes: AtomicUsize,
    clock: AtomicU64,
}

impl GatedViews {
    /// The applied copy of the file `registered` (its resident copy, at
    /// `generation`) under `verdicts`. A miss applies them to `whole()`.
    pub(crate) fn applied(
        &self,
        path: &Path,
        generation: u64,
        registered: &Arc<FileAnalysis>,
        verdicts: Vec<bool>,
        whole: impl FnOnce() -> Arc<FileAnalysis>,
    ) -> Arc<FileAnalysis> {
        let source_id = Arc::as_ptr(registered) as usize;
        if let Some(e) = self.entries.get(path) {
            if e.generation == generation && e.source == source_id && e.verdicts == verdicts {
                e.touched.store(self.tick(), Ordering::Relaxed);
                crate::util::ghost_stats::count("gated_view.hit");
                return e.analysis.clone();
            }
        }
        crate::util::ghost_stats::count("gated_view.apply");
        let analysis = Arc::new(whole().with_gated_emissions(&verdicts));
        let bytes = analysis.heap_estimate().total();
        let entry = Entry {
            generation,
            source: source_id,
            verdicts,
            analysis: analysis.clone(),
            bytes,
            touched: AtomicU64::new(self.tick()),
        };
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        if let Some(old) = self.entries.insert(path.to_path_buf(), entry) {
            self.bytes.fetch_sub(old.bytes, Ordering::Relaxed);
        }
        self.evict_to_cap(path);
        analysis
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Drop oldest-touched entries until under the cap, never `keep` (the
    /// copy just handed out stays retained even when it alone is over).
    fn evict_to_cap(&self, keep: &Path) {
        while self.bytes.load(Ordering::Relaxed) > CAP_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter(|e| e.key() != keep)
                .min_by_key(|e| e.touched.load(Ordering::Relaxed))
                .map(|e| e.key().clone());
            let Some(path) = oldest else { break };
            if let Some((_, e)) = self.entries.remove(&path) {
                self.bytes.fetch_sub(e.bytes, Ordering::Relaxed);
                crate::util::ghost_stats::count("gated_view.evict");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}
