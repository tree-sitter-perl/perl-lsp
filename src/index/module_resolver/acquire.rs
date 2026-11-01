//! The one acquisition, for every served language: locate a name through
//! the dependency map, parse what it places, persist where the process has
//! a store, and land it. Every asker — a didOpen, the bulk indexer, an
//! invalidated save, a CLI query, each asking what a file uses
//! (`used_identities`), and descent from a landed file — reaches it
//! through `request_resolve`. The resolver thread drives it: a current
//! landing in the store answers first (`acquire_warm`), then a frontier
//! of names places (locates and parses) in parallel on Rayon
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
                    let canon = crate::util::canonical_path::CanonicalPath::resolve(path).into_path_buf();
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
    Some(Arc::new(CachedModule::new(crate::util::canonical_path::CanonicalPath::resolve(canon), Arc::new(fa))))
}

/// What an acquisition landed: nothing, or the asked name's providers and
/// what they ask for next (`descent`, read off the whole copies before any
/// strip).
pub(super) enum Landed {
    Miss,
    Hit { providers: usize, asks: Vec<String> },
}

/// Land `name` from the store when it holds a current landing for the
/// name (`module_cache::warm_module`); `None` when it does not, and the
/// name is placed and parsed instead. `strip_warm` is how a warm copy is
/// kept (`land_warm`).
pub(super) fn acquire_warm(
    core: &IndexCore,
    snap: &MapSnapshot,
    name: &str,
    db: Option<&rusqlite::Connection>,
    strip_warm: bool,
) -> Option<Result<Landed, MapMoved>> {
    let warm = db.and_then(|conn| module_cache::warm_module(conn, name))?;
    Some(land_warm(core, snap, name, warm, strip_warm))
}

/// Land `name`'s placed files under the generation they were placed at,
/// and queue their persist on `persists` — the strip waits for the blob's
/// commit (`DependencyPersists::flush`). The resolver thread's half of an
/// acquisition; `place_frontier` is the half that runs with no lock held.
/// Descent reads the WHOLE providers, before any strip; `Err` when the map
/// moved since the placing.
pub(super) fn land_placed(
    core: &IndexCore,
    snap: &MapSnapshot,
    name: &str,
    placed: Vec<Placed>,
    persists: &mut DependencyPersists,
) -> Result<Landed, MapMoved> {
    // Only the files that answer land: one that does not (an enclosing
    // package's file declaring other names) lands when its own name is
    // asked, so a warm start, which stores only answers, fills the same
    // slots a cold one does.
    let whole: Providers = answering(snap.driver, name, &placed).into_iter().map(|i| Arc::clone(&placed[i].file)).collect();
    let answers: Vec<usize> = (0..whole.len()).collect();
    let asks = descent(snap.driver, whole.iter().map(|m| &*m.analysis));
    core.land_dependency(name, &answers, snap.generation, whole.clone(), false, eviction_enabled())?;
    if whole.is_empty() {
        // A miss is never persisted: no map's inputs see every install (a
        // pip or cpanm into a search root), so a remembered miss would
        // outlive the install that answers it, and locating one again is a
        // stat per search root.
        return Ok(Landed::Miss);
    }
    // Per FILE: each provider gets its own row, so a shadowed twin
    // survives the warm start too.
    let n = whole.len();
    persists.push(name, snap.generation, Some(whole));
    Ok(Landed::Hit { providers: n, asks })
}

/// Land the store's whole copies of `name` (`module_cache::warm_module`):
/// descent reads them first, exactly as it reads a fresh parse, so a warm
/// start asks for what a cold one does. Kept as the one-shot run keeps a
/// warm copy — bag-evicted, rehydrated on demand — unless `strip`.
fn land_warm(
    core: &IndexCore,
    snap: &MapSnapshot,
    name: &str,
    warm: Vec<(crate::util::canonical_path::CanonicalPath, crate::model::file_analysis::FileAnalysis)>,
    strip: bool,
) -> Result<Landed, MapMoved> {
    let asks = descent(snap.driver, warm.iter().map(|(_, fa)| fa));
    let n = warm.len();
    let providers: Providers = warm
        .into_iter()
        .map(|(path, mut fa)| {
            if !strip {
                fa.evict_witness_bag();
            }
            Arc::new(CachedModule::new(path, Arc::new(fa)))
        })
        .collect();
    // Every persisted row answered the name it is filed under.
    let answers: Vec<usize> = (0..n).collect();
    core.land_dependency(name, &answers, snap.generation, providers, true, strip)?;
    Ok(Landed::Hit { providers: n, asks })
}

/// What a landed file asks for next, in every session alike: its parents
/// and re-exports (the MRO and a module's export surface read them at query
/// time) and, for the Perl hub, the classes its subs return — `has routes
/// => sub { Mojolicious::Routes->new }` is a class a chain walks through
/// that nothing the project names would land. Never everything the file
/// uses: on FHEM (2026-11-01) that closure landed 1,173 names where the
/// project's own demand placed 303, and cost a cold `--check` 30 s.
/// A pack's return-class descent waits on a measurement of its own.
pub(super) fn descent<'a>(
    driver: &dyn LanguageDriver,
    providers: impl IntoIterator<Item = &'a crate::model::file_analysis::FileAnalysis>,
) -> Vec<String> {
    use crate::model::file_analysis::{InferredType, SymKind, SymbolDetail};
    let mut asks: Vec<String> = Vec::new();
    // Every provider's, not just the winner's: a shadowed twin has its own
    // `use` list and `@ISA`, and a class only it names must still resolve.
    for fa in providers {
        for (_pkg, parents) in fa.package_parent_edges() {
            asks.extend(parents.iter().cloned());
        }
        // A re-exporting module (Test::Most → Test::More) pulls its
        // producers' surfaces transitively, so those producers must land
        // even when no file `use`s them directly.
        asks.extend(fa.reexport_modules().cloned());
        if !driver.caps().hub_enrichment {
            continue;
        }
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
