# ADR: Symbol flags are a closed set every language maps onto

A symbol's declaration facts — static, abstract, non-public, extern, a
generated reader — are a `bitflags!` set, `SymbolFlags`, and consumers
ask `contains(..)`. No consumer compares an attribute string.

## One vocabulary, one table

`TryFrom<&str> for SymbolFlags` is the canonical spelling → flag table.
A pack's skeleton conversion feeds it the attribute tokens its queries
captured; a spelling the table does not know is display text (cpp's
`register`, a php `#[Attr]` name) and mints nothing. Perl feeds it
through its own spelling table, `conventions::field_attribute_flag`
(`:param` → `PARAM`, `:reader` → `READER`, the `:writer` / `:mutator` /
`:accessor` family → `WRITER`), because Perl's spellings differ from the
canonical names while the facts do not: a Corinna `:reader` and a Moo
`is => 'ro'` both say "this slot has a generated reader". Perl is a
declarer like any pack; nothing about the flag set knows which language
minted it.

A pack's queries may declare a flag as a capture-side attribute token
where the fact is structural rather than written — `class_rail` on a
handler def minted from a class-keyed rail capture (`@def.handler.class.
<rail>`), whose token belongs to another symbol: `CLASS_RAIL` is what
the listing verdict asks, never the attribute string.

A flag can also state what a declaration's SHAPE says about its value:
`CALLABLE_VALUE` marks a stored slot whose value is invoked (a C
function-pointer member, `int (*read)(char *)`), minted from the
`@deref.callable` level of the declarator peel. `MemberKind::admits_decl`
asks it, so `ops->read(buf)` resolves to the slot a call would otherwise
miss — without a list of callback names anywhere.

A declaration the source never wrote carries `SYNTHESIZED`: the LANGUAGE
gives every enum its `->value` and `::cases()`, and those members are
minted at the enum's own name token, resolvable and completable like any
other. The flag is what the dead-code guard asks — nothing in the source
could reference such a member into existence — so no consumer has to know
which names a runtime provides.

A class that answers ANY member name at runtime carries
`DYNAMIC_MEMBERS` — php's `__call` / `__callStatic` / `__get`, Perl's
`AUTOLOAD`. The fact is the CLASS's, not the member's: the extractor
stamps it on the container whose body holds a `@def.method.catch_all`
declaration and the Perl builder stamps it on the package that declares
`AUTOLOAD`, so `class_answers_any_member` answers it with one `INHERITS`
walk and no lane compares a member name against a per-language list.

## Why closed

Every flag added so far has turned out to have a language-generic
meaning, which is exactly what lets a consumer ask the flag instead of
the string. The set also rides the cache blob as a bare `u32` (an
explicit serde form, so a flag added at the tail reads old blobs
unchanged), so a frontend-private bit would need a stable allocation per
frontend or blobs would misread across packs.

## The open-set fork

If a frontend ever needs a flag core never reads, the seam is this table:
a pack would declare an attribute-to-flag table of its own over a
pack-reserved bit range, owning both the mint and the only reader, and
the core would carry the bits opaquely. That is a table entry, not a new
mechanism, and it is not built until a consumer exists.

## Validation

An unknown spelling is an error at the producer (`UnknownAttribute`),
never a silent `continue`. Query-declared attribute spellings are
validated when the overlay compiles; source-token attributes are checked
at mint.

`layering_tests::language_spellings_have_one_home` probes both halves of
the round trip. It derives the spellings it looks for from this table, by
reading `core_types.rs` and splitting the `TryFrom` arms — which means a
reformat of those arms silently empties the probe's list, and the
`out.len() >= 20` floor is the only thing standing between that and a
green run over an unprobed tier. **The non-fragile shape is a macro that
generates the `TryFrom` arms and the spelling list from one literal**; it
is debt, recorded here because the scrape looks deliberate and is not.

The probe can only see spellings that already HAVE a flag, so an
attribute with no twin is invisible to it. That gap is closed by a second
half: every attribute literal the adapter compares must be a twinned one
or appear in the count-exact, shrink-only `UNTWINNED_ATTRIBUTES` list,
which is currently empty.
