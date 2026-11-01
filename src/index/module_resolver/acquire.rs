//! The one acquisition, for every served language: locate a name through
//! the dependency map, parse what it places, persist where the process has
//! a store, and land it. Every asker — a didOpen's imports, the bulk
//! indexer's demand, an invalidated save, descent from a landed file —
//! reaches it through `request_resolve`. The resolver thread drives it: a
//! frontier of names places (locates and parses) in parallel on Rayon
//! (`place_frontier`), and the thread lands each name in order
//! (`land_placed`), so the shared state only ever moves on that thread.

use super::*;
use crate::build::language_driver::{LanguageDriver, Located, LocatedBy};
use crate::index::module_index::{MapMoved, MapSnapshot};

/// A file the map placed for an asked name, parsed, and how the map
/// found it.
pub(crate) struct Placed {
    pub file: Arc<CachedModule>,
    pub by: LocatedBy,
}

/// The files `snap`'s map places for `name` that are the tier's, parsed in
/// the map's precedence order (every provider, not just the winner: a
/// name maps to a SET of files — XS/PP twins, an installed copy shadowed
/// by `local/` — and a shadowed twin carries its own subs and `@ISA`). A
/// file that is unreadable, at or over the driver's cap, or whose
/// analysis panics is left out.
pub(crate) fn locate_and_parse(snap: &MapSnapshot, name: &str) -> Vec<Placed> {
    place_frontier(snap, &[name.to_string()]).pop().unwrap_or_default()
}

/// `locate_and_parse` for every name of a frontier at once, on Rayon:
/// each name locates in parallel, then every located file parses in
/// parallel, so a frontier of one-file names and a name with many twins
/// spread the same way. Out is indexed like `names`, each in the map's
/// precedence order — the order the landing registers in, so the result
/// is the serial one whatever the pool's width.
pub(crate) fn place_frontier(snap: &MapSnapshot, names: &[String]) -> Vec<Vec<Placed>> {
    use rayon::prelude::*;
    let cap = snap.driver.caps().file_cap;
    let located: Vec<Vec<(PathBuf, LocatedBy)>> = names
        .par_iter()
        .map(|name| {
            let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            snap.locator
                .locate(name)
                .into_iter()
                .filter_map(|Located { path, by }| {
                    let canon = std::fs::canonicalize(&path).unwrap_or(path);
                    (seen.insert(canon.clone()) && snap.in_tier(&canon)).then_some((canon, by))
                })
                .collect()
        })
        .collect();
    let jobs: Vec<(usize, &PathBuf, LocatedBy)> = located
        .iter()
        .enumerate()
        .flat_map(|(i, files)| files.iter().map(move |(p, by)| (i, p, *by)))
        .collect();
    // Per file, as the workspace walk does: a panic outside the driver's
    // own guard loses that file, never the frontier or the thread.
    let parsed: Vec<Option<Arc<CachedModule>>> = jobs
        .par_iter()
        .map(|&(i, canon, _)| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse_one(snap, canon, cap, &names[i])))
                .ok()
                .flatten()
        })
        .collect();
    let mut out: Vec<Vec<Placed>> = names.iter().map(|_| Vec::new()).collect();
    for ((i, _, by), file) in jobs.into_iter().zip(parsed) {
        if let Some(file) = file {
            out[i].push(Placed { file, by });
        }
    }
    out
}

/// Which placed files answer `name`, in precedence order. The file whose
/// path spells the name is its file — `require JSON::backportPP` loads
/// `JSON/backportPP.pm` though it declares `JSON::PP` — so it answers
/// whatever it declares; an enclosing module's file answers only when it
/// declares the name.
fn answering(driver: &dyn LanguageDriver, name: &str, placed: &[Placed]) -> Vec<usize> {
    (0..placed.len())
        .filter(|&i| {
            placed[i].by == LocatedBy::Exact
                || driver.registration_names(&placed[i].file.analysis).iter().any(|(n, _)| n == name)
        })
        .collect()
}

fn parse_one(snap: &MapSnapshot, canon: &std::path::Path, cap: u64, attributed: &str) -> Option<Arc<CachedModule>> {
    let t = crate::util::timings::is_enabled().then(std::time::Instant::now);
    let fa = crate::build::source_files::analyze_file(snap.driver, canon, cap)?;
    if let Some(t) = t {
        crate::util::timings::record_built(attributed, Duration::ZERO, t.elapsed());
    }
    Some(Arc::new(CachedModule::new(canon.to_path_buf(), Arc::new(fa))))
}

/// Land `name`'s placed files under the generation they were placed at,
/// and queue their persist on `persists` — the strip waits for the blob's
/// commit (`DependencyPersists::flush`). The resolver thread's half of an
/// acquisition; `place_frontier` is the half that runs with no lock held.
/// Returns the WHOLE providers (descent reads what a strip drops); `Err`
/// when the map moved since the placing.
pub(super) fn land_placed(
    core: &IndexCore,
    snap: &MapSnapshot,
    name: &str,
    placed: Vec<Placed>,
    persists: &mut DependencyPersists,
) -> Result<Option<Providers>, MapMoved> {
    let answers = answering(snap.driver, name, &placed);
    let whole: Providers = placed.into_iter().map(|p| p.file).collect();
    let rows: Providers = answers.iter().map(|&i| Arc::clone(&whole[i])).collect();
    core.land_dependency(Some((name, &answers)), snap.generation, whole.clone(), false, eviction_enabled())?;
    // Per FILE: each provider gets its own row, so a shadowed twin
    // survives the warm start too.
    persists.push(name, snap.generation, (!rows.is_empty()).then_some(rows));
    Ok((!whole.is_empty()).then_some(whole))
}

/// What a landed file asks for next. Parents and re-exports always: the
/// MRO and a module's export surface read them at query time. With
/// `prefetch` (a server's, never a one-shot run's), everything it uses
/// (`used_identities`) and the classes its subs return — the chain a
/// session walks before anyone asks for it.
pub(super) fn descent(providers: &[Arc<CachedModule>], prefetch: bool) -> Vec<String> {
    use crate::model::file_analysis::{InferredType, SymKind, SymbolDetail};
    let mut asks: Vec<String> = Vec::new();
    // Every provider's, not just the winner's: a shadowed twin has its own
    // `use` list and `@ISA`, and a class only it names must still resolve.
    for m in providers {
        let fa = &m.analysis;
        for (_pkg, parents) in fa.package_parent_edges() {
            asks.extend(parents.iter().cloned());
        }
        // A re-exporting module (Test::Most → Test::More) pulls its
        // producers' surfaces transitively, so those producers must land
        // even when no file `use`s them directly.
        asks.extend(fa.reexport_modules().cloned());
        if !prefetch {
            continue;
        }
        asks.extend(fa.used_identities());
        // `has foo => sub { Bar->new }`, plugin-emitted typed subs, return
        // annotations: classes a chain walks through at query time.
        for sym in fa.symbols() {
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) || !matches!(sym.detail, SymbolDetail::Sub { .. }) {
                continue;
            }
            if let Some(InferredType::ClassName(c)) = fa.symbol_return_type_via_bag(sym.id, None) {
                asks.push(c);
            }
        }
        // A gated emission's accessors name classes too (a `belongs_to` its
        // target); asked as candidates, like every other feed of gated
        // content.
        for w in fa.gated_candidates().witnesses {
            if let crate::model::witnesses::WitnessPayload::InferredType(InferredType::ClassName(c)) = w.payload {
                asks.push(c);
            }
        }
    }
    asks.retain(|n| !n.is_empty());
    asks
}

/// Perl's map over `locator` with no project open, as a snapshot.
#[cfg(test)]
pub(crate) fn perl_snapshot(locator: Arc<dyn crate::build::language_driver::DependencyLocator>) -> MapSnapshot {
    MapSnapshot { driver: &crate::build::language_driver::PerlDriver, root: None, locator, generation: 0, fingerprint: 0 }
}

/// The process `@INC`'s providers for `name`, parsed: what the resolver
/// lands under `name` with no project open.
#[cfg(test)]
pub(crate) fn parse_from_inc(name: &str) -> Providers {
    let inc = crate::build::inc::locator(None, &crate::build::language_driver::PerlDriver);
    let placed = locate_and_parse(&perl_snapshot(inc), name);
    let driver = &crate::build::language_driver::PerlDriver;
    answering(driver, name, &placed).into_iter().map(|i| Arc::clone(&placed[i].file)).collect()
}
