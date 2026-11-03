//! `CorpusSweep`: a batch verb's walk over every registered file.

use std::sync::Arc;

use crate::index::module_index::{ModuleIndex, SweepProbeGuard};
use crate::index::resolve::RetrievalMemoGuard;
use crate::model::file_analysis::{CrossFileLookup, FileAnalysis};
use crate::util::canonical_path::CanonicalPath;

/// One whole-index batch sweep (`--heatmap`, `--refs-parity`). Opening it
/// sizes the rehydration cache to the corpus and opens the sweep memos; the
/// corpus itself is only reachable through it, so no sweep can decode a file
/// before the cache is big enough to keep it. Decoding first left the stock
/// cap to evict every copy, which the sweep then decoded a second time while
/// the first stayed pinned (FHEM: 3.77 GB peak, 2,483 decodes for ~1,000
/// files; `/mnt/project-files/design/heatmap-page-faults.md`).
pub struct CorpusSweep<'a> {
    idx: &'a ModuleIndex,
    cache_cap: Option<usize>,
    _retrieval: RetrievalMemoGuard,
    _probes: SweepProbeGuard,
}

impl<'a> CorpusSweep<'a> {
    pub fn open(idx: &'a ModuleIndex) -> Self {
        let cache_cap = idx.size_bag_cache_for_sweep();
        CorpusSweep {
            idx,
            cache_cap,
            _retrieval: RetrievalMemoGuard::open(),
            _probes: SweepProbeGuard::open(),
        }
    }

    /// The rehydration cap in effect for the sweep, in bytes, when the
    /// index has a cache installed.
    pub fn cache_cap(&self) -> Option<usize> {
        self.cache_cap
    }

    /// Every registered project file, whole, path-sorted so a report built
    /// from it is deterministic. The copies are the cache's own entries.
    pub fn registered_whole(&self) -> Vec<(CanonicalPath, Arc<FileAnalysis>)> {
        let mut entries = Vec::new();
        self.idx.for_each_registered_file(&mut |cached| {
            entries.push((cached.path.clone(), self.idx.whole_present(cached)));
        });
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }
}
