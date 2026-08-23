# Enrichment alternatives — changing the unit, not the cache

The research question: cross-file enrichment enters the caching thrash
regime at scale, yet the incremental updates it performs are *legitimate* —
cross-file chains (A imports B extends C) really do require re-deriving
facts in many files when one file changes. "Cache harder" is measured dead
(`docs/prompt-scale-validation-hitlist.md:310-322`: 100% hit rate, 10.7M
consults, no answer). This document surveys the alternative architectures
against the constraints this project has already measured, and argues they
converge on one move: **change the unit of enrichment from "an enriched
whole FileAnalysis" to "a file's exported conclusions," and change
invalidation from a global epoch to conclusion-diff propagation along real
dependency edges.**

Companion reading, in order: `docs/adr/skipping-cross-file-work.md` (eight
closed shortcuts), `docs/adr/enrichment-build-cost.md` (where a build's
time goes), `docs/adr/level-indexed-enrichment.md` (REJECTED, why),
`docs/adr/resolution-session.md` (the containment that ships today), and
issue #120 (the working thread; the conclusion-layer design lives there).

## 1. The problem, precisely

Two distinct regimes get called "thrash", and they need different fixes:

**Capacity thrash** — LRU churn where memory grows and buys nothing.
Measured instances: repeat `references` never cache-hit (RSS 566→635 MB
over 6 identical queries, latency pinned ~3.4 s); the two-lane
stripped/whole competition in `PackBagCache` (a hover on a
reference-walked path doubles its charge and evicts ~2 stripped entries);
bench Round 9's redis inversion (warm refs 1.73 s vs cold 383 ms — the
warm lane pays rehydration on the hot path); the 13.9 GB
single-entry-LRU collapse that `resync_bytes` now guards against.

**Inherent fan-out** — the work itself is super-linear, so a perfect cache
still loses. With every cache sized past its cliff (~100% hit, 343
evictions), one `references` at 138k files executed 10.68M
`consult.moc_primary` in 15 minutes and never reached projection. Root
causes, all three structural:

1. **Answers are re-derived per consult.** `query_rec`'s seen-set dedups
   within one chase; the `ResolutionSession` memo widened that to one
   walk. Nothing survives the walk — because nothing *can*, validly: the
   only durable invalidation signal is the process-global additive epoch
   (`gen_counter + shape_bumps + freshness.write_count()`), which any
   registration moves ("over-invalidation by design",
   `module_index/mod.rs`).
2. **Enriched analyses are context-dependent.** `ENRICHING` is a
   thread-local path set, so whether B comes back enriched or raw depends
   on who asked first. Tainted results are (correctly) never cached —
   raising `ENRICHED_CAP` 64 → 100,000 changed nothing because the cap
   governs retention and tainted results never reach it.
3. **The package→file relation is a set.** 5–12 files declare a common
   package name at 122×, and the chase is keyed by name, not by an import
   edge, so every hop multiplies.

The shipped state is *containment*, not a fix (`32a3bf4e`): session memo +
wall-clock budget at the cross-file boundary + `PERL_LSP_ENRICH_DEPTH=4` +
a session around `enrich_open`. The depth cap under-enriches real code —
Koha's measured enrichment-depth tail is **12** — and the budget makes
answers reproducible only in practice ("a wall-clock bound is
deterministic only while it does not fire"; it fires ~8,000× at cpan5k).

### What enrichment actually produces

The pass (`enrich_imported_types_with_keys`, ~400 lines) re-derives, per
open doc, per publish: gated plugin emissions, loader-config params, the
provider chase (need-driven, export-gated), imported-return
TypeConstraints, hash-key owner fixups, cross-file inheritance edges,
mutation extensions, MCB edges, `stamp_method_call_targets`, and an index
rebuild — after truncating symbols/refs/witnesses to sealed baselines.
The output delta is **tiny**: +10 symbols, +37 refs, +1,618 witnesses —
**4.13% of base heap** (150-module probe). The cost is not the copy (3.8%
since the clone swap) and no longer mostly the chase (6.4× down via the
`exports_name` resident gate); it is `stamp_method_call_targets` (~10 s
per substrate `--check`, 63% of blob decodes) — and stamp's cost is
*success*: a resolved invocant means an ancestry walk means rehydrating
providers. A hit costs 6.1× a miss.

So: a huge, context-dependent, coarsely-invalidated cache entry is being
rebuilt wholesale to carry a 4% delta whose expensive part is consulting
*other files' conclusions*. That sentence is the whole problem.

## 2. Alternatives that do NOT change the unit — measured dead, do not re-open

- **Bigger/smarter caches.** Moves the wall (regime 2 above). Also the
  cheap/expensive split inverts past the cache cliff, and 124k lives
  permanently past the cliff — a memo that does nothing at 128 MB was
  4.18× past it (issue #120, sweep-scoped memo experiment).
- **Skip predicates.** Eight proposals, all measured and closed in
  `docs/adr/skipping-cross-file-work.md`: unconditional re-stamp skip
  (0.43% silent divergence), wholly-local ancestry (sound, 0.79% coverage
  on Koha), Surface-freshness skip (**unsound** — `MethodSurface::ret` is
  a local conclusion, so cross-file-dependent return changes are
  invisible; pinned by a test), `ImportedSub` (0.0% of cost), hash-key
  baking (0.12% moved), per-match gating (0.073%), owner re-anchoring
  (two irreconcilable notions of "owner").
- **Level-indexed enrichment as built.** Correct, deterministic, cacheable
  at every level, deletes the taint rule and the depth cap — and 2.5–15×
  too slow because a "build" is a whole-analysis copy + full re-derive,
  paid K times, and the correctness floor (Koha tail 12) puts K at 16,
  the column that times out. The ADR's named prerequisite: make a build
  cheap by emitting a small overlay instead of copying an analysis.
- **Narrowing that optimizes counts, not costs.** The mroc/mdmp memo
  (#151): candidate fetches 1.69M → 0, wall time Δ0 — the fetches removed
  were the cheapest rehydrations. "True about count, false about cost."

## 3. The alternatives that change the unit

Four families, from the least to the most structural. They are not
mutually exclusive — the recommendation in §4 composes 3a + 3c.

### 3a. Conclusion layer (issue #120, stage 2 — designed, gated, waiting)

Persist per file a **dependent conclusion algebra** — `Value` /
`ReturnOf(ReturnExpr)` / `Timeline` / `Link` / `Project` / `OpenNone` —
keyed by attachment, in its own blob column, ~1.5–2.5 MB per corpus
(~20× under the bag). Consults are served from conclusions; the bag is
decoded only when a key is absent ("absent means decode" — incomplete
enumeration degrades to slow, never wrong). Measured basis: consults are
one shape (`consult.moc_primary` 96%); cross-file queries are point-free
by construction (all five entry sites pass `point: None`), so timelines
need not persist for closed providers; the removable cost is **78%
compute / 22% decode**, and the compute share is cache-state-independent
(10.2% cold → 16.2% warm — it *grows* as everything else gets faster).
Stage 1 (bag in its own column) is merged (#150); the stage-2 gates are
met (deterministic fold proven by a seeded-map test; invalidation by
`build.rs` hash over the derivation tree).

What stage 2 alone does NOT address: *when* conclusions are (re)derived,
and *what* invalidates them. If conclusions are minted by the same
recursive on-demand enrichment and invalidated by the same global epoch,
the fan-out and the taint problem survive with smaller constants. Stage 2
is the right unit; it needs a propagation story (§3c).

### 3b. Overlay-shaped enrichment (delta representation)

Make `enrich_imported_types_with_keys` produce an
`EnrichmentOverlay { symbols+, refs+, witnesses+, retargeted refs }`
instead of mutating a clone; consumers read base ∪ overlay. Supported by
the consumer matrix (`enrichment-build-cost.md`): **every recursive
consumer reads only the bag**, so a *bag* overlay serves the recursion,
with whole-copy materialization reserved for the two one-shot CLI verbs.

Honest assessment: this does not make a build cheap on its own (the copy
is 3.8%; the compute is the cost), so it is not, alone, the level-index
prerequisite it was once assumed to be. What it buys: the 128 MiB
enriched-overlay budget holds ~25× more entries (4% deltas instead of
whole analyses), which directly attacks capacity thrash and the two-lane
size disparity; the truncate dance becomes structural instead of
procedural; and it is the natural in-memory shape of 3a's persisted
conclusions. Worth doing *as part of* 3a, not as its own arc.

### 3c. Conclusion-diff propagation (semi-naive fixpoint across files)

The structural fix. Reframe cross-file enrichment from *recursive
on-demand descent* ("to enrich F, enrich F's providers first") to a
**background worklist fixpoint over conclusions**:

1. A file is dirtied (edit, watcher, registration).
2. Rebuild it bare (as today), then derive its **conclusions** (3a's
   algebra) consulting only the *current stored conclusions* of its
   providers — never recursively enriching them. No recursion ⇒ no
   `ENRICHING` guard, no depth cap, no taint.
3. **Diff the new conclusions against the old.** Unchanged ⇒ stop: the
   chain is cut here, and this cutoff is *sound* — unlike the closed
   Surface-skip (#3 in `skipping-cross-file-work.md`), the diffed
   artifact is the post-consultation conclusion set, so a
   cross-file-dependent return change *does* change it. The unsoundness
   of the local-Surface cutoff was never "cutoffs don't work"; it was
   "you diffed the wrong artifact."
4. Changed ⇒ enqueue consumers via the freshness reverse-dep walk
   (`dirty_consumers` already exists and is transitive) and loop.

Termination is the builder's own worklist argument lifted one tier:
`InferredType` is a finite lattice, witnesses/conclusions are monotone
within a round, and the diff is the snapshot check. Cycles (mutual
imports) don't need detection — they iterate to fixpoint exactly like
recursive sub clusters do inside `fold_to_fixed_point` today. A cyclic
SCC converges in as many rounds as its dependency diameter, each round
costing one *conclusion derivation* (small), not one whole-analysis
enrichment. This is semi-naive Datalog evaluation, and it is also
level-indexed enrichment with the K× multiplier applied to deltas
instead of whole builds — round k re-derives only files whose inputs
changed in round k−1, and rounds stop when a diff is empty, rather than
running a fixed K for every file.

Precedent: Flow's types-first architecture and Hack's decl-diff
service — per-file signatures computed against stored signatures,
signature diffs drive the recheck cone. Their measured lesson matches
this repo's: the recheck cone is bounded by *what actually changed in
the exported surface*, not by the syntactic dependency cone.

Consequences worth naming:

- **Order-independence at quiescence.** A conclusion store at fixpoint is
  a function of the corpus, not of traversal order — the property
  level-indexing bought, without the K× price. Mid-fixpoint reads are
  eventually-consistent; the honesty channel already exists
  (`ResolutionSession::mark_degraded` → one `showMessage` per session)
  and "worklist non-empty" is a *sharper* degraded signal than a
  wall-clock budget firing, because it is deterministic.
- **The global epoch retires for this path.** Invalidation is the edge
  walk plus the diff; the additive epoch stays only for whatever memos
  remain. The "170k enrichment-key walks from one didOpen" class of churn
  disappears with the transitive fingerprint key itself — a conclusion
  store is keyed by path, validated by its inputs' diffs, not by a hash
  of the transitive closure.
- **`enrich_open` stops being recursive.** The open-doc pass keeps its
  local half (gated emissions, loader params, TCs, stamp) but every
  cross-file consult reads the conclusion store. Its cost becomes
  local-half + O(consults) lookups — no provider enrichment, ever, on
  the publish path. The `on_refresh` all-open re-enrich sweep becomes
  "re-run open docs whose providers' conclusions diffed", which the
  debounce already approximates by time instead of by data.
- **stamp_method_call_targets gets cheaper for free.** Ancestry walks
  become conclusion lookups (`PackageSymbol{class, name}` is precisely a
  conclusion key), not bag rehydrations. The 63%-of-decodes figure is
  mostly this pass; 3a×3c attacks it from both sides (smaller thing to
  read, fewer times to read it).
- **The candidate-set relation stays.** 5–12 declaring files for a
  package name still means a consult unions 5–12 conclusion sets. That
  multiplier is real-world ambiguity, not an artifact; `ScopedLookup` /
  `@INC` visibility (landed, PR #122) is what narrows it, and conclusions
  must be stored per-file (not per-package) so visibility can filter at
  read time.

#### 3c′. Generational scheduling — the queue-and-flush discipline

How the worklist should run. Instead of draining per-item as edits land,
the conclusion store carries a **generation number**: edits push dirty
files into a queue; a **flush** processes the queued frontier as one
round — derive each dirty file's conclusions against the **frozen gen-N
store**, diff, and the non-empty diffs seed the next round's frontier;
rounds repeat until one diffs empty; then gen N+1 publishes atomically.
Generations are levels *in time*, populated only by what changed —
level-indexing's correctness property (a file's form at a stratum is
independent of who asked) recovered without the K× price, because a
round's cost is the frontier, never the corpus.

What the batching buys, concretely:

- **Round-level determinism.** A per-item queue interleaves reads with
  writes, so intermediate states depend on drain order (same fixpoint,
  different trajectory). Reading a round against a frozen snapshot makes
  every round a pure function of gen N — order-independent per round,
  not merely at quiescence.
- **Structural dedup.** Ten edits to a hub module before a flush are one
  queue entry, and its consumers enqueue once per round, not once per
  keystroke. Today's `on_refresh` storm fix (33 fires → 1) does this by
  time-debounce; generations do it by data. A round can also share one
  `ResolutionSession`/memo and one SQLite pass, which per-item cannot.
- **The epoch retires correctly.** Memos and caches keyed by generation
  stay valid until the next flush *lands* — coarse but accurate,
  versus the additive epoch where any registration mid-cascade
  invalidates every memo (the ~75%-wasted-overlay-builds measurement).
- **An honest, deterministic degraded signal.** "Answers as of gen N,
  k files pending" — where the wall-clock budget fires ~8,000× at
  cpan5k and is reproducible only in practice. Gold and the one-shot
  CLI verbs get their barrier for free: flush until the queue is empty,
  then answer.

Two tiers, matching machinery that already exists: the edited open doc
itself keeps its **eager local** publish (the 150 ms debounce — the
user's own diagnostics can't wait for a cadence), while the consumer
cone rides the **lazy global** flush (settle-triggered via the
`DebouncedLatest` shape, or on idle, or on queue depth). This is the
generational-GC split the store already half-has: open docs are the
young generation (hot, enriched in place), the settled corpus is the
old (conclusions on disk, repaired by flushes), close/settle is
promotion.

One real design choice: **drain-to-quiescence per flush, or
one-round-per-flush.** Draining publishes a fully settled generation
each flush; one-round is lazier and bounded but lets a depth-d chain
take d flushes to settle (staleness ≈ chain depth × flush period — a
few seconds at Koha's depth-12 tail). Default to draining with the
diff-cutoff as the bound (rounds after the first are near-empty), keep
one-round as the degraded mode under sustained load. And one rule: a
consult mid-flush — open doc included — reads gen N like everyone
else, never a half-built N+1.

### 3d. Durable demand-driven query memoization (salsa / red-green)

Make each consult a tracked query: memoize
`conclusion(package, name, arity)` durably, record its input edges, and
on change mark dependents red and revalidate with early cutoff
(rust-analyzer's model). This converges on 3c — same unit (the
conclusion), same cutoff (the diff), same edge store — but inverts the
driver: pull-based revalidation at query time instead of push-based
repair in the background.

Why push (3c) fits this codebase better than pull (3d): the LSP's hot
paths are already "async handlers read `_cached` state, a background
thread repairs it" — a pull-based revalidation cascade lands the repair
cost on the first query after an edit, which is the exact
availability-hole shape row #1 fixed (344 s of `enrich_open` in a
handler). Push keeps queries O(read). A pull framework also wants to own
the whole computation graph (parse → build → enrich) to be sound, which
is a rewrite; 3c reuses the existing builder, freshness index, and
resolver thread as-is. The salsa idea survives in one piece: the
*within-walk* session memo stays, now backed by a store that a walk can
trust because its invalidation is edge-accurate.

## 4. Recommendation

Compose 3a + 3c, staged so each slice is independently gated:

1. **Land conclusion-layer stage 2** (the unit). Already designed and
   gated in #120; C's session is explicitly waiting on the word.
2. **Add the diff** — derive conclusions per file, store keyed by path,
   compare on write. Instrument first (ghost-stat: conclusion diffs
   empty vs non-empty per edit class); the expected result, from the
   Surface experience, is that the overwhelming majority of edits diff
   empty and the chain cuts at depth 1.
3. **Swap the driver** — replace recursive `enriched_snapshot` descent
   with the worklist, scheduled generationally (§3c′): dirty files queue,
   a settle-triggered flush runs rounds against the frozen gen-N store
   and publishes gen N+1 atomically. The
   depth cap and the taint rule become assertions ("never hit"), then
   delete. `enriched_snapshot` and its transitive fingerprint memo
   retire with them.
4. **Point the readers at conclusions** — registry hops
   (`PackageSymbol`, bridges, `SlotType`), stamp's ancestry walk, and
   `enrich_open`'s chase. Keep "absent means decode the bag" as the
   correctness backstop throughout, so partial rollout degrades to
   today's behavior, never to a wrong answer.

Measured targets this should move, for the gate: the 10.7M-consult
`references` (consults become O(candidates × conclusions read), not
O(re-derivations)); Koha depth-12 chains fully enriched with the cap
deleted; warm-sweep compute share (the 13–16%); `enrich_open` p95 on
Bug.pm-sized docs (Round 9's D1 regression); and gold + `--refs-parity`
byte-identical throughout — plus the #155 lesson: every warm lane must
decode through `decode_analysis_parts`, and the warm gold lane is the
net that catches the conclusion column's absent-vs-empty confusions.

## 5. Risks and open questions

- **Eventual consistency semantics.** A query mid-fixpoint reads
  yesterday's conclusion for a not-yet-repaired file. Today's equivalent
  is a budget-truncated or depth-declined answer — strictly less
  predictable. But gold rows must never race the worklist: the CLI's
  one-shot verbs need a "drain the worklist" barrier (the CLI already
  runs enrichment eagerly; the barrier replaces it).
- **Conclusion-store size and residency.** ~2 MB per substrate-sized
  corpus is nothing; 124k needs measurement (est. 20× under the 1.73 GB
  blob store's bag share). It rides the same SQLite + byte-capped LRU
  machinery — but it must be its OWN lane, not a fourth tenant of an
  existing LRU (the two-populations-one-LRU lesson, and PARKED's
  "re-examine on a FOURTH core" tripwire fires here).
- **Diff stability requires fold determinism.** Proven (the 3 HashMap-
  order bugs fixed in PR #123, the seeded-map test for the conclusion
  fold) — but every new conclusion kind must join that test or a
  spurious diff re-runs the cone forever. A "diff non-empty but
  byte-identical re-derivation" tripwire (the fold-64 oscillation
  detector, one tier up) is cheap insurance.
- **Plugin priority and enrichment sources.** Conclusions must carry
  `WitnessSource` priority (Plugin=100) so `PluginOverrideReducer`
  semantics survive the bake; the imports-as-methods design (#120) adds
  a precedence tier on the same axis — design them together or the
  second one re-litigates the first.
- **Timeline (temporal) conclusions.** 28.5% of chase reads are
  `Observation`s at `Variable` attachments. Point-free-ness makes them
  non-persistable *for closed providers* — verify the open-doc path
  (which does have points) never reads a provider timeline through the
  conclusion store, or `Timeline` needs a real representation.
- **What this does not fix.** Capacity thrash on the *bag/blob* lanes
  (Round 9's warm-slower-than-cold inversion) is a residency problem,
  orthogonal; the watcher's whole-copy pinning residual stands; and the
  one named hang (`Advanced-Config` 13-alt-get-tests.t, 4,000×
  in-corpus vs standalone) is unexplained and may be a different animal
  — attribute it before assuming this arc absorbs it.
