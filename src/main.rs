mod backend;
mod builder;
mod builtins_pod;
mod conventions;
mod cpanfile;
mod cst;
mod cursor_context;
mod cursor_slot;
mod document;
mod file_analysis;
mod file_store;
mod graph;
mod module_cache;
mod module_index;
mod module_resolver;
mod pack_bag_cache;
mod panic_guard;
mod plugin;
mod plugin_cli;
mod pod;
mod query_cache;
mod surface;
mod language_driver;
// Compiled unconditionally (symbols.rs consumes the macro-model surface in
// every build); the driver registration is feature-gated, so a perl-only
// build leaves most of the module unreferenced — silence dead-code there
// while keeping the all-langs build strict.
#[cfg_attr(not(feature = "cpp"), allow(dead_code))]
mod cpp_reparse;
mod cpp_macro_model;
mod cpp_toolchain;
mod cursor_sentinel;
#[cfg_attr(
    not(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake")),
    allow(dead_code)
)]
mod query_extract;
// Kept-as-spike: the Perl prototype reparenthesizer that proved the
// pre-extraction reparse seam (whose production form is cpp_reparse).
#[allow(dead_code)]
mod reparse;
mod resolve;
mod symbols;
mod timings;
mod witnesses;

#[cfg(test)]
#[path = "layering_tests.rs"]
mod layering_tests;

use backend::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("perl-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // CLI modes — dispatch before starting LSP server
    match args.get(1).map(|s| s.as_str()) {
        Some("--check") => {
            cli_check(&args[2..]);
            return;
        }
        Some("--outline") if args.len() >= 3 => {
            cli_outline(&args[2]);
            return;
        }
        // `--hover <root> <file> <line> <col>` / `<root> --at <f:l:c>` enables
        // cross-file hover (same setup as the server); the legacy
        // `--hover <file> <line> <col>` form stays single-file. The cross-file
        // form is disambiguated by a leading `--at` or a 4th positional arg.
        Some("--hover") if args.len() >= 5 && args.get(3).map(|s| s == "--at").unwrap_or(false) => {
            cli_cursor("hover", &args[2], &args[3..]);
            return;
        }
        Some("--hover") if args.len() >= 6 => {
            cli_cursor("hover", &args[2], &args[3..6]);
            return;
        }
        Some("--hover") if args.len() == 5 => {
            cli_hover_single_file(&args[2], &args[3], &args[4]);
            return;
        }
        Some("--type-at") if args.len() >= 5 => {
            cli_type_at(&args[2], &args[3], &args[4]);
            return;
        }
        // The uniform cursor queries: `<root>` then either `<file> <line> <col>`
        // (positional, 0-based/byte) or `--at <file>:<line>:<col>` (editor,
        // 1-based/char). Flag names map 1:1 to the `run_one` query string.
        Some(
            flag @ ("--definition" | "--references" | "--implementations" | "--completion"
            | "--signature-help" | "--document-highlight" | "--linked-editing"),
        ) if args.len() >= 4 => {
            cli_cursor(&flag[2..], &args[2], &args[3..]);
            return;
        }
        Some("--semantic-tokens") if args.len() >= 4 => {
            cli_semantic_tokens(&args[2], &args[3]);
            return;
        }
        // `--rename <root> <file> <line> <col> <new>` or
        // `--rename <root> --at <file>:<line>:<col> <new>`.
        Some("--rename")
            if args.len() == 6 && args.get(3).map(|s| s == "--at").unwrap_or(false) =>
        {
            cli_rename(&args[2], &args[3..5], &args[5]);
            return;
        }
        Some("--rename") if args.len() == 7 => {
            cli_rename(&args[2], &args[3..6], &args[6]);
            return;
        }
        Some("--workspace-symbol") if args.len() >= 4 => {
            cli_workspace_symbol(&args[2], &args[3]);
            return;
        }
        Some("--heatmap") if args.len() >= 3 => {
            cli_heatmap(&args[2], &args[3..]);
            return;
        }
        Some("--refs-parity") if args.len() >= 3 => {
            let sample = args.get(3).and_then(|a| a.strip_prefix("--sample="))
                .and_then(|n| n.parse::<usize>().ok());
            cli_refs_parity(&args[2], sample);
            return;
        }
        Some("--batch") if args.len() >= 3 => {
            cli_batch(&args[2]);
            return;
        }
        Some("--plugin-check") => {
            plugin_cli::cli_plugin_check(&args[2..]);
            return;
        }
        Some("--plugin-run") => {
            plugin_cli::cli_plugin_run(&args[2..]);
            return;
        }
        Some("--plugin-test") => {
            plugin_cli::cli_plugin_test(&args[2..]);
            return;
        }
        Some("--dump-package") if args.len() >= 4 => {
            cli_dump_package(&args[2], &args[3]);
            return;
        }
        Some("--clear-cache") => {
            cli_clear_cache(args.get(2).map(|s| s.as_str()));
            return;
        }
        Some("--parse") if args.len() >= 3 => {
            cli_parse(&args[2], args.get(3).map(|s| s.as_str()));
            return;
        }
        Some("--languages") => {
            cli_languages();
            return;
        }
        Some("--lang-analyze") if args.len() >= 3 => {
            // Multiple files analyze in ONE process, sharing the process-global
            // header/macro caches — so a second C++ file's gather reuses the
            // first's parsed headers (op.c → sv.c is near-free).
            for f in &args[2..] {
                cli_lang_analyze(f);
            }
            return;
        }
        // A `--flag` first arg that matched no CLI arm above is a malformed or
        // unknown invocation (usually a valid flag with the wrong argument
        // count). Refuse to fall through to the LSP server — doing so silently
        // starts a stdio server that hangs forever waiting for a client that
        // will never come (the leak that reaped a `--definition` missing its
        // <root>). Error loudly and exit instead.
        Some(flag) if flag.starts_with("--") => {
            eprintln!(
                "perl-lsp: unrecognized or malformed CLI invocation: `{}`",
                args.join(" ")
            );
            eprintln!("(a valid flag with the wrong argument count lands here; run `perl-lsp` with no args for the LSP server)");
            std::process::exit(2);
        }
        _ => {}
    }

    env_logger::init();

    // Bridge stdio through dedicated OS threads instead of `tokio::io::stdin()`
    // / `stdout()`. Tokio's stdin wrapper has a lost-wakeup race under load: a
    // complete LSP frame can sit fully buffered while `FramedRead` is never
    // re-polled, so the server never decodes the client's `initialize` and the
    // session hangs (the client waits for a response it will only get if it
    // sends more bytes). A plain blocking reader on its own thread, piped in via
    // a channel, has no such race. See `stdio_bridge`.
    let stdin = stdio_bridge::reader();
    let stdout = stdio_bridge::writer();

    let (service, socket) = LspService::new(Backend::new);

    // Wrap the service so a panic in any handler degrades to a logged warning +
    // graceful response instead of unwinding tower-lsp's single `serve` task.
    // See `panic_guard` for why the boundary lives here and not per-handler.
    Server::new(stdin, stdout, socket)
        .serve(panic_guard::PanicGuard::new(service))
        .await;

    // `serve()` returns when the client's stdin/socket reaches EOF — the CLEAN
    // `shutdown`+`exit` path AND the UNCLEAN path (editor crash / kill with no
    // `exit` notification) both land here. Exit explicitly rather than letting
    // `main` return into the tokio-runtime drop: background `spawn_blocking`
    // work (workspace indexing does `rt.block_on(client.send_request(..))`,
    // which parks forever once the client is gone) can otherwise keep the
    // runtime — and the process — alive, orphaning a 40-thread server to init.
    // An LSP server has nothing to flush after the connection closes.
    std::process::exit(0);
}

/// A blocking-thread bridge for stdio, replacing `tokio::io::stdin/stdout`.
///
/// The reader thread does plain blocking `read()`s on fd 0 and forwards chunks
/// over an mpsc channel; `poll_recv` delivers them to the async side with
/// correct waker semantics (no lost wakeups). The writer mirrors it: an
/// unbounded channel feeds a thread that writes + flushes fd 1 in order.
mod stdio_bridge {
    use std::io::{Read, Write};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::sync::mpsc;

    const CHUNK: usize = 64 * 1024;

    pub struct ChannelReader {
        rx: mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
        pos: usize,
    }

    pub fn reader() -> ChannelReader {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        std::thread::Builder::new()
            .name("lsp-stdin".into())
            .spawn(move || {
                let mut stdin = std::io::stdin().lock();
                let mut buf = vec![0u8; CHUNK];
                loop {
                    match stdin.read(&mut buf) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            if tx.blocking_send(buf[..n].to_vec()).is_err() {
                                break; // server gone
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn lsp-stdin thread");
        ChannelReader { rx, buf: Vec::new(), pos: 0 }
    }

    impl AsyncRead for ChannelReader {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            out: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let me = self.get_mut();
            if me.pos >= me.buf.len() {
                match me.rx.poll_recv(cx) {
                    Poll::Ready(Some(chunk)) => {
                        me.buf = chunk;
                        me.pos = 0;
                    }
                    Poll::Ready(None) => return Poll::Ready(Ok(())), // EOF
                    Poll::Pending => return Poll::Pending,
                }
            }
            let n = std::cmp::min(out.remaining(), me.buf.len() - me.pos);
            out.put_slice(&me.buf[me.pos..me.pos + n]);
            me.pos += n;
            Poll::Ready(Ok(()))
        }
    }

    pub struct ChannelWriter {
        tx: mpsc::UnboundedSender<Vec<u8>>,
    }

    pub fn writer() -> ChannelWriter {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("lsp-stdout".into())
            .spawn(move || {
                let mut stdout = std::io::stdout().lock();
                while let Some(chunk) = rx.blocking_recv() {
                    if stdout.write_all(&chunk).is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
            })
            .expect("spawn lsp-stdout thread");
        ChannelWriter { tx }
    }

    impl AsyncWrite for ChannelWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            match self.tx.send(buf.to_vec()) {
                Ok(()) => Poll::Ready(Ok(buf.len())),
                Err(_) => Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "stdout thread gone",
                ))),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(())) // writer thread flushes each chunk
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
}

/// Print the languages this distribution was compiled to serve. A
/// default build prints `perl`; a `cpp-lsp` build (`--features cpp`)
/// prints `perl, cpp`.
fn cli_languages() {
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    println!(
        "perl-lsp {} — languages: {}",
        env!("CARGO_PKG_VERSION"),
        reg.languages().join(", "),
    );
}

/// Analyze one file through its `LanguageDriver` and dump the outline —
/// the multi-language seam at the CLI. Routes by extension; a `.cpp`
/// file goes through the C++ driver (macro reparse → extract) when this
/// binary was built `--features cpp`.
fn cli_lang_analyze(file: &str) {
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let path = std::path::Path::new(file);
    let Ok(src) = std::fs::read_to_string(path) else {
        eprintln!("cannot read {file}");
        std::process::exit(1);
    };
    let Some(driver) = reg.for_path_sniffed(path, &src) else {
        eprintln!("no driver for {file} (this build serves: {})", reg.languages().join(", "));
        std::process::exit(1);
    };
    // Persist the transitive macro table across invocations — keyed on the
    // file's directory (the CLI has no workspace root). Without this the LSP-only
    // `set_macro_persist_dir` never fires here, so every `--lang-analyze` run
    // re-gathered the whole #include closure cold (op.c: ~1.5s). The on-disk tier
    // now makes a second run warm.
    if let Some(dir) = path.parent() {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        crate::cpp_reparse::set_macro_persist_dir(
            crate::module_cache::cache_dir_for_workspace(Some(&key.to_string_lossy())),
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
    println!("# {file} [{}] — {} symbols", driver.id(), fa.symbols.len());
    for s in &fa.symbols {
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

fn print_usage() {
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
    eprintln!("  perl-lsp --implementations <root> <file> <line> <col>  Descendant defs (role composers, overrides)");
    eprintln!("  perl-lsp --references <root> <file> <line> <col>       Cross-file find-refs");
    eprintln!("  perl-lsp --completion <root> <file> <line> <col>       Completion items at point");
    eprintln!("  perl-lsp --signature-help <root> <file> <line> <col>   Signature help at point");
    eprintln!("  perl-lsp --document-highlight <root> <file> <line> <col> In-file occurrences (read/write)");
    eprintln!("  perl-lsp --linked-editing <root> <file> <line> <col>   Linked-editing occurrence set");
    eprintln!("  perl-lsp --semantic-tokens <root> <file>               Semantic token classification");
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
    eprintln!("  perl-lsp --dump-package <root> <package>               Dump every sub in <package>");
    eprintln!("                                                         with derived type info");
    eprintln!("  perl-lsp --clear-cache [<root>]                        Wipe the module cache for");
    eprintln!("                                                         <root>, or every project if");
    eprintln!("                                                         <root> is omitted");
    eprintln!("  perl-lsp --parse <file|--> [<lang>]                    Print tree-sitter parse tree");
    eprintln!("                                                         (`-` reads from stdin; <lang>");
    eprintln!("                                                         picks a grammar by id, e.g. cpp)");
    eprintln!();
    eprintln!("MULTI-LANGUAGE (pack drivers, opt-in at build time):");
    eprintln!("  perl-lsp --languages                                   Languages this build serves");
    eprintln!("  perl-lsp --lang-analyze <file>...                      Analyze file(s) via their driver (shared caches)");
    eprintln!("                                                         (route by extension; dump outline)");
    eprintln!("    Build a cpp-lsp:  cargo build --features cpp   (or --features all-langs)");
    eprintln!();
    eprintln!("  perl-lsp --version                                     Print version");
}

// ---- Helpers ----

fn parse_file(path: &str) -> (String, tree_sitter::Tree, file_analysis::FileAnalysis) {
    let source = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Cannot read {}: {}", path, e);
        std::process::exit(1);
    });
    // Route a pack language (cpp, ...) through its driver so the CLI
    // capabilities (--outline, --hover, --batch/gold) match the LSP
    // server. Perl + truly-unrecognized files keep the existing path; an
    // extension no driver claims falls back to a content sniff
    // (`commands.def` is C, not Perl, despite its unowned extension).
    let reg = language_driver::LanguageRegistry::with_enabled();
    if let Some(driver) = reg
        .for_path_sniffed(std::path::Path::new(path), &source)
        .filter(|d| d.id() != "perl")
    {
        let mut parser = driver.make_parser();
        let tree = parser.parse(&source, None).unwrap_or_else(|| {
            eprintln!("Parse failed: {}", path);
            std::process::exit(1);
        });
        let analysis = driver.analyze_with_path(&source, Some(std::path::Path::new(path)));
        return (source, tree, analysis);
    }
    let mut parser = module_resolver::create_parser();
    let tree = parser.parse(&source, None).unwrap_or_else(|| {
        eprintln!("Parse failed: {}", path);
        std::process::exit(1);
    });
    let analysis = builder::build(&tree, source.as_bytes());
    (source, tree, analysis)
}

fn parse_point(line_str: &str, col_str: &str) -> tree_sitter::Point {
    let line: usize = line_str.parse().unwrap_or_else(|_| {
        eprintln!("line must be a number");
        std::process::exit(1);
    });
    let col: usize = col_str.parse().unwrap_or_else(|_| {
        eprintln!("col must be a number");
        std::process::exit(1);
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

/// Full CLI workspace setup: index the workspace, open the SQLite cache,
/// warm cached modules, resolve missing imports + ancestors via @INC,
/// save fresh entries back to disk. Mirrors the LSP server's startup
/// minus the resolver thread (one-shot, synchronous). Used by every
/// CLI command that needs cross-file resolution to behave like the
/// running server.
fn cli_full_startup(root: &str) -> (file_store::FileStore, module_index::ModuleIndex) {
    // `--check --timings` already flipped this on; the env var lets any
    // startup-driven CLI mode (dump-package, etc.) opt in too.
    timings::enable_from_env();
    let (root_path, root_uri) = canonical_root_and_uri(root);
    // Pin repo-local `.perl-lsp/` plugin discovery to the same root the
    // cache keys on, before the first build() (workspace indexing) fires.
    plugin::rhai_host::set_workspace_root(Some(&root_uri));

    // Register workspace files INTO the module index (not just a FileStore)
    // so their plugin bridges + package names participate in cross-file
    // lookups — the whole point of "act like the server just started".
    // Indexing without the index (bridge-less) is what forced callers to
    // hand-roll their own `index_workspace_with_index`, and they drifted.
    let module_index = module_index::ModuleIndex::new_for_cli();
    module_index.mark_long_lived_from_env();
    // Wake the headless resolver: it blocks on this channel for the
    // @INC scan + SQLite warm.
    module_index.set_workspace_root(Some(root_uri.as_str()));
    let ws = file_store::FileStore::new();
    let indexed =
        module_resolver::index_workspace_with_index(&root_path, &ws, Some(&module_index), None);
    // Label the tier: a pack-only workspace printing a bare "Indexed 0 files"
    // reads as "indexing failed" when the pack line below says otherwise.
    if indexed > 0 {
        eprintln!("Indexed {} Perl files", indexed);
    }
    // Pack languages (C++/Python/…) → per-language sub-indexes (separate
    // caches, no cross-language overlap), attached to the hub for routing.
    let pack_indexed = module_resolver::index_pack_languages(
        &root_path,
        Some(&root_uri),
        &module_index,
        None,
        crate::backend::max_cache_mb_default() as usize * 1024 * 1024,
    );
    if pack_indexed > 0 {
        // Name the languages actually served rather than the
        // generic "pack-language" — a pure-C++ workspace should read "C/C++",
        // not a term that only makes sense from inside this codebase.
        let reg = language_driver::LanguageRegistry::with_enabled();
        let langs: Vec<&'static str> = reg
            .languages()
            .into_iter()
            .filter(|id| *id != "perl")
            .map(language_driver::LanguageRegistry::display_name)
            .collect();
        eprintln!("Indexed {} {} files", pack_indexed, langs.join("/"));
    }

    let mut inc_paths = module_resolver::discover_inc_paths();
    module_resolver::add_project_lib_paths(&mut inc_paths, &root_path);

    let db = module_cache::open_cache_db(Some(&root_uri), "perl");
    let mut stale_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(ref conn) = db {
        let _ = module_cache::validate_inc_paths(conn, &inc_paths);
        let _ = module_cache::validate_plugin_fingerprint(
            conn,
            &plugin::rhai_host::plugin_fingerprint(),
        );
        let (warmed, stale) = module_cache::warm_cache(
            conn,
            &module_index.cache_raw(),
            module_index.is_long_lived() && module_resolver::eviction_enabled(),
        );
        // Stamp generations for the warm-loaded @INC providers (the warm
        // scan bypasses the registration front doors) so `enrichment_key`
        // reads a real token for every provider.
        module_index.stamp_import_generations();
        if warmed > 0 {
            eprintln!("Cache: {} modules loaded from disk", warmed);
        }
        stale_set = stale.into_iter().collect();
    }

    let mut needed = std::collections::HashSet::new();
    for entry in ws.workspace_raw().iter() {
        for imp in &entry.value().imports {
            needed.insert(imp.module_name.clone());
        }
        for parents in entry.value().package_parents.values() {
            for p in parents {
                needed.insert(p.clone());
            }
        }
    }

    let mut parser = module_resolver::create_parser();
    let mut parse_memo: module_resolver::ParseMemo = std::collections::HashMap::new();
    let mut resolved = 0usize;
    let mut already_cached = 0usize;
    // Worklist, not a fixed set: re-exporting modules (Test::Most → Test::More)
    // pull their re-exported producers' surfaces transitively, so those
    // producers must be resolved too even though no workspace file `use`s them
    // directly. Enqueue each resolved module's `reexport_modules`. Bounded by
    // the seen-set (`queued`); the cross-file surface walk handles cycles.
    let mut queue: std::collections::VecDeque<String> = needed.iter().cloned().collect();
    let mut queued: std::collections::HashSet<String> = needed.clone();
    while let Some(name) = queue.pop_front() {
        let cached_arc = if module_index.cache_raw().contains_key(name.as_str())
            && !stale_set.contains(&name)
        {
            already_cached += 1;
            timings::record_cached(&name);
            module_index.get_cached(&name)
        } else {
            if stale_set.contains(&name) {
                parse_memo.remove(&name);
            }
            module_resolver::resolve_and_parse_with_memo(&inc_paths, &name, &mut parser, &mut parse_memo)
                .map(|cached| {
                    if let Some(ref conn) = db {
                        module_cache::save_to_db(conn, &name, &Some(std::sync::Arc::clone(&cached)), "cli");
                    }
                    module_index.insert_cache(&name, Some(std::sync::Arc::clone(&cached)));
                    resolved += 1;
                    cached
                })
        };
        if let Some(cached) = cached_arc {
            for re in &cached.analysis.reexport_modules {
                if queued.insert(re.clone()) {
                    queue.push_back(re.clone());
                }
            }
        }
    }
    eprintln!("Modules: {} cached, {} resolved, {} total", already_cached, resolved, queued.len());

    // `warm_cache` populated `cache_raw()` directly, bypassing the reverse
    // index, and `insert_cache` only indexes export/export_ok. Rebuild the
    // full `func → modules` index from the cache so `find_exporters` answers
    // identically on cold and warm runs (B6 export-attribution regression).
    module_index.rebuild_reverse_index_from_cache();

    // Ancestry is now fully populated: materialize deferred cross-file
    // `ClassIsa` plugin emissions (DBIC column/relationship accessors reached
    // through a cross-file base) into the whole resident cached copies, so
    // cross-file goto-def / references see them via `whole_present`. See
    // `ModuleIndex::materialize_gated_emissions` / `GatedEmission`.
    module_index.materialize_gated_emissions();

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

/// Which coordinate dialect the CLI renders location output in. Threaded from
/// input parsing into every location a query emits so **output speaks the same
/// dialect as the input** — the fix for the 0-based-vs-1-based foot-gun. The
/// tool's own `path:line:col` output then round-trips straight back into the
/// next query's `--at`.
///
/// - `ZeroBasedByte` — tree-sitter native: 0-based line, byte column. The
///   dialect of the positional `<file> <line> <col>` form (and the batch/gold
///   protocol's JSONL input).
/// - `EditorOneBasedChar` — editor convention: 1-based line, character column.
///   The dialect of the `--at file:line:col` form, and what the `--batch`
///   path renders (gold fixtures encode these values — do NOT change it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CoordFmt {
    ZeroBasedByte,
    EditorOneBasedChar,
}

impl CoordFmt {
    /// Render a tree-sitter `(row, byte_col)` in this dialect. `line_src` is the
    /// full text of `row` (when available) — needed only to convert byte→char
    /// for the editor dialect; rows past EOF fall back to the byte column.
    fn render(self, row: usize, byte_col: usize, line_src: Option<&str>) -> (usize, usize) {
        match self {
            CoordFmt::ZeroBasedByte => (row, byte_col),
            CoordFmt::EditorOneBasedChar => {
                let char_col = line_src
                    .map(|line| line.get(..byte_col.min(line.len())).unwrap_or(line).chars().count())
                    .unwrap_or(byte_col);
                (row + 1, char_col + 1)
            }
        }
    }

    /// Render an LSP `Position` (already 0-based **character**-counted) in this
    /// dialect — no byte→char step. Used for the handful of sites that hand back
    /// an lsp `Location`/`Range` (cpp `#include` goto-def) instead of a raw span.
    fn render_pos(self, row0: usize, char0: usize) -> (usize, usize) {
        match self {
            CoordFmt::ZeroBasedByte => (row0, char0),
            CoordFmt::EditorOneBasedChar => (row0 + 1, char0 + 1),
        }
    }
}

/// Encode one rename edit's span as JSON in the caller's coordinate dialect.
/// `sources` supplies per-file text for the byte→char step (editor dialect).
fn span_to_json(
    sources: &mut SourceCache,
    path: &str,
    span: file_analysis::Span,
    text: String,
) -> serde_json::Value {
    let (line, col) = sources.display(path, span.start.row, span.start.column);
    let (end_line, end_col) = sources.display(path, span.end.row, span.end.column);
    serde_json::json!({
        "line": line, "col": col,
        "end_line": end_line, "end_col": end_col,
        "new_text": text
    })
}

/// Per-file source cache for coordinate rendering — references can fan out
/// across many files; read each at most once. Carries the `CoordFmt` so every
/// `display` call renders in one dialect. Misses (unreadable file) degrade to
/// the raw byte column via `CoordFmt::render`'s fallback.
struct SourceCache {
    fmt: CoordFmt,
    files: std::collections::HashMap<String, Option<String>>,
}

impl SourceCache {
    fn new(fmt: CoordFmt) -> Self {
        SourceCache { fmt, files: std::collections::HashMap::new() }
    }

    fn display(&mut self, path: &str, row: usize, byte_col: usize) -> (usize, usize) {
        let src = self
            .files
            .entry(path.to_string())
            .or_insert_with(|| std::fs::read_to_string(path).ok());
        let line_src = src.as_deref().and_then(|s| s.lines().nth(row));
        self.fmt.render(row, byte_col, line_src)
    }
}

// ---- Cursor-input parsing (positional vs `--at`) ----

/// A parsed cursor target for a single-mode CLI query: the file, the internal
/// tree-sitter point (always 0-based / byte column), the `CoordFmt` matching
/// the input dialect (so output round-trips), and the raw spelling the user
/// typed (for the `[pos]` self-documenting annotation).
struct CursorTarget {
    file: String,
    point: tree_sitter::Point,
    fmt: CoordFmt,
    raw: String,
}

/// Split a `--at` spec `file:line:col` into its parts. The two rightmost
/// `:`-fields are line and col; everything before is the file (paths rarely
/// contain `:`, and taking the last two fields tolerates the ones that do,
/// e.g. a Windows drive prefix). Returns `(file, line_1based, col_1based)`.
fn split_at_spec(spec: &str) -> Option<(String, usize, usize)> {
    let mut it = spec.rsplitn(3, ':');
    let col: usize = it.next()?.parse().ok()?;
    let line: usize = it.next()?.parse().ok()?;
    let file = it.next()?.to_string();
    if file.is_empty() {
        return None;
    }
    Some((file, line, col))
}

/// Convert an editor `(line_1based, char_col_1based)` to an internal 0-based
/// tree-sitter point with a **byte** column — the exact inverse of
/// `CoordFmt::EditorOneBasedChar` rendering. `source` is the target file's text
/// (for the char→byte step); without it, the char column is used as the byte
/// column (best effort, correct for ASCII).
fn editor_to_internal_point(source: Option<&str>, line1: usize, col1: usize) -> tree_sitter::Point {
    let row = line1.saturating_sub(1);
    let char_col = col1.saturating_sub(1);
    let byte_col = source
        .and_then(|s| s.lines().nth(row))
        .map(|line| {
            line.char_indices()
                .nth(char_col)
                .map(|(b, _)| b)
                .unwrap_or(line.len())
        })
        .unwrap_or(char_col);
    tree_sitter::Point::new(row, byte_col)
}

/// Resolve a cursor-verb file argument to a path that exists on disk.
/// Tries the argument as-is first (CWD-relative or absolute), then falls
/// back to `<root>`-relative — so a root-relative path works when invoked
/// from outside the project root. When neither exists, the original is
/// returned unchanged (downstream reports the honest "file not found").
fn resolve_cursor_file(file: &str, root: &str) -> String {
    if std::path::Path::new(file).exists() {
        return file.to_string();
    }
    let joined = std::path::Path::new(root).join(file);
    if joined.exists() {
        return joined.to_string_lossy().into_owned();
    }
    file.to_string()
}

/// Parse the cursor arguments that follow `<root>` for a single-mode query.
/// Two forms, disambiguated by the leading `--at`:
///   positional:  `<file> <line> <col>`      → 0-based, byte column (engine)
///   editor:      `--at <file>:<line>:<col>` → 1-based, char column (editor)
/// The chosen `CoordFmt` rides along so the query's output renders in the same
/// dialect the input used. The file argument is resolved CWD-first then
/// `<root>`-relative via `resolve_cursor_file`.
fn parse_cursor_target(rest: &[String], root: &str) -> Option<CursorTarget> {
    match rest {
        [flag, spec] if flag == "--at" => {
            let (file, line1, col1) = split_at_spec(spec)?;
            let file = resolve_cursor_file(&file, root);
            let source = std::fs::read_to_string(&file).ok();
            let point = editor_to_internal_point(source.as_deref(), line1, col1);
            Some(CursorTarget { file, point, fmt: CoordFmt::EditorOneBasedChar, raw: spec.clone() })
        }
        [file, line, col] => {
            let row: usize = line.parse().ok()?;
            let column: usize = col.parse().ok()?;
            Some(CursorTarget {
                file: resolve_cursor_file(file, root),
                point: tree_sitter::Point::new(row, column),
                fmt: CoordFmt::ZeroBasedByte,
                raw: format!("{} {} {}", file, line, col),
            })
        }
        _ => None,
    }
}

/// The maximal identifier token containing (or ending just before) `byte_col`
/// on `line`. Word = alphanumeric or `_`; a cursor sitting one past a token's
/// end (a common editor placement) still reports that token. `None` means the
/// cursor landed on whitespace / punctuation — the loud-hint case.
fn token_at_byte(line: &str, byte_col: usize) -> Option<String> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let word_span = |anchor: usize| -> Option<(usize, usize)> {
        let idx = chars
            .iter()
            .position(|&(b, c)| b <= anchor && anchor < b + c.len_utf8())?;
        if !is_word(chars[idx].1) {
            return None;
        }
        let mut lo = idx;
        while lo > 0 && is_word(chars[lo - 1].1) {
            lo -= 1;
        }
        let mut hi = idx;
        while hi + 1 < chars.len() && is_word(chars[hi + 1].1) {
            hi += 1;
        }
        Some((chars[lo].0, chars[hi].0 + chars[hi].1.len_utf8()))
    };
    let (s, e) = word_span(byte_col)
        .or_else(|| byte_col.checked_sub(1).and_then(word_span))?;
    Some(line[s..e].to_string())
}

/// Self-documenting `[pos]` annotation (stderr — stdout stays the stable
/// machine format). Prints exactly how the cursor input was interpreted, the
/// internal 0-based point, the landed token, and the source line — so the
/// 0-based/1-based trap announces itself instead of silently mislanding.
fn emit_pos_annotation(target: &CursorTarget) {
    let (label, dialect) = match target.fmt {
        CoordFmt::EditorOneBasedChar => ("EDITOR", "1-based, char col"),
        CoordFmt::ZeroBasedByte => ("POSITIONAL", "0-based, byte col"),
    };
    let row = target.point.row;
    let bc = target.point.column;
    eprintln!(
        "[pos] input {}  read as {} ({})  ->  internal {}:{}",
        target.raw, label, dialect, row, bc
    );
    // Distinguish "couldn't open the file" from "line past EOF": the old
    // code collapsed both into a "past the end" message, which lied about
    // files it never read (unresolved path, permissions).
    match std::fs::read_to_string(&target.file) {
        Err(e) => eprintln!("      (could not read {}: {})", target.file, e),
        Ok(text) => match text.lines().nth(row) {
            Some(line) => {
                match token_at_byte(line, bc) {
                    Some(tok) => eprintln!("      landed on token: {:?}", tok),
                    None => {
                        // Whitespace / no token — name the likely fix in the OTHER base.
                        let hint = match target.fmt {
                            CoordFmt::EditorOneBasedChar => format!(
                                "if these are 0-based engine coords, drop --at and pass: {} {} {}",
                                target.file, row, bc
                            ),
                            CoordFmt::ZeroBasedByte => format!(
                                "if these are 1-based editor coords, use: --at {}:{}:{}",
                                target.file, row + 1, bc + 1
                            ),
                        };
                        eprintln!("      landed on whitespace / no token — {}", hint);
                    }
                }
                eprintln!("      line {}: {}", row + 1, line);
            }
            None => eprintln!("      (line {} is past the end of {})", row + 1, target.file),
        },
    }
}

// ---- CLI Commands ----

/// --check [<root>] [--format json|human] [--severity error|warning|info|hint] — Batch diagnostics
fn cli_check(args: &[String]) {
    let root = args.iter()
        .find(|a| !a.starts_with("--") && !["json", "human", "error", "warning", "info", "hint"].contains(&a.as_str()))
        .map(|s| s.as_str())
        .unwrap_or(".");
    let json_mode = is_json_format(args);
    let min_severity = get_arg_value(args, "--severity").unwrap_or("warning");
    let min_rank = severity_rank(min_severity);
    // Opt-in QA channel, mirrors the LSP `initializationOptions` toggle.
    let options = symbols::DiagnosticOptions::from_cli_args(args);

    if args.iter().any(|a| a == "--timings") {
        timings::enable();
    }
    timings::enable_from_env();

    let (ws, module_index) = cli_full_startup(root);

    // Dump the per-module breakdown before diagnostics output so the table
    // isn't buried under (and the early `exit(1)` below doesn't swallow it).
    timings::report();
    timings::report_pattern_stats();

    let mut all_diagnostics = Vec::new();

    for (file, d) in enriched_tree_diagnostics(&ws, &module_index, options) {
        {
            let sev = match d.severity {
                Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::ERROR => "error",
                Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::WARNING => "warning",
                Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::INFORMATION => "info",
                Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::HINT => "hint",
                _ => "warning",
            };
            // Filter by minimum severity
            if severity_rank(sev) > min_rank {
                continue;
            }
            all_diagnostics.push(serde_json::json!({
                "file": file,
                "line": d.range.start.line,
                "col": d.range.start.character,
                "severity": sev,
                "code": d.code.map(|c| match c {
                    tower_lsp::lsp_types::NumberOrString::String(s) => s,
                    tower_lsp::lsp_types::NumberOrString::Number(n) => n.to_string(),
                }).unwrap_or_default(),
                "message": d.message,
            }));
        }
    }

    if json_mode {
        println!("{}", serde_json::to_string_pretty(&all_diagnostics).unwrap());
    } else {
        for d in &all_diagnostics {
            let severity = d["severity"].as_str().unwrap_or("warning");
            let file = d["file"].as_str().unwrap_or("");
            let line = d["line"].as_u64().unwrap_or(0) + 1;
            let col = d["col"].as_u64().unwrap_or(0) + 1;
            let code = d["code"].as_str().unwrap_or("");
            let msg = d["message"].as_str().unwrap_or("");
            eprintln!("{}:{}:{}: {}[{}] {}", file, line, col, severity, code, msg);
        }
        let total = all_diagnostics.len();
        let files = ws.workspace_len();
        eprintln!("{} diagnostics in {} files", total, files);
    }

    if !all_diagnostics.is_empty() {
        std::process::exit(1);
    }
}

/// --outline <file> — Document symbol outline
fn cli_outline(file: &str) {
    let (_source, _tree, analysis) = parse_file(file);
    println!("{}", outline_json(&analysis));
}

/// --hover <file> <line> <col> — single-file type info and docs (no index).
/// The cross-file form (`--hover <root> ...`) routes through `cli_cursor` so it
/// can never drift from `--batch`; this no-root form has no index, so it keeps
/// its own path.
fn cli_hover_single_file(file: &str, line_str: &str, col_str: &str) {
    let point = parse_point(line_str, col_str);
    let (source, _tree, analysis) = parse_file(file);
    // Pack languages get the language-agnostic renderer (matches the LSP); no
    // index here, so cross-file function hover is unavailable in this form (use
    // the root form for that). Perl keeps hover_info.
    let reg = language_driver::LanguageRegistry::with_enabled();
    let pack_lang = reg
        .for_path_sniffed(std::path::Path::new(file), &source)
        .map(|d| d.id())
        .filter(|id| *id != "perl");
    let markdown = match pack_lang {
        Some(lang) => {
            let files = file_store::FileStore::new();
            let cs = resolve::resolve(
                &files,
                &analysis,
                file_store::FileKey::Path(std::path::PathBuf::from(file)),
                point,
                None,
                resolve::OverrideScope::default(),
            )
            .with_source(&source)
            .pack_routed();
            symbols::pack_hover_markdown(&cs, lang)
        }
        None => analysis.hover_info(point, &source, None),
    };
    if let Some(markdown) = markdown {
        println!("{}", markdown);
    } else {
        eprintln!("No hover info at {}:{}", line_str, col_str);
        std::process::exit(1);
    }
}

/// --type-at <file> <line> <col> — Single type query
fn cli_type_at(file: &str, line_str: &str, col_str: &str) {
    let (_source, _tree, analysis) = parse_file(file);
    let point = parse_point(line_str, col_str);

    // Check refs for inferred type — route through the witness bag
    // so framework / branch / arity rules refine the answer.
    if let Some(r) = analysis.ref_at(point) {
        if let Some(ty) = analysis.inferred_type_via_bag(&r.target_name, point) {
            println!("{}", file_analysis::format_inferred_type(&ty));
            return;
        }
    }
    // Check symbols
    if let Some(sym) = analysis.symbol_at(point) {
        if let Some(ty) = analysis.inferred_type_via_bag(&sym.name, point) {
            println!("{}", file_analysis::format_inferred_type(&ty));
            return;
        }
    }
    eprintln!("No type info at {}:{}", line_str, col_str);
    std::process::exit(1);
}

/// Run `run_one` and reproduce the single-mode contract: `Ok` to stdout,
/// `Err` to stderr with exit-1 (preserves the "miss → exit 1" behavior every
/// single-mode command had). `fmt` selects the output coordinate dialect.
fn print_run_one(
    ws: &file_store::FileStore,
    idx: &module_index::ModuleIndex,
    req: &BatchReq,
    fmt: CoordFmt,
) {
    match run_one(ws, idx, req, fmt) {
        Ok(s) => println!("{}", s),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

/// Every uniform cursor query (`--definition` / `--references` /
/// `--implementations` / `--completion` / `--signature-help` /
/// `--document-highlight` / `--linked-editing` / cross-file `--hover`). `q` is
/// the `run_one` query string; `rest` is the cursor args after `<root>` (either
/// positional `<file> <line> <col>` or `--at <file>:<line>:<col>`). The input
/// dialect selects the output `CoordFmt`, and a `[pos]` annotation goes to
/// stderr so the 0-vs-1-based interpretation is never silent.
fn cli_cursor(q: &str, root: &str, rest: &[String]) {
    let target = parse_cursor_target(rest, root).unwrap_or_else(|| {
        eprintln!(
            "perl-lsp --{q}: expected `<root> <file> <line> <col>` or `<root> --at <file>:<line>:<col>`"
        );
        std::process::exit(2);
    });
    emit_pos_annotation(&target);
    let (ws, idx) = cli_full_startup(root);
    let req = BatchReq {
        id: String::new(),
        q: q.to_string(),
        file: target.file.clone(),
        line: target.point.row,
        col: target.point.column,
        query: None,
        newname: None,
    };
    print_run_one(&ws, &idx, &req, target.fmt);
}

/// --semantic-tokens <root> <file> — token classification for the file.
fn cli_semantic_tokens(root: &str, file: &str) {
    let (ws, idx) = cli_full_startup(root);
    let req = BatchReq {
        id: String::new(),
        q: "semantic-tokens".into(),
        file: file.to_string(),
        line: 0,
        col: 0,
        query: None,
        newname: None,
    };
    // semantic-tokens output is independent of the coordinate seam; the batch
    // dialect keeps it unchanged.
    print_run_one(&ws, &idx, &req, CoordFmt::EditorOneBasedChar);
}

/// Build an open `Document` (tree + analysis + stable_outline) and enrich it
/// against the index exactly like the server's open-file path. Shared by the
/// interactive CLI modes.
/// Time one CLI query step — `tphase!("completion_items", expr)` prints a
/// `[PHASE]` line when `PERL_LSP_PHASE_TIMING` is set. Sugar over `timings::phase`.
macro_rules! tphase {
    ($label:literal, $body:expr) => {
        $crate::timings::phase($label, || $body)
    };
}

fn cli_open_document(file: &str, idx: &module_index::ModuleIndex) -> document::Document {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| {
        eprintln!("Cannot read {}: {}", file, e);
        std::process::exit(1);
    });
    // Route a pack language (cpp, ...) through its driver so the CLI
    // cursor handlers (definition/references/highlight/…) match the LSP
    // server. Perl + truly-unrecognized files keep Document::new + enrichment.
    let reg = language_driver::LanguageRegistry::with_enabled();
    let pack = reg
        .for_path_sniffed(std::path::Path::new(file), &text)
        .filter(|d| d.id() != "perl");
    if let Some(driver) = pack {
        return tphase!("Document::new_routed", document::Document::new_routed(text, driver, Some(std::path::PathBuf::from(file))).unwrap_or_else(|| {
            eprintln!("Parse failed: {}", file);
            std::process::exit(1);
        }));
    }
    let mut doc = tphase!("Document::new (parse+build)", document::Document::new(text).unwrap_or_else(|| {
        eprintln!("Parse failed: {}", file);
        std::process::exit(1);
    }));
    tphase!("enrich_imported_types", std::sync::Arc::make_mut(&mut doc.analysis).enrich_imported_types_with_keys(Some(idx)));
    doc
}

#[derive(serde::Deserialize)]
struct BatchReq {
    id: String,
    q: String,
    #[serde(default)] file: String,
    #[serde(default)] line: usize,
    #[serde(default)] col: usize,
    #[serde(default)] query: Option<String>,
    #[serde(default)] newname: Option<String>,
}

/// Single source of truth for every cursor/query CLI capability. Both the
/// single-mode `cli_*` wrappers and `--batch` call this, so their stdout can
/// never drift. The expensive startup state (`ws` + `idx`) is built once by the
/// caller and shared; this re-parses only the one target file per call. Returns
/// Stage an analysis in the workspace store for the lifetime of one cross-file
/// query (references/rename need `refs_to` to see the freshly-enriched target),
/// restoring the prior entry on drop. `--batch` shares one FileStore across all
/// requests; without this restore a references/rename request would leave its
/// enrichment-synthesized symbols in the store, so a later workspace-symbol or
/// diagnostics request in the same batch would see different results depending
/// on ordering.
struct ScopedWorkspaceEntry<'a> {
    ws: &'a file_store::FileStore,
    path: std::path::PathBuf,
    prior: Option<std::sync::Arc<file_analysis::FileAnalysis>>,
}

impl<'a> ScopedWorkspaceEntry<'a> {
    fn insert(
        ws: &'a file_store::FileStore,
        path: std::path::PathBuf,
        analysis: file_analysis::FileAnalysis,
    ) -> Self {
        let prior = ws.workspace_raw().get(&path).map(|r| r.value().clone());
        ws.insert_workspace(path.clone(), analysis);
        Self { ws, path, prior }
    }
}

impl Drop for ScopedWorkspaceEntry<'_> {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(arc) => self.ws.insert_workspace_arc(self.path.clone(), arc),
            None => self.ws.remove_workspace(&self.path),
        }
    }
}

/// Resolve the query file's direct imports before answering. The LSP
/// backend does this on didOpen (request_resolve per import); a CLI
/// query is one-shot, so it requests AND waits, bounded per module.
/// Without it, modules nothing in the workspace literally `use`s —
/// framework-implied SyntheticUses like Mojolicious::Plugin::
/// DefaultHelpers — resolve in the editor but stay invisible to CLI
/// probes and the gold harness.
fn resolve_imports_blocking(
    idx: &module_index::ModuleIndex,
    analysis: &file_analysis::FileAnalysis,
) {
    for imp in &analysis.imports {
        idx.request_resolve(&imp.module_name);
    }
    // Global budget, not per-import: a cold cache resolving a
    // framework's whole dependency tree must not stall the CLI for
    // minutes. Each resolved module persists to the SQLite cache, so
    // repeated runs converge to fully warm; within one run we answer
    // with whatever resolved in time (the editor is eventually-
    // consistent here too — it refreshes when resolution lands).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for imp in &analysis.imports {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let _ = idx.wait_resolved(&imp.module_name, left);
    }
}

fn run_one(
    ws: &file_store::FileStore,
    idx: &module_index::ModuleIndex,
    req: &BatchReq,
    fmt: CoordFmt,
) -> Result<String, String> {
    use tower_lsp::lsp_types::Position;
    let file = req.file.as_str();
    let point = tree_sitter::Point { row: req.line, column: req.col };
    let pos = Position { line: req.line as u32, character: req.col as u32 };

    // Graceful miss instead of process exit — a bad path must not kill --batch.
    let needs_file = !matches!(req.q.as_str(), "workspace-symbol" | "diagnostics");
    if needs_file && (file.is_empty() || !std::path::Path::new(file).exists()) {
        return Err(format!("file not found: {}", file));
    }

    match req.q.as_str() {
        "definition" => {
            let (source, _tree, mut analysis) = parse_file(file);
            resolve_imports_blocking(idx, &analysis);
            analysis.enrich_imported_types_with_keys(Some(idx));
            let abs = std::fs::canonicalize(file).unwrap_or_else(|_| std::path::PathBuf::from(file));
            let uri = tower_lsp::lsp_types::Url::from_file_path(&abs)
                .unwrap_or_else(|_| tower_lsp::lsp_types::Url::parse("file:///unknown").unwrap());
            // Pack languages resolve cross-file through their sub-index (matches
            // the LSP server); Perl uses the hub. The CLI mirror MUST route here
            // or cross-file macro/function goto-def silently misses.
            let reg = language_driver::LanguageRegistry::with_enabled();
            let lang_id = reg.for_path_sniffed(std::path::Path::new(file), &source)
                .map(|d| d.id()).filter(|id| *id != "perl");
            let pack = lang_id.and_then(|lang| idx.pack_index(lang));
            let base_idx: &dyn crate::file_analysis::CrossFileLookup =
                pack.as_deref().map_or(idx as &dyn crate::file_analysis::CrossFileLookup, |i| i);
            // `#include "x.h"` path → the resolved header (`#include` = `use`).
            // A path token, not a name — slot-shaped, stays ahead of the set.
            if lang_id == Some("cpp") {
                if let Some(loc) = symbols::pack_include_definition(&analysis, point, Some(abs.as_path())) {
                    let path = loc.uri.to_file_path().map(|p| p.display().to_string())
                        .unwrap_or_else(|_| loc.uri.to_string());
                    let (line, col) = fmt.render_pos(
                        loc.range.start.line as usize, loc.range.start.character as usize);
                    return Ok(format!("{}:{}:{}", path, line, col));
                }
            }
            let _ = &uri;
            // Forward projection of the set (mirrors the LSP handler): the
            // source text unlocks the macro variant lane for pack routing.
            // Print EVERY offered location (one per line), ranked as
            // returned — macro variants config-active first (labels shown),
            // a domain-typed field decl FIRST then its domain enum def.
            let _staged = ScopedWorkspaceEntry::insert(ws, abs.clone(), analysis);
            let origin = ws.workspace_raw().get(&abs).map(|r| r.value().clone())
                .expect("origin staged above");
            let mut cs = resolve::resolve(
                ws, &origin, file_store::FileKey::Path(abs), point,
                Some(base_idx), resolve::OverrideScope::default(),
            )
            .with_source(&source);
            if pack.is_some() {
                cs = cs.pack_routed();
            }
            let locs = cs.definitions();
            if !locs.is_empty() {
                let mut sources = SourceCache::new(fmt);
                let mut lines = Vec::new();
                for loc in locs {
                    let path = match &loc.key {
                        file_store::FileKey::Path(p) => p.display().to_string(),
                        file_store::FileKey::Url(u) => u.to_file_path()
                            .map(|p| p.display().to_string()).unwrap_or_else(|_| u.to_string()),
                    };
                    let (line, col) = sources.display(
                        &path, loc.span.start.row, loc.span.start.column);
                    let label = loc.label.map(|l| format!("  ({l})")).unwrap_or_default();
                    lines.push(format!("{}:{}:{}{}", path, line, col, label));
                }
                return Ok(lines.join("\n"));
            }
            Err(format!("No definition found at {}:{}", req.line, req.col))
        }
        "references" => {
            let (s, _t, mut analysis) = parse_file(file);
            resolve_imports_blocking(idx, &analysis);
            analysis.enrich_imported_types_with_keys(Some(idx));
            let file_path = std::path::Path::new(file).canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(file));
            let mut sources = SourceCache::new(fmt);
            let mut results = Vec::new();
            // Pack languages route through their sub-index (matches goto-def
            // and the LSP server) — the hub only knows Perl modules, so
            // resolving/collecting against it silently misses every
            // cross-file cpp use. Perl keeps the hub (empty closure = no-op).
            let reg = language_driver::LanguageRegistry::with_enabled();
            let lang_id = reg.for_path_sniffed(std::path::Path::new(file), &s)
                .map(|d| d.id()).filter(|id| *id != "perl");
            let pack = lang_id.and_then(|lang| idx.pack_index(lang));
            let base_idx: &dyn crate::file_analysis::CrossFileLookup =
                pack.as_deref().map_or(idx as &dyn crate::file_analysis::CrossFileLookup, |i| i);
            // `#include` reverse — "who includes this header" — owns the path
            // token exclusively (its backward mirror of include goto-def).
            if lang_id == Some("cpp") {
                if let Some(incs) = symbols::pack_include_references(
                    &analysis, point, Some(file_path.as_path()), base_idx)
                {
                    for (path, span) in incs {
                        let ps = path.display().to_string();
                        let (line, col) = sources.display(&ps, span.start.row, span.start.column);
                        results.push(serde_json::json!({"file": ps, "line": line, "col": col}));
                    }
                    return Ok(serde_json::to_string_pretty(&results).unwrap());
                }
            }
            // Stage the enriched origin, then construct the set from the
            // staged snapshot — the same one-construction/one-projection
            // shape as the LSP handler, so CLI and editor answers can't
            // diverge. Pack routing + the origin's closure scope are
            // construction facts on the set.
            let _staged = ScopedWorkspaceEntry::insert(ws, file_path.clone(), analysis);
            let origin = ws.workspace_raw().get(&file_path).map(|r| r.value().clone())
                .expect("origin staged above");
            let mut cs = resolve::resolve(
                ws, &origin, file_store::FileKey::Path(file_path), point,
                Some(base_idx), override_scope_from_env(),
            );
            if pack.is_some() {
                cs = cs.pack_routed();
            }
            for loc in cs.references() {
                let path = match &loc.key {
                    file_store::FileKey::Path(p) => p.display().to_string(),
                    file_store::FileKey::Url(u) => u.to_file_path()
                        .map(|p| p.display().to_string()).unwrap_or_else(|_| u.to_string()),
                };
                let (line, col) = sources.display(&path, loc.span.start.row, loc.span.start.column);
                results.push(serde_json::json!({"file": path, "line": line, "col": col}));
            }
            Ok(serde_json::to_string_pretty(&results).unwrap())
        }
        "implementations" => {
            let (s, _t, mut analysis) = parse_file(file);
            resolve_imports_blocking(idx, &analysis);
            analysis.enrich_imported_types_with_keys(Some(idx));
            let file_path = std::path::Path::new(file).canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(file));
            let mut sources = SourceCache::new(fmt);
            let mut results = Vec::new();
            // Same pack routing as the LSP handler, declared at construction,
            // so the CLI mirror can't diverge: the domain bridge (enum def →
            // field-slot sites) and the family/spec walks are one projection.
            let reg = language_driver::LanguageRegistry::with_enabled();
            let pack = reg
                .for_path_sniffed(std::path::Path::new(file), &s)
                .map(|d| d.id())
                .filter(|id| *id != "perl")
                .and_then(|lang| idx.pack_index(lang));
            let base_idx: &dyn file_analysis::CrossFileLookup = match pack.as_deref() {
                Some(i) => i,
                None => idx,
            };
            let _staged = ScopedWorkspaceEntry::insert(ws, file_path.clone(), analysis);
            let origin = ws.workspace_raw().get(&file_path).map(|r| r.value().clone())
                .expect("origin staged above");
            let mut cs = resolve::resolve(
                ws, &origin, file_store::FileKey::Path(file_path), point,
                Some(base_idx), resolve::OverrideScope::default(),
            );
            if pack.is_some() {
                cs = cs.pack_routed();
            }
            for loc in cs.implementations() {
                let path = match &loc.key {
                    file_store::FileKey::Path(p) => p.display().to_string(),
                    file_store::FileKey::Url(u) => u.to_file_path()
                        .map(|p| p.display().to_string()).unwrap_or_else(|_| u.to_string()),
                };
                let (line, col) = sources.display(&path, loc.span.start.row, loc.span.start.column);
                results.push(serde_json::json!({"file": path, "line": line, "col": col}));
            }
            Ok(serde_json::to_string_pretty(&results).unwrap())
        }
        "hover" => {
            let (source, _t, mut analysis) = parse_file(file);
            // Pack languages present the CandidateSet's hover projection
            // (matches the LSP server — the same construction goto-def uses);
            // Perl keeps its rich renderer.
            let reg = language_driver::LanguageRegistry::with_enabled();
            if let Some(lang) = reg.for_path_sniffed(std::path::Path::new(file), &source)
                .map(|d| d.id()).filter(|id| *id != "perl")
            {
                let pack = idx.pack_index(lang);
                let base_idx: &dyn crate::file_analysis::CrossFileLookup =
                    pack.as_deref().map_or(idx, |i| i);
                let abs = std::fs::canonicalize(file)
                    .unwrap_or_else(|_| std::path::PathBuf::from(file));
                let _staged = ScopedWorkspaceEntry::insert(ws, abs.clone(), analysis);
                let origin = ws.workspace_raw().get(&abs).map(|r| r.value().clone())
                    .expect("origin staged above");
                let mut cs = resolve::resolve(
                    ws, &origin, file_store::FileKey::Path(abs), point,
                    Some(base_idx), resolve::OverrideScope::default(),
                )
                .with_source(&source);
                if pack.is_some() {
                    cs = cs.pack_routed();
                }
                return symbols::pack_hover_markdown(&cs, lang)
                    .ok_or_else(|| format!("No hover info at {}:{}", req.line, req.col));
            }
            resolve_imports_blocking(idx, &analysis);
            analysis.enrich_imported_types_with_keys(Some(idx));
            analysis.hover_info(point, &source, Some(idx))
                .ok_or_else(|| format!("No hover info at {}:{}", req.line, req.col))
        }
        "type-at" => {
            let (_s, _t, analysis) = parse_file(file);
            resolve_imports_blocking(idx, &analysis);
            if let Some(r) = analysis.ref_at(point) {
                if let Some(ty) = analysis.inferred_type_via_bag(&r.target_name, point) {
                    return Ok(file_analysis::format_inferred_type(&ty));
                }
            }
            if let Some(sym) = analysis.symbol_at(point) {
                if let Some(ty) = analysis.inferred_type_via_bag(&sym.name, point) {
                    return Ok(file_analysis::format_inferred_type(&ty));
                }
            }
            Err(format!("No type info at {}:{}", req.line, req.col))
        }
        "completion" => {
            let doc = cli_open_document(file, idx);
            // Pack languages: member (sentinel) → in-scope, via the same
            // path the LSP server uses; Perl keeps cursor-context.
            let items = if doc.language != "perl" {
                tphase!("completion_items", backend::pack_completion(
                    ws, &doc.analysis, &doc.text, &doc.tree, point, doc.language,
                    doc.path.as_deref(), idx).0)
            } else {
                let file_path = std::path::Path::new(file).canonicalize()
                    .unwrap_or_else(|_| std::path::PathBuf::from(file));
                tphase!("completion_items", symbols::completion_items(
                    ws, &file_store::FileKey::Path(file_path),
                    &doc.analysis, &doc.tree, &doc.text, pos, idx,
                    Some(doc.stable_outline.package_lines())))
            };
            let mut out = String::new();
            for it in &items {
                match &it.detail {
                    Some(d) if !d.is_empty() => out.push_str(&format!("{}\t{}\n", it.label, d)),
                    _ => out.push_str(&format!("{}\n", it.label)),
                }
            }
            Ok(out.trim_end_matches('\n').to_string())
        }
        "signature-help" => {
            let doc = cli_open_document(file, idx);
            match symbols::signature_help(&doc.analysis, &doc.tree, &doc.text, pos, idx) {
                Some(sh) => {
                    let active = sh.active_signature.unwrap_or(0) as usize;
                    let mut out = String::new();
                    for (i, sig) in sh.signatures.iter().enumerate() {
                        let marker = if i == active { "* " } else { "  " };
                        out.push_str(&format!("{}{}\n", marker, sig.label));
                        if i == active {
                            if let Some(p) = sh.active_parameter {
                                let plabel = sig.parameters.as_ref()
                                    .and_then(|ps| ps.get(p as usize))
                                    .map(|pi| match &pi.label {
                                        tower_lsp::lsp_types::ParameterLabel::Simple(s) => s.clone(),
                                        tower_lsp::lsp_types::ParameterLabel::LabelOffsets([a, b]) =>
                                            sig.label.get(*a as usize..*b as usize).unwrap_or("").to_string(),
                                    })
                                    .unwrap_or_default();
                                out.push_str(&format!("    active param: {} ({})\n", p, plabel));
                            }
                        }
                    }
                    Ok(out.trim_end_matches('\n').to_string())
                }
                None => Err(format!("No signature help at {}:{}", req.line, req.col)),
            }
        }
        "document-highlight" => {
            let doc = cli_open_document(file, idx);
            let highlights = symbols::document_highlights(&doc.analysis, pos, Some(idx));
            let mut sources = SourceCache::new(fmt);
            let path = std::fs::canonicalize(file).map(|p| p.display().to_string())
                .unwrap_or_else(|_| file.to_string());
            let mut out = String::new();
            for h in &highlights {
                let (line, col) = sources.display(&path, h.range.start.line as usize, h.range.start.character as usize);
                let kind = match h.kind {
                    Some(tower_lsp::lsp_types::DocumentHighlightKind::WRITE) => "WRITE",
                    _ => "READ",
                };
                out.push_str(&format!("{}:{}:{}\t{}\n", path, line, col, kind));
            }
            Ok(out.trim_end_matches('\n').to_string())
        }
        "linked-editing" => {
            let doc = cli_open_document(file, idx);
            let path = std::fs::canonicalize(file).map(|p| p.display().to_string())
                .unwrap_or_else(|_| file.to_string());
            match symbols::linked_editing_ranges(&doc.analysis, pos, Some(idx)) {
                Some(ranges) => {
                    let mut sources = SourceCache::new(fmt);
                    let mut out = String::new();
                    for r in &ranges {
                        let (line, col) = sources.display(&path, r.start.line as usize, r.start.character as usize);
                        out.push_str(&format!("{}:{}:{}\n", path, line, col));
                    }
                    Ok(out.trim_end_matches('\n').to_string())
                }
                None => Err(format!("No linked-editing ranges at {}:{} (need >= 2 occurrences)", req.line, req.col)),
            }
        }
        "semantic-tokens" => {
            let doc = cli_open_document(file, idx);
            let tokens = symbols::semantic_tokens(&doc.analysis);
            let legend = symbols::semantic_token_types();
            let (mut line, mut col): (u32, u32) = (0, 0);
            let mut out = String::new();
            for t in &tokens {
                line += t.delta_line;
                if t.delta_line == 0 { col += t.delta_start; } else { col = t.delta_start; }
                let name = legend.get(t.token_type as usize).map(|tt| tt.as_str()).unwrap_or("?");
                out.push_str(&format!("{}:{} len={} {}\n", line + 1, col, t.length, name));
            }
            Ok(out.trim_end_matches('\n').to_string())
        }
        "outline" => {
            let (_s, _t, analysis) = parse_file(file);
            resolve_imports_blocking(idx, &analysis);
            Ok(outline_json(&analysis))
        }
        "workspace-symbol" => {
            let q = req.query.clone().unwrap_or_default().to_lowercase();
            let mut results = Vec::new();
            // Same identity-tuple dedup as the LSP handler's
            // `dedup_workspace_symbols`: twin accessor synthesis (getter +
            // fluent-writer) mints two symbols at one span, and a path can be
            // seen by both the resident sweep and the rows pass.
            let mut seen: std::collections::HashSet<(String, String, String, usize, usize)> =
                std::collections::HashSet::new();
            let mut push = |name: &str, kind: &file_analysis::SymKind, file: String, span: file_analysis::Span| {
                if name.to_lowercase().contains(&q) {
                    let key = (
                        name.to_string(), format!("{:?}", kind), file.clone(),
                        span.start.row, span.start.column,
                    );
                    if !seen.insert(key) {
                        return;
                    }
                    results.push(serde_json::json!({
                        "name": name, "kind": format!("{:?}", kind),
                        "file": file,
                        "line": span.start.row, "col": span.start.column,
                    }));
                }
            };
            // Same resident + rows composition as the LSP handler: a
            // symbols-present copy answers residently; evicted copies are
            // rows-guaranteed and answer from the store.
            let mut covered: std::collections::HashSet<std::path::PathBuf> =
                std::collections::HashSet::new();
            for entry in ws.workspace_raw().iter() {
                let file = entry.key().display().to_string();
                if !entry.value().symbols_are_evicted() {
                    if let Ok(canon) = std::fs::canonicalize(entry.key()) {
                        covered.insert(canon);
                    }
                    covered.insert(entry.key().clone());
                }
                for sym in &entry.value().symbols {
                    if sym.hidden_in_outline() {
                        continue;
                    }
                    push(&sym.name, &sym.kind, file.clone(), sym.selection_span);
                }
            }
            // Pack-language (C/C++/…) symbols live in per-language sub-indexes,
            // not the Perl workspace map — sweep them so a C typedef/class/free
            // function surfaces in workspace search too.
            idx.for_each_pack_registered_file(&mut |path, analysis| {
                let file = path.display().to_string();
                if !analysis.symbols_are_evicted() {
                    covered.insert(path.to_path_buf());
                }
                for sym in &analysis.symbols {
                    if sym.hidden_in_outline() {
                        continue;
                    }
                    push(&sym.name, &sym.kind, file.clone(), sym.selection_span);
                }
            });
            for hit in symbols::sym_row_search(idx, &q) {
                let path = std::path::PathBuf::from(&hit.path);
                if covered.contains(&path) {
                    continue;
                }
                if hit.flags & file_analysis::SymRowSeed::FLAG_HIDDEN_IN_OUTLINE != 0 {
                    continue;
                }
                let Some(kind) = file_analysis::sym_kind_from_code(hit.kind) else { continue };
                let span = file_analysis::Span {
                    start: tree_sitter::Point::new(hit.start_row, hit.start_col),
                    end: tree_sitter::Point::new(hit.end_row, hit.end_col),
                };
                push(&hit.name, &kind, hit.path.clone(), span);
            }
            Ok(serde_json::to_string_pretty(&results).unwrap())
        }
        "rename" => {
            let new_name = req.newname.clone().unwrap_or_else(|| "RENAMED".to_string());
            // Rename edits are a machine-applied blob keyed on spans, not a
            // location round-tripped into `--at`, so the batch path emits them
            // in engine coordinates (0-based / byte) — the historical shape the
            // gold fixtures encode. The single-mode `--rename` wrapper calls
            // `run_rename` directly and passes the input dialect instead.
            run_rename(ws, idx, file, point, &new_name, CoordFmt::ZeroBasedByte)
        }
        "diagnostics" => Ok(batch_diagnostics(ws, idx)),
        other => Err(format!("unknown query: {}", other)),
    }
}

/// Document-symbol outline as the pretty-JSON array string (shared by
/// `cli_outline` and `run_one`).
fn outline_json(analysis: &file_analysis::FileAnalysis) -> String {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, usize, usize)> =
        std::collections::HashSet::new();
    for sym in &analysis.symbols {
        match sym.kind {
            file_analysis::SymKind::Sub | file_analysis::SymKind::Method
            | file_analysis::SymKind::Package | file_analysis::SymKind::Class
            | file_analysis::SymKind::Variable | file_analysis::SymKind::Field
            | file_analysis::SymKind::Enumerator | file_analysis::SymKind::Handler => {}
            _ => continue,
        }
        if sym.hidden_in_outline() { continue; }
        if matches!(sym.kind, file_analysis::SymKind::Variable | file_analysis::SymKind::Enumerator)
            && analysis.scope_within_sub_body(sym.scope)
        {
            continue;
        }
        let mut entry = serde_json::json!({
            "name": sym.name,
            "kind": format!("{:?}", sym.kind),
            "line": sym.selection_span.start.row,
            "col": sym.selection_span.start.column,
        });
        // Richer per-symbol detail (package, params, inferred return type,
        // method flag, display flavor, handler dispatchers) — the QA/debug
        // signal the outline carries beyond name/kind/line/col.
        if let Some(ref pkg) = sym.package {
            entry["package"] = serde_json::json!(pkg);
        }
        // Union members nest under their container in the LSP outline tree;
        // this flat view carries the container explicitly (`package` stays
        // the class — that's the completion/refs identity).
        if let Some(container) = analysis.union_container_of(sym) {
            entry["container"] = serde_json::json!(container.name);
        }
        if let file_analysis::SymbolDetail::Sub { ref params, is_method, ref display, .. } = sym.detail {
            if params.iter().any(|p| !p.is_invocant) {
                let param_names: Vec<&str> = params.iter()
                    .filter(|p| !p.is_invocant)
                    .map(|p| p.name.as_str())
                    .collect();
                entry["params"] = serde_json::json!(param_names);
            }
            if let Some(rt) = analysis.symbol_return_type_via_bag(sym.id, None) {
                entry["return_type"] = serde_json::json!(file_analysis::format_inferred_type(&rt));
            }
            if is_method {
                entry["is_method"] = serde_json::json!(true);
            }
            if let Some(d) = display {
                entry["display"] = serde_json::json!(format!("{:?}", d));
            }
        }
        if let file_analysis::SymbolDetail::Handler { ref params, ref dispatchers, ref display, .. } = sym.detail {
            let param_names: Vec<&str> = params.iter()
                .filter(|p| !p.is_invocant)
                .map(|p| p.name.as_str())
                .collect();
            entry["params"] = serde_json::json!(param_names);
            entry["dispatchers"] = serde_json::json!(dispatchers);
            entry["display"] = serde_json::json!(format!("{:?}", display));
        }
        use file_analysis::Namespace;
        let is_framework = matches!(sym.namespace, Namespace::Framework { .. });
        let is_dupeable = is_framework && matches!(sym.kind,
            file_analysis::SymKind::Sub | file_analysis::SymKind::Method);
        if is_dupeable {
            let key = (format!("{:?}", sym.kind), sym.name.clone(),
                sym.span.start.row, sym.span.start.column);
            if !seen.insert(key) { continue; }
        }
        results.push(entry);
    }
    serde_json::to_string_pretty(&results).unwrap()
}

/// The override-fan-out scope for CLI references/rename, from
/// `PERL_LSP_RENAME_SCOPE` (`hierarchy` | `dispatch`). Mirrors the LSP
/// `initializationOptions.rename.overrideScope`; absent = the `hierarchy`
/// default. Lets the gold harness exercise both modes per row.
fn override_scope_from_env() -> resolve::OverrideScope {
    std::env::var("PERL_LSP_RENAME_SCOPE")
        .ok()
        .map(|s| resolve::OverrideScope::from_option(&s))
        .unwrap_or_default()
}

/// Cross-file rename edit-set as the pretty-JSON object string (shared by
/// `cli_rename` and `run_one`). `file` is the originating file; `point` the cursor.
fn run_rename(
    ws: &file_store::FileStore,
    idx: &module_index::ModuleIndex,
    file: &str,
    point: tree_sitter::Point,
    new_name: &str,
    fmt: CoordFmt,
) -> Result<String, String> {
    use std::collections::HashMap;
    if !resolve::is_valid_rename_name(new_name) {
        return Err("rename: the new name must not be empty or whitespace".to_string());
    }
    let file_path = std::path::Path::new(file).canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(file));
    let (s, _t, mut analysis) = parse_file(file);
    analysis.enrich_imported_types_with_keys(Some(idx));
    // Same shape as the LSP handler: pack routing declared at construction,
    // stage the origin, construct the set once, project the rename — the
    // per-arm policy (cross-file vs group vs single-file, rewritability,
    // the pack full-or-refuse) lives on the set.
    let reg = language_driver::LanguageRegistry::with_enabled();
    let lang_id = reg.for_path_sniffed(std::path::Path::new(file), &s)
        .map(|d| d.id()).filter(|id| *id != "perl");
    let pack = lang_id.and_then(|lang| idx.pack_index(lang));
    let base_idx: &dyn crate::file_analysis::CrossFileLookup =
        pack.as_deref().map_or(idx as &dyn crate::file_analysis::CrossFileLookup, |i| i);
    let _staged = ScopedWorkspaceEntry::insert(ws, file_path.clone(), analysis);
    let origin = ws.workspace_raw().get(&file_path).map(|r| r.value().clone())
        .expect("origin staged above");
    let mut cs = resolve::resolve(
        ws, &origin, file_store::FileKey::Path(file_path), point,
        Some(base_idx), override_scope_from_env(),
    );
    if pack.is_some() {
        cs = cs.pack_routed();
    }
    if cs.resolution().is_none() {
        return Err(format!("Nothing renameable at {}:{}", point.row, point.column));
    }
    let mut sources = SourceCache::new(fmt);
    let mut all_edits: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for (loc, text) in cs.rename_edits(new_name)? {
        let path = match &loc.key {
            file_store::FileKey::Path(p) => p.display().to_string(),
            file_store::FileKey::Url(u) => u.to_file_path()
                .map(|p| p.display().to_string()).unwrap_or_else(|_| u.to_string()),
        };
        let edit = span_to_json(&mut sources, &path, loc.span, text);
        all_edits.entry(path).or_default().push(edit);
    }
    Ok(serde_json::to_string_pretty(&serde_json::json!(all_edits)).unwrap())
}

/// Whole-tree diagnostics with enrichment parity: each workspace entry
/// goes through the hub's enrichment overlay (`enriched_snapshot` — a
/// derived, fingerprint-keyed copy; the same pass `publish_diagnostics`
/// runs on open docs), so cross-file-typed shapes hint here too. A file
/// whose snapshot fails degrades to its unenriched whole view rather
/// than vanishing.
fn enriched_tree_diagnostics(
    ws: &file_store::FileStore,
    idx: &module_index::ModuleIndex,
    options: symbols::DiagnosticOptions,
) -> Vec<(String, tower_lsp::lsp_types::Diagnostic)> {
    let mut all = Vec::new();
    for entry in ws.workspace_raw().iter() {
        let file = entry.key().display().to_string();
        let cached = std::sync::Arc::new(file_analysis::CachedModule::new(
            entry.key().clone(),
            std::sync::Arc::clone(entry.value()),
        ));
        let diags = match idx.enriched_snapshot(&cached) {
            Some(fa) => symbols::collect_diagnostics(&fa, idx, options),
            None => {
                // Index copies may be refs/bag-evicted; diagnostics read
                // refs AND the bag, so degrade to the whole-on-both-axes
                // view, not the resident copy.
                let whole =
                    file_analysis::CrossFileLookup::whole_present(idx, &cached);
                symbols::collect_diagnostics(&whole, idx, options)
            }
        };
        for d in diags {
            all.push((file.clone(), d));
        }
    }
    // Pack-language files (C++/…) live in the per-language sub-indexes, not the
    // Perl-only `FileStore` above. Mirror the backend's language dispatch: they
    // get `pack_diagnostics` (Mode B — member-op swap + peel), so `--batch
    // diagnostics` / `--check` / gold see the same Mode-B answers the LSP
    // publishes. No enrichment (pack files aren't cross-file-enriched).
    idx.for_each_pack_index(|_lang, pack| {
        pack.for_each_registered_file(&mut |cm| {
            let file = cm.path.display().to_string();
            // Same whole-view routing: pack index copies are evicted.
            let whole = file_analysis::CrossFileLookup::whole_present(pack.as_ref(), cm);
            for d in symbols::pack_diagnostics(&whole, options) {
                all.push((file.clone(), d));
            }
        });
    });
    all
}

/// Whole-tree diagnostics as the pretty-JSON array string (warning+; shared by
/// `--batch` diagnostics requests). Mirrors `cli_check`'s JSON path.
fn batch_diagnostics(ws: &file_store::FileStore, idx: &module_index::ModuleIndex) -> String {
    let options = symbols::DiagnosticOptions::default();
    let mut all = Vec::new();
    for (file, d) in enriched_tree_diagnostics(ws, idx, options) {
        let sev = match d.severity {
            Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::ERROR => "error",
            Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::WARNING => "warning",
            Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::INFORMATION => "info",
            Some(s) if s == tower_lsp::lsp_types::DiagnosticSeverity::HINT => "hint",
            _ => "warning",
        };
        all.push(serde_json::json!({
            "file": file, "line": d.range.start.line, "col": d.range.start.character,
            "severity": sev,
            "code": d.code.map(|c| match c {
                tower_lsp::lsp_types::NumberOrString::String(s) => s,
                tower_lsp::lsp_types::NumberOrString::Number(n) => n.to_string(),
            }).unwrap_or_default(),
            "message": d.message,
        }));
    }
    serde_json::to_string_pretty(&all).unwrap()
}

/// --batch <root> — read JSONL queries on stdin, share one startup, print one
/// JSON response per line. The harness substrate: amortizes the workspace-index
/// + @INC cost across every query instead of paying it per process.
fn cli_batch(root: &str) {
    use std::io::{BufRead, Write};
    let (ws, idx) = cli_full_startup(root);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut diag_memo: Option<String> = None;
    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let req: BatchReq = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(out, "{}", serde_json::json!({"ok": false, "err": format!("bad request: {}", e)}));
                continue;
            }
        };
        // Whole-tree diagnostics are request-independent within one
        // batch process (ScopedWorkspaceEntry restores any staging) —
        // compute the enriched pass once, replay it for every row.
        let result = if req.q == "diagnostics" {
            Ok(diag_memo
                .get_or_insert_with(|| batch_diagnostics(&ws, &idx))
                .clone())
        } else {
            // The batch protocol's input is engine-native (0-based/byte) and its
            // output dialect is fixed to what gold fixtures encode: the location
            // modes render 1-based/char (this fmt), while rename/workspace-symbol
            // stay engine-coordinated within `run_one` itself.
            run_one(&ws, &idx, &req, CoordFmt::EditorOneBasedChar)
        };
        let resp = match result {
            Ok(s)  => serde_json::json!({"id": req.id, "ok": true,  "out": s}),
            Err(e) => serde_json::json!({"id": req.id, "ok": false, "err": e}),
        };
        let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
        let _ = out.flush();   // flush per line so a later abort still localizes
    }
}

/// --rename <root> <file> <line> <col> <new> (positional, 0-based/byte) or
/// --rename <root> --at <file>:<line>:<col> <new> (editor, 1-based/char).
/// `cursor` is the args between `<root>` and `<new>`. Output edit coordinates
/// match the input dialect.
fn cli_rename(root: &str, cursor: &[String], new_name: &str) {
    let target = parse_cursor_target(cursor, root).unwrap_or_else(|| {
        eprintln!(
            "perl-lsp --rename: expected `<root> <file> <line> <col> <new>` or `<root> --at <file>:<line>:<col> <new>`"
        );
        std::process::exit(2);
    });
    emit_pos_annotation(&target);
    // Full startup so workspace files are built with the same plugins, type
    // inference, and enrichment that the LSP backend would use.
    let (ws, idx) = cli_full_startup(root);
    match run_rename(&ws, &idx, &target.file, target.point, new_name, target.fmt) {
        Ok(s) => println!("{}", s),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

/// --dump-package <root> <package> — Dump every sub in a package with
/// derived type info. Debugging aid for the witness/reducer pipeline:
/// prints the raw `return_type` baked into the Symbol, the witness-bag
/// projection at the default and a few common arities, the structural
/// tail-delegation, every param's inferred type at a point
/// just past the sub's signature, and a witness count so you can see at
/// a glance whether the bag has anything to say.
fn cli_dump_package(root: &str, package_name: &str) {
    use std::sync::Arc;
    use file_analysis::{SymKind, SymbolDetail};

    let (ws, module_index) = cli_full_startup(root);

    // Find a FileAnalysis whose package matches. Workspace first; fall
    // back to cached @INC modules. No bespoke discovery — only what
    // the normal startup populated.
    let mut found: Option<(String, Arc<file_analysis::CachedModule>)> = None;
    for entry in ws.workspace_raw().iter() {
        let cm = std::sync::Arc::new(file_analysis::CachedModule::new(
            entry.key().clone(),
            std::sync::Arc::clone(entry.value()),
        ));
        let analysis = file_analysis::CrossFileLookup::whole_present(&module_index, &cm);
        let has_package = analysis.symbols.iter().any(|s| {
            matches!(s.kind, SymKind::Package | SymKind::Class)
                && s.name == package_name
        });
        if has_package {
            found = Some((entry.key().display().to_string(), cm));
            break;
        }
    }
    if found.is_none() {
        if let Some(cached) = module_index.get_cached(package_name) {
            found = Some((cached.path.display().to_string(), cached));
        }
    }

    let Some((path, cached)) = found else {
        eprintln!("Package '{}' not found in workspace or module cache.", package_name);
        eprintln!("(Run the LSP against this workspace once to populate cached @INC modules.)");
        std::process::exit(1);
    };

    // The enrichment overlay (R4): the same derived, fingerprint-keyed
    // enriched copy the diagnostics sweep reads — imported return types
    // visible, shared Arc untouched. Degrade to the whole view when the
    // overlay declines (cycle guard).
    let analysis = module_index.enriched_snapshot(&cached).unwrap_or_else(|| {
        // Overlay declined (serde break / byte-cap giant / cycle taint):
        // dump unenriched, LOUDLY — silent degrade here looks exactly like
        // the inference bug the user is debugging.
        eprintln!(
            "warning: enrichment overlay declined for {path}; cross-file return \
             types will be missing from this dump"
        );
        file_analysis::CrossFileLookup::whole_present(&module_index, &cached)
    });

    // Collect subs/methods declared inside this package.
    let mut subs: Vec<&file_analysis::Symbol> = analysis
        .symbols
        .iter()
        .filter(|s| {
            matches!(s.kind, SymKind::Sub | SymKind::Method)
                && s.package.as_deref() == Some(package_name)
        })
        .collect();
    subs.sort_by_key(|s| (s.span.start.row, s.span.start.column));

    let framework = analysis
        .package_framework
        .get(package_name)
        .map(|f| format!("{:?}", f));

    let mut sub_entries = Vec::with_capacity(subs.len());
    for sym in &subs {
        let SymbolDetail::Sub {
            ref params,
            is_method,
            ref display,
            hide_in_outline,
            opaque_return,
            ref doc,
            ..
        } = sym.detail
        else {
            continue;
        };

        // Pick a point inside the sub body so scope-resolved param
        // lookups land in the right scope. End of line N+1 is past
        // any signature parens for almost every shape.
        let probe = tree_sitter::Point::new(
            sym.span.start.row.saturating_add(1),
            0,
        );

        let bag_default = analysis
            .sub_return_type_at_arity(&sym.name, None)
            .as_ref()
            .map(file_analysis::format_inferred_type);
        let mut by_arity = serde_json::Map::new();
        for arity in 0u32..=2 {
            if let Some(t) = analysis.sub_return_type_at_arity(&sym.name, Some(arity)) {
                by_arity.insert(arity.to_string(), serde_json::json!(file_analysis::format_inferred_type(&t)));
            }
        }

        let raw_return = analysis
            .symbol_return_type_via_bag(sym.id, None)
            .as_ref()
            .map(file_analysis::format_inferred_type);

        let param_entries: Vec<_> = params
            .iter()
            .map(|p| {
                let inferred = analysis
                    .inferred_type_via_bag_ctx(&p.name, probe, Some(&module_index))
                    .as_ref()
                    .map(file_analysis::format_inferred_type);
                serde_json::json!({
                    "name": p.name,
                    "is_invocant": p.is_invocant,
                    "is_slurpy": p.is_slurpy,
                    "default": p.default,
                    "inferred_type": inferred,
                })
            })
            .collect();

        // Witness count on the sub's Symbol attachment — surfaces
        // arity / branch-arm observations the reducer is folding.
        let symbol_witness_count = analysis
            .witnesses
            .for_attachment(&witnesses::WitnessAttachment::Symbol(sym.id))
            .len();

        // Provenance: where did this return type come from? Default
        // (Inferred) is implicit; surfaced explicitly only when
        // something else (plugin override / reducer / delegation)
        // contributed. Critical debugging aid when inference grows
        // complex enough that "the LSP says X" needs to come with
        // "because Y" — without re-running the build.
        let provenance = match analysis.return_type_provenance(sym.id) {
            file_analysis::TypeProvenance::Inferred => None,
            file_analysis::TypeProvenance::PluginOverride { plugin_id, reason } => {
                Some(serde_json::json!({
                    "kind": "PluginOverride",
                    "plugin_id": plugin_id,
                    "reason": reason,
                }))
            }
            file_analysis::TypeProvenance::ReducerFold { reducer, evidence } => {
                Some(serde_json::json!({
                    "kind": "ReducerFold",
                    "reducer": reducer,
                    "evidence": evidence,
                }))
            }
            file_analysis::TypeProvenance::Delegation { kind, via } => {
                Some(serde_json::json!({
                    "kind": "Delegation",
                    "delegation_kind": kind,
                    "via": via,
                }))
            }
            file_analysis::TypeProvenance::FrameworkSynthesis { framework, reason } => {
                Some(serde_json::json!({
                    "kind": "FrameworkSynthesis",
                    "framework": framework,
                    "reason": reason,
                }))
            }
        };

        // Variables typed inside this sub's scope. Surfaces chain
        // assignments like `my $route = $self->_route(...)->...->to(...)`
        // — when `$route` shows up here with a class type, the chain
        // typer worked. When it doesn't, the chain died at some link.
        // The same dump that answers "why is `_generate_route`'s
        // return type None" answers "is `$route` typed at all".
        let sub_scope_id = analysis
            .scopes
            .iter()
            .find(|s| {
                matches!(
                    &s.kind,
                    file_analysis::ScopeKind::Sub { name } | file_analysis::ScopeKind::Method { name }
                        if name == &sym.name
                ) && s.span.start == sym.span.start
            })
            .map(|s| s.id);
        let mut vars_in_scope: Vec<serde_json::Value> = Vec::new();
        if let Some(sid) = sub_scope_id {
            use crate::witnesses::{WitnessAttachment, WitnessPayload};
            for w in analysis.witnesses.all() {
                let WitnessAttachment::Variable { name, scope } = &w.attachment else { continue };
                if *scope != sid { continue; }
                let WitnessPayload::InferredType(t) = &w.payload else { continue };
                vars_in_scope.push(serde_json::json!({
                    "var": name,
                    "type": file_analysis::format_inferred_type(t),
                    "line": w.span.start.row,
                }));
            }
        }

        let mut entry = serde_json::json!({
            "name": sym.name,
            "kind": format!("{:?}", sym.kind),
            "is_method": is_method,
            "line": sym.selection_span.start.row,
            "params": param_entries,
            "raw_return_type": raw_return,
            "bag_return_type": bag_default,
            "bag_return_type_at_arity": serde_json::Value::Object(by_arity),
            "symbol_witness_count": symbol_witness_count,
            "vars_in_scope": vars_in_scope,
        });
        if let Some(prov) = provenance {
            entry["return_type_provenance"] = prov;
        }
        if let Some(d) = display {
            entry["display"] = serde_json::json!(format!("{:?}", d));
        }
        if hide_in_outline {
            entry["hide_in_outline"] = serde_json::json!(true);
        }
        if opaque_return {
            entry["opaque_return"] = serde_json::json!(true);
        }
        if let Some(ref outline) = sym.outline_label {
            entry["outline_label"] = serde_json::json!(outline);
        }
        if let Some(d) = doc.as_ref() {
            let first_line = d.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            if !first_line.is_empty() {
                entry["doc_first_line"] = serde_json::json!(first_line);
            }
        }
        sub_entries.push(entry);
    }

    let parents = analysis
        .package_parents
        .get(package_name)
        .cloned()
        .unwrap_or_default();

    let out = serde_json::json!({
        "package": package_name,
        "file": path,
        "framework": framework,
        "parents": parents,
        "total_witnesses_in_file": analysis.witnesses.len(),
        "subs": sub_entries,
    });

    eprintln!(
        "Dumped {} subs from {} (file: {})",
        subs.len(),
        package_name,
        out.get("file").and_then(|v| v.as_str()).unwrap_or("?")
    );
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

/// --workspace-symbol <root> <query> — Search symbols
fn cli_workspace_symbol(root: &str, query: &str) {
    // Full startup so workspace symbols reflect plugin-synthesized entities
    // (helpers, routes, accessors), built with `root`'s plugins not cwd's.
    let (ws, idx) = cli_full_startup(root);
    let req = BatchReq {
        id: String::new(), q: "workspace-symbol".into(),
        file: String::new(), line: 0, col: 0,
        query: Some(query.to_string()), newname: None,
    };
    // workspace-symbol emits engine-coordinated spans (0-based/byte) directly,
    // independent of the location seam; the dialect here is nominal.
    print_run_one(&ws, &idx, &req, CoordFmt::ZeroBasedByte);
}

/// Which symbols a usage heatmap lists: nameable callables and packages.
/// A listing policy, not an identity decision — identity is minted by the
/// CandidateSet at the symbol's declaration. Anonymous subs (`(anon)`) and
/// other non-identifier names have no nameable reference graph (their name
/// would cross-link every other anon); lexical variables, hash-key/field
/// slots, and handlers have no meaningful cross-file usage count.
fn heatmap_symbol_eligible(sym: &file_analysis::Symbol) -> bool {
    use file_analysis::SymKind;
    sym.name.starts_with(|c: char| c.is_alphabetic() || c == '_')
        && matches!(
            sym.kind,
            SymKind::Sub
                | SymKind::Method
                | SymKind::Package
                | SymKind::Class
                | SymKind::Module
        )
}

/// One heatmap row for one symbol — the shared body both the Perl and the
/// pack-language (C/C++/…) gather loops call, so their fan-in counts come
/// from the SAME `references()` projection by construction (no second ref
/// walk). `is_pack` routes identity + the backward reference walk through the
/// caller's per-language sub-index: pack workspace files ride the DEPENDENCY
/// role (a storage artifact of the per-language cache), so the set widens to
/// VISIBLE via `pack_routed()` instead of the Perl `mask`, and the pack-only
/// entry-point guard (C's `main` is reached through the ABI, not a call site)
/// unlocks. Returns `(row, is_callable, dead, dead_export)`.
///
/// `forced_fan_in` is the relational pre-prune verdict: `Some(0)` means the
/// row store proved this declaration's references projection empty (no ref
/// row for its name), so the `references()` walk is skipped and fan-in is 0.
/// The pre-prune may only ever assert PROVABLY-EMPTY, never a substituted
/// count — `None` runs the full projection, and every computed fan-in still
/// comes from `references()`. `dead_export_override` is the row-backed
/// unused-exports verdict, passed ONLY alongside a skipped walk (where it is
/// provably equal to what the projection would derive); whenever the
/// projection runs, `None` lets it decide (exported with zero cross-file
/// references) — strictly more accurate than candidate rows, which
/// over-approximate real references.
#[allow(clippy::too_many_arguments)]
fn heatmap_symbol_row(
    ws: &file_store::FileStore,
    routing_idx: &dyn file_analysis::CrossFileLookup,
    path: &std::path::Path,
    analysis: &file_analysis::FileAnalysis,
    sym: &file_analysis::Symbol,
    is_pack: bool,
    mask: resolve::RoleMask,
    scope: resolve::OverrideScope,
    has_dynamic_dispatch: bool,
    forced_fan_in: Option<usize>,
    dead_export_override: Option<bool>,
    sources: &mut SourceCache,
) -> (serde_json::Value, bool, bool, bool) {
    use file_analysis::{AccessKind, Namespace, RefKind, SymKind};
    use std::collections::HashSet;

    let within = |outer: &file_analysis::Span, inner: &file_analysis::Span| {
        let s = |p: &tree_sitter::Point| (p.row, p.column);
        s(&inner.start) >= s(&outer.start) && s(&inner.end) <= s(&outer.end)
    };
    let path_str = path.display().to_string();

    // fan_in = the references image minus the symbol's declaration site(s);
    // cross_file_fan_in additionally drops every same-file reference (the
    // dead-export test: an export used only by its own module is dead to
    // consumers). Both project from the ONE `references()` set minted at the
    // declaration — identity is never re-derived heatmap-side. Pack routing
    // is a construction fact (which sub-index, VISIBLE-wide walk), declared
    // here exactly as the references/goto-def CLI mirrors declare it.
    //
    // The relational pre-prune (`forced_fan_in`) may skip this walk only when
    // the row store proved it empty; a computed count always comes from here.
    let (fan_in, cross_file_fan_in) = match forced_fan_in {
        Some(n) => (n, 0usize),
        None => {
            let mut cs = resolve::resolve(
                ws,
                analysis,
                file_store::FileKey::Path(path.to_path_buf()),
                sym.selection_span.start,
                Some(routing_idx),
                scope,
            );
            if is_pack {
                cs = cs.pack_routed();
            } else {
                cs = cs.with_visibility(mask);
            }
            let locs = cs.references();
            let fan_in = locs
                .iter()
                .filter(|l| l.access != AccessKind::Declaration)
                .filter(|l| {
                    !(l.span == sym.selection_span
                        && matches!(&l.key, file_store::FileKey::Path(p) if p == path))
                })
                .count();
            let cross_file = locs
                .iter()
                .filter(|l| l.access != AccessKind::Declaration)
                .filter(|l| !matches!(&l.key, file_store::FileKey::Path(p) if p == path))
                .count();
            (fan_in, cross_file)
        }
    };

    // fan_out = distinct callee names referenced inside this body (subs /
    // methods only). Packages have no body to scan.
    let is_callable = matches!(sym.kind, SymKind::Sub | SymKind::Method);
    let fan_out: Option<usize> = if is_callable {
        let mut callees: HashSet<&str> = HashSet::new();
        for r in &analysis.refs {
            if matches!(
                r.kind,
                RefKind::FunctionCall { .. }
                    | RefKind::MethodCall { .. }
                    | RefKind::DispatchCall { .. }
            ) && within(&sym.span, &r.span)
            {
                callees.insert(r.unqualified_target_name());
            }
        }
        callees.remove(sym.name.as_str());
        Some(callees.len())
    } else {
        None
    };

    let exported = analysis.exports_name(&sym.name);
    let native = matches!(sym.namespace, Namespace::Language);

    // Reachability guard — why a zero-fan-in symbol is NOT flagged dead.
    // Ordered most-specific-first. Address-taken / used-as-value functions
    // need no guard: a non-call reference (`&fn`, function-pointer decay) is
    // still a reference, so it lands in `fan_in` and never reaches here.
    let guard: Option<&'static str> = if fan_in > 0 {
        None
    } else if exported {
        Some("exported")
    } else if conventions::is_constructor_name(&sym.name) {
        Some("constructor")
    } else if !native {
        Some("framework-synthesized")
    } else if is_pack && is_callable && sym.name == "main" {
        // C/C++ entry point: the runtime enters through `main` over the ABI,
        // never a source call site the static graph can see.
        Some("entry-point")
    } else if matches!(sym.kind, SymKind::Package | SymKind::Class | SymKind::Module) {
        Some("package-implicit-use")
    } else if has_dynamic_dispatch
        && matches!(sym.kind, SymKind::Sub | SymKind::Method)
        && sym.package.as_deref().is_some_and(|p| p != "main")
    {
        Some("dynamic-dispatch")
    } else {
        None
    };

    let dead = fan_in == 0 && guard.is_none();
    // A dead export is an EXPORTED callable with no cross-file reference —
    // orthogonal to `dead_code_candidate` (which the `exported` guard shields).
    // Row-backed when the pre-prune supplied a verdict; otherwise the
    // projection's cross-file count answers it.
    let dead_export = match dead_export_override {
        Some(v) => v,
        None => is_callable && exported && cross_file_fan_in == 0,
    };
    let (line, col) = sources.display(
        &path_str,
        sym.selection_span.start.row,
        sym.selection_span.start.column,
    );
    let kind = format!("{:?}", sym.kind);

    let row = serde_json::json!({
        "name": sym.name,
        "kind": kind,
        "package": sym.package,
        "file": path_str,
        "line": line,
        "col": col,
        "fan_in": fan_in,
        "fan_out": fan_out,
        "exported": exported,
        "dead_code_candidate": dead,
        "dead_export": dead_export,
        "reachable_guard": guard,
    });
    (row, is_callable, dead, dead_export)
}

/// --refs-parity <root> — the relational-ref-index migration net
/// (`docs/adr/relational-ref-index.md`). Mints the CandidateSet at every
/// heatmap-eligible symbol declaration (Perl workspace + pack files) and
/// projects `references()` twice — resident scan (`PERL_LSP_REF_ROWS=0`) vs
/// SQL retrieval (`=1`) — asserting identical (file, span, access,
/// rewritable) sets. Exit 1 on any divergence. A dev/CI net, not a user
/// verb: run it against a real corpus after touching `refs_to`, the shred,
/// or the eviction seams.
fn cli_refs_parity(root: &str, sample: Option<usize>) {
    // The A/B needs the resident side complete: keep refs + bags resident
    // (rows are still written — eviction and persistence are independent).
    std::env::set_var("PERL_LSP_NO_EVICT", "1");
    let (ws, idx) = cli_full_startup(root);
    let scope = override_scope_from_env();

    let mut pack_entries: Vec<(
        std::path::PathBuf,
        std::sync::Arc<file_analysis::FileAnalysis>,
        std::sync::Arc<module_index::ModuleIndex>,
    )> = Vec::new();
    idx.for_each_pack_index(|_lang, pack| {
        pack.for_each_registered_file(&mut |cached| {
            // Index copies are refs-evicted; fan-out scans + set minting read
            // refs, so take the refs-present view (resident when not evicted,
            // rehydrated otherwise). Batch-CLI-sized cost, not a query path.
            pack_entries.push((
                cached.path.clone(),
                file_analysis::CrossFileLookup::whole_present(pack.as_ref(), cached),
                std::sync::Arc::clone(pack),
            ));
        });
    });
    pack_entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut entries: Vec<(std::path::PathBuf, std::sync::Arc<file_analysis::FileAnalysis>)> = ws
        .workspace_raw()
        .iter()
        .map(|e| {
            // Workspace copies may be refs-evicted; fan-out scans + set
            // minting read refs, so take the refs-present view.
            let cm = std::sync::Arc::new(file_analysis::CachedModule::new(
                e.key().clone(),
                std::sync::Arc::clone(e.value()),
            ));
            (
                e.key().clone(),
                file_analysis::CrossFileLookup::whole_present(&idx, &cm),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let normalize = |locs: &[resolve::RefLocation]| -> Vec<String> {
        let mut v: Vec<String> = locs
            .iter()
            .map(|l| {
                format!(
                    "{:?}:{}:{}-{}:{}:{:?}:{}",
                    l.key,
                    l.span.start.row,
                    l.span.start.column,
                    l.span.end.row,
                    l.span.end.column,
                    l.access,
                    l.rewritable
                )
            })
            .collect();
        v.sort();
        v
    };

    // `--sample=N` strides the symbol universe down to ~N checks — the
    // per-phase quick net (~a minute). The full sweep (no flag) is the
    // pre-merge gate: it re-runs the OLD O(symbols × tree) resident walk
    // per symbol, so it is heatmap×2-shaped by construction.
    let mut seen_symbols = 0usize;
    let total_symbols: usize = entries.iter().map(|(_, a)| a.symbols.len()).sum::<usize>()
        + pack_entries.iter().map(|(_, a, _)| a.symbols.len()).sum::<usize>();
    let stride = sample
        .map(|n| (total_symbols / n.max(1)).max(1))
        .unwrap_or(1);
    let mut checked = 0usize;
    let mut mismatched = 0usize;
    let mut check = |ws: &file_store::FileStore,
                     routing: &dyn file_analysis::CrossFileLookup,
                     path: &std::path::Path,
                     analysis: &file_analysis::FileAnalysis,
                     is_pack: bool,
                     checked: &mut usize,
                     mismatched: &mut usize| {
        for sym in &analysis.symbols {
            seen_symbols += 1;
            if seen_symbols % stride != 0 {
                continue;
            }
            if sym.hidden_in_outline() || !heatmap_symbol_eligible(sym) {
                continue;
            }
            if *checked % 200 == 0 && *checked > 0 {
                eprintln!("refs-parity: {} checked...", *checked);
            }
            let mut cs = resolve::resolve(
                ws,
                analysis,
                file_store::FileKey::Path(path.to_path_buf()),
                sym.selection_span.start,
                Some(routing),
                scope,
            );
            if is_pack {
                cs = cs.pack_routed();
            } else {
                cs = cs.with_visibility(resolve::RoleMask::VISIBLE);
            }
            resolve::set_ref_rows_override(Some(false));
            let resident = normalize(&cs.references());
            resolve::set_ref_rows_override(Some(true));
            let rows = normalize(&cs.references());
            resolve::set_ref_rows_override(None);
            *checked += 1;
            if resident != rows {
                *mismatched += 1;
                let only_resident: Vec<_> =
                    resident.iter().filter(|x| !rows.contains(x)).take(3).collect();
                let only_rows: Vec<_> =
                    rows.iter().filter(|x| !resident.contains(x)).take(3).collect();
                eprintln!(
                    "PARITY MISMATCH {}::{} @ {:?} — resident {} vs rows {}\n  only-resident: {:?}\n  only-rows: {:?}",
                    sym.package.as_deref().unwrap_or(""),
                    sym.name,
                    path,
                    resident.len(),
                    rows.len(),
                    only_resident,
                    only_rows
                );
            }
        }
    };

    for (path, analysis) in &entries {
        check(&ws, &idx, path, analysis, false, &mut checked, &mut mismatched);
    }
    for (path, analysis, pack) in &pack_entries {
        check(&ws, pack.as_ref(), path, analysis, true, &mut checked, &mut mismatched);
    }

    println!(
        "refs-parity: {} symbols checked, {} mismatched",
        checked, mismatched
    );
    if mismatched > 0 {
        std::process::exit(1);
    }
}

/// --heatmap <root> [--csv|--html] [--include-deps] [--all] — Code-usage heatmap.
///
/// Emits per-symbol USAGE metrics as a projection of the resolution
/// CandidateSet (`docs/adr/resolution-candidate-set.md`): fan-in is the
/// `references()` image of the set minted at each symbol's declaration —
/// the SAME set the references/rename verbs project from, so heatmap counts
/// cannot diverge from what `textDocument/references` answers, and every
/// construction axis (visibility masks, group/attr field splats, override
/// families, future closure/delegation gating) is inherited for free. It is
/// a reporting view, not a new analysis tier:
///
///   * fan_in  — how many reference sites a symbol has across the workspace
///               (call sites; the symbol's own declaration is excluded).
///   * fan_out — how many DISTINCT callees a sub/method references in its body
///               (cheap intra-file span containment; `null` for packages).
///   * dead_code_candidate — fan_in == 0 AND no reachability guard fired.
///   * dead_export — an EXPORTED sub with zero CROSS-FILE references (the
///               unused-exports view, `docs/adr/relational-ref-index.md`);
///               orthogonal to dead_code_candidate, which the `exported`
///               guard shields. Sound in one direction (row candidates
///               over-approximate references). When the relational store
///               covers the workspace it also PRE-PRUNES the fan-in walk for
///               provably-unreferenced names; the answer is unchanged, only
///               the work is skipped, and it degrades to the full projection
///               when the store is absent (`PERL_LSP_REF_ROWS=0`, cold cache,
///               `--include-deps`).
///
/// HONEST LABEL: a "dead-code candidate" here is an UNREFERENCED SYMBOL — a
/// reachability heuristic, NOT MISRA C:2012 Rule 2.2 dead code (undecidable).
/// We OVER-APPROXIMATE reachability (sound for "is it live?", may under-report
/// dead): a symbol is treated as reachable (never flagged) when it is exported,
/// is a constructor, or — for methods, when ANY file in the workspace dispatches
/// dynamically (`$obj->$method`) — could be reached through an edge the static
/// graph can't see. Failure modes: symbolic code refs (`\&name`, `&{$n}`),
/// `can`/`->$method` with an unresolved name, `AUTOLOAD`, and string `eval` are
/// invisible; function candidates assume none of these reach them.
fn cli_heatmap(root: &str, opts: &[String]) {
    let csv = opts.iter().any(|a| a == "--csv");
    let html = opts.iter().any(|a| a == "--html");
    let include_deps = opts.iter().any(|a| a == "--include-deps");
    // By default only candidate-eligible kinds (subs/methods/packages with a
    // body) are listed; `--all` keeps every counted symbol in `symbols`.
    let emit_all = opts.iter().any(|a| a == "--all");

    let (ws, idx) = cli_full_startup(root);

    // Pack-language (C/C++/…) files live in per-language sub-indexes, not the
    // Perl `FileStore` — `workspace/symbol` and Mode-B diagnostics sweep these
    // separately, and the heatmap gathers them the same way. Each entry keeps
    // its sub-index so fan-in routes through it (identity minting + the
    // backward reference walk both need the pack cache, not the Perl hub).
    // Snapshot to a Vec for a stable order and to sum dynamic dispatch below.
    let mut pack_entries: Vec<(
        std::path::PathBuf,
        std::sync::Arc<file_analysis::FileAnalysis>,
        std::sync::Arc<module_index::ModuleIndex>,
    )> = Vec::new();
    idx.for_each_pack_index(|_lang, pack| {
        pack.for_each_registered_file(&mut |cached| {
            // Index copies are refs-evicted; fan-out scans + set minting read
            // refs, so take the refs-present view (resident when not evicted,
            // rehydrated otherwise). Batch-CLI-sized cost, not a query path.
            pack_entries.push((
                cached.path.clone(),
                file_analysis::CrossFileLookup::whole_present(pack.as_ref(), cached),
                std::sync::Arc::clone(pack),
            ));
        });
    });
    pack_entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Workspace-level soundness gate. Any dynamic method dispatch makes the
    // static call graph an under-approximation of method reachability, so a
    // zero-fan-in METHOD can't be proven dead. Pack files contribute too
    // (virtual / function-pointer dispatch counts the same).
    let mut dynamic_dispatch_sites: u64 = 0;
    for entry in ws.workspace_raw().iter() {
        dynamic_dispatch_sites += entry.value().dynamic_dispatch_sites as u64;
    }
    for (_p, analysis, _pack) in &pack_entries {
        dynamic_dispatch_sites += analysis.dynamic_dispatch_sites as u64;
    }
    let has_dynamic_dispatch = dynamic_dispatch_sites > 0;

    // References across open + workspace files; `--include-deps` also walks
    // cached @INC modules so a library symbol used only from a dependency
    // shows nonzero fan-in. Applied as the CandidateSet's construction-time
    // visibility so every projection inherits it. The default matches the
    // set's own verdict — every heatmap symbol is workspace-declared, so
    // `references_mask_for` answers EDITABLE by construction — while skipping
    // that verdict's per-symbol whole-store scan.
    let mask = if include_deps {
        resolve::RoleMask::VISIBLE
    } else {
        resolve::RoleMask::EDITABLE
    };
    let scope = override_scope_from_env();

    // Heatmap output keeps its established 1-based/char coordinates.
    let mut sources = SourceCache::new(CoordFmt::EditorOneBasedChar);
    let mut symbol_rows: Vec<serde_json::Value> = Vec::new();
    let mut dead_rows: Vec<serde_json::Value> = Vec::new();
    let mut dead_export_rows: Vec<serde_json::Value> = Vec::new();

    // Stable file order so output is deterministic across runs.
    let mut entries: Vec<(std::path::PathBuf, std::sync::Arc<file_analysis::FileAnalysis>)> = ws
        .workspace_raw()
        .iter()
        .map(|e| {
            // Workspace copies may be refs-evicted; fan-out scans + set
            // minting read refs, so take the refs-present view.
            let cm = std::sync::Arc::new(file_analysis::CachedModule::new(
                e.key().clone(),
                std::sync::Arc::clone(e.value()),
            ));
            (
                e.key().clone(),
                file_analysis::CrossFileLookup::whole_present(&idx, &cm),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Relational pre-prune (`docs/adr/relational-ref-index.md`, phase 4). The
    // row store answers two things the per-declaration `references()` walk
    // would otherwise rediscover file-by-file: which names have ANY reference
    // row (a name absent here has a provably-empty projection → fan-in 0, walk
    // skipped) and which exported syms have no cross-file reference (the
    // unused-exports view → the dead-export verdict, no walk). Both are SOUND
    // ONLY when the store covers every file the walk would scan, so this is
    // gated: rows enabled (`PERL_LSP_REF_ROWS != 0`), the store available and
    // covering every workspace entry, and EDITABLE scope — `--include-deps`
    // widens the walk to @INC files whose ref rows this Perl store does not
    // witness. Any gate unmet ⇒ `None` ⇒ every declaration takes the full
    // projection and the dead-export verdict is derived from it (unchanged
    // behavior; pure fallback). Pack symbols always take the projection —
    // their per-language store is a separate coverage question left to the
    // sound fallback.
    let rows_env_on = std::env::var("PERL_LSP_REF_ROWS")
        .map(|v| v != "0")
        .unwrap_or(true);
    let perl_prune: Option<(
        std::collections::HashSet<String>,
        std::collections::HashSet<(String, String, usize, usize)>,
    )> = if rows_env_on && !include_deps {
        match (idx.ref_prune_index(), idx.unused_exported_syms()) {
            (Some((referenced_names, shredded)), Some(dead)) => {
                let covered = entries
                    .iter()
                    .all(|(p, _)| shredded.contains(p.to_string_lossy().as_ref()));
                if covered {
                    let dead_keys = dead
                        .into_iter()
                        .map(|d| (d.path, d.name, d.start_row, d.start_col))
                        .collect();
                    Some((referenced_names, dead_keys))
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    };

    // Gather rows for one file's symbols through `heatmap_symbol_row` — the
    // one place fan-in/fan-out/dead are computed, so Perl and pack share the
    // exact `references()` projection. `hidden_in_outline` folds arity-variant
    // accessor twins / DSL-import infrastructure into their listed primary
    // (same contract the outline honors); `heatmap_symbol_eligible` keeps it
    // to nameable callables/packages.
    let gather = |ws: &file_store::FileStore,
                  routing: &dyn file_analysis::CrossFileLookup,
                  path: &std::path::Path,
                  analysis: &file_analysis::FileAnalysis,
                  is_pack: bool,
                  row_mask: resolve::RoleMask,
                  symbol_rows: &mut Vec<serde_json::Value>,
                  dead_rows: &mut Vec<serde_json::Value>,
                  dead_export_rows: &mut Vec<serde_json::Value>,
                  sources: &mut SourceCache| {
        for sym in &analysis.symbols {
            if sym.hidden_in_outline() || !heatmap_symbol_eligible(sym) {
                continue;
            }
            // Perl symbols consult the pre-prune; pack symbols always take the
            // full projection (see the gate rationale above).
            let (forced_fan_in, dead_export_override) = match (is_pack, perl_prune.as_ref()) {
                (false, Some((referenced_names, dead_keys))) => {
                    let key = file_analysis::name_match_key(&sym.name);
                    let forced = if referenced_names.contains(&key) {
                        None // has reference rows — the projection must run
                    } else {
                        Some(0usize) // no reference row anywhere → provably empty
                    };
                    // The row verdict substitutes ONLY for a skipped walk,
                    // where it's provably equal to what the projection would
                    // derive (no ref rows at all ⇒ no cross-file references).
                    // When the walk runs, the projection decides: a candidate
                    // row is an over-approximation, so the rows can say
                    // "maybe used" for an export whose every candidate the
                    // matcher rejects — a real dead export the row verdict
                    // would mask.
                    let de = forced.map(|_| {
                        let is_callable = matches!(
                            sym.kind,
                            file_analysis::SymKind::Sub | file_analysis::SymKind::Method
                        );
                        let sel = sym.selection_span.start;
                        is_callable
                            && dead_keys.contains(&(
                                path.to_string_lossy().to_string(),
                                sym.name.clone(),
                                sel.row,
                                sel.column,
                            ))
                    });
                    (forced, de)
                }
                _ => (None, None),
            };
            let (row, is_callable, dead, dead_export) = heatmap_symbol_row(
                ws,
                routing,
                path,
                analysis,
                sym,
                is_pack,
                row_mask,
                scope,
                has_dynamic_dispatch,
                forced_fan_in,
                dead_export_override,
                sources,
            );
            if dead {
                dead_rows.push(row.clone());
            }
            if dead_export {
                dead_export_rows.push(row.clone());
            }
            if emit_all || is_callable || dead {
                symbol_rows.push(row);
            }
        }
    };

    for (path, analysis) in &entries {
        gather(
            &ws,
            &idx,
            path,
            analysis,
            false,
            mask,
            &mut symbol_rows,
            &mut dead_rows,
            &mut dead_export_rows,
            &mut sources,
        );
    }

    // Pack languages route through their own sub-index (VISIBLE-wide — pack
    // workspace files ride the DEPENDENCY role); `pack_routed()` inside the
    // helper applies that, so `mask` here is only the Perl knob.
    for (path, analysis, pack) in &pack_entries {
        let routing: &dyn file_analysis::CrossFileLookup = pack.as_ref();
        gather(
            &ws,
            routing,
            path,
            analysis,
            true,
            resolve::RoleMask::VISIBLE,
            &mut symbol_rows,
            &mut dead_rows,
            &mut dead_export_rows,
            &mut sources,
        );
    }

    // Heaviest fan-in first — the hotspots a reader wants up top.
    symbol_rows.sort_by(|a, b| {
        b["fan_in"].as_u64().cmp(&a["fan_in"].as_u64())
            .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
    });
    // Dead exports read best alphabetically — this is a to-triage list, not a
    // hotspot ranking.
    dead_export_rows.sort_by(|a, b| {
        a["name"].as_str().cmp(&b["name"].as_str())
            .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
    });

    if csv {
        println!("name,kind,package,file,line,col,fan_in,fan_out,exported,dead_code_candidate,dead_export,reachable_guard");
        let cell = |v: &serde_json::Value| -> String {
            match v {
                serde_json::Value::Null => String::new(),
                serde_json::Value::String(s) => csv_escape(s),
                other => other.to_string(),
            }
        };
        for r in &symbol_rows {
            println!(
                "{},{},{},{},{},{},{},{},{},{},{},{}",
                cell(&r["name"]), cell(&r["kind"]), cell(&r["package"]), cell(&r["file"]),
                cell(&r["line"]), cell(&r["col"]), cell(&r["fan_in"]), cell(&r["fan_out"]),
                cell(&r["exported"]), cell(&r["dead_code_candidate"]), cell(&r["dead_export"]),
                cell(&r["reachable_guard"]),
            );
        }
        return;
    }

    let out = serde_json::json!({
        "schema": "perl-lsp.heatmap.v1",
        "kind": "usage-heatmap",
        "label": "dead_code_candidate: a symbol with no references found — a review queue, not a delete list. Confirm it's unused before removing.",
        "dead_export_label": "dead_export: an EXPORTED sub with no reference from any OTHER file — its export earns nothing, though the module may use it internally. Sound-in-one-direction (rows over-approximate references, so zero cross-file candidates means truly unused by consumers; a nonzero count is never read as 'used'). A review queue for shrinking export surface, not a delete list.",
        "soundness": "Flagging errs toward reachable, so it never flags exported symbols, constructors, framework-synthesized members, packages, or (when the workspace uses dynamic dispatch) any method. C/C++ dead-code is more over-approximate: `main` and address-taken functions are shielded, but a zero-fan-in symbol may still be exported/`extern \"C\"` ABI surface, a callback wired through a function pointer the graph can't follow, or a template instantiated in an unscanned translation unit — treat the list as a review queue.",
        "root": root,
        "files_indexed": entries.len() + pack_entries.len(),
        "dynamic_dispatch_sites": dynamic_dispatch_sites,
        "include_deps": include_deps,
        "summary": {
            "symbols_reported": symbol_rows.len(),
            "dead_code_candidates": dead_rows.len(),
            "dead_exports": dead_export_rows.len(),
        },
        "symbols": symbol_rows,
        "dead_code_candidates": dead_rows,
        "dead_exports": dead_export_rows,
    });

    // `--html` wraps the SAME report in a self-contained, offline viewer
    // (treemap heat + fan-in/fan-out butterfly). No external assets: the
    // report JSON is embedded so the file opens straight off disk.
    if html {
        println!("{}", heatmap_html(&out));
        return;
    }

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

/// Render a `--heatmap` report as a single self-contained HTML document.
///
/// The whole report is embedded as a `<script type="application/json">`
/// blob and drawn client-side with dependency-free SVG — no CDN, no build
/// step, opens with a `file://` URL. Two views over the same `symbols[]`:
/// a squarified treemap (tile area = fan_in+1, color = fan_in heat,
/// dead-code candidates outlined) and a back-to-back fan-in/fan-out
/// butterfly of the hottest symbols.
fn heatmap_html(report: &serde_json::Value) -> String {
    // The report carries file paths (attacker-adjacent text), so escape every
    // `<` to its JSON unicode form: that makes a stray `</script>` impossible
    // regardless of content, and `JSON.parse` restores the `<` client-side.
    let data = serde_json::to_string(report)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c");
    HEATMAP_HTML_TEMPLATE.replace("__HEATMAP_DATA__", &data)
}

/// Self-contained viewer template; `__HEATMAP_DATA__` is replaced with the
/// embedded report JSON. Kept as one literal so the asset travels with the
/// binary (no runtime file lookup, no build-time bundling).
const HEATMAP_HTML_TEMPLATE: &str = include_str!("heatmap.html");

/// Minimal RFC-4180 CSV field escaping: quote when the value contains a
/// comma, quote, or newline; double embedded quotes.
fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
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
fn cli_clear_cache(root: Option<&str>) {
    let target = match root {
        Some(r) => {
            let (_, root_uri) = canonical_root_and_uri(r);
            module_cache::cache_dir_for_workspace(Some(&root_uri))
        }
        None => module_cache::cache_base_dir(),
    };
    let Some(path) = target else {
        eprintln!("Cannot determine cache dir: $HOME and $XDG_CACHE_HOME are both unset");
        std::process::exit(1);
    };
    if !path.exists() {
        eprintln!("Cache already absent: {}", path.display());
        return;
    }
    match std::fs::remove_dir_all(&path) {
        Ok(()) => eprintln!("Cleared cache: {}", path.display()),
        Err(e) => {
            eprintln!("Failed to remove {}: {}", path.display(), e);
            std::process::exit(1);
        }
    }
}

/// Pretty-print the tree-sitter parse tree for a Perl source file.
/// `<file>` may be `-` to read from stdin. Mirrors `tree-sitter parse`
/// output shape — `(node_kind [row, col] - [row, col]` per line,
/// 2-space indent per depth, field names prefixed (`field: kind`).
fn cli_parse(path: &str, lang: Option<&str>) {
    use std::io::Read;
    let source = if path == "-" {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            eprintln!("read stdin: {}", e);
            std::process::exit(1);
        }
        s
    } else {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("read {}: {}", path, e);
                std::process::exit(1);
            }
        }
    };
    // Route to a grammar: an explicit `lang` id (stdin can't route by
    // extension) wins, else the file's extension (cpp/python/r/cmake), else
    // a content sniff for an extension no driver claims, so
    // --parse shows the SAME tree the pack extractor sees. Perl + stdin +
    // truly-unrecognized files keep the Perl grammar.
    let mut parser = if let Some(id) = lang {
        let reg = crate::language_driver::LanguageRegistry::with_enabled();
        match reg.for_id(id) {
            Some(d) => d.make_parser(),
            None => {
                eprintln!(
                    "unknown language `{}`; served: {}",
                    id,
                    reg.languages().join(", ")
                );
                std::process::exit(1);
            }
        }
    } else if path != "-" {
        let reg = crate::language_driver::LanguageRegistry::with_enabled();
        reg.for_path_sniffed(std::path::Path::new(path), &source)
            .filter(|d| d.id() != "perl")
            .map(|d| d.make_parser())
            .unwrap_or_else(builder::create_parser)
    } else {
        builder::create_parser()
    };
    let Some(tree) = parser.parse(&source, None) else {
        eprintln!("parse failed");
        std::process::exit(1);
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

#[cfg(test)]
mod coord_tests {
    use super::*;

    #[test]
    fn zero_based_byte_renders_engine_native() {
        // row/byte passed straight through; source line ignored.
        assert_eq!(CoordFmt::ZeroBasedByte.render(4, 30, Some("anything")), (4, 30));
        // No source line still yields the raw byte column.
        assert_eq!(CoordFmt::ZeroBasedByte.render(0, 7, None), (0, 7));
    }

    #[test]
    fn editor_one_based_char_converts_bytes_on_multibyte_line() {
        // `my $msg = "héllo wörld→"; greet();` — the `greet` call starts at byte
        // 30 (é/ö are 2 bytes, → is 3) but character column 26 (0-based) → 27
        // (1-based). A byte renderer would over-count to 31.
        let line = "my $msg = \"héllo wörld→\"; greet();";
        assert_eq!(
            CoordFmt::EditorOneBasedChar.render(4, 30, Some(line)),
            (5, 27),
            "byte 30 on the multibyte line is 1-based char col 27"
        );
        // Fallback (no source) uses the byte column directly, 1-based.
        assert_eq!(CoordFmt::EditorOneBasedChar.render(4, 30, None), (5, 31));
    }

    #[test]
    fn editor_input_round_trips_to_internal_and_back() {
        // Single-line source: editor line 1, char col 27 → internal row 0, byte
        // 30 (the `g` of greet, after é/ö/→ — char index 26 but byte 30).
        let source = "my $msg = \"héllo wörld→\"; greet();";
        let p = editor_to_internal_point(Some(source), 1, 27);
        assert_eq!((p.row, p.column), (0, 30));
        // Rendering that internal point back in editor dialect returns 1:27.
        let line0 = source.lines().next().unwrap();
        assert_eq!(CoordFmt::EditorOneBasedChar.render(p.row, p.column, Some(line0)), (1, 27));
    }

    #[test]
    fn split_at_spec_takes_last_two_colon_fields() {
        assert_eq!(
            split_at_spec("absl/mutex.h:163:48"),
            Some(("absl/mutex.h".to_string(), 163, 48))
        );
        // A path with no colons still needs both line and col.
        assert_eq!(split_at_spec("foo.pm:12"), None);
        assert_eq!(split_at_spec("foo.pm"), None);
        // Extra colons (drive prefix) fold into the file part.
        assert_eq!(
            split_at_spec("C:/src/x.h:9:3"),
            Some(("C:/src/x.h".to_string(), 9, 3))
        );
    }

    #[test]
    fn token_at_byte_finds_word_and_flags_whitespace() {
        let line = "class ABSL_LOCKABLE Mutex {";
        // On the `M` of Mutex (byte 20).
        assert_eq!(token_at_byte(line, 20).as_deref(), Some("Mutex"));
        // One past the end of `Mutex` (the space) still reports it.
        assert_eq!(token_at_byte(line, 25).as_deref(), Some("Mutex"));
        // On the `{` — punctuation, no word.
        assert_eq!(token_at_byte(line, 26), None);
        // Multibyte: on `greet` after a unicode prefix.
        let uni = "my $msg = \"héllo wörld→\"; greet();";
        assert_eq!(token_at_byte(uni, 30).as_deref(), Some("greet"));
    }

    #[test]
    fn parse_cursor_target_picks_dialect_from_form() {
        // Positional → engine dialect.
        let pos = parse_cursor_target(&[
            "f.pm".to_string(), "2".to_string(), "4".to_string(),
        ], ".")
        .unwrap();
        assert_eq!(pos.fmt, CoordFmt::ZeroBasedByte);
        assert_eq!((pos.point.row, pos.point.column), (2, 4));
        // `--at` (missing file on disk) → editor dialect, char col used as byte.
        let at = parse_cursor_target(&[
            "--at".to_string(), "does/not/exist.pm:6:1".to_string(),
        ], ".")
        .unwrap();
        assert_eq!(at.fmt, CoordFmt::EditorOneBasedChar);
        assert_eq!((at.point.row, at.point.column), (5, 0));
        // Malformed → None.
        assert!(parse_cursor_target(&["only-one".to_string()], ".").is_none());
    }

    #[test]
    fn resolve_cursor_file_prefers_cwd_then_root() {
        let dir = std::env::temp_dir().join(format!("perl-lsp-rcf-{}", std::process::id()));
        let sub = dir.join("lib");
        std::fs::create_dir_all(&sub).unwrap();
        let rel = "lib/Thing.pm";
        let abs = dir.join(rel);
        std::fs::write(&abs, "package Thing;\n1;\n").unwrap();

        // Root-relative path that does NOT exist against CWD resolves via <root>.
        let resolved = resolve_cursor_file(rel, dir.to_str().unwrap());
        assert!(std::path::Path::new(&resolved).exists(), "root fallback failed: {}", resolved);

        // An absolute/CWD-existing path is kept verbatim (root not consulted).
        let kept = resolve_cursor_file(abs.to_str().unwrap(), "/nonexistent-root");
        assert_eq!(kept, abs.to_str().unwrap());

        // Neither exists → original returned unchanged for an honest downstream miss.
        let missing = resolve_cursor_file("no/such.pm", dir.to_str().unwrap());
        assert_eq!(missing, "no/such.pm");

        std::fs::remove_dir_all(&dir).ok();
    }
}
