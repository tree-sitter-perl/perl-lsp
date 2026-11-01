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
    /// The map as the project's manifests and the process state it:
    /// what persists across processes (`fingerprint`).
    pub(crate) base: Arc<dyn DependencyLocator>,
    /// The roots registered files name (`NamedRoots`) when the map was
    /// read; `locator` searches them after `base`'s own.
    pub(crate) named: Vec<std::path::PathBuf>,
    pub(crate) locator: Arc<dyn DependencyLocator>,
    /// `file_stamp` of each `base.inputs()` when the map was read.
    pub(crate) stamps: Vec<Option<(i64, i64)>>,
    pub(crate) generation: u64,
    /// Canonical path → the names it landed under.
    pub(crate) landed: std::collections::HashMap<crate::util::canonical_path::CanonicalPath, Vec<String>>,
}

impl DependencyState {
    /// The map's identity across processes: where it searches, which roots
    /// are the tier's, and the manifests it was read from with their stamps
    /// — what moves a generation in a running process — plus the inputs
    /// every analysis was built under (a C toolchain), since a landed
    /// header's macros read them. The roots files name are left out: every start reads the map before the walk
    /// registers anyone, so counting them would drop the persisted import
    /// tier on every start of a project that names one.
    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.base.search_roots().hash(&mut h);
        self.base.roots().hash(&mut h);
        self.base.inputs().hash(&mut h);
        self.stamps.hash(&mut h);
        self.driver.analysis_input_fingerprint(self.root.as_deref()).hash(&mut h);
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

/// The search roots registered files name, resolved where each file
/// lives (`FileAnalysis::search_root_dirs`). A root is named while any
/// file names it; the map searches every named root.
#[derive(Default)]
pub(crate) struct NamedRoots {
    by_file: std::collections::HashMap<crate::util::canonical_path::CanonicalPath, Vec<std::path::PathBuf>>,
}

impl NamedRoots {
    /// Every root some file names, sorted.
    pub(crate) fn roots(&self) -> Vec<std::path::PathBuf> {
        let mut out: Vec<std::path::PathBuf> = self.by_file.values().flatten().cloned().collect();
        out.sort();
        out.dedup();
        out
    }

    /// Each named root with the files naming it, path-ordered.
    pub(crate) fn namers(&self) -> std::collections::BTreeMap<std::path::PathBuf, Vec<crate::util::canonical_path::CanonicalPath>> {
        let mut out: std::collections::BTreeMap<std::path::PathBuf, Vec<crate::util::canonical_path::CanonicalPath>> = Default::default();
        for (file, roots) in &self.by_file {
            for r in roots {
                out.entry(r.clone()).or_default().push(file.clone());
            }
        }
        for files in out.values_mut() {
            files.sort();
        }
        out
    }
}

impl IndexCore {
    pub(crate) fn dependency_state(&self) -> std::sync::MutexGuard<'_, Option<DependencyState>> {
        self.dependencies.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn named_roots(&self) -> std::sync::MutexGuard<'_, NamedRoots> {
        self.named_roots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the roots `file` names (none: it names none, or is gone).
    /// When the set of named roots moves, the resolver thread is woken to
    /// read the map again (`refresh_named_roots`).
    pub(crate) fn name_search_roots(&self, file: &crate::util::canonical_path::CanonicalPath, roots: Vec<std::path::PathBuf>) {
        // Every registration passes here; the bulk walk must not queue on
        // the lock for the files that name nothing.
        if roots.is_empty() && self.named_files.load(std::sync::atomic::Ordering::Acquire) == 0 {
            return;
        }
        let moved = {
            let mut named = self.named_roots();
            if named.by_file.get(file).map_or(roots.is_empty(), |r| *r == roots) {
                return;
            }
            let before = named.roots();
            if roots.is_empty() {
                named.by_file.remove(file);
            } else {
                named.by_file.insert(file.clone(), roots);
            }
            self.named_files.store(named.by_file.len(), std::sync::atomic::Ordering::Release);
            named.roots() != before
        };
        if moved {
            self.named_roots_moved.store(true, std::sync::atomic::Ordering::Release);
            self.queue.map_moved.store(true, std::sync::atomic::Ordering::Release);
            self.queue.notify_new_work();
        }
    }

    /// Read the map again when the roots files name moved since it was
    /// read. The resolver thread's, before it adopts a map.
    pub(crate) fn refresh_named_roots(&self) -> bool {
        if !self.named_roots_moved.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return false;
        }
        let current = self.dependency_state().as_ref().map(|st| (st.driver, st.root.clone()));
        match current {
            Some((driver, root)) => self.refresh_dependency_map(driver, root.as_deref()),
            None => false,
        }
    }

    /// Bring the map up to `root`'s manifests for `driver` and the roots
    /// registered files name. A moved input, a new project root or
    /// another language is a new GENERATION: what the old map landed
    /// retires, the names it answered — and every miss — are asked again
    /// under the new one, and the resolver thread is woken to adopt it.
    /// A move in the named roots alone is a new generation too, but
    /// retires only what it moves: the landings under a root nobody names
    /// any more, and the names a newly named root places. Returns whether
    /// the generation moved. Cheap when nothing did: one stat per input.
    pub(crate) fn refresh_dependency_map(
        &self,
        driver: &'static dyn LanguageDriver,
        root: Option<&std::path::Path>,
    ) -> bool {
        let root = root.map(|r| crate::util::canonical_path::CanonicalPath::resolve(r).into_path_buf());
        let named = self.named_roots().roots();
        let mut guard = self.dependency_state();
        let same_map = guard.as_ref().is_some_and(|st| {
            st.driver.id() == driver.id() && st.root == root && stamps(st.base.inputs()) == st.stamps
        });
        if same_map && guard.as_ref().is_some_and(|st| st.named == named) {
            return false;
        }
        let old = guard.take();
        let generation = old.as_ref().map_or(0, |s| s.generation) + 1;
        let base: Arc<dyn DependencyLocator> = match (&old, same_map) {
            (Some(old), true) => Arc::clone(&old.base),
            _ => driver.dependency_locator(root.as_deref()).unwrap_or_else(|| {
                Arc::new(crate::build::search_path::SearchPathLocator::new(driver, Vec::new(), Vec::new(), Vec::new()))
            }),
        };
        let mut reask: Vec<String> = Vec::new();
        let mut landed: std::collections::HashMap<crate::util::canonical_path::CanonicalPath, Vec<String>> = Default::default();
        if let Some(old) = old {
            let added: Vec<std::path::PathBuf> = named.iter().filter(|r| !old.named.contains(r)).cloned().collect();
            let dropped: Vec<&std::path::PathBuf> = old.named.iter().filter(|r| !named.contains(r)).collect();
            let places_new = (!added.is_empty()).then(|| base.searching(&added)).flatten();
            for (path, names) in old.landed {
                let keep = same_map
                    && !dropped.iter().any(|r| path.starts_with(r))
                    && !places_new.as_ref().is_some_and(|l| names.iter().any(|n| !l.locate(n).is_empty()));
                if keep {
                    landed.insert(path, names);
                } else {
                    self.retire_dependency(&path, &names);
                    reask.extend(names);
                }
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
        let locator = match named.is_empty() {
            true => Arc::clone(&base),
            false => {
                let mut search = base.search_roots().to_vec();
                search.extend(named.iter().filter(|r| !base.search_roots().contains(r)).cloned());
                base.searching(&search).unwrap_or_else(|| Arc::clone(&base))
            }
        };
        if let Some(marker) = driver.module_marker() {
            crate::build::walk_markers::set_search_roots(marker, locator.search_roots());
        }
        // Visibility ranks candidates against the search path, so it is
        // published before anything can land under it. The process's
        // roots only: the roots files name rank by who names them
        // (`RankedRoots`).
        self.set_inc_roots(base.search_roots(), root.as_deref(), base.roots());
        *guard = Some(DependencyState {
            driver,
            root,
            stamps: stamps(base.inputs()),
            base,
            named,
            locator,
            generation,
            landed,
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

    /// List the map's names and publish them whole, replacing the previous
    /// generation's. Walks directories with no lock held, so it is the
    /// resolver thread's. Returns the generation listed.
    pub(crate) fn list_available(&self) -> u64 {
        let Some(snap) = self.map_snapshot() else { return 0 };
        let mut names = std::collections::HashSet::new();
        snap.locator.for_each_name(snap.root.as_deref(), &mut |n| {
            names.insert(n.to_string());
        });
        log::info!("dependency map: {} names available", names.len());
        *self.available.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(names);
        snap.generation
    }

    /// Land `whole` — the files the map placed for `asked`, parsed under
    /// `generation`, in the map's precedence order — under `asked` for the
    /// files `answers` indexes, and under every name each file declares.
    /// Each file registers once (`register_landed`, under its first name)
    /// and one stored copy serves all its names; a name nobody asked for
    /// only fills a slot nothing answers yet, since an ask's own answer
    /// outranks a file that happens to declare the name too. Returns the
    /// asked name's stored providers; `None` is a miss.
    pub(crate) fn land_dependency(
        &self,
        asked: &str,
        answers: &[usize],
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
        let mut declared: Vec<(String, Vec<usize>)> = vec![(asked.to_string(), answers.to_vec())];
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
        let stored: Providers = whole
            .iter()
            .enumerate()
            .map(|(i, m)| super::index_core::strip_import_copy_one(m, persisted && answers.contains(&i), strip))
            .collect();
        // Each file registers ONCE, under its home: the first name in
        // precedence order it answers (the asked name, when it does). Its
        // other names only bind it below, so a header declaring hundreds
        // of names costs one feed, not one per name.
        let mut homes: Vec<(&str, Providers)> = Vec::new();
        for (i, m) in whole.iter().enumerate() {
            let Some((home, _)) = declared.iter().find(|(_, at)| at.contains(&i)) else { continue };
            match homes.iter_mut().find(|(h, _)| h == home) {
                Some((_, files)) => files.push(Arc::clone(m)),
                None => homes.push((home.as_str(), vec![Arc::clone(m)])),
            }
        }
        for (home, files) in &homes {
            self.register_landed(home, files);
        }
        let mut out = None;
        for (name, at) in declared {
            let claim = name == asked;
            if at.is_empty() {
                self.insert_resolved(&name, None, persisted, strip);
                continue;
            }
            let s: Providers = at.iter().map(|&i| Arc::clone(&stored[i])).collect();
            for &i in &at {
                st.landed.entry(whole[i].path.clone()).or_default().push(name.clone());
            }
            let landed = self.publish_resolved(&name, Some(s), claim);
            if claim {
                out = landed;
            }
        }
        crate::util::ghost_stats::count_by("dependency.landed", whole.len() as u64);
        Ok(out)
    }

    /// Swap each of `whole`'s resident copies — still the WHOLE copy a
    /// landing under `generation` stored — for its stripped twin, now that
    /// its rows committed. A copy landed again since, or a map that moved,
    /// keeps what it holds.
    pub(crate) fn strip_persisted(&self, generation: u64, whole: &[Arc<CachedModule>]) {
        let guard = self.dependency_state();
        let Some(st) = guard.as_ref().filter(|st| st.generation == generation) else { return };
        for m in whole {
            let Some(names) = st.landed.get(&m.path) else { continue };
            let stripped = super::index_core::strip_import_copy_one(m, true, true);
            if Arc::ptr_eq(&stripped, m) {
                continue;
            }
            // Stale-pin clear BEFORE the stripped copy is reachable, so its
            // first rehydration reads the just-committed blob.
            if let Some(bc) = self.bag_cache.read().ok().and_then(|g| g.clone()) {
                bc.invalidate(&m.path);
            }
            for name in names {
                if let Some(mut v) = self.all_defs.get_mut(name) {
                    for c in v.iter_mut().filter(|c| Arc::ptr_eq(c, m)) {
                        *c = Arc::clone(&stripped);
                    }
                }
                if let Some(mut slot) = self.cache.get_mut(name) {
                    if slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, m)) {
                        *slot = Some(Arc::clone(&stripped));
                    }
                }
            }
        }
        self.note_shape_change();
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
        }
        // Its home's other files — a front door's among them — are fed
        // again; only this file's edges go.
        self.retract_edges(path);
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

    /// The roots registered files name, and the files each root's namers
    /// reach through what they use, transitively (`RootRegistry`): the
    /// stack an asker runs under. Walked breadth-first per root over the
    /// candidate relation, so a namer reaches a landed dependency as well
    /// as a workspace file.
    pub fn root_registry(&self) -> Arc<crate::model::file_analysis::RootRegistry> {
        use crate::model::file_analysis::{CrossFileLookup, RootReach, RootRegistry};
        let named = self.core.named_roots().namers();
        if named.is_empty() {
            return Arc::new(RootRegistry::default());
        }
        let epoch = self.enrichment_epoch();
        let mut memo = self.root_registry_memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, reg)) = memo.as_ref() {
            if *at == epoch {
                return Arc::clone(reg);
            }
        }
        let mut reach: std::collections::HashMap<crate::util::canonical_path::CanonicalPath, Vec<RootReach>> = Default::default();
        for (root, namers) in &named {
            let mut seen: std::collections::HashSet<crate::util::canonical_path::CanonicalPath> = namers.iter().cloned().collect();
            let mut asked: std::collections::HashSet<String> = Default::default();
            let mut frontier: Vec<(Arc<FileAnalysis>, &crate::util::canonical_path::CanonicalPath)> = namers
                .iter()
                .filter_map(|n| self.all_files.get(n).map(|c| (Arc::clone(&c.analysis), n)))
                .collect();
            let mut depth = 0u32;
            while !frontier.is_empty() {
                depth += 1;
                let mut next = Vec::new();
                for (fa, namer) in frontier {
                    for id in fa.used_identities() {
                        if !asked.insert(id.clone()) {
                            continue;
                        }
                        for c in CrossFileLookup::def_candidates(self, &id) {
                            if seen.insert(c.path.clone()) {
                                reach.entry(c.path.clone()).or_default().push(RootReach {
                                    root: root.clone(),
                                    depth,
                                    namer: namer.clone(),
                                });
                                next.push((Arc::clone(&c.analysis), namer));
                            }
                        }
                    }
                }
                frontier = next;
            }
        }
        let reg = Arc::new(RootRegistry { named, reach });
        *memo = Some((epoch, Arc::clone(&reg)));
        reg
    }

    /// What the project's own files use across files (`used_identities`),
    /// read off the resident copies.
    pub fn dependency_demand(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        self.for_each_registered_file(&mut |cm| out.extend(cm.analysis.used_identities()));
        out.sort();
        out.dedup();
        out
    }
}
