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

#### 3c′-v0. The driver's first slice: the background re-bake queue, not lazy consult-time repair

The fingerprint-clear gap (conclusions wiped, blobs kept, nothing
re-bakes; the layer dark until a manual full clear) gets the driver's
minimal form, NOT a lazy bake-on-consult. Three reasons the lazy form
loses:

1. **It pays the bake at the worst time.** A consult-time bake needs the
   whole analysis, so a consult on an evicted file pays a decode PLUS a
   bake where today it pays only the decode — strictly worse until the
   map exists, on the query path, which is the availability-hole shape
   (no synchronous CPU where answers are owed).
2. **It converges only over consulted keys**, so the measurement trap it
   exists to fix survives mid-convergence: a run after a rebuild still
   measures a partially-empty layer, just less predictably.
3. **It makes readers writers.** The consult path is read-only by
   architecture (async handlers zero I/O; writes live on the
   resolver/persist threads). A lazy bake breaks that seam for a repair
   the background can do trivially.

The v0 queue: on fingerprint mismatch (startup) or clear, enumerate
files with a valid blob and no valid conclusion row, enqueue, and drain
on the persist writer in chunks (`write_in_chunks` discipline) — each
item is decode-parts → `bake_full` → store. No diffing, no propagation,
no generation semantics beyond what the store already stamps: this
slice's whole contract is "the layer never stays dark," and the full
§3c′ driver grows out of the same queue later. Gate:
`conclcache.known_absent` returns to ~0 after a source edit + restart
with no `--clear-cache`; warm-ready wall unmoved (the work is
post-ready background); bar green.

#### 3c″. The diff must be on EVALUATED conclusions — the index-free map is not the cutoff artifact

Stage 2 as built bakes each file's map **index-free** (deliberately —
"edges, not values": a materialized cross-file value would freeze a
world that can change without this file changing). That is the right
*persisted* unit, and it is NOT the artifact §3c's diff may cut on. An
index-free map captures only local conclusions: when C's map changes,
B's map — local-only by construction — does not change, even though B's
*answers* (which chase through to C) do. A propagation that cuts on
index-free map diffs therefore stops at B and never reaches B's
consumers. That is the closed Surface-freshness unsoundness
(`skipping-cross-file-work.md` item 3) reproduced one level up:
a local projection diffed as if it were a global conclusion.

So the driver's diffed artifact is the **evaluated export surface** —
each exported key's answer evaluated against the current store
(following `Link`s, applying the absence rules) — recomputed when a
provider in the file's closure re-enters the worklist and diffed
against the previously evaluated set. The index-free map stays what is
persisted and invalidated by the fingerprint; the evaluated surface is
cheap (map lookups, no decodes) and is what makes the cutoff sound
transitively: C's change diffs C's evaluated surface, which dirties B,
whose evaluated surface diffs (through the Link/consult), which
dirties A — or doesn't, and the chain cuts *there*, correctly.

This also answers the re-stamp question the sweep measurements raised:
**94% of `stamp_method_call_targets` re-evaluations consult the index
and change nothing (`crossfile_and_stable`), and no sound gate for
them exists today** — unconditional skip is 0.43% silent divergence
(closed item 1), local-ancestry covers 2.9% and shrinks with corpus
(closed item 2), Surface freshness is unsound (closed item 3). The
sound gate is exactly this driver: stamp each file's frozen
`MethodTarget`s with the conclusion-store generation they were derived
under; a re-stamp is owed only to files a diffed evaluated surface
reached through the worklist. Until the driver exists, the honest
statement is that the 94% is not skippable — which is the fourth
independent motivation for building it, joining the fingerprint-clear
re-bake defect, the enrichment-recursion replacement, and the
generational flush itself.

**The flush's two products, disentangled** (a build-time sharpening of
this section's original phrasing, which conflated them). Because the
bake is index-free, a downstream change moves a consumer's *answers*
without moving its *map* — so the wave costs one map decode per reached
file, never a blob decode or re-bake, and the seeds' fresh maps are
SUPPLIED BY THE CALLER (who just built the analyses; making the flush
decode a blob to re-derive a map already in RAM is this layer's own
antipattern — and at the save seam that blob was invalidated a moment
earlier, so there is nothing to decode). A deleted file seeds as its
direct consumers, not as itself: it has no map and nothing to evaluate,
while its consumers, resolving through a file the store has forgotten,
are a real move with real evidence. Two distinct products: the
**refresh set** (`changed` — files whose evaluated answers moved; what
enrichment, diagnostics, and the re-stamp gate consume) and the
**generation** (the consistency clock). At the save seam the wave's
persistent write set is **empty** — publishing even the seeds' rows is
deferred, because their `modules` rows were just invalidated and a
conclusion row with no `modules` row behind it can never be caught by
the stamp check (the stamp lives on the `modules` row): writing one
re-opens the bake-outlived-blob hole by a different door. Publication
resumes when the conclusions table carries its OWN freshness stamp —
and that stamp is the generation clock's natural next customer, joining
torn-read prevention over multi-seed row writes and the deterministic
degraded signal ("answers as of gen N, k pending"). The re-stamp gate
is NOT a store-generation customer: with publication deferred the store
generation never advances, so a gate keyed to it would be dead by
construction, and the deeper rule is that a comparison clock must share
its LIFETIME with the operands it orders — the gate's stamp and marks
are both sessional, so its clock (`flush_epoch`, sessional, on
`IndexCore`) is too. The gate migrates to the store generation only
if the marks ever persist, which the coupled-halves invariant says
happens for both halves together or not at all. The
invariant that makes caller-supplied, index-free maps sound — a bake
can never produce a value that depended on another file — is silent in
its violation (a bake that learned to consult an index would quietly
stop refreshing consumers), so the driver ships with its own
equivalence switch re-baking reached files and comparing, the same
discipline one tier down.

**The gate, specified (push, not pull).** Do NOT compute `providers(F)`
at check time — that resurrects the transitive-closure walk the
enrichment-key memo existed to contain. Invert it: the FLUSH marks.
When the worklist enqueues consumer F (a provider's evaluated surface
diffed), it also bumps a store-side `last_provider_diff_gen[F]`. The
file carries `stamp_generation` (one u64, set when
`stamp_method_call_targets` last ran with the index). The gate is O(1):
`stamp_generation >= last_provider_diff_gen[F]` ⇒ skip the re-stamp;
anything else — never stamped, no recorded mark, post-clear wipe —
**fails open to today's behavior**. Three consequences: no sequencing
dependency (before the flush is the standing path the mark is never
written and the gate fails open everywhere, so it lands WITH the
store-wiring slice, not after it); new-file and deleted-file
candidate-set changes are covered because registration and removal
already route through `record_and_dirty` → `dirty_consumers` → the
enqueue that writes the mark — the gate is exactly as sound as the
freshness edge coverage, no more; and it is EQUIV-scored from the first
commit, because the protected population is the 0.43%
freeze-divergence class whose silent drift is why the unconditional
skip was rejected.

Three sharpenings from the build (all of the silent-failure class the
spec's EQUIV requirement exists to catch). The mark covers the
**enqueued** set, not the changed set: a consumer whose own conclusion
answers did not move can still *dispatch* differently once its provider
changed, because a `MethodTarget` resolves through the index rather
than off a surface — marking only movers would leave exactly those
frozen stamps answering forever, so `FlushOutcome` carries both sets
distinctly. The stamp is an `Option`, never a 0-sentinel: generation 0
is a real clock reading (every stamp taken before the first flush), and
mapping "never stamped" onto it makes a pre-flush stamp compare equal
to the first wave's mark — postdating it — and skip a re-stamp it was
owed. And the two halves are sessional TOGETHER (`serde(skip)` stamp,
in-RAM marks): persisting either half without the other compares a real
value against a lost one and skips owed work, so neither may outlive
the other. The clock bumps before the marks land, which is sound
because the caller registers the changed files' fresh analyses before
it marks — a stamp reading the new clock also sees the new provider.

One verb-scoped carve-out to the 94% figure, established by ablation
after this section was written: for `--check` specifically the entire
re-stamp is **dead computation** — diagnostics never read
`method_target()` (its lanes type invocants via the bag and resolve
methods directly), and skipping the re-stamp leaves the diagnostic set
identical at two corpus scales. That skip is sound *by construction*
(a verb not consuming a product need not pay for it), which is a
different and stronger kind of soundness than any freshness gate — it
is `LanguageScope`'s shape applied to enrichment: a verb-declared
enrichment profile, never a `--check` special case. The binding
constraint: the server's `enriched_snapshot` overlay is shared and
fingerprint-keyed, so a profile-partial copy must never be served to a
verb with a fuller profile — the profile joins the overlay key, or the
profile stays one-shot-CLI-only. The generational gate in this section
remains the answer for the verbs that DO read targets.

Landed CLI-only as ruled: **~5% of wall** against the parallelized
sweep (the first-published 13–15% was measured against the serial sweep
that no longer exists — the parallel rebase re-measure at n=8
interleaved is the number of record; set identity re-verified at
N=2,431). The per-thread accumulating phase region summed to 2.5× the
wall it sits inside after parallelization — such a region is usable as
a same-thread-count A/B ratio and never as a cost. One rule the build earned:
**the A/B control must be reachable from the shipped configuration** —
the ablation flag skips in the SAME direction as the profile, so once
the verb declares a partial profile the full behavior is unreachable and
"set-identical" becomes unfalsifiable; the control is a
force-full-enrichment override whose precedence over a declared profile
is unit-tested, because getting the precedence backwards fails nothing
and quietly disables the control.

One adjacent lever the miss-cost data licenses separately: a failed
resolution costs 32× a hit because it exhausts the candidate space,
and that space is the corpus because workspace-tier Perl passes
visibility unscoped. `VisibilityAxis`/`ScopedLookup` narrowing (the
@INC tier already landed; the workspace tier's slot is still empty)
shrinks what a miss must exhaust, independently of any conclusion
machinery.

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

## 6. Follow-up specs (post-#161)

Three items, in the order they should be built. The first turns the
landed machinery into the measured win; the second resumes publication
and closes the last declared coverage gap by construction; the third is
optional perf with a one-rule design.

### 6a. Activation — make the flush and the gate run where providers change

The driver and the gate are wired to `did_save` and the watched-files
seam (one shared body, `reindex_saved_perl`). What's left is coverage
and evidence, not design:

- **Every event that (re)registers a provider analysis marks and
  flushes**: the rule is "anything routing `record_and_dirty` →
  `dirty_consumers`" — that already includes new-file registration and
  deletion. The one path needing care is **bulk/workspace re-index**:
  never flush per-file during a bulk (the H9-2 defer/reconcile shape);
  enqueue throughout, one flush at end-of-bulk drain.
- **The win is read off counters, nothing else**: a real editing run
  (edit-bench scenario over the substrate or Koha) must show
  `restamp.skipped > 0` with `PERL_LSP_RESTAMP_EQUIV=1` reporting zero
  disagreements, and `PERL_LSP_FLUSH_EQUIV` clean. Both switches have
  had no corpus exercise; the first activated run is their first real
  test and should say so when reporting numbers.

### 6b. SPEC 3 — resume publication: the self-validating conclusions row

The no-publish ruling exists because validity currently lives on the
WRONG row: the stamp check is on `modules`, so a conclusion row with no
`modules` row behind it is invisible to the eraser. The fix is to make
each conclusions row carry its own validity, checked at consult:

- **Row stamp = `(source_fingerprint, flush_generation)`.**
  `source_fingerprint` is the content fingerprint of the analysis the
  map was baked from — the same value `FreshnessIndex` records.
  `flush_generation` is the store generation the row was written under
  (the persistent clock's fourth customer, per the customer list).
- **Validity is content-keyed, decided at consult.** `conclusions_for`
  returns the map only when `row.source_fingerprint` equals the
  fingerprint the index currently records for that path (never re-hash
  at consult time — consults are hot; no freshness record → absent).
  Anything else — an orphaned row whose `modules` row was erased, a
  stale row for an edited file, a row from an interrupted write — fails
  the SAME compare and reads as absent, and absent falls back to the
  live chase. Correctness stops depending on any caller remembering an
  eraser; `invalidate_derived_copies` stays for space and hygiene only.
  **This closes the "unit evidence only" gap by construction**: the
  wrong answer that could not be demonstrated becomes unreadable
  rather than unprovoked.
- **Torn-read prevention is a session pin, not a lock.** Each flush
  publishes all its seeds' rows in ONE transaction. The remaining
  hazard is a chase that consumed a pre-flush row and then reads a
  post-flush row: the reader records the generation of the first row it
  consumes in a `ResolutionSession` and treats any row with a DIFFERENT
  generation as absent for the rest of that session (absent → live
  chase; never a wrong answer, at worst a slower one).
- **Exactly two writers, both single-txn**: the persist writer (blob +
  map + stamps in the existing chunk txn) and the flush publish (seeds'
  fresh maps, stamped with each seed's fingerprint + gen N+1). The
  persist-mid-flush ownership rule stands as ruled: queue as next-flush
  seed or admit as a late seed under the N+1 stamp discipline; never
  write-in-place at the current generation.
- **Schema/versioning**: bump the conclusions-lane version;
  wipe+re-bake on mismatch (the `REF_ROWS_VERSION` policy — never a
  blob drop), with a shape probe so a lying stamp still rebuilds. The
  labelled dead code (`save_conclusions` / `publish_generation` /
  `prune_generations_below`) revives here; prune keys on generation.
- Keep `PERL_LSP_CONCL_EQUIV` on for the first corpus run with
  publication live — same first-exercise honesty as 6a.

### 6c. Profile lattice — SPEC 2's server-side rule

`EnrichmentProfile` is a lattice (two points today: `Diagnostics <
Full`). The `enriched_snapshot` overlay entry records the profile it
was enriched UNDER; a lookup for profile P accepts an entry whose
profile ≥ P; a request fuller than what's cached re-enriches and
REPLACES the entry (profile is a field on the fingerprint-keyed entry,
not part of the key — no double-caching, upgrade-in-place). The ≥ rule
is the entire guarantee that a partial copy is never served to a
fuller verb. Verbs declare their profile the way they declare
`LanguageScope`; the enricher never asks what query it serves. Can
land CLI-first exactly as the `--check` skip did; nothing else blocks
on it.

### 6d. Measurement addendum: the corrected A/B protocol, and the first wall-time validation

Independent review on #161 established two things every future number on
this arc must respect:

- **A bake-steering flag must never be toggled against a shared
  cache.** The full mechanism, established across two corrections:
  `PERL_LSP_NO_BAKE` is a `BAKE_STEERING_FLAGS` member, so toggling it
  changes the derivation fingerprint and WIPES the conclusions lane —
  under interleaving, both arms measure an EMPTY layer, not a baked
  one. And the flag originally gated only one of TWO producers: the
  background `repair_conclusions_slice` re-baked the corpus after each
  "disabled" run, so the row census read full-and-correct while the
  control controlled nothing (fixed: one `bake_disabled()` speller,
  layering-test-pinned to a single reader). **Protocol: batch all runs
  of one arm in its OWN cache directory; per-arm clearing is necessary
  but not sufficient while any producer ignores the flag. "Is the fill
  disabled?" is a question about the SET OF WRITERS, not about the
  flag.** Any A/B flag added by §6 work (the EQUIV switches included)
  needs its semantics verified the same way. A correction to this
  section's earlier claim: the consult-count reductions recorded in
  this document were measured under `PERL_LSP_NO_NOT_LOCAL` — a
  consult-side gate on maps both arms read identically — so they are
  NOT affected by the fill-side flaw and are neither understated nor
  lower bounds; the fill-side `MINT_LINKS` A/B self-discriminates via
  `baked_follow_incomplete` and was re-primed per arm, so it stands.
- **First wall-time validation, corrected protocol, real codebases:**
  8.4% (6,009-file app+plugins, complete separation across runs) and
  9.3% (3,554-file app, one overlapping pair — treat as the weaker
  figure), diagnostics set-identical throughout — and after the
  second-producer fix these are a FLOOR (the OFF arms' later runs were
  repair-baked, i.e. too fast). The honest substrate number under
  per-arm caches: **−20.5% of the chase, +6% wall** — ON slower in 5
  of 6 paired runs, because installed CPAN's chases are cheap and
  removing a fifth of them does not repay the map loads. Nobody quotes
  the substrate for this layer in either direction; corpus SHAPE
  dominates file count (the 3.5k app is slower than the 6k one). The
  138k-scale run on the steep part of the failed-lookup cost curve
  remains the open measurement.
- `ScopedNs` regions accumulate per-thread and nest (measured 31×
  over-report serially, worse under rayon): a region total is never a
  wall claim. Counts that cannot nest are the trustworthy headline.

### 6e. Activation findings and rulings (the three blocks)

The 6a activation report established that `restamp.skipped > 0` is
currently unreachable, via three independent measured blocks. Rulings:

- **Block 1 — bulk marks are structurally FirstSeen** (one bulk per
  session, at startup, into an empty in-memory `FreshnessIndex`; a
  one-shot CLI has no prior surface at all). Accepted as a fact of the
  current lifecycle, not a bug: the routing fix (bind the verdict,
  accumulate Changed paths, mark once at drain, mark-not-wave since a
  bulk has no before-state) is correct and inert until the index
  survives longer than one bulk — which is 6b's persistence story, not
  6a's.
- **Block 2 — the overlay lane can never skip, CORRECTLY.** Overlay
  derivations clone the BASE analysis (`stamped_at: None`, refs
  carrying build-time targets); a skip there would serve un-enriched
  targets. The gate's premise does not hold for that population, and
  the recorded WRONG-fix stands recorded: moving the stamp into the
  index (a `stamped_gen` map) would satisfy the clock-lifetime rule
  and still be wrong, because the index would claim "stamped" for a
  copy whose refs never were. The stamp belongs on the copy.
- **Block 3 — `dirty_consumers` empty for a saved provider with a
  live open consumer is a BUG, not expected shape.** The engine was
  built to carry Perl edges: `FreshnessIndex::record` maintains
  name-keyed consumer edges from `dep_names` = uses ∪ parents ∪
  plugin_bridges, and `dirty_consumers` walks them transitively. An
  empty answer means one of two coverage holes, distinguishable by one
  counter (did `surfaces.get(seed)` hit, and was the frontier
  non-empty): (a) the CONSUMER's record never landed — the open-doc
  record's single call site is the debounced diagnostics refresh,
  which fires on change, so an opened-but-unedited consumer may never
  record (didOpen must record, or the bulk record must cover it and
  survive); or (b) PATH IDENTITY — `record` keys `surfaces`/
  `consumers` by the caller's spelling while the
  `registration.rs::dirty_consumers` wrapper canonicalizes the seed;
  one canonicalization speller at the record boundary, or none
  anywhere, never a mix. The fix belongs in the freshness engine's
  record boundary, not in the wave.
- **The gate stays.** Its addressable population, post-block-2, is
  exactly what the eager stamp was built for: OPEN docs re-enriched
  without their own rebuild — the blanket `republish_open_docs_in`
  storm (every open doc re-enriches when any file's surface changes),
  the resolver refresh callback, and the cold-open heal. With N open
  docs, one provider change pays N re-enrichments today and the gate
  skips the non-consumers' re-stamps once block 3 is fixed. 6b's
  publication also grows the index-served share, raising the relative
  value of each skipped re-stamp. Inert-and-honest until then.

### 6f. RULING — the conclusions-row stamp source: persist the Surface

The contested call site fired (`docs/prompt-surface-projection-drift.md`):
`Surface::project` reads the witness bag, the warm lane projects from
bag-EVICTED copies, so one file fingerprints differently depending on
which lane recorded it — 76.7% of conclusion rows rejected (correctly,
given the compare), a 3.3× cost on the layer, and the real substrate
bake-ON number is −75.9% of fetches (17,419 vs 72,305), superseding the
crippled-layer −20.5%.

**Ruling: option (1), persist the Surface.** The warm lane READS the
persisted projection instead of re-projecting from a degraded copy.

- The decisive argument is not the stamp — it is that (1) is the only
  option that also fixes the **warm-start freshness verdict**: today an
  edit changing only bag-derived content compares equal to a degraded
  baseline, reads `Unchanged`, and no consumer re-enriches — the
  loader-shapes failure arriving through the residency door. Option
  (2) (stamp on bytes/mtime) answers the literal stamp question and
  knowingly leaves that wrong-answer-shaped hole; rejected for that
  reason, not for cost.
- Structurally this is the one-speller rule applied to an artifact:
  the drift exists because there are TWO producers of "this file's
  Surface." (1) collapses them to one — the projection is computed
  once, on the persist path, from the WHOLE analysis (which the
  reads-whole-before-evict discipline already guarantees is in hand:
  `prepare_*_parts` project pre-strip today). Precedent: the pack
  `stubs` lane. Additive migration per the build-time finding: a
  sibling `surfaces` table with its own version gate, no
  `SCHEMA_VERSION` bump, no cache-wide rebuild.
- Ownership follows the stubs discipline exactly: written by the
  persist writer in the same chunk txn; any `modules`-row rewrite
  deletes the path's surfaces row (inside the write helpers — writers
  can't forget); hard-clears wipe it with the derived rows.
- **Decline lane**: a missing/version-declined surfaces row on warm
  falls back to a point whole-decode and projects from the whole copy
  (backfilling the row, as the stub decline lane does). If even that
  fails, record NOTHING — absence of a record is `FirstSeen`, which
  fails open; recording a degraded projection is the bug and is never
  the fallback.
- **Enforcement instead of option (3)**: `Surface::project` gains a
  debug assertion that the bag is present. That makes a future
  degraded projection loud at the source, buying (3)'s guarantee
  ("a degraded projection is impossible") without (3)'s rebuild of the
  projection itself — and with the warm lane no longer projecting at
  all, the assert has no legitimate trigger. The narrow
  `index_perl.rs` loader-shapes rehydration workaround is subsumed and
  deleted when this lands.
- Pin the reduced unit property as a test: build→project→fingerprint
  equals the persisted-surface fingerprint after evict+rehydrate; it
  is the anti-drift tripwire for every future Surface field.

Also ratified from the 6b/6c builds: the `FreshBake` one-value coupling
(path+map+fingerprint describe one state; never read the fingerprint
back from the index at write time), the generation pin making pruning
safe (a lost generation degrades to a decode, never a wrong answer),
and 6c's `ResolutionSession::declare_profile` resolution
(session → process cell → `full()`; the process `OnceLock` stays
CLI-only for exactly the reason its doc comment states). The server
diagnostics profile stays unwired until the ablation over the server
path produces the same evidence the `--check` ablation did — that
measurement is green-lit.

### 6g. RULING — the open-path suppression predicate: (C), landing (B) first

The 6a edge was an ordering race, not an empty graph: `record_surface_write`
suppresses Background records keyed on "a doc is OPEN at this path" rather
than "an open-doc RECORD exists for this path", and the open-doc lane's
only write site fires 150 ms after a *change* — so between `didOpen` and
the first debounce the path has no record at all, declares no deps, and
the flush marks nobody. A Background write in that window also returns
`Unchanged` for a file the index has never seen — a false verdict in its
own right.

**Ruling: (C) — both fixes, (B) first.**

- **(B) is the invariant.** The suppression's purpose (protect the
  open-doc baseline so a buffer edit reverting to disk state cannot read
  `Unchanged` against a disk-derived record) only has meaning when an
  open-doc record EXISTS to protect. Before one exists there is no
  baseline to clobber, the disk state is the only truth available, and
  the current rule yields to a writer that does not exist. Predicate
  becomes "an OpenDoc-lane write has recorded this path"; suppression
  resumes the moment the first open-doc record lands, so the protected
  scenario is untouched. The false verdict fixes with it: a Background
  write that actually lands on a never-recorded path returns its true
  verdict (`FirstSeen`); `Unchanged` remains correct only for genuinely
  suppressed writes, where a record exists and did not move.
- **(A) is the latency.** Record `Document::baseline_surface` at
  `didOpen` — it is the exact value the architecture already designates
  as the open doc's freshness record (build-time, pre-enrichment, so
  surface verdicts stay enrichment-invariant), and it exists at open
  time. The consumer edge is then live from the moment the file opens
  instead of from its first post-change debounce.
- Neither subsumes the other, as the build report stated: (A) without
  (B) leaves the verdict lie for any future not-yet-recorded lane; (B)
  without (A) leaves the edge absent until a Background write happens
  by. The isolated no-e2e probe (background verdict on a never-recorded
  open path) becomes the pinned regression test.

Option (1)'s landed results are recorded with the ruling that ordered
them: stale rejections 47,967 → 0, `surface.reprojected` 0, stamp
residue 8.6% over bypass (all of it `unrecorded` fail-open), one-run
self-healing upgrade (~5.09 s vs ~4.7 s settled at 3,515 files). The
cold-regression dispute on the integration tip is the coordinator's
call. The corpus question is answered by the maintainer: the non-`.pm`
files are `.t` — ALL Perl, no pack lane (this section's earlier
pack-lane speculation is retracted). Which sharpens the shape
hypothesis instead: `.t` files are package-less `main` scripts with
heavy `use` lists, so that corpus provides ONE package name (`main`)
from ~6,748 files — the extreme point of the many-providers-per-name
relation (root cause 3 in §1), far beyond the 1,300×10 duplication
probe, plus thousands of consumer-heavy dep lists. The repro generator
wants that second population: unique-package `.pm` providers plus
thousands of `main`-package `.t` consumers using them.

### 6h. The cold-regression attribution, the loader ruling, and PackageHome

**Attribution (survived the SHA check the first mechanism failed):** the
`ConclusionCache` miss loader (`module_index/queries.rs`) opens a FRESH
SQLite reader via `open_reader_retrying` plus a `current_generation`
read on EVERY cache miss — ~5.1 ms/miss × ~70k misses at n800 = the
regression. Arrived with #161; the earlier "stale-rejection" mechanism
was impossible on the tip (no stamp exists there) and is withdrawn on
the record.

**Ruling on the seam — it was never given a per-thread reader, and no
design reason forbids one.** The fix has two independent halves:

- **Reader reuse**: a thread-local `Connection` memo keyed by
  `(db_path, wipe-generation)` — the exact discipline `rows.rs`'s
  intern memo already uses (`meta.strings_generation`-keyed,
  clear-on-bump), because a held connection must not survive a
  hard-clear that unlinks the DB file (the open handle would keep
  reading the old inode — a silent stale-reads hole, the same class as
  the stale `str_id`). Bounded by thread count (rayon workers + the
  resolver thread), read-only, WAL — safe by construction.
- **The generation read moves to the session**: `ResolutionSession`
  already IS the generation-pin carrier; read `current_generation`
  once per session and pass it to the loader instead of asking SQLite
  per miss. A stale session generation fails in the safe direction by
  the pin's own semantics (newer-generation rows read absent → decode
  fallback — slower, never wrong).

The remaining open question (tip ≥1296 s vs branch-head 138 s, loader
present in BOTH) is a `conclcache.miss` COUNT comparison at the two
SHAs — the self-validating rows and the generation pin are the
candidates for why the branch misses less; that measurement belongs to
whoever holds the n800 corpus.

**PackageHome (the "identity needs a home, not a name" brief) —
endorsed with two constraints.** The shape is rule #10 done right: the
property ("who can NAME this package") lives on the identity, and
`main` stops being special because nothing asks about names. The
constraints:

1. **The home decision derives from `VisibilityAxis` — one speller,
   both directions.** The forward map (name→paths, `resolve_module_
   paths` + the asker's `use lib` roots) and the inverse (path→names a
   root spells) must read the SAME root set or they drift into exactly
   the two-producers bug this arc just paid for twice. And because
   `use lib` roots are per-asker while `home` is decided at
   registration, addressability is not absolute: decide against the
   UNION of known roots, **fail toward `Global`** (a wrongly-Global
   package keeps today's behavior; a wrongly-FileLocal one loses
   answers, violating the brief's own gate), and make promotions
   MONOTONE — a late-arriving `use lib` root promotes FileLocal →
   Global and re-registers, never the reverse.
2. **Sequencing**: the brief's own "sizing, stated as unknown" is now
   answered in the large by the attribution above — the fan-out cost
   is second-order against 5 ms/miss on the loader. Loader fix first,
   then re-measure the FileLocal-attributable fetch share before
   building; the brief's validation gates (bucket distribution,
   consult density must not collapse, narrowing-only answer changes)
   are all correct as written.

**§6h addendum — PackageHome DROPPED on measurement; endorsement
superseded.** The sizing gate did its job: `main`-targeted fetches are
0.157% of PackageSymbol-primary volume, and the premise was false in
the running system — the walked `main` provider set averages ~49, not
~6,631, because implicit-`main` scripts never enter
`visible_def_candidates`. Five soundness holes beyond that, the fatal
one being companion packages (1,431 declarations whose path does not
spell them; loading a file makes ALL its packages addressable, so
reachability is a property of the FILE, not the package — deeper than
the fail-toward-Global constraint could patch). The replacement
direction is the one this doc's constraint pointed at but the drop
states correctly: not a second identity axis at all —
**origin-scoped visibility over the provider set**, on
`VisibilityAxis`, which also covers the measured real hot spot
(vendored `inc/Module/Install` copies, 50 providers of one name, all
Global under the dropped design). Separately: the parked `Link`
verdict (§3a) is under re-measurement — a `.t`-heavy real-CPAN corpus
shows `no_answer_linkable` at 49.7% of open reasons vs the substrate
prior of nothing-to-convert; the A/B scores follow COMPLETION (not
decode count) per-arm-cached, and its incomplete-rate decides whether
world-level closedness is the prerequisite lever or the sibling one.

**§6h addendum 2 — reconciling `41ead841` (inode recheck) with the
wipe-generation key.** Ruling: **the inode recheck is sufficient for
THIS seam, and on one axis it is stronger than the key I specified —
but its sufficiency is a property of 6b's rows, not of the connection
pattern, and that dependency must be stated at the site.**

- The TOCTOU window is real (recheck passes, a concurrent hard-clear
  unlinks+recreates, the query reads the old inode) but post-6b it can
  serve only CORRECT answers: the bake is deterministic (the
  seeded-map gate), and every row self-validates — content fingerprint
  against the live `FreshnessIndex`, per-row derivation-version gate,
  and the session generation pin. Any old-inode row that survives all
  three is byte-equivalent to what a fresh re-bake would produce, so
  the window degrades to "served the same answer from the old file",
  and the very next miss rechecks and reopens. A happens-before
  (generation bump) is not needed where the data proves itself.
- The recheck is WIDER than my key on the axis that matters most: a
  process-local wipe-generation counter is blind to an
  OUT-of-process `--clear-cache` (a separate CLI process unlinking the
  DB under a running server), which the stat sees immediately. My
  spec missed that case; the engineer's mechanism covers it.
- **The boundary that must be written at the site**: this sufficiency
  argument belongs to the conclusions lane's self-validating rows. The
  retained connection must NOT be reused for lanes whose reads do not
  self-validate (the blob store, the refs/syms rows) — there the
  TOCTOU window serves genuinely stale data with no gate to catch it,
  and the wipe-generation key (or no retention at all) remains the
  requirement. One comment stating "this recheck is sound because
  every row read through it self-validates" turns a future
  reuse-this-connection change into a tripwire instead of a hole.
- Item 2 stands open and composes: the generation read moves onto
  `ResolutionSession` (once per session, feeding the pin) regardless —
  the retained connection makes the per-miss read cheaper, not free,
  and the pin wants one consistent generation per walk anyway.

### 6i. RULINGS — the sequencing verdict, the pin, and the #162 carry-forwards

**Sequencing (the §3a re-rule, on the real-code numbers):** follow
completion 1.20% (koha) / 0.00% (6k app); `no_answer_linkable` is 1.5%
of open reasons on real code (the dist pile's 49.7% was a property of
that corpus); `self_only` — which minting cannot help — is 79.9%.
Per the pre-stated criterion: **world-level closedness is the
PREREQUISITE lever, not a sibling.** `Link` stays parked for real-code
serving; if the still-running dist-pile arm shows completion there, it
re-opens as a lever WITH A STATED DOMAIN (dependency trees), never as
a general win. The flush stamping ancestry-closedness classes is the
next Link-adjacent work, not more minting. Corollary accepted: the
vendored `inc/Module/Install` hot spot cited for origin-scoped
visibility is a dist-pile population (zero copies in either real
codebase) — that framing's real-code justification is the CORRECTNESS
bug (scripts bleeding into each other's answers), not the perf case,
and must say so.

**The generation pin: compare FINGERPRINTS, not generations.** The
reviewer's fragmentation finding is real — seeds-only publication
fragments the generation space, and a walk pinning its first row's
generation goes dark for the rest of the corpus, nondeterministically
by consult order. That is wall-clock-budget-shaped nondeterminism, the
class this arc exists to delete. The fix is not republishing the
reached cone (write amplification the index-free bake exists to
avoid); it is recognizing that the pin's job is already done by the
rows: given the register-before-mark ordering (the customer-2
bump-first argument, already load-bearing), a row passes its
content-fingerprint check only against the world the index currently
believes, and the deterministic bake makes any passing row unique — so
per-row fingerprint validity IS the torn-read protection, and the
generation comparison at admission adds only false darkness.
Generation demotes to bookkeeping: the degraded signal, audit, and
per-path supersede-pruning. (The PR's warm numbers predate any flush;
re-measure after this lands.)

**Frontier detects absence, never mismatch** (finding 2): with
fingerprint-at-consult, a mismatched row already reads absent at the
consult — close the loop by having that consult-side rejection ENQUEUE
the path for repair (push-shaped, one line at the rejection site),
rather than teaching the frontier query a fingerprint join. Residual
drift then self-heals through the same lane as absence.

**No eraser deletes a `surfaces` row** (finding 3): fold `surfaces`
into the derived-erasers set — `invalidate_generation`'s conditional
drop, `clear_derived_rows`, both hard clears — the same one-eraser
discipline as `invalidate_derived_copies`; the delete-on-modules-
rewrite half already exists via the `delete_stub` pattern and the
#162 delete-first fix.

Also for the flag-semantics tally: `mint_links_enabled()` tests
`is_ok()`, so `PERL_LSP_MINT_LINKS=0` turns minting ON — a baseline
written the obvious way measures ON vs ON. Same family as the NO_BAKE
half-gates (both now fixed: frontier seeding AND the executor's
`blob.is_empty()` early return). Every boolean env gate wants a
truthiness rule and a doc line saying which it is.

**§6i addendum — the Link question is CLOSED; visibility gains a
constraint.** The dist-pile arm answered the domain condition: the
linkable distribution replicates (52.7%) but follow completion is
**0%** — 1,260,943 minted follows, all abandoned, +77% wall, provider
fetches byte-identical. So `Link` does not re-open in any domain.
Final state: `PERL_LSP_MINT_LINKS` stays off ON EVIDENCE from both
regimes; **closedness is not the lever that improves Link — it is the
lever without which Link has no value at all**, and re-running this
A/B after closedness lands is the gate: `baked_follow` still 0 ⇒
minting is DEAD, not parked. Closedness's own justification is
independent and sufficient: `absent_not_closed` is 27.6% of open
reasons and 96.8% wasted (~640k no-answer decodes per n300 run that
closed-form ancestry converts directly).

The FHEM finding adds the constraint the origin-scoped-visibility
design must carry: 534 of 614 `.pm` files legitimately share `package
main` (do-loaded into one interpreter — they genuinely share one
stash), so **whether cross-file `main` visibility is a bug is itself
origin-relative**: a koha `.t` should not see another `.t`'s `main`;
an FHEM module MUST see its siblings'. The axis therefore derives from
actual load relations (`do`/`require` edges), never from file kind or
path shape — which is one more reason it is a `VisibilityAxis`
variant and could never have been an identity bit. Also adopted into
the measurement rules: state whether a counter is ATTEMPTS or
COMPLETIONS and reconcile against its sibling (`moc.provider_fetched`
is attempts; 12.3M attempts reconciled to 7,200 loader completions
three lines away in the same dump).

**§6i addendum 2 — all rulings LANDED (`db79e452`); the arc's machinery
is live end-to-end.** The pin is gone (fingerprint is the whole
admission; generation demoted to audit skew-mark + supersede-pruning,
whose safety argument now rests on the fingerprint — a reader losing an
older row finds a same-content newer one or decodes, so retention buys
speed alone: a strictly better argument than the pin's). `surfaces`
joined the erasers as one eraser with the map (`forget_orphaned_
derivations`; both hard clears via `clear_derived_rows`). Mismatch
repair is push-shaped with a PATH-KEYED SET (tens of thousands of
rejections per sweep collapse to one repair; adoption at the repair
pass is latency-not-correctness, bounded at one entry per stale file).
6a's (B)-then-(A) landed and the gate fires: `restamp.skipped` 2,
`restamp.marked` 1, EQUIV clean — block 3 was the bug, blocks 1–2
stand. One test changed verdict (`close_reconciles_the_disk_record`,
FirstSeen→Changed) and its own comment proves it was pinning the
suppression bug; assertion rewritten, not the code. Remaining: the
warm re-measure (stale twice over — pre-flush AND pre-pin-removal),
and the residual cold cost, which no corpus C holds can see — that
ablation belongs on a box whose real corpora reach `conclusions_for`
cold, not on a fourth synthetic corpus.

### 6j. SPEC 4 — world-level closedness: the self-validating certificate

**Blocker status, first**: closedness is NOT a consistency blocker for
anything shipped — "absent means decode" fails open everywhere, so its
absence costs decodes (96.8% wasted in the `absent_not_closed`
population, 22.8–27.6% of open reasons), never answers. The stakes
invert on landing: this is the arc's first fact whose STALENESS yields
a wrong answer (trusted silence about a method that now exists), so
the design's center is invalidation. The rule that discharges it:
**the certificate self-validates, exactly as 6b's rows do —
correctness never depends on an eraser; a stale certificate reads as
not-closed and fails open to today's decode.**

**What it is.** A per-CLASS certificate, minted where the index is in
hand (the resolver thread), recording: the class K, its full ancestry
enumeration [A1..An] (via the existing `for_each_ancestor_class` walk,
`INHERITS | APP_SURFACE` edges), and a validity key with TWO parts per
name in the closure:

1. **Provider-set identity** — the exact provider file set for that
   name as the index currently holds it (or a hash of it). This is
   what catches the arrival of a NEW file providing an ancestor name —
   the case per-provider fingerprints structurally cannot see.
2. **Per-provider surface fingerprint** — the same value everything
   else keys on. This catches edits to known providers (a parent list
   change surfaces here because Surface carries parents).

**Consult-side use**: the OpenNone/absent arm, before falling back to
decode, asks for K's certificate and VALIDATES it — every name's
current provider set matches, every recorded provider's fingerprint
stands (O(closure) map lookups, the same shape as the enrichment
overlay's own-plus-every-dep key). Valid ⇒ silence in the union of
those ancestors' maps is a trusted None, no decode. Any mismatch ⇒
not closed ⇒ decode, and re-mint lazily.

**Hard exclusions, on the values not the names** (rule #10):
- `has_dynamic_parents` anywhere in the closure ⇒ never certified.
- A plugin bridge in the closure ⇒ not certified in v1 — the bridge
  guard's asked-never-baked discipline stays consult-side; whether the
  bridge-set identity can join the key is a measured follow-up, not an
  assumption.
- `main` needs no special case in either direction: 534 providers
  (FHEM) is a big but enumerable set, and the provider-set identity
  covers it or invalidates it like any other name.

**Minting is LAZY, at the consult that would decode anyway**: that
consult already pays the ancestry walk; it additionally records the
certificate into a bounded, byte-accounted RAM store (residency
discipline — a new derived-copy cache must be accounted). Eager
whole-workspace enumeration would repeat level-indexing's mistake
(pay for every class when queries touch a subset). Persistence of
certificates is deliberately OUT of v1: validation needs the live
index regardless, certificates are tiny and re-mint in one walk, and
persisting them would demand a second freshness story for zero
measured need — decide it later on numbers if re-mint cost shows up.

**What closedness must NOT touch**: the bake. Maps stay index-free —
the certificate is a separate lane consulted beside the map, never a
new conclusion kind inside it, so the bake-determinism gate and both
EQUIV disciplines stand unchanged.

**Instrumentation, per the thread's rules**: `PERL_LSP_CLOSED_EQUIV`
runs the decode anyway on every trusted absence and counts
disagreements (per-arm caches; it must ablate the READ — the
certificate lane has one consumer, so no second-producer hole).
Counters: `closed.certified` / `closed.cert_invalid` (attempts) and
`closed.trusted_absence` (completions), reconciled against the
shrinkage of `concl.open.absent_not_closed` and its `.wasted`
sub-tag. Success = the 27.6% population converting to trusted Nones
with zero EQUIV breaks; then C's Link A/B re-runs as the pre-agreed
dead-or-parked gate (`baked_follow` still 0 ⇒ minting is dead).
