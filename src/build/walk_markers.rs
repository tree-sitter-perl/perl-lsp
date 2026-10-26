//! What a workspace walk saw of a language's marker files: files whose
//! presence gives their directory a meaning (python's `__init__.py` makes
//! it a package). The index records each walk once, so a build asks the
//! snapshot which directories are marked instead of stating every
//! ancestor. A marker added or removed later is noted by the pack
//! invalidator, which bumps the generation and re-files what lies under
//! that directory. A path no walk covers asks the disk.
//!
//! The language says which file is its marker (`FileModuleRule::marker`);
//! nothing here knows what a marked directory means.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// One walk's view of one marker file name.
pub struct MarkerSnapshot {
    marker: &'static str,
    /// The walk root as the walk spelled it, then its canonical spelling
    /// when that differs: walked paths use the first, watcher events and
    /// persisted rows the second.
    roots: Vec<PathBuf>,
    generation: u64,
    /// Root-relative directories holding a walked file. The walker visits
    /// a directory's parent before it, so every ancestor of a covered
    /// directory up to the root was walked too, and a marker there would
    /// have been seen.
    covered: HashSet<PathBuf>,
    /// Root-relative directories holding the marker.
    marked: HashSet<PathBuf>,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static SNAPSHOTS: RwLock<Vec<MarkerSnapshot>> = RwLock::new(Vec::new());

impl MarkerSnapshot {
    /// The snapshot of a walk under `root` that yielded `files`.
    pub fn from_walk(marker: &'static str, root: &Path, files: &[PathBuf]) -> Self {
        let mut roots = vec![root.to_path_buf()];
        if let Ok(canon) = root.canonicalize() {
            if canon != root {
                roots.push(canon);
            }
        }
        let mut covered = HashSet::new();
        let mut marked = HashSet::new();
        for f in files {
            let Some(dir) = f.strip_prefix(root).ok().and_then(Path::parent) else { continue };
            if f.file_name().is_some_and(|n| n == marker) {
                marked.insert(dir.to_path_buf());
            }
            covered.insert(dir.to_path_buf());
        }
        MarkerSnapshot {
            marker,
            roots,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            covered,
            marked,
        }
    }

    /// Bumped whenever a marker's presence under this walk changes.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn relative<'p>(&self, path: &'p Path) -> Option<&'p Path> {
        self.roots.iter().find_map(|r| path.strip_prefix(r).ok())
    }

    fn covers_file(&self, file: &Path) -> bool {
        file.parent().and_then(|d| self.relative(d)).is_some_and(|rel| self.covered.contains(rel))
    }

    /// Record that the marker at `path` now exists (or not). Returns its
    /// directory when that changed whether the directory is marked. Coverage
    /// stays the walk's: a directory the walk never entered (ignored, or
    /// created since) keeps asking the disk, since its ancestors were not
    /// walked either.
    fn note(&mut self, path: &Path, exists: bool) -> Option<PathBuf> {
        let dir = self.relative(path)?.parent()?;
        let flipped = if exists { self.marked.insert(dir.to_path_buf()) } else { self.marked.remove(dir) };
        if !flipped {
            return None;
        }
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        path.parent().map(Path::to_path_buf)
    }
}

/// Install the snapshot of a walk, replacing an earlier walk of the same
/// root for the same marker.
pub fn record_walk(marker: &'static str, root: &Path, files: &[PathBuf]) {
    let snap = MarkerSnapshot::from_walk(marker, root, files);
    let mut all = SNAPSHOTS.write().unwrap_or_else(|e| e.into_inner());
    all.retain(|s| !(s.marker == marker && s.roots[0] == snap.roots[0]));
    all.push(snap);
}

/// A marker file was created (`exists`) or deleted. Returns the directory
/// whose marked state flipped, when a walk covers it and the state did
/// change — the files under it name differently now.
pub fn note_marker(marker: &'static str, path: &Path, exists: bool) -> Option<PathBuf> {
    if path.file_name().is_none_or(|n| n != marker) {
        return None;
    }
    let mut all = SNAPSHOTS.write().unwrap_or_else(|e| e.into_inner());
    all.iter_mut()
        .filter(|s| s.marker == marker)
        .find(|s| s.relative(path).is_some())
        .and_then(|s| s.note(path, exists))
}

/// The generation of the snapshot covering `file`'s directory, if any.
#[cfg_attr(not(test), allow(dead_code))]
pub fn generation_covering(marker: &'static str, file: &Path) -> Option<u64> {
    let all = SNAPSHOTS.read().unwrap_or_else(|e| e.into_inner());
    all.iter().find(|s| s.marker == marker && s.covers_file(file)).map(MarkerSnapshot::generation)
}

/// Run `f` with the marked directories as seen for `file`: the snapshot of
/// the walk covering its directory, else the disk.
pub fn with_dirs<R>(marker: &'static str, file: &Path, f: impl FnOnce(&MarkedDirs<'_>) -> R) -> R {
    let all = SNAPSHOTS.read().unwrap_or_else(|e| e.into_inner());
    let snapshot = all.iter().find(|s| s.marker == marker && s.covers_file(file));
    f(&MarkedDirs { marker, snapshot })
}

/// Which directories hold the marker, for one file's naming.
pub struct MarkedDirs<'a> {
    marker: &'a str,
    snapshot: Option<&'a MarkerSnapshot>,
}

impl<'a> MarkedDirs<'a> {
    /// Answer from the disk alone.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn on_disk(marker: &'a str) -> Self {
        MarkedDirs { marker, snapshot: None }
    }

    /// Answer from `snapshot`, for paths under its root.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn over(snapshot: &'a MarkerSnapshot) -> Self {
        MarkedDirs { marker: snapshot.marker, snapshot: Some(snapshot) }
    }

    /// Does `dir` hold the marker? Above the walk root the snapshot saw
    /// nothing, so the disk answers there.
    pub fn is_marked(&self, dir: &Path) -> bool {
        match self.snapshot.and_then(|s| Some((s, s.relative(dir)?))) {
            Some((s, rel)) => s.marked.contains(rel),
            None => dir.join(self.marker).is_file(),
        }
    }

    /// `dir` relative to the root the file's module is named from: the
    /// walk's root, else the workspace root.
    pub fn root_relative(&self, dir: &Path) -> Option<PathBuf> {
        match self.snapshot {
            Some(s) => s.relative(dir).map(Path::to_path_buf),
            None => crate::build::plugin::rhai_host::workspace_root()
                .and_then(|root| dir.strip_prefix(root).ok().map(Path::to_path_buf)),
        }
    }
}
