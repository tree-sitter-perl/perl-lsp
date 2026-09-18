# ADR: Member kinds — a value read and a call are different things

A member access names either a stored value (`$this->prop`, `obj->field`)
or a callable (`$this->m()`, `[$obj, 'm']`). Where the syntax tells them
apart, the extractor says so at mint time and nothing downstream
re-derives it (CLAUDE.md rule #11). The two kinds are distinct all the
way down: a distinct ref kind, a distinct witness attachment, a distinct
ancestor walk. There is no shape tag on a shared ref, no source tag the
registry partitions on, and no per-pack strictness flag.

## The three seams

- **Ref kind.** `RefKind::FieldAccess { invocant, invocant_span,
  member_name_span, member_op }` is the value access; `RefKind::MethodCall`
  the call. `Ref::member_site()` is the one receiver view both project, so
  the invocant ladder (`method_call_invocant_type` / `_class`), op-DX and
  the rename token gate serve both without a kind branch. The frozen
  dispatch edge (`RefBinding::Method` / `MethodTarget`) rides both — for a
  `FieldAccess` it lands on the field symbol.
- **Attachment.** A field's value edge is `Field{owner, name} →
  Edge(Variable{decl})`, pushed per field declaration by the pack
  extractor. `Field{owner, name}` is the storage slot's project-wide
  subject that the domain fold already keyed (`field-projections.md`);
  the value rides the same subject. `ProjectionStep::ValueHop` chases
  `Field{class, member}`; `MethodHop` chases `PackageSymbol{class,
  member}`. The registry's `Field` fallback walks the owner's candidate
  files and parents exactly as the `PackageSymbol` ladder does (no bridge
  hop — a plugin entity is a callable). `FieldValueReducer` answers the
  materialized value and is registered ahead of `DomainCoherenceFold`, so
  the defeasible domain never becomes the type that flows.
- **Walk.** `resolve_field_in_ancestors` admits value kinds only
  (`Field`, class-content `Variable`, `Enumerator`);
  `resolve_method_in_ancestors` admits any kind. Both are the one MRO walk
  parameterized by the kind family the asking ref implies.
  `field_value_type(receiver, member)` is the receiver-typed entry for a
  `FieldAccess`; `member_value_type(receiver, member, arity)` is the
  kind-less ladder (method return first, field fallback) for an asker
  with no ref in hand — the sentinel mid-keystroke, member hover before a
  ref exists.

## Why the call walk still admits a data member

A language whose member read IS a call has no value-read syntax: Perl's
`$o->m` is a call with or without parens, and a `has` accessor is a
`Method` symbol. C's `obj->field` likewise mints a `MethodCall` until the
pack's member capture discriminates on the argument list. So the call
walk keeps the value-kind fallback, and strictness is by construction on
the value side: a `FieldAccess` exists only where the syntax minted one,
and it never resolves to a method. `$php->sucks` without parentheses on a
class that only declares `sucks()` is an undeclared property, not a
resolved call.

A language where a method is also readable as a value (JS `obj.method`)
publishes the callable on BOTH attachments at mint; the model never learns
which language did that.

The fallback is a fallback, not a union: a Callable target collects a
stored member of its name only where the owner declares no callable of
that name (`index/resolve/collect.rs`). Where it does, that callable IS
the target and a same-named slot is a different member of the same class —
so a php class with both `handler()` and `$handler` keeps two identities,
and `$this->handler()`'s references never splat onto the property. The
narrowing is owner-scoped through `symbol_defines_target`, so it asks the
same question the collect walk already asks; Perl is unaffected by
construction, since its class content admits only `Variable | Field |
Enumerator` and a `has` accessor's `HashKeyDef` never reached the callable
arm.

The MRO walk narrows on the same rule: `resolve_member_in_ancestors` takes
the family the asking ref NAMES, and per class a declaration of that family
answers while one the family merely admits is held as the fallback the walk
returns only when nothing else does. Without it the answer was declaration
order — a property declared above its same-named method swallowed every
call to it.

### Perl accesses that are semantically value reads

A Perl `$o->name` with no arguments is often a value read in intent (a
`has` accessor, a hand-written getter), and the question arises whether
the builder should mint a `FieldAccess` for it. It does not, and the
reason is where the fact lives: the site carries no evidence — `$o->name`
and `$o->name()` are one syntax, and a 0-arity callee is a property of
the TARGET, not of the site (a 0-arity sub may compute, side-effect, or
dispatch). Deciding the kind at the call site from the callee's arity
would be a shape branch on the target (rule #10), and it would be minted
in the consumer from a fact the producer already has. What the site wants
is the VALUE, and that already flows: the call walk's value-kind fallback
and `member_value_type` answer a method's return first and a field's value
second, so an accessor read types without a second ref kind.

If Perl ever mints a value-read fact, it is minted at the declaration by
the producer that knows the sub is storage-shaped — a `has` accessor is
already a `Method` symbol paired with its `HashKeyDef`s, and an `:lvalue`
sub (assignable like a field: `$o->name = 'x'`) is the one Perl
declaration whose members ARE value slots. That is the open note: an
`:lvalue` sub is a `Sub` symbol today, and its write sites are plain
`MethodCall` refs with no write access classification. Minting it as a
member with a value edge (`Field{owner, name}` alongside the `Symbol`) is
the producer-side fact that would make `$o->name = ...` a write in
`documentHighlight` and references, and it needs nothing from this ADR's
seams beyond the extractor's attribute walk (`SymbolFlags`).

## What this replaced

A `MemberShape { Unknown, Callable, Value }` tag on `MethodCall`, a
`member_shapes_are_strict` pack flag, a `member_kinds_overloaded` gate
consulted before the tag was honoured, a `FIELD_EDGE_SOURCE` witness tag
the registry partitioned `PackageSymbol` edges on (keyed on "no arity
hint"), and the Value/Callable rungs inside `member_value_type`. All five
were proxies for the one fact the syntax had already handed over.
