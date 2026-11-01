//! The one acquisition, for every served language: locate a name through
//! the dependency map, parse what it places, persist where the process has
//! a store, and land it. Only the resolver thread runs it; every asker —
//! a didOpen's imports, the bulk indexer's demand, an invalidated save,
//! descent from a landed file — reaches it through `request_resolve`.

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
    let cap = snap.driver.caps().file_cap;
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out: Vec<Placed> = Vec::new();
    for Located { path, by } in snap.locator.locate(name) {
        let canon = crate::util::canonical_path::CanonicalPath::resolve(path).into_path_buf();
        if !seen.insert(canon.clone()) || !snap.in_tier(&canon) {
            continue;
        }
        if let Some(file) = parse_one(snap, &canon, cap, name) {
            out.push(Placed { file, by });
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

/// Resolve `name` under the current map: locate and parse with no lock
/// held, persist, then land. Returns the WHOLE providers (descent reads
/// what a strip drops); `Err` when the map moved meanwhile.
pub(super) fn acquire(
    core: &IndexCore,
    name: &str,
    db: Option<&rusqlite::Connection>,
) -> Result<Option<Providers>, MapMoved> {
    let Some(snap) = core.map_snapshot() else {
        return Ok(None);
    };
    let placed = locate_and_parse(&snap, name);
    let answers = answering(snap.driver, name, &placed);
    let whole: Providers = placed.into_iter().map(|p| p.file).collect();
    let rows: Providers = answers.iter().map(|&i| Arc::clone(&whole[i])).collect();
    // Per FILE: each provider gets its own row, so a shadowed twin
    // survives the warm start too.
    let persisted = db.is_some_and(|conn| save_module_generation(conn, name, &(!rows.is_empty()).then_some(rows)));
    core.land_dependency(Some((name, &answers)), snap.generation, whole.clone(), persisted, eviction_enabled())?;
    Ok((!whole.is_empty()).then_some(whole))
}

/// What a landed file asks for next. Parents and re-exports always: the
/// MRO and a module's export surface read them at query time. With
/// `prefetch`, also its imports and the classes its subs return — the
/// chain a long-lived session walks before anyone asks for it.
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
        asks.extend(fa.reexport_modules.iter().cloned());
        if !prefetch {
            continue;
        }
        asks.extend(fa.imports.iter().map(|i| i.module_name.clone()));
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
    MapSnapshot { driver: &crate::build::language_driver::PerlDriver, root: None, locator, generation: 0 }
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
