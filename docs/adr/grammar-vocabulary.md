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

The enums are the Perl grammar's. A node of another grammar's tree has no
Perl kind and a Perl field id means nothing on it: the three accessors read
Perl trees only. A pack reads its grammar through its query document
(rule #15), which is where its node kinds already live.

`cst::syntax_tests::the_generated_vocabulary_agrees_with_the_grammar` walks a
parse and holds every node's `Kind` to the node's own kind string, and every
field id to the child its name reads.

## Cost

The build script links the grammar, so a clean build compiles the Perl
parser once more for the host.
