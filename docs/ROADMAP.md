# perl-lsp Roadmap

Landed work lives in `docs/adr/` and `CHANGELOG.md` — never here.
This file is only what's NEXT, in order.

## Now — the scheduled epics (in order)

Each epic has a self-contained implementation prompt under
`docs/epics/` — mission, exact code anchors, phase ladder with
acceptance criteria, invariants, and the verification gate. Implementers
start THERE (after CLAUDE.md); this section is only the schedule.

1. **Epic 1 — DBIC out of core, phase 3** →
   `docs/epics/01-dbic-phase-3.md`. Parametric emission + per-base
   semantics move to declarative plugin manifests
   (`parametric_bases()` / `parametric_mints()`); the per-method
   projection table completes; the two pinned gaps close
   (custom-resultset discovery, `->search({ | })` column completion).
   Ends with core plugin-free except generic dispatch — the phase
   ladder's end state. Design corpus: `prompt-dbic-as-plugin.md`.

2. **Epic 2 — Openness** → `docs/epics/02-openness.md`. One structural
   verdict ("walk the namespace chain; Open suppresses, exhausted
   Closed warns") subsumes the pile of per-diagnostic suppression
   rules, resolves `SUPER::`/qualified names instead of skipping them,
   gates the D4 open-world-dispatch noise, and then executes the flag
   promotion the audit evidence below supports. Design corpus:
   `prompt-graph-walking.md` (Scope nodes), `open-problems.md`
   (qualified-name suppression), `adr/narrowing-diagnostics.md`
   (promotion path).

   *Promotion audit over the gold substrate (July 2026, all flags on,
   after the disagreement-to-widen tier landed) — the evidence Epic 2
   §Phase E consumes:* `undef-deref` (always-on) 8 sites — exact parity
   with the pre-branch baseline; `derefShape` 0 hits; `optionalDeref`
   35 (honest Optional productions; residual noise = value flow beyond
   static reach, e.g. an arity-and-value-gated undef arm in
   `Path::Class::Dir::new`); `redundantGuard` 59 (was 115) and
   `contradictory` 34 (was 53) after reassignment barriers + the
   shift-invocant gate; `unresolved-method` −180 from the same fixes.
   Remaining known noise classes: open-world dispatch (Epic 2 Phase D)
   and the named-helper first-param-self over-reach
   (`gold-corpus/KNOWN-GAPS.md`). `optionalDeref` looks promotable at
   INFO severity now; `redundantGuard` after the Phase D gate;
   `unresolvedMethodCrossFile` still promotes last.

3. **Epic 3 — Value provenance, tier 1** →
   `docs/epics/03-value-provenance.md`. Residual fact classes Parts 1,
   2, and 5a (invocant-mutation consumers, hash-key unions,
   value-indexed returns) as emitter+reducer pairs on the bag. The
   named gate for un-parking instance brands and the untyped-receiver
   residual. Design corpus: `prompt-type-inference-residual.md`.

## On deck — Epics 4–12

The full slate, one implementation prompt per epic, lives in
`docs/epics/` — see `docs/epics/README.md` for the schedule table AND
the coverage map that accounts for every `prompt-*.md` and open design
item (scheduled / parked-with-condition / landed / out-of-scope).

4. One-seam sweep: magic tokens + cst backlog → `epics/04`
5. Duplicate-package identity (H1) → `epics/05`
6. Gated cross-file emission (ClassIsa) → `epics/06`
7. Rename provenance → `epics/07`
8. Diagnostic framework: PL-codes, config, SARIF → `epics/08`
9. Heatmap residuals: Handlers + framework-consumed → `epics/09`
10. Mojo polish: routes, stash, hooks, chains → `epics/10`
11. CLI analysis subcommands + `--migrate` → `epics/11`
12. Program boundaries + MAIN-1 → `epics/12`

## Queued (pull-driven — QA findings decide order)

Type intelligence:
- Residual fact classes Parts 3–4 (method loops, functional
  operators) and the 5c prefetch residual — after Epic 3;
  `prompt-type-inference-residual.md`.
- Constructor/field value flow — the remaining instance-brands
  prerequisite once Epic 3 lands (`prompt-graph-walking.md` §PARKED).
- Conditional-reassignment disagreement-to-widen — the widening tier
  LANDED (reassign barriers: an untypeable write drops earlier beliefs;
  a postfix-conditional write never asserts, only widens). Residual:
  the true JOIN for typed conditional writes (`$spec = {...} unless ref
  $spec` could keep `HashRef|prior` instead of widening to unknown) —
  that's the real lattice fold, wanted when precision (not soundness)
  becomes the complaint.
Graph / diagnostics: the Openness axis moved to Now (Epic 2). Residual
after it: `Symbol.home_namespace` field migration if a consumer beyond
the diagnostic ever needs it (Epic 2 deliberately ships the query, not
the field).

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
- Options schema → **Epic 8** (`prompt-config-schema.md`'s forcing
  function).
- Fold safety net: `eprintln!` → `tracing::error!` (builder.rs
  ~12061) + a synthetic-oscillator test so the release-mode
  `MAX_FOLD_ITERATIONS` break can't bit-rot.
- Full-bag scans in `apply_chain_typing_assignments` /
  `FileAnalysis::inferred_type` — index when profiling flags them.
- DBIC parametric column-key completion at `->search({ | })` →
  **Epic 1** Phase D.
- Cursor-context qualified-path/invocant detection should ask the
  tree, not byte-walk (`extract_package_from_prefix` & sibling) —
  adjacent to Epic 4's item-3 collapse; fold in there if touching.
- `return_via_edge` chases lack `TypeProvenance` (stamp
  `Delegation{kind: "callable_return_edge"}` on the chase).
- cst/conventions migration backlog — ranked items → **Epic 4**; the
  long tail stays the strangler rule (`prompt-cst-migration.md`).
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
- MAIN-1 → **Epic 12**; H1 → **Epic 5** (designs in
  `qa-design-items.md`). MooseX::Role::Parameterized — parked: the
  runtime-export-generator open problem wearing role clothes.
- Per-row known gaps: `gold-corpus/KNOWN-GAPS.md` (xfail rows are the
  live tracker).

## Parked (explicit unblock conditions)

- **Instance brands** — per-object dispatch scoping (`$app->minion`
  vs `$app->other_minion`, two Mojo::Lite apps in one workspace).
  Spiked and closed (PRs #65/#66, branches `branded-edges` /
  `branded-edges-accessor`); MUST NOT be rebuilt the syntactic-name
  way (rule #10 — aliasing breaks it). A downstream consumer of the
  long-distance value-provenance tier (`prompt-type-inference-residual.md`
  Parts 1–5); the birth-site design lives in `prompt-graph-walking.md`.
- **Re-export chains** — branch `worktree-agent-aae99d42f4d5d74bc`
  (correct in isolation; design in `adr/reexport-surface.md` on the
  branch). Blocked on the ts-parser-perl X1 scanner thread-safety fix
  (`parser-shortcomings.md`). On rework: rebase, confirm no
  Bugzilla-cold abort, re-verify Test::Most → Test::More end-to-end.
- **Sequence-types phases** — QA pulls; `prompt-sequence-types.md`.
- **Type-system encoding** (axis dispatch) — waits for the full axis
  set; graph walking informs it. `prompt-type-system-encoding.md`.
- **Type-is-the-gate generalization** — waits for a second motivating
  site. `prompt-type-is-the-gate.md`.

## Backburner (user-facing, ship-when-ready)

- Mojo polish → **Epic 10**; CLI diagnostic framework → **Epic 8**;
  analysis subcommands + `--migrate` → **Epic 11**; ref provenance →
  **Epic 7** (all scheduled — see `docs/epics/README.md`).
- Aspirational type features (effects/throws) —
  `prompt-type-system-futures.md` (its narrowing pillar landed as
  `adr/flow-narrowing.md`; only the effects pillar remains).
- Web extension — `prompt-wasm-web-extension.md` (the crate split it
  assumed was executed and REJECTED; branch `workspace-split` is the
  playbook if wasm ever forces it).
- Multi-language engine — proven in spikes; design + working packs on
  branch `worktree-query-extraction-spike`
  (`docs/prompt-multi-language.md` there).

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
