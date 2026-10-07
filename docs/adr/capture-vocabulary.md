# ADR: the capture vocabulary is a closed enum

## Context

A pack language's extraction is a query document plus a generic driver
(`query-extraction-rings.md`). The document owns the language's syntax:
which node is a def, which token names it, which node opens a scope. The
driver owns what a capture MEANS: a capture's name is its contract
(`@def.sub`, `@ref.call`, `@flow.target`), and the driver routes on it.

That contract was spelled only as strings. The driver matched names with
string arms and `starts_with("def.")` guards, the symbol and ref rows
carried their kind as a `String`, and the downstream projection compared
that string against literals. A capture name nobody handled was silently
inert: an unknown `def.<kind>` minted a symbol that fell through to
Variable, an unknown `ref.<kind>` was dropped. A typo in a document, or a
document teaching a capture the driver never learned, extracted nothing
and said nothing.

## Decision

**One enum is the vocabulary.** `query_extract/vocab.rs` declares
`Capture`: one variant per capture family, each carrying a sub-enum of
exactly the suffixes the documents use. The families that name an entity
kind are typed downstream too: `DefKind` is `SkelSymbol.kind`, `RefKind`
is `SkelRef.kind`. Each enum's spelling table is written once, and both
`parse` and `spelling` are generated from it.

**`Capture::parse` is the one place a capture name is read as text.**
Everything past it matches on the enum. A pack hook that used to take a
capture name takes the typed kind (`default_name: fn(DefKind)`), or drops
the parameter when no pack read it (`shape_name`).

**The vocabulary is checked when the query compiles.** The compiled query
caches its capture table beside it, one `Option<Capture>` per capture id.
A name outside the vocabulary fails the compile, so a bundled document
with a stray capture fails its pack's tests rather than extracting
nothing. Captures whose name starts with `_` are tree-sitter's
predicate-only convention (`@_const` under `#eq?`); they map to `None`
and the driver never sees them.

**The kind's meaning lives on the kind.** What a `DefKind` projects to
(`sym_kind`) and the structural marker it carries (`marker`, the
`union` / `reexport` / `macro` attribute and its `SymbolFlags` twin) are
methods on the enum, matched exhaustively. A new kind fails to compile
until it says what it is.

## Consequences

- A document that wants a new capture adds a variant first. The
  vocabulary change is an enum diff, reviewable on its own.
- A capture the driver routes nowhere is still possible (a pattern
  anchor such as `@flow.assign`), but it is now a named variant the
  routing match lists, not a string that fell through.
- The rhai plugin patterns (`pattern_dispatch.rs`) and cpp's internal
  reparse queries name their own captures. They are separate
  vocabularies, read by their own code, and this enum does not cover them.
- The cost is one capture table per compiled pack query, built once per
  process.
