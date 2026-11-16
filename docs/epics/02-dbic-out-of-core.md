# Epic 2 — DBIC out of core, phases 2–3 (meta-methods, parametric emission, projection)

> **Status:** Phase A landed (audited 2026-11-12); Phases B–F open.
> Finishes the standing "core is plugin-free except generic dispatch"
> commitment.
> **Design owner-doc:** `docs/prompt-dbic-as-plugin.md` (read it whole —
> the phase ladder, the projection table, the open questions).

## Mission

Move the last DBIC-specific machinery out of the builder and into the
plugin layer, so `frameworks/dbic.rhai` (plus generic core seams) owns
everything DBIC-shaped. Phase 1 landed: accessor/relationship
synthesis, arg-name verbs, column-keyed verbs and fluent verbs live in
the plugin (`frameworks/dbic.rhai` declares `column_keyed_verbs`,
`fluent_verbs`, `column_actions`, `relationship_method`). What remains
in core is the **meta-method suppression list**, the **parametric
ResultSet minting**, and the **hardcoded semantics of
`DBIx::Class::ResultSet`** — plus two pinned user-facing gaps that only
make sense to fix on the plugin side of the move.

## Read first, in this order

1. `CLAUDE.md` — rules #1 (tree traversal only in `build()`), #8
   (plugin-synthesized content), #10 (never special-case shapes),
   "Worklist invariants", "Type inference (witness bag)".
2. `docs/prompt-dbic-as-plugin.md` — the design, including the
   per-method projection table and the `parametric_semantics()` sketch.
3. `docs/adr/parametric-types.md` — the sealed-flavor Parametric data
   model and its per-axis policy.
4. `docs/adr/cpp-templates.md` — **read this even though the epic is
   about DBIC.** `ParametricType` is shared with C++ template
   instantiation; see the Language-pack beat below.
5. `docs/adr/return-expr.md` — the receiver-relative return machinery
   (`Operator(RowOf)` is how `find` already projects to the row class).
6. `docs/adr/plugin-system.md` — manifest families, emit vs query hooks.

## Current state — exact anchors (verify with grep before editing)

| What | Where | Find it |
| --- | --- | --- |
| Fold-time ResultSet minting | `src/build/builder/visit_method.rs` | `grep -rn 'extract_resultset_parametric' src/build/` |
| Its re-emittable dedup set | `src/build/builder/` | `grep -rn 'parametric_emitted_refs' src/build/` (struct field + clear-and-emit doc + insert sites) |
| Hardcoded DBIC class names in the model | `src/model/file_analysis/invocants.rs` | `grep -n 'DBIx::Class' src/model/file_analysis/invocants.rs` — the result-class predicate encodes `DBIx::Class::Core` / `::Row` / `::Schema` / `::ResultSet` |
| Parametric semantics read side | `src/model/file_analysis/types.rs` | `grep -n 'fn hash_key_class\|fn dispatch_class\|fn element_type' src/model/file_analysis/` — these accessors ENCODE DBIC's semantics ("type_args[0] is the row class") in core |
| Plugin as it stands | `frameworks/dbic.rhai` | `column_keyed_verbs`, `fluent_verbs`, `column_actions`, `relationship_method`, `on_match` |
| Manifest plumbing template | `src/build/plugin/mod.rs` + `rhai_host.rs` | pick the freshest landed manifest family and copy its five touch points: trait default → registry iterator → rhai read → struct init → `FrameworkPlugin for RhaiPlugin` impl |
| Where a baked table lands | `src/model/file_analysis/plugin_facts.rs` | the plugin lane — a new baked manifest union goes IN here, never beside it |

Pinned gaps this epic closes:

- **Custom resultset discovery** — `$schema->resultset('Users')` should
  resolve to `<SchemaNS>::ResultSet::Users` when that package exists,
  else default. Red-pinned by `goto_def_offers_custom_resultset_method`.
- **Column-key completion at `->search({ | })`** — goto-def through a
  typed key already works; `complete_keyval_args` has no
  parametric-receiver branch. E2E pin: `e2e/dbic_parametric.lua`.

## Non-goals — do NOT do these

- Do NOT build the full type-system-encoding axis machinery
  (`docs/prompt-type-system-encoding.md` stays parked). Phase 1 proved
  the declarative-manifest route works; stay on it.
- Do NOT port the plugin to Rhai-executed *fold* logic. Rhai hooks run
  at parse time. Anything the worklist fold re-derives per iteration
  must be DATA the plugin declares, consumed by a generic core pass. If
  you want to call Rhai from `fold_to_fixed_point`, stop — declare
  instead.
- Do NOT leave a name-keyed lookup table for DBIC methods in core
  (rule #10).
- Do NOT touch `load_components` parent registration — generic mixin
  machinery, stays core.

## Phase breakdown

### Phase A — `meta_methods()` manifest (re-absorbed phase 2)

**Landed.** The manifest is baked onto `FileAnalysis.plugin`
(`plugin_facts.rs`); `frameworks/dbic.rhai` and `frameworks/moo.rhai`
declare their entries; the unresolved-method diagnostic keeps only the
true `UNIVERSAL::` list inline and chains `analysis.plugin.meta_methods`
(`lsp/symbols/diagnostics.rs`).

**Goal:** the "methods a framework gives every class of its family"
list leaves `diagnostics.rs`.

1. New manifest hook `meta_methods()` on the plugin trait (default
   empty) + the rhai read + registry union, baked onto
   `FileAnalysis.plugin` (`plugin_facts.rs`, serde-default).
2. `frameworks/dbic.rhai` and `frameworks/moo.rhai` declare their own
   entries. Core keeps ONLY the true `UNIVERSAL::` surface
   (`new`/`AUTOLOAD`/`DESTROY`/`can`/`isa`/`DOES`).
3. The unresolved-method diagnostic consults the baked union.
4. **Acceptance:** existing diagnostics tests green unchanged;
   `grep -n 'DBIx' src/lsp/symbols/diagnostics.rs` returns nothing;
   substrate audit at exact parity (this is a pure move — any count
   that moves means the lists were not equivalent, so reconcile before
   merging).

### Phase B — `parametric_bases()` manifest (semantics move out)

**Goal:** the read-side policy "what does `Parametric{base, args}`
mean" comes from the plugin, not from `ParametricType`'s accessors.

1. Manifest shape (serde struct in `src/build/plugin/mod.rs`):
   ```rust
   pub struct ParametricBase {
       pub base: String,                   // "DBIx::Class::ResultSet"
       pub hash_key_arg_class: Projection, // where column-key args resolve
       pub element_type: Projection,       // what ->find / element access yields
       pub dispatch_class: Projection,     // where methods dispatch
   }
   pub enum Projection { TypeArg(usize), SelfBase } // closed, tiny
   ```
2. `hash_key_class` / `dispatch_class` / `element_type` become lookups
   against the baked table. **Prefer mint-time resolution** — store the
   resolved projections on the `ParametricType` when it is minted, so
   the value carries its own answers and consumers stay zero-argument.
   Check the call sites first: if any accessor is called from a place
   without `FileAnalysis` access, mint-time is the only option.
3. `frameworks/dbic.rhai` declares its entry (`hash_key_arg_class:
   TypeArg(0)`, `element_type: TypeArg(0)`, `dispatch_class: SelfBase`).
4. **Acceptance:** `parametric_resultset_tests.rs` passes unchanged;
   `grep -rn 'DBIx' src/model/` returns only comments — plus the
   Language-pack beat's C++ regression check below.

### Phase C — minting moves to a declarative manifest

**Goal:** delete `extract_resultset_parametric` from core.

1. Manifest `parametric_mints()`:
   ```rust
   pub struct ParametricMint {
       pub verb: String,                  // "resultset"
       pub base_default: String,          // "DBIx::Class::ResultSet"
       pub row_from_first_string_arg: bool,
       pub discover_base: Option<String>, // "{schema_ns}::ResultSet::{row}"
   }
   ```
   Core substitutes `{schema_ns}` (the invocant class's namespace root)
   and `{row}` (the resolved row-class tail), then checks the index /
   workspace symbols for existence — that is custom-resultset
   discovery, done generically.
2. The generic fold pass replaces `extract_resultset_parametric`: same
   trigger conditions, same witness shape, same clear-and-emit
   idempotency. **KEEP the dedup set and its invariant comment** —
   rename it if you like, but the worklist invariant it enforces must
   survive verbatim.
3. Delete the DBIC name literals from the builder. The `+`-prefix /
   `DBIx::Class::` bare-name expansion near `load_components` is
   component loading and stays — *verify with grep* that it does not
   also feed parametric minting; if it does, split it.
4. **Acceptance:** `parametric_resultset_tests.rs` green; gold dbic
   rows unmoved; **a new unit test proving a THIRD-PARTY rhai plugin
   (a test fixture, not `dbic.rhai`) can mint a Parametric on its own
   verb** — that is the proof the seam is generic, not a DBIC rename.

### Phase D — per-method projection completes

`search`/`search_rs` preserve via `fluent_verbs`; the `find` family
projects via the `RowOf` `ReturnExpr` operator. Missing: `all`/`slice`
(ArrayRef-of-row — plain `ArrayRef` is acceptable; note the honest
loss), `count`/`exists`/`update`/`delete` (Numeric).

Use the EXISTING seams — `overrides()` with a class-scoped method
target, or the same `ReturnExpr` publication path `RowOf` rides. Do not
invent a third mechanism if either fits. **Acceptance:** `$rs->count`
types Numeric; `$rs->all` types ArrayRef; `$rs->find(1)->name` still
resolves the column accessor; `$rs->search({...})->count` chains.

### Phase E — the two pinned gaps

1. Custom-resultset discovery lands with Phase C's `discover_base` —
   flip `goto_def_offers_custom_resultset_method` from red pin to green.
2. `complete_keyval_args` parametric branch: when the receiver types
   `Parametric` and the verb is column-keyed, complete the row class's
   column keys (ask the typed receiver via `hash_key_arg_class`, then
   the row class's `HashKeyDef`s, cross-file via the lookup goto-def
   already uses — **no parallel reverse index**, rule #8).
3. **Acceptance:** a gold completion row for `->search({ | })` column
   keys, authored with `gold-corpus/run.pl --emit completion`, status
   `gold`; `e2e/dbic_parametric.lua` green.

### Phase F — deletion sweep

`grep -rn 'DBIx' src/` → only comments and generic examples. Bump
`EXTRACT_VERSION` (FA shape and bag rules both changed). Update
`docs/prompt-dbic-as-plugin.md` (phases 2–3 landed, honest residuals
kept — prefetch `join =>` key extension stays out) and this README's
coverage map.

## Language-pack beat

**`ParametricType` is not a DBIC type. It is the engine's parametric
model, and C++ templates are its other tenant** — which makes Phase B
the riskiest phase in this epic and the one most likely to break a
language nobody working on it is thinking about.

What rides the same shape (`docs/adr/cpp-templates.md`, slice c):
`ParametricType::Instance` with exact-spelling dispatch; lazy
`ParamOf` / `InstanceOf` receiver substitution *beside* `RowOf`;
`substitute_type_params` for fields; the partial-pattern
spec-selection ladder (exact > partial > primary) with
`match_template_pattern` binding a spec's params from the concrete
spelling.

Concrete obligations:

1. **`hash_key_class` / `dispatch_class` / `element_type` must keep
   answering for a C++ instance**, which has no plugin manifest behind
   it. A pack language's `Parametric` values are minted by the
   extractor, not by a rhai plugin. So the baked table is a *lookup
   with a default*, and the default must be exactly today's behavior —
   not "empty means no answer".
2. Prefer mint-time resolution (Phase B step 2) partly for this
   reason: a value minted by the cpp extractor carries the projections
   the extractor knows, and never consults a Perl plugin table at all.
3. **Run the C++ suite on Phase B.** `cargo test --features cpp` and
   the gold harness built `--features cpp` with `lang-skip 0` — a
   plain release build lang-skips half the corpus and reports it as
   skips, not failures, which is exactly how this regression would
   escape.
4. The generic-seam proof in Phase C (a third-party plugin minting a
   Parametric on its own verb) is also the pack-language proof: if the
   mint seam only works for a rhai plugin, a pack language that wants
   framework-shaped parametric minting has to grow a second path.

## Scaling beat

This epic adds **two baked manifest unions to every `FileAnalysis`**
(`meta_methods`, `parametric_bases`, plus `parametric_mints`). That is
cheap per file and not cheap per corpus, so:

1. **They go in `plugin_facts.rs`**, the plugin lane, which is
   `#[serde(default)]` and default-empty. A Perl analysis with no
   matching plugin carries an empty sub-struct, not three empty fields
   in the top-level struct — this is the lane discipline CLAUDE.md
   describes, and it is why `PackFacts`/`PluginFacts` exist.
2. **`surface_feed` will not compile** until each new field's Surface
   fate is decided (it destructures every field with no `..`). These
   are *baked manifest data*, identical for every file a given plugin
   set analyses — they are **not** cross-file-visible surface, and
   including them would make every file's Surface differ on a plugin
   edit rather than on a real change. Discard them with a reason, in
   the code.
3. **Each new field joins a `heap_estimate` bucket.** A per-file
   manifest copy at 138k files is real: the CPAN-5k corpus is 138,822
   files with a 1.73 GB `modules.db` at 13.9 KB/file (measured
   2026-08-17). A few hundred bytes of duplicated manifest per file is
   ~40 MB of database. If the union is large, intern it or key it by
   plugin-set fingerprint rather than copying it per file — but
   measure first; do not pre-optimize a 20-entry table.
4. **`EXTRACT_VERSION` bump means a full cold re-index for every user.**
   Bundle Phases A–F behind one bump if they land close together
   rather than forcing three. The cold bulk index for CPAN-5k is
   ~10.5 minutes (2026-08-17); that is what a bump costs someone with a
   large workspace.
5. Phase C's `discover_base` does an **existence check against the
   index per mint**. On a fold that re-emits per iteration, that is a
   per-iteration index consult. Cache the verdict on the dedup set that
   already exists (`parametric_emitted_refs`) rather than adding a
   second memo, and confirm with `--timings` on the substrate that the
   slowest-modules tail does not move.

## Invariants that MUST survive

- Rule #1: only `build()` walks the tree. Manifest data is consumed by
  existing walk/fold passes; no new tree consumers.
- Re-emittable passes are clear-and-emit under a source tag; new tags
  go in `witnesses::tags`, never inline.
- Edges, not values: if the mint can point at an existing attachment,
  push an `Edge`, not a materialized type.
- Minted Parametrics keep their `TypeProvenance` trail so
  `--dump-package` stays honest.

## Verification gate

`cargo test` **and** `cargo test --features cpp` · gold 0 FAIL /
0 XPASS, built `--features cpp`, `lang-skip 0` confirmed in the summary ·
`./e2e/run.sh` · substrate audit with always-on `undef-deref` at exact
parity · `--timings` tail unmoved beyond noise.

## Sizing & sequencing

A → B → C → D → E → F. A is small and independently shippable; B and C
are the bulk (~2/3). D is independent of B/C's ordering but precedes E.
