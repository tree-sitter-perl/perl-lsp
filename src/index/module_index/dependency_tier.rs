//! The pack dependency tier's index half: what a `DependencyLocator`
//! landed, under which names, and its retirement when the map moves.
//!
//! A landed file enters the SAME candidate relation the @INC tier feeds
//! (`IndexCore::insert_resolved`): `all_defs` + the name slot, never
//! `all_files`. So it is DEPENDENCY by construction (`is_dependency_path`
//! is "not a front-door registration"), and every workspace-tier sweep —
//! diagnostics' and the heatmap's registered set, workspace-symbol's
//! workspace rows — never sees it. Acquisition (locate + parse) is the
//! resolver's (`module_resolver::acquire_dependencies`).

use super::*;
use crate::build::language_driver::{DependencyLocator, LanguageDriver};

/// One project's dependency map and what it has landed, for the map's
/// generation. Replaced whole when an input manifest moves.
pub(crate) struct DependencyState {
    pub(crate) driver: &'static dyn LanguageDriver,
    pub(crate) root: std::path::PathBuf,
    pub(crate) locator: Arc<dyn DependencyLocator>,
    /// `file_stamp` of each `locator.inputs()` when the map was read.
    pub(crate) stamps: Vec<Option<(i64, i64)>>,
    pub(crate) generation: u64,
    /// Identities asked this generation → the file that landed them
    /// (`None`: the map places nothing that declares it). A miss stays a
    /// miss until the map moves.
    pub(crate) asked: std::collections::HashMap<String, Option<std::path::PathBuf>>,
    /// Canonical path → the names it landed under.
    pub(crate) landed: std::collections::HashMap<std::path::PathBuf, Vec<String>>,
}

impl ModuleIndex {
    pub(crate) fn dependency_state(&self) -> std::sync::MutexGuard<'_, Option<DependencyState>> {
        self.core.dependencies.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current map's tier roots — directories the workspace walk skips
    /// and the watcher never registers from. Empty before the first map.
    pub fn dependency_roots(&self) -> Vec<std::path::PathBuf> {
        self.dependency_state()
            .as_ref()
            .map(|s| s.locator.roots().to_vec())
            .unwrap_or_default()
    }

    /// Is `path` a manifest the current map was read from?
    pub fn is_dependency_input(&self, path: &std::path::Path) -> bool {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.dependency_state().as_ref().is_some_and(|s| {
            s.locator
                .inputs()
                .iter()
                .any(|i| i == path || std::fs::canonicalize(i).is_ok_and(|c| c == canon))
        })
    }

    /// The map's generation: moves each time an input manifest does.
    #[cfg(test)]
    pub fn dependency_generation(&self) -> u64 {
        self.dependency_state().as_ref().map(|s| s.generation).unwrap_or(0)
    }

    /// Does a front-door (workspace) registration declare `name`? A
    /// dependency never takes a name the project owns.
    fn front_door_declares(&self, name: &str) -> bool {
        self.core
            .all_defs
            .get(name)
            .is_some_and(|v| v.iter().any(|c| self.all_files.contains_key(&c.path)))
    }

    /// Land one dependency file under every linkage-visible name it
    /// declares that the workspace does not. Whole copy: nothing persisted
    /// it, so nothing could rehydrate a strip — bounded by the demand
    /// closure (`acquire_dependencies`), never by the vendor tree.
    pub(crate) fn land_dependency(&self, cached: Arc<CachedModule>) -> Vec<String> {
        let mut names = Vec::new();
        for (name, _) in collect_linkage_feed(&cached.analysis) {
            if self.front_door_declares(&name) {
                continue;
            }
            self.core.insert_resolved(&name, Some(vec![Arc::clone(&cached)]), false, false);
            names.push(name);
        }
        names
    }

    /// Undo `land_dependency` for `path`: its candidates, any name slot it
    /// holds (re-picked from the survivors), and its edge feed.
    pub(crate) fn retire_dependency(&self, path: &std::path::Path, names: &[String]) {
        self.core.note_shape_change();
        for name in names {
            if let Some(mut v) = self.core.all_defs.get_mut(name) {
                v.retain(|c| c.path != path);
            }
            self.core.all_defs.remove_if(name, |_, v| v.is_empty());
            let held = self
                .core
                .cache
                .get(name)
                .is_some_and(|e| matches!(e.value(), Some(c) if c.path == path));
            if held {
                let survivor = self.core.all_defs.get(name).and_then(|v| {
                    best_candidate(&v.iter().collect::<Vec<_>>(), name, &|m, n| self.module_defines_class(m, n))
                });
                match survivor {
                    Some(c) => {
                        self.core.cache.insert(name.clone(), Some(c));
                    }
                    None => {
                        self.core.cache.remove(name);
                    }
                }
            }
            // A front door that declares the name publishes under it too;
            // its edges outlive the dependency's.
            if !self.front_door_declares(name) {
                self.core.edges.purge_module(name);
            }
        }
        self.core.edges.remove_path_record(path);
        if let Some(bc) = self.core.bag_cache.read().ok().and_then(|g| g.clone()) {
            bc.invalidate(path);
        }
    }

    /// The identities the project's own files name across files: each
    /// file's imports and declared parents. Read off the resident copies —
    /// both lanes survive every strip and ride the warm stub.
    pub fn dependency_demand(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        self.for_each_registered_file(&mut |cm| out.extend(dependency_demand_of(&cm.analysis)));
        out.sort();
        out.dedup();
        out
    }
}

/// One file's cross-file asks: what it imports and the parents it names.
pub fn dependency_demand_of(fa: &FileAnalysis) -> Vec<String> {
    let mut out: Vec<String> = fa.imports.iter().map(|i| i.module_name.clone()).collect();
    for (_child, parents) in fa.package_parent_edges() {
        out.extend(parents.iter().cloned());
    }
    out
}
