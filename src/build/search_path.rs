//! A dependency map for a language that names modules by path under a
//! search root (python's `sys.path`, perl's `@INC`): a name is a file under
//! one of the roots, spelled by the pack's `module_paths`.

use crate::build::language_driver::{DependencyLocator, LanguageDriver};
use std::path::{Path, PathBuf};

pub struct SearchPathLocator {
    sep: String,
    module_paths: fn(&str) -> Vec<String>,
    /// Where a name is looked up, in search order.
    search: Vec<PathBuf>,
    /// The directories the tier owns (an environment, an install base),
    /// each holding some of `search`.
    roots: Vec<PathBuf>,
    inputs: Vec<PathBuf>,
}

impl SearchPathLocator {
    /// A map in `driver`'s language: its namespace separator splits a name,
    /// its pack's `module_paths` spells each prefix as a file.
    pub fn new(driver: &dyn LanguageDriver, search: Vec<PathBuf>, roots: Vec<PathBuf>, inputs: Vec<PathBuf>) -> Self {
        let pack = driver.lang_pack();
        let canon = |v: Vec<PathBuf>| v.into_iter().map(|p| std::fs::canonicalize(&p).unwrap_or(p)).collect();
        SearchPathLocator {
            sep: pack.as_ref().and_then(|p| p.names.use_map_sep().map(str::to_string)).unwrap_or_default(),
            module_paths: pack.map_or(|_| Vec::new(), |p| p.module_paths),
            search: canon(search),
            roots: canon(roots),
            inputs,
        }
    }
}

impl DependencyLocator for SearchPathLocator {
    /// The module a name spells, or the module declaring it: `a.b.C` is
    /// `a/b/C`, else `a/b` (which declares `C`), else `a`, longest first.
    fn locate(&self, identity: &str) -> Vec<PathBuf> {
        let segs: Vec<&str> = if self.sep.is_empty() { vec![identity] } else { identity.split(self.sep.as_str()).collect() };
        let mut out = Vec::new();
        for k in (1..=segs.len()).rev() {
            let module = segs[..k].join(self.sep.as_str());
            for dir in &self.search {
                for rel in (self.module_paths)(&module) {
                    let p = dir.join(rel);
                    if p.is_file() && !out.contains(&p) {
                        out.push(p);
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
