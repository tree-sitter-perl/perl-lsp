# Epic 6 — Gated cross-file emission (ClassIsa triggers + in_role params through cross-file ancestors)

> **Status:** scheduled (6th).
> **Design owner-doc:** `docs/prompt-enrichment-inheritance-residual.md`
> — read it WHOLE; its applicability matrix is the epic's map, and its
> two `#[ignore]`d probes are the acceptance tests. The landed pattern
> to copy is `docs/adr/receiver-gated-dispatch.md`.

## Mission

Plugin **emission** gated on a class's ancestry is not cross-file
aware: `package Leaf; use parent 'Mid';` where `Mid` (another file)
extends `Mojo::EventEmitter` means `Leaf`'s `$self->on('ready', …)`
synthesizes NO Handler — the `ClassIsa("Mojo::EventEmitter")` trigger
sees only local parents. The fix is the same move `dispatch_verbs`
already made: **defer the ancestry check to query time on the
`ReceiverGated` seam** — the builder emits an ungated candidate, and
consumers resolve the isa-check against the module index when they
read. The owner doc names this exactly: "Phase 2 mints it on the same
seam."

## Read first

1. `docs/prompt-enrichment-inheritance-residual.md` — the matrix, §"The
   `ClassIsa` trigger axis", and why a mid-walk index consult is
   forbidden (rule #1: the builder is index-free; indexing order is
   not guaranteed).
2. `docs/adr/receiver-gated-dispatch.md` — the landed seam:
   `ReceiverGated<T>` wraps a payload unreadable without the isa check;
   `resolve_for` walks the single `class_isa` seam.
3. `CLAUDE.md` rules #1, #8; "Cross-file enrichment".

## Current state — anchors

- The failing probe (your acceptance test #1):
  `grep -n 'probe_class_isa_trigger_through_cross_file_parent' src/builder_tests.rs`
  — `#[ignore]`d, FAILS today. Un-ignore it at the end.
- Trigger matching: `PluginRegistry::applicable`
  (`grep -n 'fn applicable' src/plugin/mod.rs`) matches `ClassIsa`
  against `transitive_parents` — LOCAL `package_parents` only
  (`grep -n 'fn transitive_parents' src/builder.rs`).
- The seam to extend: `grep -n 'ReceiverGated' src/file_analysis.rs src/builder.rs | head`
  — how `gated_param_types` and dispatch candidates ride the FA and
  resolve lazily.
- Emit-hook dispatch sites: `dispatch_function_call_plugins` /
  `dispatch_method_call_plugins` in `src/builder.rs`.

## The design (prescribed)

At build time, when a plugin's `ClassIsa(T)` trigger does NOT match
locally, the builder cannot know it never will — so for call shapes a
ClassIsa-triggered plugin registered interest in, the builder records a
**gated emission candidate**: the plugin id + the `CallContext`
ingredients needed to re-run the hook + `gate = ClassIsa(T)` on the
enclosing package. These ride `FileAnalysis` (serde, like
`gated_param_types`). At query/enrichment time, a resolver with the
module index checks the gate via the single `class_isa` seam and, on
pass, applies the plugin's emissions.

Two honest constraints shape the implementation — confront them
up front:

1. **Re-running a Rhai hook needs the engine, not just data.** Two
   options; pick per measurement, record in the ADR:
   - (a) Store the ungated hook's OUTPUT: run the hook at build time
     regardless of trigger, wrap the resulting `Vec<EmitAction>` in
     `ReceiverGated`, apply on gate-pass. Cost: hooks run for
     non-matching packages (measure with `--timings` on the substrate;
     plugin hooks are cheap and verb-filtered, so this is the expected
     winner). Benefit: pure data rides the cache; no engine at query
     time.
   - (b) Store the `CallContext` and re-run at enrichment (engine
     available in-process, NOT in cached-dep consumers) — weaker: dep
     files served purely from cache can't re-run. Prefer (a) unless
     timings veto it.
2. **Which EmitActions can apply post-build?** Symbol/Handler/
   HashKeyDef synthesis appends to FA — the enrichment idempotency
   machinery (`base_symbol_count` truncation,
   `rebuild_enrichment_indices`) already supports append-after-build;
   route through it. Actions that steer the WALK itself (VarType at a
   span the fold already consumed) may need the fold re-entered —
   `enrich_imported_types_with_keys` already re-runs fold pieces;
   study it before assuming.

## Phase breakdown

### Phase A — gated candidates for `ClassIsa` emit hooks

Implement design option (a): run ClassIsa-triggered hooks ungated at
build; wrap outputs `ReceiverGated<Vec<EmitAction>>` keyed on the
enclosing package + trigger class; store on FA (new serde-default
field; EXTRACT_VERSION bump). Apply at the same points enrichment
applies its synthesis for OPEN docs, and lazily via the query paths
non-open files already use for gated dispatch (mirror
`applicable_dispatches`' consumption shape).

**Acceptance:** the `probe_class_isa_trigger_through_cross_file_parent`
probe un-ignored and green: `Leaf` (child file) + `Mid` (parent file,
extends Mojo::EventEmitter) → `$self->on('ready', …)` in Leaf
synthesizes the Handler, cross-file, without Leaf being open.

### Phase B — `param_types` `in_role` cross-file (verify, then close)

The matrix says this landed via `gated_param_type_for`; the owner doc's
header still lists "two real gaps". Verify which probe is which:
if `probe_*` for param_types is still ignored/failing, apply the same
Phase-A consumption path; if it is already green, update the owner doc
and move on. Do not build machinery for a closed gap.

### Phase C — timings + docs

1. `--timings` on the substrate before/after (option (a) runs more
   hooks at build): the per-module build report must not regress the
   slowest-modules tail by more than noise; if it does, gate the
   ungated-run to packages with ANY parents (cheap prefilter) and
   re-measure.
2. Update `docs/prompt-enrichment-inheritance-residual.md` (matrix
   rows → landed), write `docs/adr/gated-emission.md` (the option-(a)
   decision, the honest boundary: emission still cannot depend on
   values only the index knows at WALK time — it is gate-then-apply,
   not a re-walk).
3. Full gate: cargo test, gold, substrate audit at parity or improved
   (`unresolved-method` should DROP for cross-file-ancestry framework
   classes — note the sites).

## Non-goals

- No mid-walk module-index access (rule #1 stands).
- No enrichment of dependency files (the "OPEN documents only"
  lifecycle stands; consumption is lazy/query-time for everything
  else, exactly like receiver-gated dispatch).
- Helper-consumption phase 3 (per-app surfaces) — different gate
  (instance brands), stays parked.

## Sizing

Medium. Phase A is the bulk; B is verification; C is half a day.
