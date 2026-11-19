# Helper consumption — phase 3 and the standing policy

Phase 1 (dependency-registered helpers resolve end-to-end, warm and
cold, editor and CLI) landed in the crm QA sweep. The standing policy
from that round:

- **Framework-loaded plugins** (DefaultHelpers, TagHelpers): apply
  unconditionally — Mojolicious loads them, full stop.
- **Installed/workspace plugins**: "you downloaded it, you probably
  intend to use it" — resolve generously; PRECISION is the
  diagnostic's job, not the resolver's.

## Phase 2 — landed

The `helper-not-loaded` entrypoint-scan lint (`lsp/symbols/diagnostics.rs`;
`lsp/symbols/tests/lint_tests.rs`) fires at the usage site, HINT severity,
only for WORKSPACE plugin modules. An auto-fix code action (insert the
`plugin` line, mirroring auto-import) is not built.

## Phase 3 — per-app surfaces (graph walking)

mojo-helpers' namespace ids are already per-package; when app
attribution exists (branded edges), the generous union narrows to
per-app surfaces and the phase-2 lint gets exact. Tracked with
`prompt-graph-walking.md`.
