# Sequence Types — Residual Phases

> **Status: spike landed (`ec62653`), broader phases queued.**
>
> The core data model and walker contract live in
> `docs/adr/sequence-types.md`: `InferredType::Sequence(Vec<…>)`,
> tuple shape on `Variable{name, scope}`, `array_element_expression`
> projection in `resolve_expression_type`, the
> `SequenceTransform / SeqOp` pattern for list operators, the
> `package main` seed. This document is the design corpus for the
> phases that don't ship in the spike — full shape lattice,
> cross-method contributions, pipeline reducers, framework
> integration. Read the ADR first.
>
> Cross-refs:
> - `docs/adr/sequence-types.md` — what landed; the data-model contract.
> - `docs/adr/bag-canonical.md` — the unification foundation.
> - `CLAUDE.md` "Build pipeline phases" + "Worklist invariants".

## What's landed

The spike (`ec62653`) shipped the minimum-viable container:

- `InferredType::Sequence(Vec<InferredType>)` — tuple shape only.
- Walker pushes per-array `Variable{name, scope}` witnesses
  accumulated via `pending_array_pushes` + post-fold
  `emit_array_push_witnesses`.
- `array_element_expression` arm in `resolve_expression_type`
  projects via `element_at(i)`. Cursor-context completion AND
  `method_call_invocant_class_with_tree` (hover/dispatch) share it.
- Top-level scripts seed `current_package = Some("main")`.

What's NOT landed and what this prompt is for:

1. **Full shape lattice** — extend the payload to
   `Empty | Homogeneous(T) | Tuple | CycleTuple | Heterogeneous`
   with classification rules (LUB, period detection ≥4, reset
   semantics). Current payload is bare `Vec<InferredType>` which
   only models the `Tuple` shape.
2. **`Container(ArrayId)` attachment** for cross-method
   contributions — when `push @{$self->kids}, X` happens in one
   sub and `$self->kids->[0]` reads in another. Needs identity
   at emission time, not scope-keyed `Variable` witnesses.
3. **`SequenceAccess(Span)` attachment + ElementAtReducer** —
   reducer-time projection. Becomes load-bearing when method
   returns carry sequence shapes (e.g. a sub returns `@list`,
   the caller indexes into it without binding to a local first).
4. **Framework synthesis** — Mojo::Base / Moo `has 'kids'`
   accessors emit `SlotReturnGetter` so the getter's return
   type folds to the slot's `Sequence(...)` shape.
5. **Pipeline reducers** — `map` / `grep` / `sort` / `reverse` /
   `flatten`. Shape locked in `adr/sequence-types.md`
   (`SequenceTransform { source, op: SeqOp }` payload + one
   reducer). This prompt has the per-op folding rules.

Each is purely additive. Each is independently shippable.

## Data model

```rust
pub enum InferredType {
    // … existing variants except ArrayRef, which is removed.
    Sequence(ArrayShape),
}

pub enum ArrayShape {
    Empty,
    Homogeneous(Box<InferredType>),
    Tuple(Vec<InferredType>),       // length ≤ TUPLE_LIMIT, mixed types
    CycleTuple(Vec<InferredType>),  // period K, requires ≥ 2 full cycles
    Heterogeneous,                  // unit; no useful element type
}

const TUPLE_LIMIT: usize = 6;

impl ArrayShape {
    pub fn element_at(&self, index: i32) -> Option<InferredType>;
    pub fn iter_element(&self) -> Option<InferredType>;
}

pub enum ArrayId {
    Lexical { scope: ScopeId, name: String },
    HashSlot { container: String, key: String },
    Anonymous { origin: Span },
}

pub enum ContributionPosition {
    Index(i32),
    Tail,
    Head,
    Iter,
    Unknown,
}

pub enum AccessPosition {
    Index(i32),
    Iter,
    Splat,
}
```

`element_at` projects:
- `Empty | Heterogeneous` → `None`
- `Homogeneous(t)` → `Some(t)` for any index
- `Tuple(slots)` → bounds-checked indexing, supports negative
- `CycleTuple(period)` → modular indexing

`iter_element` returns the LUB of all element positions, `None`
when shape is Empty or Heterogeneous.

## Witness vocabulary

Two new attachments:

```rust
pub enum WitnessAttachment {
    // … existing
    Container(ArrayId),
    SequenceAccess(Span),
}
```

Three new observations:

```rust
pub enum TypeObservation {
    // … existing
    ArrayContribution { contributed: InferredType, position: ContributionPosition },
    ArrayReset,
    ArrayAccessAt { container: ArrayId, position: AccessPosition },
    SlotReturnGetter { container: ArrayId },  // for framework synthesis
}
```

`ArrayContribution` and `ArrayReset` attach to `Container(_)`.
`ArrayAccessAt` attaches to `SequenceAccess(_)`. `SlotReturnGetter`
attaches to `Symbol(_)` for synthesized accessors.

## Reducers

Three new reducers, each stateless, each claiming a precise
attachment + observation pair.

### ShapeReducer

Claims: `Container(_)` carrying `ArrayContribution` or `ArrayReset`.

Folds contributions through the temporal-filter rule (drop
witnesses past query point) and the reset-generation rule (drop
contributions older than the latest reset before query point), then
classifies:

```
classify(contribs):
  if empty           → Empty
  if all-tail/iter   → Homogeneous (or Heterogeneous if types disagree)
  if pure-indexed:
    per-slot LUB
    if all slots agree              → Homogeneous
    if length ≥ 4, period 2 detected → CycleTuple
    if length ≤ TUPLE_LIMIT, dense   → Tuple
    else                            → Heterogeneous
  if mixed indexed + tail            → collapse to Homogeneous (LUB)
```

Cross-method ordering not honored — across subs, all generations
contribute (LUB), since static call-graph analysis is out of scope.

### ElementAtReducer

Claims: `SequenceAccess(_)` carrying `ArrayAccessAt`.

Reads the source container's shape via `ctx.shape_of(container)`,
projects per the access position. `Iter` returns
`shape.iter_element()`; `Index(N)` returns `shape.element_at(N)`;
`Splat` returns `Sequence(shape)` (sub-sequence preserves shape for
v1 — slice-narrowing is a follow-on).

### SlotReturnReducer

Claims: `Symbol(_)` carrying `SlotReturnGetter`.

For synthesized accessors that return `$self->{KEY}`. Reads
`ctx.shape_of(container)` and lifts the result back as
`Sequence(shape)`. `Empty` shape → no useful refinement → yields to
the framework's baked default (next reducer in priority).

Registry order: PluginOverride first (priority), then SlotReturn
(framework-aware), then ShapeReducer / ElementAtReducer
(attachment-disjoint), then the rest.

## Builder emission contract

The walker observes; reducers fold. Emission rules (each is one
visitor or visitor-fragment):

| CST shape | Emit |
|---|---|
| `push @TARGET, $a, $b, …` | `ArrayContribution { contributed: type_of(a), position: Tail }` per arg, on `Container(resolve_array_id(TARGET))` |
| `unshift @TARGET, …` | Same, with `position: Head` |
| `[a, b, c]` | `ArrayContribution { contributed: type_of(slot_n), position: Index(n) }` per slot, on `Container(Anonymous { origin: literal_span })` |
| `my @x = (a, b, c)` | Same per slot, on `Container(Lexical { scope, name: "@x" })` |
| `@x = ()` / `$obj->{k} = []` | `ArrayReset` on the LHS's container |
| `@x = (a, b, c)` (non-fresh) | `ArrayReset` + per-slot `ArrayContribution(Index(n))` |
| `$arr[N]` / `$obj->{k}->[N]` | `ArrayAccessAt { container, position: Index(N) }` on `SequenceAccess(node_span)` |
| `for my $e (@arr)` | `ArrayAccessAt { container, position: Iter }` on the iter var's `Variable` attachment |
| `@arr[A..B]` | `ArrayAccessAt { ..., position: Splat }` on `SequenceAccess(node_span)` |

`type_of(node)` is the post-unification canonical query:
`bag_query(canonical_attachment(node))`. No procedural walker-side
inference is needed for sequences specifically.

`resolve_array_id(node)`: structural recognizer for the LHS of a
push or assignment site. Cases:

- `array(varname X)` → `Lexical { current_scope, "@X" }`
- `array(varname (block (... EXPR)))` → recurse on EXPR (the `@{ EXPR }` form)
- `array(varname (scalar X))` → `None` (alias through a scalar; lossy by design — see Phase 5 note on aliasing)
- `container_variable(varname X)` → `Lexical { current_scope, "@X" }` (the access form, `$arr[N]`)
- `anonymous_array_expression` → `Anonymous { origin: span }`
- `hash_element_expression` (with $self/__PACKAGE__ receiver, or a bag-resolvable receiver class) → `HashSlot { container: class, key }`
- `method_call_expression` where the method is a known framework getter for class C → look up the getter's `SlotReturnGetter` witness; collapse to its container id
- everything else → `None` (drop emission; no guess)

The accessor → slot collapse is the load-bearing piece for
`$self->kids` and `$self->{kids}` referring to the same container.
After unification, the lookup is a single `ctx.query` on the
method's Symbol attachment.

## Framework integration

Mojo::Base / Moo `has 'KEY'` synthesis emits two witnesses per
attribute (post-Step 3, framework synthesis is witness-driven, not
field-pinning):

1. **Getter symbol** carries `SlotReturnGetter { container: HashSlot { class, "KEY" } }`.
   The reducer projects the slot's folded shape back as the getter's
   return type. With no contributions, the slot's `Empty` shape
   yields to a fallback baked default (e.g. a typed Moo `isa => 'Str'`
   would emit a low-priority `InferredType::String` witness too).

2. **Writer symbol** (Mojo::Base / Moo fluent setter) emits a
   `FluentClass(class)` observation on its Symbol attachment.
   Same pattern as today's plugin override flow, just witness-shaped.

Synthesized HashKeyDef entries for constructor-key completion are
unaffected — they're already a separate concern.

## Phasing

Each phase is independently shippable, each delivers user-visible
value, each is purely additive over the spike's foundation.

### Phase 1 — Full shape lattice.

The spike's `Sequence(Vec<InferredType>)` only models a `Tuple`
shape. Extend to the full lattice:

```rust
pub enum InferredType {
    // …
    Sequence(ArrayShape),
}

pub enum ArrayShape {
    Empty,
    Homogeneous(Box<InferredType>),
    Tuple(Vec<InferredType>),       // ≤ TUPLE_LIMIT, mixed types
    CycleTuple(Vec<InferredType>),  // period K, requires ≥ 2 full cycles
    Heterogeneous,                  // unit; no useful element type
}

const TUPLE_LIMIT: usize = 6;
```

Migrate the spike's emission to push the classified shape: the
post-fold `emit_array_push_witnesses` classifies the accumulated
contributions, not just stores the `Vec`. Drop `InferredType::ArrayRef`
in the same pass — migrate consumers to `Sequence(ArrayShape::Empty)`
as the "we know it's array-shaped, contents unknown" answer.

Add a strict `unify_via_object_subsumption` helper (or rename
`resolve_return_type` if its semantics overlap) — the existing
"Object subsumes HashRef" laxness silently mis-classifies
`[User, Order]` slot LUBs.

**Cycle detection requires length ≥ 4.** Length 2 is
indistinguishable from a 2-tuple.

Acceptance: ~20 unit tests covering each shape variant, every
projection mode, the temporal filter, the reset-generation rule.
`ArrayRef` is gone. Tuple, Homogeneous, CycleTuple, Heterogeneous
all surface through `bag_query` at real-Perl-source access sites.

Estimated diff: ~400 lines added (the lattice + classification),
~50 deleted (ArrayRef migration).

### Phase 2 — `Container(ArrayId)` + `SequenceAccess(Span)` attachments.

The spike piggybacks on `Variable{name, scope}` which is fine for
in-sub `@arr`. Cross-method contributions need identity at
emission time, not scope-keyed lookup. Introduce:

```rust
pub enum WitnessAttachment {
    // …
    Container(ArrayId),
    SequenceAccess(Span),
}

pub enum ArrayId {
    Lexical { scope: ScopeId, name: String },
    HashSlot { container: String, key: String },  // $self->{kids}
    Anonymous { origin: Span },
}
```

Walker `resolve_array_id(node)` recognizes the LHS shape:
`array(varname X)` → `Lexical`; `hash_element_expression` →
`HashSlot`; `anonymous_array_expression` → `Anonymous`; method
call where the receiver is a known framework accessor →
collapse to the getter's slot.

Add a `ShapeReducer` claiming `Container(_) + ArrayContribution|ArrayReset`
and an `ElementAtReducer` claiming `SequenceAccess(_) + ArrayAccessAt`.
The walker still owns identity (no per-witness recomputation); the
reducer applies the lattice classifier from Phase 1.

Replace the spike's `array_element_expression` arm in
`resolve_expression_type` with `bag_query(SequenceAccess(span))` —
one query path, build-time + query-time identical.

**No framework integration in this phase** — `has` accessors keep
returning baked defaults until Phase 3.

Acceptance: `my @users; push @users, User->new; $users[0]` keeps
typing as User (regression coverage from the spike). Plus:
cross-method contributions in one file (`sub a { push @{$self->{x}}, … }
sub b { $self->{x}[-1] }`) type end-to-end. The `_route` plugin
override is removable for the single-file case.

Estimated diff: ~300 lines added (attachments + reducers +
emission helpers). The spike's walk-time projection arm is
replaced rather than augmented.

### Phase 3 — Framework integration.

Mojo::Base and Moo `has` synthesis emits `SlotReturnGetter` for
getters and `FluentClass` for writers. The `resolve_array_id`
helper learns the accessor → slot collapse via lookup of the
synthesized Symbol's witness.

Acceptance: a Mojo::Base class with `has 'kids'` and an
`add_child` that pushes onto `$self->{kids}` (or `$self->kids`)
makes `$self->kids->[-1]` resolve to the element type, with no
plugin override and no special-case wiring. The current
`mojolicious-routes::_route` override is provably retire-able for
the *single-file* case.

Estimated diff: ~150 lines added (synthesis emission + accessor
collapse), ~80 deleted (the `_route` override and any related
shims that become redundant).

### Phase 4 — Cross-file mutation effects (Regime 2).

Sub signatures gain mutation effects: alongside the return type, a
list of effects per parameter ("pushes type T at position Tail onto
arg-N's container," "resets arg-N's container," etc.). The walker
infers effects from the body — same machinery as return-type
inference, just claiming a different attachment.

Call sites read the callee's effect signature and synthesize
contributions on the resolved arg's container. Cross-file flow
goes through the existing module index — same channel return
types ride.

This is where `add_child` mutating `$self->children` from a
*different* method (or a *different file*) becomes visible. The
real-world `Mojolicious::Routes::Route::_route` chain types
end-to-end through this phase, eliminating the override entirely.

Acceptance: real Mojolicious source (under `~/.plenv/.../Mojolicious/Routes/Route.pm`)
parses such that `_route`'s return type folds to
`ClassName(Mojolicious::Routes::Route)` without a plugin override.

This is a real new design surface; it gets its own spec doc when
phase 3 lands. The shape and timing depend on what the unification
has finalized for cross-file effect propagation.

### Phase 5 — Pipeline reducers.

`@dst = map { BLOCK } @src` (and grep / sort / splat) emit a
`SequencePipeline { src, dst, kind, block }` observation on the
dst container. A new reducer pulls the src's shape and applies the
kind-specific transform (Map: replace element type with block's
return; Grep/Sort: preserve element type; Splat: union of src
element types). Composes with everything else.

Same machinery for `@all = (@a, @b)` concat — `Splat([a, b])` kind
on `dst`, reducer LUBs.

Estimated diff: ~250 lines, all additive. Tests cover the four
pipeline kinds against fold-known src shapes.

## Out of scope

These are real but separable design surfaces; each gets its own
spec when its turn comes:

- **`wantarray` / context-dispatched returns.** Same arity-axis
  trick, plus a context dimension. Mechanical to bolt on.
- **Tuple destructure for `my ($a, $b) = func()`.** Once Phase 5's
  pipeline reducers exist, list-context returns can be typed as
  `Sequence(Tuple([...]))`, and per-position scalar destructure
  consults the tuple shape. Modest extra emission rule.
- **Slice narrowing.** `@arr[0..2]` could project a sub-tuple from
  a Tuple shape. Phase 5+ extension to the splat reducer.
- **Aliasing through scalars across function boundaries.** Sharing
  `ArrayId` through a scalar parameter requires aliasing analysis
  beyond identity-at-emission-time. Effect signatures cover the
  common cases; full aliasing analysis is out of scope indefinitely.
- **Duck-tuple narrowing.** "I see different methods called on
  different positions; the type is whatever has all those methods."
  HoTT-y, defensible, deferred — no real-world Perl that the
  analyzer cares about needs it.

## Pickup checklist

If you're about to start a phase, the load-bearing facts:

- The data model is in `src/file_analysis.rs::InferredType`.
  `Sequence(Vec<InferredType>)` lives at the END of the enum;
  keep new variants at the end and bump `EXTRACT_VERSION` in
  `src/module_cache.rs`.
- Walker emission queues live on `Builder`. The spike uses
  `pending_array_pushes: Vec<(ScopeId, String, Vec<Span>)>` —
  Phase 2's `Container(ArrayId)` shift replaces the scope+name
  key with `ArrayId`.
- The post-fold pass is `Builder::emit_array_push_witnesses`,
  called after `fold_to_fixed_point` so method-call return types
  are resolvable.
- Walker queues anything `expr_payload` can't resolve at walk
  time via `unresolved_expr_nodes` — retried post-walk against
  the final symbol table + refs. New emission shapes inherit
  this for free.
- Test pattern: `spike_array_hop_with_helper_and_cross_file_completion`
  in `src/builder_tests.rs` shows the build-FA + module-index +
  resolve / complete / hover assertions.
