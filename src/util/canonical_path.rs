//! `CanonicalPath`: a file path in the spelling every cross-file fact is
//! keyed under, resolved once where the path enters the process.

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
    /// Resolve `p` against the filesystem. A deleted file resolves through
    /// its parent directory, so it keys where it was registered while it
    /// existed (a symlinked root otherwise leaves its delete un-findable);
    /// a path with no resolvable parent keeps its own spelling.
    pub fn resolve(p: impl Into<PathBuf>) -> Self {
        let p = p.into();
        if let Ok(c) = std::fs::canonicalize(&p) {
            return CanonicalPath(c);
        }
        let rejoined = match (p.parent(), p.file_name()) {
            (Some(dir), Some(name)) => std::fs::canonicalize(dir).ok().map(|d| d.join(name)),
            _ => None,
        };
        CanonicalPath(rejoined.unwrap_or(p))
    }

    /// A spelling a `CanonicalPath` already produced, read back from the
    /// module cache it was written to. Re-resolving would re-key a row whose
    /// symlinks moved since it was persisted, orphaning its ref rows.
    pub fn from_stored(p: PathBuf) -> Self {
        CanonicalPath(p)
    }

    /// A path a test names for a file that is never on disk, kept
    /// verbatim. Resolving it would ask the filesystem about the test box
    /// (macOS spells `/tmp` as `/private/tmp`), so fixture keys stay the
    /// same everywhere.
    #[cfg(test)]
    pub fn fixture(p: impl Into<PathBuf>) -> Self {
        CanonicalPath(p.into())
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

impl From<CanonicalPath> for PathBuf {
    fn from(p: CanonicalPath) -> PathBuf {
        p.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_deleted_file_keys_where_it_was_registered() {
        let base = std::env::temp_dir().join(format!("canon_path_test_{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let file = link.join("Gone.pm");
        std::fs::write(&file, "1;").unwrap();

        let registered = CanonicalPath::resolve(&file);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(CanonicalPath::resolve(&file), registered);

        std::fs::remove_dir_all(&base).unwrap();
    }
}
