# Epic 9 — Heatmap residuals: Handlers + plugin-owned reachability

> **Status:** scheduled (9th).
> **Design owner-doc:** `docs/prompt-heatmap.md` §"What's next" — the
> two residuals are specified there in detail, including the verified
> false positive each one fixes. Both are the same generalization: the
> plugin knows an edge the static graph can't see.

## Mission

Two heatmap gaps, both plugin-knowledge-shaped (rule #10 — never a
per-verb/per-name list in core):

1. **Handlers become heatmap-eligible** with a plugin-stamped
   definition site, so orphan routes / never-enqueued Minion tasks /
   never-emitted events surface in the dead-code queue (verified: the
   reference graph already computes their fan-in correctly — only the
   listing elides them).
2. **Plugin-declared "framework-consumed" reachability** replaces the
   blanket dynamic-dispatch shield for lifecycle hooks — fixing the
   verified false positive where a Mojolicious `sub startup` is
   flagged dead (a violation of the heatmap's "never falsely flag a
   live symbol" promise).

## Read first

1. `docs/prompt-heatmap.md` — WHOLE doc, especially "What's next" #1
   and #2 and the guard table.
2. `docs/adr/resolution-candidate-set.md` — heatmap fan-in is the
   `references()` projection; nothing here may add a parallel count.
3. `docs/adr/plugin-system.md` — EmitAction shapes; how Handlers are
   minted.

## Current state — anchors

- Listing policy: `grep -n 'heatmap_symbol_eligible' src/main.rs` —
  admits `Sub|Method|Package|Class|Module`, elides `Handler`.
- Declaration subtraction: the fan-in logic near `cli_heatmap` in
  `src/main.rs` subtracts `AccessKind::Declaration` + the decl
  name-token span — insufficient for Handlers, whose registration IS
  one of their refs (the owner doc explains).
- Handler minting: `grep -n 'Handler' src/plugin/mod.rs | head` — the
  EmitAction that creates them; the definition-site stamp goes here.
- Guards: `grep -n 'reachable_guard' src/main.rs`.

## Phase breakdown

### Phase A — plugin-stamped Handler definition site

1. Extend the Handler-minting EmitAction with a definition-site marker
   — the Handler-shaped equivalent of `AccessKind::Declaration`. The
   cleanest shape: the emitted Handler's OWN declaration ref/span is
   already known to the plugin at mint time (the string key in
   `add_task(cleanup => …)`, the `Controller#action` string in
   `->to(…)`, the event name in `->on(…)`); make the mint record that
   span as the Handler's declaration site the same way Sub symbols
   record theirs (grep how `selection_span`/declaration refs are set
   for plugin Methods and mirror it). If Handlers already carry a
   decl span that the fan-in subtraction just doesn't use, this phase
   is wiring, not schema — CHECK FIRST.
2. Update every bundled plugin that mints Handlers (minion,
   mojo-routes, mojo-events, mojo-lite, dancer, catalyst — grep
   `Handler` in `frameworks/*.rhai`) to stamp it. A plugin that
   doesn't stamp gets the old behavior (eligible but with
   registration counted in fan-in) — decide whether unstamped
   Handlers stay ELIDED instead, and write the decision down;
   eliding-unless-stamped is the safe default (no fan-in ≥ 1 noise).
3. **Acceptance:** unit tests on a fixture: a wired+dispatched task
   (`fan_in ≥ 1`, not dead), a wired-never-dispatched task
   (`fan_in = 0`, dead-candidate), plus the same pair for a route and
   an event. EXTRACT_VERSION bump if the Handler shape changed.

### Phase B — Handlers in the report

1. `heatmap_symbol_eligible` admits stamped Handlers; the fan-in
   subtraction uses the stamped site.
2. The HTML viewer (`src/heatmap.html`): Handlers get their outline
   word (route/task/event — `HandlerDisplay` already knows) in the
   treemap tooltip and dead-code table.
3. **Acceptance:** `--heatmap` JSON schema stays `v1`-compatible
   (additive fields only) or bumps to `v2` with the schema string
   updated — decide, document in the doc's schema section.

### Phase C — framework-consumed reachability

1. New plugin manifest `framework_consumed()` → method names (or
   name+trigger pairs) the framework invokes through its own
   machinery: mojo (`startup`), minion workers if any, Moo/Moose
   lifecycle (`BUILD`, `BUILDARGS`, `DEMOLISH`, `_build_*` builders —
   note `_build_*` is a PATTERN; support a simple `prefix:` form or
   enumerate from `has` options at emit time — prefer emit-time
   enumeration: the moo plugin SEES the `builder => '_build_x'`
   option and can mark that symbol precisely, no pattern needed),
   DBIC (`sqlt_deploy_hook`, `register`…).
2. Carrier: a marker on the Symbol (an EmitAction field for
   plugin-minted syms; for USER-WRITTEN syms like `sub startup`, the
   plugin can't mint — it must mark. Add a small EmitAction
   (`MarkFrameworkConsumed { name }`) applied per-package when the
   trigger fires; bake as a set on FA, serde-default).
3. Heatmap: `reachable_guard = "framework-consumed"` checked BEFORE
   the blanket dynamic-dispatch shield (most-specific-first per the
   guard table), and such syms are skipped for fan-OUT hotspot
   dilution per the owner doc's note.
4. Epic 8 interlock: PL006 dead-sub must consult the same guard —
   if Epic 8 landed first, extend its guard reuse; if not, leave a
   pointer.
5. **Acceptance:** the verified FP as a regression test — a
   `sub startup` in a Mojolicious app fixture is NOT a dead candidate
   and carries the new guard; a genuinely-uncalled non-lifecycle
   method in the same fixture STILL flags (the shield must not
   over-widen).

## Non-goals

- SARIF for heatmap (deferred; `--check` SARIF is Epic 8).
- Transitive fan-out depth (deferred, owner doc).
- Fan-in precision split by RefKind (deferred until `RefLocation`
  carries kind).

## Verification gate

cargo test + gold + `--heatmap` run over the substrate committed as a
before/after summary in the PR (counts of dead candidates by kind —
the Handler additions should ADD candidates (orphans found) while
framework-consumed REMOVES false ones; both deltas listed).

## Sizing

Medium-small. A+B one PR arc, C a second.
