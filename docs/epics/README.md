# Epics — the scheduled work, and where every design doc stands

Each numbered file here is a **self-contained implementation prompt**:
mission, reading list, grep-able code anchors, phased ladder with
per-phase acceptance criteria, hard non-goals, invariants, and the
verification gate. An implementing session starts with `CLAUDE.md`,
then its epic file, then the epic's listed design docs.

Ordering is a schedule, not a dependency graph — dependencies are
stated inside each epic. Epics 4–12 are mutually independent unless
their docs say otherwise; 1–3 come first because they finish the
standing roadmap commitments.

| # | Epic | Size | Depends on |
|---|---|---|---|
| 1 | [DBIC out of core, phase 3](01-dbic-phase-3.md) | M | — |
| 2 | [Openness + flag promotion](02-openness.md) | M | — |
| 3 | [Value provenance, tier 1](03-value-provenance.md) | L | — |
| 4 | [One-seam sweep: magic tokens + cst backlog](04-one-seam-sweep.md) | S | — |
| 5 | [Duplicate-package identity (H1)](05-duplicate-package-identity.md) | S | — |
| 6 | [Gated cross-file emission (ClassIsa)](06-gated-cross-file-emission.md) | M | — |
| 7 | [Rename provenance](07-rename-provenance.md) | M | Epic 5 helps Phase D |
| 8 | [Diagnostic framework: PL-codes, config, SARIF](08-diagnostic-framework.md) | L | after Epic 2's promotions |
| 9 | [Heatmap residuals: Handlers + framework-consumed](09-heatmap-residuals.md) | S–M | interlocks with 8 (PL006) |
| 10 | [Mojo polish: routes, stash, hooks, chains](10-mojo-polish.md) | L | — |
| 11 | [CLI analysis subcommands + --migrate](11-cli-analysis-and-migrate.md) | L | 8/9 for the two lint aliases |
| 12 | [Program boundaries + MAIN-1](12-program-boundaries.md) | M | brands-half waits on 3 |

## Coverage map — every `docs/prompt-*.md` and open design item

The rule: every forward-design doc is either (a) scheduled in an epic,
(b) parked with a named unblock condition, (c) landed with only parked
residuals, or (d) explicitly out of scope. Nothing is unaccounted for.

| Doc / item | Disposition |
|---|---|
| `prompt-dbic-as-plugin.md` | **Epic 1** (phases 1–2 landed) |
| `prompt-graph-walking.md` — Scope nodes / Openness | **Epic 2** |
| `prompt-graph-walking.md` — instance brands | **Parked**: unblocks after Epic 3 + constructor/field flow (queued); rebuild ONLY per its birth-site rule, never the syntactic spike |
| `prompt-type-inference-residual.md` Parts 1, 2, 5a | **Epic 3** |
| `prompt-type-inference-residual.md` Parts 3, 4 | Queued after Epic 3 (same engine, QA pulls decide) |
| `prompt-type-inference-residual.md` Part 5c residuals (prefetch, `join =>` keys) | Queued; natural follow-on to Epic 1 |
| `prompt-type-inference-residual.md` Part 7 (Rhai reducers) | **Parked**: wants a second concrete consumer beyond route aggregation |
| `prompt-magic-tokens.md` | **Epic 4** (phases A–B) |
| `prompt-cst-migration.md` items 1–5, 7 | **Epic 4** (phases C–G); item 6 is a standing strangler rule, not schedulable |
| `qa-design-items.md` §H1 | **Epic 5** |
| `qa-design-items.md` §MAIN-1 | **Epic 12** (phase C) |
| `qa-design-items.md` §MooseX::Role::Parameterized | **Parked**: it is the runtime-export-generator open problem (`open-problems.md`) wearing role clothes; no static answer without a probe/eval tier |
| `prompt-enrichment-inheritance-residual.md` | **Epic 6** |
| `prompt-ref-provenance.md` | **Epic 7** |
| `prompt-cli-tools.md` — diagnostic framework | **Epic 8** |
| `prompt-config-schema.md` | **Epic 8** (its named forcing function) |
| `prompt-heatmap.md` residuals | **Epic 9** (SARIF piece rides Epic 8) |
| `prompt-mojo-todo.md` | **Epic 10** |
| `prompt-cli-tools.md` — analysis subcommands + `--migrate` | **Epic 11** |
| `prompt-entrypoint-analysis.md` | **Epic 12** (brands-half stays parked) |
| `prompt-helper-consumption.md` | Phases 1–2 landed; phase 3 (per-app surfaces) **parked** with instance brands |
| `prompt-long-distance.md` | Landed (epic record); open-world caller gather **parked** on Epic 2's openness/unresolved bucket giving an enumerability witness |
| `prompt-method-resolution-residuals.md` | §§1–3 landed; §3's probe-based plugin generation **parked** (needs a runtime-probe design); §4 rides Epic 3's tier |
| `prompt-flow-narrowing.md` | Landed; residuals deliberately parked in-doc (accessor places, Option B knob, negation) |
| `prompt-optional-types.md` | Landed incl. production gaps |
| `prompt-sequence-types.md` | **Parked — QA pulls** (its own status header; phases additive, no do-now tax) |
| `prompt-type-is-the-gate.md` | **Parked**: waits for the next motivating strict-eq gate; Epic 1's emission work is the likeliest place it surfaces — implementers there should re-read it |
| `prompt-type-system-encoding.md` | **Parked**: Epic 1 decides the manifest-vs-axis question at the boundary; revisit only if the manifest route fails |
| `prompt-type-system-futures.md` | Pillar 1 (narrowing) LANDED (`adr/flow-narrowing.md`); pillar 2 (effects/throws) aspirational, **out of the QA loop by its own charter** |
| `prompt-wasm-web-extension.md` | **Parked**: crate split was executed and REJECTED (layering tests enforce the DAG instead); `workspace-split` branch is the playbook if wasm demand materializes |
| `open-problems.md` — untyped param/hash-element boundary | Hard boundary; Epic 3 + the queued constructor/field flow are the approach vector; stays listed there |
| `open-problems.md` — qualified-name suppression | **Epic 2** phase C |
| `open-problems.md` — runtime export generators | Hard boundary; stays; MooseX::R::P and Sub::Exporter ride it |
| Re-export chains (ROADMAP parked) | **Parked** on the ts-parser-perl X1 scanner thread-safety fix |
| Multi-language engine (ROADMAP backburner) | **Parked** on branch `worktree-query-extraction-spike` |

## House rules for implementers (apply to every epic)

- Read `CLAUDE.md` first; its numbered rules override anything an
  epic doc accidentally contradicts. When in doubt, the rule wins and
  the epic doc gets a PR comment.
- Every epic ends at the same gate: `cargo test` green, gold harness
  0 FAIL / 0 XPASS (XPASS → promote the row), `./e2e/run.sh` via CI if
  nvim is absent locally, and — for anything touching inference or
  diagnostics — the substrate audit diffed against a pre-epic binary
  with always-on `undef-deref` at exact parity:

  ```
  perl-lsp --clear-cache gold-corpus/local/lib/perl5
  perl-lsp --check gold-corpus/local/lib/perl5 --format json --severity hint \
    --optional-deref --redundant-guard --deref-shape --unresolved-method-cross-file
  ```

- Bump `EXTRACT_VERSION` whenever FA shape or bag rules change; new
  Fact families / source tags go into `witnesses::tags`, never inline.
- One phase = one reviewable commit (or PR); each lands with its
  negative tests, not just its happy path.
- Update the owner design doc + this README's coverage map in the
  same PR that changes a disposition.
