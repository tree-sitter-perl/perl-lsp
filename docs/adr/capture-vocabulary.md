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

## Decision: each capture family has one handler file

`query_extract/extract/` holds the driver (`mod.rs`: the event flatten,
the sort, the shared `ExtractState`) and one file per family: `scopes`,
`defs`, `refs`, `members`, `flow`, `narrowing`, `commands`, `imports`,
plus `links`, which the driver reads while flattening (below).
`family(Capture)` is an exhaustive match, so a new variant does not
compile until it names its family. The family's state (its per-match
joins) lives in the same file, on a sub-struct of the shared
`ExtractState`, and its handler sees one event at a time. A family file
may expose up to four entry points, each run by the driver at a fixed
point:

- a flatten hook, for a capture that needs its live node (a declarator to
  peel, an argument list to count);
- `collect`, a pre-pass for joins a handler reads before the capture that
  feeds them fires (a def's name, qualifier and return type sit inside the
  def node, so their events sort after the def's own);
- `handle`, called in source order with the scope stack in force;
- post-passes, which the driver lists in the order their rows and
  witnesses land. That order is load-bearing: the witness bag is
  latest-wins for some reducers, and a flow join reads the symbol table
  only after def dedup and the command defs.

A feature that teaches the extractor a capture touches the variant, the
family routing line, and the family file it lives in. When a language
needs extraction a query cannot express, its frontend adds a typed Rust
handler in the same shape rather than a string-keyed hook on the pack.

## Decision: depth is chained from links, never walked

A tree-sitter pattern has a fixed depth: no descendant axis, no recursion,
and quantifiers repeat siblings, never nesting. Some structure nests
without bound, such as a declarator wrapped any number of times
(`Foo**& f()`) or a name qualified any number of times (`A::B::m`). One
pattern per depth would silently miss the next depth.

So the document states each level once, as a link: `@link.from.<walk>` on
a node and `@link.<rel>` on the node one level away, by a closed set of
relations (`inner`, `declares`, `scope`, `name`, `base`). The `<walk>` suffix
names the walk the link belongs to, from a closed set (`ool`, `qualifier`),
and each walk has a root capture (`@ool.def`, `@qualifier`). The driver
records every link while it flattens the matches, keyed by walk and node id.
A walk starts at its root's node and follows only its own links
(`extract/links.rs`), so two walks never read each other's edges, and a
reader of the document sees which construct each link serves. Rust reads no
node kind and no field to do it; which node links to which is the
document's statement. The compile fails on a pattern with a link but no
single `@link.from.<walk>`, a `from` with no link, or a walk whose root
capture the document never uses.

## Consequences

- A new capture starts as a new variant. The vocabulary change is an enum
  diff, reviewable apart from the handler that reads it.
- Links are read before any other capture, because a level inside a
  definition is matched after the definition. The driver keeps the whole
  file's captures until every match is in, then flattens them.
- A variant can have no reader. `@flow.assign` names a pattern's root so
  the document can anchor `@flow.target` and `@flow.source` under it; the
  driver reads the two halves, not the anchor.
- The enum covers the pack skeleton documents only. `perl/flow.scm` (read
  by `builder/narrowing.rs`), the rhai plugin patterns
  (`pattern_dispatch.rs`) and cpp's internal reparse queries name their own
  captures, read by their own code.
- The capture table lives as long as its compiled query: one per pack,
  leaked to `&'static` for the life of the process.
