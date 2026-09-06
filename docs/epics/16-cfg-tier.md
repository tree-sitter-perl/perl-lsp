# Epic 16 — The CFG tier: path sensitivity on the witness bag

> **Status:** scheduled (16th on the slate, but see the ordering note —
> **two of its representation decisions bind on epics scheduled
> before it**).
> **Design owner-doc:** `docs/prompt-cfg-tier.md` — read it WHOLE
> before planning anything. It is a completed design brief with the
> options weighed and the pick made; this epic is its ladder, its
> anchors and its gates, not a second design round.

## Mission

Three shipped things are parked on a tier that does not exist:

- `docs/adr/use-after-move.md` ships a **decidable subset** explicitly
  *because* "we don't have a CFG";
- the cpp D-codes (`adr/narrowing-diagnostics.md` §C/C++ applicability)
  are blocked on a nullability layer **along cpp control flow** —
  "D1/D2/D3/D4/D6 have no cpp facts";
- **D9 reachability** needs a pass nobody has written.

The brief's answer is deliberately *not* a control-flow graph in the
textbook sense. It is **sparse guarded value-flow on the existing
`FlowEdge` spine**: `FlowEdge` is already the def-site record and
`earliest_rebind_in` is already a poor-man's dominance query, so the
tier adds only what SSA adds to a def list — **join points and guard
labels on their arms**.

## The two constraints, and why they are non-negotiable

Both are quoted from the brief because a phase that violates either
produces a second engine, which is the failure this design exists to
avoid:

1. **Spans are the program-point currency.** "A CFG design that makes
   consumers key on block IDs introduces a second program-point
   currency beside spans — the parallel-store disease." **No `BlockId`
   is ever a consumer key.**
2. **The bag is monotone; textbook dataflow is not.** Per-point IN/OUT
   tables with kill/gen transfer functions violate the bag's
   invariants. The escape is SSA: "a kill stops being *delete the
   nullness fact* and becomes *a newer def is the one that reaches
   you* — superseded by structure, not destroyed." The whole design is
   chosen backward from that fact.

The pick, already made: **region algebra as the persisted substrate,
sparse guarded value-flow as the analysis.** Dense basic blocks never
persist and never exist as a resident structure; the worklist is the
already-landed `fold_to_fixed_point` driver. Do not relitigate this —
the brief weighs all three options and says why.

## Read first

1. `docs/prompt-cfg-tier.md` — whole. §§1–2 are the constraints and the
   pick; §3 is the entity list; §4 is "the solver is not a component";
   §6 is this epic's ladder.
2. `docs/adr/flow-narrowing.md`, `docs/adr/use-after-move.md`,
   `docs/adr/narrowing-diagnostics.md`, `docs/adr/bag-canonical.md`.
3. `src/model/witnesses/registry.rs` — `query_rec` and its visited
   guard (§3.6 is the one place this tier changes shared semantics).
4. `src/build/builder/narrowing.rs` — `NarrowSubject`, `GuardFact`,
   `NarrowOp`, `recognize_guards`.
5. `src/model/file_analysis/core_types.rs` — `FlowEdge`,
   `earliest_rebind_in`.
6. CLAUDE.md worklist invariants, in full.

## Current state — exact anchors

| Entity | Where it is today | Find it |
| --- | --- | --- |
| `control_regions` (untyped `Vec<Span>`) | **`PackFacts`** — pack-only | `grep -rn 'control_regions' src/` — note `surface_feed` discards it as "own-file straight-line gate spans"; the typed upgrade is also its promotion to a lane Perl populates |
| `NarrowSubject` (builder-transient) | `build/builder/narrowing.rs` | `grep -rn 'NarrowSubject' src/` |
| The three spellings `Place` converges | across layers | `GuardSite.subject: String`, `moved_from`'s `(String, Span, ScopeId)`, `ArrowDerefSite.receiver` |
| `FlowEdge` + the dominance stand-in | `model/file_analysis/core_types.rs` | `grep -n 'struct FlowEdge' -A 12 src/model/file_analysis/core_types.rs`; `grep -n 'fn earliest_rebind_in'` |
| `cst::is_conditionally_executed` — the syntactic must-run stand-in, shared by key-write flow edges and the `@_` window (`consume_arg_head`: a conditional `shift` opens the window) | `cst.rs` | `grep -rn 'is_conditionally_executed' src/` — reachability replaces the "unconditional syntax or open" rule with the real must-run verdict |
| `BranchArmFold` — what grows into `JoinFold` | `model/witnesses/reducers.rs` | `grep -rn 'BranchArmFold' src/model/witnesses/` |
| The cycle-cut site | `model/witnesses/registry.rs` | `grep -n 'fn query_rec' src/model/witnesses/registry.rs` |
| Does NOT exist yet | — | `place_state_at`, `PredicateAtom`, `GuardRef`, `ExitFact`, `unreachable_regions` — all return zero hits; that is expected |

## Phase breakdown (the brief's §6 ladder, verbatim in order)

### Phase A — typed `ControlRegion` + `ExitFact`

**No φ, no solver.** Buys D9 reachability, UAM class-3 arm-scoping, and
`unless`/`until` correctness.

1. `ControlRegion { span, kind, condition: Option<Span>, arms:
   Vec<Span>, guard: Option<GuardRef> }` with `kind` a **closed** enum:
   `If | Ternary | Loop { has_back_edge } | Switch | PreprocIf |
   Catchy`. Per-language recognition, rule #1: Perl beside
   `build/builder/narrowing.rs`; packs via new capture vocabulary
   (`@ctl.region` / `@ctl.cond` / `@ctl.arm`, following the `@flow`
   pattern).
2. `ExitFact` per §3.2, including the `Unwind` / `NoReturn` split — a
   `croak` is catchable by an enclosing `Catchy`; an `exit` wrapper is
   not, and reachability dies regardless. **Jump destinations are edges,
   not baked spans** (the brief's own late correction — do not bake a
   resolved span).
3. UAM's Gate C keeps reading regions **kind-blind** (containment still
   works) and gets arm-scoping for free. Verify that rather than
   assuming it.
4. Persisted, `#[serde(default)]`, `EXTRACT_VERSION` bump, in its lane,
   with its `surface_feed` fate decided (see the Scaling beat).
5. **Acceptance:** D9 unreachable-arm detection on a fixture per region
   kind; UAM class-3 arm-scoping; `unless`/`until` polarity tests; the
   pack capture vocabulary exercised by at least one pack language.

### Phase B — the `Place` promotion

**Mechanical and wide.** Buys UAM class-2 (subobject moves) and unified
subjects.

`NarrowSubject` is promoted three ways at once (§3.3): **spelling → binding**
(so aliases stop breaking it), **flat key → path steps** (so
`other.msg_type` and `self.msg_type` are disjoint paths rather than a
lossy string — a flat string faking prefix queries is rule #10's
lossy-string projection), and **builder-transient → Model-layer serde
entity**. `NarrowSubject` becomes its recognition-side constructor.

This converges `GuardSite.subject`, `moved_from`'s tuple, and
`ArrowDerefSite.receiver` into one entity. **Acceptance:** all three
call sites read `Place`; a subobject-move test that the flat spelling
could not express; an aliasing test that the spelling key got wrong.

### Phase C — assembler + `JoinFold` + cycle-cut markers + atoms

The core. Buys cpp D1/D2/D6 along real control flow and must/may UAM.
**v1 needs no fixpoint at all**; loop precision is v2 via the existing
driver.

1. The φ as a `Join { place, at }` attachment plus a `JoinFold`
   reducer — **`BranchArmFold` grown up** to handle bypass arms and
   back edges. Not a new parallel reducer beside it.
2. `PredicateAtom`, the **closed** guard algebra, including
   `Config(macro)` so a superposition-qualified verdict is
   expressible.
3. **The one shared-machinery change (§3.6), and the most dangerous
   item in this epic.** `query_rec`'s visited guard currently resolves
   an on-path revisit to *nothing* — the cyclic arm vanishes from the
   fold. Harmless for types; **wrong in the dangerous polarity for
   must/may facts**: a loop-head φ whose back-edge arm is silently
   dropped folds over the remaining arm and answers "must be the init
   value" — a stronger claim than the paths justify, which is the
   mechanism for a manufactured false positive. And because the chase
   resolves edges into synthetic witnesses before reducers see the
   list, a reducer **cannot distinguish** "arm resolved to nothing"
   from "arm was cycle-cut".
   **Fix:** a cycle-cut leaves an **explicit unknown-marker witness**.
   `JoinFold` folds it as ⊤ ("a path exists whose value I cannot name"
   → silence). Type reducers ignore the marker and behave exactly as
   today — assert that with a test, because it is the compatibility
   claim the whole change rests on.
4. **P1 binds here.** `place_state_at` must have a provenance mode
   returning the path skeleton it traversed —
   `Vec<(join_at, arm_taken, guard_atom)>` — alongside the verdict.
   This is free today and an unrecoverable retrofit later. It pays
   rent immediately as **hover provenance** ("possibly-null because the
   else-arm never assigned"), before any referee exists.
5. **P2 binds here.** `PredicateAtom` flattens `if (n > 0)` to `Opaque`
   by design; the **extraction-time** lowering (query time is
   tree-free) must ALSO record the condition in a small symbolic IR
   when it fits the fragment — comparisons, linear integer arithmetic,
   equality over symbolic values, boolean structure — and `Opaque`
   otherwise. It rides `GuardRef` as a **dormant payload**; no
   always-on consumer reads it. Do not build a consumer here.
6. **Verdict outputs (§3.7) are the design's strongest property:**
   region-bounded `Variable`/`Place` witnesses under a
   `Builder("cfg_flow")` clear-and-emit tag, which the existing D1–D6
   seams and `inferred_type_via_bag` consume with **zero consumer
   changes**. Plus `unreachable_regions: Vec<Span>` as a plain
   `FileAnalysis` row for D9. If a phase here needs a consumer change,
   something has gone wrong upstream of it.
7. **Acceptance:** cpp D1/D2/D6 fire along real control flow with a
   substrate sweep showing zero new false positives; must/may UAM; the
   correlated-branch case (`if ($ok) { $x = init() } … if ($ok) {
   $x->use }`) behaves; a loop fixture proving the cycle-cut marker
   prevents the over-strong claim; type reducers byte-identical.

### Phase D — consumer wiring and the promotion it unblocks

Wire the verdicts to the diagnostics that were waiting: D9's
unreachable arms, the cpp D-codes, UAM's must/may. Each promotes on its
own substrate evidence per `adr/narrowing-diagnostics.md`'s ladder —
this epic supplies facts, it does not get to skip the promotion bar.

Write `docs/adr/cfg-tier.md`: what landed, the cycle-cut semantics
change and its compatibility argument, the P1/P2 payloads and their
dormancy, and the honest boundary (no interprocedural effects).

## Non-goals — each has a named owner

- **Interprocedural effects** (ladder step 4: `Moves(param)`,
  `Derefs(param)`, the `CallBinding` upgrade, Surface membership for
  summaries). **Its own arc, and it is not epic-ready:** §5.2 is a
  deliberately open hole — parameter identity for dependent effects,
  where positional vs invocant vs Perl `@_` flattening/aliasing vs
  kwargs vs unpacking projections all disagree about what a parameter
  *is*. The brief enumerates the axes and the requirements any answer
  must satisfy; that design round gates the arc. Do not start it inside
  this epic.
- **Path-symbolic refinement** (§8 — the demand-driven referee, the
  checker ladder, SMT). Forward-looking and additive; **only P1 and P2
  bind now**, and they are Phase C items above. Do not build a
  refuter, a confirmer, or a solver here.
- **Dense basic blocks, per-block bitvectors, a second worklist.** The
  brief rejects option B explicitly.
- **A `BlockId` consumer key.** Ever.
- Loop fixpoint precision — v2, via the existing
  `fold_to_fixed_point` driver, once v1 is honest.

## Ordering note — the obligations bind before the epic runs

`P1`/`P2` and the cycle-cut change are **representation decisions that
are free today and unrecoverable retrofits.** Three epics scheduled
ahead of this one touch exactly those representations, and each carries
a pointer back here:

- **Epic 4** adds a cycle guard on the registry's visited set (its
  Phase C) and touches `BranchArmFold`'s neighborhood. It must not
  entrench the silent drop.
- **Epic 12** adds narrowing recognizer arms that mint `GuardFact`s over
  `NarrowSubject`s — P2's lowering and Phase B's `Place` both land on
  what those arms record.
- **Epic 7** designs suppression keys and SARIF output, and §7 of the
  brief says finding fingerprints key on `(rule, function symbol,
  Place path)` — **never line numbers**.

If this epic runs late, those three still owe it their seams.

## Language-pack beat

**This is the most cross-language epic on the slate, and unusually it
is cross-language by birth rather than by retrofit — the pack languages
are the *motivating* consumers, not the inherited ones.**

Evidence, all from the tree and the ADRs:

- **The cpp D-codes are the headline consumer.**
  `adr/narrowing-diagnostics.md` states "D1/D2/D3/D4/D6 have no cpp
  facts. The whole narrowing tier is a child of the Perl side", and
  "nothing records cpp guard sites for redundancy". Phase C is what
  changes that.
- **`use-after-move` is a C++ diagnostic**, already registered and
  off by default, and it ships a decidable subset *because* this tier
  is missing. UAM class-2 and class-3 are Phases B and A.
- **`control_regions` is currently a `PackFacts` lane** — the pack side
  got here first. Phase A's typed upgrade promotes it to a lane Perl
  also populates, which is the opposite of the usual direction.
- **Recognition is per-language by construction** (rule #1): Perl in
  the builder beside `narrowing.rs`; packs through **new capture
  vocabulary** (`@ctl.region` / `@ctl.cond` / `@ctl.arm`) following the
  landed `@flow` pattern. That split is already the right shape — one
  entity, two recognizers.
- **The narrowing lattice is already shared and already region-bounded.**
  The cpp arc's own lesson (`cpp-golive-map.md` ARC 2 E): "a narrowing
  is a SCOPED ASSERTION over a region, not a temporal value — must be
  explicitly region-bounded… cutoff is the shared
  `earliest_rebind_in`, edge-driven, consumed by Perl AND the query
  engine (cross-language)." Phase A is that lesson's next rung.

Obligations:

1. **Every phase runs `cargo test --features cpp` and the gold suite
   built with it, `lang-skip 0` confirmed.** A tier whose motivating
   consumer is C++ cannot be validated on Perl tests.
2. **Phase A's capture vocabulary is an interface, not an
   implementation detail.** It is how every future pack language gets
   control regions. Design it with more than one language in mind and
   land at least one pack language's recognition in Phase A, so the
   vocabulary is exercised rather than hypothesised.
3. **`PredicateAtom::Config(macro)`** is there so a
   superposition-qualified verdict is expressible — a C-family concern
   (`#ifdef` arms) with no Perl analogue. It must be in the closed enum
   from Phase C, not bolted on.
4. **When the effects arc eventually runs, `CallBinding` is where each
   language's calling convention is encoded once** — "cpp by-ref params
   make every callee write a caller-place effect; Perl's `@_` aliasing
   is the same trap in older clothes." That is a note for the arc, not
   work for this epic, but it is why §5.2's hole is language-shaped.
5. **Epic 13 interlock:** pack-language diagnostics beyond
   `use-after-move` are gated on a calibrated substrate (Epic 13
   Phase A) *and* on these facts existing. Neither alone is enough;
   say which one is missing when a code fails to promote.

## Scaling beat

**The brief's §4 title is the whole discipline: "the solver is not a
component."** This tier can be built so that it costs nothing until a
diagnostic asks, or so that it taxes every keystroke. The difference is
placement.

1. **Placement is the first decision, and the brief already made it:**
   diagnostic finalization or CI-only — the `--use-after-move` opt-in
   is the named precedent — **never the interactive fold.** Memoize by
   finding fingerprint × file generation.
2. **v1 has no fixpoint at all.** That is a cost decision as much as a
   precision one; do not add the loop iteration in Phase C because a
   fixture wanted it. v2 reuses `fold_to_fixed_point`, which already
   has a bounded driver and a debug-only `MAX_FOLD_ITERATIONS` net.
3. **Phase A and B both grow the persisted `FileAnalysis`.** Typed
   `ControlRegion`s replace bare spans (bigger per region);
   `Place` promotes a builder-transient into a serde entity with path
   steps. Both ride the bincode+zstd blob into `modules.db` — 1.73 GB at
   138,822 files, 13.9 KB/file (2026-08-17). **Report the per-file blob
   delta on Koha for each phase.** A blob regression is a warm-start
   regression for every user, and `EXTRACT_VERSION` bumps cost a cold
   re-index (~10.5 min at CPAN-5k).
4. **Both new lanes need a `surface_feed` decision and it is not
   obvious.** `surface_feed` destructures every field with no `..`, so
   this will not compile until decided. Control regions and places are
   *file-local* — discard with a reason. **Effects would be the
   opposite** (§5's "summaries join the Surface — load-bearing"), which
   is exactly why that arc is deferred: it is the phase that changes
   the invalidation story, and `FreshnessIndex::dirty_consumers` is
   what makes differential CI cost proportional to blast radius.
5. **P1's trail is allocated per chase.** A `Vec<(join_at, arm_taken,
   guard_atom)>` built on every `place_state_at` call, when almost no
   caller reads it, is a per-query allocation on a hot path. Make the
   provenance mode **opt-in at the call** — the brief says "a
   provenance mode", not "always on" — and assert the non-provenance
   path allocates nothing.
6. **P2's `SymExpr` is a dormant payload with no always-on consumer**,
   and it rides the blob. Keep the fragment small and bail to `Opaque`
   early; an unbounded symbolic IR recorded for every guard in every
   file is a blob regression bought for a feature that does not exist
   yet.
7. **The cycle-cut marker adds witnesses to the bag** on every cut.
   Monotone and bounded by the chase, but measure: the bag rides the
   blob too, and `query_rec` cuts are not rare on real code (the
   512-depth cap is an open Tier-2 row and was seen again during the
   references probe).
8. **`--check` is the batch verb that will pay for Phase D**, and it is
   already the constrained one — FHEM does not complete it on 31 GB.
   Report `--check` wall and peak RSS on Koha and FHEM, three runs,
   dated, for each promoted code.

## Verification gate

`cargo test` **and** `cargo test --features cpp` · gold 0 FAIL /
0 XPASS built `--features cpp` with **`lang-skip 0`** in the summary ·
`./e2e/run.sh` · **substrate audit at exact parity for Phases A–C** —
this tier adds facts, and no diagnostic changes behavior until Phase D
promotes it; a count that moves earlier means a fact leaked into a
consumer · per-code audited deltas in Phase D · **a test proving type
reducers are byte-identical across the cycle-cut change** · per-file
blob-size delta on Koha for A and B · `--check` wall + peak RSS, three
runs, dated.

## Sizing

Large. A is self-contained and independently valuable (D9 alone
justifies it). B is mechanical but wide — expect it to touch more call
sites than it looks like. C is the bulk and holds the only shared-
semantics change in the epic. D is promotion work whose length depends
on what the audits say.
