# Epic 9 — Mojo polish: route names, stash intelligence, hooks, transitive plugins

> **Status:** scheduled (9th). The user-facing feature epic.
> **Design owner-doc:** `docs/prompt-mojo-todo.md` — read WHOLE; its
> stash section contains a fully-made design decision (per-action
> ownership via the brand) that this epic implements as written.

## Mission

Four missing Mojo features, all landing as plugin patches
(`frameworks/mojo-*.rhai`) on existing core seams: route naming +
`url_for`, stash-key intelligence, hook completion + signatures, and
transitive plugin-chain helper discovery. Plus a `.conf` config
completion stretch, explicitly droppable.

## Read first

1. `docs/prompt-mojo-todo.md` — the spec. Its "Ready vs missing"
   paragraph for stash is the checklist.
2. `docs/adr/brands.md` — route marks accumulate route
   defaults; the stash key set per action IS the brand's stash at the
   terminal `->to`.
3. `docs/adr/plugin-system.md` + `docs/PLUGIN_AUTHORING.md` — emit vs
   query hooks; `Handler` + bridges; `classified_pairs`.
4. `frameworks/mojo-routes.rhai`, `frameworks/mojo-helpers.rhai`.

## Phase breakdown

### Phase A — route naming + `url_for`

1. At `->name('show_user')` chain links, `mojo-routes.rhai` emits a
   Handler keyed by the route NAME (display `Route`), bridged the way
   routes already are. Lite auto-naming emits the same Handler when no
   explicit `->name` exists — **implement only the documented default
   rule**, and mark synthesized-name Handlers `hide_in_outline` to avoid
   outline noise.
2. `url_for('…')` / `redirect_to('…')` first-string-arg becomes a
   dispatch ref to that Handler, via the existing dispatch-verb
   machinery (register the verbs through the manifest).
3. Completion inside the string arg offers known route names — an
   `on_completion` query hook enumerating the route-name namespace.
4. **Acceptance:** goto-def from `url_for('show_user')` to the `->name`
   call; references on the name lists both; completion offers it; a gold
   row for each; the heatmap treats a never-`url_for`ed named route
   honestly (fan-in 0 → orphan). This composes with Epic 8 — note it
   either way.

### Phase B — stash intelligence (the big one)

Implement the owner doc's decision EXACTLY — keys are per-ACTION,
sourced from the brand. Do not relitigate per-controller ownership; the
doc explains why it over-broadens.

1. **Emission, route side:** at each terminal `->to` naming an action,
   emit `HashKeyDef`s for the in-force route marks
   (inherited overlay + local), owned per-action. Ownership shape: the
   doc's options (a)+(b) BOTH — an action-scoped `HashKeyOwner` variant
   for deref reads AND namespace registration for string-arg
   enumeration. **The owner enum lives in core and is generic
   ("action-scoped key"); the MINTING is the plugin's.**
2. **Emission, body side:** `render(k => v)` / `stash(k => v)` inside
   `sub action` add body-local keys to the same per-action set — plugin
   `on_method_call` with `classified_pairs`, skipping the known render
   options (`template`/`format`/`status`/`handler`/…), a vocabulary the
   plugin owns.
3. **Identity bridge:** the body-side `<current_package>#<sub>` must
   meet the decl-side `users#list` through the SAME decamelize +
   namespace rule goto-def already uses. **Grep the controller
   resolution in `mojo-routes.rhai` and REUSE it** — a second spelling
   of decamelize is the bug the doc warns about.
4. **Read side:** `$c->stash('|')` string-arg completion (query hook);
   `$c->stash->{|}` hash-key completion + goto-def via the owner path;
   hover on a key shows the defining `->to`/`render` site.
5. **Honest boundary (from the doc):** an action whose chain roots at an
   unbranded hashref param has an empty inherited stash — body-local
   keys still work. Do not attempt to fix that boundary here
   (`open-problems.md` owns it).
6. **Acceptance:** the doc's worked example as a fixture; completion at
   both read forms lists exactly the expected labels for `list` and NOT
   for `show`; gold completion rows with `exact_labels`; cross-file (app
   file + controller file) versions of the same.

### Phase C — hook completion + signatures

1. A new small `mojo-hooks.rhai` (hooks are their own concept):
   `on_completion` inside `->hook('|')` returns the hook-name table;
   `on_signature_help` returns the per-hook param shape. The owner
   doc's table is complete — encode it as plugin DATA.
2. The handler sub's params get typed via the existing
   `NamedSubParamType`/`VarType` emission the helpers plugin already
   uses.
3. **Acceptance:** completion + sig-help rows for two hooks with
   different shapes (`before_dispatch` vs `around_dispatch`).

### Phase D — transitive plugin chains

Plugin A's `register` calls `$app->plugin('B')` — B's helpers should
reach the host. Short-name resolution and one hop landed; this adds the
transitive walk **at RESOLVE time, not parse time**: where helper
resolution consults loaded plugins, follow `plugin_loads` found in an
already-loaded plugin's module one more level, with a seen-set and a
small depth cap. **Termination goes on the dispatcher, never the
worker** (rule #10's note).

**Acceptance:** a three-file fixture (app loads A; A's register loads B;
B registers helper `h`) — `$c->h` resolves in the app's controllers; a
cycle fixture terminates.

### Phase E — `.conf` config completion (STRETCH — droppable)

Only if A–D land with room. Mojo config is a Perl hashref, so it can be
parsed with the existing parser as an expression file; emit its key
shape and complete `$app->config->{…}` off it via the existing hash-key
machinery. **If the parse story gets ugly, drop the phase and record
why in the owner doc.**

## Non-goals

- Multi-app workspaces (parked with instance brands).
- The unbranded-root boundary (`open-problems.md`).

## Language-pack beat

**This epic is Perl-only, and that is correct — but it is also the
best available reference implementation of a framework tier, and Epic
13 will read it.**

Pack languages have no framework plugin tier today; giving them one is
the open design round (`prompt-multi-language.md`), and it is Epic 13
Phase C. The honest statement for this epic is: *nothing here
generalizes, and nothing here should try.* But two things it produces
are worth writing down deliberately, because they are the requirements
document for that future tier:

1. **What this epic needs that a query overlay could NOT express.**
   Phase B is the interesting case: the stash key set is accumulated
   along a route chain (route brand inheritance) and then attributed
   to an action identified by a decamelize rule. That is *name surgery
   and accumulation across nodes*, not pattern matching. When Epic 13
   asks "does a declarative query overlay suffice, or does the tier need
   an imperative hook?", Phase B is the exhibit. **Record, in the PR or
   the owner doc, which of these four phases could have been a pure
   pattern match and which could not.** That list is worth more to Epic
   13 than any speculation.
2. **The core seams this epic extends must not gain Mojo names.** The
   action-scoped `HashKeyOwner` variant (Phase B step 1) lives in core
   and is generic; `HandlerDisplay`, dispatch verbs, and
   `classified_pairs` are all already generic. If any phase needs a
   `if framework == Mojo` branch in core, that is a rule-#10 violation
   AND a signal that the seam is under-specified for the pack tier —
   fix the seam, do not add the branch.

## Scaling beat

**Two of these phases add completion candidates, and completion payload
is a measured, previously-regressed axis.**

`prompt-scale-validation-hitlist.md` Tier 1 #4 (2026-08-17): completion
at 138k files was **7.29 MB and 236 ms per keystroke**; fixed to 55.9 KB
/ 4 ms (`b6312ea2`). Phases A, B and C each add a completion source.

Obligations:

1. **Every completion phase ships an `exact_labels` gold row AND a
   payload measurement.** The gold row proves correctness; the payload
   number proves you did not undo `b6312ea2`. Use
   `bench/lsp_bench.py` with a completion scenario, three runs, dated,
   into `bench/`.
2. **Phase A's route-name namespace and Phase C's hook table are
   bounded and small** — a workspace has tens of routes, and the hook
   table is fixed. These are safe. Say so with a number rather than
   assuming.
3. **Phase B is the one to watch.** Per-action stash key sets are
   emitted as `HashKeyDef`s per terminal `->to`. A large Mojo app has
   many routes × many inherited stash keys, and the emission is
   per-route, not per-key-once. Report the FA symbol-count delta on the
   corpus's Mojo project. Koha is the right corpus — it is the only one
   hitting DBIC **and** Mojo plugin paths together, and it runs in
   minutes per round.
4. **Phase D walks plugin chains at resolve time**, which means it
   happens on the query path, not the index path. A depth cap and a
   seen-set are correctness requirements (cycles) *and* latency
   requirements. Cap it low — the doc says one more level; do not make
   it unbounded because a fixture wanted three.
5. `EXTRACT_VERSION` bump for the emission changes; bundle A–D under
   one.

## Verification gate

`cargo test` · gold, with each phase authoring its rows (`exact_labels`
for every completion addition) · **e2e additions in `e2e/mojo_*.lua`
where cursor-position behavior is the deliverable** — completion inside
a string literal is exactly what e2e catches and unit tests fake ·
substrate audit at parity (emission changes can move unresolved counts;
triage any) · completion payload bytes, three runs, dated.

## Sizing

Large overall but cleanly phased; A/C/D are each small, B is the bulk.
Ship phases as separate PRs.
