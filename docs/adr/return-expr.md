# ADR: `ReturnExpr` — receiver-relative return types

A sub's return type often depends on the call: DBIC `find` returns
the receiver's row class, Mojo `has`-synthesized writers return the
receiver itself, an arity-discriminated sub returns one type at
zero args and another at ≥1. Three different answers, one shape:
"the type is a function of the call." Pre-`ReturnExpr`, each got
its own machinery.

The pre-existing pieces:

- **Per-method projection** lived call-side. `extract_resultset_
  parametric` emitted `Parametric(RowOf(self))` directly on each
  `Expression(refidx)` for `find` / `single` / etc., and
  `ParametricType::return_projection` was a per-flavor lookup
  table. Coderef-of-method (`\&MyRS::find; $cb->($rs, ...)`) and
  dynamic-method (`$rs->$cb()`) routed around the call-site
  emitter and silently lost the projection.
- **Arity dispatch** lived in `FluentArityDispatch` — a bespoke
  reducer claiming `TypeObservation::ArityReturn { arg_count,
  return_type }` witnesses keyed on `Symbol(_)` and
  `PackageSymbol{package, name}`. Each framework's accessor synth
  pushed its own observations; `SubReturnReducer` had an
  `arity_hint.is_some()` short-circuit so getter-vs-writer
  dispatch reached the class-keyed arm.
- **Two declaration sites per Mojo accessor.** Walk-time push the
  per-Symbol getter, push the per-Symbol writer, push the per-
  class arity union. Each duplicated the framework's policy.

All three were the same fact: **the return depends on the call.**
The encoding made them look different — one as a value-side type
(`Parametric(RowOf(_))`), one as an observation variant, one as a
class-keyed union — and the analyzer paid for the pretense at
every consumer.

## Decisions worth keeping

### `ReturnExpr` is a payload, not a type

```rust
pub enum WitnessPayload {
    // ... existing variants ...
    ReturnExpr(ReturnExpr),
}

pub enum ReturnExpr {
    Concrete(InferredType),
    Receiver,
    ReceiverOr(InferredType),                // fallback when there's no call-site receiver
    Operator(ParametricOp),                 // RowOf(Box<ReturnExpr>)
    UnionOnArgs { branches: Vec<(ArgGuard, ReturnExpr)> },
    Arg(u32),                               // n-th call argument, the positional mirror of Receiver
}

pub enum ArgGuard { Empty, Exact(u32), AtLeast(N), AtMost(N), Any }
```

Subs publish one `ReturnExpr` on whichever attachment carries the
answer (`Symbol(sid)` for per-sym, `PackageSymbol{package, name}`
for cross-symbol class-keyed dispatch). `ReturnExprReducer`
evaluates against a `ReducerQuery` — substituting `q.receiver` for
`Receiver` placeholders, dispatching `UnionOnArgs` against
`q.arity_hint`, and recursing through `Operator` arms. The
evaluator is pure substitution; the only context it reads is
`q.receiver` / `q.arity_hint`.

Putting it on the witness payload (not on `InferredType`) is
load-bearing. `InferredType` is the value side — what a variable
holds. `ReturnExpr` is the rule side — what a callable returns.
Folding the rule into the value would re-create the consumer-
side projection problem the parametric ADR solved: every reader
of `InferredType::Foo(ReturnExpr)` would need to ask "is this
fully-evaluated?" before consuming, and a missed projection
would silently surface as a type the analyzer can't name.

### `Receiver` is a placeholder, evaluated at the consumer

The chain typer threads each call's invocant as `q.receiver`.
`Receiver` evaluates to whatever the caller asks about — and the
same declaration answers the direct call (`$rs->find(...)`),
the coderef-of-method (`\&MyRS::find; $cb->($rs, ...)`), and the
dynamic-method (`$rs->$cb()`) routes uniformly. One declaration,
three call shapes, no per-shape emitter.

`q.receiver` defaults to `ClassName(class)` for class-keyed
lookups (`find_method_return_type` / `query_sub_return_type`)
when no explicit invocant is supplied, so introspection queries
on the symbol still answer the fluent-writer arm.

### `UnionOnArgs` is first-match, with `None` falling through Any → Empty

Per-call-site dispatch is strict: a concrete `q.arity_hint` runs
through the branches in declaration order, first match wins.
`Empty` matches `Some(0)` only; `AtLeast(N)` matches `Some(h ≥
N)`; `Any` matches anything concrete; `None` matches none of the
above directly.

A `None` hint arrives from sym introspection (`--dump-package`,
class-keyed cross-file lookup with no specific call site). The
union resolves it by preferring `Any` (the catch-all default),
falling back to `Empty` (the conventional primary for getter +
writer pairs). Without this, a writer's per-Symbol UnionOnArgs
would be unmatched at a `None` hint and the query would silently
return None — sym introspection would lose its answer.

`symbol_return_type_via_bag` further defaults `arity_hint` from
`params.len()` when the caller doesn't pin one, so a writer at
its native arity finds its own arm.

### `Operator(RowOf(_))` evaluates to the value-side parametric

The operator arm is a recursive ReturnExpr. `Operator(RowOf(
Receiver))` evaluates `Receiver` first (yielding the call's
invocant type), then wraps the result in `Parametric(RowOf(_))`.
The existing value-side accessors (`ParametricType::class_name`,
`hash_key_class`) handle consumption uniformly — the projection
rule lives in one place (the parametric ADR's per-flavor
methods), evaluated whether the value arrives via direct
emission or through `ReturnExpr` substitution.

### Class-scoped union via `publish_class_accessor_union`

When two symbols with the same name carry distinct per-arity
returns (Mojo getter + writer at arities 0 and 1), per-Symbol
attachments aren't enough — class-keyed lookups
(`PackageSymbol{package, name}`) need to disambiguate. The helper
emits one multi-arm `UnionOnArgs` on the class slot:

```
publish_class_accessor_union("level", vec![
    (Empty,        Concrete(String)),    // getter default
    (AtLeast(1),   Receiver),            // fluent writer
])
```

Moo / Moose accessors share return types between getter and
writer (the writer returns the same isa-typed value), so they
emit a single-arm `(Any, Concrete(t))` declaration. DBIC
relationship accessors and `add_columns` synthesize one Symbol
per accessor name, so the per-Symbol UnionOnArgs is the only
needed declaration — class-keyed lookups fall through to it via
the standard inheritance walks.

### Receiver identity in the cycle guard

`ReturnExprReducer`'s receiver substitution and `UnionOnArgs`
dispatch can produce different answers for the same attachment
under different `q.receiver` / `q.arity_hint`. The cycle-guard
key widens to `(bag_ptr, attachment, receiver_identity,
arity_hint)`, where `receiver_identity` is the receiver's full
structural identity (a Debug projection of the whole
`InferredType`), not a coarse discriminant — two different
`ClassName` receivers must stay distinct or the memo hands one
class's answer to another. Without the widening, inheritance
walks that visit the same class with different invocants would
short-circuit and return `None` on the second hop.

### Anon subs are `Symbol(_)`, not `Expr(_)`

`sub { ... }` literals get a synthetic `(anon)` Sub symbol;
`coderef_return_edge_for` routes them through `Symbol(sym_id)`
instead of `Expr(body_last)`. The walk-order is reordered so
assignment RHS visits run before the LHS's TC extraction,
guaranteeing the anon symbol exists when consumers read its
`return_edge`.

This collapses a third shape into the same attachment family.
Anon-closures with arity arms dispatch through their `Symbol`
edge exactly like named subs.

## One evaluator across every call shape

`emit_parametric_return_expr_decls` (`builder.rs`) declares per-flavor
projections once on `PackageSymbol{base, method}`;
`ParametricType::return_method_declarations` (`file_analysis.rs`) is the
per-flavor table; arity dispatch is `ReturnExpr::UnionOnArgs` evaluated by
`ReturnExprReducer` (`witnesses.rs`), which runs before `SubReturnReducer`
— the latter claims only a plain `Symbol + InferredType` attachment.
Walk time only classifies `ArityBranch`; types come from arm-fold at fold
time (`builder.rs`).

Three subsumption tests in `return_expr_tests.rs` pin the cross-
shape uniformity:

- `\&Foo::name; $cb->($foo)` resolves to the right Mojo arm.
- `\&MyRS::find; $cb->($rs, ...)` projects through `RowOf`.
- `$obj->$cb()` routes through the CodeRef edge with the right
  receiver substitution.

## Trade-offs

**Cycle-guard cost.** Widening the cycle key to include the
receiver's full identity + arity hint inflates the guard map under
deep inheritance chains. Acceptable today — hot-path bag queries
short-circuit on the first matching reducer and don't recurse
through the full registry. Coarsening the receiver slot to a
variant-only discriminant (or "any non-None vs None") is not a
safe pruning — two distinct receivers reaching the same attachment
would then share a memo entry, and the memo would hand one
receiver's answer to the other.

**Cycle-guard cost** (above) is the standing trade-off. Per-arm
return witnesses live on their own `SymbolReturnArm(sid)`
attachment, so `SubReturnReducer` claims plain `Symbol(_) +
InferredType` by attachment shape with no source-tag filter — the
"claim by attachment shape, not source tag" rule holds with no
exception.

**Cache invalidation.** `EXTRACT_VERSION` bumps on every payload /
observation shape change. Bumping is free; old blobs re-resolve
lazily.

## Where this is going

The receiver-relative shape is open under `ArgGuard`. New guard
kinds (type-of-arg dispatch, truthiness, `wantarray` context)
become new variants — `ReturnExprReducer` already handles the
union evaluation. Effects on `Operator` (`Promise<X>`,
`Try<X>`) compose by wrapping the inner evaluator. The
`prompt-sequence-types.md` lattice plugs in here when the
sequence flavors land.

The DBIC-as-plugin port (`prompt-dbic-as-plugin.md`) was gated
on this ADR. With `Operator(RowOf(Receiver))` declared once per
flavor and threaded through `PackageSymbol{base, method}`, the
plugin's emission becomes one `return_method_declarations()`
return — no per-call-site walker, no per-method allowlist. The
remaining gate is type-system encoding for axis dispatch.

## Test discipline

`return_expr_tests.rs` is the regression suite — red-pin tests
covering the three cross-shape routes that used to break
(coderef-of-method, dynamic-method, anon-closure) plus the
in-body arity-discriminated sub case.

`witnesses_tests.rs` covers the evaluator: `Concrete` /
`Receiver` / `UnionOnArgs` / `Operator(RowOf(_))` resolution
without the bag in the loop, so reducer logic is testable in
isolation.

External-behavior tests (`builder_tests.rs`,
`type_inference_invariants_tests.rs`) hold unchanged — the
fluent-chain assertions, the Mojo arity tests, and the DBIC
projection tests. Internal-shape pins on
`InferredType::Parametric(...)` from the parametric ADR still
hold; `ReturnExpr` payloads are tested separately.
