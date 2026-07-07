# Epic 13 — Type::Tiny completeness: check-guards, import-scoped vocabulary

> **Status:** scheduled (13th; independent — can run any time, pairs
> naturally with Epic 4's small-seam character).
> **Design owner-docs:** `docs/adr/type-constraints.md` (the landed
> `TypeConstraintOf` design — the foundation everything here projects
> through), `frameworks/type-tiny.rhai` (the vocabulary plugin),
> `docs/adr/flow-narrowing.md` + `docs/adr/narrowing-diagnostics.md`
> (the lattice the guards feed).

## What is ALREADY designed and landed (do not rebuild)

- `InferredType::TypeConstraintOf(inner)` — a constraint is a VALUE
  over the type it constrains, never conflated with it; consumers
  project via `constrained_inner()` (`adr/type-constraints.md`).
- The vocabulary plugin: `InstanceOf`/`ConsumerOf`, `Maybe[T]` →
  `Optional<T>`, the 0-arity base constants (`Str`/`Int`/`Num`/
  `ArrayRef`/…) folding to their reps — both `isa` spellings (quoted
  string and bareword constructor) type accessors identically.
- Import vocabulary: `Types::Standard` / `Types::Common::{String,
  Numeric}` / `Types::Common` full export lists incl. the
  `is_X`/`assert_X`/`to_X` companions, `-all`/`:all` expansion, and
  the BYO story for house type libraries (their plugin emits
  `SyntheticUse "Types::Standard"`).

## Mission — what is NOT yet designed, in one epic

1. **Check-function guards feed the narrowing lattice.**
   `if (is_ArrayRef($x)) { $x->[0] }` and
   `assert_Str($name); …` are type guards exactly like
   `ref $x eq 'ARRAY'` / `defined $x` — today they narrow nothing.
   The vocabulary already enumerates every `is_X`/`assert_X` name;
   the lattice already has the ops. Connect them, plugin-declared.
2. **The constraint-constructor gate becomes import-scoped.** The
   `type_constraint_names()` gate is global ("first cut" caveat on the
   trait doc, `src/plugin/mod.rs`): ANY call named `Str`/`Int`
   anywhere types as a constraint, colliding with user subs. Scope it
   to packages that actually imported the name.
3. **Close the `completion-typetiny-imported-blessed` xfail** — a
   generic completion gap (imported names missing from bareword
   completion) that happens to be pinned on a Type::Tiny fixture.
4. **Doc hygiene:** `frameworks/type-tiny.rhai` cites
   `docs/prompt-type-constraint-types.md`, which does not exist —
   the design lives in `adr/type-constraints.md`; fix the pointer
   (this epic's README-coverage update is where Type::Tiny's
   disposition now lives).

## Read first

1. `CLAUDE.md` — rules #1, #10; the narrowing/witness sections.
2. `docs/adr/type-constraints.md`, `docs/adr/flow-narrowing.md`.
3. `src/builder/narrowing.rs` — `recognize_guards` and the
   `GuardFact`/`NarrowOp` shapes (the truthiness recognizer added
   July 2026 is the freshest example of extending it).
4. `frameworks/type-tiny.rhai` — `types_standard_exports()` (the
   `is_`/`assert_` companion generation) and `base_constant_type`.

## Phase breakdown

### Phase A — `type_check_guards()` manifest + narrowing recognizer

1. New plugin manifest `type_check_guards()` returning
   `{ fn_name, constraint_name, asserts }` entries. The plugin
   DERIVES them from the same base list that generates the exports —
   one vocabulary, three projections (`is_X` → check-guard,
   `assert_X` → asserting guard, the export list) — never a second
   hand-kept table. Only names whose constraint folds to an
   expressible type contribute (ask `base_constant_type`; `is_Object`
   etc. fold to nothing → omit).
2. Core resolves each entry's `constraint_name` → `InferredType`
   through the EXISTING `type_constraint_inner` fold (empty params) at
   registry-load/bake time — no second name→type mapping in core.
   Bake the resolved map onto `FileAnalysis` (serde-default;
   EXTRACT_VERSION bump) like `meta_methods`.
3. `recognize_guards` gains a function-call arm: a
   condition `is_X($subject)` whose name is in the baked map and whose
   argument is a narrowable subject (`narrow_subject_of`) yields
   `GuardFact { subject, op: To(resolved) or the StripOptional analog
   for defined-like reps, asserts_when_true: true }`. `HashRef`/
   `ArrayRef`/`CodeRef` reps → `To(rep)` (same as `ref…eq`);
   `Str`/`Num` → `To(String/Numeric)`; negation/polarity/elsif-chains
   come free from the existing machinery.
4. `assert_X($subject);` at statement level: the fall-through narrows
   (assert dies otherwise) — reuse the early-exit statement machinery
   (`narrow_block_remainder`'s shape; the assert IS the guard and the
   region is the rest of the block). Postfix and bare-statement forms.
5. **Object form:** `$type->check($x)` where the invocant types
   `TypeConstraintOf(T)` narrows `$x` to `T` in the guarded region;
   `$type->assert_valid($x)` narrows the fall-through. Recognizer asks
   the invocant's type — no name matching on `$type` (rule #10). This
   is the payoff of the ADR's "constraint is a value" decision.
6. **Acceptance:** unit tests per form (`is_ArrayRef` if/unless/
   postfix; `assert_Str` fall-through; `->check` object form;
   a NON-imported `is_Foo` user sub narrows nothing — see Phase B);
   the D6 deref-shape diagnostic composes (an `is_ArrayRef`-guarded
   `$x->{k}` hash deref flags) — one test proving the lattice, not
   just the type, sees the guard. Substrate audit: guard-lint counts
   move only DOWN (new narrowing can only remove false "maybe undef"/
   unresolved noise) — triage anything up.

### Phase B — import-scoped constraint gate

1. The builder's `type_constraint_names` gate (grep
   `self.type_constraint_names.contains` in `src/builder.rs`) and the
   new Phase-A guard map both consult per-package import state: the
   name must be imported in the enclosing package (literal qw-list,
   meta-import expansion, or SyntheticUse — all already recorded on
   `imports` / handled by the plugin's `on_use`).
2. Keep a compatibility carve-out ONLY if the substrate shows real
   code using the constructors without importable evidence (measure
   first; the expected answer is no carve-out needed — Type::Tiny
   constants MUST be imported to compile).
3. **Acceptance:** a user package with its own `sub Str` — calls type
   as the sub's return, not a constraint; the existing bareword-isa
   tests still green (they `use Types::Standard qw/…/` properly);
   substrate audit at parity-or-better.

### Phase C — imported names in bareword completion

Per the KNOWN-GAPS fix sketch: bareword/function completion candidates
fold in `analysis.imports[].imported_symbols` (goto-def and
diagnostics already consult them; completion doesn't). Route through
the CandidateSet's completion sources (`complete()` — see the ADR's
honest-boundary list) — not a handler-side append. Flip the
`completion-typetiny-imported-blessed` xfail row to gold. Add an
`exact_labels`/`max_items` noise-guard row alongside (imports can be
large; if the candidate flood is bad, complete only on ≥1 typed char
prefix match and record the policy).

### Phase D — doc hygiene

Fix the stale `prompt-type-constraint-types.md` pointer in
`frameworks/type-tiny.rhai` → `docs/adr/type-constraints.md` (+ this
epic). Note: touching the rhai changes the plugin fingerprint →
caches self-invalidate; expected, harmless.

## Non-goals

- `ArrayRef[T]` / `HashRef[T]` ELEMENT typing — parked with
  sequence-types phase 3 (`prompt-sequence-types.md`, QA pulls).
- House Type::Library generators (`Clove::Types`-style runtime
  `setup_import_methods`) — the runtime-export-generator open problem
  (`open-problems.md`); the BYO SyntheticUse plugin story is the
  supported answer. Do not attempt static execution.
- Coercions (`to_X`, `coerce => …`) — recognized as exports for
  suppression only; no semantic modeling this epic.
- `Enum[…]`/`Dict[…]`/`Tuple[…]` semantics beyond what already folds —
  each is its own design conversation; decline cleanly (the fold
  already returns unit for unhandled shapes).

## Verification gate

The standard gate (README house rules): cargo test, gold 0 FAIL /
0 XPASS with the Phase-C promotion, substrate audit — Phase A/B deltas
individually triaged, always-on parity.

## Sizing

Small-to-medium. A is the core (recognizer + manifest); B is a
contained gate change with a measurement step; C/D are small. One PR
for A+B, one for C+D works.
