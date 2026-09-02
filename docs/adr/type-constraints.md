# ADR: `TypeConstraintOf` — a Type::Tiny constraint is not the class it constrains

`has x => (isa => InstanceOf['Foo'])` should give the accessor `x` a
return type of `Foo`, so `$self->x->method` resolves. The obvious fix
— map `InstanceOf['Foo']` to `ClassName(Foo)` — is wrong, and wrong in
a way that the rule-#10 "lossy projection" trap predicts.

`InstanceOf['Foo']` evaluates to a `Type::Tiny` *value*. You call
`->check` / `->assert_valid` / `->coerce` / `->name` on it, never
`Foo`'s methods. Typing it `ClassName(Foo)` conflates the constraint
with the thing it constrains: `$constraint->name` would wrongly
resolve against `Foo`. The accessor yields a `Foo`; the isa
*expression* is a constraint over `Foo`. Two types, related by "the
accessor yields what the constraint constrains."

## Decisions worth keeping

### The constraint is a wrapper type; the accessor projects through it

```rust
pub enum InferredType {
    // … existing variants
    TypeConstraintOf(Box<InferredType>),   // a Type::Tiny constraint over the inner
}
```

A value typed `TypeConstraintOf(X)` dispatches methods against
`Type::Tiny` (when that's indexed); the inner `X` is recoverable by
projection. `InferredType::constrained_inner()` is the accessor —
consumers ask the value, they never destructure the serde shape.
Pairs with `Sequence(Vec<_>)` / `Parametric(_)` (inner-carrying) — the
`…Of` reads as "constraint of `<inner>`".

### Core extracts the params; the plugin folds them to the inner

A type library exports a vocabulary of constructors of varying arity
(`ArrayRef` at 0, `InstanceOf['Foo']` at 1, `Enum['a','b']` at N).
Core can't enumerate shapes — it hands the plugin a param list and the
plugin folds it. The split obeys rule #1: only the builder walks the
CST.

- **Core** intercepts a call whose name is in the plugin's
  `type_constraint_names()` gate, extracts each param as a
  `ConstraintParam { string, ty }` (`'Foo'` → `{string}`; a nested
  constructor → `{ty}`, typed through the bag — see below), and wraps
  the plugin's fold result in `TypeConstraintOf`.
- **Plugin** (`frameworks/type-tiny.rhai`) declares the names and a
  `type_constraint_inner(name, params)` fold returning the inner type
  or `()`. Arity lives in the fold, not the core: a constructor takes
  0/1/N params and the fold branches on `params.len()`. New
  constructor = a name + a few lines of fold, zero core change.

This is one member of the declarative-manifest family in
`adr/plugin-system.md` — the plugin owns the vocabulary, the core owns
the mechanism.

### The accessor unwraps; the constraint value does not

`has` resolves the isa RHS expression's type through the bag and asks
the *constraint* what it constrains — `bag_query_expr_span(rhs)
.constrained_inner()`. The unwrap is the accessor's projection, not a
property of the constraint value: `my $t = InstanceOf['Foo']` keeps `$t`
typed `TypeConstraintOf` so `$t->name` resolves against the constraint,
while `has x => (isa => $t)` gives `x` the inner `Foo`. Asking the value
its question (rule #10) is why the same path covers the bare constructor,
the const-folded binding (`isa => $t`), and a `CodeRef` isa
(`isa => sub {...}` has no constrained inner → correctly untyped) with no
per-shape branching in `has`.

### Nested vocabulary recurses through the same expr typing

`Maybe[InstanceOf['Foo']]`'s single param is the call
`InstanceOf['Foo']`. The core types it *through the bag*
(`emit_expr_witness(el); bag_query_expr_span(el)`) and lands it in
`ConstraintParam.ty`. Because this reuses `expr_payload` — the path the
outer call already walks — it recurses to arbitrary depth
(`ArrayRef[InstanceOf[X]]`, `Dict[...]`) for free. The plugin asks the
value its question via a `constrained_inner(ty)` Rhai helper mirroring
`InferredType::constrained_inner`, never the serde shape.

## `Maybe[T]` is `Optional<T>`

`Maybe[InstanceOf['Foo']]` resolves to
`TypeConstraintOf(Optional(ClassName(Foo)))`. The plugin's `Maybe` fold
projects the inner's constrained type and lifts it; the accessor
projection carries the optionalness through, so `has thing => (isa =>
Maybe[InstanceOf['My::Thing']])` types the getter `Optional<My::Thing>`.

The declaration says the value may be undef, and every consumer that
matters can now hear it. `InferredType::Optional` is first class
(`optional-types.md`), flow-sensitive guard narrowing strips it at a
guard (`flow-narrowing.md`), and D2 `optional-deref`
(`narrowing-diagnostics.md`) reports an unguarded access. Dispatch is
unaffected — optional receivers resolve leniently — so the lift costs
resolution nothing and buys the diagnostic the truth.

Both `isa` spellings agree: the quoted `isa => 'Maybe[T]'` string form
and the bareword constructor fold reach the same type, which is the
property that keeps the two paths from drifting.

The boundary is the parameter, not the wrapper. A parameterized
container folds to its base rep (`ArrayRef[Int]` → `ArrayRef`) because
an element type has no `InferredType` slot to ride; that slot is
sequence-types phase 3 (`prompt-sequence-types.md`), which names this
fold as its waiting caller.

## Trade-offs

**`EXTRACT_VERSION` bump** for the `TypeConstraintOf` variant and the
nested-`ty` shape. Bumping is free; old blobs re-resolve lazily.

**Constraint names are import-scoped.** The `type_constraint_names()`
manifest declares which names are constructors; the enclosing file must
have IMPORTED the name for a call to type as one
(`Builder::constraint_name_imported`). The import is the authoritative
source — `use Types::Standard qw/Str Int/` injects exactly those names,
`-all` / `:all` / bare `use` expand to the full vocabulary through the
plugin's `on_use`, and a house library re-exporting Types::Standard
rides its kit's `SyntheticUse`.

Scoping is what lets the vocabulary include the 0-arity base constants.
`Str` / `Int` / `Num` / `HashRef` are ordinary English words and common
sub names, and the gate fires BEFORE local symbol lookup — so an
unscoped list would silently beat a package's own `sub Str`, and
hand-curating the names "unlikely to collide" is a partial enumeration
that is always incomplete (rule #10). Requiring the import costs
nothing: a Type::Tiny constant must be imported to compile.

## What's deferred

- **Method dispatch on a constraint value** (`$t->assert_valid`,
  `->check`, `->coerce`) → `Type::Tiny`. No-op until `Type::Tiny` is
  indexed from CPAN; the projection design above is chosen partly so
  this composes — `$t` keeps its `TypeConstraintOf` type for dispatch
  while the accessor projects the inner.
- **Richer vocabulary** (`ArrayRef[InstanceOf[X]]`, `Enum`, `Dict`).
  The `ty`-filling plumbing exists; each is one fold entry.
- **Retiring the name gate entirely.** A constraint constructor is a sub
  that returns a constraint value, so the honest model is a SYMBOL with
  a return type, not a name gate in `expr_payload`. Core has a gate only
  because `Types::Standard` generates its exports at runtime through
  `Type::Library`, leaving no static `sub Str` for an import to bind to
  — the runtime-export-generator boundary (`open-problems.md`). Repairing
  that invisibility by minting symbols is what the plugin layer does
  everywhere else (Moo's `has`, DBIC's `add_columns`); Type::Tiny is the
  one place it adds a gate instead.

  Minting dissolves the special case for the names it can reach: the
  name resolves as an ordinary imported sub, its type flows through
  `sub_return_type`, and a local `sub Str` competes by the normal
  local-beats-import rule rather than by a gate that runs before symbol
  lookup at all.

  **It reaches the 0-arity constants and stops.** `Str` / `Int` /
  `HashRef` are `ReturnExpr::Concrete(TypeConstraintOf(rep))` — and they
  are also the whole reason the import scoping is needed, since they are
  the names that collide. The parameterized constructors do not follow,
  because of what the syntax actually is: `Maybe[Int]` parses as a call
  whose single argument is an `anonymous_array_expression` whose element
  is a bareword, and `InstanceOf['Foo']` as one whose element is a
  string literal. A return shape for those has to say "the type of
  element 0 of my first argument" and "the literal VALUE of element 0 of
  my first argument" — two projections deep, through an arrayref.
  `ReturnExpr::Arg(n)` yields the argument's own type (`ArrayRef`), and
  no operator composes the unwrap.

  `extract_constraint_params` already does exactly that unwrap, handing
  the plugin a flat `ConstraintParam { string, ty }` per element with
  nesting resolved through the same `expr_payload` path. That extractor
  is the real machinery here and survives either design; the gate around
  it is the small part.

  So the honest choice is not "mint or gate" but where to draw the line.
  Minting the 0-arity half removes the collision class structurally and
  lets the gate shrink to `InstanceOf` / `ConsumerOf` / `Maybe` — three
  distinctive names for which a global gate carries no real risk. The
  cost is two mechanisms instead of one, which is a real cost and should
  not be waved away by observing that each covers the case it suits.
  Deciding that is the open question; inventing an arrayref-projecting
  `ReturnExpr` shape to serve one library's syntax is the alternative,
  and it is a cross-language type-system change for a Perl-shaped
  problem.
