//! The @INC tier's landing: the files the dependency map locates for a
//! name, parsed in process (or read from their rows) with a
//! parent-fallback memo.

use super::*;
use crate::build::dependency_map::{Ask, DependencyMap};
use crate::util::canonical_path::CanonicalPath;

// ---- Module parsing ----

/// Run-local memo for `resolve_and_parse_with_memo`. Persists across many
/// top-level calls within a single resolver sweep so that parent-fallback
/// recursion (e.g. 50 children all inheriting from `Exporter`) parses each
/// parent exactly once.
pub type ParseMemo = HashMap<String, Option<Providers>>;

/// THE landing of one `@INC` module name, shared by the resolver thread and
/// the one-shot CLI: locate every file the map places under the name, take
/// each from its current row (`rows`) or parse it, keep what the tier may
/// hold (`IndexCore::tier_providers`), persist those path-keyed, and land
/// them (`IndexCore::land_providers`). `strip` is the eviction switch; a copy
/// strips only once its row is on disk. Returns the whole copies (what a
/// descent reads — the strip drops the bag) and the stored ones.
pub(crate) fn land_module(
    core: &IndexCore,
    rows: Option<&rusqlite::Connection>,
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
    strip: bool,
) -> (Option<Providers>, Option<Providers>) {
    let whole = core.tier_providers(resolve_providers(map, module_name, parser, memo, rows));
    let persisted = match (rows, &whole) {
        (Some(conn), Some(providers)) => providers.iter().all(|m| persist_dependency(conn, m)),
        _ => false,
    };
    let stored = core.land_providers(module_name, whole.clone(), persisted, strip);
    (whole, stored)
}

#[allow(dead_code)]
pub fn resolve_and_parse(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
) -> Option<Arc<CachedModule>> {
    resolve_and_parse_all(map, module_name, parser)
        .and_then(|p| p.into_iter().next())
}

/// `resolve_and_parse` keeping the whole provider set.
#[allow(dead_code)]
pub fn resolve_and_parse_all(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
) -> Option<Providers> {
    let mut memo: ParseMemo = HashMap::new();
    resolve_and_parse_with_memo(map, module_name, parser, &mut memo)
}

/// Parse a module while sharing a memo across calls. Callers that resolve
/// many modules in a loop (the resolver thread, CLI startup) should hoist
/// one `ParseMemo` and reuse it so parent-fallback recursion doesn't
/// re-parse the same ancestor for each child.
pub fn resolve_and_parse_with_memo(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
) -> Option<Providers> {
    resolve_providers(map, module_name, parser, memo, None)
}

/// `resolve_and_parse_with_memo`, taking each file from its current row in
/// `rows` when there is one.
fn resolve_providers(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
    rows: Option<&rusqlite::Connection>,
) -> Option<Providers> {
    let mut visiting: std::collections::HashSet<String> = std::collections::HashSet::new();
    resolve_and_parse_inner(map, module_name, parser, &mut visiting, memo, rows)
}

fn resolve_and_parse_inner(
    map: &DependencyMap,
    module_name: &str,
    parser: &mut Parser,
    visiting: &mut std::collections::HashSet<String>,
    memo: &mut ParseMemo,
    rows: Option<&rusqlite::Connection>,
) -> Option<Providers> {
    if let Some(cached) = memo.get(module_name) {
        return cached.clone();
    }
    if !visiting.insert(module_name.to_string()) {
        // Cycle in `@ISA` parent fallback — bail rather than blow the stack.
        return None;
    }

    // Every provider is parsed, not just the @INC winner: a shadowed twin
    // carries its own subs and `@ISA`, and the candidate relation is only
    // as honest as its acquisition.
    let mut providers: Providers = Vec::new();
    // A file the map places under a name it does not load as (an enclosing
    // module) is not that name's provider.
    for located in map.locate(&Ask::Name(module_name.to_string())) {
        if located.loads_as.as_deref() != Some(module_name) {
            continue;
        }
        if let Some(cached) =
            parse_one_provider(map, module_name, located.path, parser, visiting, memo, rows)
        {
            providers.push(cached);
        }
    }
    if providers.is_empty() {
        return None;
    }
    memo.insert(module_name.to_string(), Some(providers.clone()));
    Some(providers)
}

/// Read + parse ONE provider file into a `CachedModule`, or take it from its
/// current row. Every provider runs this same body — the @INC winner has no
/// privileged path (rule #10).
fn parse_one_provider(
    map: &DependencyMap,
    module_name: &str,
    path: CanonicalPath,
    parser: &mut Parser,
    visiting: &mut std::collections::HashSet<String>,
    memo: &mut ParseMemo,
    rows: Option<&rusqlite::Connection>,
) -> Option<Arc<CachedModule>> {
    if let Some(fa) = rows.and_then(|conn| module_cache::load_dependency_row(conn, &path)) {
        return Some(Arc::new(CachedModule::new(path, Arc::new(fa))));
    }
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
    let mut analysis = crate::build::builder::build(&tree, source.as_bytes());
    let build_dur = t_build.map(|s| s.elapsed()).unwrap_or_default();
    crate::util::timings::record_built(module_name, parse_dur, build_dur);

    // If this module has no exports but inherits via @ISA (e.g. DDP → Data::Printer),
    // fall back to the first parent's exports. This only patches `export`/`export_ok`;
    // the parent's own cached analysis is still the source of truth for its symbols.
    if analysis.export.is_empty() && analysis.export_ok.is_empty() {
        let parents = crate::index::module_index::primary_package_parents(&analysis, module_name);
        for parent in &parents {
            let parent_primary =
                resolve_and_parse_inner(map, parent, parser, visiting, memo, rows)
                    .and_then(|p| p.into_iter().next());
            if let Some(parent_cached) = parent_primary {
                if !parent_cached.analysis.export.is_empty()
                    || !parent_cached.analysis.export_ok.is_empty()
                {
                    analysis.export = parent_cached.analysis.export.clone();
                    analysis.export_ok = parent_cached.analysis.export_ok.clone();
                    break;
                }
            }
        }
    }

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
