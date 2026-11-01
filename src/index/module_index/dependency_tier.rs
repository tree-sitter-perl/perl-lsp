//! The dependency tier's index half: the map a `DependencyLocator` reads,
//! what the resolver thread landed under it, and their retirement when the
//! map moves.
//!
//! A landed file enters the SAME candidate relation the front doors feed
//! (`IndexCore::publish_resolved`): `all_defs` + the name slot, never
//! `all_files`. So it is DEPENDENCY by construction (`is_dependency_path`
//! is "not a front-door registration"), and every workspace-tier sweep —
//! diagnostics' and the heatmap's registered set, workspace-symbol's
//! workspace rows — never sees it. Acquisition (locate + parse + land) is
//! the resolver thread's (`module_resolver::acquire`).

use super::*;
use crate::build::language_driver::{DependencyLocator, LanguageDriver};

/// One project's dependency map and what it has landed, for the map's
/// generation. Replaced whole when an input manifest moves.
pub(crate) struct DependencyState {
    pub(crate) driver: &'static dyn LanguageDriver,
    /// Canonical; `None` with no project open.
    pub(crate) root: Option<std::path::PathBuf>,
    pub(crate) locator: Arc<dyn DependencyLocator>,
    /// `file_stamp` of each `locator.inputs()` when the map was read.
    pub(crate) stamps: Vec<Option<(i64, i64)>>,
    pub(crate) generation: u64,
    /// Canonical path → the names it landed under.
    pub(crate) landed: std::collections::HashMap<crate::util::canonical_path::CanonicalPath, Vec<String>>,
}

impl DependencyState {
    /// The map's identity across processes: where it searches, which roots
    /// are the tier's, and the manifests it was read from with their stamps
    /// — what moves a generation in a running process.
    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.locator.search_roots().hash(&mut h);
        self.locator.roots().hash(&mut h);
        self.locator.inputs().hash(&mut h);
        self.stamps.hash(&mut h);
        h.finish()
    }
}

/// The map as one acquisition reads it, so locating and parsing run with
/// no lock held. `generation` is what the landing re-checks.
pub(crate) struct MapSnapshot {
    pub(crate) driver: &'static dyn LanguageDriver,
    pub(crate) root: Option<std::path::PathBuf>,
    pub(crate) locator: Arc<dyn DependencyLocator>,
    pub(crate) generation: u64,
    /// What the persisted import tier was landed under
    /// (`module_cache::validate_dependency_fingerprint`).
    pub(crate) fingerprint: u64,
}

impl MapSnapshot {
    /// A located file is the tier's when it is under one of the map's
    /// roots, or outside the project altogether. Anything else under the
    /// root is the workspace walk's to index, or to leave alone.
    pub(crate) fn in_tier(&self, canon: &std::path::Path) -> bool {
        self.locator.roots().iter().any(|r| canon.starts_with(r))
            || self.root.as_ref().is_none_or(|root| !canon.starts_with(root))
    }
}

/// The map moved between an acquisition's snapshot and its landing; the
/// name is asked again under the new one.
pub(crate) struct MapMoved;

fn stamps(inputs: &[std::path::PathBuf]) -> Vec<Option<(i64, i64)>> {
    inputs.iter().map(|p| crate::index::module_cache::file_stamp(p)).collect()
}

impl IndexCore {
    pub(crate) fn dependency_state(&self) -> std::sync::MutexGuard<'_, Option<DependencyState>> {
        self.dependencies.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bring the map up to `root`'s manifests for `driver`. A moved input,
    /// a new root or another language is a new GENERATION: what the old
    /// map landed retires, the names it answered — and every miss — are
    /// asked again under the new one, and the resolver thread is woken to
    /// adopt it. Returns whether the generation moved. Cheap when nothing
    /// did: one stat per input.
    pub(crate) fn refresh_dependency_map(
        &self,
        driver: &'static dyn LanguageDriver,
        root: Option<&std::path::Path>,
    ) -> bool {
        let root = root.map(|r| crate::util::canonical_path::CanonicalPath::resolve(r).into_path_buf());
        let mut guard = self.dependency_state();
        if let Some(st) = guard.as_ref() {
            if st.driver.id() == driver.id() && st.root == root && stamps(st.locator.inputs()) == st.stamps {
                return false;
            }
        }
        let old = guard.take();
        let generation = old.as_ref().map_or(0, |s| s.generation) + 1;
        let mut reask: Vec<String> = Vec::new();
        if let Some(old) = old {
            for (path, names) in &old.landed {
                self.retire_dependency(path, names);
                reask.extend(names.iter().cloned());
            }
        }
        // A miss under the old map may be a hit under this one.
        self.cache.retain(|name, slot| {
            if slot.is_some() {
                return true;
            }
            reask.push(name.clone());
            false
        });
        let locator: Arc<dyn DependencyLocator> = driver.dependency_locator(root.as_deref()).unwrap_or_else(|| {
            Arc::new(crate::build::search_path::SearchPathLocator::new(driver, Vec::new(), Vec::new(), Vec::new()))
        });
        if let Some(marker) = driver.module_marker() {
            crate::build::walk_markers::set_search_roots(marker, locator.search_roots());
        }
        // Visibility ranks candidates against the search path, so it is
        // published before anything can land under it.
        self.set_inc_roots(locator.search_roots());
        *guard = Some(DependencyState {
            driver,
            root,
            stamps: stamps(locator.inputs()),
            locator,
            generation,
            landed: Default::default(),
        });
        drop(guard);
        self.queue.map_moved.store(true, std::sync::atomic::Ordering::Release);
        self.queue.ask(reask);
        // A resolver still waiting for initialize's root can start: the
        // map says where the project is.
        let _root = self.workspace_root.root.lock().unwrap_or_else(|e| e.into_inner());
        self.workspace_root.condvar.notify_all();
        true
    }

    /// `refresh_dependency_map` for a caller with no driver of its own:
    /// the driver and root the current map was read with, or `served` and
    /// `root` when there is none yet.
    pub(crate) fn ensure_dependency_map(&self, served: &'static dyn LanguageDriver, root: Option<&std::path::Path>) {
        let current = self.dependency_state().as_ref().map(|st| (st.driver, st.root.clone()));
        match current {
            Some((driver, root)) => self.refresh_dependency_map(driver, root.as_deref()),
            None => self.refresh_dependency_map(served, root),
        };
    }

    pub(crate) fn map_snapshot(&self) -> Option<MapSnapshot> {
        self.dependency_state().as_ref().map(|st| MapSnapshot {
            driver: st.driver,
            root: st.root.clone(),
            locator: Arc::clone(&st.locator),
            generation: st.generation,
            fingerprint: st.fingerprint(),
        })
    }

    pub(crate) fn dependency_generation(&self) -> u64 {
        self.dependency_state().as_ref().map_or(0, |s| s.generation)
    }

    /// Land `whole` — the files the map placed for an asked name (none:
    /// its always-loaded files), parsed under `generation`, in the map's
    /// precedence order — under the asked name for the files `asked`
    /// says answer it, and under every name each file declares. One
    /// stored copy per file serves all its names; a name nobody asked for
    /// only fills a slot nothing answers yet, since an ask's own answer
    /// outranks a file that happens to declare the name too. Returns the
    /// asked name's stored providers; `None` is a miss.
    pub(crate) fn land_dependency(
        &self,
        asked: Option<(&str, &[usize])>,
        generation: u64,
        whole: Providers,
        persisted: bool,
        strip: bool,
    ) -> Result<Option<Providers>, MapMoved> {
        debug_assert!(
            std::thread::current().name().is_some_and(|n| n.starts_with("module-resolver")),
            "only the resolver thread lands a dependency",
        );
        let mut guard = self.dependency_state();
        let Some(st) = guard.as_mut().filter(|st| st.generation == generation) else {
            return Err(MapMoved);
        };
        // Name → the files answering it, in precedence order: the asked
        // name first, so its answer lands before any by-product, then
        // every name a file declares.
        let mut declared: Vec<(String, Vec<usize>)> = asked.map(|(a, at)| (a.to_string(), at.to_vec())).into_iter().collect();
        for (i, m) in whole.iter().enumerate() {
            for (name, _) in st.driver.registration_names(&m.analysis) {
                match declared.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, at)) if !at.contains(&i) => at.push(i),
                    Some(_) => {}
                    None => declared.push((name, vec![i])),
                }
            }
        }
        // `persisted` covers the asked name's providers — the rows the
        // caller wrote; any other file stays whole, its bag unrecoverable.
        let answers: &[usize] = asked.map_or(&[], |(_, at)| at);
        let stored: Providers = whole
            .iter()
            .enumerate()
            .map(|(i, m)| super::index_core::strip_import_copy_one(m, persisted && answers.contains(&i), strip))
            .collect();
        let mut out = None;
        for (name, at) in declared {
            let claim = asked.is_some_and(|(a, _)| a == name);
            if at.is_empty() {
                self.insert_resolved(&name, None, persisted, strip);
                continue;
            }
            let w: Providers = at.iter().map(|&i| Arc::clone(&whole[i])).collect();
            let s: Providers = at.iter().map(|&i| Arc::clone(&stored[i])).collect();
            for &i in &at {
                st.landed.entry(whole[i].path.clone()).or_default().push(name.clone());
            }
            let landed = self.publish_resolved(&name, Some(&w), Some(s), claim);
            if claim {
                out = landed;
            }
        }
        crate::util::ghost_stats::count_by("dependency.landed", whole.len() as u64);
        Ok(out)
    }

    /// Undo a landing of `path`: its candidates, any name slot it holds
    /// (re-picked from the survivors), and its edge feed.
    pub(crate) fn retire_dependency(&self, path: &crate::util::canonical_path::CanonicalPath, names: &[String]) {
        self.note_shape_change();
        for name in names {
            if let Some(mut v) = self.all_defs.get_mut(name) {
                v.retain(|c| c.path != *path);
            }
            self.all_defs.remove_if(name, |_, v| v.is_empty());
            let held = self
                .cache
                .get(name)
                .is_some_and(|e| matches!(e.value(), Some(c) if c.path == *path));
            if held {
                let survivor = self.all_defs.get(name).and_then(|v| {
                    best_candidate(&v.iter().collect::<Vec<_>>(), name, &|m, n| self.module_defines_class(m, n))
                });
                match survivor {
                    Some(c) => {
                        self.cache.insert(name.clone(), Some(c));
                    }
                    None => {
                        self.cache.remove(name);
                    }
                }
            }
            // A front door that declares the name publishes under it too;
            // its edges outlive the dependency's.
            if !self.front_door_declares(name) {
                self.edges.purge_module(name);
            }
        }
        self.edges.remove_path_record(path);
        if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
            bc.invalidate(path);
        }
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
        let canon_root = crate::util::canonical_path::CanonicalPath::resolve(root);
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
        let canon = crate::util::canonical_path::CanonicalPath::resolve(path);
        self.core.dependency_state().as_ref().is_some_and(|s| {
            s.locator
                .inputs()
                .iter()
                .any(|i| i == path || crate::util::canonical_path::CanonicalPath::resolve(i) == canon)
        })
    }

    /// The map's generation: moves each time an input manifest does.
    #[cfg(test)]
    pub fn dependency_generation(&self) -> u64 {
        self.core.dependency_generation()
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
