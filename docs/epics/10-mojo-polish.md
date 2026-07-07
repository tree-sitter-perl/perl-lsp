# Epic 10 — Mojo polish: route names, stash intelligence, hooks, transitive plugins

> **Status:** scheduled (10th). User-facing feature epic.
> **Design owner-doc:** `docs/prompt-mojo-todo.md` — read WHOLE; its
> stash section contains a fully-made design decision (per-action
> ownership via the brand) that this epic implements as written.

## Mission

The four missing Mojo features, all landing as plugin patches
(`frameworks/mojo-*.rhai`) on existing core seams: route naming +
`url_for`, stash-key intelligence, hook completion + signatures, and
transitive plugin-chain helper discovery. Plus the `.conf` config
completion stretch, explicitly droppable.

## Read first

1. `docs/prompt-mojo-todo.md` — the spec. Its "Ready vs missing"
   paragraph for stash is the checklist.
2. `docs/adr/route-branding.md` — `BrandedRoute` accumulates route
   defaults; the stash key set per action IS the brand's stash at the
   terminal `->to`.
3. `docs/adr/plugin-system.md` + `docs/PLUGIN_AUTHORING.md` — emit vs
   query hooks; `Handler` + bridges; `classified_pairs`.
4. `frameworks/mojo-routes.rhai` and `frameworks/mojo-helpers.rhai` —
   the plugins being extended.

## Phase breakdown

### Phase A — route naming + `url_for`

1. At `->name('show_user')` chain links, `mojo-routes.rhai` emits a
   Handler keyed by the route NAME (display `Route`), bridged the same
   way routes already are. Lite auto-naming (the route path with
   non-word chars stripped — check Mojolicious docs for the exact
   rule; implement only the documented default) emits the same Handler
   when no explicit `->name` exists — mark synthesized-name Handlers
   `hide_in_outline` to avoid outline noise.
2. `url_for('…')` / `redirect_to('…')` first-string-arg becomes a
   dispatch ref to that Handler (the existing dispatch-verb machinery:
   register the verbs via the manifest — grep `dispatch_verbs` in the
   rhai files for the pattern).
3. Completion inside the string arg offers known route names — an
   `on_completion` query hook enumerating the route-name namespace.
4. **Acceptance:** goto-def from `url_for('show_user')` to the
   `->name` call; references on the name lists both; completion offers
   it; a gold row for each (author with `--emit`); heatmap treats a
   never-`url_for`ed named route honestly (fan-in 0 → orphan — this
   composes with Epic 9 if landed; note it either way).

### Phase B — stash intelligence (the big one)

Implement the owner doc's decision EXACTLY — keys are per-ACTION,
sourced from the brand; do not relitigate per-controller ownership
(the doc explains why it over-broadens):

1. **Emission, route side:** at each terminal `->to` that names an
   action, emit `HashKeyDef`s for the in-force `BrandedRoute.stash`
   keys (inherited overlay + local), owned per-action. Ownership
   shape: the doc's option (a)+(b) BOTH — a new
   `HashKeyOwner::MojoAction { class, action }`-style owner for deref
   reads AND namespace registration for string-arg enumeration. The
   owner enum lives in core (file_analysis) but is generic
   ("action-scoped key"), the MINTING is the plugin's.
2. **Emission, body side:** `render(k => v)` / `stash(k => v)` inside
   `sub action` add body-local keys to the same per-action set —
   plugin `on_method_call` with `classified_pairs`, skipping the known
   render options (`template`/`format`/`status`/`handler`/… — the
   plugin owns this vocabulary list).
3. **Identity bridge:** body-side `<current_package>#<sub>` must meet
   decl-side `users#list` through the SAME decamelize+namespace rule
   goto-def already uses — grep the controller resolution in
   `mojo-routes.rhai` and REUSE it; a second spelling of decamelize is
   the bug the doc warns about.
4. **Read side:** `$c->stash('|')` string-arg completion (query hook
   enumerating the action's set); `$c->stash->{|}` hash-key completion
   + goto-def via the owner path; hover on a key shows the defining
   `->to`/`render` site.
5. **Honest boundary (from the doc):** an action whose chain roots at
   an unbranded hashref param has empty inherited stash — body-local
   keys still work. Do not attempt to fix the boundary here
   (`open-problems.md` owns it).
6. **Acceptance:** the doc's worked example as a fixture (under +
   local defaults + render keys; completion at both read forms lists
   exactly `layout`,`title`,`count` for `list` and NOT for `show`);
   gold completion rows with `exact_labels` (noise guard); cross-file
   (app file + controller file) versions of the same.

### Phase C — hook completion + signatures

1. New `mojo-hooks.rhai` (or extend mojo-helpers — prefer a new small
   plugin; hooks are their own concept): `on_completion` inside
   `->hook('|')` returns the hook-name table from the owner doc;
   `on_signature_help` returns the per-hook param shape (the doc's
   table is complete — encode it as plugin data).
2. The handler sub's params get typed via the existing
   `NamedSubParamType`/`VarType` emission the helpers plugin already
   uses (`$c` → controller, etc.) per the table.
3. **Acceptance:** completion + sig-help unit/gold rows for two hooks
   with different shapes (`before_dispatch` vs `around_dispatch`).

### Phase D — transitive plugin chains

Plugin A's `register` calls `$app->plugin('B')` — B's helpers should
reach the host. Short-name resolution + one hop landed; this adds the
transitive walk at RESOLVE time (not parse time): where helper
resolution consults loaded plugins (grep `SyntheticUse` +
`plugin_loads` consumption in resolve/enrichment), follow
`plugin_loads` found in an already-loaded plugin's module one more
level, with a seen-set and a small depth cap (the dispatcher owns
termination — rule #10's "termination on the dispatcher" note).
**Acceptance:** three-file fixture (app loads A; A's register loads B;
B registers helper `h`) — `$c->h` resolves in the app's controllers;
cycle fixture terminates.

### Phase E — `.conf` config completion (STRETCH — droppable)

Only if A–D land with room: parse workspace `*.conf` (Mojo config is a
Perl hashref — it can be parsed with the existing parser as an
expression file), emit its key shape, and complete
`$app->config->{…}` off it via the existing hash-key machinery. If the
parse story gets ugly, drop the phase and record why in the owner doc.

## Non-goals

- Multi-app workspaces (the doc's cross-cutting note — parked with
  instance brands).
- The unbranded-root boundary (`open-problems.md`).

## Verification gate

cargo test + gold (each phase authors its rows) + e2e additions in
`e2e/mojo_*.lua` where cursor-position behavior is the deliverable
(completion inside strings is exactly what e2e catches and unit tests
fake) + substrate audit at parity (emission changes can move
unresolved counts — triage any).

## Sizing

Large overall but cleanly phased; A/C/D are each small, B is the bulk.
Ship phases as separate PRs.
