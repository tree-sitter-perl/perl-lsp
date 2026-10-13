# ADR: the pack tier owns no grammar

A `LangPack` is the per-language half of query-driven extraction
(`query-extraction-rings.md`): the query document plus the host
predicates a pattern cannot express. The predicates are an escape hatch,
and an escape hatch widens. A Rust table of node kinds, field names,
operator tokens or callee names on the pack states a fact the `.scm`
beside it already states, in a place the query cannot see.

Two things go wrong, both quietly. The table and the document disagree
after one of them is edited, and the consumer reading the table answers
for a shape the extractor never matched. And a plugin dir can extend a
document but not a table, so an overlay that teaches the language a new
shape teaches the extractor and nothing else.

## Decision: a three-way line

**Shapes and small closed keyword sets are captures.** Node kinds, field
names, operator tokens, and the handful of names a pattern must fire on
(`parent`, `assert`, `isinstance`, `clear|reset|swap`) live in the `.scm`
as a capture with a suffix plus an `#eq?` / `#any-of?` predicate. The
extractor routes on the capture name (`capture-vocabulary.md`), which is
the language-neutral contract it already speaks.

**Large or open vocabularies are data documents.** A runtime's core class
names, its magic methods, the names a framework runner invokes: sets that
grow, differ per framework, and that a plugin dir must be able to extend.
They are documents the pack registers, read by an engine-side reader.

**A Rust `fn` on the pack is right only for text→structure.** Parsing a
type spelling, a docblock, a module path; shaping a captured name. It
takes text in and gives structure out, never a node and never a kind.

**A Rust table of node kinds, field names, token texts or callee names is
right for nothing.** CLAUDE.md rule #15 states it.

**A consumer never re-recognises a shape.** A fact the extractor could
mint from a capture (this method is the constructor, this receiver names
the enclosing class) is a flag or a ref kind minted at extraction, and
the consumer asks the symbol. Rule #11 at the pack tier.

## Decision: the tripwires read the pack, the grammar and the sources

- `LangPack::declared_strings` reflects every `&'static str` a pack
  declares, tagged with its field. It destructures the whole struct, so a
  new field does not compile until its strings are declared or it is named
  as holding none.
- `layering_tests::pack_fields_name_no_grammar_shapes` compares each
  declared string to the language's own node kinds and field names, read
  off the grammar, so it cannot lag a rename.
  `pack_string_tables_are_ratcheted` counts the rest: vocabulary the
  grammar cannot check (`"main"`, `"self"`). Both allowlists are
  count-exact, keyed `(language, field)`, filtered to the languages the
  build serves, and shrink only. Each entry says what change moves it.
- Trigger characters are on neither list. They are the LSP client's
  vocabulary, and where they collide with a grammar's anonymous tokens
  they do so by coincidence.
- `packs_do_not_borrow_perls_current_package_token` keeps Perl's
  `__PACKAGE__` (`conventions::CURRENT_PACKAGE_TOKEN`) out of every pack
  source. A pack mints the class its receiver names; a borrowed Perl token
  would put Perl's vocabulary into an analysis of a language that has no
  such word.

The neighbouring rules read the same tiers. Rule #12's probe covers the
model, index and adapter tiers; its attribute half reads the spellings out
of `TryFrom<&str> for SymbolFlags`, so a flag added there is probed from
the same commit, and an attribute literal a consumer compares with no flag
to answer to is named in `UNTWINNED_ATTRIBUTES`. Rule #13's split probe
covers the model and the adapter. Rule #14's `PackFacts` ratchet counts
the fields the pack lane carries per file.

## Consequences

- A fn is code the reflection cannot see into. A callee name inside one
  (`rebind_method`, `narrow_guard`) escapes both tripwires; moving it into
  the document is still rule #15's work, done per language as each lane
  moves. R's constructors and imports and CMake's command families already
  live in their documents.
- A capability the document already states is read off the compiled
  document, never kept as a pack bool beside it: `#include`-style path
  tokens are the document minting `@import.path`, a preprocessor is it
  minting `@def.macro` (`LanguageRegistry::document_mints`). The bools
  left (`implicit_this_members`, `brace_scoped_members`) hold no strings
  and are not counted; each is a statement the document can make about
  itself.
- The allowlists today are the cursor sentinel's node-kind tables, cpp's
  declarator and receiver peels, cpp's member-operator map, the
  receiver-name lists, and cpp's `main` entry point.
