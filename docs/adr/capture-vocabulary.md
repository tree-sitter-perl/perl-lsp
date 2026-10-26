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
use, named `<Family>Cap`. The families that name an entity kind stay
typed past the driver: `DefCap` is `SkelSymbol.kind` and `RefCap` is
`SkelRef.kind`, so the projection to `FileAnalysis` matches on the enum
too. The `Cap` suffix keeps them apart from the model's `RefKind` /
`SymKind`, which they project to.

Each enum's spelling table is written once, in a `spelled!` invocation,
which generates `parse`, `spelling` and `Display` from it. The two
directions cannot disagree.

## Decision: a capture name is read as text in one place

`Capture::parse` turns a name into a `Capture`. Nothing else reads a
capture name: the driver, the pack hooks and the projection all match on
the enum. A pack hook that depends on a kind takes the typed kind
(`default_name: fn(DefCap)`).

## Decision: the vocabulary is checked when the query compiles

A compiled pack query (`CompiledPack`) carries its capture table beside
it, one `Option<Capture>` per capture id, built when the query first
compiles. A name outside the vocabulary fails the compile with
`unknown capture @<name>`, so a typo or an unlearned capture in a bundled
document fails that pack's tests instead of extracting nothing.

A name starting with `_` follows tree-sitter's predicate-only convention
(`@_const` under `#eq?`). It maps to `None`, and the driver never sees it.

## Decision: structural predicates are a closed set too

Core tree-sitter evaluates text predicates (`#eq?`, `#match?`,
`#any-of?`) and hands every other predicate to the caller unevaluated.
A document needs one more kind of filter: where a node sits. A member
read is a `field_expression` that is not a call's callee, and that fact
lives on the parent, which a negated field (`!arguments`) cannot test.

`build/query_predicates.rs` evaluates Neovim's structural forms, with
the same names and arguments: `#has-parent?`, `#not-has-parent?`,
`#has-ancestor?`, `#not-has-ancestor?`, each `@capture kind ...`. Every
node the capture holds must satisfy it. `check` runs when a query
compiles and fails it on any other predicate name, a malformed argument
list, or a kind the grammar lacks, because an unread predicate is a
filter that silently matches everything. `admits` runs per match, before
any capture is recorded. Both query runners use them: pack extraction,
and the Perl plugin patterns, where `#receiver-isa?` is the one predicate
deferred to query time.

Ancestry is read by one descent from the walked root
(`child_with_descendant`), because `Node::parent` descends from the root
too and would repeat the walk per hop. Only patterns that carry a
structural predicate pay it.

## Decision: a def's name token and its written spelling are two captures

A def has three parts a document may capture: the node (`@def.<kind>`), its
name token (`@def.<kind>.name`), and, when the name token is only part of
what is written, the whole written spelling (`@def.<kind>.spelled`, php's
`$x` around `x`), which is the label a listing shows. It is the def-side
twin of `@ref.spelled`: the token keys and is rewritten, the spelling is
read.

A variable read splits the same way. `@expr.read.var` is the node whose
value the read is; `@expr.read.var.name`, when the node also writes a sigil
(php's `$x`), is the token that is the reference, the name the read
resolves by, and the span a rename rewrites.

`@def.pair` sits on a name token one match declares as two defs (php's
promoted constructor parameter, a property and a local). It is opt-in on
the pattern because two defs in one match are a pair only when the
document says so; the extractor links them as `Symbol::declared_with`.

`@receiver.<kind>` sits on a receiver token whose value is a class, so a
call on it dispatches there (`Lookup::Named`). `class` names the class it
spells (php's `Foo::m()`, through the use-map); `enclosing` names the class
of the method the token is written in (php's `$this`, `self::`,
`static::`). Which tokens these are is the document's: the extractor holds
no receiver spelling.

`@rettype` is a callable's declared return, captured in the def's match.
`@rettype.enclosing` sits on a return the document says names the class the
method is written in rather than a class by its spelling (php's `self`,
`static`). It is a pattern of its own, joined to the def's `@rettype` by
span.

## Decision: a kind's meaning lives on the kind

What a kind means is a method on its enum, matched exhaustively:
`DefCap::sym_kind` (the `SymKind` it projects to), `DefCap::marker`
(the `union` / `reexport` / `macro` attribute and the `SymbolFlags` it
implies), `MarkCap::flag` (the `SymbolFlags` a `@mark.<flag>` states about
the declaration whose name token it captures), `LitKind::value_type`,
`ObsKind::observation`. A new kind does not compile until it says what it
is.

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

## Decision: a fact the grammar parses is a capture, not a text probe

When the grammar already gives a fact its own node, the document captures
that node and the driver reads the capture; it never re-derives the fact
from the text of a wider capture. Python parses `from ..x import C` as
`relative_import(import_prefix "..", dotted_name "x")`, so the dots are
`@import.relative` and the module is `@import.prefix`, joined to each
`@import.name` by match. Whether an import is relative is whether the
capture fired, and its level is the number of separators the capture
holds. What a level climbs from is the language's `ImportPrefix` rule
(`PackageRelative` for Python); a language without relative imports
(PHP's `use \A\B\{C}`) never captures one.

## Decision: depth is chained from links, never walked

A tree-sitter pattern has a fixed depth: no descendant axis, no recursion,
and quantifiers repeat siblings, never nesting. Some structure nests
without bound, such as a declarator wrapped any number of times
(`Foo**& f()`) or a name qualified any number of times (`A::B::m`). One
pattern per depth would silently miss the next depth.

So the document states each level once, as a link: `@link.from.<walk>` on
a node and `@link.<rel>` on the node one level away, by a closed set of
relations (`inner`, `declares`, `scope`, `name`, `base`). The `<walk>` suffix
names the walk the link belongs to, from a closed set (`ool`, `qualifier`,
`deref`), and each walk has a root capture (`@ool.def`, `@qualifier`,
`@nested.target`). The `deref` walk also reads `@deref.*` marks, which say
what a level is rather than where it leads (`docs/adr/pointer-stack.md`). The driver
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
