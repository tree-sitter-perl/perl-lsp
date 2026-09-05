# Helper consumption — the per-app residual

Dependency-registered helpers resolve end-to-end, warm and cold, editor
and CLI (phase 1), and the `helper-not-loaded` entrypoint-scan diagnostic
(phase 2, `lsp/symbols/diagnostics.rs`) is landed: at the USAGE site,
"`$c->was_loaded` is provided by Clove::App::Plugin::WasLoaded, which no
entrypoint loads (`plugin 'WasLoaded'`)", HINT severity, firing only for
WORKSPACE plugin modules (installed CPAN plugins keep the generous
policy). Loaded = imported (literally or via SyntheticUse) by any
workspace file, entrypoint scripts included. The standing policy:

- **Framework-loaded plugins** (DefaultHelpers, TagHelpers): apply
  unconditionally — Mojolicious loads them, full stop.
- **Installed/workspace plugins**: "you downloaded it, you probably
  intend to use it" — resolve generously; PRECISION is the
  diagnostic's job, not the resolver's.

An auto-fix code action (insert the `plugin` line) mirroring auto-import
is not built.

## What's open — per-app surfaces (phase 3, graph walking)

mojo-helpers' namespace ids are already per-package; when app
attribution exists (branded edges), the generous union narrows to
per-app surfaces and the `helper-not-loaded` lint gets exact. Tracked
with `prompt-graph-walking.md`.
