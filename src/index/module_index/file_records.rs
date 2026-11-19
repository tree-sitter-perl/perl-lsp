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
    /// A dependency's places in the map: each module name the map located
    /// this file under. Minted by the landing, which has the map's order in
    /// hand; the name slot and the dependency inverse read it. Empty for a
    /// workspace file.
    pub(crate) loads_as: Vec<Placement>,
    /// A dependency's source stamp when it landed: a later ask restamps the
    /// held copy only while the file on disk is still the one parsed.
    pub(crate) stamp: Option<(i64, i64)>,
    pub(crate) role: FileRole,
}

/// One module name a map placed a dependency under.
pub(crate) struct Placement {
    pub(crate) name: String,
    /// Its rank among that name's files (0 is what `require` loads).
    pub(crate) precedence: usize,
    /// The map generation that last placed it. A placement of an older
    /// generation is not current, and the idle sweep takes it back.
    pub(crate) map_gen: u64,
}

/// `FileRecords::placement_of`'s answer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldAs {
    Project,
    Placed,
    Other,
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
            .insert(path.clone(), FileRecord { names, loads_as: Vec::new(), stamp: None, role: FileRole::Workspace })
            .filter(|old| old.role == FileRole::Workspace)
            .map(|old| old.names)
    }

    /// The dependency tier landed `path` under `placement`, read at `stamp`.
    /// `false` when the path is the project's: a workspace record is never
    /// demoted, and the tier does not land it a second time.
    pub(crate) fn land_dependency(
        &self,
        path: &CanonicalPath,
        names: Vec<(String, bool)>,
        placement: Placement,
        stamp: Option<(i64, i64)>,
    ) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.0.entry(path.clone()) {
            Entry::Occupied(mut o) => {
                if o.get().role != FileRole::Dependency {
                    return false;
                }
                let rec = o.get_mut();
                rec.names = names;
                rec.stamp = stamp;
                match rec.loads_as.iter_mut().find(|p| p.name == placement.name) {
                    Some(slot) => *slot = placement,
                    None => rec.loads_as.push(placement),
                }
                true
            }
            Entry::Vacant(v) => {
                v.insert(FileRecord { names, loads_as: vec![placement], stamp, role: FileRole::Dependency });
                true
            }
        }
    }

    /// What the index holds at a path the map placed under `name`: the
    /// project's file (never the tier's), a dependency placed there still
    /// read at the stamp `stamp` gives — what a restamp may carry over
    /// without a parse — or anything else. `stamp` is asked only for a
    /// dependency placed under `name`, so a fresh ask pays no stat.
    pub(crate) fn placement_of(
        &self,
        path: &CanonicalPath,
        name: &str,
        stamp: impl FnOnce() -> Option<(i64, i64)>,
    ) -> HeldAs {
        let Some(r) = self.0.get(path) else {
            return HeldAs::Other;
        };
        match r.role {
            FileRole::Workspace => HeldAs::Project,
            FileRole::Dependency if r.loads_as.iter().any(|p| p.name == name) => {
                let now = stamp();
                if now.is_some() && r.stamp == now {
                    HeldAs::Placed
                } else {
                    HeldAs::Other
                }
            }
            _ => HeldAs::Other,
        }
    }

    /// A move's restamp: `path`'s placement under `name` is the current map's
    /// too, at `precedence`.
    pub(crate) fn restamp(&self, path: &CanonicalPath, name: &str, precedence: usize, map_gen: u64) {
        if let Some(mut r) = self.0.get_mut(path) {
            if let Some(p) = r.loads_as.iter_mut().find(|p| p.name == name) {
                p.precedence = precedence;
                p.map_gen = map_gen;
            }
        }
    }

    /// Whether `path` is a current candidate under `name` at `map_gen`: a
    /// workspace file always is, a dependency only through a placement of
    /// that generation. A path with no record is not the tier's to judge.
    pub(crate) fn placed_at(&self, path: &CanonicalPath, name: &str, map_gen: u64) -> bool {
        self.0.get(path).is_none_or(|r| {
            r.role != FileRole::Dependency || r.loads_as.iter().any(|p| p.name == name && p.map_gen == map_gen)
        })
    }

    /// Every dependency placement older than `map_gen`, as `(path, name)`.
    pub(crate) fn stale_placements(&self, map_gen: u64) -> Vec<(CanonicalPath, String)> {
        let mut out = Vec::new();
        for r in self.0.iter() {
            if r.role != FileRole::Dependency {
                continue;
            }
            for p in &r.loads_as {
                if p.map_gen < map_gen {
                    out.push((r.key().clone(), p.name.clone()));
                }
            }
        }
        out
    }

    /// Take back `path`'s placement under `name`. `Some(true)` when it was
    /// the last one and the record went with it.
    pub(crate) fn unplace(&self, path: &CanonicalPath, name: &str) -> Option<bool> {
        let emptied = {
            let mut r = self.0.get_mut(path)?;
            if r.role != FileRole::Dependency {
                return None;
            }
            r.loads_as.retain(|p| p.name != name);
            r.loads_as.is_empty()
        };
        if emptied {
            self.0.remove_if(path, |_, r| r.role == FileRole::Dependency && r.loads_as.is_empty());
        }
        Some(emptied)
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
            .and_then(|r| r.loads_as.iter().find(|p| p.name == name).map(|p| p.precedence))
    }
}
