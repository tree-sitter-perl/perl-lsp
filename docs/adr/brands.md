# ADR: Brands — plugin-owned marks that ride a value

A plugin sometimes knows a fact about a VALUE that has to follow it
wherever the value goes. Mojolicious routes are the motivating case:
children inherit their parent's `->to(...)` defaults, and most `->to`
calls in a real app (crm) are partial:

```perl
my $alerts_r = $r->any('/alerts')->to('alerts#');  # controller default = 'alerts'
$alerts_r->get('/')->to('#list');                  # action='list', controller INHERITED
my $crud = $alerts_r->under('/:type')->to('#get_alert');
$crud->get('/settings')->to('#read_settings');     # controller still 'alerts'
```

The missing half of a partial target is set by an ancestor route and
inherited down a graph of route values linked by method chains,
assignments and `under` nesting. Core knows nothing about routes; the
mechanism is general, and routes are its first consumer.

## Decision: marks ride the type

```rust
InferredType::Branded { base: Box<InferredType>, marks: Vec<Mark> }
pub struct Mark { ns: String, key: String, value: String }
```

- `base` is what the value IS. `class_name()` and every display / dispatch
  projection read through it, so a brand never changes where a method
  dispatches.
- `marks` are opaque to core. `ns` is the owning plugin's id, so two
  plugins never collide. `InferredType::branded` is the one constructor:
  marks stay sorted, a branded base merges, and no marks is the plain base.
- Because the type already rides every edge in the bag (assignment,
  `Invoke` receivers, implicit returns), the marks ride them too. There is
  no side table and no brand id to keep cache-stable: the marks ARE the
  value, content-addressed.

## Producing marks

A plugin emits `EmitAction::Brand { at, on_class, set, drop }` for the
method call spanning `at`. The builder records it as a `BrandOverlay`
witness on that call's `Expression`, beside its `Invoke`. The registry
applies overlays after reduction (`apply_brand_overlays`): whatever the
attachment answers, with `set` laid over the plugin's marks and `drop`
removed. An overlay applies only when the value is an `on_class` instance,
because the plugin matched the call by its method name alone.

The overlay is a producer fact: the plugin parsed the call's literals at
walk time and needs no inherited state, so it runs before the fold.

## Carrying marks

A method passes its receiver's marks on by returning its receiver. Plugin
overrides can declare a `ReturnExpr` for that (`OverrideReturn::Expr`):
mojo-routes declares every Route verb `ReceiverOr(ClassName(Route))`, so
`$branded->get(...)` comes out carrying the same marks, and the
receiver-less lite `under(...)` still types as a Route. A method that
returns something else drops the marks, which is right: it is a different
value.

The router (`Mojolicious::Routes`) is the unbranded root, so its verbs keep
returning a plain Route.

## Reading marks

Fold-phase patterns read a capture's marks in their own namespace through
the `brand` projection (`[[key, value], …]`). mojo-routes' partial
`->to('#act')` reads `controller` from its receiver's brand, falling back
to the `topic_base` projection (the lite topic stack's replayed base).

## Joins and the fold

- Subsumption: a branded value subsumes a narrowing whose marks it carries,
  so a re-derivation with fewer marks never clobbers an accumulated brand.
- `FrameworkAwareTypeFold` lets a branded instance dominate a bare
  `ClassName` of the same class.
- The fold's snapshot compares unbranded values: marks are a value's, not a
  sub's return contract.
- Joins compare whole types, so two arms whose marks disagree do not agree
  at all (`BranchArmFold` answers nothing), exactly as two different classes
  don't. Keeping the marks both arms share is the refinement if a consumer
  needs it.

## Limits

- **Values in types.** Marks are literal strings riding a type. When the
  value lane lands they should become value edges; the carrier stays.
- **Cost.** The receiver key in both memos is the full type, so each
  distinct brand on a receiver is a distinct callee question. Route apps
  have a handful of controllers; a plugin branding with per-site values
  would multiply callee evaluations.
- **The param boundary stays dark.** crm roots its routes from
  `my $r = $conf->{root}`, an untyped hash element of a parameter, so the
  chain never starts there. That is the general untyped-param boundary
  (`docs/open-problems.md`), not a brand problem.
- **One global namespace.** A resolved partial route is keyed per declaring
  package, not per app instance, like the helper app surface. Per-object
  scoping is the parked instance-brand work (`docs/prompt-graph-walking.md`),
  which is birth-site provenance, not marks.
