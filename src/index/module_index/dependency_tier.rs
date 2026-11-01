//! The dependency tier's index half: the map a `DependencyLocator` reads,
//! and the roots and manifests the walks and the watcher ask it about.

use super::*;
use crate::build::language_driver::{DependencyLocator, LanguageDriver};

/// One project's dependency map, for the map's generation. Replaced whole
/// when an input manifest moves.
pub(crate) struct DependencyState {
    pub(crate) driver: &'static dyn LanguageDriver,
    /// Canonical; `None` with no project open.
    pub(crate) root: Option<std::path::PathBuf>,
    pub(crate) locator: Arc<dyn DependencyLocator>,
    /// `file_stamp` of each `locator.inputs()` when the map was read.
    pub(crate) stamps: Vec<Option<(i64, i64)>>,
    pub(crate) generation: u64,
}

fn stamps(inputs: &[std::path::PathBuf]) -> Vec<Option<(i64, i64)>> {
    inputs.iter().map(|p| crate::index::module_cache::file_stamp(p)).collect()
}

impl IndexCore {
    pub(crate) fn dependency_state(&self) -> std::sync::MutexGuard<'_, Option<DependencyState>> {
        self.dependencies.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bring the map up to `root`'s manifests for `driver`. A moved input,
    /// a new root or another language is a new GENERATION. Returns whether
    /// the generation moved. Cheap when nothing did: one stat per input.
    pub(crate) fn refresh_dependency_map(
        &self,
        driver: &'static dyn LanguageDriver,
        root: Option<&std::path::Path>,
    ) -> bool {
        let root = root.map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()));
        let mut guard = self.dependency_state();
        if let Some(st) = guard.as_ref() {
            if st.driver.id() == driver.id() && st.root == root && stamps(st.locator.inputs()) == st.stamps {
                return false;
            }
        }
        let old = guard.take();
        let generation = old.as_ref().map_or(0, |s| s.generation) + 1;
        let locator: Arc<dyn DependencyLocator> = driver.dependency_locator(root.as_deref()).unwrap_or_else(|| {
            Arc::new(crate::build::search_path::SearchPathLocator::new(driver, Vec::new(), Vec::new(), Vec::new()))
        });
        // Visibility ranks candidates against the search path, so it is
        // published before anything can land under it.
        self.set_inc_roots(locator.search_roots());
        *guard = Some(DependencyState {
            driver,
            root,
            stamps: stamps(locator.inputs()),
            locator,
            generation,
        });
        true
    }
}

impl ModuleIndex {
    /// See `IndexCore::refresh_dependency_map`.
    pub fn refresh_dependency_map(
        &self,
        driver: &'static dyn LanguageDriver,
        root: Option<&std::path::Path>,
    ) -> bool {
        self.core.refresh_dependency_map(driver, root)
    }

    /// The map's generation check, for a caller that holds no root: the
    /// driver and root the current map was read with.
    pub fn refresh_dependency_map_again(&self) -> bool {
        let current = self.core.dependency_state().as_ref().map(|st| (st.driver, st.root.clone()));
        match current {
            Some((driver, root)) => self.core.refresh_dependency_map(driver, root.as_deref()),
            None => false,
        }
    }

    /// The current map's tier roots — directories the workspace walk skips
    /// and the watcher never registers from. Empty before the first map.
    pub fn dependency_roots(&self) -> Vec<std::path::PathBuf> {
        self.core
            .dependency_state()
            .as_ref()
            .map(|s| s.locator.roots().to_vec())
            .unwrap_or_default()
    }

    /// The tier roots under `root`, spelled under it as a walk of `root`
    /// spells its entries, so a walk's skip is a prefix test, not a
    /// canonicalize per entry. A root outside the walk needs no skip.
    pub fn dependency_roots_under(&self, root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.dependency_roots()
            .iter()
            .filter_map(|r| r.strip_prefix(&canon_root).ok().map(|rel| root.join(rel)))
            .collect()
    }

    /// Is `path` under one of the current map's tier roots? Such a file is
    /// never the workspace's.
    pub fn under_dependency_root(&self, path: &std::path::Path) -> bool {
        self.dependency_roots().iter().any(|r| path.starts_with(r))
    }

    /// Is `path` a manifest the current map was read from?
    pub fn is_dependency_input(&self, path: &std::path::Path) -> bool {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.core.dependency_state().as_ref().is_some_and(|s| {
            s.locator
                .inputs()
                .iter()
                .any(|i| i == path || std::fs::canonicalize(i).is_ok_and(|c| c == canon))
        })
    }
}
