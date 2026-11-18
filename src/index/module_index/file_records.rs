//! The per-path record of every file the index holds: the names it was
//! registered under and its role. The role is minted where the file lands —
//! a front door registers the project's own files, the `@INC` tier lands the
//! rest — and read through `FileRecords::role_of`, never inferred from which
//! table a path is missing from.

use crate::model::file_analysis::FileRole;
use crate::util::canonical_path::CanonicalPath;
use dashmap::DashMap;

pub(crate) struct FileRecord {
    /// `(name, declares-a-Class)` pairs, recorded from the WHOLE analysis
    /// because the resident copy's `symbols` may be evicted. For a
    /// workspace file they are the inverse `unregister_*` walks; for either
    /// role, the class-rank source and the file's own packages.
    pub(crate) names: Vec<(String, bool)>,
    pub(crate) role: FileRole,
}

#[derive(Default)]
pub(crate) struct FileRecords(DashMap<CanonicalPath, FileRecord>);

impl FileRecords {
    /// A front door registered `path`: the project's file, whatever an
    /// earlier landing said. Returns the names a previous workspace
    /// registration of `path` recorded, for the caller's re-registration
    /// inverse.
    pub(crate) fn register_workspace(&self, path: &CanonicalPath, names: Vec<(String, bool)>) -> Option<Vec<(String, bool)>> {
        self.0
            .insert(path.clone(), FileRecord { names, role: FileRole::Workspace })
            .filter(|old| old.role == FileRole::Workspace)
            .map(|old| old.names)
    }

    /// The `@INC` tier landed `path`. A workspace record is never demoted:
    /// project code the tier also finds (`lib/` on `@INC`, carton's `local/`
    /// inside the root) stays the project's.
    pub(crate) fn land_dependency(&self, path: &CanonicalPath, names: Vec<(String, bool)>) {
        use dashmap::mapref::entry::Entry;
        match self.0.entry(path.clone()) {
            Entry::Occupied(mut o) => {
                if o.get().role == FileRole::Dependency {
                    o.get_mut().names = names;
                }
            }
            Entry::Vacant(v) => {
                v.insert(FileRecord { names, role: FileRole::Dependency });
            }
        }
    }

    /// A front door's registration of `path` went away; its names, for the
    /// caller's inverse. A dependency record is the tier's, not the front
    /// door's, and stays.
    pub(crate) fn forget_workspace(&self, path: &CanonicalPath) -> Option<Vec<(String, bool)>> {
        self.0
            .remove_if(path, |_, r| r.role == FileRole::Workspace)
            .map(|(_, r)| r.names)
    }

    /// THE role read. `None`: the index holds no file at `path`.
    pub(crate) fn role_of(&self, path: &CanonicalPath) -> Option<FileRole> {
        self.0.get(path).map(|r| r.role)
    }

    /// The names `path` was recorded under, read without copying.
    pub(crate) fn with_names<R>(&self, path: &CanonicalPath, f: impl FnOnce(&[(String, bool)]) -> R) -> Option<R> {
        self.0.get(path).map(|r| f(&r.names))
    }
}
