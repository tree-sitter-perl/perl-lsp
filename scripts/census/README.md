# Answer census (`scripts/census/`)

The check a behavior PR answers with: **every change in what a user sees is
listed and explained.** Where `scripts/equiv/` proves a refactor changed
nothing, this measures what a behavior change changed, on real projects.

```
scripts/census/census.sh BASE_BIN HEAD_BIN [CORPUS...]   # default: every corpus
```

`BASE_BIN` / `HEAD_BIN` are two release builds; an equivalence run leaves
them at `$EQUIV_WORK/target-{base,head}/release/perl-lsp`. Corpora are the
real-project ones (`corpus/bootstrap.sh`), dependencies on `PERL5LIB` from
`deps/<name>/`, never in the workspace.

## What is compared

Per corpus, for each binary (`run.sh`, fresh cache each):

- **diagnostics**: `--check --severity hint --format json`, every row;
- **probe answers**: `--batch` over a fixed sample (`probes.py`): 15 files,
  seeded, and per line the first `$var` and the first `->method`, at most 40
  positions a file, each asked `hover` and `definition` (`CENSUS_VERBS` adds
  `references`, `completion`, ...). Same root, same seed, same probes.

`ledger.py` diffs the two: diagnostics as a multiset keyed by
(code, file, position, severity, message) and grouped by code; answers by
probe id, grouped by verb into added / dropped / changed. A list-shaped answer
compares as a set.

## The ledger

`ledger.md` holds one table per corpus. Every row is a group (a diagnostic
code or a verb and direction) with its count, one example, and a `class` the
PR author fills in:

| class | means | then |
|---|---|---|
| `fixed` | the old answer was wrong | add a gold row for the example |
| `intended` | a deliberate change | say why in the PR |
| `regression` | the new answer is wrong | blocks the PR |

The PR body carries the filled ledger. A row nobody can classify is a
regression until shown otherwise.

## Trust

- **Null check** (2026-10-31, Conch, main vs the same code): 851 → 851
  diagnostics, 7,226 / 7,226 probes identical.
- The ledger reports synthetic deltas (dropped and added diagnostics, a
  dropped definition) as separate rows with examples.

## Cost

`--check` is seconds on Conch; `--batch` answers about 7 probes a second
(each re-parses its file and resolves its imports), so the default sample is
about 850 requests a corpus, a couple of minutes a side. Corpus runs need no
ask (standing OK, 2026-10-31).
