//! The dependency tier's landing: the files the dependency map locates for
//! a name, parsed in process or read from their rows, registered through
//! the served language's front door.

use super::*;
use crate::build::dependency_map::{Ask, DependencyMap};
use crate::index::module_index::FrontDoor;
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
    let persisted = match (rows, &whole) {
        (Some(conn), Some(providers)) => providers.iter().all(|m| persist_dependency(conn, m)),
        _ => false,
    };
    let stored = core.land_providers(module_name, whole.clone(), persisted, strip, map_gen, parse.door());
    core.place_members(map, module_name, &located, map_gen);
    (whole, stored)
}

/// THE landing of one file the map loads by path (`Ask::File`: Composer's
/// `files`, which no name places): parsed or read from its row, persisted,
/// and landed under every name the map places it under and every name it
/// declares — a function it defines is called by name with nothing asking
/// for its file. A file that offers no name lands nothing.
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
    let mut names: Vec<String> = map.locate(&Ask::File(path.clone())).into_iter().filter_map(|l| l.loads_as).collect();
    for m in &whole {
        for (name, _) in crate::index::module_index::collect_linkage_feed(&m.analysis) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let Some((first, rest)) = names.split_first() else {
        return (None, None);
    };
    let persisted = match rows {
        Some(conn) => whole.iter().all(|m| persist_dependency(conn, m)),
        None => false,
    };
    let stored = core.land_providers(first, Some(whole.clone()), persisted, strip, map_gen, parse.door());
    for name in rest {
        core.place_name(path, name, 0, map_gen);
    }
    (Some(whole), stored)
}

/// The files the map places under `module_name` that load as it, in the
/// map's order. A file placed under a name it does not load as (an
/// enclosing module) is not that name's provider.
fn located_as(map: &DependencyMap, module_name: &str) -> Vec<CanonicalPath> {
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
            .filter_map(|path| match prepared.remove(&path) {
                Some(done) => done,
                None => parse_one_provider(module_name, path, parse, rows),
            })
            .collect()
    });
    (!providers.is_empty()).then_some(providers)
}

/// A batch's reads and parses, run ahead of its landings: each located
/// file's analysis (or `None` when it reads as nothing), by path. A landing
/// takes its file from here and parses only what is missing, so what lands,
/// in what order, and what the memo records are the serial landing's.
pub(crate) type Prepared = std::collections::HashMap<CanonicalPath, Option<Arc<CachedModule>>>;

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

/// Read and parse, in the global Rayon pool the workspace indexer also runs
/// in, every file the batch's unsettled asks would land that the tier does
/// not hold yet. Each worker parses with its own parser and reads rows
/// through `reader`'s checkout pool, so no worker waits on the resolver's
/// own connection. Only the parse moves: the landings stay on the resolver
/// thread, in ask order. A held file is left to its landing (a restamp, or
/// a re-parse if it changed), and one file is parsed where it lands.
///
/// The shared pool, not one of its own: declared asks arrive while the
/// walk holds every worker, and a second pool of that size oversubscribed
/// the cores (laravel-app cold: 1.9 s shared, 2.5 s apart).
pub(crate) fn prepare_batch(
    core: &IndexCore,
    map: &DependencyMap,
    map_gen: u64,
    batch: &[Ask],
    driver: &'static dyn crate::build::language_driver::LanguageDriver,
    db_path: Option<&std::path::Path>,
    reader: &module_cache::RetainedReader,
) -> Prepared {
    use rayon::prelude::*;
    let mut jobs: Vec<(String, CanonicalPath)> = Vec::new();
    let mut seen: std::collections::HashSet<CanonicalPath> = std::collections::HashSet::new();
    let mut job = |label: &str, path: CanonicalPath| {
        if core.records.role_of(&path).is_none() && seen.insert(path.clone()) {
            jobs.push((label.to_string(), path));
        }
    };
    for ask in batch {
        if core.asked.get(ask).is_some_and(|e| e.0 == map_gen) {
            continue;
        }
        match ask {
            Ask::File(path) => job(&path.to_string_lossy(), path.clone()),
            Ask::Name(name) => {
                for module_name in landing_names(core, map, name) {
                    if core.project_answers(&module_name) {
                        continue;
                    }
                    for path in located_as(map, &module_name) {
                        job(&module_name, path);
                    }
                }
            }
        }
    }
    if jobs.len() < 2 {
        return Prepared::new();
    }
    crate::util::ghost_stats::count_by("resolver.prepared", jobs.len() as u64);
    let open = || db_path.and_then(|p| module_cache::open_reader_retrying(p).ok());
    crate::util::ghost_stats::timed("resolver.prepare", || {
        jobs.into_par_iter()
            .map_init(
                || Parse::of(driver),
                |parse, (label, path)| {
                    let done = match db_path {
                        Some(_) => reader
                            .with(open, |conn| parse_one_provider(&label, path.clone(), parse, Some(conn)))
                            .unwrap_or_else(|| parse_one_provider(&label, path.clone(), parse, None)),
                        None => parse_one_provider(&label, path.clone(), parse, None),
                    };
                    (path, done)
                },
            )
            .collect()
    })
}

/// Read + parse ONE provider file into a `CachedModule`, or take it from its
/// current row. Every provider runs this same body — the @INC winner has no
/// privileged path (rule #10).
fn parse_one_provider(
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

/// Scan the map's search roots for .pm files, populating the
/// available_modules map. Fast — no file reads, just directory traversal +
/// path→module name conversion.
pub(super) fn scan_inc_module_names(map: &DependencyMap, available: &DashMap<String, PathBuf>) {
    for inc in map.search_roots().iter() {
        if inc.is_dir() {
            scan_dir_recursive(inc, inc, available, 0);
        }
    }
}

fn scan_dir_recursive(base: &std::path::Path, dir: &std::path::Path, available: &DashMap<String, PathBuf>, depth: u32) {
    if depth > 15 { return; } // prevent symlink loops
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            scan_dir_recursive(base, &path, available, depth + 1);
        } else if path.extension().map(|e| e == "pm").unwrap_or(false) {
            if let Ok(rel) = path.strip_prefix(base) {
                let module_name = crate::model::conventions::perl_module_from_rel_path(rel);
                // Roots are walked in search order, so the FIRST one to claim
                // a name is the one `require` would load — the map's own first
                // answer for the name. Overwriting would name the LAST root.
                available.entry(module_name).or_insert_with(|| path.clone());
            }
        }
    }
}
