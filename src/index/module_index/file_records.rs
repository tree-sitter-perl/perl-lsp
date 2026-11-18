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
    /// A dependency's place in the map: each module name the map located
    /// this file under, with its precedence among that name's files (0 is
    /// what `require` loads). Minted by the landing, which has the map's
    /// order in hand; the name slot and the dependency inverse read it.
    /// Empty for a workspace file.
    pub(crate) loads_as: Vec<(String, usize)>,
    pub(crate) role: FileRole,
}

#[derive(Default)]
pub(crate) struct FileRecords(DashMap<CanonicalPath, FileRecord>);

impl FileRecords {
    /// A front door registered `path`: the project's file. Returns the names
    /// a previous workspace registration of `path` recorded, for the
    /// caller's re-registration inverse. A dependency landing at the same
    /// path is retired by the caller first (`IndexCore::claim_for_workspace`).
    pub(crate) fn register_workspace(&self, path: &CanonicalPath, names: Vec<(String, bool)>) -> Option<Vec<(String, bool)>> {
        self.0
            .insert(path.clone(), FileRecord { names, loads_as: Vec::new(), role: FileRole::Workspace })
            .filter(|old| old.role == FileRole::Workspace)
            .map(|old| old.names)
    }

    /// The `@INC` tier landed `path` under `loads_as` at that precedence.
    /// `false` when the path is the project's: a workspace record is never
    /// demoted, and the tier does not land it a second time.
    pub(crate) fn land_dependency(&self, path: &CanonicalPath, names: Vec<(String, bool)>, loads_as: (String, usize)) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.0.entry(path.clone()) {
            Entry::Occupied(mut o) => {
                if o.get().role != FileRole::Dependency {
                    return false;
                }
                let rec = o.get_mut();
                rec.names = names;
                match rec.loads_as.iter_mut().find(|(n, _)| *n == loads_as.0) {
                    Some(slot) => slot.1 = loads_as.1,
                    None => rec.loads_as.push(loads_as),
                }
                true
            }
            Entry::Vacant(v) => {
                v.insert(FileRecord { names, loads_as: vec![loads_as], role: FileRole::Dependency });
                true
            }
        }
    }

    /// A dependency landing went away; its record, for the caller's inverse.
    /// A workspace record is the front door's and stays.
    pub(crate) fn forget_dependency(&self, path: &CanonicalPath) -> Option<FileRecord> {
        self.0
            .remove_if(path, |_, r| r.role == FileRole::Dependency)
            .map(|(_, r)| r)
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

    /// `path`'s precedence among the files the map placed under `name`;
    /// `None` when it was not placed there.
    pub(crate) fn precedence(&self, path: &CanonicalPath, name: &str) -> Option<usize> {
        self.0
            .get(path)
            .and_then(|r| r.loads_as.iter().find(|(n, _)| n == name).map(|(_, p)| *p))
    }
}
