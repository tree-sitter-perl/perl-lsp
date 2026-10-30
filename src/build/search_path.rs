//! A dependency map for a language that names modules by path under a
//! search root (python's `sys.path`, perl's `@INC`): a name is a file under
//! one of the roots, spelled by the driver's `ModuleSpelling`.

use crate::build::language_driver::{Declared, DependencyLocator, LanguageDriver, Located, LocatedBy};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct SearchPathLocator {
    sep: String,
    module_paths: fn(&str) -> Vec<String>,
    module_of: fn(&str) -> Option<String>,
    extensions: Vec<&'static str>,
    /// Where a name is looked up, in search order, as given: a located file
    /// is canonical, so the roots need not be.
    search: Vec<PathBuf>,
    /// The directories the tier owns (an environment, an install base),
    /// each holding some of `search`.
    roots: Vec<PathBuf>,
    inputs: Vec<PathBuf>,
    declared: Vec<Declared>,
}

impl SearchPathLocator {
    /// A map in `driver`'s language. A use-map language's separator splits
    /// a name into the module and what it imports from it; in a language
    /// whose names are their modules (perl), a name is one module.
    pub fn new(driver: &dyn LanguageDriver, search: Vec<PathBuf>, roots: Vec<PathBuf>, inputs: Vec<PathBuf>) -> Self {
        let spelling = driver.module_spelling();
        SearchPathLocator {
            sep: spelling.as_ref().and_then(|s| s.names.use_map_sep().map(str::to_string)).unwrap_or_default(),
            module_paths: spelling.as_ref().map_or(|_| Vec::new(), |s| s.module_paths),
            module_of: spelling.map_or(|_| None, |s| s.module_of),
            extensions: driver.extensions().to_vec(),
            search,
            roots: roots.into_iter().map(|p| std::fs::canonicalize(&p).unwrap_or(p)).collect(),
            inputs,
            declared: Vec::new(),
        }
    }

    /// The same map, with what the project declares besides.
    pub fn declaring(mut self, declared: Vec<Declared>) -> Self {
        self.declared = declared;
        self
    }
}

impl DependencyLocator for SearchPathLocator {
    /// The module a name spells, or the module declaring it: `a.b.C` is
    /// `a/b/C` (exact), else `a/b` (which declares `C`), else `a` — both
    /// enclosing — longest first.
    /// Canonical, so distinct roots naming one file (a symlinked root, a
    /// duplicated entry) give one candidate.
    fn locate(&self, identity: &str) -> Vec<Located> {
        let segs: Vec<&str> = if self.sep.is_empty() { vec![identity] } else { identity.split(self.sep.as_str()).collect() };
        let mut out: Vec<Located> = Vec::new();
        for k in (1..=segs.len()).rev() {
            let module = segs[..k].join(self.sep.as_str());
            let by = if k == segs.len() { LocatedBy::Exact } else { LocatedBy::Enclosing };
            for dir in &self.search {
                for rel in (self.module_paths)(&module) {
                    let p = dir.join(rel);
                    if !p.is_file() {
                        continue;
                    }
                    let canon = std::fs::canonicalize(&p).unwrap_or(p);
                    if !out.iter().any(|l| l.path == canon) {
                        out.push(Located { path: canon, by });
                    }
                }
            }
        }
        out
    }
    fn declared_demand(&self) -> &[Declared] {
        &self.declared
    }
    fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
    fn search_roots(&self) -> &[PathBuf] {
        &self.search
    }
    fn inputs(&self) -> &[PathBuf] {
        &self.inputs
    }
    /// Each module file under a listable search root, named by the
    /// language's `module_of`.
    fn for_each_name(&self, workspace: Option<&Path>, visit: &mut dyn FnMut(&str)) {
        for dir in self.search.iter().filter(|d| listable(d, workspace)) {
            for path in module_files(dir, &self.extensions) {
                if let Some(name) = rel_path(dir, &path).and_then(|rel| (self.module_of)(&rel)) {
                    visit(&name);
                }
            }
        }
    }
    fn searching(&self, search: &[PathBuf]) -> Option<std::sync::Arc<dyn DependencyLocator>> {
        Some(std::sync::Arc::new(SearchPathLocator { search: search.to_vec(), ..self.clone() }))
    }
}

/// Whether a name listing may walk `root`. Never a filesystem root, nor a
/// directory the workspace sits in: either would enumerate the project and
/// whatever surrounds it.
pub fn listable(root: &Path, workspace: Option<&Path>) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    root.parent().is_some() && workspace.is_none_or(|w| !w.starts_with(&root))
}

/// Every file of `extensions` under `dir`, through the one walk: no ignore
/// files (an install directory is routinely ignored), no size cap (nothing
/// is read), and through symlinked directories, which installs use.
pub fn module_files(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    crate::build::source_files::walk(
        dir,
        &crate::build::source_files::Walk { extensions, cap: u64::MAX, skip: &[], ignores: false, follow_links: true },
    )
    .files
}

/// `path` under `dir`, `/`-separated whatever the platform.
pub fn rel_path(dir: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(parts.join("/"))
}

/// The directories directly under `root` that hold `marker`.
pub fn marked_children(root: &Path, marker: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(marker).is_file())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
#[path = "search_path_tests.rs"]
mod tests;
