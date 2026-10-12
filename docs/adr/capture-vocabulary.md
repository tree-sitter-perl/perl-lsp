# ADR: the capture vocabulary is a closed enum

A pack language's extraction is a query document plus a generic driver
(`query-extraction-rings.md`). The document owns syntax: which node is a
def, which token names it, which node opens a scope. The driver owns
meaning, and it reads meaning off the capture's name: `@def.sub`,
`@ref.call`, `@flow.target`. The capture name is the whole contract
between the two, so the set of names is a vocabulary both sides must agree
on.

## Decision: one enum is the vocabulary

`query_extract/vocab.rs` declares `Capture`: one variant per capture
family, each carrying a sub-enum of exactly the suffixes the documents
use. The families that name an entity kind stay typed past the driver:
`DefKind` is `SkelSymbol.kind` and `RefKind` is `SkelRef.kind`, so the
projection to `FileAnalysis` matches on the enum too.

Each enum's spelling table is written once, in a `spelled!` invocation,
which generates `parse`, `spelling` and `Display` from it. The two
directions cannot disagree.

## Decision: a capture name is read as text in one place

`Capture::parse` turns a name into a `Capture`. Nothing else reads a
capture name: the driver, the pack hooks and the projection all match on
the enum. A pack hook that depends on a kind takes the typed kind
(`default_name: fn(DefKind)`).

## Decision: the vocabulary is checked when the query compiles

A compiled pack query (`CompiledPack`) carries its capture table beside
it, one `Option<Capture>` per capture id, built when the query first
compiles. A name outside the vocabulary fails the compile with
`unknown capture @<name>`, so a typo or an unlearned capture in a bundled
document fails that pack's tests instead of extracting nothing.

A name starting with `_` follows tree-sitter's predicate-only convention
(`@_const` under `#eq?`). It maps to `None`, and the driver never sees it.

## Decision: a kind's meaning lives on the kind

What a kind means is a method on its enum, matched exhaustively:
`DefKind::sym_kind` (the `SymKind` it projects to), `DefKind::marker`
(the `union` / `reexport` / `macro` attribute and the `SymbolFlags` it
implies), `LitKind::value_type`, `ObsKind::observation`. A new kind does
not compile until it says what it is.

## Consequences

- A new capture starts as a new variant. The vocabulary change is an enum
  diff, reviewable apart from the handler that reads it.
- A variant can have no reader. `@flow.assign` names a pattern's root so
  the document can anchor `@flow.target` and `@flow.source` under it; the
  driver reads the two halves, not the anchor.
- The enum covers the pack skeleton documents only. `perl/flow.scm` (read
  by `builder/narrowing.rs`), the rhai plugin patterns
  (`pattern_dispatch.rs`) and cpp's internal reparse queries name their own
  captures, read by their own code.
- The capture table lives as long as its compiled query: one per pack,
  leaked to `&'static` for the life of the process.
