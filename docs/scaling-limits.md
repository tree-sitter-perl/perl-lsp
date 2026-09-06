# Scaling limits: what we know, measured

Two shapes cost far more than their size suggests. Both are documented rather
than fixed, because in each case the fix is large and the population affected is
small and identifiable.

Everything here was measured on 28 large open-source Perl codebases plus two
private applications (`corpus/README.md` reconstructs the public set).

## The healthy baseline, so the outliers mean something

Across 28 real Perl codebases, cold `--check`:

- conclusion-cache hit rate **96–99.9%**, every corpus
- provider-fetch **attempts** 8–206 per file for 25 of 28
- 4–36 ms per file
- memory flat and bounded

That band is what normal looks like. Two things fall outside it.

## 1. `package main` monocultures (FHEM shape) — PATHOLOGICAL, not fixed

[FHEM](https://github.com/fhem/fhem-mirror) — 991k LOC, 973 Perl files, zero
vendored — does not complete `--check` on a machine with 31 GB of RAM.

**Scope, established by using it rather than by measuring it: this is a
BATCH-VERB limit, not a server limit.** FHEM opens fine in an editor. Startup is
slow, and after that it behaves. The difference is structural rather than
lucky — `--check` runs the enriched-diagnostic sweep across *every* workspace
file, while the server indexes nothing at `initialize` and enriches only the
documents you actually open. The crest below is 20 rayon workers each holding a
per-file working set; an editor session has no such sweep to run.

So: `--check`, `--heatmap` and the other whole-workspace verbs are the affected
surface. Interactive editing of a `main`-monoculture codebase is not, and this
file said otherwise until someone opened FHEM in nvim and found it usable.

**The shape:** 503 of its 614 `.pm` files declare `package main` explicitly, and
31 more declare nothing. **534 of 614 files (87%) provide one package name.**
This is not a mistake in FHEM: `fhem.pl` `do`-loads all of them into a single
interpreter, and 361 of them call `readingsSingleUpdate` from `fhem.pl`'s main.
They genuinely share one stash. **The 534-wide provider relation is correct.**

**The cost:** every `main` lookup consults a 534-member candidate set. `main` is
27% of package lookups and **94% of provider fetches**. Per-worker sweep working
sets reach 633 MB, and ~20 rayon workers multiply that into the crest.

**What does not explain it, each ruled out by a control rather than an argument:**

| suspect | ruled out by |
|---|---|
| walk residency | index-only run holds 0.98 GB — *leaner than release's entire check* |
| overlay clones | 0.76 GB cumulative; the overlay cache is capped at 133 MB |
| the sweep path memo | disabling it moves peak 0.7% — and costs 55% wall, it is load-bearing |
| allocator arena count | `MALLOC_ARENA_MAX=2` gives ≤5%, inside run variance |
| the diagnostics channel | 300 KB high-water against 10 GB |
| per-file memo byte cap | engaged correctly (19,929 evictions) and made peak **worse** (+15%) and wall **worse** (+51%) — reverted |
| source-byte admission gate | never engages: needs 3+ giants concurrently in flight to reach its budget |

**What does explain it, in two parts:**

- **glibc brk retention** owns the *sustained* figure. 1.6 MB analyses churn
  decode-and-drop and freed chunks never return.
  `MALLOC_MMAP_THRESHOLD_=65536` cuts sustained RSS **53%** (10.02 → 4.68 GB)
  and turns a monotone climb into a sawtooth that actually returns memory. Peak
  falls only 16%, so this helps a long-lived server and barely helps a one-shot
  CLI.
- **Per-worker in-flight sets** own the *crest*. `RAYON_NUM_THREADS=4` cuts peak
  **67%** (9.83 → 3.20 GB) for **4.9%** wall — the sweep is not CPU-bound at 20
  workers, so 16 of 20 buy almost nothing in time while costing 6.6 GB.

**Workaround for this shape today:**

```
RAYON_NUM_THREADS=4 MALLOC_MMAP_THRESHOLD_=65536 perl-lsp --check <root>
```

**Why it is not fixed.** The relation is semantically correct, so narrowing it
would be wrong. The remaining honest fix is deduplicating provider decoding
across a sweep (~13,456 rehydrates for ~500 distinct providers is ~27x redundant
work held in up to 20 overlapping per-file memos). That is a real change to the
sweep's memory model and it has not been built.

**Who this affects:** codebases where one package name has hundreds of
providers. That is `do`-loaded plugin frameworks. It is not ordinary
application code, and it is not `.t`-heavy repositories — test files declaring
`main` implicitly do **not** reproduce it.

## 2. Vendored dependency piles — MISLEADING, not slow

A tree of many unrelated distributions (a CPAN mirror, a large `thirdparty/`)
has no locality: each file consults a different module set, so the conclusion
cache thrashes. A 551-distribution pile took **137x more cache misses** than
real application code and produced a "cliff" — 65x slowdown for 1.84x the files
— that **no real codebase reproduces**.

If you are benchmarking, this is the trap: unresolvable imports cost the
resolver nothing to answer, so a dependency pile both thrashes the cache *and*
understates resolution work. Measure applications, and install their
dependencies onto `@INC` rather than into the workspace (`corpus/README.md`).

## Reporting a scaling problem

Useful report: `PERL_LSP_GHOST_STATS=/tmp/g.txt perl-lsp --check <root>` plus
`/usr/bin/time -v`, and the count of files declaring the same package name
(`grep -rhc '^package ' --include='*.pm' . | sort | uniq -c | sort -rn | head`).
Those three tell us within minutes which of the shapes above you have, or that
you have a new one.

## 3. Dependencies are not free — every historical number here understates load

Measured on the eight-repo corpus, cold `--check`, same binary both arms, deps
on `@INC` via `PERL5LIB` (never in the workspace — see `corpus/README.md`):

| repo | wall nodeps → deps | Δ | RSS nodeps → deps |
|---|---|---:|---|
| BMO | 7.82 → 13.76 s | **+76%** | 0.48 → 0.62 GB |
| openfoodfacts | 9.51 → 12.76 s | +34% | 0.37 → 0.47 GB |
| WeBWorK | 7.40 → 9.89 s | +34% | 0.30 → 0.36 GB |
| FHEM | 41.06 → 54.23 s | +32% | both killed |
| Evergreen | 13.37 → 17.18 s | +29% | 0.53 → 0.65 GB |
| Foswiki | 10.80 → 12.83 s | +19% | 0.48 → 0.53 GB |
| Webmin | 9.26 → 8.76 s | −5% | flat |
| Znuny | 85.30 → 79.07 s | −7% | flat |

**+19–76% wall, +2–29% RSS** where it bites. The two flat rows are controls
rather than noise: Webmin uses path-based `require` and barely touches CPAN, and
Znuny vendors 723 `cpan-lib` modules *inside* its workspace so its imports
already resolved.

Wall and fetch counts do **not** track each other — BMO gains 76% wall on 16%
more fetches, openfoodfacts 34% on 168% more. The expense is what a *successful*
resolution pulls in, not the lookup count.

**Consequence:** any benchmark run without dependencies installed understates
real resolution load by roughly a third. State which shape a number was measured
on.

## 4. Memory and fan-out are independent axes

`Znuny` is the correction to an earlier framing here. It has the **lowest**
provider fan-out of 28 corpora (8 attempts/file) and is entirely healthy on that
axis — and it still peaks at **8.15 GB** on 3,078 files, second only to FHEM.
Low fan-out does not imply low memory; file count does not predict either. A
corpus needs both measured.

## 5. `--heatmap` is a batch verb, not an interactive one

| corpus | `--check` | `--heatmap` | ratio | max fan-in |
|---|---:|---:|---:|---:|
| WeBWorK (225 files) | 3.80 s | 5.00 s | 1.3x | 81 |
| Webmin (1,333) | 4.76 s | 31.07 s | 6.5x | 199 |
| BMO (739) | 5.39 s | 91.69 s | **17x** | 340 |

Cost tracks **fan-in**, not file count — BMO is smaller than Webmin and costs
3x more. That follows from what the verb does: it mints the `references()`
projection at every declaration, so the work is declarations x their fan-in.

Those numbers are the pre-2026-09-06 state, kept because the SHAPE of the
cost (declarations × fan-in) is still what the verb does. **It ran on one
core** (104-105% CPU) and, worse, every candidate-file visit re-scanned the
file's whole ref vec twice and every LRU miss decoded under one process-wide
mutex. Re-measured 2026-09-06 after the gather fanned out per declaration
and the walk stopped doing that (loaded box — a quiet-box re-take is owed
before any of this is seeded into `bench/baselines.jsonl`; three runs each,
byte-identical output to the serial verb in every configuration):

| corpus | before (1 thr) | after, 1 thr | after, 4 thr | after, 20 thr | peak RSS before → 4 → 20 |
|---|---:|---:|---:|---:|---|
| WeBWorK (225) | 3.1 s | 2.3 s | 1.1 s | 1.4 s | 288 → 297 → 325 MB |
| Webmin (1,333) | 29–42 s | 12.5 s | 4.7 s | 2.6 s | 831 → 825 → 870 MB |
| BMO (739) | 111 s | 27 s | 8.9 s | 8.1 s | 655 → 649 → 715 MB |
| BMO `--include-deps` | 158 s | — | — | 9.0 s | 679 → — → 713 MB |
| abseil (C++, 875) | 527 s | — | — | 118–132 s | 1.23 → — → 1.62 GB |

Where BMO's 111 s went, by ghost accumulator: 59 s in the candidate-file
view (LRU lookups, a 12% miss rate decoding under the reader mutex, and a
linear pre-scan per visit), 27 s in the matcher's linear ref scan, 7 s in
SQL retrieval (a `SELECT path FROM files` PER WALK), plus ~58k whole-copy
rehydrates from a pack-only member probe running once per candidate file.
The five levers, in order of landing: the match-key ref index
(`RefTable::by_key`), a per-walk memo for target-only facts, the sweep
retrieval memo, the reader checkout pool (the parallel ceiling — 188% CPU
with the lock, 800–1500% without), and the corpus-sized rehydration LRU
(`cache_policy` — 35 s → 9 s at 20 threads on its own, AND lower RSS,
because the stock cap was cycling a 274 MB working set). `docs/adr/heatmap.md`
§Cost has the knobs; `docs/adr/relational-ref-index.md` the seams.

**Pack languages ride the same levers** — the key index, the walk memo,
the retrieval memo, the reader pool and the corpus-sized LRU are all
language-blind (abseil's cpp sub-index sized itself to 632 MiB; every
rehydrated copy of any language rebuilds the key index in
`after_deserialize`) — but the pack walk keeps pack-only per-visit costs
this work did not touch: the textual-inclusion sweep over every cached
file per walk, the per-file `ScopedLookup` and closure gate, and the
scoped member probe (the walk memo deliberately bypasses a closure-scoped
lookup, whose answer is per-asker). That is why abseil gains 4× where
BMO gains 14×; those are Epic 14/15 items, not heatmap ones.

**Residual, measured:** the knee is 8 threads on BMO (6.3 s at 711% CPU);
20 threads reach the same wall at 4× the CPU, and `MALLOC_ARENA_MAX=2`
makes it much WORSE (13.6 s), so the contention is allocation churn inside
the walk — per-candidate `PathBuf` clones and per-walk candidate-set copies
— not a lock. That is Epic 15's references-walk work, and it applies to the
LSP verb as much as here. Attribution is by ghost accumulator only: this
box's `perf_event_paranoid=4` blocks `perf` and `samply`.

Memory stays mild: the parallel gather adds ~10% peak RSS at 20 workers
over serial, and the corpus-sized LRU LOWERED it relative to the thrashing
default.

## 6. Single huge files: one accidental O(bindings x bag) pass, now fixed

Found by opening FHEM's biggest file in an editor and waiting 30 seconds for
semantic tokens. Root-caused and fixed by `[05]`; this section records the
corrected story, because the first version of it got two things wrong.

### The symptom

`76_SolarForecast.pm`, 46,522 lines / 2.6 MB, opened via `didOpen`:

```
[PHASE] parse       349.01 ms
[PHASE] build()   33179.55 ms     <- 33.2 s
```

Inside `build()`, `fold_to_fixed_point` was 30.7 s — 92% — while the CST walk
was 0.74 s. The fold converged in **3 iterations**, so it was never spinning.

### The cause: a full index rebuild per binding

`propagate_call_bindings_to_constraints` called `remove_attachment_source_at`
once per binding, and that helper does a **full-bag retain plus a full witness
index rebuild** (cloning every String-bearing attachment) on each call.

```
4,857 calls -> 3,246 rebuilds -> 185,757,638 cumulative index re-insertions
                                = 20.7 s of the 30 s
```

**Fix: batch the removals** — one retain and one rebuild per pass
(`WitnessBag::remove_attachment_sources_at`).

```
fold::call_binding  21.6 s -> 16.5 ms
bag length          57,228 -> 57,228   (unchanged)
fold iterations          3 -> 3        (unchanged)
```

Identical bag and iteration count with the wall dropping is what makes this a
fix rather than a tuning: the same fixed point, reached without the quadratic.

Ladder through `didOpen`, first build in a fresh server, median of 3:

| lines | pre-fix | + call_binding fix | + chain_pre fix |
|---:|---:|---:|---:|
| 3,268 | 188 ms | 424 ms | 627 ms |
| 5,607 | 276 ms | 599 ms | 639 ms |
| 20,669 | 2,374 ms | 1,424 ms | 2,290 ms |
| **46,522** | **33,180 ms** | **10,698 ms** | **5,364 ms** |

**6.2x at the top.** The small rows are NOT a regression — see below, and the
reason two people chased one anyway is worth more than the table.

### The first-build constant — found, attributed, and removed

Every row above is a fresh server, and the first `build()` in a process used to
carry ~600 ms that was **not the build**. On small files it dominated the
quantity being compared, which produced an apparent 424 → 627 ms "regression"
that two people chased.

`[05]` bisected it: first-build on a 1,760-line file was ~700 ms at all three
commits (721/696, 688/685, 696/701 — indistinguishable), and two `didOpen`s in
one process gave **712 ms then 121 ms**.

The first attribution — lazy `rhai` plugin compilation — was **wrong, and `[05]`
retracted it themselves**. The phases now print, and they name the real cost:

```
registry::engine          0.65 ms
registry::rhai_compile    7.45 ms     <- rhai was never the problem
registry::queries       119.02 ms     <- 14 tree-sitter Query::new + the flow query,
                                         ~597 ms when run serially
```

Fixed three ways (`0e33a8d4`): the registry cell is keyed by resolved `.rhai`
paths so a pre-root warm is safe, the warm runs in `main()` before the
handshake, and the query compiles run rayon-parallel (597 → ~140 ms). A latent
bug went with it — the old `OnceLock` would have baked a wrong pre-root plugin
set permanently.

Result, 3,268 lines: **627 → 223 ms** (223/229/223, tight). The 46k file is
unchanged at 5,335 ms, which is the correct signature for removing a fixed cost.

**Protocol, now enforced by the code rather than by discipline:** the registry
phases print, so a ladder cannot silently eat this constant again. Still prefer
the second build in a process when quoting per-build cost.

### Two corrections to the first version of this section

**The superlinearity was accidental, not intrinsic.** This section previously
said the fold is "superlinear in bag size — which is why the exponent climbs."
Wrong. The exponent climbed because one pass was O(bindings x bag). The registry
chases the first analysis blamed are cheap: `fold::seed` 30 ms and
`fold::snapshot` 27 ms for **all** 1.8M hops. A "16.7 µs per hop" figure was
derived by dividing total fold time by a hop count that was not the cost — a
ratio of two unrelated quantities, and it read as a smoking gun.

**Consequence for design: chunking the solver would have masked a bug.** The bag
was not too big; one pass was rebuilding an index 3,246 times.

### The 1 MB cap — why the same file measures 0.39 s or 29 s

The first version claimed `@INC` was required to reproduce, citing an isolated
one-file `--check` that took 0.39 s against 33 s in a real workspace. That was
wrong twice over, and the real reason is a one-line filter:

```
index_perl.rs:52,99   m.len() < 1_000_000
76_SolarForecast.pm = 2,652,209 bytes
```

**The workspace walk skips files over 1 MB, so `--check` never built it** —
there is no `[PHASE] build()` line in that run at all. `didOpen` has no such
cap. The same file is skipped by batch verbs and always built by the editor,
which is the entire 0.39 s / 29 s gap. `build()` has no module-index access, so
`@INC` cannot affect it either way.

**A consequence worth its own line: files over 1 MB silently receive no
`--check` diagnostics.** FHEM has five (contrib ships a second `76_SolarForecast.pm`). "No diagnostics" reads exactly like "no
problems found," and nothing in the output distinguishes them.

### Residual

After the fix, `fold::chain_pre` (chain typing PreFold) is 5.3 s on the 46k
file — **96% of what remains** — and is the next target if 7.8 s is still too
slow.

### The interactive symptom is separate, and separately fixable

While the build runs, verbs answer **fast and return `null`** rather than
blocking: on the 46k file hover 0.82 s, definition 1.22 s, completion 1.62 s,
semanticTokens 2.02 s, every one empty. The client renders nothing and **never
retries**, so it looks like a dead server until the build finishes and the
server sends `workspace/semanticTokens/refresh` — then everything appears at
once. Confirmed in nvim's log: 22 s of answered `documentHighlight` and nothing
else, then `refresh`, then the client's first `semanticTokens/full`.

Answering "not ready" in a way clients retry on (`ContentModified`) would mask
the remaining delay regardless of how fast the fold gets. Not done.

### Instrumentation note

Per-build counters were previously unobtainable: the ghost-stats sink is written
by an activity-driven re-emit, so the default interval never fires on a short
build and a short interval **re-adds per emission** (a 3,268-line file reported
1,117,581 witnesses and 1,995 iterations against the real 57,228 and 3 — ~665x).
`[05]` added a thread-local per-build scope emitting one `[build-scope]` block
at build end, delta'd per build. That is what made this root-cause possible, and
it is immune to both failure modes.
