# ADR: Member kinds — a value read and a call are different things

A member access names either a stored value (`$this->prop`, `obj->field`)
or a callable (`$this->m()`, `[$obj, 'm']`). Where the syntax tells them
apart, the extractor says so at mint time and nothing downstream
re-derives it (CLAUDE.md rule #11). The two kinds are distinct all the
way down: a distinct ref kind, a distinct witness attachment, a distinct
ancestor walk. There is no shape tag on a shared ref, no source tag the
registry partitions on, and no per-pack strictness flag.

## The seams

- **Ref kind.** `RefKind::FieldAccess { invocant, invocant_span,
  member_name_span, member_op }` is the value access; `RefKind::MethodCall`
  the call. `Ref::member_site()` is the one receiver view both project, so
  the invocant ladder (`method_call_invocant_type` / `_class`), op-DX and
  the rename token gate serve both without a kind branch. The frozen
  dispatch edge (`RefBinding::Method` / `MethodTarget`) rides both — for a
  `FieldAccess` it lands on the field symbol. The pack document states
  the kind: `@ref.member.value` is a read, `@ref.member.call` a call
  whose pattern also captures its argument list (`@arity.args`, joined by
  match). The value pattern excludes the callee with
  `#not-has-parent? call_expression` (`capture-vocabulary.md`), so the
  two never fire at one token.
- **Attachment.** A field's value edge is `Field{owner, name} →
  Edge(Variable{decl})`, pushed per field declaration by the pack
  extractor. `Field{owner, name}` is the storage slot's project-wide
  subject that the domain fold already keyed (`field-projections.md`);
  the value rides the same subject. A read (`@expr.member`) is
  `Expr(read) → Projected{Expr(receiver), ValueHop{member}}`, which chases
  `Field{class, member}`; a call is an `Invoke` that dispatches
  `PackageSymbol{class, member}` (`calls-are-invokes.md`). A field whose
  declared type mentions its class's template parameters pushes the
  deferred `ReturnExpr` instead of the edge, so the hop's receiver
  substitutes them (`item_: T` read on a `Box<int>` is `int`). The
  registry's `Field` fallback walks the owner's candidate
  files and parents exactly as the `PackageSymbol` ladder does (no bridge
  hop — a plugin entity is a callable). `FieldValueReducer` answers the
  materialized value and is registered ahead of `DomainCoherenceFold`, so
  the defeasible domain never becomes the type that flows.
- **Walk.** `resolve_method_in_ancestors` asks for a `Callable`,
  `resolve_field_in_ancestors` for a `Value`; both are the one MRO walk,
  and which declarations answer is the language's `MemberNamespace`
  (`MemberKind::admits_decl`), declared on its `NameSpellings`.
  `field_value_type(receiver, member)` is the receiver-typed entry for a
  `FieldAccess`; `member_value_type(receiver, member, arity)` is the
  kind-less ladder (method return first, field fallback) for an asker
  with no ref in hand — the sentinel mid-keystroke, member hover before a
  ref exists.

- **Target.** `TargetRef.member_kind` carries the family the cursor
  stated: a `FieldAccess` cursor or a stored-member declaration is
  `Value`, a `MethodCall` cursor or a sub declaration `Callable`. The
  override family and rename chain walk that family, and the collector
  admits a declaration (`admits_decl`) and a use (`admits_ref`) under the
  SCANNED file's member namespace. Outgoing calls skip value reads.

## A member is declared at its class body

A data member's symbol and its value witness belong to the class body
scope, wherever the declaration is written. Most languages write it there;
php's constructor promotion (`__construct(private Clock $made)`) writes it
in a callable's parameter list, which sits inside the callable's own scope.
So a `@def.field` is filed at the innermost scope that is not a callable's
body, and the document names the member as the target of its declared type
with `@flow.target.member`, which lands the `Variable` at that same scope.
The `Field{owner, name}` edge then reaches it class-wide, the outline lists
it under the class rather than under the constructor, and no consumer
reasons about where the token sat.

A member's name is the name its accesses use. A php property is declared
`$x` and read `$obj->x`, so the symbol is `x`, its selection span is the
bare name token (what rename rewrites), and the written `$x` rides
`@def.field.spelled` onto `Presentation::label`, the spelling the outline
lists it by. No pack declares a sigil for the model to put back.

## A language decides how its classes hold members

Whether a call can reach a stored member is a fact about the language,
not about the call, so it is declared once per language and the walk
reads it; nothing branches on a language.

- **`PerFamily`** (Perl, and the neutral default): each ask is answered
  by its own family. A Perl call reaches a sub, never a `field` of the
  same name; a value read never resolves to a method, so `$php->sucks`
  without parentheses on a class that only declares `sucks()` is an
  undeclared property, not a resolved call.
- **`Shared`** (C++, Python): one namespace holds data members and
  member functions alike, and the nearest declaration of the name
  answers either ask. `d.hook()` reads member `hook` and calls whatever
  it holds, and a derived `hook()` hides a base data member `hook` for a
  value read too.

The callable-only arms of the walk (plugin entities bridged to the class,
cross-package typeglob installs, plugin bridges from other files) answer
only an ask that reaches callables.

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
is the VALUE, and that already flows: a `has` accessor is a `Method` the
call walk reaches, and `member_value_type` answers a method's return first
and a field's value second, so an accessor read types without a second ref
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
