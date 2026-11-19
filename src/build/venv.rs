//! python's reader of the dependency map: the virtual environments
//! directly under the project root (PEP 405: a directory holding
//! `pyvenv.cfg`). Their `site-packages` are one search-roots lane, in
//! environment order, and the environments are the tier's roots, so the
//! walk leaves them out. Nothing here runs Python.
//!
//! TODO(python-stdlib-roots): the interpreter's standard library, and —
//! when an environment's `pyvenv.cfg` says `include-system-site-packages
//! = true` — the base interpreter's `site-packages`, belong on the search
//! path AFTER the environment's own `site-packages`, as `site` orders
//! them. Neither is searched yet, so a stdlib import or a system-wide
//! package is a miss.

use crate::build::dependency_map::{DependencyMap, Lane, ModuleSpelling};
use crate::build::packs::PYTHON_PACKAGE_MARKER;
use crate::model::file_analysis::NameSpellings;
use std::path::{Path, PathBuf};

/// The file that makes a directory an environment.
const MARKER: &str = "pyvenv.cfg";
/// Where an environment is usually created. Their markers are inputs even
/// when absent, so one appearing there moves the map; an environment
/// created under another name is seen from the next read.
const CONVENTIONAL: &[&str] = &[".venv", "venv"];
/// A source module's extension.
const SOURCE_EXT: &str = "py";

fn names() -> &'static NameSpellings {
    static NAMES: std::sync::OnceLock<NameSpellings> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| crate::build::packs::python_pack().names)
}

fn sep() -> &'static str {
    names().sep().unwrap_or_default()
}

/// How python spells a module under a search root. A package directory
/// answers before a module file of the same name, as the path finder
/// tries them.
pub const SPELLING: ModuleSpelling = ModuleSpelling {
    id: "python-module",
    module_paths: |m| {
        let dir = m.replace(sep(), "/");
        vec![format!("{dir}/{PYTHON_PACKAGE_MARKER}"), format!("{dir}.{SOURCE_EXT}")]
    },
    module_of: |rel| {
        let mut segments: Vec<&str> = rel.parent()?.iter().map(|c| c.to_str()).collect::<Option<_>>()?;
        if rel.file_name()? != PYTHON_PACKAGE_MARKER {
            if rel.extension()? != SOURCE_EXT {
                return None;
            }
            segments.push(rel.file_stem()?.to_str()?);
        }
        // A segment that is no identifier (`six-1.0.dist-info`, `tests-data`)
        // is a directory no import names.
        (!segments.is_empty() && segments.iter().all(|s| is_identifier(s))).then(|| segments.join(sep()))
    },
    member_of: |name| name.rsplit_once(sep()).map(|(module, _)| module),
};

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// The map for a project at `root` (none: no project, no environment).
pub fn read_map(root: Option<&Path>) -> Option<DependencyMap> {
    let root = root?;
    let venvs = environments(root);
    let search: Vec<PathBuf> = venvs.iter().flat_map(|v| site_packages(v)).collect();
    let mut inputs: Vec<PathBuf> = CONVENTIONAL.iter().map(|d| root.join(d).join(MARKER)).collect();
    for v in &venvs {
        let marker = v.join(MARKER);
        if !inputs.contains(&marker) {
            inputs.push(marker);
        }
    }
    // Not `with_complete_inputs`: which environments exist and which
    // `lib/pythonX.Y` each holds come from listing directories no input
    // stamps, so the map is read every run (a few stats, not a table).
    Some(DependencyMap::new(vec![Lane::SearchRoots { roots: search, spelling: SPELLING }], venvs, inputs))
}

/// The spellings a kept python map's lanes may name.
pub const SPELLINGS: &[ModuleSpelling] = &[SPELLING];

/// The directories directly under `root` that hold the environment marker.
fn environments(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(MARKER).is_file())
        .collect();
    out.sort();
    out
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

#[cfg(test)]
#[path = "venv_tests.rs"]
mod tests;
