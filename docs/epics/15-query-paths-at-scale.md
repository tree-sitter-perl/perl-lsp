# Epic 15 — Query paths at scale: the Tier 1 residual

> **Status:** scheduled (15th) but **high priority by impact** — with
> Epic 14 it is one of the two places the product is unusable rather
> than incomplete, and it is the one that bounds the stated target
> market.
> **Design owner-docs:** `docs/prompt-scale-validation-hitlist.md`
> (the status board is the worklist; every row has a detail section),
> `docs/scaling-limits.md` (the two measured pathologies),
> `docs/adr/skipping-cross-file-work.md` (**read before proposing to do
> less cross-file work — seven proposals were measured and rejected**).

## Mission

The 122× validation pass reached a clear verdict, and it is two
sentences:

> **Storage and startup hold.** Warm ready is scale-free, post-ready RSS
> is flat, the bulk walk is near-linear, and per-file db cost *fell* at
> 39× the corpus. …
> **Query paths break**, each for its own reason, and the CLI's one-shot
> "act like the LSP just started" semantics are O(corpus) in time and
> RAM.

Storage is done. This epic is the other half.

## The board, as of the last measurement (2026-08-17)

| metric | crm (1,136) | Koha (3,554) | CPAN-5k (138,822) |
|---|---|---|---|
| LSP warm ready | 0.81 s | 1.58 s | **1.06 s** |
| post-ready RSS (warm) | ~297 MB | 170 MB | **255 MB** |
| open (warm) | — | 1.6 ms | 0.9 ms |
| hover/def (warm) | — | 0.2 ms | 402 ms |
| completion (warm) | — | 7 ms / 24 KB | 188 ms / 55.9 KB *(after `b6312ea2`)* |
| references, hot name | ~15 ms | 5.6 s | **265–368 s**, 2.8 GB peak, marked incomplete |
| diagnostics after edit | — | 330 ms | **never (60 s)** |
| CLI one-shot (warm) | 1.33 s | 1.90 s | **350 s** *(after PR #125; was DNF at 42:32 / 7.11 GB)* |

**Re-take every one of these before acting on it.** They are dated, the
tree has moved, and a number without a fresh date is a hypothesis.

## Read first

1. `docs/prompt-scale-validation-hitlist.md` — the status board, then
   the detail section for whichever row you are taking.
2. `docs/scaling-limits.md` — §1 (FHEM `package main` monoculture) and
   §4 (memory and fan-out are independent axes) in particular.
3. `docs/adr/skipping-cross-file-work.md` — **seven proposals to do less
   cross-file work were each measured and rejected.** Do not re-open
   without reading it. The one that worked (the resident-copy export
   gate) took the cross-file provider chase from 1,541 ms to 241 ms.
4. `docs/adr/relational-ref-index.md` — the row store that made
   `references` return at all.
5. `docs/adr/conclusion-layer.md` and `model/witnesses/closedness.rs` —
   the persisted bake and the closedness certificate; both exist to
   avoid decodes, and both are levers here.
6. `docs/prompt-storage-residuals.md` — the known unbounded residuals,
   deliberately listed.
7. `bench/MEASURE.md` and the `edit-bench` skill — the protocol.

## Phase breakdown

### Phase A — `references` at scale (Tier 1 #3, the headline)

**Status: RETURNS, honestly, and slowly.** It used to never return, at
7+ GB; `32a3bf4e` and `b6312ea2` made it finite. At 138k files it is
265–368 s at 2.8 GB peak and marks the answer incomplete. Slow, honest,
and bounded — not yet fast.

1. **Profile first.** The hitlist lists "profile the 150 s of refs CPU"
   as IN PROGRESS; finish it and publish the attribution before
   optimizing. The candidate costs are known in shape:
   - the backward-walk matcher rehydrating per candidate file,
   - the `matcher_view` upgrade to a whole copy when a name-matching
     ref's verdict is not baked (`Ref::match_verdict_baked` — unstamped
     MethodCall, unowned hash key),
   - the candidate set itself (`ref_candidate_files` = refs UNION syms).
2. **The row store is the pre-prune and it is already sound.** `refs`
   rows are `(name_id, file_id)` pairs, `WITHOUT ROWID` so the table IS
   the name index, with **deliberately no occurrence `count` column** —
   a row count is a CANDIDATE count, and that over-approximation is what
   makes `unused_exported_syms` sound. Any speedup that adds a count
   column breaks that. Use `PERL_LSP_REF_ROWS=0` and
   `--refs-parity <root> [--sample=N]` as the A/B nets.
3. **Reduce the whole-copy upgrades.** Every upgrade is a full decode.
   The honest lever is making more verdicts bakeable — which is the
   conclusion layer's job, and `OpenReason`'s tally exists to measure
   exactly which population would convert.
4. **Acceptance:** the 138k hot-name reference query in **interactive
   time or with a declared, visible incompleteness** — and if it stays
   slow, an honest bound stated in the docs rather than a silent
   timeout. Three runs, dated. `--refs-parity` clean.

### Phase B — the sweep-level provider decode dedup

`scaling-limits.md` §1 names this as the remaining honest fix for the
FHEM shape, with the arithmetic:

> deduplicating provider decoding across a sweep (~13,456 rehydrates for
> ~500 distinct providers is ~27× redundant)

1. The relation is **semantically correct** — 534 files genuinely
   providing `package main` is what FHEM is — so narrowing it would be
   wrong. The fix is at the sweep level, not the relation level.
2. **This interlocks hard with Epic 1**, which converts ~75 consumers
   from the single winner to the candidate set. If Epic 1 lands first,
   this phase's arithmetic gets worse before it gets better and its
   value goes up correspondingly. Coordinate: Epic 1's PR reports the
   FHEM numbers this phase then has to fix.
3. **Bounded, byte-accounted, and shared.** CLAUDE.md's residency
   discipline is not negotiable: a per-sweep dedup cache is a derived
   copy store and must be byte-accounted like `PackBagCache` and the
   enrichment overlay (128 MiB + 64 entries). Note the measured trap:
   the per-file memo byte cap **engaged correctly (19,929 evictions) and
   made peak worse (+15%) and wall worse (+51%)** — it was reverted.
   Whatever you build, measure it against that outcome.
4. Note also what is already known not to work, so it is not re-tried:
   the sweep path memo is **load-bearing** (disabling it moves peak 0.7%
   and costs 55% wall); walk residency, overlay clones, allocator arena
   count, the diagnostics channel and the source-byte admission gate
   were each ruled out **by a control, not an argument**.
5. **Acceptance:** FHEM `--check` completes on a normal machine, or the
   remaining gap is attributed. Report peak RSS and wall with and
   without the documented workaround
   (`RAYON_NUM_THREADS=4 MALLOC_MMAP_THRESHOLD_=65536`, which today cuts
   peak 67% for 4.9% wall).

### Phase C — the CLI one-shot, and diagnostics-after-edit

1. **CLI one-shot** (Tier 1 #5 residual + #6). PR #125 made it finite —
   350 s at 138k with a real answer. The root cause of the remaining
   pack share is understood: `LanguageScope` lets a verb declare the
   families its answer can consult, measured at **−52% CPU** on a
   synthetic Perl query with the pack phase falling 936 ms → 0.11 ms.
   **That is ROOT-CAUSED but unconfirmed at 138k** — confirm it, then
   audit every verb's scope choice (Epic 10 adds more).
2. **Diagnostics after edit: never, at 60 s** on CPAN-5k. This is the
   most user-visible row on the board — it is the thing an editor does
   after every keystroke pause. Attribute it: it is the enriched
   diagnostic path over an open document, and
   `adr/enrichment-build-cost.md` already measured the composition
   (copy 3.8%, cross-file provider chase 61.6% — **that share predates
   the resident-copy export gate**, which took the chase from 1,541 ms
   to 241 ms, so re-measure rather than citing it).
   `prompt-incremental-build.md` is the parked design space, and this
   row is its forcing function: read its Tier 0 ("stop blocking, reuse
   the existing lane") first — it ships alone and may be most of the
   answer.
3. **Acceptance:** diagnostics-after-edit returns at 138k with a stated
   latency; the CLI scope audit complete with per-verb justification.

### Phase D — the owed validations

`prompt-scale-validation-hitlist.md` §Validation lists four rows as
**OWED** or **IN PROGRESS**, and they are owed because a fix is not
verified until the corpus that found the bug runs clean:

1. **Cold cpan5k with every fix in** — OWED.
2. **The differential sweep (main vs branch)** — OWED. This is the one
   track the whole validation pass still owes; `--refs-parity` is the
   template for what a differential net looks like.
3. **Re-soak `PackBagCache` on current tip** — OWED. The pack-language
   soak that ran was clean at 3h20m but **pre-rows-lane**, and the 4.65 h
   Perl soak was Perl-only, so `PackBagCache` was compiled in and never
   exercised. This is the cache whose denormalized byte counter once
   collapsed the LRU to one entry and cost 13.9 GB.
4. **Profile the 150 s of refs CPU** — IN PROGRESS; it is Phase A's
   step 1.

Land the results in `bench/` and update the status board. A row that
cannot be closed gets its residual named.

### Phase E — the Tier 2/3 stragglers, or an explicit decline

Each of these is OPEN and small; take them or decline them with a
reason, but do not leave them unlabelled:

- `query_rec` 512-depth cap — seen again during the Phase-A probe.
- `cursor_slot.rs` deferred reducible case.
- Merge the two index families (Tier 3, structural — likely its own
  arc, and Epic 11's ADR is asked to note the same debt from the
  closure side).
- The grammar-kind tripwire — LANDED, accepting DECLARED future kinds so
  it cannot eat a forward-compat arm (`parenthesized_expression` was the
  live example until ts-parser-perl 2.0.0 landed it).
- "A stale cache hides a fix as readily as it hid the crash" — OPEN,
  and it cost one false gold FAIL during integration. This is a
  developer-experience bug with a real cost; `perl-lsp --clear-cache`
  is the tool, and **never "fix" a flaky gold run by clearing the real
  cache** — that deletes the evidence a warm gap exists, and a PARTIAL
  clear is worse than none.

## Non-goals

- **Re-opening the rejected cross-file-work reductions.**
  `adr/skipping-cross-file-work.md` measured and rejected seven. Read it
  first; if you have an eighth, the doc's format is the bar.
- Narrowing a semantically-correct relation to make it cheaper. FHEM's
  534 providers of `main` are real.
- Level-indexed enrichment as stated — its named prerequisite (shrink
  the copy) is not the bottleneck; the copy is 3.8%.

## Language-pack beat

**The two axes point opposite ways, and conflating them has already
cost time.**

`cpp-status.md` says it plainly: C++ is **memory-healthy and
wall-pathological** (Godot: RSS flat at ~2 GB across 7,041 files, but
30–66 s on individual generated headers); Perl's FHEM shape is
**memory-pathological** (does not complete `--check` on 31 GB). *"They
share no mechanism, and the Perl scaling work neither caused nor fixed
the C++ one."*

So:

1. **This epic is the Perl/engine axis. Epic 14 is the C/C++ per-file
   axis.** Do not merge them, and do not cite one's numbers as evidence
   about the other.
2. **But the shared machinery is shared, and every fix here must be
   proven not to hurt the pack side.** The row store, the conclusion
   layer, the closedness certificate, the eviction/residency discipline,
   `LanguageScope` and the warm-stub path all serve both. Every phase
   runs `cargo test --features cpp` and the gold suite with
   `lang-skip 0`.
3. **Phase C's `LanguageScope` work is explicitly cross-language** —
   the whole row exists because pack indexing dominated startup on a
   *Perl* corpus. Every verb's scope choice is a language decision, and
   Epic 10 adds more verbs that need auditing.
4. **Phase D's `PackBagCache` re-soak is the pack side's row on this
   board**, and it is owed. Do not close the validation section without
   it.
5. `prompt-unify-language-paths.md` is parked and stays parked — a
   cleanup with no user-visible product does not belong in the epic
   whose entire subject is user-visible cost.

## Scaling beat

The whole epic is the beat; what follows is the method, which is
non-negotiable here more than anywhere:

1. **A single run is not a baseline.** Three minimum. A phantom +400 ms
   "regression" survived a day on one.
2. **A number without a date rots silently.** Stamp every measurement.
3. **Quiet box only** for the editor surface; `bench/editor-baseline.sh`
   records loadavg on its run lines for exactly this reason.
4. **Raw at collection, aggregated at query.** `bench/measure.sh` emits
   tall JSONL (`{kind,name,value,unit}`); `bench/load.sql` /
   `report.sql` slice it. `bench/baselines.jsonl` is the checked-in KPI
   record, curated by `seed-baselines.py` after a *trusted* sweep — every
   row carries sha/dirty/host/n/spread, and `baseline-check.sql` flags
   only moves clearing both sides' measured noise. **Counters are
   diagnostics and are never baselined.**
5. **`bench/RESULTS.md` is append-only.** Findings go in; nothing is
   quietly revised.
6. **The instrument must not distort the measurement.** Route everything
   through `util/timings.rs`; `bphase!` for per-file regions,
   `ghost_stats::count_by` for per-file quantities, never a hand-rolled
   env-gated `eprintln!`. `adr/instrument-blindness.md` is the record of
   getting this wrong.
7. **Detach long measurement runs from agent worktrees** — unchanged
   worktrees get swept mid-run.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS with
`lang-skip 0` · `./e2e/run.sh` · substrate audit at exact parity
(**this epic must not change a single answer** — it changes what they
cost) · `--refs-parity` clean for Phase A · the status board in
`prompt-scale-validation-hitlist.md` updated, with every row this epic
touched moved to CLOSED or given a named residual · `bench/RESULTS.md`
appended · `bench/baselines.jsonl` re-seeded from a trusted sweep for
any KPI that moved.

## Sizing

Large, and the least predictable on the slate — it is profiling work,
and profiling work sizes itself. Phase A is the headline and should go
first; B interlocks with Epic 1; C is the most user-visible; D is owed
regardless of whether anything else here happens, and could be done by
itself in a day of machine time.
