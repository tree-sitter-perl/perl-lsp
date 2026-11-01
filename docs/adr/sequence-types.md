# ADR: Sequence types — positional containers as bag witnesses

`@arr`, `push @arr, X`, `$arr[N]`, list literals, and the list-
context operators (`map`, `grep`, `sort`, `reverse`, splat) are all
positional containers. This ADR is the data model + walker contract;
`prompt-sequence-types.md` is the design corpus for the residual
phases (full shape lattice, cross-method contributions, framework
integration).

## Decisions worth keeping

### `Sequence(Vec<InferredType>)` is an `InferredType` variant

```rust
pub enum InferredType {
    // … existing variants
    Sequence(Vec<InferredType>),
}

impl InferredType {
    pub fn element_at(&self, i: i32) -> Option<&InferredType>;
}
```

One variant covers `@arr` and `[a, b, c]` — no Array vs ArrayRef
carrier distinction. Perl's list-vs-scalar context is a property
of the *expression's evaluation context*, not the contained data.
The spike ships tuple shape only; the residual phases extend the
payload to the full lattice (`Empty` / `Homogeneous` / `Tuple` /
`CycleTuple` / `Heterogeneous`).

Placed at the END of `InferredType` so cached-blob bincode variant
indices stay stable; new variants always go at the end and bump
`EXTRACT_VERSION`.

### Walker emits on `Variable`, projection lives in `resolve_expression_type`

The walker pushes one `Variable{name, scope} + InferredType(Sequence(…))`
witness per array, deferred to a post-fold pass
(`emit_array_push_witnesses`) so method-call return types are
resolvable by the time each contribution is queried.

`resolve_expression_type` gains an `array_element_expression` arm
that queries the array's witness and projects via `element_at`.
Cursor-context completion falls back to this arm when no witness is
recorded at the node's span; `method_call_invocant_classes` (hover,
dispatch) reads the same witness tree-free, through
`expr_type_at_span`.

No new bag attachment was needed. `Variable{"@arr", scope}` is
already the canonical store; tuple-shape sequences ride a richer
payload there. `Container(ArrayId)` and `SequenceAccess(Span)`
attachments earn their entry in later phases when cross-method
contributions and reducer-time projection become load-bearing.

### List operators would ride a `SequenceTransform` payload

Proposed shape, not landed — the spike scope was the container only.

```rust
pub enum WitnessPayload {
    // … existing
    SequenceTransform { source: WitnessAttachment, op: SeqOp },
}

pub enum SeqOp {
    Map(WitnessAttachment),   // block's last-expr; its type is the new element
    Grep,                     // preserve element type, lose precise length
    Sort,                     // identity on element type AND shape
    Reverse,                  // reverse a Tuple; identity for Homogeneous
    Flatten,                  // `(@x, @y)` and splat contexts
}
```

A `SequenceTransformReducer` would claim the payload, recurse on
`source` via the registry, and apply the per-op rule. Nested
operators (`map { … } sort grep { … } @names`) would be nested
witnesses; the reducer would materialize innermost first.

New operator (`first`, `pairs`, `pairmap`) → one `SeqOp` variant +
one `match` arm, once this lands.

### Top-level scripts seed `current_package = Some("main")`

`Builder::new` seeds the implicit package. Without it, top-level
`Mojolicious::Lite` scripts never recorded their `use` lines in
`package_uses` and `Trigger::UsesModule` plugin triggers never
fired. Matches Perl's runtime semantics — every script starts in
`main`.

Hash-key owners on top-level subs carry `Some("main")`.

## Where this is going

Map / grep / sort / reverse / flatten lands as the `SequenceTransform`
shape above plus the reducer — concrete next step. Then
`Container(ArrayId)` for cross-method contributions (the
`$self->add(...)` / `$self->kids->[0]` split-sub case). Then
framework synthesis (`has 'kids'` returning slot shape).

Each extends the data model without retrofitting consumers.
`prompt-sequence-types.md` carries the per-phase scope.
