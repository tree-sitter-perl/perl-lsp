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

### Core extracts the params; the plugin folds them to the constraint

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
matters can hear it. `InferredType::Optional` is first class
(`optional-types.md`), flow-sensitive guard narrowing strips it at a
guard (`flow-narrowing.md`), and D2 `optional-deref`
(`narrowing-diagnostics.md`) reports an unguarded access. Dispatch is
unaffected — optional receivers resolve leniently — so the lift costs
resolution nothing and buys the diagnostic the truth.

Both spellings reach the same type through the SAME vocabulary, fold and
parameter walk. Moose's type string is Perl-parsable, so it is re-parsed
and walked exactly like a constructor written in the file; the two differ
only in how a leaf name resolves — a node in the file asks the bag, a
re-parsed leaf asks the plugin vocabulary directly, because a re-parsed
tree's spans live in the string's own coordinate space and emitting
witnesses from them would collide with the file's attachments. That is
the honest limit on "one implementation": one walk, one fold, two
resolvers. Agreement is structural, not two tables kept in step.

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
  that returns a constraint value; the honest model is a minted symbol,
  not a name gate ahead of symbol lookup. The design is
  `prompt-type-constraint-flow.md`: the plugin's `on_use` mints the sub
  the runtime installs, and `Name[...]` parameterization is asked of the
  callee's owner. No `ReturnExpr` shape carries it — `Arg(n)` is inert for
  Perl (no call site threads argument types into a query), and
  `InstanceOf['Foo']`'s operand is a literal *value*, which no type-level
  operator reads.
