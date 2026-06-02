# Modeling optionalness / nullability

> **Status:** design note. The immediate `Maybe[InstanceOf['Foo']]` fix is
> **landed** as an *unwrap* (resolution-only) — no first-class type variant.
> This note records why, and what a first-class `Maybe` would buy if we ever
> want it. Recommendation up front: **do not add a variant speculatively.**

## What landed (the unwrap)

`has thing => (isa => Maybe[InstanceOf['Foo']])` now types the accessor `Foo`.
Mechanism (no new type variant):

- **Core** (`builder.rs::extract_constraint_params` / `constraint_param_for`):
  a constraint-constructor param that is itself a nested constructor
  (`Maybe[InstanceOf['Foo']]`'s single param is the call `InstanceOf['Foo']`)
  is typed *through the bag* — `emit_expr_witness(el); bag_query_expr_span(el)`
  — and lands in `ConstraintParam.ty` as `TypeConstraintOf(ClassName(Foo))`.
  This recurses to arbitrary depth because it reuses `expr_payload`, the same
  path the outer call walks. rule #1 holds: only the builder walks nodes.
- **Plugin** (`frameworks/type-tiny.rhai`): `Maybe` joins the
  `type_constraint_names` gate; its `type_constraint_inner` fold is a
  **passthrough** — it returns `constrained_inner(params[0].ty)`, the inner of
  T's constraint. The core re-wraps that in `TypeConstraintOf`, so
  `Maybe[InstanceOf['Foo']]` → `TypeConstraintOf(ClassName(Foo))`, identical to
  the bare `InstanceOf['Foo']`. The `has` isa→accessor projection
  (`constrained_inner().cloned()`) then yields `Foo`, no `Maybe`-specific code.
- A new rhai helper `constrained_inner(ty)` mirrors
  `InferredType::constrained_inner` so the plugin asks the value its question
  (rule #10) instead of destructuring the serde shape.

Net: `Maybe[T]` is **erased** to T for resolution purposes. The optionalness —
"the value might be undef" — is discarded. That is the whole question of this
note: is erasure enough, or do we want to *keep* the optionalness?

## The question: first-class `InferredType::Maybe(Box<InferredType>)`?

A first-class variant (call it `Maybe`/`Optional`/`Nullable`) would carry the
"might be undef" bit through the type lattice instead of erasing it. What would
it actually *buy*?

### Method dispatch
`$x->m` where `$x: Maybe[Foo]`. With erasure (today), `$x` types as `Foo` and
`->m` resolves against `Foo` — which is what you want for goto-def / hover /
completion. A first-class `Maybe` would dispatch the *same way* (resolve
against the inner `Foo`); the only delta is it could *also* know "this might be
undef." So for dispatch alone, first-class buys **nothing over erasure** —
resolution still goes to the inner. The inner-resolution is the 95% need and
erasure already serves it.

### Diagnostics (the one real payoff)
The thing erasure *can't* do: flag an **unguarded** method call on an optional.
`my $t = $obj->task; $t->process` when `task` is `Maybe[ProgressTask]` and
there's no intervening `if ($t)` / `return unless $t` / `$t //= ...`. That is a
genuine nullability lint (the Perl analogue of TypeScript's "Object is possibly
undefined"). It needs (a) the `Maybe` bit to survive on the type, and (b)
flow-sensitive guard tracking to *clear* the bit after a guard. We do not have
(b) today — the witness bag is largely flow-insensitive (it has a temporal
"skip witnesses past the query point" rule in `FrameworkAwareTypeFold`, but no
narrowing on guards). So a `Maybe` variant without guard-narrowing would either
never fire (useless) or fire on every optional access including guarded ones
(false-positive storm). The payoff is real but the prerequisite (flow
narrowing) is a much larger build than the variant itself.

### Hover display
`Foo?` or `Maybe[Foo]` instead of `Foo`. Mild nicety. Cheap *if* the variant
exists, worthless reason to add it on its own.

### Completion
No delta — completion on `$x->` wants the inner class's members regardless of
optionalness. Erasure already gives that.

**Verdict on the variant:** the only capability that needs it is the
unguarded-access diagnostic, and that capability is gated on flow-sensitive
guard narrowing we don't have. Adding the variant now is speculative — it would
ripple through every `match` on `InferredType` (the "never `_ =>`" invariant in
`file_analysis.rs` means every consumer must handle it), every reducer, the
bincode wire format (an `EXTRACT_VERSION` bump), and the `class_name()` /
`constrained_inner()` / `element_at()` projection family — all to carry a bit
nothing yet reads.

## Interaction with the existing type zoo

If a `Maybe` variant ever lands, the rule-#10 projections tell us exactly where
it slots:

- **`class_name()`** — `Maybe(inner)` should delegate to `inner.class_name()`
  (so dispatch transparently sees through it, same as `Parametric` delegates to
  its flavor). This is what makes "dispatch resolves against the inner" fall
  out without per-call-site branching.
- **`constrained_inner()`** — orthogonal. `Maybe` is not a constraint *object*;
  `TypeConstraintOf` is. `Maybe[InstanceOf[X]]` today produces
  `TypeConstraintOf(ClassName(X))` (the `Maybe`-ness folded away). If `Maybe`
  became first-class it would be the *inner* of the constraint:
  `TypeConstraintOf(Maybe(ClassName(X)))`, and the `has` projection would call
  `constrained_inner()` → `Maybe(ClassName(X))`, keeping the optional bit on the
  accessor return. The plugin fold would change from passthrough to
  `wrap_maybe(constrained_inner(params[0].ty))`.
- **`Sequence` / `Parametric`** — `Maybe(Sequence([...]))` and
  `Maybe(Parametric(ResultSet))` compose naturally; the `Maybe` is an outer
  wrapper that delegates the inner-shape questions inward. No conflict.
- A `maybe_inner()` accessor would join the projection family: "ask the value
  if it's optional, and what it's optional *of*." Consumers that care about
  nullability call it; everyone else calls `class_name()` and sees through.

The shape is clean — which is *why* we can defer it safely. Adding it later is
additive (a new variant + delegating projections), not a refactor.

## Where optionalness comes from (beyond `Maybe[...]`)

Type::Tiny `Maybe[T]` is the *declared* entry point, but optionalness is
pervasive in Perl idiom. A first-class model would want to source it from:

- **`//` defined-or:** `$x // $default` — the result is non-optional if either
  side is (the `//` *removes* optionalness). `my $y = $x // 0` narrows `$y` to
  non-optional even if `$x` was `Maybe`.
- **Conditional assignment:** `$x //= compute()` — same narrowing.
- **Guards:** `return unless $x;` / `if ($x) { ... }` / `$x or return;` — after
  the guard, `$x` is non-optional in the guarded region. This is the
  flow-narrowing prerequisite called out above.
- **Hashref-key access:** `$h->{maybe_absent}` is intrinsically optional (the
  key may not exist). We currently type it from the key's witnessed type with
  no optional bit. A real model would make every `->{k}` produce a `Maybe`.
- **Signature defaults:** `sub f ($x = undef)` — `$x` is optional; `sub f ($x =
  [])` — not. Param typing already sees the declaration site.
- **`first { } @list` / `List::Util` returns**, DBIC `->find` (row or undef),
  `->single`, etc. — library returns that are conventionally optional. Each
  would be a plugin-declared "returns Maybe" once the variant exists.

The breadth here is the argument *for* eventually modeling it (optionalness is
everywhere) and *against* doing it now (a half-model that only knows
`Maybe[...]` and ignores `//` / guards / `->{k}` would be more misleading than
no model — it would flag declared optionals while silently missing the idiomatic
ones, an inconsistent lint).

## Cost / benefit

| | Unwrap (landed) | First-class `Maybe` variant |
|---|---|---|
| `Maybe[InstanceOf[X]]` accessor → X | yes | yes |
| `$x->m` dispatch | yes (inner) | yes (inner) |
| unguarded-access diagnostic | no | only with flow narrowing |
| hover `Foo?` | no | yes (minor) |
| cost | ~40 LOC, no wire change | new variant: every `match`, reducers, bincode bump, projection family; **plus** flow-narrowing for the diagnostic to be usable; **plus** sourcing from `//`/guards/`->{k}` to not be a misleading partial model |

## Recommendation

**Ship the unwrap (done). Do not add `InferredType::Maybe` speculatively.**

Erasure satisfies every resolution need (goto-def, hover, completion, chain
dispatch) — the crm cases this round targeted. The *only* feature that wants a
first-class variant is the unguarded-optional-access diagnostic, and that
feature is not buildable on the variant alone: it needs flow-sensitive guard
narrowing the engine doesn't have, and to avoid being a misleading partial lint
it needs optionalness sourced from `//` / guards / `->{k}` / signature defaults,
not just `Maybe[...]`.

Revisit a first-class `Maybe` **only** when we commit to the nullability
diagnostic as a feature, and budget the flow-narrowing + idiom-sourcing with it.
At that point the slot is clean: a delegating `class_name()` plus a new
`maybe_inner()` projection, the plugin fold flips passthrough → wrap, and
`EXTRACT_VERSION` bumps. Until then the bit nothing reads is pure liability.
