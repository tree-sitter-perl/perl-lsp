# Narrow-seam review — the few hundred lines where a bug is silent

The "Validation still owed" row of `docs/prompt-scale-validation-hitlist.md`,
against `5cf44dfd`. Five seams: cache byte accounting, residency, invalidation,
the enrichment writer, `IndexCore` shared state — plus the adjudication the
deferred `attached`/`durable` gate split was waiting on.

Ranked by "silent and catastrophic", not by cost to fix. Two findings landed
with base-verified tests; the rest are reported with the evidence that decides
them. **Clean verdicts are stated as verdicts** — an unbroken seam with a
reason is a result, and four of the five are mostly that.

---

## 1. `PackInvalidator::swap` strips against a persist it never checked — **fixed, `586823d`**

`src/index/pack_invalidator.rs`. The watcher's re-analysis path hand-rolled
its own persist loop instead of using `run_persist_writer`, discarding both
signals that say whether a blob landed (`save_to_db`'s `bool`, `tx.commit()`'s
`Result`) and setting `persisted = true` unconditionally — the strip
license. On a rollback the *stale* prior-generation row loads successfully
(the single-row loader deliberately skips stamp validation), so references/
goto-def/types silently answer against the pre-edit generation for the rest
of the session, with no counter, log line, or `PERL_LSP_STRICT_RESIDENCY`
panic. The persist now collapses onto `run_persist_writer`'s existing
BEGIN/COMMIT/ROLLBACK + committed/fallback fork, so one file's failure no
longer licenses its siblings' strip. Base-verified with SQLite triggers
forcing both the whole-transaction-rollback and single-blob-abort lanes.

## 2. The persist/strip licence is untestable, and it is the one thing worth testing — **fixed, `3e14958`**

`#[cfg(test)] open_cache_db -> None` made every strip-licensing decision
unreachable from `cargo test` — exercised only by the gold harness and corpus
runs, and only when they happen to take the failure lane (which they
essentially never do). This is the coverage shape that let #1 exist. The
writer open now factors behind `open_cache_db_at` so both profiles (test and
production) drive the same body, and failure lanes inject via SQLite
triggers rather than lock timing, so each is deterministic.

## 3. `ResolveQueue`'s priority lane can lose its wakeup — **fixed, `7febd2b`**

`IndexCore`'s `ResolveQueue` guarded two lanes with two mutexes but parked on
one condvar bound to only one of them (`pending`), so a `priority` push had
no ordering against the drain: the notify could land before anyone parked,
and the wait loop never re-checked `priority`. On an `EXTRACT_VERSION` bump —
the documented priority trigger — every push took that branch, so the
resolver thread could sleep for the rest of the session: cross-file
resolution silently never completes. Fixed by checking `priority` inside the
wait with `pending` held, and routing producer notifies through
`ResolveQueue::notify_new_work`, which takes `pending` so a push can neither
be missed nor land its notify in the pre-park window. Base-verified:
`priority_push_wakes_a_parked_drain` timed out at 5 s before, returns in
0.3 s after. The rest of `IndexCore`'s locking was audited clean: no guard
held across `resolve()`, no lock-order cycle into the queue.

## 4. The byte-accounting alarm fired on a designed state — **fixed, `841ef9e`**

`evict_to_cap` never evicts `keep` (a single oversized bag over the whole cap
still resolves the query it was loaded for), so an entry larger than the cap
hit the out-of-victims arm on every insert — which unconditionally counted
`resync_bytes_fired`, the drift counter `5cf44dfd` added. A benign, reachable
trigger on that counter is exactly what a real drift instance would hide
behind, and the oversized-entry case is not exotic at 122×. `resync_bytes`
now recomputes first and reports only when the stored total actually needed
correcting. Base-verified: `an_oversized_entry_is_not_reported_as_drift`
fails before, passes after.

### The two-flavors-one-budget question: clean, and structurally so

The brief flagged the rows lane (`b6312ea2`) sharing one byte budget with the
whole lane as "exactly where accounting drifts". It does not, and the reason is
worth stating because it is a property of the design rather than of the current
code:

**No flavor-specific accounting exists.** The charge travels with the entry as
`(Arc<FileAnalysis>, usize)`, so `entries.insert` returns the displaced entry's
own charge and `credit()` refunds exactly it, in the same DashMap operation
that installs the replacement. A stripped→whole upgrade debits the new size and
credits the exact old one; there is no path where a flavor is charged at one
size and refunded at another. `heap_estimate()` is taken **after**
`evict_witness_bag()` on the rows lane, so the stripped entry is charged its
stripped size. One key means one entry, so the two flavors can never coexist
for a path.

I also walked the race interleavings (two threads decoding one path in either
insert order; `invalidate` racing an insert; eviction racing `invalidate`) and
the counter stays exact in each: every debit precedes its entry becoming
visible and every credit is keyed to a removal that returned `Some`.

Two residuals, neither an accounting bug:

- **Thrash, not drift, is the two-lane risk.** The lanes compete for one budget
  with entry sizes differing ~2×, and the LRU has no notion of "this path is
  mostly wanted stripped". A reference walk populates stripped entries; a hover
  on any of those re-decodes whole and roughly doubles that path's charge,
  evicting ~2 stripped entries. Near the cap this is the classic
  two-populations-one-LRU problem. It is invisible to every functional test and
  to a Perl-only soak, and the instrument that would show it is precisely the
  **pack-language soak the hitlist already lists as owed**. Recommend measuring
  the ratio of `refs.matcher_upgrade` to `refs.matcher_rows_view` and the
  rows/whole re-decode counts in that soak before trusting the density win at
  hour scale.
- `generation` and `recency` accumulate `PathBuf` keys that nothing removes
  (`invalidate` inserts a generation entry forever; a lost insert/recency race
  can strand a recency key). Bounded by corpus size, not by time — ~14 MB of
  path strings at 138k files — and not byte-accounted, in a module whose whole
  point is byte accounting. Low priority, worth a sweep on invalidate.

## 5. Residency: the invariant holds, and I checked it exhaustively

The rule that matters — **a reader must never see resident-or-empty** — is
intact. `b6312ea2` made this a live question by changing `symbols_present` from
"a whole copy" to "a deliberately bag-stripped copy" (via `rows_for`), so any
pre-existing consumer that read the bag off a `symbols_present` view was
previously right by accident and would now be silently wrong. I audited all of
them:

| site | reads | verdict |
|---|---|---|
| `resolve/refs.rs:664, 777, 797, 859` | `.symbols()` only | clean |
| `resolve/definitions.rs:786` | `sub_info_view(..).def_line()` — primary symbol span | clean |
| `resolve/definitions.rs:826` | `package_var_def_line` — symbols | clean |
| `model/ancestry.rs:464` | symbols + `symbol_is_class_content` (no bag reads) | clean |
| `model/class_queries.rs:543` | name/kind/package scan | clean |
| `model/cross_file.rs:460, 464` | `has_sub_in_package` / `sub_info_view(..).is_some()` — existence | clean |
| `module_index/queries.rs:246, 376` | existence probes | clean |
| `module_index/lookup.rs:147` | `plugin.namespaces` (never-evicted lane) + symbols | clean |

Every one carries an explicit "symbols-axis read only" comment. The discipline
was applied when the lane landed; this is a confirmation, not a discovery.

`matcher_view` (`resolve/refs.rs:321-348`) — the upgrade-to-whole decision that
the "naive bag-strip loses 106 sites at Koha" note turns on — is also correct,
including the part that looks wrong. Its `_ => false` arm is a target-kind
allowlist, which reads like rule #10's partial enumeration, but:

- `Ref::match_verdict_baked` is `true` for every ref kind except `MethodCall`
  and `HashKeyAccess`, so no other kind can ever need the upgrade;
- the only matcher arms that consult the bag are `(Sub|Method, MethodCall)`
  (`method_call_invocant_class` when `method_target()` is `None` — exactly the
  unbaked case) and `Handler` (`applicable_dispatches`, no-op when
  `provisional_dispatches` is empty). Both are in the allowlist;
- I specifically chased the delegation-alias path, where a name that is not
  `target.name` can match and the pre-scan would miss it. `alias_matched`
  excludes `RefKind::MethodCall` and short-circuits `matches_kind`, so an alias
  match never reaches a bag-reading arm. No gap.
- the closure gate (`file_sees_target_ids`) runs on the raw resident copy
  *before* the view upgrade, which would be a hole if it read an evictable
  axis. It reads `analysis.pack.include_closure` — the pack lane, which
  `evict_axes` never touches. Safe.

Still, `matcher_view` would be strictly better spelled as the general rule
(upgrade when *any* name-matching ref is unbaked, `Handler` keeping its
dispatch check), which is a superset of today's behavior at a cost of at most
one extra decode per file, and stops the enumeration from having to be
re-audited every time a matcher arm grows a bag read.

### One latent hazard: `bag_present` is read for symbols

`model/enrichment.rs:293-329` takes `idx.bag_present(&cached)` and then
iterates `whole.symbols`. `bag_present` promises the bag axis only; the trait's
own doc says a consumer reading more than one axis must take `whole_present`.
This is safe **today** only because of an invariant held in the callers, not in
the reader: every production strip site passes `(true, false)` or
`(true, true)` to `evict_axes` — `index_perl.rs:188` computes `strip_rows =
strip_bag && rows_ok`, `strip_import_copy` is bag-only, `pack_invalidator` is
both — so "bag present, symbols evicted" is unreachable. The signature
`evict_axes(strip_bag: bool, strip_rows: bool)` can express it, and the first
site that writes `(false, true)` silently turns that loop into
absence-by-eviction on every cross-file import enrichment. Either make the
pairing unrepresentable (a closed `StripPlan` enum) or `debug_assert!(strip_bag
|| !strip_rows)` in `evict_axes`; the reader should take `whole_present`
either way.

**Resolved** — the pairing is now unrepresentable. `evict_axes(bool, bool)` is
gone; `FileAnalysis::evict_to(Residency)` takes a ladder enum whose three
variants are exactly the states that exist (`Whole` / `RowsOnly` /
`Skeleton`). The invariant the callers were holding by hand —
`strip_rows = strip_bag && rows_ok` — is now `Residency::for_strip`, so
"bag present, rows evicted" has no spelling and `bag_present` carrying its
symbols is a property of the type rather than of an audit. Proved over every
inhabitant by `residency_is_a_ladder_so_a_bag_view_always_carries_its_rows`.
The enrichment reader was left on `bag_present`: it is correct now for a
structural reason, and moving it to `whole_present` would cost a whole-blob
rehydrate per candidate on a path that only needs the bag.

### `whole_copy_registration_sites_are_allowlisted`

The test greps source lines for `name(` call sites and compares counts. It
does what it claims (it catches a *new* whole-copy site) but three limits are
worth writing down, since the gate split adds registration sites:

- it counts **call sites, not residency** — an allowlisted site with count 1
  can register N whole copies in a loop. The `residency_tripwire`
  (`count_fully_resident` vs `expected_whole`) is the machine gate for that;
  the allowlist is a human-review trigger. The two are complementary, and only
  the tripwire runs on real data.
- it skips `//`-prefixed lines only — a call inside a block comment or a string
  counts.
- it names six registration functions. `insert_cache` (whole by construction —
  `persisted: false, strip: false`) is not among them; it is test-only today,
  and a first production caller would pin whole copies without tripping the
  allowlist. Worth adding with an empty expectation, the way
  `register_workspace_module` already is.

## 6. Invalidation and the enrichment writer: clean

**`PackInvalidator`** — beyond finding #1, the coordination is sound. The H9-1
generation guard claims *before* unregistering, so a stale re-analysis leaves
the fresher registration intact rather than tearing it down; the surface gate
(`skip_consumers`) refreshes consumers' `deps_stamp` on the Unchanged path,
which is the non-obvious half (an unchanged header still moved every consumer's
closure stamp, and skipping the refresh would make the next warm reject every
consumer row). `is_consumer` stays the single include-closure rule.

**`FileStore::enrich_open`** is the one open-doc writer and holds its
contract. It clones off the store lock, enriches, and swaps under a short
`Arc::ptr_eq` guard, so a concurrent rebuild wins and this derivation is
dropped. Idempotency is real: `enrich_imported_types_with_keys` truncates
`symbols`/`witnesses`/`refs` back to their sealed baselines before re-appending,
and I checked that the enrichment body's appends land only in those three lanes
(`enrichment.rs:151, 172, 180, 194, 347, 464, 540, 573, 581, 589` — symbols,
witnesses, refs, and `push_type_constraint`, which is itself a witness push).
`plugin.gated_emissions` is assigned wholesale rather than appended, so it
cannot accumulate. Re-enriching an already-enriched clone is therefore stable
rather than growing, which is the property that matters for a long-lived open
doc.

The concurrency residual is benign: two threads enriching one URL both derive
from the same `base`, one wins the swap, and the loser returns its own equally
valid derivation to its caller. Both were built from the same baseline.

`Document::baseline_surface` being the freshness record — pre-enrichment, by
construction — is what makes surface verdicts enrichment-invariant without any
record-before-publish ordering. That is the load-bearing piece and it holds.

## 7. Not a bug: `epoch.gen_stamp_missing = 1074`

The hitlist's Tier 2 "a counter incrementing a thousand times unexplained".
It is `IndexCore::stamp_missing_import_gens`, whose whole job is to stamp a
generation for cache entries the @INC warm scan wrote without a registration
front door. `or_insert_with` means it fires at most once per path, so 1074 is
simply the number of warm-loaded providers that had no front-door generation —
the function working. (`9d5e1cc0` closed this row; recording the mechanism here
because it took thirty seconds to confirm from the code and the row's
"correlated with nothing" reads as more mysterious than it is.)

## 8. Unmeasured, lower confidence: `record_loader_shapes` in the parallel walk

`IndexCore::record_loader_shapes` opens with

```rust
self.loader_config_shapes.retain(|_n, v| { v.retain(|(c, _)| c != contributor); !v.is_empty() });
```

and it is called once per file from inside `paths.par_iter().for_each(...)`
(`index_perl.rs:399`) and again per file on the warm scan (`:181`).
`DashMap::retain` takes a write guard on **every shard**, so every one of
138,822 files puts a global write barrier in the middle of the parallel bulk
index — even when the map is empty and there is nothing to retire. The
per-call work is bounded by the number of distinct loader names (small), so
this is a contention claim, not a complexity one, and I have **not measured
it** — the corpora are not available here. If the cold-walk's 4.5 ms/file (vs
3.0 at Koha) is ever re-attributed, this is a cheap thing to rule out: guard
the retain on a non-empty map, or track contributors in a reverse map so a
re-registration touches one key.

## 9. 22 gold rows skip silently on a threaded Linux perl

Found while building the substrate rather than by reading the seams, but it
is the same failure shape the review is about. The DateTime fixtures spell
their corpus path with a hardcoded arch triple:

```
gold-corpus/local/lib/perl5/x86_64-linux/DateTime.pm
```

Debian/Ubuntu's threaded perl installs into `x86_64-linux-gnu-thread-multi`,
so on those hosts all 22 DateTime rows report `file not found` and land in
`skip` — the run still exits 0 and still prints `0 FAIL / 0 XPASS / 0 CRASH`.
A 469-row green and a 491-row green are indistinguishable at a glance, and the
skipped set is exactly the cross-file hover / references / type-at rows that
exercise a real XS-bearing dist.

Confirmed by bridging the two spellings: with a symlink in place the same
binary and the same substrate go 469 PASS / 22 skip → **491 PASS / 0 skip**.
The fixture paths want to resolve the arch dir from `$Config{archname}` (or
glob it) rather than assume one; failing that, `run.pl` should treat a nonzero
skip count as something the summary shouts about.

---

# The gate split — adjudication

**Closed by measurement, not by this adjudication.**
`docs/prompt-scale-validation-hitlist.md` measures the post-walk drain at
1.4 s at 138k files — the multi-minute drain this section's risk-weighing
assumes throughout does not exist, which retires the `attached`/`durable`
split before it is built. What follows is kept as the reasoning against a
drain that turned out not to exist; the verdict below is moot, not a live
recommendation.

The design, from `fc863769`'s Tier 1 #1: split `IndexReady.perl` into
`attached` (opens at walk end) and `durable` (opens at drain end), plus
worker-time registration of stripped copies behind a pending-blob overlay.
Named failure lanes: commit-fail must replace a stripped registration with the
whole-copy fallback; budget-overrun must UNREGISTER, "needing a removal path
that does not currently exist".

**Verdict: the gate split is safe. The worker-time registration it is bundled
with is not, as specified — and the two are separable. Land the split; do not
land register-before-commit until three things are true.**

## Why the split alone is safe

The gate is a `ReadyGate` that bounded waiters block on
(`await_index_ready(language, WaitPolicy)`). Splitting it into two gates
changes nothing about residency; it changes which verbs wait for what. The
whole risk is per-verb classification, and it is a bounded, reviewable
exercise: `attached` may gate only answers derivable from what exists at walk
end (which files exist, which names they declare — the registration feed is
extracted pre-strip and is complete at walk end), and `durable` must gate every
answer that reads file *content* through a possibly-evicted axis, because
content is what needs a committed blob to rehydrate from.

The one thing to insist on: **there is no type-level enforcement of that
split.** A `ReadyGate` is a runtime latch, so a verb wired to the wrong gate
under-answers silently — the same failure class as everything else in this
review. If the split lands, the classification wants to be a property a verb
declares once (next to its `WaitPolicy`) rather than a call-site choice, and it
wants a test that enumerates the verbs, the way
`whole_copy_registration_sites_are_allowlisted` enumerates registration sites.

## Why register-before-commit is the dangerous half

**Today, "not yet registered" is the safe state, and every failure lane is a
*decision not to register*.** `index_perl.rs:412-416` says it exactly: *"Until
then the file reads as 'not yet indexed' — never wrong-empty."* That is why
`on_fallback`'s drop-past-budget is honest — nothing was registered, so nothing
has to be taken back.

Worker-time registration inverts this. Both failure lanes stop being decisions
and become **retractions**, which is a strictly harder problem, and it breaks
three properties that the current design leans on:

### (a) It retires the instrument that catches absence-as-answer

`rehydrate_or_resident` documents its miss as *"ALWAYS an invariant break
in-session: eviction is licensed only by a committed blob (persist-first)"*,
and `PERL_LSP_STRICT_RESIDENCY` panics on it — the gold harness sets it, which
is what makes a run that serves absence fail as a CRASH row instead of scoring
wrong answers. With stripped copies registered before commit, a rehydration
miss during the pending window is **normal**. That forces one of two things:

- the overlay serves the pending blob, so misses stay invariant breaks — this
  is the only acceptable option; or
- misses become expected and strict mode must be relaxed for the window — which
  retires the one instrument that catches absence-as-answer, in the same change
  that introduces a new way to serve it.

So the overlay is not an optimization; it is load-bearing for the crash canary.

### (b) The overlay must WIN over the loader, not fall back to it

This is the concrete flaw, and it is the same mechanism as finding #1.
`load_one_diag` skips stamp validation for a single-row path. On any warm start
of an edited tree, the previous session's row exists. So during the pending
window a rehydration for a pending path would find a row, **succeed**, and
serve last session's analysis — retained in the LRU, with no miss counted.
`on_committed`'s `invalidate_bag_cache` would clear it afterwards, so the
wrongness is bounded by the window; but the window is the 7–9 minute writer
drain, i.e. exactly the first-time user's first ten minutes that Tier 1 #1
exists to fix.

Requirement: the overlay must be consulted **ahead of** the SQLite loader for
pending paths, and the loader must refuse a row for a path with a pending
registration. Fallback-on-miss (the R4 overlay's shape) is the wrong shape here.

### (c) The ordering guarantee has nowhere to sit

Today `on_committed` does `invalidate_bag_cache(path)` and *then* registers, so
"an evicted copy is never reachable before its blob can rehydrate it" holds by
construction. Register-before-commit removes the point in time where that
sequence is expressible. The pending-blob overlay has to supply the guarantee
instead, which means the overlay must be populated **before** the stripped copy
is registered, and torn down **after** the LRU is invalidated at commit.
That ordering should be minted by one function that owns both, the way
`prepare_workspace_parts` owns reads-whole-before-evict — not spelled at the
call sites.

## The two failure lanes, adjudicated

### Commit-fail → whole-copy fallback: **mechanically fine, and the memory is a wash**

`run_persist_writer` already forks committed/fallback and the fallback already
registers whole copies under `FALLBACK_WHOLE_BYTE_CAP`. Under
register-before-commit the fallback becomes *replace the stripped registration
with a whole one*, which is a re-registration — `register_workspace_resident`
already replaces in place and re-picks the name-slot winner. No new machinery.

On memory: the overlay holds the same `Vec<u8>` blobs the `WsFresh` channel
entries already hold today, so it is roughly neutral against the ~7 GB drain
backlog rather than additive — **provided the overlay holds encoded blobs, not
decoded analyses.** An analysis-shaped overlay would multiply that backlog by
the decode ratio and rebuild the wall this whole arc exists to avoid.

### Budget-overrun → unregister: **the removal path exists; the brief's premise is slightly off, and what is actually missing is different**

`ModuleIndex::unregister_workspace_path` (`registration.rs:692-733`) is a real
inverse for the Perl workspace tier — `remove_surface`, `all_files`,
`edges.remove_path_record`, `registered_names` → `all_defs` →
`rebuild_name_registration` per affected name, with a cache-scan fallback for
legacy doors. `unregister_file` (`:1248`) is the pack twin. The
`registered_names` design exists precisely so the inverse is exact under symbol
eviction (`docs/adr/storage-engine.md`, "Registration inverse under symbol
eviction").

What is genuinely missing is narrower, and worth having the real list:

1. **`FileStore.workspace`** is not part of either unregister; the caller must
   pair `files.remove_workspace(path)`. Today the only unregister callers are
   deletion paths that do. A budget-overrun retraction is a new caller and will
   forget unless the two are fused.
2. **`IndexCore.loader_config_shapes` has no removal.** Entries are keyed by
   contributor path string and retired *only* by that contributor
   re-registering (`record_loader_shapes`'s opening `retain`). A retracted file
   leaves its loader-config shapes in the index permanently — a phantom
   contributor for `PluginLoad` config typing.
3. **`ModuleIndex.loaded_modules` has no removal.** `record_workspace_projections`
   inserts every import and plugin-load name; nothing takes them out. This
   feeds the entrypoint-scan lint, so a retracted file leaves phantom
   "module is loaded" facts.

Both (2) and (3) are written at **worker time** by
`record_workspace_projections`, which is exactly the point the design proposes
to make reachable early. That is the real gap: not "no removal path" but
"the removal path does not cover the two lanes worker-time registration
writes".

4. Not missing but load-bearing: **unregister has never run concurrently with
   live queries.** Every existing caller is on the watcher path behind
   `PackInvalidator`'s serialization lock, or is a delete. A budget-overrun
   retraction fires from the persist writer thread while handlers are reading.
   `unregister_workspace_path` does `all_files.remove` → `registered_names.remove`
   → per-name `all_defs` mutation → `rebuild_name_registration`, all as separate
   DashMap operations with no overall atomicity: a query interleaving mid-way
   sees a file removed from `all_files` but still a candidate in `all_defs`, or
   a name slot briefly pointing at a departed path. Today that window is
   invisible because it is serialized; under the new caller it is not.

## What must be true first

1. **A test-mode `open_cache_db`** (finding #2). Both failure lanes are
   persist-outcome lanes; without this they ship unexercised, and finding #1
   is the proof that an unexercised persist lane rots.
2. **Fix finding #1 first**, on its own. It is the same bug the commit-fail
   lane must not reproduce, in code the gate split will sit on top of, and it
   is a much smaller change to reason about in isolation.
3. **The overlay wins over the loader for pending paths**, with the loader
   refusing a row for a pending path — not fallback-on-miss (b). Otherwise the
   stale-single-row path serves last session's analysis for the whole window.
4. **The overlay holds encoded blobs**, byte-capped, so it is neutral against
   today's channel backlog rather than additive; and overflow of *that* cap is
   the budget-overrun lane, so cap and lane are one mechanism.
5. **Retraction is one function** that fuses `unregister_workspace_path`,
   `FileStore::remove_workspace`, and removal of the `loader_config_shapes` /
   `loaded_modules` contributions — and its concurrency window is either closed
   or explicitly bounded (4 above).
6. **The `attached`/`durable` classification is declared, not chosen per call
   site**, with an enumerating test.

1, 2, 3 and 5 are prerequisites. 4 and 6 are shape constraints on the design
itself. None of them is large; together they are most of the work, which is
the honest reading — the hard part of this change was never the gate.

**And a separability note that may be the most useful thing here:** the
availability win the row is after comes from opening `attached` at walk end.
Worker-time registration is what makes `attached` *useful* for content verbs —
but a split where `attached` gates only existence/name answers needs **no**
worker-time registration, no overlay, and neither failure lane. That is a
strictly smaller change that banks part of the win and can land now, against
seams that are already proven. Whether it banks enough is a measurement
question (how much of the wedge is existence-shaped), and it is one the bench
harness can answer before committing to the larger design.
