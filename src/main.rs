mod build;
mod cst;
mod index;
mod lsp;
mod model;
mod util;

#[cfg(test)]
#[path = "layering_tests.rs"]
mod layering_tests;

use lsp::cli::*;
use lsp::{backend, panic_guard, plugin_cli, stdio_bridge};

use backend::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Both of these must precede the CLI dispatch, because every verb returns
    // out of it: the logger so `RUST_LOG` reaches CLI runs (a CLI verb does
    // the same startup the server does, and it is the cheaper thing to debug),
    // and the ghost trail as a scope guard rather than one emit per arm.
    // Both inert unless their env var is set.
    env_logger::init();
    let _ghost_trail = util::ghost_stats::EmitOnDrop::new("cli-eof");
    // Self-imposed caps, armed here so every verb inherits them. Inert unless
    // PERL_LSP_MAX_RSS_MB / PERL_LSP_MAX_SECONDS are set. An external kill takes
    // the instrumentation down with the process; this exits through the front
    // door, so a capped run still reports what it managed to measure.
    util::watchdog::arm();

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
        // `--type-at` mirrors `--hover`'s three forms: `<root> --at <f:l:c>`,
        // `<root> <file> <line> <col>` (cross-file, full startup), and the
        // legacy single-file `<file> <line> <col>`. Every other cursor verb
        // takes a root, so the root spelling must not die on `Is a directory`.
        Some("--type-at") if args.len() >= 5 && args.get(3).map(|s| s == "--at").unwrap_or(false) => {
            cli_cursor("type-at", &args[2], &args[3..]);
            return;
        }
        Some("--type-at") if args.len() >= 6 => {
            cli_cursor("type-at", &args[2], &args[3..6]);
            return;
        }
        Some("--type-at") if args.len() == 5 => {
            cli_type_at(&args[2], &args[3], &args[4]);
            return;
        }
        // The uniform cursor queries: `<root>` then either `<file> <line> <col>`
        // (positional, 0-based/byte) or `--at <file>:<line>:<col>` (editor,
        // 1-based/char). Flag names map 1:1 to the `run_one` query string.
        Some(
            flag @ ("--definition" | "--type-definition" | "--references" | "--implementations"
            | "--type-hierarchy" | "--call-hierarchy" | "--completion"
            | "--signature-help" | "--document-highlight" | "--linked-editing"),
        ) if args.len() >= 4 => {
            cli_cursor(&flag[2..], &args[2], &args[3..]);
            return;
        }
        Some("--semantic-tokens") if args.len() >= 4 => {
            cli_semantic_tokens(&args[2], &args[3]);
            return;
        }
        Some("--document-link") if args.len() >= 4 => {
            cli_document_link(&args[2], &args[3]);
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
        Some("--gc-cache") if args.len() >= 3 => {
            cli_gc_cache(&args[2]);
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

    // Bridge stdio through dedicated OS threads instead of `tokio::io::stdin()`
    // / `stdout()`. Tokio's stdin wrapper has a lost-wakeup race under load: a
    // complete LSP frame can sit fully buffered while `FramedRead` is never
    // re-polled, so the server never decodes the client's `initialize` and the
    // session hangs (the client waits for a response it will only get if it
    // sends more bytes). A plain blocking reader on its own thread, piped in via
    // a channel, has no such race. See `stdio_bridge`.
    let stdin = stdio_bridge::reader();
    let stdout = stdio_bridge::writer();

    // Warm the plugin registry NOW, overlapping the client's own startup and
    // the initialize handshake — the compile is ~600 ms, and paid lazily it
    // lands inside the first didOpen's build (measured: first build() 712 ms
    // vs 121 ms for the second, same file class). The workspace root isn't
    // known yet, but the registry cell is keyed by the resolved plugin-source
    // paths: when `initialize`'s root doesn't change the on-disk plugin set
    // (any workspace without a repo-local `.perl-lsp/`), this warm is the
    // registry the first build uses; when it does, `initialize`'s own warm
    // rebuilds with the right set and this one cost only background CPU.
    std::thread::spawn(build::plugin::default_plugin_registry);

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
    // An LSP server has nothing to flush after the connection closes —
    // except the report-only ghost-stats trail, which is inert unless
    // PERL_LSP_GHOST_STATS is set and must land before the hard exit.
    util::ghost_stats::emit_all("eof");
    std::process::exit(0);
}
