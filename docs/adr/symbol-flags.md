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
