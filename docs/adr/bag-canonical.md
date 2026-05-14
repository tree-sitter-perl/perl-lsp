# ADR: Bag-canonical typing — edges, not values

The analyzer had two truths about types: a walk-time eager path
(`infer_expression_type`, `Builder.resolved_returns` map, framework
synthesis writing `return_type` fields) and a bag-time path
(witnesses + reducers). Every new type feature paid a tax to bridge
them — a `deferred_X` field, a `seed_X_into_return_types` shim, a
writeback that re-published the registry's own answer as an
`InferredType` "cache." The four-step unification staircase
(`prompt-type-inference-unification.md`, retired) collapses both
paths into one.

## Decisions worth keeping

### Type production is `bag.push(...)`; consumption is `bag_query(att)`

There is no second source. Walk-time synthesis emits witnesses (a
source value the walker uniquely knows, or an edge to another
attachment). Every type read goes through `bag_query_attachment` →
`ReducerRegistry::query`. No helper returns a type without pushing
the witness first; no consumer reads a type that isn't materialized
through the registry.

`Builder::infer_expression_type` is deleted. The closed-syntax
cases (literals, anon-sub `CodeRef`, constructor → `ClassName`)
live in `expr_payload`'s match, called only by
`emit_expr_witness`. Callers that need the type at walk time do
`emit_expr_witness(node); bag_query_expr_span(node_to_span(node))`
— emit first, query after.

### Edges, not values

If a registry query on `Symbol(sid)` already resolves through an
edge chase, do NOT re-push the materialized `InferredType(t)` onto
the same attachment as a "cache." The registry's edge-chase IS the
canonical flow; pushing the value adds a parallel store that drifts
the moment any upstream changes.

Every published witness is either (a) a source the walker uniquely
knows (a string literal's `String`, a plugin's `Plugin +
InferredType(rt)`, a framework's `ReturnExpr(UnionOnArgs)`) or (b)
an `Edge` to another attachment.

### Implicit-last-statement return is an edge

```rust
// populate_witness_bag, for each Sub/Method scope with no explicit return:
bag.push(Symbol(sid) + Builder("implicit_return")
         + Edge(Expr(last_expr_span)));
```

The registry materializes through to the expression's type on each
query. Subs with explicit `return EXPR` route via the
`Edge(SymbolReturnArm(sid))` chain `publish_return_arm_witnesses`
emits — `SymbolReturnArmFold` claims first, so the implicit edge is
inert for those. `return_infos` is walk-final by the time
`populate_witness_bag` runs, so the gate `return_infos.is_empty()`
is a one-shot decision and the edge needs no clear-and-emit tag.

### Writeback mirrors `MethodOnClass` via `Edge(Symbol(sid))`

```rust
// write_back_sub_return_types, primary sym per (class, name):
bag.push(MethodOnClass{class, name} + Builder("local_return")
         + Edge(Symbol(sym.id)));
```

The class-keyed slot routes to the sym's own bag answer — whatever
shape that sym carries (UnionOnArgs, plugin override, arm fold,
implicit-return edge, delegation edge) materializes uniformly. No
value copying. `ReturnExpr` declarations on the same attachment
(from `publish_class_accessor_union`) claim first via
`ReturnExprReducer` when arity is known; the edge fills the
no-arity-hint slot and the fallback for non-arity-discriminated
subs.

`Builder.resolved_returns` is deleted. Walk-time synthesis pushes
`Symbol(sid) + Plugin + InferredType(rt)` directly (free-fn and
class-scoped both — the old `is_class_scoped` gate that relied on
`resolved_returns → writeback → MethodOnClass` is gone).

### Single registry entry point

`bag_query_attachment(att)` is the only "what's at this attachment"
call. Build-time, query-time, writeback, `fold_state_snapshot` —
all go through the registry. Termination protection (cycle visited
set, cross-bag recursion guard) lives once in `query_rec`; nothing
duplicates it.

`bag_query_expr_span(span)` and `bag_query_named_sub(name, arity)`
are thin sugar wrappers over the same call.

## Why "no exceptions, even for literals"

A string literal's `String` is the simplest case the walker knows.
Pushing it through the bag looks like ceremony. It isn't —
sequence types will fold `push @arr, "x"` by reading the `"x"`'s
`Expr(span)` witness. Bypassing the bag for "trivial" cases would
force every future consumer to either re-walk the CST or duplicate
the walker's branching. One witness emitted at one place; many
consumers query.

## Where this is going

Sequence types — the first feature built fully on this foundation
— landed in ~90 LOC (`ec62653`). See `adr/sequence-types.md` for
the data-model contract; `prompt-sequence-types.md` for the
residual phases. The shape held: zero new bag attachments, zero
new reducers, zero registry touches, every list operator extends
the same way (`SequenceTransform` payload + one `SeqOp` variant).

Residual Parts 1–5 (`prompt-type-inference-residual.md`) are
each a reducer + emitter pair. Same shape, same path.
