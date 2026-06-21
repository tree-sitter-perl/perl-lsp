# Optional / Maybe types — design

**Status: Phase 1 implementing.** The complement of flow narrowing
(`docs/prompt-flow-narrowing.md` §"Sum / optional types"): a function
that early-returns undef produces `Foo | undef`, which today collapses
to `None` (arm disagreement). `Optional(Box<InferredType>)` captures it,
and `defined $x` / `blessed $x` narrowing strips it back to `T` — the
two features are two halves of one story.

Scoped to **`Optional<T>` (value-or-undef), not arbitrary unions.**
`Foo | Bar` stays `None` until a motivating case lands. `Maybe<T>` is
the dominant Perl idiom and maps 1:1 onto Type::Tiny `Maybe[T]` /
`Optional[T]`.

## The lattice variant

`InferredType::Optional(Box<InferredType>)`, appended at the **END** of
the enum (`file_analysis.rs`) for bincode variant-index stability — same
discipline as `Sequence` / `TypeConstraintOf` / `BrandedRoute` /
`HashWithKeys`. Bump `EXTRACT_VERSION`.

Blast radius is small: exhaustive (`_`-less) matches on `InferredType`
are confined to `file_analysis.rs` and terminate in `_ =>` wildcards
(unlike `ParametricType`, which forbids `_`). So adding the variant does
not cascade compile errors.

**`class_name()` → `None` for `Optional(_)`** (no arm; falls through the
existing `_`). An `Optional<Foo>` is *not definitely* a `Foo` — letting
it dispatch as `Foo` is the exact unsoundness the variant exists to
surface. `is_object()` (`= class_name().is_some()`) is `false`
automatically; `hash_key_class()` → `None` automatically. The user is
meant to narrow (`defined`/`blessed`) first; the narrowing yields
`ClassName(Foo)`, which *does* dispatch. Dispatch-through-optional is a
deliberate non-feature.

## The join — where {T, undef} becomes Optional<T>

The shared join is `resolve_return_type` (`file_analysis.rs`). Today undef
arms are **invisible** to it: a `return undef` arm pushes an
`Edge(Expr(undef_span))` that materializes to nothing (the `undef`
rvalue has no `Expr` payload — `expr_payload` falls to `_ => None`), and
a bare `return;` pushes no arm at all. So `{Foo, undef}` looks like just
`{Foo}` → `Foo`, losing the optionality.

**Fix — make the undef arm legible without adding an `Undef` lattice
variant:** mark it with a witness **source tag** `Builder("undef_arm")`
on the existing `SymbolReturnArm(sid)` / `BranchArm(span)` attachment.
`publish_return_arm_witnesses` detects the bare-`return;` (`None` body)
and `undef_expression` body cases and pushes the marker. The fold then
counts undef-marker witnesses separately from typed arms and:

- **≥1 undef-arm AND value-arms resolve to a single non-optional `T`** →
  `Optional(Box::new(T))`.
- **no undef-arm** → unchanged (existing behavior; existing tests are a
  no-op for the new rule).
- **value-arms genuinely conflict** (`Foo` vs `Bar`) → still `None` (no
  arbitrary union this phase).
- **only undef-arms** → `None` (no `Unknown` to wrap; a sub that only
  `return undef` has no useful value type).

Keeping undef out of the lattice (only `Optional` enters) is the
load-bearing choice: a private `Undef` variant would leak into every
consumer.

## `subsumes_narrowing`

- `(Optional(a), Optional(b)) => a.subsumes_narrowing(b)` — recurse.
- `(a, Optional(b)) => a.subsumes_narrowing(b)` — a concrete `self` is at
  least as specific as an optional narrowing (narrowing already
  happened).
- `(Optional(_), concrete)` — default `false`: the optional does NOT
  subsume a concrete narrowing, so a `defined`/`blessed` guard's
  `Optional<T> → T` refinement wins. (No arm; the discriminant fallback
  is already `false`.)

## Phased plan

**Phase 1 (this doc's target):** `sub f { return undef unless $x;
return Foo->new }` ⇒ `Optional<ClassName(Foo)>`, all existing tests
green.
1. Append `Optional(Box<InferredType>)`; bump `EXTRACT_VERSION`.
2. `publish_return_arm_witnesses`: push `Builder("undef_arm")` markers
   for bare `return;` and `return undef`.
3. `resolve_return_type`: the partition rule above (thread the undef-arm
   signal in).
4. `SymbolReturnArmFold`: collect undef-marker count, pass to the join.
5. `subsumes_narrowing`: the two Optional arms.
6. `class_name()` untouched (Optional → `None`).
7. Tests: the target sub; existing arm-fold tests stay green.

**Phase 2 — light up narrowing (LANDED for `defined`/`blessed`):**
- `InferredType::optional_inner()` accessor; `GuardFact` carries a
  `NarrowOp { To(InferredType), StripOptional { query_point } }`. The
  `defined`/`blessed` guards (`func1op_call_expression` /
  `ambiguous_function_call_expression`) recognize their subject and emit
  `StripOptional`.
- Because the subject's `Optional` is often a sub return that only
  converges in the fold, the strip is a **re-emittable fold pass**
  (`emit_defined_narrowing_witnesses`, tag `defined_narrowing`): each
  iteration it reads the subject's type at `query_point` — the guard's
  own location, BEFORE the narrowed region, so the pass's own output is
  excluded (no oscillation) — and narrows the region to the inner when
  it's `Optional`. `blessed` ships as `defined`'s strip (the extra
  is-an-object precision has no lattice target yet).
- Still deferred: route `BranchArmFold` (ternary `$c ? Foo->new : undef`)
  and `SlotTypeFold` through the join so they *produce* Optional from a
  `{T, undef}` pair (needs the same undef-marker on branch/slot arms).

**Phase 3 — negative lattice (LANDED for the `defined` family):**
- `InferredType::Undef` (bottom), appended at the enum END; bump
  `EXTRACT_VERSION`. `class_name() → None`, `subsumes_narrowing` needs no
  arm (the discriminant fallback gives `(Undef, Undef) → true` and
  `false` elsewhere). Never produced by the return-arm join — purely a
  narrowing witness.
- `NarrowOp::negated()`: `StripOptional → To(Undef)`; `To(_) → None`
  (no representable complement). `GuardFact::op_for_region(holds)` picks
  the fact's op when its polarity matches, else the negation. So the
  early-exit negatives (`return if defined`, `unless (defined){}`) light
  up through the *existing* polarity plumbing; only the `else`-block emit
  (`trailing_else` on `conditional_statement`) was new. elsif-chain `else`
  (cumulative negation) is deferred — needs unions.
- General `Not`/`Difference` negation for `isa`/`ref-eq` negatives:
  **parked** — a negative class yields no positive lookup target, so no
  consumer (goto/completion/hover/dispatch) benefits. Unblock: a real
  union lattice + a consumer of negative facts.

**Phase 4 — Optional production + Type::Tiny:**
- **Ternary (LANDED):** `$c ? Foo->new : undef` ⇒ `Optional<Foo>`. The
  `undef` arm is marked like a `return undef` arm (`Builder("undef_arm")`
  Fact on `BranchArm(span)`); `BranchArmFold` keeps its **strict** arm
  agreement (a ternary wants exact agreement, not the return-arm join's
  loose hash/object subsumption — `$c ? Foo : Bar` stays `None`) and
  lifts the agreed `T` to `Optional<T>` when an undef arm is present.
  Composes with `defined`: `my $x = $c ? Foo->new : undef; return unless
  defined $x; $x->go` dispatches on `Foo`.
- Still deferred: `SlotTypeFold` ({T, undef} slot writes → Optional);
  bare `return;` as an undef arm (wider gold blast radius);
  `TypeProvenance::ReducerFold { reducer: "optional_join" }` for
  `--dump-package`; Type::Tiny `Maybe[T]` / `Optional[T]` →
  `Optional(Box<T>)`.

## Sequencing wrinkle (Phase 2+)

A `defined` narrowing whose subject's `Optional` comes from a sub return
resolved during `fold_to_fixed_point` (phase 6) can't be computed in the
live walk — the return type isn't in the bag yet. The `defined` emit for
that case moves to a post-fold pass. The `my ($x) = @_`-typed and
assignment-typed subjects work in the live walk.
