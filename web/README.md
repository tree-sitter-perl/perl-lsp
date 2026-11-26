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
