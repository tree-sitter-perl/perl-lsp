# Prompt: move iteration to relational

**Status: direction brief for the scaling push. Landed so far: the member
pre-filter (ladder 1), the bridged-walk early exit (ladder 3's cheap half),
the seam-retry gate (`serves_enriched`), and `RetainedReader` under both the
conclusion loader and the bag-rehydrate loaders. The rest is ranked, not
started.**

**Honest sizing note for the retained-connection family**: connection-per-miss
was the dominant term ONLY on a synthetic no-locality corpus (551 unrelated
dists: 69,933 conclusion-cache misses; real apps measured 511 on crm and 668
on Koha — misses are a LOCALITY property, not a file-count one). On real code
the fix saves ~0.4 s per cold check. It stays because per-call open is the
wrong shape regardless and the cliff exists for whoever indexes a dep-mirror
tree — but nothing further in this family is worth cutting on cost grounds.

## The thesis

Cross-file enrichment and resolution look superlinear in workspace size
because their unit of work is not the file — it is the file's provider
neighborhood, walked imperatively. The recurring shape: a walker fetches an
object (rehydrating a whole `FileAnalysis` through the blob LRU) in order to
answer a question that is relationally expressible, and every axis of that
walk grows with the workspace:

- **candidates per name** — a package is a SET of files
  (`visible_def_candidates`), 5–12 declaring files for a common name at 122x;
- **cost per touch** — resident-map read below the cache caps, a full
  zstd+bincode decode above them, and the sweep's access pattern is cyclic, so
  past the cap the hit rate collapses to ~0 rather than degrading;
- **recursion over the closure** — enrichment builds enriched overlays of its
  providers, whose enrichment recurses; reuse across consumers is defeated by
  traversal-order-dependent taint plus a 64-entry drop-oldest cache.

Multiply those and a bulk sweep is `N × B(N) × D(N)`. The per-file cost
reductions all closed (`docs/adr/skipping-cross-file-work.md` — eight
proposals, measured, rejected) because they attacked the linear constant, not
the multiplicative factors. The factors fall only by replacing
fetch-and-inspect with an indexed relation.

## The relations already exist

Three seams landed for other reasons and are exactly the right carriers:

- **The row store** (`docs/adr/relational-ref-index.md`): `refs(name, file)`
  candidate pairs and `syms(file, name, key, kind, span, container, flags)`,
  shredded in the same transaction as the blob (cannot drift — the failure
  that retired the RAM-side parallel reverse indexes), version-gated, indexed
  by name/key/file. `container` is the symbol's package attribution.
- **The conclusion layer** (`docs/adr/conclusion-layer.md`): the registry
  chase partially evaluated per file; cross-file answers residualize as
  `Link` (an ordered MRO ladder as data), never as baked values — so there is
  nothing to taint.
- **The flush worklist**: change-driven propagation with a movement cutoff —
  the declarative fixpoint the per-file recursive enrichment never was.

The migration is therefore mostly re-pointing walkers at relations that are
already maintained, not building new infrastructure.

## The migration ladder

Ranked by (evidence of cost) / (blast radius). Each row names today's walker,
the relation that answers it, and the soundness constraint.

1. **The ancestor walk's per-candidate member probe** — LANDED (this slice).
   `method_resolution_on_class`'s cross-file arm decoded every candidate of
   every ancestor to run an existence scan (`mroc.candidate_wasted`); the
   stamp built on it is ~63% of all blob decodes. Now `candidate_may_declare`
   asks the `syms` rows first and skips the rehydrate on proven absence.
   Constraints honored: fail-open everywhere the store cannot speak (below).

2. **`module_declaring_method_in_package`** — LANDED. Same probe, same
   guardrails, different loop; it runs on exactly the misses. Cold
   substrate: 24,506 of 25,523 candidate scans skipped (96%), found-rate
   unchanged; slice 1's equivalence evidence covers it a fortiori
   (`has_sub_in_package` is a strict subset of the walk's member test).

3. **Bridge entities by name — LANDED, without the row family.** Bridged
   entities are standard symbols of their file, already shredded, so a
   container-BLIND rows probe (`sym_name_row_exists` — an entity's container
   is the plugin's home package, not the bridged class) gates each
   candidate's decode. `for_each_entity_bridged_to_named` carries the name
   as a pre-filter license; the first-match-by-name consumers ride it, the
   early exit bounds hits, the probe deletes the per-miss scan. The
   `(class, entity_name, file)` row family stays unbuilt unless a corpus
   shows the name-only probe leaking (a plugin file declaring the queried
   name OUTSIDE the bridging namespace still decodes — over-approx, sound).

4a. **Cross-file consults are point-free, memoized per walk and per sweep —
   LANDED** (`24f66e0a`), and it is the arc's largest single win. The chain
   that found it: FHEM (a package-main corpus, ~n providers of one name ×
   n files × keys) drove 12.3M SlotType chase ATTEMPTS through an arm with
   no memo tier; the session memo was inert on the enrichment cascade (no
   session open — `session.absent` was the tell) and keyed on the consumer's
   POINT (meaningless in provider coordinates; every call site a fresh
   miss); and first-encounter pairs PER BUILD are the n², which no
   thread-local memo can span. The fix: point-free cross-file sub-queries
   (gold's 503 exact assertions license the semantics), a session around
   the overlay build, and `SweepAnswerGuard` — a sweep-wide, stamp-guarded,
   worker-shared (candidate, point-free query) → verdict store. Measured:
   attempts 918x down, wall 2.06x at n=250, n=500 from killed to
   completing, overlay builds 8.9x cheaper at unchanged count (the overlay
   was a CARRIER of chase cost, not a cost). The surviving ~keys×n attempts
   are the linear first-encounter floor — do NOT gold-plate this arm
   further (key-set gating exists as an idea; 918x is past the knee and
   the residual wall lives elsewhere). A failed first cut (reverted) is
   part of this row's record: a memo added where no session existed and
   keyed with the point displaced 20 of 12.3M — inert by construction,
   found by measurement.

   **The RSS half closed as two mechanisms, seven dead suspects later**
   (walk residency, overlay clones, sweep path memo, arena retention,
   diagnostics channel — each killed by a control arm or a byte counter,
   never by arithmetic): (a) **brk retention of decode-and-drop churn**
   owns the SUSTAINED figure — `MALLOC_MMAP_THRESHOLD_=65536` returns 53%
   (the server's number; ship posture is a product trade — jemalloc /
   threshold / accept); (b) **per-worker in-flight working sets** own the
   CREST — ~414 MB marginal per worker, peak linear in worker count at an
   invariant per-file footprint. Two fix attempts, one reverted: byte-
   capping the per-file sweep memo measured NET-NEGATIVE at n=500 (+51%
   wall, +15% RSS — 19,929 drop-oldest evictions converted retention into
   re-decode churn; the memo is the measurable MINORITY of the per-worker
   set, and a safety bound that degrades the case it protects is not one).
   The landed fix is **byte-budgeted ADMISSION** on the sweep: permits =
   source bytes (the a-priori footprint proxy, ~65x expansion measured)
   against an in-flight budget — bounds the product (concurrency × size)
   without converting anything, since a queued file is decoded later, not
   twice; provably no-ops on small-file corpora. The path memo itself is
   LOAD-BEARING (1.55x wall) and stays untouched.

4b. **The registry sweep's per-candidate pre-filter — LANDED for the
   many-provider shape.** Row 4's "don't decode to learn nothing" was retired
   on real-CPAN evidence of ~1.5 candidates per escalation — narrowing was
   not the lever THERE. FHEM measured the opposite shape: 634 of 877 `.pm`
   files declare `package main`, and the 28-corpus sweep put ~90% of its
   `--check` wall (both phases — warm ratio 0.90, the corpus outlier) inside
   the `PackageSymbol`/`SlotType` candidate sweeps, 118k of 171k warm
   provider fetches answering nothing, conclusions absorbing only 28k.
   Corpus keying check: 4,755 of FHEM's 4,996 distinct slot keys appear in
   ≤5 files, and most sub names are defined in exactly one — the probes are
   selective exactly where the fan-out is wide. The slice:
   `sweep_candidate_may_answer` (registry) gates each candidate on its
   never-evicted lanes (declared/dynamic parents for the class, app-surface
   membership, the per-file `unrowed_attachment_names` residue — declarative
   attachment names with no backing row, derived generically from the final
   bag so no push site can bypass it), then asks the rows
   (`candidate_bag_may_answer`: the `sym_member` probe for attributed
   `PackageSymbol`, the widest refs∪syms mention probe for `SlotType`). A
   skip is remembered as the `None` verdict it claims, so each (candidate,
   key) pair costs one row probe ever — the same first-encounter floor at a
   probe instead of a decode+chase. Ships `PERL_LSP_NO_CONSULT_PREFILTER`,
   `PERL_LSP_CONSULT_PREFILTER_EQUIV`, and counters
   `consult.prefilter_skip` / `consult.prefilter_break`. Row 4's
   program-scoped `main` stays the semantically right endgame — but note
   FHEM's inverted load direction (`fhem.pl` requires modules dynamically;
   modules call `main::` subs the loader defined), where a naive
   asker-closure rule would orphan every such call: that rule needs the
   reverse edge before it can land.

4. **The registry chase's no-answer fetches, and `main`'s program scope.**
   Measured on a real-CPAN corpus (12k files, 54% package-less scripts)
   against the substrate: top-level `PackageSymbol` query DENSITY is nearly
   equal (~180–200/file both), so query volume is not the differentiator —
   the OUTCOME MIX is. Substrate: ~3% of queries reach a candidate bag fetch
   and 52% of fetches answer nothing; real-CPAN: ~44% fetch and **94% of
   1.06M fetches answer nothing** — a million decodes to learn nothing,
   at ~1.5 candidates per escalation (so candidate-set narrowing is NOT the
   lever; the fan-out was never large). Three levers, separable:
   - **Don't decode to learn nothing** — RETIRED as a new probe by the
     consult decomposition (12k real-CPAN corpus: `baked_open` 59.9%,
     `not_local` 12.1%, `absent_but_inherits` 0.3%). The conclusion map's
     `NotLocal` verdict IS the symbol-absence skip, already live and
     carrying an eighth of consults; adding a rows probe beside it would be
     a third absence oracle where the leak is elsewhere. The leak:
     the OpenReason distribution, which is CORPUS-SHAPED — substrate-era
     prior: `AbsentNotClosed` 70.3% of decodes; 12k real-CPAN reading:
     `no_answer_linkable` 49.7%, `no_answer_self_only` 25.5%,
     `absent_not_closed` 22.8%. Two levers, both conclusion-layer roadmap,
     ranked by that reading: (a) **`Link` minting** — built, flag-gated
     (`PERL_LSP_MINT_LINKS`), parked on a substrate measurement (decodes
     4,103→4,104; "a follow abandons at the first rung whose own map says
     Decode") that predates knowing the linkable population is half of all
     open reasons on real CPAN — unparked-by-corpus is one A/B away, and a
     follow's win is bounded by TARGET-side closedness, so score
     `baked_follow` vs `baked_follow_incomplete` in both arms, not just
     decode count; (b) **world-level closedness** (the flush evaluates
     against a world; its product can stamp classes whose full ancestry
     enumeration is known) — converts the `absent_not_closed` fifth and
     raises every follow's completion odds, so it compounds (a).
   - **`main` is program-scoped**: an empty package resolves to `"main"`
     (`resolve/collect.rs`), and two scripts never share a stash, so a
     workspace-relation answer for a script's `main` is cross-program
     pollution — wrong, not just wasteful. Fix is the ScopedLookup/T3
     shape: main's candidates from asker F = F plus F's load closure's
     explicit-`package main` files; a scope rule keyed on program-locality,
     never a string match. Cost share is measured by the `mocpkg.*` /
     `mocfetch.*` attribution counters (negligible on the substrate, whose
     indexed root holds few scripts; unmeasured on script-heavy corpora).
   - **The R4 escalation lane**: `consult.moc_primary` counts raw-bag-None
     escalations to `enriched_present` — each is an overlay consult, its
     cost read directly from `consult.enriched` accumulated ns.

5. **The `MethodCall` stamp as a `Link`.** The frozen `MethodTarget` exists so
   an answer cannot depend on which verb asked; a conclusion `Link` is
   verb-independent by construction and cheap to evaluate. If the stamp's
   product were the ladder rather than the resolved value, the enrichment
   re-stamp collapses into the flush worklist's cutoff.

6. **Return-type consults through conclusions.** `MethodOnClass` is 96.0% of
   measured cross-file consults; serving them from the conclusion map removes
   the chase's bag decodes and the transitive overlay recursion behind them.
   This is the conclusion layer's own roadmap; listed here because it retires
   the `enriched_present` fallback that makes enrichment recursive.

7. **Residency policy for digests.** The export gate won because
   `export`/`export_ok` happen to be non-evictable axes; per-package
   method-name sets and bridge entity names lose because they happen not to
   be. "What survives the strip" should be derived from what the confirm
   loops read, not from field placement. The Surface already computes
   per-file method names and throws them away.

8. **Overlay key honesty — the enumerable half LANDED.** The key was already
   more honest than this rung claimed (depth-20 dep-closure walk, all loader
   shapes hashed); the real gaps were the BRIDGE and PROVIDER relations,
   whose members are not candidates of any walked name. Now
   `hash_relations_for` rides the key: bridge membership + member
   registration generations (the stamp freezes a bridging module's identity
   into overlay `MethodTarget`s) and provider membership, for the walked
   closure plus the file's own registration names. Pinned by
   `a_new_bridge_to_the_consumers_class_moves_the_enrichment_key`. The
   RESIDUAL is principled, not accidental: the stamp reads the whole world
   (any invocant class), which no closure-scoped key can cover — that
   residual collapses when rung 5 lands (a `Link`-shaped stamp freezes no
   world-dependent values), which makes 5 the true closer of 8.

9. **Enrichment as a delta artifact.** Every recursive consumer of an
   enriched analysis reads only the bag (`docs/adr/enrichment-build-cost.md`,
   consumer matrix), and the measured enrichment delta is 4.13% of base heap.
   The truncate-to-baseline + swap dance is imperative state management for
   what is logically a small immutable overlay. This is also the "make a
   build cheap" prerequisite that priced out level-indexed enrichment — the
   algebra was right, the carrier (whole-FA copies) was too heavy.
   **Design written: `docs/prompt-enrichment-delta.md`** (four stages, each
   land-alone; stage 2's bag view is where the clone leaves the hot path).

10. **The FA multiplier (density).** A giant Perl file's analysis costs
    ~2.2 KB per source line — refs 41.5% (a ref per meaningful token, each
    with an OWNED `target_name: String`), witness vec+index 39.4% (a
    witness per meaningful expression, each with an OWNED
    `WitnessSource::Builder(String)` tag) — ~10x release's footprint, and
    the term behind FHEM's brk churn, the crest's per-worker sets, and the
    rehydrate unit price. Options, cheapest-first, measurement-gated:
    (a) representation shrink — intern source tags (a closed set of
    static strings carried as per-witness heap allocations today) and
    dedupe repeated names within a file's tables; no semantic change,
    EXTRACT_VERSION bump; (b) consultation census — ghost counters on
    which witness kinds are ever READ per corpus, making elision of
    never-consulted emissions evidence-based rather than speculative;
    (c) density budget — per-file emission caps with the honest `degraded`
    mark (the MAX_CST_DEPTH precedent) for files past a size threshold;
    (d) chunked/lazy bag storage — shard the persisted bag so rehydration
    loads only the shard a query touches; composes with rung 9's bag view.
    Chunking the FILE itself (multiple FAs per source file) is the
    non-option: scopes, packages, and SymbolIds are file-scoped
    invariants, and every consumer assumes them.

## The discipline (every slice obeys these)

- **Skips need positive evidence; every unknown fails open.** A relation may
  answer "provably absent" only where it provably covers the file: rows conn
  available, file present in `files` (the single shredded marker), and the
  resident copy actually evicted — a whole copy answers from RAM for free and
  its rows may predate it. Same fail-open shape as `restamp_owed`.
- **Over-approximate toward the decode.** The member probe is kind-blind and
  matches name OR key: a wasted decode is the cheap error, a hidden member is
  the silent-wrong-goto-def one.
- **Know what the rows cannot see.** Deferred plugin emissions shred into
  their own presence table, `held` (`FileAnalysis::held_row_names`), never
  into `refs`/`syms`, whose rows every view shows. Every retrieval that
  licenses a skip (candidate files, the per-file probes, the dead-export
  view, the heatmap prune) reads `held` too and fails open on a hit. A new
  post-shred mutator of cached copies must shred its content the same way —
  or better, not be built.
- **Ship the switch that checks the assumption.** Each skip lands with its
  disable control and an equivalence mode that runs the skipped work anyway
  and screams on divergence (`PERL_LSP_RESTAMP_EQUIV` is the pattern; the
  member pre-filter ships `PERL_LSP_NO_MEMBER_PREFILTER` and
  `PERL_LSP_MEMBER_PREFILTER_EQUIV`).
- **A plausible product landing near a measured total is not an
  attribution.** Four fits failed in one night of the FHEM investigation
  (12 GB / 232 entries; 929 × 12 MB walk residency; clone-boundedness;
  "the sweep owns it so the clones own it") — every one was arithmetic
  that terminated at "the numbers work" instead of at "I changed one thing
  and the number moved." An attribution is a control arm or a byte counter
  at the holder; everything else is a hypothesis and must be labeled one.
- **An attempt counter is not a completion counter.** `moc.provider_fetched`
  counts attempts; the sweep memo made most of them Arc bumps, and a slice
  was cut against the wrong number before the reconciliation
  (`sweep.memo_hit + memo_miss ≈ attempts`; only `rehydrate.loader`'s n
  decodes) was done. State which kind any quoted counter is, and show the
  reconciliation when the two could diverge.
- **Verify the corpus contains what the mechanism KEYS ON**, not just that
  it is the right size — a giant-file gate was A/B'd against a subset built
  to exclude the giants, and separately its threshold sat above what the
  full corpus could reach. And the quantified corollary: installing real
  dependencies costs 19–76% cold wall across six corpora (Webmin, nearly
  CPAN-free, flat as the control) — every no-deps number understates real
  resolution load by roughly a third, so a sizing argument states which
  shape it measured.
- **One claim per shell invocation when the output is evidence.** Three
  commands in one line put a dirty-tree diff directly under a truncated
  `git show`, and the juxtaposition was read as provenance — 20 lines
  attributed to a commit they were never in. Separators are not
  attribution; the pipeline's shape can lie exactly like `tail`'s exit
  code and mawk's silent `\b`.
- **Counters, not vibes.** Every pre-filter counts skip/decode/unknown so the
  hit rate is a ghost-stats read, and the next ranking pass starts from
  numbers (`docs/adr/skipping-cross-file-work.md`'s denominator rules apply).
