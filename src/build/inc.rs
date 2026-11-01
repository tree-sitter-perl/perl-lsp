//! Perl's dependency map: `@INC` as the search roots of a
//! `SearchPathLocator`. Project `lib/` and carton's (or cpm's)
//! `local/lib/perl5` go ahead of the process `@INC`; carton's `local/` is
//! the tier's root, so the workspace walk leaves it out. Project `lib/` is
//! searched but never a tier root: the walk keeps owning it.

use crate::build::language_driver::LanguageDriver;
use crate::build::search_path::SearchPathLocator;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Carton's install base, under the project root.
pub const BASE: &str = "local";
/// Where its modules are, under the project root.
pub const LIB: &str = "local/lib/perl5";
/// The project's own modules, under the project root.
const PROJECT_LIB: &str = "lib";

/// The process `@INC`, as the `perl` on PATH reports it. Read once: it is
/// fixed for the life of the process (`PERL5LIB` included), and reading it
/// shells out.
pub fn discover_inc_paths() -> Vec<PathBuf> {
    static INC: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    INC.get_or_init(|| {
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
    })
    .clone()
}

/// Put the project's lib paths (`lib/`, then `local/lib/perl5/` ahead of
/// it) at the front of `inc_paths`.
pub fn add_project_lib_paths(inc_paths: &mut Vec<PathBuf>, workspace_root: &Path) {
    for local_lib in &[PROJECT_LIB, LIB] {
        let p = workspace_root.join(local_lib);
        if p.is_dir() {
            log::info!("Auto-discovered project lib: {:?}", p);
            inc_paths.insert(0, p);
        }
    }
}

/// The map for a project at `root` (none: the process `@INC` alone).
pub fn locator(root: Option<&Path>, driver: &'static dyn LanguageDriver) -> Arc<SearchPathLocator> {
    let mut search = discover_inc_paths();
    let mut roots = Vec::new();
    let mut inputs = Vec::new();
    if let Some(root) = root {
        add_project_lib_paths(&mut search, root);
        let lib = root.join(LIB);
        if lib.is_dir() {
            roots.push(root.join(BASE));
        }
        // Carton's snapshot, the one manifest an install rewrites that the
        // watcher hears about. The lib directory is no input: its stamp
        // moves on an install no watcher reports, and a stamp only a
        // one-shot run sees would clear a running server's landings.
        inputs = vec![root.join("cpanfile.snapshot")];
    }
    Arc::new(SearchPathLocator::new(driver, search, roots, inputs))
}
