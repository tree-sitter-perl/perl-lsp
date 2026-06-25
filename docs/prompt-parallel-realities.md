# Parallel realities — config-lifted `#ifdef` analysis

**Status: PARKED — needs deep thought before any build.** Branch
`spike/cpp-support`. Sibling to `docs/prompt-cpp-reparse.md` (the reparse
seam) and the strategy context in
`~/personal/resume/research-static-analysis.md` (the Klocwork / MISRA-C
wedge). A thin proof exists (`c_preproc_tests.rs ::
a2_parallel_realities_reduce_to_presence_conditions`); everything past it
is open. This doc captures the idea, the open questions, and the
tradeoffs so the decision can be made cold, not in the heat of a spike.

## The idea in one paragraph

A `#ifdef` is a fork in the source's *reality*. The build-coupled tools
(Klocwork, CodeQL) analyze **one** reality — the configuration you built
— and are blind to defects in the branches you didn't compile. The dual
move: don't pick a config, **cover all realities and reduce to
presence-conditioned facts.** `dbg` exists `when defined(DEBUG)`, `rel`
`when !defined(DEBUG)`, both in one analysis, each finding tagged with
the configuration under which it fires. This is the academic field of
**variability-aware / configuration-lifted analysis** (landmarks:
TypeChef, SuperC); naming it tells us where the pain is.

## What is already proven (don't re-litigate)

- **Blank-in-place reparse works** (A2, `c_preproc::select_config`):
  evaluate conditionals against a config, blank dead branches + directive
  lines to spaces (newlines kept), re-parse. Offsets are preserved, so
  re-parse spans equal the original's — **no anchor map**. The doc's A2
  fixture (4 ERROR nodes) recovers to a clean `function_definition` named
  `main` at its original byte offset.
- **The reduce is real** (the thin proof): fork both realities of one
  `#ifdef`, parse each, union the symbols tagged by which realities they
  appear in → `common @ Always`, `dbg @ defined(DEBUG)`, `rel @
  !defined(DEBUG)`. No build config chosen.

## The tier ladder (how the three compose — this part is settled)

All three sit on the **same** blank-in-place reparse; they differ only in
how the config axis is resolved:

| situation | mechanism | output |
|---|---|---|
| condition decidable + config known | `select_config` | one clean tree (as-built) |
| config unknown / want all variants | **parallel realities** | presence-tagged union |
| condition undecidable (arithmetic, include-pulled macro) | real-`cpp` probe, amortized once | resolves one axis, feeds back |

`select_config` already emits an `unresolved: Vec<Unresolved>` — that list
**is** the fork/probe worklist. Lite-eval what you can, fork realities
for the axes you want to cover, probe the axes you can't evaluate.

## The load-bearing architectural insight

**A `#ifdef` is a coarser arm of the witness bag's arm-fold.** The bag
already reduces "agreement across branch arms" (`BranchArmFold`,
`SymbolReturnArmFold`: arms agree → one value, diverge → split). A
ternary's two RHS arms and a `#ifdef`'s two declaration branches are the
**same shape** — alternatives selected by a condition. A fact true in all
realities is `Always`; true in some carries a **presence condition**.
This is not a new mechanism; it is the existing join with the config as
the arm selector. **If** that framing holds under scrutiny, parallel
realities is a *lift* of machinery we own, not a bolt-on.

## The strategic case (why it might be worth the pain)

Config-**independent** analysis is the differentiator the strategy doc
keeps circling. An automotive shop ships N build variants from one tree;
a MISRA violation in `#ifdef CAN_LEGACY` is invisible to a tool that
analyzed the CAN-FD build. "We checked every variant at once, and here's
the presence condition for each finding" is a pitch Klocwork (per-config,
build-coupled) structurally cannot make. This is the same incrementality/
buildless wedge from the strategy doc, applied to the *configuration*
axis instead of the build axis.

## OPEN QUESTIONS (the reason this is parked)

1. **Representation — the fork that decides everything.** Do presence
   conditions live as:
   - (a) a tag on `Symbol` / `Ref` in `FileAnalysis` (a `presence:
     Option<PresenceCond>` field), or
   - (b) a new axis on the **witness bag** — a `WitnessSource` /
     provenance carrying the presence condition, so the existing arm-fold
     reduces them natively?

   (a) is a bolt-on: simpler, but every consumer must learn to read the
   tag, and it does not compose with type inference (a type that holds
   only under a config can't be expressed). (b) is the first-class lift:
   if the bag framing above is right, presence conditions fold through
   the *same* reducers as everything else, and a config-conditional type
   falls out for free — but it touches the engine's core and may not
   actually fit (presence is a property of *existence*, not of *value*;
   does the attachment model accommodate "this witness exists only when
   C"?). **This question gates the whole effort. Answer it before code.**

2. **Does the arm-fold analogy actually hold, or is it a seductive
   surface match?** Ternary arms produce *values* on the same attachment;
   `#ifdef` branches produce *different symbols/scopes/structure*. The
   reduce for the latter is a union over *existence*, not a fold over
   *value*. Spike the smallest case where they diverge before betting the
   architecture on the analogy.

3. **Granularity of the fork.** Per-file 2^N is the trap (see tradeoffs).
   Per-`#ifdef`-region local fork + local reduce is the escape — but what
   is a "region", exactly, and how do we reduce two locally-forked
   parses whose *surrounding* text is identical (blank-in-place
   guarantees it) without re-deriving the whole file? Is the unit a
   top-level declaration? A scope? Needs a concrete algorithm.

4. **Presence-condition algebra.** Conditions compose (`defined(A) &&
   !defined(B)`) and want simplification → a BDD / SAT-lite over config
   variables. How much do we need? (TypeChef uses full SAT.) Is a
   normalized-DNF-with-dedup enough for the navigation/MISRA tier, with
   full SAT deferred?

5. **Cross-file × cross-config.** A header's `typedef` may itself be
   `#ifdef`'d. The B1 symbol table (`c_reparse`) becomes
   *config-conditional*: `Widget` is a type `when defined(USE_WIDGETS)`.
   Does the lexer-hack reparse then need a presence condition too? This
   couples the two spikes in a way neither currently models.

6. **What does the USER see?** Presence-tagged findings are more honest
   but more complex. Is the product "one finding per (defect, config)"?
   Deduped across configs? Does goto-def on a config-conditional symbol
   ask which reality? The UX is unspecified and may constrain the model.

## TRADEOFFS

- **2^N blowup.** N independent `#ifdef`s → 2^N whole-file
  configurations. **Whole-file cross-product is non-viable** on real
  code. Mitigation = locality (Q3): independent regions fork
  independently, so cost is *sum of local forks*, not their product —
  but this is unproven here and is the central engineering risk.
- **Soundness vs. the probe.** Parallel realities is sound only for the
  config axes you actually fork. Axes hidden behind undecidable
  conditions (Q in `select_config`) still need the probe; claiming
  "all configs" while silently dropping the unforked ones is the
  dishonesty trap — log what was not covered (the no-silent-caps rule).
- **Interacting configs.** A `#define` inside one branch that changes a
  later `#if` breaks region-locality (Q3) — the realities are no longer
  independent. This is the genuinely hard tail; fall back to per-config
  real-`cpp` probe.
- **Engine intrusion (if representation (b)).** The first-class lift
  touches `witnesses.rs` / `file_analysis.rs` — the load-bearing core,
  under the bag's monotonicity + edges-not-values discipline. A presence
  axis must respect those invariants or it rots them. High blast radius;
  high payoff. Representation (a) trades the payoff for safety.
- **Precedent says it's expensive.** TypeChef/SuperC are large research
  systems. The honest read: the *navigation + syntactic-MISRA* tier of
  this (presence-tagged symbols/refs) is far cheaper than their
  *type-correct, full-C* ambition, and that cheaper tier may be the whole
  win for the wedge. Scoping to that tier is itself an open decision.

## Decision criteria (what would green-light a build)

Build **only if** (1) representation is decided (Q1) and the bag framing
either holds (Q2) or is consciously dropped for the tag approach, and (2)
a concrete locality algorithm (Q3) bounds the cost below 2^N on a real
macro-heavy file. Until both, this stays parked. The thin proof is enough
to know it *works*; it says nothing about whether it *scales* or *fits*.
