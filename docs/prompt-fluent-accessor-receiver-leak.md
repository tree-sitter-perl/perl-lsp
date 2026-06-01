# Fluent Mojo::Base accessor in a chain returns the wrong class (receiver leak)

> A Mojo::Base `has` accessor called as a fluent setter inside a method
> chain (`Obj->new->acc($v)`) does **not** resolve to `Obj`. It resolves to
> whatever class the *outer* query carried as its receiver — the call-site
> invocant leaks down into the inner accessor's receiver-relative return
> type. Plain subs that `return $self` are unaffected; this is specific to
> the `ReturnExpr(Receiver)` substitution path.
>
> Split out of the variable-chase unification work
> (`feat(types): unify Variable into the canonical chase`), which fixed the
> chain/cross-file plumbing this sits on top of. See
> `docs/adr/return-expr.md` for the `ReturnExpr` model and
> `docs/adr/bag-canonical.md` for the query path.

## Symptom

crm: the `minion` helper is `$app->helper(minion => sub { $minion })`, with

```perl
my $minion = Clove::Minion->new(Pg => $mngr->pg_string)->app($app);
```

`Clove::Minion` is `Mojo::Base 'Minion'`; `Minion` declares
`has app => sub {...}, weak => 1`. So `->app($app)` is a fluent setter and
should return `Clove::Minion`. Instead `$c->minion`'s return type comes back
empty/wrong, because `$minion` never types (its chain dies at `->app`).

## Minimal reproduction

```perl
# lib/My/Base.pm
package My::Base;
use Mojo::Base -base;
has acc => sub { {} };               # fluent accessor
sub plainret ($self) { return $self; }   # control: plain sub

# lib/My/Plugin.pm
package My::Plugin;
use Mojo::Base 'Mojolicious::Plugin';
sub register ($self, $app, $conf) {
  $app->helper(viaacc   => sub ($c) { return My::Base->new->acc($x); });
  $app->helper(viaplain => sub ($c) { return My::Base->new->plainret; });
}

# lib/My/Ctrl.pm
package My::Ctrl;
use Mojo::Base 'Mojolicious::Controller';
sub a ($c) { my $x = $c->viaacc; my $y = $c->viaplain; ... }
```

Hover (`perl-lsp --hover <root> Ctrl.pm <line> <col>`):

| call | expected | actual |
|---|---|---|
| `$c->viaplain` (plain `return $self`) | `My::Base` | **`My::Base`** ✓ |
| `$c->viaacc` (`->acc($x)`, Mojo::Base accessor) | `My::Base` | **`Mojolicious::Controller`** ✗ |

`Mojolicious::Controller` is the type of `$c` *in the consumer* (`sub a ($c)`).
That is the tell: the consumer's call-site receiver for `$c->viaacc` is
propagating all the way down into `My::Base->new->acc`'s receiver-relative
return type and being substituted there.

`viaplain` works because `sub plainret { return $self }` types `$self` as a
concrete `ClassName(My::Base)` directly — no `Receiver` placeholder, nothing
to substitute, nothing to leak into.

## Root cause

Mojo::Base accessors are synthesized with a fluent return modeled as
`WitnessPayload::ReturnExpr(ReturnExpr::Receiver)` (a free variable, not a
concrete type). `ReturnExprReducer` substitutes `q.receiver` at query time.

The leak is in the **edge chase's receiver propagation**. In
`witnesses.rs::materialize`, the non-Variable edge branch builds the
sub-query with `receiver: q.receiver.clone()` — it forwards the *outer*
query's receiver unchanged into every nested attachment, including an inner
method call's `Expression(refidx) → Edge(MethodOnClass{class, name})`.

So when the chase descends:

```
Symbol(viaacc helper)            receiver = ClassName(Mojolicious::Controller)   ← from $c->viaacc
  → … → Expr(My::Base->new->acc($x))
    → Expression(acc-refidx)     receiver still Mojolicious::Controller
      → MethodOnClass{My::Base, acc}   receiver STILL Mojolicious::Controller
        → ReturnExpr(Receiver) → substitute q.receiver → Mojolicious::Controller   ✗
```

The receiver for a method call's return must be **that call's invocant**
(`My::Base`, the `class` in `MethodOnClass{class, name}`), not whatever the
enclosing query happened to carry. `query_sub_return_type` already does the
right thing for its direct `MethodOnClass` lookup —
`effective_receiver = receiver.or(Some(ClassName(class)))` — but the generic
edge chase in `materialize` doesn't, so a `MethodOnClass` reached *through an
edge* inherits the stale receiver.

## Fix direction

When the chase crosses into an attachment that re-establishes a receiver,
reset it instead of forwarding `q.receiver`:

- `Edge(MethodOnClass{class, name})`: the sub-query's receiver should be
  `ClassName(class)` (the invocant), overriding the inherited one. This is
  the surgical fix and mirrors `query_sub_return_type`'s `effective_receiver`.
- `Edge(Expression(refidx))`: a method call's resolved type — same idea; the
  receiver is the call's invocant, which `Expression` resolves via its own
  `MethodOnClass` edge, so resetting at the `MethodOnClass` hop is enough.

Keep forwarding `q.receiver` for `Expr`/`Variable`/`Symbol` edges (those are
receiver-transparent rvalues; the receiver only has meaning at a concrete
method dispatch). The cleanest encoding is on the *target*: the materialize
branch matches `target`, and for `MethodOnClass { class, .. }` sets
`receiver: Some(InferredType::ClassName(class.clone()))` rather than
`q.receiver.clone()`.

Watch for: a `MethodOnClass` query that legitimately wants the caller's
receiver (e.g. an inherited fluent accessor where the invocant is the *child*
class, not where `has` was declared). Resetting to `ClassName(class)` where
`class` is the queried (child) class is correct for that too — `acc` queried
as `MethodOnClass{My::Sub, acc}` should substitute `My::Sub`, and the
inheritance walk to the parent's `ReturnExpr` keeps the child receiver
because the edge `MethodOnClass{child} → Edge(MethodOnClass{parent})` should
*also* reset to the child... confirm the MRO edges carry the originating
child class as receiver, not the parent. (This is the one subtlety to test.)

## Acceptance

Add a unit pin alongside `cross_file_lexical_chain_return_type` in
`symbols_tests.rs`:

```rust
// helper returns `My::Base->new->acc($x)` where `has acc` is a Mojo::Base
// fluent accessor on My::Base (registered cross-file). Must resolve to
// My::Base, not the consumer's call-site receiver.
fn cross_file_fluent_accessor_chain_return_type() { … assert hover contains "My::Base" … }
```

End-to-end check: with a `Minion`-shaped fixture (`has app => sub {...}, weak
=> 1` on a base, child via `Mojo::Base 'Base'`), `Obj->new->app($x)` resolves
to the child. Then crm `$c->minion` hover should show `Clove::Minion`.

## Not the cause (already ruled out)

- `has X => sub {...}, weak => 1` parses fine — the accessor symbol is
  synthesized (getter + setter overloads both present in `--dump-package`).
- The chain / cross-file / lexical plumbing is fixed (the `viaplain` control
  resolves cross-file). This is purely the receiver substituted into the
  fluent accessor's `ReturnExpr(Receiver)`.
