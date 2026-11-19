//! The dependency tier's landing: the files the dependency map locates for
//! a name, parsed in process or read from their rows, registered through
//! the served language's front door.

use super::*;
use crate::build::dependency_map::{Ask, DependencyMap};
use crate::index::module_index::{FrontDoor, PlacedAs};
use crate::util::canonical_path::CanonicalPath;

/// How the served language parses a landed file: the Perl hub through its
/// own parser and builder, as its workspace walk does, a pack through the
/// guarded parse every pack file goes through.
pub(crate) enum Parse {
    Hub(Parser),
    Pack(&'static dyn crate::build::language_driver::LanguageDriver),
}

impl Parse {
    pub(crate) fn of(driver: &'static dyn crate::build::language_driver::LanguageDriver) -> Self {
        match FrontDoor::of(driver) {
            FrontDoor::Hub => Parse::Hub(create_parser()),
            FrontDoor::Pack => Parse::Pack(driver),
        }
    }

    fn door(&self) -> FrontDoor {
        match self {
            Parse::Hub(_) => FrontDoor::Hub,
            Parse::Pack(_) => FrontDoor::Pack,
        }
    }

    /// Whether a stored analysis of `path` still holds what it read about
    /// the path besides its source: the module a path-named file is, which
    /// moves with a package marker or the search root it is named from.
    /// The hub's builder reads nothing but the source.
    fn path_inputs_current(&self, path: &std::path::Path, fa: &crate::model::file_analysis::FileAnalysis) -> bool {
        match self {
            Parse::Hub(_) => true,
            Parse::Pack(driver) => driver.path_inputs_current(path, fa),
        }
    }
}

// ---- Module parsing ----

/// THE landing of one module name, run only by the resolver thread: locate
/// every file the map places under the name; when each is already held
/// there, unchanged, carry its placement over to `map_gen`
/// (`IndexCore::restamp_placed`); otherwise take each from its current row
/// (`rows`) or parse it, keep what the tier may hold
/// (`IndexCore::tier_providers`), persist those path-keyed, and land them
/// (`IndexCore::land_providers`). `strip` is the eviction switch; a copy
/// strips only once its row is on disk. Returns the whole copies (what a
/// descent reads — the strip drops the bag; `None` for a restamp, whose
/// descent was asked when it first landed) and the stored ones.
#[allow(clippy::too_many_arguments)]
pub(crate) fn land_module(
    core: &IndexCore,
    rows: Option<&rusqlite::Connection>,
    map: &DependencyMap,
    map_gen: u64,
    module_name: &str,
    parse: &mut Parse,
    prepared: &mut Prepared,
    strip: bool,
) -> (Option<Providers>, Option<Providers>) {
    let located = located_as(map, module_name);
    if let Some(held) = core.restamp_placed(&located, module_name, map_gen) {
        core.place_members(map, module_name, &located, map_gen);
        return (None, Some(held));
    }
    let parsed = parse_providers(module_name, located.clone(), parse, prepared, rows);
    let whole = core.tier_providers(parsed);
    let (persisted, surfaces) = match &whole {
        Some(providers) => persist_providers(rows, providers),
        None => (false, Default::default()),
    };
    let stored = core.land_providers(module_name, whole.clone(), persisted, surfaces, strip, map_gen, PlacedAs::Identity, parse.door());
    core.place_members(map, module_name, &located, map_gen);
    (whole, stored)
}

/// THE landing of one file the map loads by path (`Ask::File`: Composer's
/// `files`, which no name places): parsed or read from its row, persisted,
/// and landed under every name the map places it under and every name it
/// declares — a function it defines is called by name with nothing asking
/// for its file. The file loads as the map's names and as what its
/// language's `FileIdentity` makes of its own (a class a PHP `files` entry
/// declares); the rest are its members. A file that offers no name lands
/// nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn land_file(
    core: &IndexCore,
    rows: Option<&rusqlite::Connection>,
    map: &DependencyMap,
    map_gen: u64,
    path: &CanonicalPath,
    parse: &mut Parse,
    prepared: &mut Prepared,
    strip: bool,
) -> (Option<Providers>, Option<Providers>) {
    let label = path.to_string_lossy().into_owned();
    let Some(whole) = core.tier_providers(parse_providers(&label, vec![path.clone()], parse, prepared, rows)) else {
        return (None, None);
    };
    let mut names: Vec<(String, PlacedAs)> = Vec::new();
    for name in map.locate(&Ask::File(path.clone())).into_iter().filter_map(|l| l.loads_as) {
        if !names.iter().any(|(n, _)| *n == name) {
            names.push((name, PlacedAs::Identity));
        }
    }
    for m in &whole {
        let declared = crate::index::module_index::collect_linkage_feed(&m.analysis);
        let identities = m.analysis.identity_names(&declared);
        for (name, _) in declared {
            if !names.iter().any(|(n, _)| *n == name) {
                let placed_as = if identities.contains(&name) { PlacedAs::Identity } else { PlacedAs::Member };
                names.push((name, placed_as));
            }
        }
    }
    let Some(((first, first_as), rest)) = names.split_first() else {
        return (None, None);
    };
    let (persisted, surfaces) = persist_providers(rows, &whole);
    let stored = core.land_providers(first, Some(whole.clone()), persisted, surfaces, strip, map_gen, *first_as, parse.door());
    for (name, placed_as) in rest {
        core.place_name(path, name, *placed_as, 0, map_gen);
    }
    (Some(whole), stored)
}

/// Persist each file of a landing, keeping the surface each row describes
/// for the landing to record. The strip licence is all-or-nothing, so the
/// first file with nothing to persist ends the writes; a write the store
/// refuses ends the process (`PersistFailure`).
fn persist_providers(rows: Option<&rusqlite::Connection>, providers: &Providers) -> (bool, crate::index::module_index::Surfaces) {
    let mut surfaces = crate::index::module_index::Surfaces::new();
    let Some(conn) = rows else {
        return (false, surfaces);
    };
    for m in providers {
        let (on_disk, surface) = persist_dependency(conn, m).unwrap_or_else(|failure| failure.exit());
        if let Some(s) = surface {
            surfaces.insert(m.path.clone(), s);
        }
        if !on_disk {
            return (false, surfaces);
        }
    }
    (true, surfaces)
}

/// The files the map places under `module_name` that load as it, in the
/// map's order. A file placed under a name it does not load as (an
/// enclosing module) is not that name's provider.
pub(super) fn located_as(map: &DependencyMap, module_name: &str) -> Vec<CanonicalPath> {
    map.locate(&Ask::Name(module_name.to_string()))
        .into_iter()
        .filter(|l| l.loads_as.as_deref() == Some(module_name))
        .map(|l| l.path)
        .collect()
}

/// Every file the map places under `module_name`, parsed (tests and the
/// blocking test lookup; the resolver thread lands through `land_module`).
#[cfg(test)]
pub fn resolve_and_parse_all(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
) -> Option<Providers> {
    // The caller's parser, lent to the hub's parse and handed back.
    let mut parse = Parse::Hub(std::mem::replace(parser, Parser::new()));
    let out = parse_providers(module_name, located_as(map, module_name), &mut parse, &mut Prepared::new(), None);
    if let Parse::Hub(p) = parse {
        *parser = p;
    }
    out
}

/// `resolve_and_parse_all`'s first provider.
#[cfg(test)]
pub fn resolve_and_parse(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
) -> Option<Arc<CachedModule>> {
    resolve_and_parse_all(map, module_name, parser).and_then(|p| p.into_iter().next())
}

/// Every provider is parsed, not just the @INC winner: a shadowed twin
/// carries its own subs and `@ISA`, and the candidate relation is only as
/// honest as its acquisition.
fn parse_providers(
    module_name: &str,
    located: Vec<CanonicalPath>,
    parse: &mut Parse,
    prepared: &mut Prepared,
    rows: Option<&rusqlite::Connection>,
) -> Option<Providers> {
    // Accumulated apart from `resolver.land`: what is left on the one
    // thread once the batch's parses ran in the pool.
    let providers: Providers = crate::util::ghost_stats::timed("resolver.parse", || {
        located
            .into_iter()
            .filter_map(|path| match prepared.take(&path) {
                Some(done) => done,
                None => parse_one_provider(module_name, path, parse, rows),
            })
            .collect()
    });
    (!providers.is_empty()).then_some(providers)
}

/// The module names an ask for `name` lands, as the map places it: itself,
/// or the modules it is a member of (`pkg.models.Base` loads `pkg.models`).
/// A name a project file answers is answered as written.
pub(crate) fn landing_names(core: &IndexCore, map: &DependencyMap, name: &str) -> Vec<String> {
    if core.project_answers(name) {
        return vec![name.to_string()];
    }
    let mut names: Vec<String> = Vec::new();
    for l in map.locate(&Ask::Name(name.to_string())) {
        if let Some(m) = l.loads_as.filter(|m| !names.contains(m)) {
            names.push(m);
        }
    }
    if names.is_empty() {
        names.push(name.to_string());
    }
    names
}

/// Read + parse ONE provider file into a `CachedModule`, or take it from its
/// current row. Every provider runs this same body — the @INC winner has no
/// privileged path (rule #10).
pub(super) fn parse_one_provider(
    module_name: &str,
    path: CanonicalPath,
    parse: &mut Parse,
    rows: Option<&rusqlite::Connection>,
) -> Option<Arc<CachedModule>> {
    if let Some(fa) = rows
        .and_then(|conn| module_cache::load_dependency_row(conn, &path))
        .filter(|fa| parse.path_inputs_current(path.as_path(), fa))
    {
        return Some(Arc::new(CachedModule::new(path, Arc::new(fa))));
    }
    let parser = match parse {
        Parse::Hub(parser) => parser,
        Parse::Pack(driver) => {
            let start = std::time::Instant::now();
            let analysis = crate::build::source_files::analyze_file(*driver, &path, driver.caps().file_cap)?;
            crate::util::timings::record_built(module_name, std::time::Duration::ZERO, start.elapsed());
            return Some(Arc::new(CachedModule::new(path, Arc::new(analysis))));
        }
    };
    let bench = std::env::var_os("PERL_LSP_BENCH").is_some();
    let bench_start = if bench { Some(std::time::Instant::now()) } else { None };

    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > 1_000_000 {
        if let Some(start) = bench_start {
            eprintln!("bench\t{}\t{}\toversize\t{}", module_name, start.elapsed().as_micros(), metadata.len());
        }
        return None;
    }
    let bytes = metadata.len();
    let source = crate::util::text::read_source(&path).ok()?;

    let timing = crate::util::timings::is_enabled();
    let t_parse = if timing { Some(std::time::Instant::now()) } else { None };
    let tree = parser.parse(&source, None)?;
    let parse_dur = t_parse.map(|s| s.elapsed()).unwrap_or_default();

    let t_build = if timing { Some(std::time::Instant::now()) } else { None };
    let analysis = crate::build::builder::build(&tree, source.as_bytes());
    let build_dur = t_build.map(|s| s.elapsed()).unwrap_or_default();
    crate::util::timings::record_built(module_name, parse_dur, build_dur);

    let symbols = analysis.symbols().len();
    let result = Arc::new(CachedModule::new(path, Arc::new(analysis)));
    if let Some(start) = bench_start {
        eprintln!("bench\t{}\t{}\t{}\t{}", module_name, start.elapsed().as_micros(), symbols, bytes);
    }
    Some(result)
}

