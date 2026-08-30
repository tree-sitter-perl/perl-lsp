# perl-lsp Roadmap

Landed work lives in `docs/adr/` and `CHANGELOG.md` — never here.
This file is only what's NEXT, in order.

> **`spike/cpp-support` branch:** the multi-language (cpp/python) go-live arc has
> its own altitude map — `docs/cpp-golive-map.md`. The Flow/value-flow tier
> (FlowEdge spine, query-driven assignment shapes, narrowing-on-edges) lives
> there; it's shared seam, not Perl-specific. Consult it before zooming into a
> Flow slice.

## Now (in order)

1. **Narrowing / Optional — completeness.** The flow-narrowing +
   `Optional<T>` + `Undef` lattice and the bug-detection diagnostics it
   feeds have both landed (decision records: `adr/flow-narrowing.md`,
   `adr/optional-types.md`, `adr/narrowing-diagnostics.md`). What remains
   is **completeness** — widen what the narrower recognizes: direct-element
   places (`$hash{key}`, `$arr[0]`), and dynamic-key places (`$self->{$k}`)
   where the key scalar is stable enough to stay sound
   (`prompt-flow-narrowing.md` / `prompt-optional-types.md`) — and graduate
   the opt-in diagnostic flags to default-on per code as the gold substrate
   and real projects show no false-positive flood (the promotion path in
   `adr/narrowing-diagnostics.md`).
2. **DBIC out of core — phases 2–3.** Phase 1 landed (`visit_dbic_*`
   gone; `frameworks/dbic.rhai`, trigger `ClassIsa("DBIx::Class")`).
   Remaining: meta-method suppression → manifest (the `universal_methods`
   rule-#10 debt still hardcoded in `symbols.rs`) and parametric
   emission + per-method return projection (the one axis-shaped piece).
   Ladder in `prompt-dbic-as-plugin.md`. Ends with core plugin-free
   except generic dispatch.

## Queued (pull-driven — QA findings decide order)

Type intelligence:
- Residual fact classes Parts 1–5 (invocant mutations, hash-key
  unions, method loops, functional operators, value-indexed returns)
  — `prompt-type-inference-residual.md`.
- Conditional-reassignment disagreement-to-widen (`$spec = {...}
  unless ref $spec`) — replaces the `reassigned_scalars` trust-gate
  clause with a real lattice fold.
- A4 v2: cross-FILE slot writes (`$self->{k} = Obj->new` in another
  file) — the `PackageSymbol` bridge pattern.

Graph / diagnostics (graph-walking pillar landed; residual only):
- Scope-node taxonomy + Openness diagnostic (`home_namespace`,
  "when is an unresolved call real?") — forward work in
  `prompt-graph-walking.md`; subsumes the coarse qualified-name
  suppression noted in `open-problems.md`.

Plugin genericity:
- `has_options` final dissolution: the option pairing already moved out
  of core — the plugin reads accessor options via the shared
  `classified_pairs` over the flattened, per-arg `value_shape`-classified
  args. The one Moo-semantic field still in core is the
  `isa`-string→`InferredType` mapping; moving it onto the
  `type_constraint_names()` / `type_constraint_inner()` plugin seam is the
  last step, after which `HasOptions` dissolves entirely (attr names come
  from `value_shape`/`arg_names`, options from `classified_pairs`).

Hardening:
- Options schema: `DiagnosticOptions` is serde-driven (the struct is the
  schema). A `Config` god-struct (own-at-top, pass-slices), a generated
  editor schema (`schemars`), and the per-code-config shape wait for their
  forcing functions — `prompt-config-schema.md`.
- Fold safety net: `eprintln!` → `tracing::error!` (builder.rs
  ~12061) + a synthetic-oscillator test so the release-mode
  `MAX_FOLD_ITERATIONS` break can't bit-rot.
- Full-bag scans in `apply_chain_typing_assignments` /
  `FileAnalysis::inferred_type` — index when profiling flags them.
- DBIC parametric column-key completion at an empty `->search({ | })`
  (goto-def proves the chain; `complete_keyval_args` lacks the
  parametric-receiver branch; pin in `e2e/dbic_parametric.lua`).
- Cursor-context qualified-path/invocant detection should ask the
  tree, not byte-walk (`extract_package_from_prefix` & sibling).
- `return_via_edge` chases lack `TypeProvenance` (stamp
  `Delegation{kind: "callable_return_edge"}` on the chase).
- cst/conventions migration backlog — `prompt-cst-migration.md`.
- Unify autoquoted-key-as-literal into `cst::string_list`. Today
  `string_list` routes `autoquoted_bareword` through the caller's
  `fold` (const resolution), so the DSL-arg callers (`extract_arg_name_list`)
  carry a per-caller fold that special-cases autoquoted→literal. An
  autoquoted bareword is a grammar-certified literal for *every* caller,
  so the right home for the rule is `string_list` itself — then
  `extract_arg_name_list` deletes and the DBIC/keyval paths just use
  `extract_string_list`. **Blocked on** a latent use-import bug it
  unmasks: `use constant NAME => v`'s autoquoted key gets emitted as a
  spurious `FunctionCall` import ref (resolved_package `"constant"`) by
  the use-list walker — the old fold hid it by dropping non-constant
  barewords. Regression-guarded by `const_call_form_not_double_reffed`.
  Fix the use-`constant` path to not feed its declared names to the
  generic import-ref emitter (it already routes them to
  `accumulate_use_constant`), THEN move the autoquoted arm into
  `string_list` and drop the per-caller fold. Proper unification; not
  urgent (the per-caller fold is correct, just not DRY).

QA tail:
- MAIN-1 (`main::` across `require`) and H1 (duplicate packages) —
  designs in `docs/open-problems.md`. MooseX::Role::Parameterized — no
  design yet.
- Per-row known gaps: `gold-corpus/KNOWN-GAPS.md` (xfail rows are the
  live tracker).

Protocol surface (breadth, not depth):
- **Advertised verb surface** — a capability listing reads the
  `initialize` response, not the answers, so verbs the analysis can
  already answer should be advertised. The cluster worth doing is type
  hierarchy + call hierarchy + typeDefinition: all three are projections
  over machinery that already exists (`GraphView`, the `references()`
  projection, `dispatch_class()`). Non-goals and the reasoning are in
  `prompt-lsp-surface-parity.md`; `linkedEditingRange` stays OFF (#117).

## Scale validation (2026-08-17) — the Tier 1 queue

The first measurements outside `crm`: a 4.65 h soak, Koha (3.1x), and a
5,000-dist CPAN sample (122x — the target rung). Storage and startup hold
scale-free; query paths break. Findings, tiers, corpora and repros:
`prompt-scale-validation-hitlist.md`. Tier 1, in order:

1. **Post-cold-index availability hole** — ~10 min where every verb times
   out; a warm restart of the same state is ready in 1 s. Restarting beats
   staying up.
2. **Fatal stack overflow on deep CSTs (P0)** — one XML-as-`.pm` aborts the
   whole server; `catch_unwind` cannot catch it. Depth gate before build.
3. **`references` terminal at scale** — no refs-axis reader, so the backward
   walk decodes a whole blob per candidate (~4x of the cost).
4. **Completion payload unbounded** — 7.8 MB / ~50k items per keystroke.

## Parked (explicit unblock conditions)

- **Instance brands** — per-object dispatch scoping (`$app->minion`
  vs `$app->other_minion`, two Mojo::Lite apps in one workspace).
  Spiked and closed (PRs #65/#66, branches `branded-edges` /
  `branded-edges-accessor`); MUST NOT be rebuilt the syntactic-name
  way (rule #10 — aliasing breaks it). A downstream consumer of the
  long-distance value-provenance tier (`prompt-type-inference-residual.md`
  Parts 1–5); the birth-site design lives in `prompt-graph-walking.md`.
- **Sequence-types phases** — QA pulls; `prompt-sequence-types.md`.
- **Type-system encoding** (axis dispatch) — waits for the full axis
  set; graph walking informs it. `prompt-type-system-encoding.md`.
- **Type-is-the-gate generalization** — waits for a second motivating
  site. `prompt-type-is-the-gate.md`.

## Backburner (user-facing, ship-when-ready)

- Mojo polish: route naming/url_for, stash intelligence, hooks,
  transitive plugin chains, config completion —
  `prompt-mojo-todo.md`.
- CLI diagnostic framework (PL-codes, suppression, SARIF), --migrate —
  `prompt-cli-tools.md`.
- Ref provenance: constant-fold `folded_from`, package→file rename,
  inheritance override scoping — `prompt-ref-provenance.md`.
- Aspirational type features (effects/throws) —
  `prompt-type-system-futures.md`.
- Web extension — `prompt-wasm-web-extension.md` (the crate split it
  assumed was executed and REJECTED; branch `workspace-split` is the
  playbook if wasm ever forces it).
- Multi-language engine — the go-live arc (cpp/python) is live on
  mainline (`build/language_driver.rs`, altitude map in
  `docs/cpp-golive-map.md`); design in `docs/prompt-multi-language.md`.
- PHP — the designated next serve-in-anger language: pack skeleton
  landed (`--features php`); market case + build-out sequencing in
  `docs/prompt-php-target.md`.

## Out of scope

Multi-workspace/monorepo · cross-file rename of deps (read-only by
`RoleMask::EDITABLE`) · effect facts · full dependent inference ·
`wantarray` returns · cross-function scalar aliasing · runtime
namespace extension (graph-gated).

## Reading order for someone joining

1. `CLAUDE.md` — live architecture. Source of truth.
2. `docs/adr/*.md` — load-bearing decisions for landed work.
3. This roadmap.
4. `docs/open-problems.md` — the deliberate deferrals.
5. The `prompt-*.md` for your workstream.
6. `gold-corpus/README.md` + `KNOWN-GAPS.md` — the regression net.
