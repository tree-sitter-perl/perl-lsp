# ADR: Member kinds — a value read and a call are different things

A member access names either a stored value (`$this->prop`, `obj->field`)
or a callable (`$this->m()`, `[$obj, 'm']`). Where the syntax tells them
apart, the extractor says so at mint time and nothing downstream
re-derives it (CLAUDE.md rule #11). The two kinds are distinct all the
way down: a distinct ref kind, a distinct witness attachment, a distinct
ancestor walk. There is no shape tag on a shared ref and no source tag the
registry partitions on. What a language does declare is how its classes
hold members, which decides what an ask may be answered by (below).

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
- **Walk.** `resolve_field_in_ancestors` asks for a value,
  `resolve_method_in_ancestors` for a callable. Both are the one MRO walk
  parameterized by the kind family the asking ref implies, and the
  language's `MemberNamespace` decides which declarations answer
  (`MemberKind::admits_decl`).
  `field_value_type(receiver, member)` is the receiver-typed entry for a
  `FieldAccess`; `member_value_type(receiver, member, arity)` is the
  kind-less ladder (method return first, field fallback) for an asker
  with no ref in hand — the sentinel mid-keystroke, member hover before a
  ref exists.

## A language declares its member namespace

Whether a call can reach a stored member is not a property of the call. It
is a property of the language, so it is declared once, on the language's
spellings (`PackSpellings::member_namespace`, reached by id, rule #14), and
the walk reads the value without asking which language it serves:

- **`PerFamily`**: a read reaches only a stored value and a call only a
  callable. Perl is here: a call reaches a sub, never a `field` of the
  same name. A language with property and method namespaces is here too.
- **`Shared`**: a name is one member whatever its kind. C++ is here, and
  Python. A member access reads the member and a call applies to whatever
  it holds, so `p.x`, `p.len()` and `d.hook()` all reach the nearest
  declaration of their name. The ref's family does not take part.

The neutral default is `PerFamily`, so a language that declares nothing
gets no cross-family answers.

The walk answers the nearest declaration the ask admits and stops. There
is no held fallback that prefers a farther declaration of the asked family
over a nearer one the namespace admits: C++ finds `D`'s function-pointer
`read` for `d->read(buf)` and never `B::read()`, so a walk that reached
`B::read()` answered a member the language would not. A declaration needs
no flag saying its value is invoked. Nothing resolves ACROSS families
where the language keeps them apart: a same-named declaration of the
other family is a diagnostic's suggestion (an undefined method with a
property of that name to point at), never a resolution.

A language where a method is also readable as a value (JS `obj.method`)
publishes the callable on BOTH attachments at mint; the model never learns
which language did that.

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
is the VALUE, and that already flows: a `has` accessor is a `Method`, so
the call reaches it and its return types the read, without a second ref
kind.

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
