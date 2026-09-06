# Storage/residency residuals — deferred work queue

Deferred WORK items extracted from closed hardening rounds — each is a
known gap with a design sketch and a trigger, not a decision awaiting
ratification (those live in `docs/open-forks.md`; the closed rounds these
came from are in `docs/forks-resolved.md`). An item leaves this doc by
landing (delete it, record the landing in the owning ADR or ledger entry)
or by being explicitly declined.

## Residency / RAM

- **`Witness` is 240 bytes and the bag is half an analysis.** Measured
  2026-09-01 on the gold substrate, `--check` with `PERL_LSP_NO_EVICT=1`,
  3,364 files, 511.7 MB of whole analyses: `witness_vec` **189.97 MB
  (37.1%)** and `witness_index` **77.12 MB (15.1%)** — the bag is
  **52.2%** of a whole analysis, the largest thing in it (refs are 27.2%,
  symbols 7.5%). `size_of::<Witness>()` is **240 B**: attachment 72,
  payload 104, source 32, span 32 — of which **up to 72 B are inline
  `String` structs (30%), paid even when the string is empty**, carrying
  ~30 distinct short tags repeated across every witness in the corpus.
  **The lever is the struct, not the allocations.** The heap bytes those
  tags own are only ~5.9 MB (1.1% of the total, and undrilled by
  `heap_bytes_estimate`, which is `capacity × size_of`), and ~141
  allocations/file extrapolates to ~19.5M over a CPAN-5k cold index —
  about 0.6 s against 10.5 minutes, plus zstd eats the repetition on the
  blob side. So do NOT chase the `.into()`s. Interning the tag or making
  it `&'static str` shrinks the *element*, which is ~10–15% off the
  biggest bucket in the biggest resident structure, and lands where it
  matters: the FHEM per-worker in-flight crest (§1 of
  `scaling-limits.md`) and `PackBagCache`. Caveats before acting: one
  corpus, one run, NO_EVICT — re-measure on Koha under the `edit-bench`
  protocol, and note the tag also serializes into the cache blob, so
  `&'static str` needs a serde story.
- **Watcher re-registration never re-strips.** Whole copies pinned until
  restart; a big `git pull` is an unbounded resident delta. Design:
  persist (blob+rows) in the watcher's blocking task, then
  `register_workspace_stripping` on commit, whole-copy fallback only on
  persist failure. (The one remaining KNOWN unbounded residency residual
  alongside the deliberate CLI one-shot warm-whole profile.)
- **Rows-missing re-strip after backfill.** After a REF_ROWS_VERSION
  bump, refs+symbols stay resident for one session (self-healing at next
  restart; never trips the fully-resident wire). Re-registering post-
  backfill needs a surface-PRESERVING residency-only lane (re-projecting
  from a bag-evicted copy would corrupt the freshness record) — build it
  on `register_workspace_residency`/`register_symbols_inner` with the
  original parts, not the stripped copy.
- **@INC tier: symbols/refs reader routing.** The import-tier strip drops
  only the witness bag (`strip_import_copy`); symbols and refs stay
  resident because their readers don't yet route through rehydration for
  this tier. Extending the strip to those axes needs the same
  reader-routing discipline the workspace tier got.
- **R4 overlay in-flight dedup.** Two threads missing on one path both
  pay the enrichment deep-copy (last insert wins). Bounded waste; revisit
  if profiling shows it.
- **`gated_emissions` rides eviction unstriped.** The field is NOT an
  eviction axis (deliberate — `materialize_gated_emissions` reads it off
  the evicted resident copy to decide which files need re-materializing),
  so it stays resident on every cached copy. Sparse by construction:
  populated only for plugin-triggered files whose `ClassIsa` gate resolves
  cross-file (DBIC result classes), and materialization is CLI/batch-only.
  It is serde-carried, so if it ever grows to non-sparse volume it can be
  added to the strip axes like any other — no format change needed.

### Resolved

- **cpp gather caches — unbounded growth + no single-flight (H9-3).** RESOLVED.
  The four `cpp_reparse.rs` gather caches (macro table, pre-expanded external,
  header parse, include closure) were bare `OnceLock<Mutex<HashMap>>` with no
  cap and check-release-compute-insert races. They now share one
  `GatherCache<K,S,V>` wrapper: single-flight population (a key's first misser
  claims it; siblings expanding the same header cone block on its result via a
  condvar instead of recomputing), byte-accounted LRU eviction (never the
  just-inserted key), and cancel-safe invalidation (an `evict_gather_caches`
  during an in-flight compute drops the claimant's stale result; a waiter
  recomputes — no deadlock, the state lock is never held across a compute).
  Bounds (per cache, `PERL_LSP_GATHER_CACHE_MB` overrides all to one value; 0 =
  never retain): `header_cache` 128 MiB (session-lived, shared by header path,
  highest reuse); `macro_table_cache` 128 MiB (per-file raw merged closure);
  `pre_expanded_cache` 128 MiB (full+alias expansion on top of raw);
  `include_closure_cache` 64 MiB (per-file path lists, smallest per-entry).

## Wall clock

- **cpp references sweep cost** (edit-bench, 2026-07-14; profile first).
  abseil warm references 1.62 s for 54 result sites vs redis 0.63 s
  (~250 sites) and curl 0.11 s (155 sites). Cost tracks the
  VISIBILITY-GATE-PASSING file count, not the result count — status.h is
  included by most of abseil's tree, so most TUs pass the include-closure
  gate and get whole-view rehydrated through the LRU per query. Next:
  PERL_LSP_PHASE_TIMING profile of one warm abseil references call;
  likely fixes are candidate-row pre-narrowing for pack tiers (the Perl
  rows machinery exists; pack rows are per-language DBs) or memoizing the
  swept whole-views across one query. Measure before building.
- **Probe serialization in the `PackInvalidator` swap** (Changed case): the
  changed file's probe runs serially before the parallel consumer fan-out
  (~one header-analysis of added latency per save while actively editing
  a widely-included header whose surface DID change). Speculative
  consumer re-analysis concurrent with the probe would restore the old
  wall clock at the cost of wasted work on Unchanged — measure before
  building.
- **@INC one-shot rehydration profile.** Warm gold runs 162s vs 40s
  under NO_EVICT — per-row cold-LRU rehydration in one-shot processes,
  the accepted profile (the long-lived server amortizes it). Revisit if
  CI minutes ever matter. Options: per-process blob-decode memo, or
  NO_EVICT in the harness at the cost of blinding the eviction nets.

## Correctness suspicions (verify first)

Two unverified observations from the cpp adversarial review:

- **`modules-{lang}.db` rows for deleted files are skipped on warm but
  never purged** — a suspected unbounded-growth residual on long-lived
  pack caches. Sibling of the watcher re-registration residual above.
- **The `strings` interner is never garbage-collected.** `shred_derived_rows`
  re-shreds a file by deleting and re-inserting its `refs` and `syms` rows
  (`DELETE FROM refs/syms WHERE file_id = ?`), but never its strings; the
  only `DELETE FROM strings` in the tree is in `clear_derived_rows`, the
  hard clear. So within one cache generation the interner grows
  monotonically with every edit and re-shred — a name that no surviving row
  references still occupies its row. Measured at 556k rows / 31 MB, which is
  why this is a residual and not a bug, but it is unbounded *by
  construction*: nothing in the write path can ever shrink it. A collector
  would be a `DELETE FROM strings WHERE id NOT IN (SELECT name_id FROM refs
  UNION SELECT name_id FROM syms)` sweep, which is cheap to write and
  expensive to run, so it wants a trigger (generation rollover, or a row
  count threshold) rather than running per shred.
- **`clean_body` truncates at `//` inside string literals** — a
  `#define URL "https://x"` body would be mangled, potentially flipping
  the whole-file validate gate to alias-only. Needs a string-literal-aware
  comment strip if confirmed.

## Vocabulary / future-proofing

- **Pack provided-names vocabulary.** `SurfaceRecord.provided` is
  packages-only; a future pack-tier NAME-keyed dirty walk (cpp uses
  include-closure consumers today, so nothing reads it) would
  under-invalidate for free-function headers. If that walk ever lands,
  feed `provided` from the linkage feed, not `packages`.

## API-shape dedup (refactors, no behavior change)

- **Writer-thread harness dedup.** WsFresh and FreshEntry writers share
  the whole chunk/txn/fallback scaffold shape; a generic harness would
  make fixes land in both by construction. Moderate refactor; the
  writers' panic-arm LRU-pin fix is the drift it would have prevented.
- **Stamp-capture helper.** The stamp-before-read + re-stat-after-parse
  protocol is spelled in both fresh workers.

## The reader side of the residency ladder

`Residency` closed the WRITER side: the strip is one ladder, and
`(rows evicted, bag kept)` is no longer expressible. The reader side is still
open. `symbols_present` / `refs_present` hand back BAG-STRIPPED copies, so a
bag-backed query on one answers `None` — not an error, not a panic, just a
quietly missing type that reads as "no type known".

Every site today is a deliberate symbols-axis read and says so in a comment.
A comment is not a type, so `layering_tests::a_narrow_axis_view_is_never_asked_a_bag_question`
scans for the shape instead: a narrow view chained straight into a bag-backed
accessor, or bound and asked one later in the same scope. It is a heuristic —
scope end is approximated by dedent, because a brace matcher that desyncs on a
single string literal swallows the rest of the file and reports nonsense, and a
tripwire that cries wolf gets deleted. False negatives are acceptable; false
positives are not.

The real fix is the reader-side axis-typed view, still deferred for the reason
recorded in `docs/review-narrow-seams.md`: consumers read `.symbols`,
`.plugin.namespaces`, `.provisional_dispatches` as public FIELDS, so a wrapper
would have to re-export the struct's whole shape across ~106 sites — it drags
the residency model behind it rather than hiding it.
