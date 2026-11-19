//! The dependency map: which files the dependency tier holds for a name.
//! It is data — an ordered list of lanes, each a mechanism the engine
//! knows, plus the directories the tier owns and the manifests the map was
//! read from. A language owns only the reader that produces it (perl:
//! `build::inc`, php: `build::composer`); nothing here names a language or
//! a package manager.

use crate::util::canonical_path::CanonicalPath;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How a language spells a module as a file under a directory: the files a
/// module name may be, most-preferred first, and the name a root-relative
/// file spells (`None`: no module's file).
#[derive(Clone, Copy)]
pub struct ModuleSpelling {
    pub module_paths: fn(module: &str) -> Vec<String>,
    pub module_of: fn(rel: &Path) -> Option<String>,
}

/// One mechanism of a map. Lane order is precedence: a name's files come
/// back in the order of the lanes that place them, then each lane's own
/// order.
///
/// TODO(dependency-lane-id): nothing reads a lane's meaning for its
/// language yet. When something must, the lane carries an opaque
/// `LaneId(u8)` its reader assigns, as `NamespaceId(u8)` does for a
/// language's namespaces — never a variant named for a language.
/// TODO(dependency-custom-lane): a `Custom(LaneId)` lane, located by the
/// language driver, for a mechanism no variant here expresses.
pub enum Lane {
    /// An exact name → file table, one file per name (the first row a
    /// reader gives for a name). Kept sorted by name, so a lookup is a
    /// search, not a scan.
    Table(Vec<(String, PathBuf)>),
    /// A name under `prefix` is a file under one of `dirs`, spelled by
    /// `spelling` from what `spells` says.
    Prefix { prefix: String, dirs: Vec<PathBuf>, spelling: ModuleSpelling, spells: Spells },
    /// A name is a file under one of `roots`, in root order.
    SearchRoots { roots: Vec<PathBuf>, spelling: ModuleSpelling },
    /// Files that load whatever is asked; no name places them.
    AlwaysLoad(Vec<PathBuf>),
}

/// What a `Prefix` lane's spelling spells under its dirs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Spells {
    /// The name past the prefix (`App\Models\User` under `App\` is
    /// `Models/User.php`).
    Remainder,
    /// The whole name, the prefix too (`Old_Tool` under `Old_` is
    /// `Old/Tool.php`).
    WholeName,
}

/// What an asker hands the tier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ask {
    /// A module name or identity, as the language spells it.
    Name(String),
    /// A file by path.
    #[allow(dead_code)] // no asker yet: perl asks by name
    File(CanonicalPath),
}

/// A file the map places for an ask, and the module name it loads as there
/// (`None`: the map places it under no name). Minted by the map, so no
/// consumer re-derives a name from the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub path: CanonicalPath,
    pub loads_as: Option<String>,
}

pub struct DependencyMap {
    lanes: Vec<Lane>,
    /// Every `SearchRoots` lane's roots in lane order, canonical: the
    /// search path a visibility rank prefix-matches candidates against,
    /// computed once because that runs on request paths.
    search_roots: Arc<Vec<PathBuf>>,
    /// The directories whose files are the tier's, never the walk's, each
    /// as read and canonical.
    tier_roots: Vec<PathBuf>,
    /// The manifests the map was read from; their stamps are part of its
    /// generation.
    inputs: Vec<PathBuf>,
}

impl DependencyMap {
    pub fn new(mut lanes: Vec<Lane>, tier_roots: Vec<PathBuf>, inputs: Vec<PathBuf>) -> Self {
        for lane in &mut lanes {
            if let Lane::Table(rows) = lane {
                // Stable, so a name's first row is the one that stays.
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                rows.dedup_by(|later, first| later.0 == first.0);
            }
        }
        let mut search: Vec<PathBuf> = Vec::new();
        for lane in &lanes {
            if let Lane::SearchRoots { roots, .. } = lane {
                for r in roots {
                    let canon = CanonicalPath::resolve(r.as_path()).into_path_buf();
                    if !search.contains(&canon) {
                        search.push(canon);
                    }
                }
            }
        }
        // Each root as read and canonical: a walk from either spelling of
        // the project root must leave it out.
        let mut spelled: Vec<PathBuf> = Vec::new();
        for r in tier_roots {
            let canon = CanonicalPath::resolve(r.as_path()).into_path_buf();
            for p in [r, canon] {
                if !spelled.contains(&p) {
                    spelled.push(p);
                }
            }
        }
        let tier_roots = spelled;
        DependencyMap { lanes, search_roots: Arc::new(search), tier_roots, inputs }
    }

    /// The map with no lanes: the tier holds nothing.
    pub fn empty() -> Self {
        DependencyMap::new(Vec::new(), Vec::new(), Vec::new())
    }

    /// The files `ask` places, lanes in order, each file once per name (its first
    /// placement under a name), canonical so distinct roots naming one file
    /// give one candidate.
    pub fn locate(&self, ask: &Ask) -> Vec<Located> {
        let mut out: Vec<Located> = Vec::new();
        let mut push = |path: CanonicalPath, loads_as: Option<String>| {
            if !out.iter().any(|l| l.path == path && l.loads_as == loads_as) {
                out.push(Located { path, loads_as });
            }
        };
        for lane in &self.lanes {
            match ask {
                Ask::Name(name) => match lane {
                    Lane::Table(rows) => {
                        if let Ok(i) = rows.binary_search_by(|(n, _)| n.as_str().cmp(name.as_str())) {
                            let p = &rows[i].1;
                            if p.is_file() {
                                push(CanonicalPath::resolve(p.as_path()), Some(name.clone()));
                            }
                        }
                    }
                    Lane::Prefix { prefix, dirs, spelling, spells } => {
                        let Some(rest) = name.strip_prefix(prefix.as_str()) else { continue };
                        let spelled = match spells {
                            Spells::Remainder => rest,
                            Spells::WholeName => name.as_str(),
                        };
                        for dir in dirs {
                            for rel in (spelling.module_paths)(spelled) {
                                let p = dir.join(rel);
                                if p.is_file() {
                                    push(CanonicalPath::resolve(p), Some(name.clone()));
                                }
                            }
                        }
                    }
                    Lane::SearchRoots { roots, spelling } => {
                        for root in roots {
                            for rel in (spelling.module_paths)(name) {
                                let p = root.join(rel);
                                if p.is_file() {
                                    push(CanonicalPath::resolve(p), Some(name.clone()));
                                }
                            }
                        }
                    }
                    Lane::AlwaysLoad(_) => {}
                },
                Ask::File(path) => match lane {
                    Lane::Table(rows) => {
                        // A table runs to thousands of rows: only a row
                        // naming the same file name pays for a resolve.
                        let leaf = path.as_path().file_name();
                        for (n, p) in rows {
                            if p.file_name() == leaf && CanonicalPath::resolve(p.as_path()) == *path {
                                push(path.clone(), Some(n.clone()));
                            }
                        }
                    }
                    Lane::Prefix { prefix, dirs, spelling, spells } => {
                        for dir in dirs {
                            let Some(name) = spelled_under(dir, path, spelling) else { continue };
                            let name = match spells {
                                Spells::Remainder => format!("{prefix}{name}"),
                                Spells::WholeName if name.starts_with(prefix.as_str()) => name,
                                Spells::WholeName => continue,
                            };
                            push(path.clone(), Some(name));
                        }
                    }
                    Lane::SearchRoots { roots, spelling } => {
                        for root in roots {
                            if let Some(name) = spelled_under(root, path, spelling) {
                                push(path.clone(), Some(name));
                            }
                        }
                    }
                    Lane::AlwaysLoad(files) => {
                        if files.iter().any(|f| CanonicalPath::resolve(f.as_path()) == *path) {
                            push(path.clone(), None);
                        }
                    }
                },
            }
        }
        out
    }

    /// The search path, canonical, in precedence order.
    pub fn search_roots(&self) -> Arc<Vec<PathBuf>> {
        Arc::clone(&self.search_roots)
    }

    /// The directories whose files are the tier's: the workspace walk and
    /// the watcher leave them out.
    pub fn tier_roots(&self) -> &[PathBuf] {
        &self.tier_roots
    }

    /// The manifests the map was read from.
    pub fn inputs(&self) -> &[PathBuf] {
        &self.inputs
    }

    /// Whether `path` is one of the manifests the map was read from, by
    /// either spelling.
    pub fn is_input(&self, path: &Path) -> bool {
        let canon = CanonicalPath::resolve(path);
        self.inputs
            .iter()
            .any(|i| i == path || CanonicalPath::resolve(i.as_path()) == canon)
    }

    /// Whether `path` lies under a tier root.
    pub fn owns(&self, path: &Path) -> bool {
        self.tier_roots.iter().any(|r| path.starts_with(r))
    }

    /// The map's generation: its lanes, its tier roots and each input's
    /// stamp. A moved manifest is a new map even when no root moved.
    pub fn generation(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        for lane in &self.lanes {
            match lane {
                Lane::Table(rows) => (0u8, rows).hash(&mut h),
                Lane::Prefix { prefix, dirs, spells, .. } => (1u8, prefix, dirs, spells).hash(&mut h),
                Lane::SearchRoots { roots, .. } => (2u8, roots).hash(&mut h),
                Lane::AlwaysLoad(files) => (3u8, files).hash(&mut h),
            }
        }
        self.tier_roots.hash(&mut h);
        for input in &self.inputs {
            input.hash(&mut h);
            let stamp = std::fs::metadata(input).ok().map(|m| {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos());
                (m.len(), mtime)
            });
            stamp.hash(&mut h);
        }
        format!("{:016x}", h.finish())
    }
}

/// The module name `path` spells under `dir`, if it lies there.
fn spelled_under(dir: &Path, path: &CanonicalPath, spelling: &ModuleSpelling) -> Option<String> {
    let dir = CanonicalPath::resolve(dir);
    let rel = path.as_path().strip_prefix(dir.as_path()).ok()?;
    (spelling.module_of)(rel)
}

#[cfg(test)]
#[path = "dependency_map_tests.rs"]
mod tests;
