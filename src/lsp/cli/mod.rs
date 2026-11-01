//! The CLI machinery behind the `perl-lsp --…` modes: one-shot startup
//! helpers, usage text, and the utility commands `main()` dispatches to.

use crate::build::{language_driver, plugin};
use crate::index::{document, open_docs, module_cache, module_index, module_resolver, resolve};
use crate::lsp::{backend, symbols};
use crate::model::{file_analysis, witnesses};
use crate::util::timings;

/// Time one CLI query step — `tphase!("completion_items", expr)` prints a
/// `[PHASE]` line when `PERL_LSP_PHASE_TIMING` is set. Sugar over `timings::phase`.
macro_rules! tphase {
    ($label:literal, $body:expr) => {
        $crate::util::timings::phase($label, || $body)
    };
}

mod heatmap;
mod positions;
mod query;

pub(crate) use heatmap::*;
use positions::*;
pub(crate) use query::*;

/// Print the languages this distribution was compiled to serve. A
/// default build prints `perl`; a `cpp-lsp` build (`--features cpp`)
/// prints `perl, cpp`.
/// The ONE sanctioned CLI exit. `process::exit` skips destructors, so a bare
/// call silently discards everything the run measured — 42 exit sites vs 13
/// emit calls is how most verbs came to produce no instrumentation at all,
/// and the failure rots: the next verb exits directly and is unmeasured
/// forever. `layering_tests::cli_exits_flush_instrumentation` pins the rule.
///
/// `moment` labels the human report; the JSON sinks write regardless of it.
pub(crate) fn exit_with(code: i32, moment: &str) -> ! {
    crate::util::ghost_stats::emit_all(moment);
    std::process::exit(code); // sanctioned-exit
}

/// Lock the process to `path`'s language unless `--lang` already chose one,
/// and hand back the served driver. Exits on a refused lock — answering in a
/// language other than the one the caller asked for is never a fallback.
pub(crate) fn lock_for_file(path: &std::path::Path) -> &'static dyn language_driver::LanguageDriver {
    language_driver::LanguageRegistry::lock_process_language_for(path)
        .and_then(|d| language_driver::LanguageRegistry::admits(d, path).map(|()| d))
        .unwrap_or_else(|e| {
            eprintln!("perl-lsp: {e}");
            exit_with(2, "exit");
        })
}

/// Remove `--lang <id>` / `--lang=<id>` from `args`, locking the process to
/// `<id>`. Exits on an id this build was not compiled with, or a missing
/// value.
pub(crate) fn take_lang_flag(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let id = if a == "--lang" {
            it.next().unwrap_or_else(|| {
                eprintln!("perl-lsp: `--lang` needs a language id");
                exit_with(2, "exit");
            })
        } else if let Some(id) = a.strip_prefix("--lang=") {
            id.to_string()
        } else {
            out.push(a);
            continue;
        };
        if let Err(e) = language_driver::LanguageRegistry::lock_process_language(&id) {
            eprintln!("perl-lsp: {e}");
            exit_with(2, "exit");
        }
    }
    out
}

pub(crate) fn cli_languages() {
    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    // `id` first, maturity as a trailing parenthetical: consumers that parse
    // this line split on `, ` and read the id up to ` (`. `gold-corpus/run.pl`
    // is one — it decides which fixture rows to run from this output, so a
    // format change that hides an id silently SKIPS that language's rows
    // while still reporting green.
    let listed: Vec<String> = reg
        .languages()
        .into_iter()
        .map(|id| {
            let suffix = reg.for_id(id).map(|d| d.maturity().suffix()).unwrap_or("");
            format!("{id}{suffix}")
        })
        .collect();
    println!(
        "perl-lsp {} — languages: {}",
        env!("CARGO_PKG_VERSION"),
        listed.join(", "),
    );
}

/// Analyze one file through the served `LanguageDriver` and dump the
/// outline. A `.cpp` file goes through the C++ driver (macro reparse →
/// extract) when the process serves C++.
pub(crate) fn cli_lang_analyze(file: &str) {
    let path = std::path::Path::new(file);
    let Ok(src) = std::fs::read_to_string(path) else {
        eprintln!("cannot read {file}");
        exit_with(1, "exit");
    };
    let driver = lock_for_file(path);
    // Persist the transitive macro table across invocations — keyed on the
    // file's directory (the CLI has no workspace root). Without this the LSP-only
    // `set_macro_persist_dir` never fires here, so every `--lang-analyze` run
    // re-gathered the whole #include closure cold (op.c: ~1.5s). The on-disk tier
    // now makes a second run warm.
    if let Some(dir) = path.parent() {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        crate::build::cpp_reparse::set_macro_persist_dir(
            crate::index::module_cache::cache_dir_for_workspace(Some(&key.to_string_lossy())),
        );
    }
    // `PERL_LSP_BENCH_ITERS=N` re-analyzes N times in-process — the 2nd+ runs
    // are WARM (macro tables cached), which is where the per-analyze macro-
    // expansion cost shows. Combine with `PERL_LSP_PHASE_TIMING=1` for the
    // gather/expand breakdown. Default 1 (single cold analyze).
    let iters: usize = std::env::var("PERL_LSP_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(1);
    let mut fa = driver.analyze_with_path(&src, Some(std::path::Path::new(path)));
    for _ in 1..iters {
        fa = driver.analyze_with_path(&src, Some(std::path::Path::new(path)));
    }
    println!("# {file} [{}] — {} symbols", driver.id(), fa.symbols().len());
    for s in fa.symbols() {
        let pkg = s.package.as_deref().unwrap_or("");
        let sep = if pkg.is_empty() { "" } else { "::" };
        println!(
            "{:<8} {pkg}{sep}{}\t{}:{}",
            format!("{:?}", s.kind),
            s.name,
            s.span.start.row + 1,
            s.span.start.column + 1,
        );
    }
}

pub(crate) fn print_usage() {
    eprintln!("perl-lsp {} — Perl Language Server", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  perl-lsp                                              Start LSP server (stdio)");
    eprintln!();
    eprintln!("  Cursor position — two input forms, output MATCHES the form you use:");
    eprintln!("    positional  <file> <line> <col>       0-based line, byte column (engine-native)");
    eprintln!("                                          -> output renders 0-based / byte too");
    eprintln!("    editor      --at <file>:<line>:<col>  1-based line, character column (editor-native,");
    eprintln!("                                          matches grep -n and this tool's own");
    eprintln!("                                          path:line:col output) -> output renders");
    eprintln!("                                          1-based / char, so it round-trips straight");
    eprintln!("                                          back into the next query's --at");
    eprintln!("    e.g.  perl-lsp --definition <root> lib/Foo.pm 12 4");
    eprintln!("          perl-lsp --references <root> --at lib/Foo.pm:13:5");
    eprintln!("    Each cursor query prints a [pos] annotation to stderr showing how the");
    eprintln!("    input was read, the internal 0-based point, and the landed token.");
    eprintln!("    --rename accepts both forms too (its edit spans follow the same rule).");
    eprintln!("    NOTE: --workspace-symbol / --outline JSON always emit engine coordinates");
    eprintln!("          (0-based / byte); the --batch protocol keeps its established output.");
    eprintln!();
    eprintln!("ANALYSIS:");
    eprintln!("  perl-lsp --check [<root>] [--severity error|warning]    Batch diagnostics (CI)");
    eprintln!("                           [--format json|human]");
    eprintln!("                           [--timings]                    Per-module build-timing report (stderr, slowest-first)");
    eprintln!("  perl-lsp --outline <file>                              Document symbol outline");
    eprintln!("  perl-lsp --hover [<root>] <file> <line> <col>         Type info and docs (root = cross-file)");
    eprintln!("  perl-lsp --type-at <file> <line> <col>                 Single type query");
    eprintln!("  perl-lsp --definition <root> <file> <line> <col>       Cross-file goto-def");
    eprintln!("  perl-lsp --type-definition <root> <file> <line> <col>  Def of the value's inferred class");
    eprintln!("  perl-lsp --implementations <root> <file> <line> <col>  Descendant defs (role composers, overrides)");
    eprintln!("  perl-lsp --type-hierarchy <root> <file> <line> <col>   Class item + direct supertypes/subtypes");
    eprintln!("  perl-lsp --call-hierarchy <root> <file> <line> <col>   Callable item + incoming/outgoing calls");
    eprintln!("  perl-lsp --references <root> <file> <line> <col>       Cross-file find-refs");
    eprintln!("  perl-lsp --completion <root> <file> <line> <col>       Completion items at point");
    eprintln!("  perl-lsp --signature-help <root> <file> <line> <col>   Signature help at point");
    eprintln!("  perl-lsp --document-highlight <root> <file> <line> <col> In-file occurrences (read/write)");
    eprintln!("  perl-lsp --linked-editing <root> <file> <line> <col>   Linked-editing occurrence set");
    eprintln!("  perl-lsp --semantic-tokens <root> <file>               Semantic token classification");
    eprintln!("  perl-lsp --document-link <root> <file>                 POD L<> links, comment URLs, path loads");
    eprintln!();
    eprintln!("REFACTORING:");
    eprintln!("  perl-lsp --rename <root> <file> <line> <col> <new>     Cross-file rename");
    eprintln!("  perl-lsp --workspace-symbol <root> <query>             Search symbols");
    eprintln!("  perl-lsp --batch <root>                                Stream JSONL queries (one startup, many)");
    eprintln!();
    eprintln!("INSIGHT:");
    eprintln!("  perl-lsp --heatmap <root> [--csv|--html] [--include-deps] [--all]");
    eprintln!("                                                         Per-symbol usage (fan-in/fan-out)");
    eprintln!("                                                         + unreferenced-symbol candidates");
    eprintln!("                                                         (JSON default; --csv / --html viewer)");
    eprintln!();
    eprintln!("PLUGIN AUTHORING:");
    eprintln!("  perl-lsp --plugin-check <file.rhai>                    Lint a Rhai plugin");
    eprintln!("  perl-lsp --plugin-run <file.rhai> --on <fixture.pl>    Run plugin on one Perl file");
    eprintln!("  perl-lsp --plugin-test <plugin-dir> [--update]         Snapshot-test a plugin dir");
    eprintln!();
    eprintln!("DEBUG:");
    eprintln!("  perl-lsp --equiv-dump <out-dir> <file|dir>...          Canonical analysis dump of every");
    eprintln!("                                                         file, for base-vs-head diffs");
    eprintln!("                                                         (scripts/equiv/README.md)");
    eprintln!("  perl-lsp --dump-package <root> <package>               Dump every sub in <package>");
    eprintln!("                                                         with derived type info");
    eprintln!("  perl-lsp --gc-cache <root>                             Reclaim unreferenced interned");
    eprintln!("                                                          strings (run when nothing else");
    eprintln!("                                                          is writing the cache)");
    eprintln!("  perl-lsp --clear-cache [<root>]                        Wipe the module cache for");
    eprintln!("                                                         <root>, or every project if");
    eprintln!("                                                         <root> is omitted");
    eprintln!("  perl-lsp --parse <file|->                              Print the served language's");
    eprintln!("                                                         tree-sitter parse tree (`-` reads");
    eprintln!("                                                         stdin; `--lang cpp` for C++)");
    eprintln!();
    eprintln!("LANGUAGES (pack drivers, opt-in at build time; one language per process):");
    eprintln!("  perl-lsp --lang <id> [...]                             Serve <id> instead of Perl. Any mode;");
    eprintln!("                                                         a file-target verb without it serves");
    eprintln!("                                                         its file's language");
    eprintln!("  perl-lsp --languages                                   Languages this build can serve");
    eprintln!("  perl-lsp --lang-analyze <file>...                      Analyze file(s) via the served driver");
    eprintln!("                                                         (shared caches; dump outline)");
    eprintln!("    Build a cpp-lsp:  cargo build --features cpp; run `perl-lsp --lang cpp`");
    eprintln!();
    eprintln!("  perl-lsp --version                                     Print version");
}

// ---- Helpers ----

fn parse_file(path: &str) -> (String, tree_sitter::Tree, file_analysis::FileAnalysis) {
    let source = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Cannot read {}: {}", path, e);
        exit_with(1, "exit");
    });
    // The served driver, as the LSP server would analyze it. A driver that
    // builds from the caller's tree (no pre-parse transform) analyzes the
    // one parse we already hold; the rest analyze independently.
    let driver = lock_for_file(std::path::Path::new(path));
    let mut parser = driver.make_parser();
    let tree = parser.parse(&source, None).unwrap_or_else(|| {
        eprintln!("Parse failed: {}", path);
        exit_with(1, "exit");
    });
    let analysis = driver
        .analyze_from_tree(&tree, &source)
        .unwrap_or_else(|| driver.analyze_with_path(&source, Some(std::path::Path::new(path))));
    (source, tree, analysis)
}

fn parse_point(line_str: &str, col_str: &str) -> tree_sitter::Point {
    let line: usize = line_str.parse().unwrap_or_else(|_| {
        eprintln!("line must be a number");
        exit_with(1, "exit");
    });
    let col: usize = col_str.parse().unwrap_or_else(|_| {
        eprintln!("col must be a number");
        exit_with(1, "exit");
    });
    tree_sitter::Point::new(line, col)
}

/// Canonicalize `root` and produce the matching `file://...` URI.
/// Returns both because callers usually want the path (for `@INC`
/// project-lib discovery, file walking) AND the URI (the cache hash
/// key, since the LSP server's `cache_dir_for_workspace` is fed the
/// initialize request's `root_uri` string verbatim — `file://...`).
/// Falls back to the raw input if `canonicalize` fails (path doesn't
/// exist, permissions): same string lands in both halves so the
/// caller's downstream "does this dir exist?" check still fires.
fn canonical_root_and_uri(root: &str) -> (std::path::PathBuf, String) {
    let path = std::path::Path::new(root)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(root));
    let uri = format!("file://{}", path.display());
    (path, uri)
}

/// Full CLI workspace setup: index the workspace, then wait for the
/// resolver thread to land what the project names (it warms and persists
/// through the store as the server's does). Mirrors the LSP server's
/// startup, run to completion before the verb answers. Used by every
/// CLI command that needs cross-file resolution to behave like the
/// running server.
fn cli_full_startup(root: &str) -> (open_docs::OpenDocs, module_index::ModuleIndex) {
    // `--check --timings` already flipped this on; the env var lets any
    // startup-driven CLI mode (dump-package, etc.) opt in too.
    timings::enable_from_env();
    let (root_path, root_uri) = canonical_root_and_uri(root);
    // Pin repo-local `.perl-lsp/` plugin discovery to the same root the
    // cache keys on, before the first build() (workspace indexing) fires.
    plugin::rhai_host::set_workspace_root(Some(&root_uri));

    // Register workspace files INTO the module index (not just a OpenDocs)
    // so their plugin bridges + package names participate in cross-file
    // lookups — the whole point of "act like the server just started".
    // Indexing without the index (bridge-less) is what forced callers to
    // hand-roll their own `index_workspace_with_index`, and they drifted.
    let module_index = module_index::ModuleIndex::new_for_cli();
    module_index.mark_long_lived_from_env();
    // Wake the headless resolver: it blocks on this channel for the
    // dependency map and the store warm.
    module_index.set_workspace_root(Some(root_uri.as_str()));
    let ws = open_docs::OpenDocs::new();
    // Only the served language's tree is walked — the same choice the
    // server's `didOpen` latch makes.
    let served = language_driver::LanguageRegistry::served();
    let indexed = if served.caps().hub_enrichment {
        timings::phase("cli::index_workspace", || {
            module_resolver::index_workspace_with_index(&root_path, &module_index, None, None)
        })
    } else {
        timings::phase("cli::index_pack", || {
            module_resolver::index_pack_language(
                served,
                &root_path,
                Some(&root_uri),
                &module_index,
                None,
                crate::lsp::backend::max_cache_mb_default() as usize * 1024 * 1024,
            )
        })
    };
    if indexed > 0 {
        eprintln!(
            "Indexed {} {} files",
            indexed,
            language_driver::LanguageRegistry::display_name(served.id())
        );
    }

    // Every language's dependencies land on the resolver thread, asked by
    // the indexer as a server's are; a one-shot verb answers from the
    // settled index, so it waits for what the project's own files name
    // (and what those landings ask in turn).
    timings::phase("cli::resolve_dependencies", || module_index.wait_idle());
    if !served.caps().hub_enrichment {
        return (ws, module_index);
    }

    // Landings feed their own edges (`publish_resolved`); the rebuild
    // re-picks each name's winner and re-feeds the candidates, so
    // `find_exporters` answers identically on cold and warm runs.
    timings::phase("cli::rebuild_reverse_index", || {
        module_index.rebuild_reverse_index_from_cache()
    });

    (ws, module_index)
}

fn is_json_format(args: &[String]) -> bool {
    args.windows(2).any(|w| w[0] == "--format" && w[1] == "json")
}

fn get_arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].as_str())
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "error" => 0,
        "warning" => 1,
        "info" => 2,
        "hint" => 3,
        _ => 1,
    }
}

/// --gc-cache <root> — reclaim interned strings nothing references.
///
/// A MAINTENANCE verb, not an automatic sweep, and the distinction is the
/// point. `strings` is append-only in normal operation: every deletion path
/// drops `refs`/`syms` rows and leaves their names behind, so a long-lived
/// workspace accumulates every name it has ever seen (measured: deleting
/// half a 300-module set orphans 34.6% of the table, deleting all orphans
/// 100%).
///
/// It is not wired into startup because `shred_derived_rows` has standalone
/// autocommit callers — the watcher's invalidation path among them — whose
/// intern and row-insert land in SEPARATE transactions. A sweep between the
/// two would free a string the insert is about to reference, and the rows
/// written after it would carry a `name_id` nothing joins to: retrieval
/// answers EMPTY, not wrong, but silently. Making it automatic means giving
/// the shred one transaction so intern-and-insert cannot be split; until
/// then this runs when the caller knows nothing else is writing.
pub(crate) fn cli_gc_cache(root: &str) {
    let (_, root_uri) = canonical_root_and_uri(root);
    let Some(conn) = module_cache::open_cache_db(Some(&root_uri), "perl") else {
        eprintln!("No cache database for {root}");
        exit_with(1, "exit");
    };
    use module_cache::sql;
    let n = |count: sql::Sql| -> i64 {
        conn.query_row(count.registered(), [], |r| r.get(0)).unwrap_or(-1)
    };
    let before = n(sql::GC_COUNT_STRINGS);
    // Report the whole derived store, not just the table being swept: a
    // `strings` sweep that reclaims nothing because DEAD FILE rows are still
    // holding their names looks identical to a store with no garbage in it,
    // and those are very different situations.
    eprintln!(
        "before: files={} refs={} syms={} held={} strings={} (of which {} orphaned)",
        n(sql::GC_COUNT_FILES),
        n(sql::GC_COUNT_REFS),
        n(sql::GC_COUNT_SYMS),
        n(sql::GC_COUNT_HELD),
        before,
        n(sql::GC_COUNT_ORPHANED_STRINGS),
    );
    let missing = n(sql::GC_COUNT_WORKSPACE_FILES);
    let reclaimed = module_cache::gc_strings(&conn);
    eprintln!(
        "Reclaimed {reclaimed} of {before} interned strings ({:.1}%); {missing} workspace file rows remain",
        100.0 * reclaimed as f64 / before.max(1) as f64
    );
}

/// --clear-cache [<root>] — Remove the SQLite module cache.
///
/// Without `<root>`: nuke the entire `~/.cache/perl-lsp` (or
/// `$XDG_CACHE_HOME/perl-lsp`) tree — every project the user has
/// touched. With `<root>`: only the per-project cache dir
/// (`<base>/<workspace_hash>`) is removed; other projects keep theirs.
///
/// `<root>` is canonicalized and joined with `file://` to match the
/// hash key the LSP server and `cli_full_startup` write under (the
/// initialize request hands a `file://` URI; both CLI and server feed
/// that string into `cache_dir_for_workspace`). Without canonicalizing
/// here a relative path would hash to a different bucket and silently
/// "clear" a non-existent dir.
///
/// On the next LSP start the cache is recreated from scratch — the
/// resolver re-resolves modules lazily and the workspace indexer
/// re-walks the tree. Pure side-effect command; prints what was
/// removed so it's obvious whether anything happened.
pub(crate) fn cli_clear_cache(root: Option<&str>) {
    let target = match root {
        Some(r) => {
            let (_, root_uri) = canonical_root_and_uri(r);
            module_cache::cache_dir_for_workspace(Some(&root_uri))
        }
        None => module_cache::cache_base_dir(),
    };
    let Some(path) = target else {
        eprintln!("Cannot determine cache dir: $HOME and $XDG_CACHE_HOME are both unset");
        exit_with(1, "exit");
    };
    if !path.exists() {
        eprintln!("Cache already absent: {}", path.display());
        return;
    }
    match std::fs::remove_dir_all(&path) {
        Ok(()) => eprintln!("Cleared cache: {}", path.display()),
        Err(e) => {
            eprintln!("Failed to remove {}: {}", path.display(), e);
            exit_with(1, "exit");
        }
    }
}

/// `--equiv-dump <out-dir> <file|dir>...`: build every file of the served
/// language under the given paths and write each analysis's canonical dump
/// to `<out-dir>` (`build/equiv_dump.rs`). Builds only — no index, no cache,
/// no enrichment — so the dump is a function of the inputs and the binary.
/// Directories are walked whole, ignore files included: a dependency tree
/// such as the gold substrate is usually git-ignored.
pub(crate) fn cli_equiv_dump(out: &str, paths: &[String]) {
    use rayon::prelude::*;
    if let Err(e) = crate::build::equiv_dump::enable(std::path::PathBuf::from(out)) {
        eprintln!("perl-lsp: --equiv-dump: {e}");
        exit_with(2, "exit");
    }
    let served = language_driver::LanguageRegistry::served();
    let claims = |p: &std::path::Path| {
        language_driver::LanguageRegistry::compiled()
            .for_path(p)
            .is_some_and(|d| d.id() == served.id())
    };
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for root in paths {
        let root = std::path::Path::new(root);
        if root.is_file() {
            files.push(root.to_path_buf());
            continue;
        }
        for entry in ignore::WalkBuilder::new(root).standard_filters(false).build().flatten() {
            let p = entry.path();
            if entry.file_type().is_some_and(|t| t.is_file()) && claims(p) {
                files.push(p.to_path_buf());
            }
        }
    }
    files.sort();
    let unreadable: usize = files
        .par_iter()
        .map(|path| {
            // Non-UTF-8 is skipped, as the workspace walk skips it.
            let Ok(src) = std::fs::read_to_string(path) else { return 1 };
            let _file = timings::current_file_scope(Some(path));
            served.analyze_with_path(&src, Some(path));
            0
        })
        .sum();
    eprintln!(
        "equiv-dump: {} {} files built, {} unreadable -> {}",
        files.len() - unreadable,
        served.id(),
        unreadable,
        out
    );
}

/// Pretty-print the tree-sitter parse tree for a Perl source file.
/// `<file>` may be `-` to read from stdin. Mirrors `tree-sitter parse`
/// output shape — `(node_kind [row, col] - [row, col]` per line,
/// 2-space indent per depth, field names prefixed (`field: kind`).
pub(crate) fn cli_parse(path: &str) {
    use std::io::Read;
    let source = if path == "-" {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            eprintln!("read stdin: {}", e);
            exit_with(1, "exit");
        }
        s
    } else {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("read {}: {}", path, e);
                exit_with(1, "exit");
            }
        }
    };
    // The served grammar — the SAME tree the extractor sees. stdin has no
    // path, so it parses as the `--lang` language (Perl without one).
    let driver = if path == "-" {
        language_driver::LanguageRegistry::served()
    } else {
        lock_for_file(std::path::Path::new(path))
    };
    let mut parser = driver.make_parser();
    let Some(tree) = parser.parse(&source, None) else {
        eprintln!("parse failed");
        exit_with(1, "exit");
    };
    use std::io::IsTerminal;
    let color = std::io::stdout().is_terminal();
    // 6 rainbow ANSI 256-color picks. Both the paren AND the node
    // kind name share the depth's color — that's the visual cue
    // for "this paren matches this kind." Field names + line
    // ranges get distinct colors so they don't blur into the
    // rainbow.
    const RAINBOW: [&str; 6] = [
        "\x1b[38;5;196m", // red
        "\x1b[38;5;208m", // orange
        "\x1b[38;5;226m", // yellow
        "\x1b[38;5;46m",  // green
        "\x1b[38;5;39m",  // blue
        "\x1b[38;5;165m", // magenta
    ];
    const FIELD: &str = "\x1b[38;5;245m"; // gray
    const RANGE: &str = "\x1b[38;5;242m"; // darker gray
    const RESET: &str = "\x1b[0m";
    fn walk(node: tree_sitter::Node, field: Option<&str>, depth: usize, color: bool) {
        let pad = "  ".repeat(depth);
        let (hue, fc, rc, rs) = if color {
            (RAINBOW[depth % 6], FIELD, RANGE, RESET)
        } else {
            ("", "", "", "")
        };
        let prefix = field
            .map(|f| format!("{}{}: {}", fc, f, rs))
            .unwrap_or_default();
        let s = node.start_position();
        let e = node.end_position();
        print!(
            "{}{}{}({}{} {}[{}, {}] - [{}, {}]{}",
            pad,
            prefix,
            hue,
            node.kind(),
            rs,
            rc,
            s.row,
            s.column,
            e.row,
            e.column,
            rs,
        );
        let mut cursor = node.walk();
        let mut field_idx: u32 = 0;
        for child in node.children(&mut cursor) {
            let fname = node.field_name_for_child(field_idx);
            field_idx += 1;
            if !child.is_named() {
                continue;
            }
            println!();
            walk(child, fname, depth + 1, color);
        }
        print!("{}){}", hue, rs);
    }
    walk(tree.root_node(), None, 0, color);
    println!();
}
