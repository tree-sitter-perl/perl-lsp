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

## What this replaced

A `MemberShape { Unknown, Callable, Value }` tag on `MethodCall`, a
`member_shapes_are_strict` pack flag, a `member_kinds_overloaded` gate
consulted before the tag was honoured, a `FIELD_EDGE_SOURCE` witness tag
the registry partitioned `PackageSymbol` edges on (keyed on "no arity
hint"), and the Value/Callable rungs inside `member_value_type`. All five
were proxies for the one fact the syntax had already handed over.
