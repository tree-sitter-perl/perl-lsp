//! Carton's (and cpm's) install base, `local/` under the project root:
//! the dependency tier owns it, so the workspace walk leaves it out.
//!
//! TODO(perl-dependency-acquisition): the map owns the directory and
//! locates nothing (the Perl driver declares no pack `module_paths`); the
//! @INC tier acquires these modules, since `add_project_lib_paths` puts
//! `LIB` on @INC. One acquisition (the @INC tier's `parse_one_provider` and
//! the dependency tier's `land_path`) is the follow-up.

use crate::build::language_driver::{DependencyLocator, LanguageDriver};
use crate::build::search_path::SearchPathLocator;
use std::path::Path;
use std::sync::Arc;

/// The install base, under the project root.
pub const BASE: &str = "local";
/// Where its modules are, under the project root.
pub const LIB: &str = "local/lib/perl5";

pub fn locator(root: &Path, driver: &'static dyn LanguageDriver) -> Arc<dyn DependencyLocator> {
    let lib = root.join(LIB);
    let (search, roots) = if lib.is_dir() { (vec![lib.clone()], vec![root.join(BASE)]) } else { (Vec::new(), Vec::new()) };
    // An install creates or touches the lib directory; carton also writes
    // its snapshot.
    Arc::new(SearchPathLocator::new(driver, search, roots, vec![lib, root.join("cpanfile.snapshot")]))
}
