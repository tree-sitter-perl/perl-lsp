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

The rule has a consumer half, and it reaches past the pack: a name set
handed DOWN to a model lane so the lane can `contains` it is the same sin
with the split replaced by a match. What a capture says about a
DECLARATION is `SymbolFlags`; what it says about a REFERENCE SITE — the
receiver's flavour, whether the call constructs — is `RefFlags`, minted in
the extractor's one stamping pass and read through `Ref`'s accessors. A
set of spellings reaches the model only when there is no per-site fact to
mint: a runtime's builtin type names, or a capability the document states
about the language rather than about a site.

The same rule governs the engine's own walks. A parameter's shape is not a
node kind the extractor knows — it is `@arity.param`, `@arity.param.optional`,
`@arity.param.variadic`, `@arity.param.byref`, and the parts
(`@arity.param.name` / `.default` / `.type`) the document anchors on the
parameter. The engine joins a parameter to the signature it is a child of
and a part to the parameter that encloses it, and counts; a token no
fixed-depth pattern can reach (a C declarator's `identifier`, buried under
however many pointer and array wrappers the type wrote) is a descent whose
stopping kinds are the document's own read-pattern roots.

## The cursor-time seam

A consumer that keeps node-kind tables is usually a cursor consumer: the
sentinel needs to know whether the cursor sits in a member access, a
call, or a string. It reads the document instead, through the seams in
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
- `fires_at(query, node, src, capture)` / `is_captured_as(..)` — whether
  a match rooted at `node` carries the capture (anywhere in it / on
  `node` itself), on the same three bounds, stopping at the first match
  that does. This is where a table like `member_kinds` goes once the
  table is gone: "is this a member access" is the document's own
  `@member.recv` pattern firing on the node, fields and predicates
  included, so a `binary_expression` is a domain comparison only when
  the operator predicate holds. A node that roots one match per child (a
  50,000-argument list) still costs a walk over those matches when the
  answer is no.
- `capture_literals(query, capture)` — the literals a capture's `#eq?` /
  `#any-of?` predicates name. The bindings keep text predicates private,
  so `cached_query` reads them through the C API between
  `Query::into_raw` and `Query::from_raw`, once per compiled query.
- `recovery(query)` — what the document says about half-typed code: the
  bracket pairs (`(#recover-pair! "(" ")")` on the pattern whose construct
  they delimit; several closers for one open are tried in declaration
  order) and the statement terminator (`(#set! recover.terminator ";")`).
  Directives never filter a match, so they can sit on any pattern. The
  sentinel closes the innermost open bracket it finds unmatched in the
  sentinel's damaged region, then appends the terminator unless the
  grammar already inserted a MISSING one, and keeps the first splice
  whose construct parses whole. A language that declares no pairs gets no
  recovery. The attempts are bounded by the declarations, but each is a
  reparse; error-dense input makes every reparse slow
  (`docs/scaling-limits.md` §9).

### What the three bounds cost (php, measured 2026-09-17)

Nine classifier modes in one binary over 25 inputs, three process runs
each, medians, against the sentinel reparse paid in the same call.

Unbounded — the whole skeleton query rooted at `tree.root_node()` with
`set_byte_range(member)` — is a per-keystroke regression, because
`set_byte_range` prunes DESCENT and never the walk TO the range: every
pattern rooted at an intersecting ancestor keeps a state alive that
forces descent through each sibling subtree before the cursor. One class
of 5,000 methods, the same 17-byte member span, cost 68 µs with the
cursor in the first method and 19.3 ms in the last; 100k statements
before the cursor cost 284 ms; a minified 1 MB line, 175 ms. The bill is
the prefix of the file.

Slicing the document down to the 19 patterns carrying `@member.recv`
escapes the prefix walk and loses elsewhere: 5.8 ms on 5,000 nested
blocks (one match, all descent), Θ(depth²) on a member chain (55 ms at
2,000 hops), 513 ms on a 1.29 MB receiver — plus a second `Query::new`
(104 ms on the first completion after startup, 192–320 KB resident). It
is not the fix.

With all three bounds the full query classifies in 1.4–2.7 µs on files up
to 6 KB, 3.6–9.5 µs on real 122–358 KB class files, and 5.9–18.4 µs on
0.6–1.2 MB synthetic giants — 0.06–10% of the reparse beside it, with no
second compile and no extra resident bytes. The depth cap is what carries
the two pathological shapes: the 2,000-hop chain 55 ms → 1.7 µs, the
1.29 MB receiver 512 ms → 126 µs. Declines (cursor at byte 0, inside a
string or comment, no member access) cost zero in every mode, and syntax
damage is not pathological — the enclosing node collapses and there is
nothing to walk.

Not yet measured: cpp, whose receiver patterns have a different shape.
Forward: once the extractor mints per-call argument spans as facts, the
inlay-hint path (`calls_in_rows`) reads them off the analysis and walks
no tree at all — the extractor already saw every call (rule #11).

### A doc-comment vocabulary is a hand parser, not a grammar

php's docblock reader stays Rust (`packs/php/doc.rs`), and each frontend
owns its own. Measured 2026-09-17 against `tree-sitter-phpdoc` v0.1.8
over 23,051 real `/**` comments (laravel/framework, phpstan-src): 82.8% /
70.0% parse without an ERROR node, ~92% / 84% with four small upstream
fixes, and the residual is callable types and unions inside generics.
Tree-sitter's recovery is not local, so on a block with an ERROR node
whole-block parity with the hand parser is 15% and the facts that do
emerge are wrong ones — at 21.6 µs per docblock against 1.1. A grammar in
that shape would have to be forked to be usable, which is the opposite of
the reason to adopt one.

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
node's capture list. The predicate literals behind `capture_literals` are
scanned by hand for the same reason: `#eq?` / `#any-of?` fold into
tree-sitter's internal text predicates and the compiled query exposes
neither. Both scanners step over string literals, because a literal may
contain the character that ends the form.

`layering_tests::bundled_query_documents_are_served_whole` runs the three
detectors over every registered pack's skeleton and every bundled overlay
under `cargo test`, so none of them depends on a CLI flag being
remembered.

## Structure

Each language's declarations live in `src/build/packs/<lang>` — `perl`,
`python`, `r`, `cmake`, `cpp`, and `php` (whose text→structure half is
`php/doc.rs`). `query_extract/packs.rs` holds the contract: `LangPack`,
its spec types, and the reflection the tripwires walk.
