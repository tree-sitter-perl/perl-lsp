//! A dependency map for a language that names modules by path under a
//! search root (python's `sys.path`, perl's `@INC`): a name is a file under
//! one of the roots, spelled by the driver's `ModuleSpelling`.

use crate::build::language_driver::{DependencyLocator, LanguageDriver};
use std::path::{Path, PathBuf};

pub struct SearchPathLocator {
    sep: String,
    module_paths: fn(&str) -> Vec<String>,
    /// Where a name is looked up, in search order, as given: a located file
    /// is canonical, so the roots need not be.
    search: Vec<PathBuf>,
    /// The directories the tier owns (an environment, an install base),
    /// each holding some of `search`.
    roots: Vec<PathBuf>,
    inputs: Vec<PathBuf>,
}

impl SearchPathLocator {
    /// A map in `driver`'s language. A use-map language's separator splits
    /// a name into the module and what it imports from it; in a language
    /// whose names are their modules (perl), a name is one module.
    pub fn new(driver: &dyn LanguageDriver, search: Vec<PathBuf>, roots: Vec<PathBuf>, inputs: Vec<PathBuf>) -> Self {
        let spelling = driver.module_spelling();
        SearchPathLocator {
            sep: spelling.as_ref().and_then(|s| s.names.use_map_sep().map(str::to_string)).unwrap_or_default(),
            module_paths: spelling.map_or(|_| Vec::new(), |s| s.module_paths),
            search,
            roots: roots.into_iter().map(|p| std::fs::canonicalize(&p).unwrap_or(p)).collect(),
            inputs,
        }
    }
}

impl DependencyLocator for SearchPathLocator {
    /// The module a name spells, or the module declaring it: `a.b.C` is
    /// `a/b/C`, else `a/b` (which declares `C`), else `a`, longest first.
    /// Canonical, so distinct roots naming one file (a symlinked root, a
    /// duplicated entry) give one candidate.
    fn locate(&self, identity: &str) -> Vec<PathBuf> {
        let segs: Vec<&str> = if self.sep.is_empty() { vec![identity] } else { identity.split(self.sep.as_str()).collect() };
        let mut out = Vec::new();
        for k in (1..=segs.len()).rev() {
            let module = segs[..k].join(self.sep.as_str());
            for dir in &self.search {
                for rel in (self.module_paths)(&module) {
                    let p = dir.join(rel);
                    if !p.is_file() {
                        continue;
                    }
                    let canon = std::fs::canonicalize(&p).unwrap_or(p);
                    if !out.contains(&canon) {
                        out.push(canon);
                    }
                }
            }
        }
        out
    }
    fn always_loaded(&self) -> &[PathBuf] {
        &[]
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
