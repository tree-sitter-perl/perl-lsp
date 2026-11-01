//! The @INC tier: module-file location over the search path Perl's
//! dependency map (`build::inc`) assembles, and the in-process parse with
//! its parent-fallback memo.

use super::*;
use crate::util::canonical_path::CanonicalPath;

// ---- Module parsing ----

/// Run-local memo for `resolve_and_parse_with_memo`. Persists across many
/// top-level calls within a single resolver sweep so that parent-fallback
/// recursion (e.g. 50 children all inheriting from `Exporter`) parses each
/// parent exactly once.
pub type ParseMemo = HashMap<String, Option<Providers>>;

/// Parse a module file directly in-process.
/// tree-sitter-perl is stable — no subprocess isolation needed.
pub(super) fn parse_module(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
) -> Option<Providers> {
    resolve_and_parse_with_memo(inc_paths, module_name, parser, memo)
}

// ---- Resolution ----

/// EVERY `@INC` root that provides `module_name`, in `@INC` order — the
/// `(name, inc-root)` relation's acquisition half. A name maps to a SET of
/// files (XS/PP twins, a project `lib/` shadowing an installed copy,
/// `t/lib` vs `lib` per entrypoint); stopping at the first hit is what made
/// the tier answer from whichever root happened to win. Perl's map over
/// `inc_paths` locates them, canonical, deduped to one candidate per file
/// (a symlinked `lib`, a duplicated @INC entry): the per-asker search-path
/// rank prefix-matches a candidate against canonical roots with no
/// query-time `canonicalize`, which is filesystem I/O on a request path.
pub fn resolve_module_paths(inc_paths: &[PathBuf], module_name: &str) -> Vec<CanonicalPath> {
    use crate::build::language_driver::{DependencyLocator, PerlDriver};
    let inc = crate::build::search_path::SearchPathLocator::new(&PerlDriver, inc_paths.to_vec(), Vec::new(), Vec::new());
    let mut out: Vec<CanonicalPath> = Vec::new();
    for l in inc.locate(module_name) {
        let canon = CanonicalPath::resolve(l.path);
        if !out.contains(&canon) {
            out.push(canon);
        }
    }
    out
}

#[allow(dead_code)]
pub fn resolve_and_parse(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
) -> Option<Arc<CachedModule>> {
    resolve_and_parse_all(inc_paths, module_name, parser)
        .and_then(|p| p.into_iter().next())
}

/// `resolve_and_parse` keeping the whole provider set.
#[allow(dead_code)]
pub fn resolve_and_parse_all(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
) -> Option<Providers> {
    let mut memo: ParseMemo = HashMap::new();
    resolve_and_parse_with_memo(inc_paths, module_name, parser, &mut memo)
}

/// Parse a module while sharing a memo across calls. Callers that resolve
/// many modules in a loop (the resolver thread, CLI startup) should hoist
/// one `ParseMemo` and reuse it so parent-fallback recursion doesn't
/// re-parse the same ancestor for each child.
pub fn resolve_and_parse_with_memo(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
) -> Option<Providers> {
    let mut visiting: std::collections::HashSet<String> = std::collections::HashSet::new();
    resolve_and_parse_inner(inc_paths, module_name, parser, &mut visiting, memo)
}

fn resolve_and_parse_inner(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
    visiting: &mut std::collections::HashSet<String>,
    memo: &mut ParseMemo,
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
    for path in resolve_module_paths(inc_paths, module_name) {
        if let Some(cached) =
            parse_one_provider(inc_paths, module_name, path, parser, visiting, memo)
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

/// Read + parse ONE provider file into a `CachedModule`. Every provider runs
/// this same body — the @INC winner has no privileged path (rule #10).
fn parse_one_provider(
    inc_paths: &[PathBuf],
    module_name: &str,
    path: CanonicalPath,
    parser: &mut Parser,
    visiting: &mut std::collections::HashSet<String>,
    memo: &mut ParseMemo,
) -> Option<Arc<CachedModule>> {
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
    let source = std::fs::read_to_string(&path).ok()?;

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
                resolve_and_parse_inner(inc_paths, parent, parser, visiting, memo)
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

/// Scan @INC directories for .pm files, populating the available_modules map.
/// Fast — no file reads, just directory traversal + path→module name conversion.
pub(super) fn scan_inc_module_names(inc_paths: &[PathBuf], available: &DashMap<String, PathBuf>) {
    for inc in inc_paths {
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
                let module_name = rel.to_string_lossy()
                    .trim_end_matches(".pm")
                    .replace(std::path::MAIN_SEPARATOR, "::");
                // Roots are walked in @INC order, so the FIRST one to claim a
                // name is the one `require` would load. Overwriting here made
                // the availability map name the LAST root — disagreeing with
                // `resolve_module_paths` about which file a name means.
                available.entry(module_name).or_insert_with(|| path.clone());
            }
        }
    }
}
