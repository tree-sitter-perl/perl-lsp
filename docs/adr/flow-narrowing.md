# ADR: Flow-sensitive narrowing

A guard (`$x->isa('Foo')`, `ref($x) eq 'HASH'`, `return unless defined
$x`) refines a subject's type over the region it dominates, then the
type widens back at the region's exit. Lives in its own file,
`src/builder/narrowing.rs` — a child module of `builder` so its
`impl Builder` methods keep private-field access while staying the single
tree-sitter consumer (rule #1: `build()` drives it).

## It's an emission feature, not a query change

The query path already does flow-sensitive lookup: on a `Variable`
attachment with a point, `ReducerRegistry` returns the **narrowest-span
`InferredType` witness containing the point**, skips witnesses whose span
*starts after* the point (temporal), and **excludes** witnesses whose
span doesn't contain it. That exclusion *is* the un-narrowing — a
witness scoped to the guarded region stops containing points past the
region, so the wide (zero-extent) witness wins again. Monotone bag, no
retraction. So narrowing only had to **emit** span-extent witnesses;
"widen at block exit" is span arithmetic, not a deletion.

The one trap: narrowing does **not** route through `push_type_constraint`
(which zero-extents the witness, `span.start..span.start`). A narrowing
witness must carry the **full region span** so narrowest-span-wins picks
it inside and scoped-exclusion drops it outside. Source tag
`Builder("narrowing")`, emitted in the live walk so it survives
enrichment truncation.

## Soundness: truncate at the first disturbance

A span-extent witness would shadow a *later* reassignment's temporal
witness, so the region is truncated at the first op that could disturb
the subject (`first_subject_write` / `first_place_invalidation`).
Conservative by construction — it under-narrows, never lies. The bias is
load-bearing for the diagnostics built on top: they miss some real bugs
but never invent a false `Undef`/`Optional`.

## `Unknown`: a rebind nothing can type still happened

Temporal ordering answers a read from the newest witness at or before it.
That is only right while every write leaves one. A rebind whose RHS nothing
can type (`$ct = f($_)` with `f` unresolved) would otherwise leave nothing,
and the read falls back on the newest belief it *can* see — the one the
write replaced. `my $ct = undef; … $ct = f(); defined $ct` read `Undef`
and every `defined` guard on it read as one that can never pass.

So such a rebind lands as `InferredType::Unknown`, through the same
`push_type_constraint` every typed rebind uses, at the same statement
start. It is a real answer, not an absence, and each half of that matters:

- **Latest-wins retires the prior belief.** `Unknown` subsumes only itself,
  so when the RHS resolves on a later fold iteration the real type lands
  on top of it; until then it is the standing answer.
- **The scope walk stops on it.** A scope that answers `None` lets the
  walk continue outward, so a shadowed inner `$x` with its belief gone
  would answer with the *outer* `$x`. `Some(Unknown)` is authoritative
  at the binding scope and the walk never reaches the namesake.
- **Consumers ask the value.** `InferredType::is_known()` is the one
  spelling of "no information"; display surfaces (hover, inlay hints,
  signature help) and the guard verdicts filter on it rather than
  matching the variant. Dispatch needs no gate: `class_name()` is `None`.

A rebind that may not run is old-or-new no matter what its RHS is, and
lands as `Unknown` outright. "May not run" is asked at two granularities,
both spelled once in `cst.rs` (one climb, two boundaries):

- **Relative to the block the witness lands on**
  (`is_conditionally_executed_in_block`): a statement modifier, a
  ternary arm, a short-circuit. `$x = undef if COND` anchors at the
  statement start, so the guard's own condition reads no value the write
  has not produced, and the successor reads no value it may never
  produce. The block's own `if`/loop does not count — inside the block,
  the block has run, so `if (…) { $x = Foo->new; $x->m }` still types
  the call.
- **Relative to the variable's binding scope.** A write inside a nested
  block sits on the block's scope, which a read after the block never
  walks through, so that read would keep the pre-block belief. The honest
  answer there is `old ⊔ new`, and no reducer can compute it: a reducer
  sees ONE attachment, and the write landed on the block's. So the
  emitter stands in for the join and puts `Unknown` on the binding scope
  — the one every later read in the variable's extent walks through.
  **This is an approximation of a join and is deleted when Epic 16's
  `JoinFold` lands** (`docs/epics/16-cfg-tier.md`, Phase C); its anchor
  is in that epic's table.

The class-identity axis honors the same order. A `ClassName` rebind also
pushes a `ClassAssertion`, and identity-over-rep lets that axis answer
ahead of the plain axis; `ClassIdentity` (the axis's one owner in
`FrameworkAwareTypeFold`) records where the assertion was made and a
plain-type write at or after it retires it — unless the class subsumes
the newcomer, a deref's bare `HashRef` being representation, not a new
value. So `my $x = Foo->new; $x = 'str'` reads `String`, and an
untypeable rebind reaches class-typed variables too.

Declarations are a first binding, not a rebind: one whose RHS nothing
can type stays absent. The rule is spelled once, in the fold's
assignment pass (`apply_chain_typing_assignments`); it keeps no record of
where a variable was reassigned, because the witness *is* that record.

## Subjects: variables and places, one keying

A guard subject is a variable (`$x`) or a **place** — a chain of stable
hash/array projections (`cst::canonical_place_path`). A projection is
stable when its subscript pins one slot: a constant key/index (`{a}`,
`[0]`) or a plain scalar (`{$k}`, `[$i]`), off a scalar root
(`$self->{a}{b}`, arrow form) or the named container itself
(`$h{a}`/`$h[0]`, direct form, root `%h`/`@h`). Both subject kinds are
keyed by the subject's **source spelling** on the same `Variable`
attachment, so the reducer, scope-walk, and narrow-filter are untouched —
a place witness is just a span-scoped slot witness. The place-vs-variable
distinction lives in exactly two spots: the truncation rule and the one
`method_call_invocant_class` query seam (which fires for any invocant
carrying a subscript — `->` / `{` / `[`). Truncation generalizes to
multi-hop as "an opaque use of any **proper prefix** of the place (root,
intermediate element, or — for a dynamic-key place — a key scalar)
disturbs it, unless that prefix is the base of a longer access (a read)."

Accessor places (`$self->name`) are out: an accessor isn't a stable slot
(it can return a different object per call, with side effects), so
soundness needs a stricter no-call-between-guard-and-use model.

## Polarity and the negative lattice

`GuardFact { subject, op, asserts_when_true }`; `op` is `NarrowOp::To(T)`
(`isa`/`ref-eq`) or `StripOptional` (`defined`/`blessed`). The region a
guard dominates either asserts the guard TRUE or FALSE;
`op_for_region(holds)` returns the fact's op when polarity matches, else
`op.negated()`. `To(_).negated()` is `None` ("not Foo" has no positive
lookup target → emit nothing, stay wide); `StripOptional.negated()` is
`To(Undef)`. So the `defined`-family negatives (`else` of `if (defined
$x)`, `return if defined $x`, `unless (defined $x)`) narrow to the bottom
`Undef`, lighting up the early-exit rows **for free** through the
existing polarity plumbing. `narrow_block_guard` walks the whole
`if`/`elsif`*/`else` chain: each arm narrows by its own condition plus the
cumulative negation of every preceding condition, so an elsif-chain `else`
(and the intervening elsif blocks) light up the `defined`-family negatives
too — non-representable negations (`isa`) contribute nothing, making the
cross-condition intersection automatic. General negation stays parked
(see `optional-types.md`).

## `defined`/`blessed` strip via a re-emittable fold pass

`StripOptional` can't resolve at walk time: the subject's `Optional`
often comes from a sub return that only converges during the fold. So
`recognize_*` records the guard (subject, region, and the guard's own
`query_point` — *before* the region, so the read sees the un-narrowed
type and the pass's own output can't feed back), and
`emit_defined_narrowing_witnesses` re-derives `Optional<T> → T` each fold
iteration (clear-and-emit on tag `defined_narrowing`).

## Forward work

The remaining residuals — accessor places and the general
`Not`/`Difference` negation tier — live in
`docs/prompt-flow-narrowing.md` (which also records the dynamic-key
soundness knob, Option A vs B). Diagnostics the lattice enables:
`docs/adr/narrowing-diagnostics.md`.
