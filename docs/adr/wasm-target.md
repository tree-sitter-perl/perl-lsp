# ADR: The WebAssembly target

Status: accepted. The threaded build and the demo page are built; the
VS Code web extension and a single-threaded build are forward work.

## Context

The server should run where there is no native process: a demo page on
GitHub Pages, and a web extension on vscode.dev. The server is threaded by
design (a resolver thread, the stdio bridge, a blocking pool, Rayon
indexers), keeps a SQLite store, and links C (tree-sitter and its grammars,
zstd, SQLite).

## Decision

**One crate, one more target.** `wasm32-wasip1-threads` builds the whole
server unmodified in shape: `target_os = "wasi"` gates the four things the
platform lacks (tokio's multi-thread runtime and process support, the
SIGTERM handler, perltidy). There is no core/shell crate split; the layer
DAG stays enforced by `layering_tests`.

**WASI, not `wasm32-unknown-unknown`.** The C dependencies need libc
headers, which the WASI sysroot provides. `scripts/wasm/build.sh` is the one
build entry; CI fetches the sysroot pinned by digest.

**Threads are real threads.** Each wasm thread is a Web Worker sharing one
memory (`web/runtime/`). A kernel worker owns the file system and answers
WASI calls for all of them, so every thread sees one file table. The page
speaks LSP over the server's stdin and stdout, exactly as an editor does to
the native binary.

**The store is SQLite's `memdb` on WASI.** WASI has no file locks, so the
default VFS lets two threads write one file unguarded. `memdb` locks inside
the process. The store does not outlive the page
(`TODO(wasm-store-persistence)`).

**The wasm build uses a pinned nightly** (`scripts/wasm/toolchain`).
DashMap's locks park through `parking_lot_core`, whose wasm parker needs
the unstable wasm atomic wait (rust-lang/rust#77839). Native builds stay on
stable.

**The demo page is a thin LSP client** (`web/demo/`): Monaco's provider
interfaces mapped onto LSP requests, one provider per advertised
capability. It carries no analysis and no Perl knowledge.

## Hosts

| Host | Cross-origin isolated | Runs the threaded build |
|---|---|---|
| GitHub Pages | via the page's service worker (`web/demo/coi.js`) | yes |
| A server that sends COOP/COEP | yes | yes |
| claude.ai Artifacts | no, and service workers are forbidden | no |
| vscode.dev | opt-in by the visitor (`?vscode-coi`), unverified | open |

A host without isolation needs a build with no threads. That is a rework of
the server's concurrency (a host seam for spawning, a resolver that can be
stepped), not a flag; the single-threaded `wasm32-wasip1` build links today
and answers only the one-shot single-file CLI verbs.

## What does not work on WASI

- Formatting: there is no `perltidy` process to run.
- `@INC` discovery: there is no `perl` to ask. Dependencies resolve only
  from files placed in the virtual file system.
- Persistence between page loads.

## Tripwires

- CI's wasm job builds the target, then runs `web/test/smoke.mjs` (the
  server answers cross-file requests in headless Chromium) and
  `web/test/demo.mjs` (the built page gets isolation from its service worker
  and its scenes work).
