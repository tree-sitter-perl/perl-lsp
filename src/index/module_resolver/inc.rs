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
    strip: bool,
) -> (Option<Providers>, Option<Providers>) {
    let located = located_as(map, module_name);
    if let Some(held) = core.restamp_placed(&located, module_name, map_gen) {
        core.place_members(map, module_name, &located, map_gen);
        return (None, Some(held));
    }
    let parsed = parse_providers(module_name, located.clone(), parse, rows);
    let whole = core.tier_providers(parsed);
    let persisted = match (rows, &whole) {
        (Some(conn), Some(providers)) => providers.iter().all(|m| persist_dependency(conn, m)),
        _ => false,
    };
    let stored = core.land_providers(module_name, whole.clone(), persisted, strip, map_gen, parse.door());
    core.place_members(map, module_name, &located, map_gen);
    (whole, stored)
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
    let out = parse_providers(module_name, located_as(map, module_name), &mut parse, None);
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
    rows: Option<&rusqlite::Connection>,
) -> Option<Providers> {
    let providers: Providers =
        located.into_iter().filter_map(|path| parse_one_provider(module_name, path, parse, rows)).collect();
    (!providers.is_empty()).then_some(providers)
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
