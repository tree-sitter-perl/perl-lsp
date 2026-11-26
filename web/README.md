# perl-lsp in the browser

`runtime/` runs the threaded WebAssembly build of the server
(`scripts/wasm/build.sh`) in a page:

- `client.js` is what a page imports. `startServer({ wasm, shim, files })`
  returns an LSP connection (`request`, `notify`, `onNotification`).
- `kernel.js` is a worker that owns one in-memory file system and answers
  WASI calls for every thread.
- `thread.js` is one worker per wasm thread.

The page must be cross-origin isolated (`Cross-Origin-Opener-Policy:
same-origin` and `Cross-Origin-Embedder-Policy: require-corp`), because the
threads share one memory. `shim` is the URL of
[`@bjorn3/browser_wasi_shim`](https://github.com/bjorn3/browser_wasi_shim)'s
`dist/index.js`.

`npm ci && npm run smoke` runs the server on `demo/` in headless Chromium and
checks hover, definition, completion and references across files.

## The demo page

`demo/` is a static page: Monaco as the editor, the server in workers, and
`demo.js` mapping Monaco's providers onto LSP requests.

```
scripts/wasm/build.sh --release
cd web && npm ci
cpm install -L deps --cpanfile demo/project/cpanfile
node build-site.mjs ../target/wasm32-wasip1-threads/release/perl-lsp.wasm site deps/lib/perl5
```

The frameworks the project uses are not in the repo. `demo/project/cpanfile`
pins them, the build ships their `.pm` files as `deps.json`, and the page
places them at `local/lib/perl5` in the server's file system, where the
server looks for a project's dependencies without running `perl`.

`site/` is self-contained and can be served by any static host. On a host
that cannot send the isolation headers (GitHub Pages), `coi.js` registers a
service worker that adds them and reloads the page once.
The project the page opens is `demo/project/`, written so each scene in
`demo.js` has something to show. `node test/demo.mjs site` clicks every scene
on the built page and checks what appears; a new scene needs an expectation
there. The "Demo page" workflow
deploys it to GitHub Pages when run by hand.

Design and limits: `docs/adr/wasm-target.md`.
