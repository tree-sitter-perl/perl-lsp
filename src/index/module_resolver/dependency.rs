//! The pack dependency tier's acquisition: read the driver's map, and land
//! the files it places for identities nothing in the workspace declares.
//!
//! Demand-driven, like the @INC tier: the project's own imports and
//! parents are the asks, each landed file's parents are asked in turn (the
//! MRO needs them), and nothing else of the tier is ever parsed. Blocking
//! — callers are the bulk indexer and the invalidator, never a request.

use super::*;
use crate::build::language_driver::LanguageDriver;
use crate::index::module_index::{DependencyState, ModuleIndex};

fn stamps(inputs: &[PathBuf]) -> Vec<Option<(i64, i64)>> {
    inputs.iter().map(|p| module_cache::file_stamp(p)).collect()
}

/// Bring `index`'s dependency map up to `root`'s manifests. A moved input
/// (or a new root) is a new GENERATION: everything the old map landed
/// retires, and nothing asked under it is remembered. Returns whether the
/// generation moved. Cheap when nothing did: one stat per input.
pub fn refresh_dependency_map(
    index: &ModuleIndex,
    driver: &'static dyn LanguageDriver,
    root: &std::path::Path,
) -> bool {
    let mut guard = index.dependency_state();
    if let Some(st) = guard.as_ref() {
        if st.root == root && stamps(st.locator.inputs()) == st.stamps {
            return false;
        }
    }
    let old = guard.take();
    let generation = old.as_ref().map_or(0, |s| s.generation) + 1;
    if let Some(old) = old {
        for (path, names) in &old.landed {
            index.retire_dependency(path, names);
        }
    }
    let Some(locator) = driver.dependency_locator(root) else {
        return generation > 1;
    };
    if let Some(marker) = driver.module_marker() {
        crate::build::walk_markers::set_search_roots(marker, locator.search_roots());
    }
    let always: Vec<PathBuf> = locator.always_loaded().to_vec();
    *guard = Some(DependencyState {
        driver,
        root: root.to_path_buf(),
        stamps: stamps(locator.inputs()),
        locator,
        generation,
        asked: Default::default(),
        landed: Default::default(),
    });
    let st = guard.as_mut().expect("just set");
    let mut parents: Vec<String> = Vec::new();
    for path in always {
        if let Some(fa) = land_path(index, st, &path) {
            parents.extend(parent_asks(&fa));
        }
    }
    acquire_locked(index, st, parents);
    true
}

/// The map's generation check, for a caller that holds no root: the one
/// the last `refresh_dependency_map` read.
pub fn refresh_dependency_map_again(index: &ModuleIndex) -> bool {
    let (driver, root) = match index.dependency_state().as_ref() {
        Some(st) => (st.driver, st.root.clone()),
        None => return false,
    };
    refresh_dependency_map(index, driver, &root)
}

/// Land every identity in `demand` that nothing declares and the map can
/// place, then their parents, transitively. Returns how many files landed.
/// A no-op before the first map.
pub fn acquire_dependencies(index: &ModuleIndex, demand: Vec<String>) -> usize {
    let mut guard = index.dependency_state();
    let Some(st) = guard.as_mut() else { return 0 };
    acquire_locked(index, st, demand)
}

fn acquire_locked(index: &ModuleIndex, st: &mut DependencyState, mut demand: Vec<String>) -> usize {
    use crate::model::file_analysis::CrossFileLookup;
    demand.sort();
    demand.dedup();
    let mut queue: std::collections::VecDeque<String> = demand.into();
    let mut n = 0;
    while let Some(id) = queue.pop_front() {
        if st.asked.contains_key(&id) || !index.def_candidates(&id).is_empty() {
            continue;
        }
        let mut hit = None;
        for path in st.locator.locate(&id) {
            let canon = std::fs::canonicalize(&path).unwrap_or(path);
            if st.landed.contains_key(&canon) || !in_tier(st, &canon) {
                continue;
            }
            if let Some(fa) = land_path(index, st, &canon) {
                n += 1;
                queue.extend(parent_asks(&fa));
                if st.landed.get(&canon).is_some_and(|names| names.contains(&id)) {
                    hit = Some(canon);
                    break;
                }
            }
        }
        st.asked.insert(id, hit);
    }
    n
}

/// A located file is the tier's when it is under one of the map's roots,
/// or outside the workspace altogether. Anything else under the root is
/// the workspace walk's to index, or to leave alone.
fn in_tier(st: &DependencyState, canon: &std::path::Path) -> bool {
    if st.locator.roots().iter().any(|r| canon.starts_with(r)) {
        return true;
    }
    let root = std::fs::canonicalize(&st.root).unwrap_or_else(|_| st.root.clone());
    !canon.starts_with(root)
}

/// Parse and land one file; `None` when it cannot be read or parsed.
fn land_path(
    index: &ModuleIndex,
    st: &mut DependencyState,
    canon: &std::path::Path,
) -> Option<Arc<crate::model::file_analysis::FileAnalysis>> {
    use crate::build::source_files::{analyze_file, PACK_FILE_CAP};
    let fa = Arc::new(analyze_file(st.driver, canon, PACK_FILE_CAP)?);
    crate::util::ghost_stats::count("dependency.landed");
    let names = index.land_dependency(Arc::new(CachedModule::new(canon.to_path_buf(), Arc::clone(&fa))));
    st.landed.insert(canon.to_path_buf(), names);
    Some(fa)
}

fn parent_asks(fa: &crate::model::file_analysis::FileAnalysis) -> Vec<String> {
    fa.package_parent_edges().flat_map(|(_, ps)| ps.iter().cloned()).collect()
}
