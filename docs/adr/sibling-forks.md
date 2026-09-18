# Sibling forks: when two code paths answer one question

Two implementations of one decision, never reconciled, are the defect
class behind a run of real bugs (the gd/references disagreements, the
Perl-vs-pack null completion, the bare-cursor insert text). The
projection-consistency net (`src/index/resolve/tests/
projection_consistency_tests.rs`) finds instances on a schedule; this
ADR owns the doctrine for what to DO with one once found. The governing
line is the CandidateSet ADR's: **pairs verify, the seam prevents** — a
test that keeps two implementations in agreement is a written
confession that there are two implementations, and the question is
always whether the confession is escapable.

## The four classes

**1. Forward/backward algorithm pairs — irreducible.** A computation
and its inverse cannot be merged, only checked for composition. The
membership test is strict: a genuinely irreducible pair composes in
BOTH directions and loses information in each. The `requires`
round-trip qualifies (its leg-2 weakening is the proof: forcing
symmetry would break rename, because implementations answers dispatch
reachability while references answers rename correctness — different
questions). Two enumerations that merely disagree do NOT qualify,
however different the verbs sound — the gd/references bugs presented
as forward-vs-backward and were both fixed by projection, so the
default for a new pair is **reducible until it survives the
composition test**. For this class the net's invariant is the
permanent, honest tool.

**2. Scope/perf forks of one question — collapse.** One enumeration
grown a sibling for a smaller scope or a faster path. The template:
one driver, scope-parameterized, with everything cross-cutting hoisted
above the scope split so a new axis lands once (`walk_refs` and its
`WalkScope`; `rehydrate_axes_or_resident`). After a collapse the
paired invariant SHRINKS to the residual claim the scope split leaves
(scope enumeration commutes with filtering) — it is re-documented, not
deleted, because that residue is exactly what a scope split can
silently break.

The collapse-shape rule: **subsuming forks merge; nested ones
narrow.** Two enumerations of ONE relation merge into a driver. Two
enumerations of two OVERLAPPING relations, one nested inside the
other, narrow instead — the outer keeps only what the inner cannot
answer. The mroc/mdmp pair is the worked case: 98.6% path overlap on
the substrate, but the residual 1.4% is a strict-superset lane and
the only part that ever produces an answer — deleting the "redundant"
fallback would have been sound-looking and wrong. Measure overlap on a
real corpus before choosing the shape: the in-regime synthetic corpus
showed 100% overlap and would have made deletion look safe.

**And the mroc/mdmp narrowing was then built, measured, and rejected.**
The shape above is right; the reason to pay for it was not. The
narrowing is implementable and answer-preserving — the ancestor walk
records, per candidate whose symbols it already holds, the answer to
the FALLBACK's question (not its own: the walk sees through re-exports
and admits class-content, so donating `has_member` loses answers), and
the fallback honours it. It removed the fetches it targeted and nothing
else:

| counter | before | after |
|---|---|---|
| `mdmp.candidate_fetched`, substrate | 39,491 | 882 |
| `mdmp.candidate_fetched`, in-regime | 1,689,378 | 0 |
| `mdmp.found` | 80 | 80 |
| `mroc.*`, `mdmp.call`/`modules_scanned`/`not_found` | — | identical |

It bought no wall time in any regime measured — substrate cold
(20.79 → 20.59 s median), the in-regime duplicated-package corpus
(76.00 → 74.20 s), and substrate with the sweep memo disabled
(21.06 → 21.02 s), all inside run-to-run spread on three interleaved
cold repeats each.

The third arm is the one that explains the other two. Disabling the
sweep memo was meant to make these fetches expensive; instead, removing
36,712 rehydrations left `rehydrate.loader` TOTAL flat (3,531 → 3,579
ms, up by noise) while its AVERAGE rose (10.2 → 11.5 µs). That
signature is self-consistent and it is the finding: pull the cheapest
entries out of a denominator and the mean rises while the total does
not move. **The fetches the narrowing removes are the cheapest
rehydrations in the system.**

So the overlap number was **true about count and false about cost** —
and a 98.6% overlap invites exactly that inference, which is why this
is written down. A rehydrate COUNT is a poor proxy for rehydrate COST.

The structural argument does not rescue it either. The sibling-fork
rule exists to stop two implementations of one question from diverging
in their ANSWERS; the path-level data says the fallback's set is a
strict superset (the 563 paths it alone answers), so this is two
questions with overlapping work, not two answers to one question.
Duplicated work with no measured cost does not earn new machinery.
"Nested ones narrow" stands as taxonomy; it does not oblige paying for
the narrowing here.

What landed instead is the pair of fixtures that pin the fallback's
semantics (`typeglob_fallback_still_answers_from_a_candidate_the_
ancestor_walk_rejected`, `typeglob_fallback_keeps_its_provider_
ordering_across_the_overlap`) — previously unpinned, and the thing any
future attempt must not break. The rejected implementation is PR #151,
commit `0f41907e`.

**Bound-first is not enough on its own — ablate the bound.** The first
fixture written for this narrowing, committed before the change per the
bound-first discipline, PASSED under both wrong designs: an installer
sorting before the class short-circuited the fallback's `find()` before
the memo was ever consulted, so it asserted a true thing about a path
the change does not touch. It was caught by ablating it, not by reading
it. **An un-ablated fixture is an assertion, not a net.** The cheap
check is to break the change deliberately, in each way it could
plausibly be got wrong, and confirm the fixture fails each time — the
pair above was verified that way (donate `has_member` → the first
fails; answer out of the walk loop → the second fails).

> A worked one, because the fork was invisible until the two walks were
> read side by side: `implementations_of` gathered descendants PLUS the
> co-ancestors those descendants reach going back up their own MRO,
> while `member_override_family` gathered descendants only. Nothing
> declared them siblings — they were written for different verbs. But a
> role that CALLS `$self->m` and the sibling role that DEFINES it are
> joined only through their shared composer, down one edge and back up
> another, so the narrower gather returned nothing and `collect`'s
> membership test turned that into an empty `references` answer from a
> cursor where `--implementations` answered fine.
>
> Collapsed to `dispatch_participants`: one gather, `implementations_of`
> subtracts its own contract line, `member_override_family` unions the
> root. The residual claim is that the two verbs differ only by those
> post-filters — which is the shape the leg-2 weakening in class 1 makes
> explicit from the other side.

**3. Language-lane forks — fold the decision into a seam.** A Perl arm
and a pack arm making one decision. The cure is the value-carries-its-
rule pattern already in service: `Slot`, `VisibilityAxis`,
`LanguageScope`. A fork is sanctioned only where the *content*
genuinely differs per language and the *decision* is already seamed
(presenters over a shared resolution; `cursor_context` vs
`cursor_sentinel` under the `Slot` vocabulary).

> And a consequence worth recording where the next person will find it,
> because the measurement points the other way. The sibling-role gold
> fixture carries **no `requires`**: once the gather is shared, the
> projection alone answers, so the shape that motivated a `demands` lane
> — a Surface field, an `EXTRACT_VERSION` bump, per-package consumer
> churn — needs no recorded obligation at all. The 107-shape figure in
> `skipping-cross-file-work.md` is the population a `demands` lane
> *could* convert; it is not an argument that one is needed for the
> template-method case, which is closed. What remains is the
> abstract-base shape, pinned as an xfail row in
> `fixtures/call-hierarchy.json`, which will XPASS the day someone
> closes it.

**4. Eager/lazy twins — a ledger, not a backlog.** An eager writer and
a lazy reader of one rule is a deliberate purchase of speed with a
consistency risk. These are inventoried so the debt is known, and paid
only when one bites; most carry their sanction in a comment or ADR.

## The inventory

Collapsed (the templates): `refs_to`/`refs_to_in_file` → `walk_refs`;
`rehydrate_or_resident`/`rehydrate_rows_or_resident` → one body;
`package_isa_local` → `class_isa` over the `LocalParents` seam;
`walk_ancestry`/`GraphView::walk` → one engine (`graph::bounded_dfs`).
The ancestry collapse is the worked example of the bound rule below:
the two walkers guaranteed different things (visit budget vs depth
cap), so `WalkBound` carries BOTH axes as a type, each family's preset
preserves its pre-collapse guarantee exactly (`GRAPH`: depth 21,
visits unbounded; `ISA`: visits 200, depth unbounded), and the
divergent cases were pinned BEFORE the engines merged — after a
collapse nothing can detect a silently changed guarantee, because the
net compares siblings and there are no siblings left. Tightening a
preset is a deliberate, corpus-measured change to a constant, never a
side effect of routing.

Open, in execution order:

| pair | class | seam / note |
|---|---|---|
| `completion_items` vs `pack_completion` | 3 | assembly skeleton only (two-half gather, cap, `is_incomplete` composition); entity-content gathering stays out per the CandidateSet ADR's honest boundary. Ranked above its size: two of the six net-era bugs lived in this fork |
| `prepare_pack_parts` vs `prepare_workspace_parts` | 2/3 | tier-policy parameter; take it only if it falls out of the completion collapse |
| `index_perl` vs `index_pack` | 3 | **parked as a program, not a slice**: H9 bulk-defer coordination lives in the unshared halves, which is exactly where a mistake stays invisible until a corpus is large; no demonstrated bug yield. The persist harness, chunk writer, and residency tripwire are already shared |

The class-4 ledger: enrichment's eager `HashKeyOwner` stamp vs
`deferred_hash_key_owner` (comment-acknowledged); PostFold's
`invocant_class` bake vs the query-time invocant ladder;
`seed_return_types_from_bag`'s `return_types` map vs the registry
query (the watch item — a materialized map is what "edges, not values"
exists to prevent, but it predates the rule and retiring it is its own
project); `Ref::match_verdict_baked`'s bake-else-bag-fallback
(engineered for the rows lane).

## Discards — audited, not forks

Listed so nobody re-audits them: `sub_return_type_local` (a component
the full query composes), `resolve_symbol`/`resolve_symbol_scoped`
(wrapper), the `_inner`/`_cached` families (layering call-throughs;
`_cached` is the async-handler discipline, not a fork),
`hover_info`/`pack_hover` (presenters over one shared resolution —
sanctioned; flag only if a presenter starts making resolution
decisions).

## Related, distinct: stale agreement

The boundary case of the family, not a code fork: two verdicts that
agree by construction on a question neither is being asked. The worked
instance is a PR stack showing MERGEABLE with all checks GREEN —
MERGEABLE is a statement about text, GREEN is a statement about a base
that has since moved, and neither is a statement about the tree the
merge would produce (a rename sweeping 69 files merges cleanly against
new code referencing the old name; the result names a type that does
not exist). The cure is operational, not structural: a verdict is only
trusted about the tree it was computed on, so re-verify at the
integration tip before merging anything that predates it. Listed here
because a reader hunting fork-shaped hazards should find the edge of
the territory: sibling forks are two answers to one live question;
stale agreement is two answers to a question that expired.
