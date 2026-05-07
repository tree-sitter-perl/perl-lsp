# Per-flavor parametric semantics

**Status:** discussion. Open for iteration.

## The problem

`InferredType::Parametric { base, type_args }` shipped with two
hard-coded accessors:
- `class_name() -> &base` (method dispatch)
- `hash_key_class() -> type_args[0].as_class()` (hash-key arg owner)

That works for DBIC `Parametric{ResultSet, [Users]}`. It does NOT
generalize. Different parametric flavors carry different semantics
for "what does the hash-key arg belong to":

| Flavor | hash_key_class wants… |
| --- | --- |
| `Parametric{ResultSet, [Users]}` | type_args[0] (row class) |
| `Parametric{Mojo::Pg::Results, [Users]}` | type_args[0] |
| `Parametric{Promise, [X]}` | **none** — Promise's hash-key args (rare) belong to Promise, not the wrapped value |
| `Parametric{Mojo::Collection, [User]}` | **none** — `c(...)->grep({...})` takes a hashref of *predicates*, not row-keyed columns |
| `Parametric{HashRef, [String, ArrayRef[Str]]}` | open question — depends on whether HashRef carries struct-key semantics or generic key-value |
| `Parametric{ArrayRef, [User]}` | none for the array itself; element narrowing is `->[N]`, not `->{K}` |

A single `hash_key_class()` rule baked into `InferredType` is
DBIC-shaped. The minute we add Promise, Collection, GraphQL types,
the rule fragments — and we end up either (a) hardcoding per-base
`if/match` chains in the accessor, or (b) silently misresolving on
non-DBIC parametrics.

## Design surface — three options

### Option A: per-base lookup table

A static map `BASE_NAME -> SemanticsKind`:
```rust
const PARAMETRIC_SEMANTICS: &[(&str, ParametricSemantics)] = &[
    ("DBIx::Class::ResultSet", ParametricSemantics::RowKeyed),
    ("Mojo::Pg::Results",      ParametricSemantics::RowKeyed),
    ("Mojo::Promise",          ParametricSemantics::Wrapped),
    ("Mojo::Collection",       ParametricSemantics::ElementWrapped),
    // …
];
```
`hash_key_class()` consults the table.

**Pros:** zero new traits, fast, dead simple to add a new flavor.
**Cons:** per-base hardcoded. New parametric flavors require a
core-crate edit. Not pluggable from Rhai. The "DBIC support is a
plugin" direction (`prompt-dbic-as-plugin.md`) wants plugins to own
their parametric flavors' semantics.

### Option B: semantics on the type itself

Add a `semantics: ParametricSemantics` field to the
`Parametric` variant:
```rust
Parametric {
    base: String,
    type_args: Vec<TypeArg>,
    semantics: ParametricSemantics,
}

enum ParametricSemantics {
    RowKeyed,        // type_args[0] is the row class for hash-key args
    Wrapped,         // wraps a value (Promise<X>, Try<X>) — no hash semantics
    ElementWrapped,  // collection of T (Mojo::Collection<X>, ArrayRef<T>)
    Custom(u32),     // plugin-defined; index into a registry
}
```

**Pros:** semantics travel with the value through the bag. Plugins
can synthesize Parametric witnesses with a `Custom(idx)` tag and
register a callback for what `hash_key_class` (and friends) returns.
**Cons:** every emit site has to pick a semantics. The `Custom`
escape hatch is necessary but adds a registry indirection. More
weight on the InferredType variant.

### Option C: traits on a wrapper, opt-in per consumer

Don't bake any semantics into `Parametric` itself. Consumers ask
typed questions via traits:
```rust
trait HashKeyArgClass {
    fn hash_key_arg_class(&self) -> Option<&str>;
}
trait ElementType {
    fn element_type(&self) -> Option<&InferredType>;
}
trait WrappedValue {
    fn wrapped_value(&self) -> Option<&InferredType>;
}
```
Implementations dispatch on `base` (hardcoded match in core, plugin
table for plugin-emitted flavors).

**Pros:** consumers ask for exactly the dimension they want; the
match site is the type-level rule. Composes with the dual-class
problem (#2 — see `prompt-type-system-encoding.md`).
**Cons:** more API surface. Requires consumers to know which trait
fits their question, but that's also the point — it's *forced* by
the type system, no accidental wrong-axis read.

## Pluggability requirements (from #6)

DBIC support is moving to a plugin. The plugin owns:
- Witness emission for `recv->resultset('Foo')` → Parametric.
- Discovery of `<Schema>::ResultSet::<arg>` for the resolved
  resultset_class (currently hard-coded to `DBIx::Class::ResultSet`).
- Per-source row-class resolution.

If parametric semantics live in the core (option A or B-without-
Custom), the plugin can't introduce a new flavor without core
changes. That's a regression vs the "everything is a plugin"
direction.

Recommended: **option C**, with a registry table for plugin-
contributed semantics. Same shape as `WitnessReducer` — a trait
the plugin implements, registered at plugin load.

## Cross-cut with the cleanup pass (#3)

Each new "I want X dimension of this type" trait is one less
`Option<String>`-returning helper. The cleanup pass should not just
add typed siblings — it should add the *trait-shape question* the
caller is really asking. Example: `method_call_invocant_class` is
"give me the dispatch class"; replace with `method_call_invocant_type`
+ caller-side `ty.dispatch_class()` (which is `class_name()`
today; `class_name` is the trait method on the dispatch axis).

## Open questions for iteration

1. Are there parametric flavors where TWO type_args matter
   simultaneously? (`Either<L, R>`, `HashMap<K, V>`.) In that case
   `type_args[0].as_class()` is wrong even when row-keyed; need
   per-arg semantics, not per-flavor.
2. How does this interact with the future Effect facts (`Promise`
   carries IO effect)? Is Effect orthogonal to parametric (effect
   on the wrapper, type on the args), or does it go on a TypeArg
   slot?
3. What's the cost of porting the four DBIC-related accessors I
   added (`extract_resultset_parametric`, `hash_key_class`,
   `method_call_invocant_type`, `emit_call_arg_key_accesses_open`)
   over to the trait shape? Some of them stay (the emitter); others
   become caller-side trait calls.
