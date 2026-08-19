# ADR: What cross-file enrichment costs, and every way of doing less of it that failed

Cross-file enrichment is the dominant cost of a batch analysis. Seven distinct
ways of doing less of it were proposed and measured; **all seven closed**. This
records the numbers so the next attempt starts from them rather than from the
idea, and so a closed question is not re-opened on the strength of how
plausible it sounds.

Measurements are `--check` over the gold substrate (2,265 `.pm`, cold cache)
unless stated. Counters are `PERL_LSP_GHOST_STATS`; nothing here is live
instrumentation cost.

## Where the time actually is

| phase | ms | note |
|---|---|---|
| `stamp_method_call_targets` | **~10,000** | 63% of all blob decodes |
| `pattern_dispatch` | ~2,700 | ~99.9% of it the query traversal |
| the provider chase | 241 | was 1,541 before the export gate |

`stamp` dominates and its cost is **success**, not failure: resolving an
invocant means walking ancestors, and walking ancestors means rehydrating
providers.

| resolve outcome | calls | total | avg |
|---|---|---|---|
| hit | 37,938 | 8,773 ms | **231 µs** |
| miss | 20,599 | 778 ms | 38 µs |

A miss is **6.1× cheaper** — it fails before the walk starts. Apportioning this
phase's cost by ref COUNT is therefore wrong in a predictable direction, and
was wrong every time it was tried.

## The seven closed questions

**1. Skip the re-stamp unconditionally.** 40.9% of the pass re-derives an
answer the build already froze. Rejected: 116 refs in 26,864 (**0.43%**) resolve
differently with the index, and a ref answering differently depending on which
verb asked is the silent divergence the freeze exists to prevent.

**2. Skip when the invocant's class ancestry is wholly local.** Sound — a local
parent chain walks identical edges with or without an index, and the
cross-tabulation found **zero** counterexamples in 26,864 refs. Worthless where
it matters: 18.6% coverage on the substrate, **0.79% on Koha**, where 99.2% of
refs have cross-file ancestry. Any predicate keyed on "is this resolvable
locally" is dead on real applications.

> The first version of this predicate was **unsound and also scored zero
> counterexamples**. A split package satisfies "declared here, no dynamic
> parents, no parent the index knows that we do not" and still breaks the
> argument, because the method can live in the other file's copy of the same
> package. Found by re-reading the claim, not by testing it.

**3. Skip when no provider's Surface moved (freshness).** **Unsound.**
`MethodSurface::ret` is a local conclusion — projection runs with no module
index — so two provider bodies with different *enriched* return types project
byte-identical Surfaces. `SurfaceVerdict::Unchanged` sets `skip_consumers`, so
such an edit invalidates nobody while a consumer holds a stale frozen
`MethodTarget`. Pinned by
`surface_tests::a_cross_file_dependent_return_change_is_invisible_to_the_surface`.
Transitivity is *not* the hole: `dirty_consumers` does propagate through a
dirty consumer's own provided names.

**4. An `ImportedSub` attachment, sold as throughput.** The lazy name-keyed
chase it proposes **already exists** — `query_sub_return_type` walks
`find_exporters(name)` per question. And the path is not where the cost is:
`consult.imported_sub_return` fires **5** times per run against 107,463
`consult.moc_primary`; sampled decode backtraces put `query_sub_return_type` at
**0.0%**. Still worth doing as a *modelling* fix; never as a performance one.

> A `SymbolId` is file-relative, so `ReducedValue` cannot carry "the resolved
> symbol" for the cross-file case that motivates the design.
> `MethodResolution::CrossFile` already refuses to carry one for exactly this
> reason. The file-independent payload is the owner — `HashKeyOwner::Sub
> { package, name }`.
>
> And carrying it is **not** a widening. A catch-all arm swallows a new variant
> silently — inference goes dark with no error — and `FactMap` does not avoid
> this, because a reducer that returns it stops returning `Type` for those same
> callers. Every consumer therefore now names its variants (`if let` included,
> which falls through just as quietly and which no exhaustiveness check
> reaches); `layering_tests::every_reduced_value_match_names_its_variants` keeps
> it that way.
>
> Making the compiler produce the worklist is also what priced the design out.
> **23 arms across 15 functions**, of which exactly two want an owner:
> `class_queries::method_return_type_on` (hover's second ancestry walk) and
> `query::query_sub_return_type` (which exporter answered). The other 13
> functions — the five `registry::materialize` edge-chases, the build-time
> `fold` queries, the structural drills — would gain an arm whose whole job is
> to discard a payload they never asked for. And the two that want it return
> `Option<InferredType>`, so a new variant dies at their signature anyway: the
> load-bearing change is those two return types, not the enum.
>
> The shape that follows: leave `ReducedValue` alone and let the chase report
> where it terminated, opt-in, to the callers that asked. `query_rec` already
> holds the owner at each cross-file frame (`visible_def_candidates(idx,
> class)`) and discards it.

**5. Bake the unowned hash keys.** 17.4% of hash-key refs on the substrate
(54.7% on Koha) carry no owner, and the question was whether local information
could close that. It cannot, for ~95% of them: **51.9%** are literal/pair-list
keys with no container variable to own them (`connect(timeout => 30)`,
`%EXPORT_TAGS`), **43.7%** name a variable the file has neither a type
constraint nor a declaration for. Only **4.4%** — 0.7% of all hash-key refs —
are gap-shaped.

**6. The `matcher_view` upgrade fires too often on hash keys.** It does not:
**8.6%** for hash-key targets against **20.3%** for method targets. The upgrade
needs a ref that is *both* unbaked *and* name-matching the target; the 81.8%
unbaked figure is over every hash-key ref in a file, which is a different
population. Note the arm is chosen by TARGET KIND, so "what share of upgrades
are hash keys" is a question about the mix of queries people run, not a
property of the corpus.

**7. The per-match gating in `pattern_dispatch`.** It recomputes per-package
facts per match — package by linear scan, `use` set cloned, transitive parents
rebuilt, 1,238 matches over 373 packages (×3.32). Textbook cardinality smell,
identical in shape to three things that *were* worth fixing. It measures
**2.7 ms, 0.10%** of the phase. The phase is the traversal (~99.9%), and the
fixed-point loop is 1.09 rounds/file.

## The rules these bought

**A structural smell is not evidence of cost.** Item 7 has the same shape as
the owner cardinality, the stamp re-derivation and the ref-row over-specifying.
Three were worth fixing and it was not, and only measurement separated them.

**Numerator measured, denominator assumed** is the recurring error, in both
directions: apportioning stamp cost by ref count (wrong by 6×), predicting the
upgrade rate from the global unbaked rate (wrong population), computing
rounds-per-file from another subsystem's file count (wrong by 5×). Counting the
denominator has cost one line every time.

**When a change has a local path and a cross-file path, the local path passing
is not evidence.** Items 2, 3 and 4 each had a version that worked locally,
passed everything runnable, and was wrong on the cross-file case that motivated
it. All three were caught by reading a type or an argument.
