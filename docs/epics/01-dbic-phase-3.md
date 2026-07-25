# Epic 1 — DBIC out of core, phase 3 (parametric emission + projection)

> **Status:** scheduled, next up. Finishes ROADMAP "Now #2".
> **Design owner-doc:** `docs/prompt-dbic-as-plugin.md` (read it whole —
> the phase ladder, the projection table, the open questions).
> **End state, verbatim from the ladder:** core is plugin-free except
> generic dispatch.

## Mission

Move the last DBIC-specific machinery out of the builder and into the
plugin layer, so `frameworks/dbic.rhai` (plus generic core seams) owns
everything DBIC-shaped. Phases 1–2 already landed: accessor/relationship
synthesis, arg-name verbs, column-keyed verbs, fluent verbs, and
meta-method suppression all live in the plugin. What remains in core is
the **parametric ResultSet minting** and the **hardcoded semantics of
`DBIx::Class::ResultSet`** — plus two pinned user-facing gaps that only
make sense to fix on the plugin side of the move.

## Read first, in this order

1. `CLAUDE.md` — especially rules #1 (tree traversal only in build()),
   #8 (plugin-synthesized content), #10 (never special-case shapes),
   "Worklist invariants" (re-emittable passes are clear-and-emit;
   edges, not values), and "Type inference (witness bag)".
2. `docs/prompt-dbic-as-plugin.md` — the design, including the
   per-method projection table and the `parametric_semantics()` sketch.
3. `docs/adr/parametric-types.md` — the sealed-flavor Parametric data
   model and its per-axis policy.
4. `docs/adr/return-expr.md` — the receiver-relative return machinery
   (`Operator(RowOf)` is how `find` already projects to the row class).
5. `docs/adr/plugin-system.md` — manifest families, emit vs query hooks.

## Current state — exact anchors (verify with grep before editing)

Core-side DBIC residue to remove or generify:

| What | Where | Find it |
| --- | --- | --- |
| Fold-time ResultSet minting | `src/builder.rs` | `grep -n 'fn extract_resultset_parametric' src/builder.rs` |
| Its re-emittable dedup set | `src/builder.rs` | `grep -n 'parametric_emitted_refs' src/builder.rs` (struct field + clear-and-emit doc + the two insert sites) |
| Hardcoded default base + `+`-prefix expansion | `src/builder.rs` | `grep -n '"DBIx::Class' src/builder.rs` (the `DBIx::Class::ResultSet` fallback and the `DBIx::Class::{}` bare-name prefixing near the `load_components` helper) |
| Open-key arg emission ("type is the gate") | `src/builder.rs` | `grep -n 'fn emit_call_arg_key_accesses_open' src/builder.rs` |
| Parametric semantics read side (core-owned policy) | `src/file_analysis.rs` | `grep -n 'fn hash_key_class\|fn dispatch_class\|fn element_type' src/file_analysis.rs` — `ParametricType`'s accessors currently ENCODE DBIC's semantics ("type_args[0] is the row class") in core |
| Plugin as it stands | `frameworks/dbic.rhai` | already owns `arg_name_verbs`, `column_keyed_verbs`, `fluent_verbs`, `meta_methods`, accessor/relationship synthesis |

Pinned gaps this epic closes (both listed in `docs/ROADMAP.md` Hardening):

- **Custom resultset discovery** — `$schema->resultset('Users')` should
  resolve to `<SchemaNS>::ResultSet::Users` when that package exists,
  else default. Red-pinned by `goto_def_offers_custom_resultset_method`
  (grep for it in tests; it documents the expected behavior).
- **Column-key completion at `->search({ | })`** — goto-def through a
  typed key already works; `complete_keyval_args`
  (`grep -n 'fn complete_keyval_args' src/file_analysis.rs`) has no
  parametric-receiver branch. E2E pin: `e2e/dbic_parametric.lua`.

## Non-goals — do NOT do these

- Do NOT build the full type-system-encoding axis machinery
  (`docs/prompt-type-system-encoding.md` stays parked). The decision
  gate from the owner doc: a declarative manifest field sidesteps it.
  Phases 1–2 proved the manifest route works; stay on it.
- Do NOT port the plugin to Rhai-executed *fold* logic. Rhai hooks run
  at parse time only. Anything the worklist fold re-derives per
  iteration must be DATA the plugin declares (a manifest), consumed by
  a generic core pass. If you find yourself wanting to call Rhai from
  `fold_to_fixed_point`, stop — declare instead.
- Do NOT leave a name-keyed lookup table for DBIC methods in core
  (rule #10). Core dispatches on manifest-declared data only.
- Do NOT touch `load_components` parent registration — it stays core
  (generic mixin machinery, per the owner doc).

## Phase breakdown

### Phase A — `parametric_bases()` manifest (semantics move out)

**Goal:** the read-side policy "what does `Parametric{base, args}` mean"
comes from the plugin, not from `ParametricType`'s hardcoded accessors.

1. Add a manifest hook `parametric_bases()` to `FrameworkPlugin`
   (default empty) + the rhai read (one line now —
   `read_manifest_list::<ParametricBase>` in `src/plugin/rhai_host.rs`)
   + registry union (copy the `meta_methods()` plumbing shape, it is the
   freshest example: trait default → registry iterator → rhai field →
   struct init → `FrameworkPlugin for RhaiPlugin` impl).
2. Shape (serde struct in `src/plugin/mod.rs`):
   ```rust
   pub struct ParametricBase {
       pub base: String,                 // "DBIx::Class::ResultSet"
       pub hash_key_arg_class: Projection, // where column-key args resolve
       pub element_type: Projection,      // what ->find/element access yields
       pub dispatch_class: Projection,    // where methods dispatch
   }
   pub enum Projection { TypeArg(usize), SelfBase } // closed, tiny
   ```
3. Bake the union onto `FileAnalysis` (serde-`default` field, mirror
   `meta_methods` exactly: parts struct, both constructors, builder
   assignment at the `FileAnalysisParts` literal).
4. `ParametricType::hash_key_class` / `dispatch_class` / `element_type`
   become lookups against the baked table (pass it in, or store the
   resolved projections on the `ParametricType` at mint time —
   PREFER mint-time resolution: the value carries its own answers,
   consumers stay zero-argument. Check how many call sites each
   accessor has before choosing; if accessors are called from places
   without `FileAnalysis` access, mint-time is the only option).
5. `frameworks/dbic.rhai` declares the entry for
   `DBIx::Class::ResultSet` (`hash_key_arg_class: TypeArg(0)`,
   `element_type: TypeArg(0)`, `dispatch_class: SelfBase`).
6. **Acceptance:** all existing `parametric_resultset_tests.rs` tests
   pass unchanged; `grep -rn 'DBIx' src/file_analysis.rs` returns only
   comments/docs, no logic.

### Phase B — minting moves to a declarative manifest

**Goal:** delete `extract_resultset_parametric` from core; the fold's
re-emittable pass mints Parametric values from plugin-declared data.

1. New manifest hook `parametric_mints()` returning entries like:
   ```rust
   pub struct ParametricMint {
       pub verb: String,             // "resultset"
       pub base_default: String,     // "DBIx::Class::ResultSet"
       pub row_from_first_string_arg: bool,
       pub discover_base: Option<String>, // "{schema_ns}::ResultSet::{row}"
   }
   ```
   (Template string with two placeholders is fine; core substitutes
   `{schema_ns}` = the invocant class's namespace root and `{row}` =
   the resolved row-class tail, then checks `module_index` /
   workspace symbols for existence — that is the custom-resultset
   discovery, done generically.)
2. The generic fold pass replaces `extract_resultset_parametric`: same
   trigger conditions (method call whose verb matches a mint entry,
   invocant class known), same output witness shape, same
   `parametric_emitted_refs` clear-and-emit idempotency (KEEP the
   dedup set and its invariant comment — rename it if you like, but
   the worklist invariant it enforces must survive verbatim).
3. Delete the DBIC name literals from `builder.rs`. The `+`-prefix /
   `DBIx::Class::` bare-name expansion near `load_components` is
   component-loading (phase-1 territory, stays) — ONLY if it is used
   solely for parent registration. Verify with grep before deciding;
   if it also feeds parametric minting, split it.
4. **Acceptance:** `parametric_resultset_tests.rs` green;
   `gold-corpus/run.pl` shows no regression on the dbic rows (grep the
   fixtures for `dbic`); a new unit test proves a THIRD-PARTY rhai
   plugin (test fixture, not dbic.rhai) can mint a Parametric on its
   own verb — that's the proof the seam is generic, not a DBIC rename.

### Phase C — per-method projection completes

**Goal:** the owner doc's projection table fully declared by the plugin.

Current state: `search`/`search_rs` preserve via `fluent_verbs`;
`find`-family projects via the `RowOf` ReturnExpr operator (verify:
`grep -n 'RowOf' src/witnesses.rs frameworks/dbic.rhai`). Missing:
`all`/`slice` (ArrayRef-of-row — plain `ArrayRef` is acceptable, note
the honest loss), `count`/`exists`/`update`/`delete` (Numeric).

1. Extend the plugin's declarations so every row of the table in
   `prompt-dbic-as-plugin.md` §"Per-method return-type projection" is
   covered. Use the EXISTING seams: `overrides()` with a class-scoped
   method target if it supports that, else the same `ReturnExpr`
   publication path `RowOf` rides. Do not invent a third mechanism if
   either existing one fits.
2. **Acceptance:** unit tests: `$rs->count` types Numeric,
   `$rs->all` types ArrayRef, `$rs->find(1)->name` still resolves the
   column accessor (regression), `$rs->search({...})->count` chains.

### Phase D — the two pinned gaps

1. Custom-resultset discovery lands automatically with Phase B's
   `discover_base` — flip `goto_def_offers_custom_resultset_method`
   from red pin to green assertion.
2. `complete_keyval_args` parametric branch: when the receiver types
   `Parametric` and the verb is column-keyed, complete the row class's
   column keys (ask the typed receiver via `hash_key_arg_class`, then
   the row class's `HashKeyDef`s — cross-file via the same lookup
   goto-def already uses; DO NOT write a parallel reverse index,
   rule #8). Verify with `e2e/dbic_parametric.lua` (CI runs it; note
   in the PR that e2e needs nvim).
3. **Acceptance:** a gold-corpus completion row for
   `->search({ | })` column keys — author it with
   `gold-corpus/run.pl --emit completion <file> <row> <col>` against
   the substrate, status `gold`.

### Phase E — deletion sweep + verification

1. `grep -rn 'DBIx' src/` → only comments and generic examples remain.
2. Bump `EXTRACT_VERSION` (`src/module_cache.rs`) — FA shape changed
   (new baked table) and bag rules changed.
3. Full gate: `cargo test` (all green), `./e2e/run.sh` (or CI),
   `gold-corpus/run.pl` (0 FAIL, 0 XPASS — if XPASS, promote the row
   per `gold-corpus/README.md`), and the substrate diagnostic audit:
   ```
   perl-lsp --clear-cache gold-corpus/local/lib/perl5
   perl-lsp --check gold-corpus/local/lib/perl5 --format json --severity hint \
     --optional-deref --redundant-guard --deref-shape --unresolved-method-cross-file
   ```
   Diff per-code counts against a main-branch binary (build one in a
   worktree). Always-on `undef-deref` must be at exact parity.
4. Update `docs/prompt-dbic-as-plugin.md` (mark phase 3 landed, keep
   the honest residuals — e.g. prefetch `join =>` key extension stays
   out) and `docs/ROADMAP.md`.

## Invariants that MUST survive

- Rule #1: only `build()` walks the tree. Manifest data is consumed by
  existing walk/fold passes; no new tree consumers.
- Worklist: any pass that pushes witnesses per fold iteration is
  clear-and-emit under a source tag (see `witnesses::tags` and the
  "Re-emittable passes" bullet in CLAUDE.md). New tags go in
  `witnesses::tags`, never as inline literals.
- Edges, not values: if the mint can point at an existing attachment,
  push an Edge, not a materialized type.
- Provenance: minted Parametrics should keep whatever
  `TypeProvenance` trail the current code records (grep
  `type_provenance` near the mint) so `--dump-package` stays honest.

## Sizing & sequencing

A → B → C → D → E strictly in order; A and C are independent of each
other but both precede D. Each phase is one reviewable commit. Expect
A+B to be the bulk (~2/3 of the work).
