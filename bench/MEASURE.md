# Scale measurement harness

`bench/measure.sh` runs perl-lsp over the real-project corpora and writes one
JSONL line per fact. `bench/load.sql` loads every run into DuckDB;
`bench/report.sql` slices it.

```
cargo build --release --features cpp        # features are recorded per run
bench/measure.sh                            # all corpora, 3 reps, cold+warm
bench/measure.sh --reps 1 FHEM              # one corpus, quickly
duckdb bench/measurements.duckdb < bench/load.sql
duckdb bench/measurements.duckdb < bench/report.sql
```

Needs `jq`, `/usr/bin/time`, and the corpora (`corpus/bootstrap.sh`). DuckDB
is only needed to read the results — collection writes plain JSONL, so a box
that cannot install DuckDB can still collect and ship the files.

## What is recorded

Per corpus × rep × phase (cold/warm): `--check` wall, peak RSS, CPU
utilization, `modules.db` size, and **every** file's parse and build time.
Then one more cold+warm pass per corpus, `rep` 0, with the ghost gate on:
**every** ghost counter, the per-file check lanes and the heap composition.
Nothing is aggregated or rounded at collection.

**Timed reps never run with `PERL_LSP_GHOST_STATS`.** The gate costs ~25%
wall on a witness-heavy build (`util/ghost_stats.rs::BuildScope`), so a KPI
taken with it on is not the number a user sees. The counter pass's own wall,
RSS, CPU, store size and outcome land as `instrumented_timing`,
`instrumented_rss`, ... — readable, never a KPI sample. `--no-instrumented`
skips the pass.

The `run` line carries provenance once: run id, timestamp, git SHA, **dirty
flag**, **build features**, host, kernel, nproc, MemTotal, load average.
Load average is also recorded per measurement, because a corpus that ran while
the box was busy is not comparable to one that did not.

## Three rules the schema enforces

**Every repetition is a row.** There is no way to emit "the number" — reps are
stored individually and reports aggregate. A one-run baseline once produced a
phantom +400 ms regression that survived a day; `n` and `spread` ride every
aggregate, and `n<3` prints `PROVISIONAL`.

**Nothing derived is stored.** Attempts and completions are separate rows, ns
and its count are separate rows. A stored ratio is how an attempts-vs-
completions mixup became a reported finding once already — let the report
divide, where the denominator is visible.

**Counters lead, wall lags.** Every quadratic found in the scaling sprint was
visible as a counter before anyone attributed the wall. Wall tells you
something is wrong; counters tell you where.

## KPIs and baselines

`bench/baselines.jsonl` is the checked-in KPI record: one row per
(date, sha, host, corpus, phase, metric) with median, n, and spread. Seeded
DELIBERATELY by `bench/seed-baselines.py` after a sweep you trust — review
the diff, commit it; never auto-appended. `bench/baseline-check.sql` joins
the latest run against it and flags only moves that clear both sides'
measured noise.

The KPIs are the promises, and they live in the seeder in one place:
batch `--check` wall + peak RSS (cold and warm — the warm/cold ratio is
derived in reports, never stored), the per-file build p50/p99/max (the
leading indicator that catches the next 20-second file), and the editor
surface from `lsp_bench.py --jsonl` (startup-to-ready, per-verb latency,
diagnostics push, server RSS). Counters and per-file lanes are DIAGNOSTICS,
never baselined — they attribute a KPI move and legitimately shift with
every refactor; baselining them means chasing noise.

Editor baselines are taken on a QUIET box only. A latency baseline recorded
while a sweep thrashes the machine is the loadavg trap in its purest form.

## Per-file check lanes

`--check` cost is attributed per FILE, not just per phase: every `ScopedNs`
region that runs while a file is current lands in the ghost JSON under
`file_ns` as `{incl_ns, excl_ns, n}`, and the harness ingests both as
`check_incl` / `check_excl` rows with a `tag` column. Exclusive time is
computed on a per-thread LIFO stack (the same shape rustc's self-profiler
uses); it is exact because the sweep's rayon fork sits OUTSIDE the per-file
region — never instrument a region that contains a fork, its exclusive time
is undefined, not approximate.

Both inclusive and exclusive ride every row because only one direction is
derivable: exclusive can be summed up into inclusive's children, but
inclusive can never be reconstructed from exclusive alone. A large
`diag.0_enriched_snapshot` SELF time is the signal that something inside it
is uninstrumented — that is how two untimed lints were found on day one.

`file_counts` is the allowlisted per-file counter lane (`file_count` rows):
which enrichment arm served the file (`check.arm_enriched` vs
`check.arm_whole_fallback` — different work, so a dimension, not noise),
diagnostic yield per code (`yield.<code>`), and `session.budget_exhausted` —
the one that names WHICH files run near the budget cliff. It is a separate
entry point from the global counters on purpose: attributing hot counters
per-file would put a map probe on paths that fire millions of times.

## Heap composition

`PERL_LSP_HEAP_JSON[_DIR]` dumps per-file heap-bucket estimates (the
`HeapBreakdown` lanes: refs, symbols, witness vec/index, includes, rebuilt
indices, …) as JSON — the Perl `--check` sweep writes it right after
sweeping, BEFORE eviction, so it approximates peak composition rather than
exit residue; the pack indexer writes it where its human dump fires.
measure.sh ingests one `heap_bytes` row per (file, bucket) plus
`heap_total_bytes` aggregates. This is the attribution lane for memory: RSS
says a corpus costs 9 GB, the heap rows say whose refs. The estimate is a
documented undercount (flat capacities plus the dominant string buckets);
trend and composition are its job, absolute totals are `/usr/bin/time`'s.

## Gold and e2e

Multi-process harnesses set the DIR variants — `PERL_LSP_GHOST_JSON_DIR` /
`PERL_LSP_TIMINGS_JSON_DIR` — and each process writes
`<prefix>-<pid>-<nanos>.json`; a single-path sink under N processes is
last-write-wins. Gold's `--batch` returns normally so sinks fire at
end-of-run; the server flushes on LSP shutdown and on SIGTERM. Every CLI
error path flushes too: `cli::exit_with` is the one sanctioned exit
(`layering_tests::cli_exits_flush_instrumentation` pins it), because
`process::exit` skips destructors and a bare call discards the run.

## Adding a metric

Nothing to migrate — rows are `{kind, name, value, unit}`. A new ghost counter
appears in the data the run after it appears in the code. That is why the
schema is tall: the counter set grows constantly, and a wide table would need
a migration each time.

## Traps

**Cold means cold.** Each rep gets a fresh throwaway `XDG_CACHE_HOME`;
otherwise rep 2 measures rep 1's cache and calls itself cold.

**Build the binary with the features you mean.** A default build serves Perl
only, and its numbers are not comparable to a `--features cpp` build's. The
run line records what `--languages` reported, so a mismatch is visible rather
than silent.

**Never compare an armed wall to a bare one.** Measured 2026-08-30 on gold
(same HEAD, single runs): bare 30.3s; armed with the per-thread staged lanes
28.7s — within noise. The first cut of the file lane took a String allocation
and a global lock on EVERY ScopedNs drop and cost 34.8s (+15%), and that
cost sat inside parents' EXCLUSIVE times: the instrument distorting exactly
the number it exists to produce. The lane stages per-thread now and flushes
once per file transition. If armed overhead ever reappears, decompose it the
same way — a bare control plus a pre-change binary on the same HEAD — before
optimizing anything.

**Never write inside the measured region.** The sinks accumulate in memory and
serialize at exit. Emitting per-file lines to a stream during the run once
cost 3.2M lines and 43 minutes, measuring something that no longer resembled
the thing under test.

**A number without a date rots.** These rows carry a timestamp and a SHA for
exactly that reason: abseil's warm RSS sat recorded in two ADRs at 34 MB and
47 MB, both ~2x low, for seven weeks.
