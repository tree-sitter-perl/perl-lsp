# PHP member access: what the rule cannot see yet

`readonly-write` is one rule, `FileAnalysis::misplaced_member_writes`
(`model/file_analysis/class_queries.rs`): a write to a member is judged
against the member's set scope (`SymbolFlags::set_scope`), and a write-once
member against whether a constructor encloses the write. The writer's
class is `package_at(site)`, the receiver's classes are
`receiver_classes(.., ReceiverPolicy::Stated)`, and a site is reported only
when every receiver arm resolves the member to a declaration that finds the
write misplaced. Everything below is a gap in what reaches that rule, not in
the rule. Each has a `TODO(<tag>)` at the rule site.

## Member writes the model doesn't see (`TODO(unseen-member-writes)`)

A member token is a write when it is the member read an `@flow.place`
projects (`extract/flow.rs`): the left side of `=` / `=&` / a compound
assignment, an `++`/`--` operand, an `unset()` argument; `??=` marks it
`WriteIfUnset` (`@flow.init`). Only a place whose own node is the member
access projects one. Three shapes write a member without being that:

- **Element writes**: `$o->x[] = $v`, `$o->x['k'] = $v`, `$o->x['k']++`,
  `unset($o->x['k'])`. The place is the `subscript_expression`, which projects
  nothing, so the member token stays a read. PHP fetches the member for
  writing: on a `readonly` member that throws from any scope, constructor
  included (a readonly array is built whole and assigned once); on a
  `private(set)` / `protected(set)` member it throws outside the set scope.
  When the member holds an object, the element write is `offsetSet` on an
  object fetched for reading: a read of the member, not a write. The fact: a
  subscript place projects an *element write* on its base member's ref, and
  the rule treats it as a write only where the member's declared type is an
  array (an untyped or object-typed member is no finding).
- **By reference**: `$r = &$o->x;`, `foreach ($o->x as &$v)`, and an argument
  in a by-reference parameter position (`sort($o->x)`,
  `preg_match($re, $s, $o->m)`, a user function's `&$out`). Taking a
  reference is a write for both checks: it throws on a `readonly` member
  from any scope and on a set-scoped member outside the set scope. Today the
  right side of `=&` is an `@flow.source` (a read), the iterated expression
  of a `foreach` is a read, and nothing marks an argument as a by-reference
  binding. The facts: the `=&` source and a `foreach (… as &$v)` subject as
  reference-taking sites on the member's ref; and, for calls, the callee's
  by-reference positions. The model already has the binding fact,
  `WitnessAttachment::Param { package, name, index }` with the `ParamOf`
  projection for dispatched calls, but the PHP extraction mints none of it,
  a call site edges only a bare-variable argument, and builtins
  (`sort`, `array_push`, `preg_match`) need a by-reference signature table
  PHP has no home for yet.
- **Destructuring and loop targets**: `[$o->a, $o->b] = $pair;`,
  `['k' => $o->a] = $row;`, `list($o->a) = …`, `foreach ($xs as $o->x)`,
  `foreach ($xs as $k => $o->x)`. These assign the member exactly as `=`
  does, so the same verdicts apply. The query names only `variable_name`
  targets in these positions (`@flow.rebind`), and the `@flow.place` on the
  whole `list_literal` projects nothing. The fact: each destructuring or loop
  target is a place of its own, captured `@flow.place` in the query document,
  nested lists and keyed pairs included.

## Get visibility as its own diagnostic (`TODO(get-visibility-diagnostic)`)

A `private` or `protected` member read, written, or called from outside the
scope its visibility allows is not checked. PHP throws there (`Cannot access
private property`, `Call to private method`), unless the class answers the
name through `__get` / `__set` / `__call`, which take over an inaccessible
member.

This is a separate diagnostic code from `readonly-write`: it judges where a
member may be *named*, not where it may be set, it fires on reads and calls
as well as writes, and an editor wants to turn the two off independently. A
write outside both scopes should report once, under `readonly-write`.

What it reuses from `misplaced_member_writes`, unchanged: the writer's class
(`package_at`, with the trait skip), the receiver arms
(`receiver_classes(.., ReceiverPolicy::Stated)`) and the every-arm rule, the
member lookup (`resolve_field_in_ancestors`, `resolve_method_in_ancestors`
for calls) and its widened-guess skip, and the family test (`class_isa`).
The axis is the read visibility the member's flags state:
`SymbolFlags::NON_PUBLIC` (two-state) on this layer, and
`SymbolFlags::access_scope()` over `PRIVATE` / `PROTECTED` once it lands,
which returns the same `Class` / `Family` / `Anywhere` enum `set_scope`
returns. The verdict is the same match with `access_scope` in place of
`set_scope`. New: the `DYNAMIC_MEMBERS` skip (a class with `__get` /
`__call` answers any name), and method calls and class constants
(`Foo::LIMIT`) alongside value access.

PHP's protected check (`zend_check_protected`) passes in either direction
along the parent chain: a parent's code may reach a protected member its
child declares. `class_isa(writer, declaring)` tests one direction only; the
family test both diagnostics share should test both.

## `Closure::bind` / `bindTo` / `Closure::call` scope (`TODO(closure-bind-scope)`)

`Closure::bind($fn, $newThis, Foo::class)`, `$fn->bindTo($obj, Foo::class)`
and `$fn->call($obj)` rebind a closure's `$this` and class scope at runtime.
Its body then runs with `Foo`'s access: it may initialise `Foo`'s readonly
members and read and write its private ones. That is the hydrator pattern
(`Closure::bind(function (Foo $o, array $v) { $o->id = $v['id']; }, null,
Foo::class)`).

Today a closure is judged by the class it is written in: the writer is
`package_at(site)`, and `$this` inside the closure is `@receiver.enclosing`,
the lexical class. So a closure written outside `Foo` and bound to `Foo`
reports a write through a typed receiver as `OutsideClass`, and one written in
`Foo` and bound elsewhere is judged as `Foo`, its `$this` typed `Foo`. (A
closure written outside every class whose receiver is an untyped `$this` has
no receiver arms, so it reports nothing.)

The fix shape: the bound scope is a fact on the closure value, and both the
writer class and `$this` read it ahead of the lexical class. The inline form,
a closure literal as `bind`'s first argument or `bindTo`'s / `call`'s
receiver, is syntactic: the query can capture the literal and the scope
argument (`Foo::class`, `static::class`, an object whose class is its type)
and mint the closure's scope at extraction. A closure that reaches the bind
through a variable or a parameter needs value flow: the bind becomes a
witness on the closure value, and the scope is what that value's chase
answers at the closure's body.
