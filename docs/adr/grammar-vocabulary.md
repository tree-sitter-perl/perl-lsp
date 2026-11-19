# ADR: The grammar vocabulary

## Context

tree-sitter names a grammar's fields and node kinds by string, and resolves
both by id. `child_by_field_name` searches the grammar's field table with
`strncmp` on every call; `Node::kind` hands back a C string that is measured
and UTF-8-checked into a `&str` before any comparison runs. On 228 FHEM files
(2026-11-19, one thread, callgrind) field reads by name were 5.3% of all
instructions, 3.9% of it the name search alone, and kind strings 4.3%.

A string also fails silently. A misspelled field answers `None`, a misspelled
kind never matches, and a grammar release that renames either drops behaviour
with nothing to say so.

## Decision

`build.rs` links the Perl grammar and writes `cst::perl::{Field, Kind}`: one
variant per field and per named, visible node kind, each variant's
discriminant the grammar's own id.

- **A name the grammar lacks is a compile error**, at every site that uses
  it, on the grammar bump that removed it.
- **Reads are by id.** `NodeExt::field(Field)` is `child_by_field_id`;
  `NodeExt::kind_of()` is one table load from `kind_id()`; `NodeExt::is(Kind)`
  compares two of them.
- **A kind name is one `Kind`**, however many symbols the grammar gives it.
  Aliases put several internal ids behind one name (`scalar`, `block`,
  `varname`, …); the table maps each to the name's variant, and the
  discriminant is the public id, the one a node reports and the one the core
  tree's kind column stores.
- **`Kind::Token`** is every node the grammar does not name (punctuation,
  keywords), **`Kind::Error`** a tree-sitter ERROR node. A token is still
  told apart by its text.

Each grammar has its own module: `cst::perl`, and `cst::pod` for the POD
grammar, generated the same way. `NodeExt::field` takes any grammar's `Field`
(`GrammarField`); a field id means nothing on another grammar's tree, and
the tree does not say whose it is, so the module a site imports is the
statement of which grammar it reads. A pack reads its grammar through its
query document (rule #15), which is where its node kinds already live.

`layering_tests::tree_fields_are_read_by_id` ratchets the by-name field
reads, shrink-only. The C++ reparse is what is left: it compiles in builds
that do not link the C++ grammar, so the build script has no table to
generate its vocabulary from.

`cst::syntax_tests::the_generated_vocabulary_agrees_with_the_grammar` walks a
parse and holds every node's `Kind` to the node's own kind string, and every
field id to the child its name reads.

## Cost

The build script links the grammars, so a clean build compiles the Perl
and POD parsers once more for the host.
