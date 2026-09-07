# Epic 8 — Heatmap residuals: Handlers + plugin-owned reachability

> **Status:** scheduled (8th).
> **Design owner-doc:** `docs/adr/heatmap.md` — the honest
> over-approximation, the guard table, the failure modes, and the two
> residuals this epic closes.

## Mission

Two gaps, both plugin-knowledge-shaped (rule #10 — never a
per-verb/per-name list in core):

1. **Handlers become heatmap-eligible** with a plugin-stamped
   definition site, so orphan routes / never-enqueued tasks /
   never-emitted events surface in the dead-code queue. The reference
   graph already computes their fan-in correctly — only the listing
   elides them.
2. **Plugin-declared "framework-consumed" reachability** replaces the
   blanket dynamic-dispatch shield for lifecycle hooks — fixing the
   verified false positive where a Mojolicious `sub startup` is flagged
   dead, which violates the heatmap's "never falsely flag a live
   symbol" promise.

## Read first

1. `docs/adr/heatmap.md` — WHOLE doc, especially the guard table and
   the failure modes.
2. `docs/adr/resolution-candidate-set.md` — heatmap fan-in **is** the
   `references()` projection, minted at each declaration. Nothing here
   may add a parallel count; that equality is what makes the heatmap's
   numbers match the references verb by construction.
3. `docs/adr/relational-ref-index.md` — the dead-export queue is the
   relational `unused_exported_syms` view, and the same row store
   SOUND-pre-prunes the fan-in walk. A new guard must not defeat the
   pre-prune's soundness.
4. `docs/adr/plugin-system.md` — EmitAction shapes; how Handlers are
   minted.

## Current state — anchors

| What | Where | Find it |
| --- | --- | --- |
| Listing policy | `lsp/cli/heatmap.rs` | `grep -n 'heatmap_symbol_eligible' src/lsp/cli/heatmap.rs` — admits `Sub\|Method\|Package\|Class\|Module`, elides `Handler` |
| Declaration subtraction | `lsp/cli/heatmap.rs` | the fan-in logic subtracts `AccessKind::Declaration` + the decl name-token span — insufficient for Handlers, whose registration IS one of their refs |
| Guards | `lsp/cli/heatmap.rs` | `grep -n 'reachable_guard' src/lsp/cli/heatmap.rs` |
| The language capability already consulted | `lsp/cli/heatmap.rs` | `grep -n 'entrypoint_symbols\|LanguageRegistry::caps\|Namespace::Language' src/lsp/cli/heatmap.rs` — the entry-point guard already reads the analysis language's declared symbols. **This is the pattern to extend, not to invent.** |
| Handler minting | `src/build/plugin/mod.rs` | `grep -n 'Handler' src/build/plugin/mod.rs \| head` |
| Handlers in the bundled plugins | `frameworks/*.rhai` | `grep -ln 'Handler' frameworks/*.rhai` |
| The HTML viewer | `src/heatmap.html` | embedded via `include_str!` |

## Phase breakdown

### Phase A — plugin-stamped Handler definition site

1. **CHECK FIRST:** if Handlers already carry a declaration span that
   the fan-in subtraction simply does not use, this phase is wiring, not
   schema.
2. Otherwise, extend the Handler-minting EmitAction with a
   definition-site marker — the Handler-shaped equivalent of
   `AccessKind::Declaration`. The plugin knows the span at mint time
   (the string key in `add_task(cleanup => …)`, the `Controller#action`
   in `->to(…)`, the event name in `->on(…)`). Mirror how plugin
   Methods record theirs.
3. Update every bundled plugin that mints Handlers to stamp it. A
   plugin that does not stamp gets a decided fallback: **elide-unless-
   stamped is the safe default** (no `fan_in ≥ 1` noise). Write the
   decision down.
4. **Acceptance:** on a fixture — a wired+dispatched task (`fan_in ≥ 1`,
   not dead), a wired-never-dispatched task (`fan_in = 0`,
   dead-candidate), and the same pair for a route and an event.
   `EXTRACT_VERSION` bump if the Handler shape changed.

### Phase B — Handlers in the report

1. `heatmap_symbol_eligible` admits stamped Handlers; the fan-in
   subtraction uses the stamped site.
2. The HTML viewer gives Handlers their outline word (route/task/event —
   `HandlerDisplay` already knows) in the treemap tooltip and the
   dead-code table.
3. **Acceptance:** the `--heatmap` JSON schema stays `v1`-compatible
   (additive fields only) or bumps to `v2` with the schema string
   updated. **Decide and document** in `adr/heatmap.md`'s schema
   section — a silent shape change breaks every downstream consumer.

### Phase C — framework-consumed reachability

1. New plugin manifest `framework_consumed()` → the method names a
   framework invokes through its own machinery: Mojo (`startup`),
   Moo/Moose lifecycle (`BUILD`, `BUILDARGS`, `DEMOLISH`), DBIC
   (`sqlt_deploy_hook`, `register`…).
   **`_build_*` is a pattern, and patterns are the wrong shape here** —
   prefer emit-time enumeration: the moo plugin SEES the
   `builder => '_build_x'` option and can mark that symbol precisely.
   No prefix matching, no name table.
2. Carrier: for plugin-minted symbols, an EmitAction field. For
   USER-WRITTEN symbols like `sub startup` the plugin cannot mint — it
   must MARK. Add a small `MarkFrameworkConsumed { name }` EmitAction
   applied per-package when the trigger fires; bake as a set on the FA's
   plugin lane, serde-default.
3. Heatmap: `reachable_guard = "framework-consumed"` checked BEFORE the
   blanket dynamic-dispatch shield (most-specific-first, per the guard
   table), and such symbols skip fan-OUT hotspot dilution.
4. **Epic 7 interlock:** PL006 dead-sub must consult the same guard. If
   Epic 7 landed first, extend its guard reuse; if not, leave the
   pointer.
5. **Acceptance:** the verified FP as a regression test — `sub startup`
   in a Mojolicious fixture is NOT a dead candidate and carries the new
   guard; a genuinely-uncalled non-lifecycle method in the SAME fixture
   still flags (the shield must not over-widen).

## Non-goals

- SARIF for heatmap (deferred; `--check` SARIF is Epic 7).
- Transitive fan-out depth (deferred).
- Fan-in precision split by `RefKind` (deferred until `RefLocation`
  carries kind).

## Language-pack beat

**The heatmap already serves pack languages, so this epic is a
maintenance obligation, not an opportunity.**

`lsp/cli/heatmap.rs` already asks the language for its capabilities:
the entry-point guard reads the analysis language's declared
`entrypoint_symbols`, and `Namespace::Language` distinguishes native
symbols. A C++ heatmap run exists and works. That means:

1. **Phase B's eligibility change is cross-language by default.** A
   pack language that mints Handler-shaped symbols gets them listed;
   one that does not is unaffected. Verify the C++ heatmap output is
   byte-identical before/after Phase B — if it is not, the eligibility
   predicate grew a Perl assumption.
2. **Phase C's `framework_consumed` manifest is a `.rhai` plugin
   hook, and pack languages have no rhai plugin tier.** Their framework
   knowledge, when it exists, arrives as query overlays and driver
   capabilities. So `framework_consumed` needs a second producer to be
   honest cross-language: **the language capability**. `entrypoint_symbols`
   is the precedent sitting right there — a C++ language declaring
   `main` as an entry point is the same idea. Design the guard to
   consume a UNION of (plugin manifest ∪ language capability), so the
   pack side has a door even before anyone walks through it.
3. Do NOT let the guard become a name list in `heatmap.rs`. That is the
   rule-#10 failure this epic exists to prevent, and it is the exact
   shape a "just add `main` for C++" fix would take.
4. The `--heatmap` schema decision in Phase B is user-visible for every
   language. Whatever version story you pick applies to all of them.

## Scaling beat

**`--heatmap` is a batch verb, and `scaling-limits.md` §5 says so
explicitly. This epic must not blur that.**

Facts to respect:

1. **The gather is parallel per declaration and the sweep sizes its
   rehydration LRU to the corpus** (`adr/heatmap.md` §Cost has the
   knobs; `scaling-limits.md` §5 the dated numbers). Per-worker
   in-flight sets own the RSS crest, so any phase that changes the wall
   reports peak RSS at the default worker count and at
   `RAYON_NUM_THREADS=4`, three runs, dated.
2. **The deeper heatmap cost is not addressable here.** Fan-in is the
   `references()` projection, and references at scale is Epic 15
   Phase A (265–368 s at 138k files, still OPEN) — as is the per-visit
   allocation churn `scaling-limits.md` §5 measured as the heatmap's
   own residual (the 8-thread knee). Epic 15 makes each walk cheaper;
   this epic must not try to.
3. **The heatmap runs `cli_full_startup` with `LanguageScope::All`**
   (`grep -n 'LanguageScope::All' src/lsp/cli/heatmap.rs`) — correct,
   because it sweeps the workspace, and CLAUDE.md's rule is that
   under-indexing a sweeping verb is a quiet wrong answer. It is also
   why it is expensive: the CLI's one-shot semantics are O(corpus) in
   time and RAM, and `--heatmap` is named in the hitlist among the verbs
   that bounds as workspace-scale tools.
4. **Fan-in must stay the `references()` projection.** It is also the
   most expensive thing the heatmap does — references at 138k files
   takes 265–368 s (2026-08-17, Tier 1 #3, still OPEN). Phases A/B add
   Handlers to the listing, which adds symbols whose fan-in must be
   computed. **Report the delta in listed-symbol count and in wall
   time** on Koha, three runs, dated. If Handlers add 20% more symbols,
   the heatmap got 20% slower and users deserve to know.
5. **Do not defeat the sound pre-prune.** The relational row store
   pre-prunes the fan-in walk for provably-unreferenced names,
   degrading to the full projection when rows are absent
   (`adr/relational-ref-index.md`). A Handler whose refs are minted by
   a plugin must either be in the rows or must degrade correctly — a
   Handler that is pre-pruned because the row store never saw it is a
   silently-wrong "dead" verdict, which is the one thing the heatmap
   promises never to do.
6. **Phase C's guard is a set membership check per symbol** — cheap, as
   long as the set is baked once per file and not recomputed. Keep it in
   the plugin lane, default-empty.
7. The honest framing in `adr/heatmap.md` — over-approximation, named
   failure modes, batch-not-interactive — is load-bearing. Any number
   this epic changes gets its date stamped in the doc.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS ·
`./e2e/run.sh` · a `--heatmap` run over the substrate committed as a
before/after summary in the PR: **dead candidates by kind** — Handlers
should ADD candidates (orphans found) while framework-consumed REMOVES
false ones; both deltas listed · C++ heatmap output unchanged by Phase B ·
Koha `--heatmap` wall, three runs, dated.

## Sizing

Medium. A+B are one PR arc, C a second.
