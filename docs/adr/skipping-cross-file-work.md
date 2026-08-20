# ADR: What cross-file enrichment costs, and every way of doing less of it that failed

Cross-file enrichment is the dominant cost of a batch analysis. Eight distinct
ways of doing less of it were proposed and measured; **all eight closed**. This
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
| `pattern_dispatch` | 3,690 | 91.6% of it the query traversal |
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

## The eight closed questions

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
**2.7 ms over 1,238 calls at 2.2 µs — 0.073%** of a 3,690 ms phase, of which
`pd.collect` (the traversal) is 91.6%. The fixed-point loop is 1.09
rounds/file.

> The share figure here was first reported as "0.10% of a phase that is ~99.9%
> traversal", and the traversal half of that was **my own denominator error**.
> I divided `collect` by the sum of the timers I had added rather than by the
> phase, so the regions I had not instrumented — `pd.loop` at 239 ms,
> `pd.on_match` at 183 ms — were missing from the bottom of the fraction and
> the top absorbed them. The 2.7 ms numerator was right and unchanged; only
> what it was a fraction OF was wrong. Re-measured against #136's
> instrumentation, which times the phase itself. #136 independently reports
> the same gate share (0.07%), from a different box and a different
> instrument.

**8. Route the return-type query at the owner the first walk already found.**
Hover asks one `(class, method)` question twice: `resolve_method_in_ancestors`
climbs to the declaring class, then `find_method_return_type` asks
`MethodOnClass{access_class, name}` and the reducer climbs the same ancestry
again inside the registry. Both hover arms already **bind** the owner from the
first walk — they use it for the `"Child (from Parent)"` label — and then pass
the access class to the second. The owner is in scope, one line up, unused.
`method_return_type_on` separates the dispatch class from the receiver VALUE,
so re-anchoring looked free.

Measured with the `owner` probe (`PERL_LSP_PROBES=owner`): 13,534 probes over
**5,779 distinct `(access, owner, method)` shapes**, 1,167 of them inherited.
The owner anchor is **2.7× cheaper** — 43.0 µs against 114.3 µs — and agrees on
13,491 of 13,534 occurrences.

Rejected on the residual. The 43 disagreements are **one shape**:
`Catalyst` / `Catalyst::Component` / `config`, where the owner anchor answers
`None` and the access class answers `HashRef`. In all 43 the owner is
cross-file and the ACCESS class carries its own `MethodOnClass{Catalyst,
config}` witness, which re-anchoring discards; restoring the access class's
framework recovers none of them, so it is the anchor and not the context.

The reason it cannot be patched is the interesting part: **the two walkers mean
different things by "owner".** `resolve_method_in_ancestors` answers *which
class declares the symbol* — it climbs past `Catalyst` because no `Symbol` for
`config` lives there. The bag answers *which attachment carries the witness* —
and that is `Catalyst`. Neither is wrong. So a channel that reports "the owner"
must pick one of the two notions and consumers cannot tell which they got: the
owner is not a property of the answer, it is a property of which walker you
asked. That is rule #10 wearing a provenance costume, and it retires the
richer-`ReducedValue` design in item 4 for the same reason a variant would not
have reached hover.

> Limits, stated because one counterexample in 1,167 inherited shapes is thin:
> this substrate contains exactly one Catalyst. It is enough to falsify "always
> safe", which is what the design needed. It is not enough to say how often the
> two notions diverge in general — Koha would test that, and is not runnable
> here.


## The rules these bought

**A structural smell is not evidence of cost.** Item 7 has the same shape as
the owner cardinality, the stamp re-derivation and the ref-row over-specifying.
Three were worth fixing and it was not, and only measurement separated them.

**Numerator measured, denominator assumed** is the recurring error, in both
directions: apportioning stamp cost by ref count (wrong by 6×), predicting the
upgrade rate from the global unbaked rate (wrong population), computing
rounds-per-file from another subsystem's file count (wrong by 5×), and
dividing a region by the timers on hand rather than by the phase (item 7's
own "99.9% traversal", really 91.6%). Counting the denominator has cost one
line every time.

The last of those is the trap specific to *adding* instrumentation: the
regions you just wrote feel like the whole, because they are the whole of what
you can see. A share is only meaningful against a total that was measured
independently of which probes you happened to place.

**An occurrence count is not a shape count.** Item 8's 43 disagreements read
like a 0.3% tail distributed across the corpus. They are one method asked 43
times, in one distinct shape out of 1,167. The occurrence figure and the shape
figure support opposite conclusions about whether a residual generalises, and
only the second one was about the design. `count_distinct` is one line.

**When a change has a local path and a cross-file path, the local path passing
is not evidence.** Items 2, 3 and 4 each had a version that worked locally,
passed everything runnable, and was wrong on the cross-file case that motivated
it. All three were caught by reading a type or an argument.
