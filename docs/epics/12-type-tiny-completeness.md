# Epic 12 — Type::Tiny completeness: check-guards, import-scoped vocabulary

> **Status:** scheduled (12th; independent — can run any time, pairs
> naturally with Epic 5's small-seam character).
> **Design owner-docs:** `docs/adr/type-constraints.md` (the landed
> `TypeConstraintOf` design — the foundation everything here projects
> through), `frameworks/type-tiny.rhai` (the vocabulary plugin),
> `docs/adr/flow-narrowing.md` + `docs/adr/narrowing-diagnostics.md`
> (the lattice the guards feed).

## What is ALREADY landed (do not rebuild)

- `InferredType::TypeConstraintOf(inner)` — a constraint is a VALUE over
  the type it constrains, never conflated with it; consumers project via
  `constrained_inner()`.
- The vocabulary plugin: `InstanceOf` / `ConsumerOf`, `Maybe[T]` →
  `Optional<T>`, the 0-arity base constants (`Str`/`Int`/`Num`/
  `ArrayRef`/…) folding to their reps — both `isa` spellings (quoted
  string and bareword constructor) type accessors identically.
- The constraint-name gate is import-scoped
  (`Builder::constraint_name_imported`): a call types as a constraint
  only where the enclosing file imported the name. That is what lets the
  vocabulary carry the 0-arity base constants without a local `sub Str`
  losing to them. Design in `adr/type-constraints.md`.
- Import vocabulary: `Types::Standard` / `Types::Common::{String,
  Numeric}` / `Types::Common` export lists including the
  `is_X`/`assert_X`/`to_X` companions, `-all`/`:all` expansion, and the
  BYO story for house type libraries (their plugin emits a
  `SyntheticUse`).

## Mission — what is NOT yet designed

1. **Check-function guards feed the narrowing lattice.**
   `if (is_ArrayRef($x)) { $x->[0] }` and `assert_Str($name); …` are
   type guards exactly like `ref $x eq 'ARRAY'` / `defined $x` — today
   they narrow nothing. The vocabulary already enumerates every
   `is_X`/`assert_X` name; the lattice already has the ops. Connect
   them, plugin-declared.
2. **The constraint-constructor gate becomes import-scoped.** The
   `type_constraint_names()` gate is global — ANY call named `Str`/`Int`
   anywhere types as a constraint, colliding with user subs. Scope it to
   packages that actually imported the name.
2. **Close the `completion-typetiny-imported-blessed` xfail** — a
   generic gap (imported names missing from bareword completion) that
   happens to be pinned on a Type::Tiny fixture.
3. **Doc hygiene:** `frameworks/type-tiny.rhai` cites a design doc that
   does not exist (`docs/prompt-type-constraint-types.md`). The design
   lives in `adr/type-constraints.md`; fix the pointer.

## Read first

1. `CLAUDE.md` — rules #1, #10; the narrowing and witness sections.
2. `docs/adr/type-constraints.md`, `docs/adr/flow-narrowing.md`.
3. `src/build/builder/narrowing.rs` — `recognize_guards` and the
   `GuardFact`/`NarrowOp` shapes. The truthiness recognizer
   (`NarrowOp::StripOptionalTruthy`) is the freshest example of
   extending it; `src/build/builder/narrowing_tests.rs` is its test
   pattern.
4. `frameworks/type-tiny.rhai` — `types_standard_exports()` (the
   `is_`/`assert_` companion generation) and `base_constant_type`.
6. `docs/prompt-cfg-tier.md` §3.3 and §8.1 — **this epic mints new
   `GuardFact`s over `NarrowSubject`s, which is exactly the
   representation that tier promotes.** Two obligations land on the
   arms written here; see below.

## Phase breakdown

### Phase A — `type_check_guards()` manifest + narrowing recognizer

1. New plugin manifest `type_check_guards()` returning
   `{ fn_name, constraint_name, asserts }` entries. **The plugin DERIVES
   them from the same base list that generates the exports** — one
   vocabulary, three projections (`is_X` → check-guard, `assert_X` →
   asserting guard, the export list) — never a second hand-kept table.
   Only names whose constraint folds to an expressible type contribute
   (ask `base_constant_type`; `is_Object` folds to nothing → omit).
2. Core resolves each entry's `constraint_name` → `InferredType` through
   the EXISTING `type_constraint_inner` fold (empty params) at
   registry-bake time — **no second name→type mapping in core.** Bake
   the resolved map onto the FA's plugin lane, serde-default,
   `EXTRACT_VERSION` bump.
3. `recognize_guards` gains a function-call arm: a condition
   `is_X($subject)` whose name is in the baked map and whose argument is
   a narrowable subject yields
   `GuardFact { subject, op: To(resolved), asserts_when_true: true }`.
   `HashRef`/`ArrayRef`/`CodeRef` reps → `To(rep)` (same as `ref…eq`);
   `Str`/`Num` → `To(String/Numeric)`. **Negation, polarity and
   elsif-chains come free from the existing machinery** — do not
   re-implement them.
4. `assert_X($subject);` at statement level: the fall-through narrows
   (assert dies otherwise). Reuse the early-exit statement machinery —
   the assert IS the guard and the region is the rest of the block.
   Postfix and bare-statement forms.
5. **Object form:** `$type->check($x)` where the invocant types
   `TypeConstraintOf(T)` narrows `$x` to `T` in the guarded region;
   `$type->assert_valid($x)` narrows the fall-through. The recognizer
   asks the invocant's TYPE — **no name matching on `$type`** (rule
   #10). This is the payoff of the ADR's "a constraint is a value"
   decision.
6. **Two forward obligations from the CFG tier, both free here and
   unrecoverable later** (`prompt-cfg-tier.md` §3.3, §8.1):
   - **`NarrowSubject` becomes `Place`** — spelling→binding, flat key→
     path steps. The new arms should take the subject from
     `narrow_subject_of` and never re-spell it as a string; a `String`
     subject minted here is one more of the three spellings Epic 16
     Phase B has to converge.
   - **P2: keep the condition's symbolic form.** A check-guard's
     condition is `is_X($subject)` — a shape that fits the decidable
     fragment trivially (equality over a symbolic value). Record it
     under the atom as the dormant `SymExpr` payload rather than
     flattening straight to the resolved `NarrowOp`. Nothing reads it
     yet; that is the point.
7. **Acceptance:** unit tests per form (`is_ArrayRef` if/unless/postfix;
   `assert_Str` fall-through; the `->check` object form; a NON-imported
   `is_Foo` user sub narrows nothing); **the deref-shape
   diagnostic composes** (an `is_ArrayRef`-guarded `$x->{k}` hash deref
   flags) — one test proving the LATTICE, not just the type, sees the
   guard. Substrate audit: guard-lint counts move only DOWN; triage
   anything up.

### Phase C — imported names in bareword completion

Bareword/function completion candidates fold in
`analysis.imports[].imported_symbols` (goto-def and diagnostics already
consult them; completion does not). **Route through the CandidateSet's
completion sources (`complete()`), not a handler-side append.** Flip the
`completion-typetiny-imported-blessed` xfail row to gold, and add an
`exact_labels`/`max_items` noise-guard row alongside — see the Scaling
beat.

### Phase D — doc hygiene

Fix the stale pointer in `frameworks/type-tiny.rhai` →
`docs/adr/type-constraints.md` (+ this epic). Note: touching the rhai
changes the plugin fingerprint, so caches self-invalidate — expected and
harmless, but say it in the PR so nobody debugs it.

## Non-goals

- `ArrayRef[T]` / `HashRef[T]` ELEMENT typing — parked with
  sequence-types (`prompt-sequence-types.md`, QA pulls).
- House Type::Library generators (runtime `setup_import_methods`) — the
  runtime-export-generator open problem. The BYO `SyntheticUse` plugin
  story is the supported answer. **Do not attempt static execution.**
- Coercions (`to_X`, `coerce => …`) — recognized as exports for
  suppression only; no semantic modeling.
- `Enum[…]`/`Dict[…]`/`Tuple[…]` beyond what already folds — each is its
  own design conversation; decline cleanly (the fold already returns
  unit for unhandled shapes).

## Language-pack beat

**Perl-only in its vocabulary; cross-language in the seam it extends —
and the second half is the part to protect.**

`recognize_guards` / `GuardFact` / `NarrowOp` are the narrowing lattice,
and the lattice is explicitly cross-language. From the cpp arc's own
record (`cpp-golive-map.md`, ARC 2 E): *"a narrowing is a SCOPED
ASSERTION over a region, not a temporal value — must be explicitly
region-bounded… cutoff is the shared `earliest_rebind_in`, edge-driven,
consumed by Perl AND the query engine (cross-language)."* C++ narrowing
rides the same machinery; `instanceof`-shaped narrowing in any pack
language would too.

Obligations:

1. **The new recognizer arm is a Perl-syntax arm producing a
   language-neutral `GuardFact`.** That split must stay clean: the
   function-call shape recognition is Perl's; the `GuardFact` it
   produces is the lattice's. If the arm needs to add a field to
   `GuardFact` or `NarrowOp`, that field is now cross-language — check
   `cargo test --features cpp` and the cpp gold rows, because a
   lattice change that breaks C++ narrowing is invisible to every Perl
   test.
2. **Region-boundedness is not optional.** Phase A step 4 narrows "the
   rest of the block" after an `assert_X`. That must go through the
   existing region machinery and the shared `earliest_rebind_in`
   cutoff — a hand-rolled "until the end of the block" is exactly the
   temporal-value mistake the cpp arc already made and fixed.
3. **A future pack language gets check-guards for free if the manifest
   is the only Perl-specific part.** `type_check_guards()` is a `.rhai`
   plugin hook, and pack languages have no rhai tier — so the baked map
   on the FA should be populated by *whoever knows*, with the manifest
   as one producer. Do not gate the recognizer on "a plugin declared
   this"; gate it on "the baked map contains this name". That one word
   is the difference between a reusable seam and a Perl seam.
4. Type::Tiny's vocabulary itself does not generalize, and should not
   try to. `is_ArrayRef` is a Perl library's function name.

## Scaling beat

**Two costs: a per-condition map lookup in the walk, and a completion
list that grows.**

1. **Phase A adds a lookup to every function-call condition the walk
   sees.** That is a hot path — `recognize_guards` runs per condition
   per file. The baked map must be a fast lookup (interned or
   hash-by-`&str`), baked once per file, not rebuilt per condition.
   Check `--timings` on the substrate: the slowest-modules tail must not
   move beyond noise. Use `bphase!` if you need per-file attribution —
   it routes to `ghost_stats::timed` and accumulates, which is the right
   shape for a per-file region; **a printed line per entry is not**
   (`adr/instrument-blindness.md`).
2. **Phase A's payoff is a scaling win, not just a correctness one.**
   More narrowing means fewer `Optional`/unknown beliefs, which means
   fewer speculative chases downstream. Report the guard-lint deltas and
   note any change in the conclusion bake's open-reason distribution —
   better narrowing should move keys out of `NoAnswerOpaque`.
3. **Phase C is the risky one.** It folds every imported symbol into
   bareword completion. `Types::Standard -all` alone is a large export
   list, and a file importing several libraries has hundreds. Completion
   payload is a measured, previously-regressed axis: 7.29 MB / 236 ms
   per keystroke at 138k files, fixed to 55.9 KB / 4 ms (`b6312ea2`,
   2026-08-17).
   - Ship the `exact_labels`/`max_items` noise-guard row.
   - **If the candidate flood is bad, complete only on ≥1 typed
     character with a prefix match, and record the policy** — this is
     exactly what the C++ side does for macros and cross-file
     identifiers (prefix-gated server-side, `is_incomplete: true` so
     clients re-request per keystroke). Reuse that pattern rather than
     inventing one.
   - Measure the payload bytes with `bench/lsp_bench.py`, three runs,
     dated.
4. **The import-scoped gate rejects before the expensive half** — the
   param extraction and the rhai fold — so a guard map consulted after
   it inherits that filter. Confirm the recognizer sits on the same
   side of it.

## Verification gate

`cargo test` (both feature sets — the lattice is shared) · gold 0 FAIL /
0 XPASS with the Phase-C promotion · `./e2e/run.sh` · substrate audit
with Phase A/B deltas individually triaged and always-on parity ·
`--timings` tail unmoved · completion payload bytes for Phase C, three
runs, dated.

## Sizing

Small-to-medium. A is the core (recognizer + manifest); C and D are
small. One PR for A, one for C+D works.
