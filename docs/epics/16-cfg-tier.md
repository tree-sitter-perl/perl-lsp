# Epic 16 — The CFG tier: path sensitivity on the witness bag

> **Status:** Phases A–C landed (audited 2026-11-19); Phase D — the
> diagnostics the tier exists for — is open. The contract is
> `docs/adr/cfg-tier.md`; it departs from the original ladder where the
> anchors moved (joins are bag edges and nothing is persisted as a
> conclusion; control structure is declared in each language's query
> document; effects have their own lane).
> **Design owner-doc:** `docs/prompt-cfg-tier.md` — the brief with the
> options weighed and the pick made. Read it before planning Phase D or
> the effects arc; the ADR wins where they differ.

## Mission

Three shipped things were parked on a tier that did not exist:

- `docs/adr/use-after-move.md` ships a **decidable subset** because it has
  no CFG;
- the cpp D-codes (`adr/narrowing-diagnostics.md` §C/C++ applicability)
  are blocked on a nullability layer **along cpp control flow** —
  "D1/D2/D3/D4/D6 have no cpp facts";
- **D9 reachability** needs a pass nobody has written.

The tier supplies the facts; this epic's remaining work is wiring them to
those three consumers, each on its own substrate evidence.

The two constraints stand and bind Phase D: **spans are the program-point
currency** (no `BlockId` is ever a consumer key) and **the bag is
monotone** (a kill is "a newer def reaches you", never a deletion).

## What landed, with evidence

| Phase | Landed as | Evidence |
|---|---|---|
| A — typed control regions + exit facts | Reshaped: control structure is the `@ctl.*` capture vocabulary read through a tree-free `ControlView` (leaves, gates, bodies), and exits are the effect lane's `Ends` (`COMPLETES`/`UNWINDS`/`EXITS`) rather than an `ExitFact` | `queries/perl/ctl.scm` and the `@ctl.*` captures in `queries/{php,python,cpp}/skeleton.scm`; `ControlView` in `model/file_analysis/control.rs` (`control_tests.rs`); `Ends` in `model/witnesses/types.rs`; `EffectFold` registered in `model/witnesses/registry.rs`; `MethodSurface.ends` in `model/surface.rs` |
| B — `Place` | `Place { root: Binding, path: Vec<PathStep> }` | `model/file_analysis/place.rs`; `FileAnalysis::place_type_at` / `subject_type_at` (`queries.rs`) |
| C — joins, `JoinFold`, cycle handling | Joins are `Edge(Join{place, of, role})` bag witnesses minted after variable binding; `JoinFold` folds arms with `types::join`; a loop's recurrence is chased to a fixed point at query time (`rechase_recurring`, `pass_round_loop`, `MAX_LOOP_PASSES`) instead of a cycle-cut marker; arms carry their condition as `(Span, bool)` and gates | `build/builder/joins.rs`, `JoinPlan` (`control.rs`), `JoinFold` in `model/witnesses/registry.rs`; `docs/adr/cfg-tier.md` §Joins, §Loops, §Try, §Effects |

`PredicateAtom`, `place_state_at`, `ExitFact`, `ControlRegion` and
`unreachable_regions` do not exist as named: the ADR's shapes replaced them
(`grep` returns nothing for each). `PackFacts.control_regions` remains the
pack lane `use-after-move` reads.

## Phase D — consumer wiring and the promotion it unblocks (open)

Wire the facts to the diagnostics that were waiting:

1. **D9 unreachable arms.** Needs an `unreachable` projection over the
   joins and gates (nothing named `unreachable_regions` exists). The
   claim is "no path reaches this block", read from the same `JoinPlan`
   verdicts the narrowing uses, never a second reachability pass.
2. **cpp D1/D2/D6 along real control flow.** The cpp facts exist only
   once cpp guard sites record into the same joins; the C-family
   `Config(macro)` superposition case (`#ifdef` arms) must be expressible
   in the arm's condition from the start.
3. **Must/may use-after-move** (`adr/cfg-tier.md` §Not in this tier yet).

Each code promotes on its own evidence per
`adr/narrowing-diagnostics.md`'s ladder — registered, default-off, swept
over the substrate, per-site triage, default-on at a stated severity. This
epic supplies facts; it does not skip the promotion bar. A code that fails
to promote names its gate: a substrate (Epic 13 Phase A), an openness fact
(Epic 3), or these path facts.

**Acceptance per code:** the sweep count and triage on the substrate, a
fixture per region kind it reads, the correlated-branch case
(`if ($ok) { $x = init() } … if ($ok) { $x->use }`) behaving, and
`--check` wall and peak RSS on Koha and FHEM, three runs, dated.

## Non-goals — each has a named owner

- **Interprocedural effects beyond `Ends`** (`Moves(param)`,
  `Derefs(param)`, Surface membership for summaries). Its own arc,
  blocked on `prompt-cfg-tier.md` §5.2: parameter identity for dependent
  effects, where positional, invocant, `@_` aliasing, kwargs and
  unpacking projections disagree about what a parameter *is*.
- **Path-symbolic refinement** (§8: P1 trail, P2 `SymExpr`, the checker
  ladder). P1 is carried in span form by each arm's `(Span, bool)`
  condition; P2 and everything past it are deferred and additive.
- **Dense basic blocks, per-block bitvectors, a second worklist, a
  `BlockId` consumer key.** The brief rejects them.
- **Constant folds as values** on `Place`s: `docs/prompt-value-lane.md`,
  a consumer of Phases B and C.

## Language-pack beat

The pack languages are the motivating consumers: the cpp D-codes and the
C++ `use-after-move` channel are what the tier is for.

1. **Every phase-D change runs `cargo test --features cpp` and the gold
   suite built with it, `lang-skip 0` confirmed.** A tier whose motivating
   consumer is C++ cannot be validated on Perl tests.
2. **The `@ctl.*` vocabulary is how a language gets control structure**
   (rule #15): Perl, PHP, Python and C++ declare theirs in the query
   document; a new pack language adds captures, not Rust.
3. **A guard's condition may name a macro** (`Config`) so a
   superposition-qualified verdict is expressible — a C-family concern with
   no Perl analogue.
4. When the effects arc runs, `CallBinding` is where each language's
   calling convention is encoded once (cpp by-ref params make every callee
   write a caller-place effect; Perl's `@_` aliasing is the same trap).

## Scaling beat

The solver is not a component: the tier costs nothing until a diagnostic
asks.

1. **Placement:** diagnostic finalization or CI-only — the
   `--use-after-move` opt-in is the named precedent — never the
   interactive fold.
2. **Persisted growth rides the blob.** The joins and `Place` lanes are
   bincode+zstd in `modules.db` (1.73 GB at 138,822 files, 13.9 KB/file,
   2026-08-17 — re-take before citing). Report the per-file blob delta on
   Koha for any new persisted fact.
3. **`--check` is the batch verb that pays for Phase D** and is already
   the constrained one (FHEM does not complete it on 31 GB). Report wall
   and peak RSS on Koha and FHEM per promoted code, three runs, dated.

## Verification gate

`cargo test` and `cargo test --features cpp` · gold 0 FAIL / 0 XPASS built
`--features cpp` with `lang-skip 0` · `./e2e/run.sh` · per-code audited
deltas on the substrate · per-file blob-size delta on Koha · `--check`
wall + peak RSS, three runs, dated.

## Sizing

Large. D is promotion work whose length depends on what the audits say;
D9 is the smallest piece and stands alone.
