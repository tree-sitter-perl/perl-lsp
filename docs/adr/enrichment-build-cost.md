# ADR: What an enrichment build actually costs

`ModuleIndex::enriched_snapshot` produces a private, enriched copy of a
`FileAnalysis`. `docs/adr/level-indexed-enrichment.md` identified the cost of
that build as the thing blocking level-indexed enrichment, and named the fix:
replace the whole-analysis copy with a small overlay of derived facts.

**The copy was a real cost and is now gone. It was not the dominant one.**
Measured, so the next attempt starts from numbers rather than from the idea.

The dominant one is `stamp_method_call_targets`, and every proposal for doing
less of it has closed — see `docs/adr/skipping-cross-file-work.md` for the
seven and their numbers.

## Measurements

150 substrate modules (`gold-corpus/local/lib/perl5`, 4 KB–400 KB, 58.8 MB of
analyses), release build. The probe is
`file_analysis_tests::probe_copy_cost_and_delta_size`, `#[ignore]`d so it
stays out of the bar; run it with `--ignored --nocapture`.

**The copy.** Enrichment obtained its private copy by bincode round-tripping
the whole analysis — serialize, deserialize, then `after_deserialize` to
rebuild every index from scratch:

| copy | ms |
|---|---|
| serde round-trip + `after_deserialize` | 895 |
| `clone` | 80 |

11x, and `clone` is also the more faithful copy: `bag_evicted`, `degraded` and
the ref/symbol eviction flags are `serde(skip)`, so the round-trip reset them
to false and nothing put them back — an enriched copy of a DEGRADED analysis
claimed to be whole.

**Where a build's time goes**, once the copy is a clone:

| part | ms | share |
|---|---|---|
| copy (`clone`) | 80 | 3.8% |
| enrichment, local half | 732 | 34.6% |
| enrichment, cross-file provider chase | 1,301 | 61.6% |

**The delta is small, as predicted** — 4.13% of base heap over the 122 of 150
files where enrichment changes anything at all (+10 symbols, +37 refs, +1,618
witnesses). The overlay idea is sound about the *data*.

## What this means for level-indexed enrichment

That design asks for one build per level. Its arithmetic against the
containment branch was 2.5x at K=4 and 15x at K=8, attributed to the copy.

Removing the copy removes 27.8% of a build (2,928 → 2,113 ms), which turns
15x into roughly 11x. **An overlay would remove a further 3.8%.** Neither is
the difference between "too slow" and "affordable", so the prerequisite that
ADR names does not, on these numbers, unblock it.

The blocking term is the **cross-file provider chase** — 61.6% of a build,
and it is re-done from scratch every time a file is built. A level-indexed
design builds each file K times, so it pays that chase K times over.

**Where that led is not where this section pointed.** The guess above was
"memoize the chase across builds"; per-caller attribution inside the chase
said otherwise. Of 1,541 ms of chase, provider *resolution* was 0.4% and the
overlay 0.02% — 93.5% was `bag_present`, because the chase is a breadth sweep
that misses the bag LRU 45% of the time against 1.0% for every other caller,
and each miss is a real decode. The loop took a bag view up front for a scan
whose filters are all symbols-axis reads; gating on the export surface (an
axis that survives the strip, so the skip costs no rehydrate) dropped the
per-candidate fetch 7,829 → 502 and the chase 1,541 → 240 ms, work done
unchanged. A memo would have cached the cheap half. Attribution inside a term
is what turned a 61.6% share into a fix; the share alone named the wrong one.

## The consumer matrix

Who reads an enriched analysis, and through what surface. The split matters:

| consumer | reads via | recursive? |
|---|---|---|
| `query_sub_return_type`'s imported recursion (`witnesses/query.rs`) | witness bag | yes |
| `MethodOnClass` cross-file primary (`witnesses/registry.rs`) | witness bag | yes |
| bridged-namespace bake (`witnesses/registry.rs`) | witness bag | yes |
| forward slot-seeding retry (`witnesses/registry.rs`) | witness bag, pins the Arc | yes |
| enrichment's own provider chase (`enrichment.rs`) | witness bag | yes |
| `--check` diagnostics (`lsp/cli/query.rs`) | whole analysis | no |
| `--dump-package` (`lsp/cli/query.rs`) | whole analysis | no |
| `ScopedLookup` (`model/file_analysis/cross_file.rs`) | passthrough | n/a |

**Every recursive consumer reads only the bag.** Only the two one-shot CLI
verbs need the whole analysis. So if an overlay is built later, it should be a
BAG overlay serving the recursion, with whole-copy materialization reserved
for the verbs that actually read refs and symbols. That would not buy much
time — the copy is 3.8% — but it would shrink what the 128 MiB byte cap has to
hold, which is the other half of why builds are rationed.

## The R4 rule

Enrichment must never write through the shared `Arc`. `clone` satisfies this
the same way the round-trip did — by being a private copy — and an overlay
would satisfy it by construction, never touching the base at all. Nothing here
weakens it.

## The truncate dance

`enrich_imported_types_with_keys` begins by truncating symbols/refs/witnesses
back to their sealed baselines so repeat enrichment is idempotent. On a fresh
copy that truncate is a no-op; it earns its keep for `FileStore::enrich_open`,
which re-enriches an already-enriched open document in place. It is procedural
rather than structural, and a delta representation would make it structural —
but it is not a cost worth chasing on its own: it is a length assignment.
