# Epics — the scheduled work, and where every design doc stands

Each numbered file here is a **self-contained implementation prompt**:
mission, reading list, grep-able code anchors, phased ladder with
per-phase acceptance criteria, hard non-goals, invariants, and the
verification gate. An implementing session starts with `CLAUDE.md`,
then its epic file, then the epic's listed design docs.

Ordering is a schedule, not a dependency graph — dependencies are
stated inside each epic. `docs/ROADMAP.md` links here and carries what
is deliberately *not* scheduled.

## Every epic carries three axes, not one

The slate was first drawn when this was a Perl analyzer with a
performance appendix. It is now a multi-language engine with a measured
scaling envelope, and those two facts are not a separate workstream —
they are properties of every seam anyone touches. So each epic file
carries two standing sections beside its ladder:

- **`## Language-pack beat`** — what this epic owes the pack languages
  (C/C++, Python, R, CMake). There are three legitimate answers and the
  epic must give one explicitly: *the seam is neutral and packs inherit
  it*, *the seam is Perl-flavored and here is where the pack sibling
  lives*, or *this is Perl-only and here is why that is honest*.
  Leaving it unanswered is how a Perl-shaped rule ends up in a shared
  layer — `src/layering_tests.rs` catches a file in the wrong layer,
  but nothing catches a Perl *semantic* in a shared function.
- **`## Scaling beat`** — the measured cost this epic must respect, and
  the scaling row it advances or endangers. Numbers come from
  `docs/scaling-limits.md`, `docs/prompt-scale-validation-hitlist.md`,
  `docs/cpp-status.md` and `bench/RESULTS.md` — always with the date
  they were taken, because a number without one rots silently
  (CLAUDE.md's own warning; abseil's warm RSS was recorded ~2× low in
  two ADRs seven weeks apart).

Epics 13–15 exist because the two axes have mass with no home in a
feature epic: the pack-language ceiling, the C++ per-file stall, and
the Tier-1 query-path residual are each an arc, not a paragraph.

**Epic 16 is a third kind of entry: a tier three shipped features are
already parked on.** `use-after-move` ships a decidable subset because
there is no CFG; the cpp D-codes have no facts to read; D9 reachability
has no pass. It is scheduled late, but **two of its representation
decisions bind on epics scheduled before it** — they are free today and
unrecoverable retrofits — so Epics 4, 7 and 12 each carry a pointer
back to it. Read the ordering note in `16-cfg-tier.md` before starting
any of those three.

## The slate

| # | Epic | Size | Depends on |
|---|---|---|---|
| 1 | [Provider identity — retire the single-winner assumption](01-provider-identity.md) | M | — |
| 2 | [DBIC out of core, phases 2–3](02-dbic-out-of-core.md) | M | — |
| 3 | [Openness — one answer to "is this name real?"](03-openness.md) | M | — |
| 4 | [Value provenance, tier 1](04-value-provenance.md) | L | — |
| 5 | [One-seam sweep: magic tokens + cst backlog](05-one-seam-sweep.md) | S | — |
| 6 | [Rename provenance (residual)](06-rename-provenance.md) | S–M | 1 gates Phase B |
| 7 | [Diagnostic framework: codes, config, SARIF](07-diagnostic-framework.md) | L | after 3's promotions |
| 8 | [Heatmap residuals: Handlers + plugin-owned reachability](08-heatmap-residuals.md) | M | interlocks with 7 |
| 9 | [Mojo polish: routes, stash, hooks, chains](09-mojo-polish.md) | L | — |
| 10 | [CLI analysis subcommands + `--migrate`](10-cli-analysis-and-migrate.md) | L | 7/8 for two lint aliases |
| 11 | [Program boundaries + MAIN-1](11-program-boundaries.md) | M | brands-half waits on 4 |
| 12 | [Type::Tiny check-guards](12-type-tiny-completeness.md) | S | — |
| 13 | [Pack-language ceiling: diagnostics, framework tier, calibration](13-pack-language-ceiling.md) | L | — |
| 14 | [The per-file stall — C++ beta → GA](14-per-file-stall.md) | M | — |
| 15 | [Query paths at scale — Tier 1 residual](15-query-paths-at-scale.md) | L | 1 interlocks (candidate sets) |
| 16 | [The CFG tier — path sensitivity on the bag](16-cfg-tier.md) | L | **4, 7 and 12 owe it seams** — see below |
| 17 | [Builder decomposition](17-builder-decomposition.md) | S–M | — |

**Suggested order.** 1 first (it is a class of confidently-wrong
answers, and Veesh named it next); then 14 and 15, because they are the
two places the product is currently *unusable* rather than merely
incomplete; then 2–4, which finish the standing type-intelligence
commitments; then the rest pull-driven.

## Coverage map — every open design doc and item

The rule: every forward-design doc is either (a) scheduled in an epic,
(b) parked with a named unblock condition, (c) landed with only parked
residuals, or (d) explicitly out of scope. Nothing is unaccounted for.

### Perl / type-intelligence

| Doc / item | Disposition |
|---|---|
| `open-problems.md` §"Duplicate-package resolution" | **Epic 1** — the relation landed (`b32814a0`); the consumer conversion is the epic |
| `prompt-dbic-as-plugin.md` | **Epic 2** (phase 1 landed; phase 2's `meta_methods` manifest was drafted in the unmerged #109 and is re-absorbed here) |
| `prompt-graph-walking.md` — Scope nodes / Openness | **Epic 3** |
| `prompt-graph-walking.md` — instance brands | **Parked**: unblocks after Epic 4 + constructor/field flow; rebuild ONLY per its birth-site rule, never the syntactic spike |
| `prompt-type-inference-residual.md` Parts 1, 2, 5a | **Epic 4** |
| `prompt-type-inference-residual.md` Parts 3, 4 | Queued after Epic 4 (same engine, QA pulls decide) |
| `prompt-type-inference-residual.md` Part 5c residuals (prefetch, `join =>` keys) | Queued; natural follow-on to Epic 2 |
| `prompt-type-inference-residual.md` Part 7 (Rhai reducers) | **Parked**: wants a second concrete consumer beyond route aggregation |
| `prompt-magic-tokens.md` | **Epic 5** (phases A–B) |
| `prompt-cst-migration.md` items 1–5, 7 | **Epic 5** (phases C–G); item 6 is a standing strangler rule, not schedulable |
| `prompt-ref-provenance.md` | **Epic 6** — `Ref.folded_from` LANDED; phases B–E remain |
| `prompt-cli-tools.md` — diagnostic framework | **Epic 7** |
| `prompt-config-schema.md` | **Epic 7** (its named forcing function) |
| `adr/heatmap.md` §residuals | **Epic 8** (SARIF piece rides Epic 7) |
| `prompt-mojo-todo.md` | **Epic 9** |
| `prompt-cli-tools.md` — analysis subcommands + `--migrate` | **Epic 10** |
| `prompt-entrypoint-analysis.md` | **Epic 11** (brands-half stays parked) |
| `open-problems.md` §"`main::` aggregation across `require`" | **Epic 11** (phase C) |
| Type::Tiny check-guards | **Epic 12**. The import-scoped name gate and the `Maybe[T]` → `Optional<T>` fold are landed (`adr/type-constraints.md`); `ArrayRef[T]` elements are parked with sequence-types, which names the fold as its waiting caller |
| `open-problems.md` §"Cross-file `ClassIsa`-trigger emissions" | **LANDED** — `plugin.gated_emissions` + `class_isa_prefix` + `enrichment::apply_gated_emissions`; the doc section is stale and Epic 3 Phase A retires it |
| `prompt-enrichment-inheritance-residual.md` | Landed with the above; only the `ClassIsa`/`param_types` applicability matrix rows remain, verified in **Epic 3** |
| `prompt-helper-consumption.md` | Phases 1–2 landed; phase 3 (per-app surfaces) **parked** with instance brands |
| `prompt-long-distance.md` | Landed; open-world caller gather **parked** on Epic 3's enumerability witness |
| `prompt-method-resolution-residuals.md` | §§1–3 landed; probe-based plugin generation **parked** (needs a runtime-probe design); §4 rides Epic 4 |
| `prompt-flow-narrowing.md` / `prompt-optional-types.md` | Landed; residuals parked in-doc |
| `prompt-sequence-types.md` | **Parked — QA pulls** |
| `prompt-type-is-the-gate.md` | **Parked**: waits for the next motivating strict-eq gate; Epic 2's emission work is the likeliest place it surfaces |
| `prompt-type-system-encoding.md` | **Parked**: Epic 2 decides the manifest-vs-axis question at the boundary |
| `prompt-type-system-futures.md` | Pillar 1 (narrowing) LANDED; pillar 2 (effects/throws) aspirational, out of the QA loop by its own charter |
| `open-problems.md` — untyped param/hash-element boundary | Hard boundary; Epic 4 + constructor/field flow are the approach vector |
| `open-problems.md` — runtime export generators | Hard boundary; MooseX::Role::Parameterized and Sub::Exporter ride it |
| `prompt-optional-types.md` / `prompt-relational-iteration.md` | Landed (see `adr/relational-ref-index.md`) |
| `prompt-cfg-tier.md` | **Epic 16** — ladder steps 1–3 (typed regions/exits, the `Place` promotion, the assembler + `JoinFold` + cycle-cut markers + atoms), plus the two binding obligations P1/P2 |
| `prompt-cfg-tier.md` §5 — interprocedural effects (ladder step 4) | **Blocked on a design round, not on an epic.** §5.2 is a deliberately open hole: parameter identity for dependent effects, where positional vs invocant vs Perl `@_` flattening/aliasing vs kwargs vs unpacking projections disagree about what a parameter *is*. The brief enumerates the axes and the requirements any answer must satisfy. Its own arc once that round closes; note it is also the phase where summaries join the Surface |
| `prompt-cfg-tier.md` §8 — path-symbolic refinement | **Forward-looking and additive.** Only P1 (the chase reports its arm trail) and P2 (guards keep a decidable-fragment `SymExpr`) bind now, as Epic 16 Phase C items. The checker ladder (atom-SAT → intervals → optional SMT) and its placement are all later |
| `adr/use-after-move.md` residual classes | **Epic 16** — class-3 arm-scoping is Phase A, class-2 subobject moves are Phase B, must/may is Phase C. The ADR's decidable subset is the honest floor until then |
| `prompt-lsp-surface-parity.md` | Landed — the type-hierarchy / call-hierarchy / typeDefinition cluster plus a re-scoped documentLink, each with gold rows. The brief's own non-goals stand; `linkedEditingRange` stays OFF |
| `prompt-wasm-web-extension.md` | **Backburner** (ROADMAP). The crate split it assumes was executed and REJECTED; `workspace-split` is the playbook if wasm ever forces it |
| `parser-shortcomings.md` | Upstream tree-sitter-perl bugs, handed off to the parser team. Not schedulable here; `adr/error-recovery.md` is what we do about them meanwhile |

### Language packs

| Doc / item | Disposition |
|---|---|
| `prompt-multi-language.md` §"What pack languages still don't get" | **Epic 13** — diagnostics, framework tier (capture-event hooks), completion context, the calibration substrate |
| `prompt-multi-language.md` §"Shipping shape" (the `lsp-engine` crate cut) | **Parked** by its own text: does not start before a second pack language's ceiling work forces the split to pay for itself — i.e. after Epic 13 |
| The next serve-in-anger pack language | Scheduled, brief not yet on main. It rides **Epic 1** (its identity model is the same single-winner residual) and its calibration substrate is **Epic 13** Phase A. Add its rows here when the branch lands |
| `cpp-status.md` §"The scaling limit: measured" | **Epic 14** — the per-file stall is the beta→GA gate |
| `prompt-macro-salvage-scaling.md` | **Epic 14** (its ranked fixes are the epic's Phase B) |
| `prompt-vendored-dirs.md` | **Epic 14** (Phase D — the role-remap lever; `scaling-limits.md` §2 is the Perl-side evidence) |
| `prompt-cpp-member-refs.md` §residuals | Queued behind Epic 14; each is a separate careful change |
| `prompt-unify-language-paths.md` (serving tier) | **Parked** with a stated condition in-doc: a cleanup with no user-visible product, and it gets cheaper every time a pack seam generalizes. Epics 3/7/13 each shrink it; re-read after 13 |
| `prompt-unify-semantic-tiers.md` (the emission twin) | **Parked** with its sibling above. Perl and the packs already share ONE witness engine, registry, chase and storage stack; what differs is emission. Its phase 4 just points at the serving-tier brief, so the two land together or not at all |
| `prompt-parallel-realities.md` (config-lifted `#ifdef`) | **PARKED — needs deep thought before any build**, by its own header. The spike PoC modules it cites were removed in the 2026-07-13 GC; re-land from the brief if the arc starts. Sibling to `adr/reparse-stratification.md` |
| `cpp-system-headers.md`, `cpp-stdlib-autoconfig-research.md`, `cpp-lsp-experience-research.md`, `clangd-comparison.md`, `clangd-benchmark-procedure.md` | Research and procedure records, not schedulable work. Epic 14 reads them |
| `cpp-golive-map.md` §Deferred | Recorded, not queued — each line names its own prerequisite |
| `docs/adr/cpp-templates.md` parked residue (deduction, template-template params, `extern template`) | **Parked** on their own brief |
| Pack framework plugins (tidyverse, CMake conventions) | **Epic 13** Phase C — keying plugin hooks on capture events is the open design round |
| PHP target scouting (`780065a5`) | Not scheduled; a market question, not an engineering one |

### Scaling

| Doc / item | Disposition |
|---|---|
| `prompt-scale-validation-hitlist.md` Tier 1 #3 (`references` at scale) | **Epic 15** Phase A — RETURNS at 138k but 265–368 s; slow, honest, bounded, not yet fast |
| `prompt-scale-validation-hitlist.md` Tier 1 #5 residual + #6 | **Epic 15** Phase B — CLI one-shot is O(corpus); `LanguageScope` root-caused but unconfirmed at 138k |
| `prompt-scale-validation-hitlist.md` §Validation ("OWED" rows) | **Epic 15** Phase D — cold cpan5k with every fix in, the differential sweep, the `PackBagCache` re-soak |
| `prompt-scale-validation-hitlist.md` Tier 2/3 open rows (`query_rec` 512-depth, `cursor_slot.rs:205`, index-family merge) | **Epic 15** Phase C, or explicitly declined there |
| `scaling-limits.md` §1 (FHEM `package main` monoculture) | **Epic 1** owns the mechanism (the 534-member candidate set IS the package-identity relation) and **Epic 15** owns the sweep-level dedup |
| `scaling-limits.md` §5 (`--heatmap` is a batch verb) | **Landed** (2026-09-06). The residuals it records — the 8-thread knee (allocation churn in the walk) and the pack-only per-visit costs — are Epic 15 Phase A and Epic 14, not Epic 8 |
| `prompt-incremental-build.md` | **Parked** with a named bar in-doc; Epic 15 Phase D's measurements are its forcing function |
| `prompt-storage-residuals.md` | Known unbounded residuals, deliberately listed; not an epic until one is measured to hurt |
| `prompt-enrichment-delta.md` (enrichment as a delta artifact) | **Design, not started.** Its three named pressures — level-indexed enrichment's rejection, the FHEM crest, the overlay retention story — are all Epic 15 territory; it is the candidate design if Phase B's dedup proves insufficient. Do not start it before Epic 15 Phase C measures the enrichment path fresh |
| `build/builder/` grab-bag parts | **Epic 17** — behavior-frozen decomposition; the split rule is CLAUDE.md's own, applied per concern rather than per line count |
| `bench/RESULTS.md` + `bench/baselines.jsonl` | The standing record. Every epic that moves a KPI updates it — see the house rules |

### Ledgers, not design docs

These are records with their own lifecycles. They are **not** covered by
the rule above, and the epics do not supersede them:

- **`docs/PARKED.md`** is THE deferred-work ledger and stays the source
  of truth for what is deliberately not done — design-debt, feature and
  residual-bug tiers. The relationship: **PARKED is the pool, an epic is
  a commitment.** Work moves PARKED → epic when it gets scheduled, and
  an epic's dropped phase moves back. Its design-debt tier is drained by
  the `tighten-loop` skill between feature arcs, not by an epic.
- `docs/open-forks.md` — architectural forks picked mid-flight and
  logged for later ratification, per the loosely-coupled-option
  convention. Read it before relitigating a fork.
- `docs/forks-resolved.md`, `docs/review-narrow-seams.md`,
  `docs/gold-roadmap.md`,
  `docs/cpp-golive-map.md`, `docs/hitlist-*.md` — completed audits, arc
  records and working docs. The `docs-gc` skill converts them to their
  durable form when their arc closes; do not treat a closed hitlist as
  a worklist.
- `docs/PLUGIN_AUTHORING.md` — user-facing reference.
- `gold-corpus/KNOWN-GAPS.md` — the live per-row gap tracker. Always
  current, always readable, never an epic.

## House rules for implementers (apply to every epic)

- Read `CLAUDE.md` first; its numbered rules override anything an epic
  doc accidentally contradicts. When in doubt, the rule wins and the
  epic doc gets a PR comment.
- **Verify every anchor before editing it.** These files name grep
  targets, not line numbers, and the tree moves. An anchor that does
  not resolve is a signal that the epic's premise changed — go read
  what replaced it before writing code against a memory.
- Every epic ends at the same gate: `cargo test` (and `--features cpp`
  when the change is not Perl-only), gold harness 0 FAIL / 0 XPASS
  (XPASS → promote the row) built `--features cpp` with `lang-skip 0`
  in the summary, `./e2e/run.sh`, and — for anything touching inference
  or diagnostics — the substrate audit diffed against a pre-epic
  binary with always-on `undef-deref` at exact parity:

  ```
  perl-lsp --clear-cache gold-corpus/local/lib/perl5
  perl-lsp --check gold-corpus/local/lib/perl5 --format json --severity hint \
    --optional-deref --redundant-guard --deref-shape --unresolved-method-cross-file
  ```

- **A measurement claim needs three runs and a date.** A single run is
  not a baseline (a phantom +400 ms "regression" survived a day on
  one). The protocol is the `edit-bench` skill; the store is
  `bench/measure.sh` → JSONL, and `bench/baselines.jsonl` is the
  checked-in KPI record. Never hand-roll an env-gated `eprintln!`
  timer — route through `util/timings.rs`.
- Bump `EXTRACT_VERSION` whenever FA shape or bag rules change; new
  Fact families / source tags go into `witnesses::tags`, never inline.
  A new `FileAnalysis` lane goes INTO its owner sub-struct, and
  `surface_feed` will not compile until its Surface fate is decided.
- One phase = one reviewable commit (or PR); each lands with its
  negative tests, not just its happy path.
- Update the owner design doc + this README's coverage map in the same
  PR that changes a disposition.
