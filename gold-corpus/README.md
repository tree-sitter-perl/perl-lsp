# perl-lsp gold corpus

A reproducible regression net of verified LSP-capability rows, run against a **cpm-installed, snapshot-pinned substrate**. The substrate is a hermetic, version-locked module tree built from [`gold-corpus/cpanfile`](cpanfile) + [`gold-corpus/cpanfile.snapshot`](cpanfile.snapshot) into `gold-corpus/local/lib/perl5`, so every query resolves against the **same pinned versions** in dev and CI. The LSP is pointed at that tree as both workspace root and `PERL5LIB` (its arch dir, e.g. `x86_64-linux/`, included), so resolution is deterministic and editor-free.

Rebuild the substrate:

```sh
cd gold-corpus && carton install --deployment          # exact, from snapshot
# or, faster:    cpm install -L local --resolver snapshot
```

Positions are **0-based on input, 1-based on output**.

## Harness

`gold-corpus/run.pl` is an **exact-assertion** regression runner. **`fixtures/*.json` is the source of truth** (machine-checkable rows); each per-capability **`*.md` is the human view** of the same rows. The runner needs the substrate (default `gold-corpus/local/lib/perl5`, override `CORPUS=`) and a release build (override `BIN=`); it is **not** a cargo/CI test (it depends on the locally-installed substrate).

```sh
gold-corpus/run.pl                 # run the suite, cold + warm (non-zero on FAIL/CRASH/XPASS/warm-FAIL)
gold-corpus/run.pl --no-warm       # cold pass only (faster iteration)
gold-corpus/run.pl definition hover
gold-corpus/run.pl --list          # capabilities + gold/xfail/prov counts
gold-corpus/run.pl --emit definition LWP/RobotUA.pm 64 23   # author against this
```

Each fixture row asserts the query's **normalized** output against two substring lists — `expect.all` (every string must appear) and `expect.none` (none may) — with a `status`:

- **gold** — the assertion must hold; otherwise **FAIL** (a real regression).
- **xfail** — a known gap: the (correct) assertion must currently *not* hold. If it starts holding → **XPASS**, a soft failure telling you to promote the row to gold. Known gaps can't silently rot, and a fix is detected automatically.
- **provisional** — run and reported, never fails the suite.

The suite runs **twice**: once cold, then again against the cache the cold pass wrote. A row that passes cold and fails warm is **warm-FAIL** — a fact that survives the first analysis but not rehydration. Cold is a user's first open; warm is every session after, so a cold-only run cannot see this class at all. A known warm gap is declared per row with `"warm": "xfail"` (reported as `warm-xfail`; it becomes **warm-XPASS** when fixed, exactly like `xfail` → `XPASS`). `--no-warm` skips the second pass, and the summary says so rather than printing zeros it did not earn.

Both passes run under a **private, throwaway `XDG_CACHE_HOME`**, so the cold pass is cold by construction and the run never touches the cache of any project on the machine. This is deliberately *not* done by clearing the real cache: bare `perl-lsp --clear-cache` wipes every project's cache dir, and clearing only *some* roots is worse than clearing none — an uncleared root makes the cold pass silently warm and nothing reports which roots were reused. The header line names the cache dir and the number of roots (26, not the 17 fixture directories — the substrate and per-row roots count too), so "every root started cold" is checkable rather than assumed.

**Gold owns the rendered assertion; `tests/` owns the structure.** A row here
pins the CLI's normalized OUTPUT — the exact hover markdown, the exact
reference list — and `--emit` re-authors it when the rendering moves, which is
what makes an exact-output net maintainable. A cargo test that shells the CLI
and matches a rendered line with `starts_with` has no such re-authoring path:
it pins the rendering with none of gold's machinery, so a display change
breaks a test about resolution. A test in `tests/` asserts the structured
answer (the JSON verbs' decoded fields, the analysis the binary exposes); if
what it wants is the rendered line, the row belongs here.

A process abort (the scanner-overflow class) is always a hard **CRASH** fail. Output is normalized before matching — absolute paths reduced to basenames; JSON outputs (references / workspace-symbol / outline / rename / diagnostics) decoded and re-encoded canonically (sorted keys, compact) so one substring ties file+line+kind. The **same** `normalize()` backs `--emit`, so fixtures authored against `--emit` output match the runner by construction.

## Capabilities

| capability | file | gold | xfail | provisional | dropped |
|------------|------|------|-------|-------------|---------|
| outline (documentSymbol) | [outline.md](outline.md) | 10 | 0 | 2 | 1 |
| definition (goto-def) | [definition.md](definition.md) | 18 | 2 | 0 | 0 |
| references | [references.md](references.md) | 20 | 1 | 0 | 1 |
| hover | [hover.md](hover.md) | 19 | 1 | 4 | 1 |
| type-at | [type-at.md](type-at.md) | 12 | 2 | 2 | 5 |
| rename | [rename.md](rename.md) | 8 | 0 | 1 | 2 |
| workspace-symbol | [workspace-symbol.md](workspace-symbol.md) | 14 | 0 | 0 | 1 |
| diagnostics | [diagnostics.md](diagnostics.md) | 3 | 5 | 0 | 9 |
| completion | [completion.md](completion.md) | 9 | 4 | 0 | 3 |
| signature-help | [signature-help.md](signature-help.md) | 11 | 2 | 3 | 0 |
| semantic-tokens | [semantic-tokens.md](semantic-tokens.md) | 13 | 0 | 1 | 2 |
| document-highlight | [document-highlight.md](document-highlight.md) | 9 | 0 | 1 | 2 |
| linked-editing | [linked-editing.md](linked-editing.md) | 12 | 0 | 2 | 0 |
| re-export (definition) | [fixtures/reexport.json](fixtures/reexport.json) | 3 | 0 | 0 | 0 |
| **total** | | **160** | **17** | **14** | **27** |

> Counts are the source-of-truth `fixtures/*.json` row counts (run `gold-corpus/run.pl --list` to regenerate). The per-capability `*.md` human views may lag the JSON after a mining sweep; the JSON is authoritative.

The substrate is installed lib content only — it contains **no test files (`t/*.t`), `bin/`, or examples**. Rows whose original cursor lived in a test file, or whose module is not in the snapshot (e.g. JSON::PP, Time::HiRes), are listed in each capability's "Dropped" section. Rows that leaned on test-file call sites were re-authored to the surviving in-lib locations.

**Cost report.** Every suite run ends with a per-capability timing table
(wall-time per query, attributed by row id), per-root startup latency
(first-response time: workspace index + cache warm), child CPU
(user/sys), and peak RSS (`/proc` VmHWM, Linux-only) — so feature work
sees what it costs as it lands. Set `METRICS_OUT=<file>` to also write
the numbers as JSON for run-over-run comparison.

**Nested-hashkey fixture.** Rows under `fixtures/nested-*.json` run
against the committed workspace at `nested-fixture/` (per-row `root`,
same mechanism as re-export below) and exercise the structural typing
tiers: the mixed drill `$obj->{users}->[0]->{name}` (type-at), imported
array-literal tuples (`Sequence<…>` hover), drill hops through an
imported literal (`my $db = $config->{db}` hover — the `Projected`
edge payload), `$row->{name}` → column def (definition + references,
cross-file), and the closed-shape unknown-key hint (diagnostics: the
local-literal typo is gold; the cross-file-typed typo is xfail — batch
diagnostics have no enrichment parity, see KNOWN-GAPS.md; the
mutation-extension rows pin that an unconditional write joins the
shape — the written key reads back typed, the typo beside it still
hints; the literal-hash rows pin the `%plain` spelling of both).

**Deep-CST fixture.** `fixtures/deepcst.json` + `fixtures/deepcst-diagnostics.json`
run against `deepcst-fixture/`: two copies of a 12,003-level synthetic CST
(the XML-shipped-as-`.pm` family — a recursive builder walk on these
stack-overflows a 2 MB rayon worker, a fatal abort `catch_unwind` cannot
catch) plus an ordinary goto-def target. The definition row is the crash
canary — the abort kills the whole `--batch`, and the harness hard-fails a
CRASH; the diagnostics row pins the degradation contract (`cst-too-deep`
warning per skipped file, rest of the workspace unaffected). Two copies so
at least one lands on a worker stack regardless of rayon scheduling.
**Authoring trap, and it cuts both ways:** a warm module cache serves the
stored blob and never re-walks the tree, so it masks whatever the walk would
have done. Before the walk was iterative that hid the *crash*; after, it hid
the *fix* — a stale blob replayed a `cst-too-deep` diagnostic naming the old
`500` limit while a freshly-walked file in the same run named `100000`, two
limits live at once from a source with only one gate. Either way the symptom
is a result that does not match the code you are looking at. Run
`perl-lsp --clear-cache gold-corpus/deepcst-fixture` before trusting any
check here, base-failure or fix-verification.

**Re-export fixture.** Three rows (`fixtures/reexport.json`, capability `definition` — folded under `definition` in `--list`) run against a small **committed, self-contained workspace** at `reexport-fixture/` instead of the snapshot substrate, via a per-row `root`. They flex the transitive export-surface feature: `goto-def` from a consumer of a re-exporter resolves to the *original* sub through both re-export forms — static splice (`our @EXPORT = (@RexBase::EXPORT)`) and loop-push (`push @EXPORT, @{"${m}::EXPORT"}`). The harness groups rows by `root` and runs one `--batch` per root.

## Already in corpus

Two earlier repo-keyed matrices cover definition + type-inference rows organized by repo and predate this capability-keyed corpus:

- [`matrix-A-oo-framework.md`](matrix-A-oo-framework.md) — Moo / Plack / Catalyst / Minion
- [`matrix-B-exporter-classic.md`](matrix-B-exporter-classic.md) — Exporter-Tiny / Sub-Exporter / URI / DateTime / Log-Log4perl

## Editor-only capabilities (not CLI-queryable — covered by e2e)

The following capabilities have **no CLI query mode** and are therefore **not** covered by this corpus. They are exercised by `e2e/run.sh` (drives a real nvim LSP client). This corpus does not pretend to cover them:

- inlayHint
- foldingRange
- selectionRange
- codeAction / auto-import
- formatting + rangeFormatting (perltidy)

> **Note:** completion, signatureHelp, semanticTokens/full, documentHighlight, and linkedEditingRange are CLI-queryable and covered by this corpus (see the capability files above). They are no longer editor-only.

## Known failing (xfail rows)

Confirmed gaps captured at xfail — the correct assertion does not currently hold, so the runner pins the gap (XPASS the day it's fixed). Full write-up (root cause + fix sketch + difficulty per gap): [`KNOWN-GAPS.md`](KNOWN-GAPS.md).

- **def-16-codegen-type-function** (definition) — `Types::Standard::Any` has no literal `sub`; Type::Library codegen mints it at runtime, so goto-def degrades to the package decl (`Standard.pm:1:1`) instead of the `name => "Any"` declaration at `Standard.pm:215`.
- **diag-08** (diagnostics) — XS-bootstrapped `bootstrap` at `SSLeay.pm:1023` flagged unresolved-function; should be suppressed (XS-installed sub).
- **diag-09 / diag-10** (diagnostics) — codegen'd Log4perl accessors `is_warn`/`warn` (`Logger.pm:879/883`) flagged unresolved-method; the typeglob-codegen install isn't recognized as defining them.
- **completion-datetime-hashkey** (completion) — `$self->{` offers only 2 of 13 mutated keys; keys assigned via `$self->{k}=...` in `_new` aren't harvested.
- **completion-typetiny-imported-blessed** (completion) — imported `blessed` (`use Scalar::Util qw(blessed)`) absent from bareword-statement completion; only local subs offered.
- **sig-uri-check-path-function-noinvocant** (signature-help) — `$path` wrongly elided as invocant on a PLAIN function call `_check_path($rest, $$self)`; signature shows only `($pre)`.
- **ti-12** (type-at) — **ts-parser-perl 1.1.0 regression.** `my $self = shift->SUPER::new` no longer types `$self` as the enclosing class (`Minion`). Returned `Minion` on 1.0.3, `None` on 1.1.0; the isolated `shift->SUPER::new` CST is byte-identical across versions, so the break is in a subtler cross-file shape change (under investigation by the main agent). Pinned at xfail so it XPASSes the day it's fixed.
