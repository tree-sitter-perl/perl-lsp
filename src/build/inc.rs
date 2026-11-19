//! Perl's reader of the dependency map: `@INC` as one search-roots lane,
//! with carton's (or cpm's) `local/lib/perl5` and the project's `lib/`
//! ahead of the process `@INC`. Carton's `local/` is the tier's root, so
//! the workspace walk leaves it out; project `lib/` is searched but never a
//! tier root, because the walk owns it.

use crate::build::dependency_map::{DependencyMap, Lane, ModuleSpelling};
use std::path::{Path, PathBuf};

/// Carton's install base, under the project root.
const BASE: &str = "local";
/// Where its modules are, under the project root.
const BASE_LIB: &str = "local/lib/perl5";
/// The project's own modules, under the project root.
const PROJECT_LIB: &str = "lib";
/// Carton's snapshot: what an install rewrites.
const SNAPSHOT: &str = "cpanfile.snapshot";

/// How Perl spells a module under an `@INC` root.
pub const SPELLING: ModuleSpelling = ModuleSpelling {
    module_paths: |m| vec![crate::model::conventions::perl_module_rel_path(m)],
    module_of: |rel| {
        (rel.extension().is_some_and(|e| e == "pm")).then(|| crate::model::conventions::perl_module_from_rel_path(rel))
    },
};

/// The process `@INC`, as the `perl` on PATH reports it (`PERL5LIB`
/// included). Shells out, so it is read once per map.
pub fn discover_inc_paths() -> Vec<PathBuf> {
    let output = std::process::Command::new("perl")
        .args(["-e", r#"print join "\n", @INC"#])
        .stdin(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect(),
        _ => vec![],
    }
}

/// The map for a project at `root` (none: the process `@INC` alone).
pub fn read_map(root: Option<&Path>) -> DependencyMap {
    let mut search = discover_inc_paths();
    let mut tier_roots = Vec::new();
    let mut inputs = Vec::new();
    if let Some(root) = root {
        // `local/lib/perl5` ends up first, `lib/` second.
        for rel in [PROJECT_LIB, BASE_LIB] {
            let p = root.join(rel);
            if p.is_dir() {
                log::info!("Auto-discovered project lib: {:?}", p);
                search.insert(0, p);
            }
        }
        if root.join(BASE_LIB).is_dir() {
            tier_roots.push(root.join(BASE));
        }
        inputs.push(root.join(SNAPSHOT));
    }
    DependencyMap::new(vec![Lane::SearchRoots { roots: search, spelling: SPELLING }], tier_roots, inputs)
}

/// A map over exactly `roots`, for a test that names its own search path.
#[cfg(test)]
pub fn map_over(roots: Vec<PathBuf>) -> DependencyMap {
    DependencyMap::new(vec![Lane::SearchRoots { roots, spelling: SPELLING }], Vec::new(), Vec::new())
}

#[cfg(test)]
#[path = "inc_tests.rs"]
mod tests;
