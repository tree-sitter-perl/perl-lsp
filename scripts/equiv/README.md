# Whole-analysis equivalence (`scripts/equiv/`)

The check a refactor PR answers with: **over every input, the head binary
builds the same analysis the base binary builds.** It stands in for reading
a no-behavior-change diff line by line.

```
scripts/equiv/run.sh                 # base = merge-base with origin/main
scripts/equiv/run.sh <rev>           # any base that has --equiv-dump
EQUIV_TESTS=0 scripts/equiv/run.sh   # CLI inputs only (minutes, not a test run)
```

Exit 0 means no input changed and none was dropped. A refactor PR's body
shows the summary lines (`equiv: N inputs ... 0 changed, 0 dropped`), the
base sha, and the input counts.

## What is compared

`FileAnalysis::equivalence_dump` (`src/model/file_analysis/equiv_dump.rs`):

- **the analysis**: every serialized field (symbols, refs and their bindings,
  scopes, the witness bag, package facts, pack and plugin lanes), maps sorted,
  the `HashSet` fields sorted, every other sequence in emission order;
- **the answers**: the reducer registry's answer for every attachment the
  bag holds, so a reducer change that leaves the witnesses alone still shows.

It is the BUILD output, before cross-file enrichment: a function of the
source and the binary alone. Enrichment, resolution and the LSP projections
are covered by gold, e2e and the answer census, not here.

## Inputs

| Set | Where | How it is fed |
|---|---|---|
| repo fixtures | `test_files/`, `gold-corpus/*-fixture/` | `--equiv-dump` |
| gold substrate | `gold-corpus/local/` (~2,265 modules; `dev-setup` skill) | `--equiv-dump` |
| grammar corpus | `../tree-sitter-perl/test/corpus`, one input per entry | `split_corpus.py`, then `--equiv-dump` |
| C/C++ fixtures | `test_files/cpp`, `gold-corpus/cpp-*` | `--lang cpp --equiv-dump` |
| every build the test suite makes | inline sources, all languages | `PERL_LSP_EQUIV_DUMP_DIR` under `cargo test` |

A worktree has no `gold-corpus/local`; the runner says so, and
`EQUIV_EXTRA=<checkout>/gold-corpus/local` puts the substrate back.

Inputs are always read from the HEAD tree, so a fixture the change edits is
the same bytes on both sides. A test the change adds shows as `NEW` (not a
failure); an input only base builds shows as `DROPPED` (a failure: something
stopped being built).

## Reading a failure

`diff.py` prints, per changed input, where it came from (`.where`, or the
first line of an inline test source, kept beside the dump as `.src`), the
JSON path of the first difference, and a unified diff of the re-indented
dumps. `diff.py BASE HEAD --show N` lists more.

A dump name is `<input>.<content>.json`. `<input>` hashes language + source
(+ the module a path-named pack file is); one input built two ways in one run
(a test's own plugin set, a pack with and without include context) leaves
one file per distinct result, and the SET per input is what is compared.

## Trust

- **Null check:** base = head must report 0 changed. Measured 2026-10-31:
  3,905 CLI inputs and 2,355 cargo-test inputs, 0 changed. Getting there
  took three fixes, each a fact spelled in a process-dependent way: the
  include closure serialized in intern order (now in `SET_FIELDS`), absolute
  paths of the two checkouts and of test temp dirs (`diff.py` rewrites
  `--root` trees to `<root>` and `/tmp/<x>` to `/tmp/<tmp>`, and pairs inputs
  whose sources differ only there), and a test spelling its pid into a
  source (a span shift no rewrite can undo; the pid is fixed-width now).
- **Mutation checks** (2026-10-31): dropping `SymbolReturnArmFold`'s
  all-undef guard changed 4 inputs; spanning a quoted hash key's quotes
  changed 95. A mutation that changes nothing on any input (first-wins in
  `SubReturnReducer`, whose attachments never carry two types here) shows
  nothing, as it should.
- **Hash order:** `canonical_dump_survives_a_rehash` (cargo test) rehashes
  every fixture's analysis and fails if the dump moves — a new `HashSet`
  field missing from `SET_FIELDS` is caught there, not as a phantom diff.

## Cost

The CLI half is about 3 s of wall per thousand files (4 cores, release). The
test half is a full `cargo test` per side in its own target dir
(`$EQUIV_WORK/target-<side>-test`), kept between runs so a second run at the
same base is incremental. Dumps go under `$EQUIV_WORK/runs/<stamp>/`, about
1.1 GB a run with the substrate in; a passing run deletes them (keep them
with `EQUIV_KEEP=1`), a failing one keeps them for `diff.py --show`. The
target dirs (release + test, per side) are the other ~13 GB.
