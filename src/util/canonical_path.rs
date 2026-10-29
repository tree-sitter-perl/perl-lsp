//! `CanonicalPath`: a file path in the spelling every cross-file fact is
//! keyed under, resolved once where the path enters the process.

use std::borrow::Borrow;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A path as `std::fs::canonicalize` spells it — the key `def_paths`,
/// include closures, ref rows and the index registries file a file under.
///
/// Minted where a path ENTERS (index registration, didOpen, a CLI
/// argument), so a query compares spellings and never asks the filesystem
/// again: `realpath` costs one `readlink` per path component, and a
/// per-query re-canonicalize was half of a heatmap sweep's kernel time
/// (`layering_tests::query_paths_never_canonicalize` pins the boundary).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct CanonicalPath(PathBuf);

impl CanonicalPath {
    /// Resolve `p` against the filesystem. A path that does not resolve (a
    /// deleted file, a synthetic key) keeps its own spelling.
    pub fn resolve(p: impl Into<PathBuf>) -> Self {
        let p = p.into();
        CanonicalPath(std::fs::canonicalize(&p).unwrap_or(p))
    }

    /// A spelling a `CanonicalPath` already produced, read back from the
    /// module cache it was written to. Re-resolving would re-key a row whose
    /// symlinks moved since it was persisted, orphaning its ref rows.
    pub fn from_stored(p: PathBuf) -> Self {
        CanonicalPath(p)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl Deref for CanonicalPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for CanonicalPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Borrow<Path> for CanonicalPath {
    fn borrow(&self) -> &Path {
        &self.0
    }
}

impl From<CanonicalPath> for PathBuf {
    fn from(p: CanonicalPath) -> PathBuf {
        p.0
    }
}

impl PartialEq<Path> for CanonicalPath {
    fn eq(&self, other: &Path) -> bool {
        self.0.as_path() == other
    }
}

impl PartialEq<PathBuf> for CanonicalPath {
    fn eq(&self, other: &PathBuf) -> bool {
        &self.0 == other
    }
}

// Lookups by a raw spelling are fine — a registry only refuses to be
// WRITTEN under one, and insertion takes a `CanonicalPath`.
impl Borrow<PathBuf> for CanonicalPath {
    fn borrow(&self) -> &PathBuf {
        &self.0
    }
}

impl PartialEq<&Path> for CanonicalPath {
    fn eq(&self, other: &&Path) -> bool {
        self.0.as_path() == *other
    }
}

impl PartialEq<CanonicalPath> for PathBuf {
    fn eq(&self, other: &CanonicalPath) -> bool {
        self == &other.0
    }
}
