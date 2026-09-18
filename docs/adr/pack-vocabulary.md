# ADR: the pack tier owns no grammar

## Context

A `LangPack` is the per-language half of query-driven extraction
(`query-extraction-rings.md`): the query document plus the host
predicates a pattern cannot express. The predicates are the escape
hatch, and an escape hatch widens. Over five languages the pack
accumulated Rust tables of grammar: lists of node kinds, tables of field
names, sets of operator tokens, and lists of the callee names a rule
fires on — each one a fact the `.scm` beside it already states, restated
in a place the query cannot see.

Two things go wrong, both quietly. A table and a document disagree
after one of them is edited, and the consumer that reads the table
answers for a shape the extractor never matched. And a plugin dir can
extend a document but not a table, so a `.scm` overlay that teaches the
language a new member shape teaches the extractor and nothing else.

## Decision: a three-way line

**Shapes and small closed keyword sets are captures.** Node kinds, field
names, operator tokens, and the handful of names a pattern must fire on
(`parent`, `assert`, `isinstance`, `clear|reset|swap`) live in the
`.scm` as a capture with a suffix plus an `#eq?` / `#any-of?` predicate.
The extractor routes on the capture NAME, which is the language-neutral
contract it already speaks.

**Large or open vocabularies are data documents.** A runtime's core
class names, its magic methods, the names a framework runner invokes:
these are sets that grow, that differ per framework, and that a plugin
dir must be able to extend. They are documents the pack registers —
entry documents, rail documents, stub sources — read by an engine-side
reader.

**A Rust `fn` on the pack is right only for text→structure.** Parsing a
type spelling, a docblock, a module path; shaping a captured name. This
is the work a query medium genuinely cannot do, and it takes text in and
structure out — never a node, never a kind.

**A Rust table of node kinds, field names, token texts or callee names
is right for nothing.** CLAUDE.md rule #15 states it; two tripwires
enforce it. `layering_tests::pack_fields_name_no_grammar_shapes` walks
every `&'static str` a registered pack declares
(`LangPack::declared_strings`, a hand-written exhaustive destructure, so
a new field is a compile error until its strings are declared) and
compares each against that language's own `node_kind_for_id` /
`field_name_for_id`. `pack_string_tables_are_ratcheted` counts the
vocabulary tables a grammar cannot check. Both carry an allowlist that
is count-exact and shrinks only, each entry naming the slice that moves
it.

**A consumer never re-recognises a shape.** A fact the extractor could
mint from a capture — this method is the constructor, this class answers
any member, this call reads arguments it never declared — is a flag or a
ref kind minted at extraction; the consumer asks the symbol. Rule #11 at
the pack tier.

## The cursor-time seam

A consumer that keeps node-kind tables is usually a cursor consumer: the
sentinel needs to know whether the cursor sits in a member access, a
call, or a string. It reads the document instead, through three seams in
`build/query_extract/cursor_query.rs`:

- `pack_query(pack)` — the compiled query the EXTRACTOR uses, memoised
  per language at the one site that compiles it. A keystroke costs a map
  lookup; re-deriving the effective source would cost a plugin-dir
  `read_dir` and a hash of every overlay.
- `captures_at(query, node, src)` — the captures of every match whose
  pattern roots at `node`. Three bounds, all load-bearing: the cursor
  runs on `node` rather than the tree root, `set_byte_range` keeps it
  inside the subtree, and `set_max_start_depth(Some(0))` admits only
  patterns rooted at `node` itself. The depth cap is the one a reader is
  tempted to drop; without it a large subtree (a long member chain, a
  class body) is matched at every descendant, which is the difference
  between ~2 µs and ~20 ms per keystroke. The cost signature is pinned
  by a test on a 2,000-hop chain inside a 5,000-method class.
- `pattern_root_kinds(query, capture)` — the node kinds that root a
  pattern carrying a capture, read off the compiled query's
  `capture_quantifiers` and its pattern sources. This is where a table
  like `member_kinds` comes from once the table is gone: "which nodes
  are a member access" is answered by the patterns that capture
  `@member.recv`. A pattern whose root is not a named node (an anonymous
  token, a wildcard, a grouped sibling pattern) names no kind, so the
  set is what a consumer may match ON, never a claim that nothing else
  can carry the capture.

## Spellings by language id

A language's WRITE and DISPLAY spellings — its display vocabulary, the
native spellings a quick-fix writes, the import / stub / return-annotation
templates, the static-property sigil, the class-name literal, and whether
members are package-bound — are the same for every file of the language.
They are `model::file_analysis::PackSpellings`, one `const` per pack,
reached by id through `LanguageRegistry::spellings`; an analysis carries
a `#[serde(skip)]` pointer and consumers ask `FileAnalysis::spellings()`.
A language that declares none answers `PackSpellings::NONE`, which is
what every surface assumed before packs existed — so the host language
reads unchanged.

Because the pointer never rides the blob, every path that rebuilds a
`FileAnalysis` from bytes re-attaches it (`attach_spellings`, called by
the blob decode and the warm-stub decode). That is the one silent failure
this seam has — an unattached analysis answers the neutral defaults,
indistinguishable from a language that declares none — so
`layering_tests::decoded_pack_analyses_carry_spellings` round-trips every
registered pack through both codecs.

## Query gotchas

**An underscore capture is a predicate anchor and nothing reads it.**
`@_plain_row`, `@_narrow_guard`, `@_cmd`: the name exists so an `#eq?` /
`#any-of?` / `#not-match?` can be written beside it, and the fact the
pattern states is minted by its MATCH or by a sibling capture. The
spelling says so — a reader looking for what consumes `@_narrow_guard`
stops at the sigil instead of grepping — and `unserved_captures` treats a
`_` capture as served by definition. A capture the extractor or the cursor
runner reads BY NAME (`@skip`, `@recv.peel`, `@arity.arg`, `@member.recv`,
`@expr.read.var`, `@domain.compare.op`) carries no underscore however
declaration-like it looks: those are read.

**A query step holds at most three captures.** tree-sitter's
`MAX_STEP_CAPTURE_COUNT` is 3 and `query_step__add_capture` no-ops past
the third: the document compiles, `Query::capture_names()` still lists the
fourth name, and the capture simply never fires. The symptom is a whole
lane going dark with nothing to read — php's assignment pattern lost
`@flow.target` this way and every assignment stopped typing, with no
error anywhere. The fix is to anchor the extra capture on the pattern
root or on a sibling node, which is usually where it belonged: four
captures on one node is generally four questions asked of one token.

`--plugin-check` reports it (`dropped_step_capture_findings`), counted
over the document SOURCE rather than the compiled query — the Rust
`Query` API exposes patterns, capture names and quantifiers, but no
per-step capture list, so the compiled form cannot answer which capture
was dropped. Four or more consecutive `@name` tokens in the source is one
node's capture list.

## Structure

Each language's declarations live in `src/build/packs/<lang>` — `perl`,
`python`, `r`, `cmake`, `cpp`, and `php` (whose text→structure half is
`php/doc.rs`). `query_extract/packs.rs` holds the contract: `LangPack`,
its spec types, and the reflection the tripwires walk.
