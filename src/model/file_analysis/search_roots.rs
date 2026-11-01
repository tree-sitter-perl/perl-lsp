//! Search roots a file adds to its language's module search path (`use
//! lib`, `sys.path.insert`), and the tiers an asker ranks them in. A root
//! is minted where it is written, with the anchor it is relative to; the
//! anchor resolves in ONE place (`SearchRoot::resolve`), for registration
//! and visibility alike.

use super::*;
use std::path::{Path, PathBuf};

/// What a written root is relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RootAnchor {
    /// The process's working directory, which for a server is the
    /// workspace root: the one a relative `use lib` names.
    WorkingDir,
    /// The directory of the file that names the root (`$FindBin::Bin`,
    /// `os.path.dirname(__file__)`).
    FileDir,
    /// Written absolute.
    Absolute,
}

/// One directory a file adds to its search path, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchRoot {
    pub anchor: RootAnchor,
    pub path: String,
}

impl SearchRoot {
    /// `path` written under an idiom whose anchor is `anchor`: a path
    /// written absolute is absolute whatever the idiom's anchor.
    pub fn written(anchor: RootAnchor, path: &str) -> SearchRoot {
        if Path::new(path).is_absolute() {
            return SearchRoot { anchor: RootAnchor::Absolute, path: path.to_string() };
        }
        SearchRoot { anchor, path: path.to_string() }
    }

    /// The directory this root names for the file at `file`, with
    /// `working_dir` as the process's working directory: canonical, and
    /// only when it is an existing directory.
    pub fn resolve(&self, file: Option<&Path>, working_dir: Option<&Path>) -> Option<PathBuf> {
        let p = match self.anchor {
            RootAnchor::Absolute => PathBuf::from(&self.path),
            RootAnchor::WorkingDir => working_dir?.join(&self.path),
            RootAnchor::FileDir => file?.parent()?.join(&self.path),
        };
        let canon = std::fs::canonicalize(p).ok()?;
        canon.is_dir().then_some(canon)
    }
}

impl FileAnalysis {
    /// This file's search roots as directories, in the order written, for
    /// the file at `file` under `working_dir`. A root that names no
    /// existing directory drops out.
    pub fn search_root_dirs(&self, file: Option<&Path>, working_dir: Option<&Path>) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for r in &self.search_roots {
            if let Some(d) = r.resolve(file, working_dir) {
                if !out.contains(&d) {
                    out.push(d);
                }
            }
        }
        out
    }
}

/// The roots registered files name, and which files each root's namers
/// reach through what they use. Derived by the index
/// (`CrossFileLookup::root_registry`); empty when no registered file
/// names a root.
#[derive(Debug, Default)]
pub struct RootRegistry {
    /// Each named root, with the files naming it, path-ordered.
    pub named: std::collections::BTreeMap<PathBuf, Vec<crate::util::canonical_path::CanonicalPath>>,
    /// A file a root's namers use, transitively: per root, the fewest
    /// hops from a namer and the namer that took them.
    pub reach: std::collections::HashMap<crate::util::canonical_path::CanonicalPath, Vec<RootReach>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootReach {
    pub root: PathBuf,
    pub depth: u32,
    pub namer: crate::util::canonical_path::CanonicalPath,
}

/// What a root of the process's search path holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootHolds {
    /// The project's own code: under the workspace and outside every
    /// dependency tier root (Perl's `lib/`).
    Project,
    /// Installed code: a dependency tier root (carton's `local/`) or
    /// anywhere outside the workspace (the system `@INC`).
    Installed,
}

/// One root of the process's search path, classified where the map
/// publishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRoot {
    pub path: PathBuf,
    pub holds: RootHolds,
}

impl ProcessRoot {
    /// `path` (canonical) for a project at `workspace` whose dependency
    /// tier roots are `tier_roots`.
    pub fn classify(path: PathBuf, workspace: Option<&Path>, tier_roots: &[PathBuf]) -> ProcessRoot {
        let project = workspace.is_some_and(|ws| path.starts_with(ws))
            && !tier_roots.iter().any(|t| path.starts_with(t));
        let holds = if project { RootHolds::Project } else { RootHolds::Installed };
        ProcessRoot { path, holds }
    }
}

/// Where a root an asker searches came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootTier {
    /// The asker names it.
    Own,
    /// The process's search path, where it holds the project's own code.
    Project,
    /// A file that uses the asker, transitively, names it: the asker
    /// runs in that file's process.
    Stack { namer: PathBuf },
    /// Some other registered file names it. Over-approximate: nothing says
    /// the asker runs under that file.
    Registered { namer: PathBuf },
    /// The process's search path, where it holds installed code.
    Installed,
}

impl RootTier {
    /// Whether another file's root put this one on the asker's path.
    fn borrowed(&self) -> bool {
        matches!(self, RootTier::Stack { .. } | RootTier::Registered { .. })
    }
}

/// An asker's search roots, most-preferred first, each with its tier: the
/// asker's own, then the project's code, then the stack's (nearest
/// first), then every other registered root, then installed code. A root
/// another file names is borrowed: it fills what the project's own code
/// lacks and outranks an installed copy (a patched CPAN module under a
/// script's `use lib`), but never shadows the project's code (a `t/lib`
/// mock of a `lib/` module). THE ranking an asker's search path has;
/// `VisibilityAxis::for_origin` derives it for the search-path axis and
/// the supplier stamp reads the same list.
#[derive(Debug, Default)]
pub struct RankedRoots {
    pub roots: Vec<(PathBuf, RootTier)>,
}

impl RankedRoots {
    /// The ranking for `origin` at `self_path`, read from `index`.
    pub fn for_origin(origin: &FileAnalysis, self_path: Option<&crate::util::canonical_path::CanonicalPath>, index: &dyn CrossFileLookup) -> RankedRoots {
        let inc = index.inc_roots();
        let registry = index.root_registry();
        let mut ranked = RankedRoots::default();
        if !origin.search_roots.is_empty() {
            let ws = index.workspace_root_path();
            for d in origin.search_root_dirs(self_path.map(|p| p.as_path()), ws.as_deref()) {
                ranked.push(d, RootTier::Own);
            }
        }
        let process = |holds: RootHolds| inc.iter().filter(move |r| r.holds == holds);
        for r in process(RootHolds::Project) {
            ranked.push(r.path.clone(), RootTier::Project);
        }
        if !registry.named.is_empty() {
            // A root every asker searches anyway (`use lib 'lib'` naming the
            // project's own lib) is the process's, whoever else names it.
            let theirs = |root: &PathBuf| !inc.iter().any(|r| r.path == *root);
            let mut stack: Vec<&RootReach> =
                self_path.and_then(|p| registry.reach.get(p)).map(|v| v.iter().collect()).unwrap_or_default();
            stack.sort_by(|a, b| (a.depth, &a.root).cmp(&(b.depth, &b.root)));
            for r in stack.into_iter().filter(|r| theirs(&r.root)) {
                ranked.push(r.root.clone(), RootTier::Stack { namer: r.namer.to_path_buf() });
            }
            for (root, namers) in registry.named.iter().filter(|(r, _)| theirs(r)) {
                if let Some(namer) = namers.first() {
                    ranked.push(root.clone(), RootTier::Registered { namer: namer.to_path_buf() });
                }
            }
        }
        for r in process(RootHolds::Installed) {
            ranked.push(r.path.clone(), RootTier::Installed);
        }
        ranked
    }

    /// Add `root` unless it is already ranked: a root keeps its best tier.
    fn push(&mut self, root: PathBuf, tier: RootTier) {
        if !self.roots.iter().any(|(r, _)| *r == root) {
            self.roots.push((root, tier));
        }
    }

    /// Whether only the process's roots are ranked: the asker names none
    /// and no registered file does.
    pub fn is_process_only(&self) -> bool {
        self.roots.iter().all(|(_, t)| matches!(t, RootTier::Project | RootTier::Installed))
    }

    /// The rank of the root `path` is under: longest prefix wins, then
    /// root order — a vendored `local/lib/perl5` inside the project root
    /// is the more specific fact. `None` when no root holds it.
    pub fn rank(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .enumerate()
            .filter(|(_, (r, _))| path.starts_with(r))
            .min_by_key(|(i, (r, _))| (std::cmp::Reverse(r.as_os_str().len()), *i))
            .map(|(i, _)| i)
    }

    /// The root and tier `path` is found under, when that tier is another
    /// file's: the supplier an answer must name.
    pub fn supplier(&self, path: &Path) -> Option<RootSupplier> {
        let (root, tier) = &self.roots[self.rank(path)?];
        match tier {
            RootTier::Stack { namer } | RootTier::Registered { namer } => Some(RootSupplier {
                root: root.clone(),
                namer: namer.clone(),
                stack: matches!(tier, RootTier::Stack { .. }),
            }),
            RootTier::Own | RootTier::Project | RootTier::Installed => None,
        }
    }
}

/// An answer found under a root another file names: the root, the file
/// naming it, and whether that file uses the asker (the stack) or is only
/// registered beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootSupplier {
    pub root: PathBuf,
    pub namer: PathBuf,
    pub stack: bool,
}

/// A module this file imports whose answer came through a root another
/// file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppliedImport {
    pub span: Span,
    pub module: String,
    pub supplier: RootSupplier,
}

impl FileAnalysis {
    /// The imports whose provider, as this file at `self_path` sees it,
    /// lives under a root only another file names — the answers the
    /// `resolved-through-search-root` lane says out loud. `scoped` is this
    /// file's own view (its per-asker winner); `index` the index it ranks
    /// against.
    pub fn imports_supplied_by_other_roots(
        &self,
        self_path: Option<&crate::util::canonical_path::CanonicalPath>,
        index: &dyn CrossFileLookup,
        scoped: &dyn CrossFileLookup,
    ) -> Vec<SuppliedImport> {
        let ranked = RankedRoots::for_origin(self, self_path, index);
        if !ranked.roots.iter().any(|(_, t)| t.borrowed()) {
            return Vec::new();
        }
        self.imports
            .iter()
            .filter_map(|i| {
                let winner = scoped.get_cached(&i.module_name)?;
                let supplier = ranked.supplier(&winner.path)?;
                Some(SuppliedImport { span: i.span, module: i.module_name.clone(), supplier })
            })
            .collect()
    }
}
