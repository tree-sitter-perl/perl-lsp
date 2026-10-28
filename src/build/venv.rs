//! Python virtual environments directly under the project root (PEP 405:
//! a directory holding `pyvenv.cfg`): the dependency tier's map. An import
//! nothing in the workspace declares is looked up in their
//! `site-packages`, and the walk leaves the environments out.

use crate::build::language_driver::{DependencyLocator, LanguageDriver};
use crate::build::search_path::{marked_children, SearchPathLocator};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MARKER: &str = "pyvenv.cfg";
/// Where an environment is usually created. Their markers are inputs even
/// when absent, so one appearing there moves the map; an environment
/// created under another name is seen from the next start.
const CONVENTIONAL: &[&str] = &[".venv", "venv"];

pub fn locator(root: &Path, driver: &'static dyn LanguageDriver) -> Arc<dyn DependencyLocator> {
    let venvs = marked_children(root, MARKER);
    let search = venvs.iter().flat_map(|v| site_packages(v)).collect();
    let mut inputs: Vec<PathBuf> = CONVENTIONAL.iter().map(|d| root.join(d).join(MARKER)).collect();
    for v in &venvs {
        let marker = v.join(MARKER);
        if !inputs.contains(&marker) {
            inputs.push(marker);
        }
    }
    Arc::new(SearchPathLocator::new(driver, search, venvs, inputs))
}

/// `lib/python3.X/site-packages` (posix), or `Lib/site-packages` (Windows).
fn site_packages(venv: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(venv.join("lib"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("site-packages"))
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    let windows = venv.join("Lib").join("site-packages");
    if windows.is_dir() {
        out.push(windows);
    }
    out
}
