# perl-lsp Roadmap

Landed work lives in `docs/adr/` and `CHANGELOG.md` — never here.
This file is only what's NEXT, in order.

## The schedule lives in `docs/epics/`

Open work is organized as **epics** — self-contained implementation
prompts with anchors, phased ladders, per-phase acceptance criteria,
non-goals and a verification gate. `docs/epics/README.md` is the index,
the schedule, and the coverage map that accounts for every open design
doc.

Each epic carries three axes, not one: its own ladder, plus a
**Language-pack beat** (what it owes C/C++ and the other pack languages)
and a **Scaling beat** (the measured cost it must respect). Those two
are not a separate workstream — this is a multi-language engine with a
measured scaling envelope, and both are properties of every seam.

| # | Epic | Why now |
|---|---|---|
| 1 | [Provider identity](epics/01-provider-identity.md) | A class of confidently-wrong answers, not misses |
| 2 | [DBIC out of core](epics/02-dbic-out-of-core.md) | Finishes "core is plugin-free except generic dispatch" |
| 3 | [Openness](epics/03-openness.md) | One verdict replaces six partial suppression rules; unlocks flag promotion |
| 4 | [Value provenance, tier 1](epics/04-value-provenance.md) | The named gate for instance brands and the untyped-receiver residual |
| 5 | [One-seam sweep](epics/05-one-seam-sweep.md) | Small, self-contained; the good warm-up |
| 6 | [Rename provenance](epics/06-rename-provenance.md) | `folded_from` landed; three residuals left |
| 7 | [Diagnostic framework](epics/07-diagnostic-framework.md) | The CI-readiness gate |
| 8 | [Heatmap: parallelize + residuals](epics/08-heatmap-residuals.md) | Runs on one core (up to 17x `--check`); then a verified FP against its own promise |
| 9 | [Mojo polish](epics/09-mojo-polish.md) | User-facing feature work |
| 10 | [CLI analysis + `--migrate`](epics/10-cli-analysis-and-migrate.md) | Rounds out the CLI surface |
| 11 | [Program boundaries](epics/11-program-boundaries.md) | MAIN-1; ~270 FPs each direction on the AWStats shape |
| 12 | [Type::Tiny completeness](epics/12-type-tiny-completeness.md) | Check-guards feed a lattice that already exists |
| 13 | [Pack-language ceiling](epics/13-pack-language-ceiling.md) | Calibration is the ship gate; it is half the work |
| 14 | [The per-file stall](epics/14-per-file-stall.md) | C/C++ is unusable at Godot size; nobody has profiled it yet |
| 15 | [Query paths at scale](epics/15-query-paths-at-scale.md) | Storage holds at 122x; query paths break |
| 16 | [The CFG tier](epics/16-cfg-tier.md) | UAM, the cpp D-codes and D9 are all parked on it |
| 17 | [Builder decomposition](epics/17-builder-decomposition.md) | Grab-bag parts hid a duplicated type table until it mistyped 12 Moose types |

**Suggested order:** 1 first, then 14 and 15 — those two are where the
product is unusable rather than merely incomplete — then 2–4, then
pull-driven. Epic 16 is scheduled late but **binds early**: its P1/P2
obligations and its cycle-cut change are free today and unrecoverable
retrofits, so Epics 4, 7 and 12 carry pointers back to it.

## Not in an epic

Small items that have no epic home and need none. Take them
opportunistically; each is a commit, not an arc.

- Fold safety net: `eprintln!` → `tracing::error!` at the release-mode
  `MAX_FOLD_ITERATIONS` break, plus a synthetic-oscillator test so it
  can't bit-rot.
- Full-bag scans in `apply_chain_typing_assignments` — index when
  profiling flags them.
- Cursor-context qualified-path/invocant detection should ask the tree,
  not byte-walk (`extract_package_from_prefix` and sibling).
- `return_via_edge` chases lack `TypeProvenance` — stamp
  `Delegation { kind: "callable_return_edge" }` on the chase.
- Unify autoquoted-key-as-literal into `cst::string_list`. **Blocked
  on** a latent use-import bug it unmasks: `use constant NAME => v`'s
  autoquoted key gets emitted as a spurious `FunctionCall` import ref
  (resolved_package `"constant"`) by the use-list walker — the old fold
  hid it by dropping non-constant barewords. Regression-guarded by
  `const_call_form_not_double_reffed`. Fix the use-`constant` path to
  not feed its declared names to the generic import-ref emitter, THEN
  move the autoquoted arm into `string_list` and drop the per-caller
  fold. Proper unification; not urgent.
- Per-row known gaps: `gold-corpus/KNOWN-GAPS.md` — the xfail rows are
  the live tracker.

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

## Backburner (no epic, no unblock condition — just not now)

- Aspirational type features (effects/throws) —
  `prompt-type-system-futures.md`. Pillar 1 (narrowing) landed; pillar 2
  is out of the QA loop by its own charter.
- Web extension — `prompt-wasm-web-extension.md`. The crate split it
  assumed was executed and REJECTED (layering tests enforce the DAG
  instead); branch `workspace-split` is the playbook if wasm ever
  forces it.
- The `lsp-engine` crate cut — parked by `prompt-multi-language.md`'s
  own text until a second pack language's ceiling work (Epic 13) makes
  the split pay for itself.
- Incremental analysis — `prompt-incremental-build.md`, with a named
  bar in-doc. Epic 15's diagnostics-after-edit row is its forcing
  function.

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
