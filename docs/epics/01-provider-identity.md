# Epic 1 — Provider identity: retire the single-winner assumption

> **Status:** partly landed (audited 2026-11-12). Phase A (the
> `pkgid-*` rows, all `gold` now), Phase B.2 (`parents_cached` unions
> every provider) and Phase E (the `@INC` multi-provider tier, PR #122)
> are done. Open: Phase B.1 and B.3, then Phases C–D — the residual
> consumer conversion. It is a class of confidently-wrong answers, not a
> class of misses, which is why it outranks the feature epics.
> **Design owner-docs:** `docs/open-problems.md` §"Duplicate-package
> resolution", the commit message of `b32814a0` (the relation's own
> rationale), and `docs/adr/file-store-and-resolve.md` +
> `docs/adr/resolution-candidate-set.md` (the seam the consumers move
> onto).

## Mission

`name → one provider` is not a model of any language this engine
serves. It is a model of *well-behaved* code, and the difference shows
up as a wrong file, confidently returned.

The **relation** is landed (`b32814a0`): `IndexCore.all_defs` holds
every file per declared name, `best_candidate` derives the name-keyed
cache slot from that set (class-over-value, then smallest canonical
path — derived, never inserted, which is what killed the order
dependence), `rebuild_name_registration` is the one mutation seam, and
gold rows `pkgid-01..05` pin it on `gold-corpus/pkgsplit-fixture`.

What did **not** land is the consumer conversion. **30 non-test
`get_cached` call sites (2026-11-12; ~75 when this epic was written)
still read the derived single winner.** Each is wrong
whenever the queried fact lives in a losing file. This epic converts
the user-visible ones to the candidate relation and leaves the winner
only where it is genuinely bookkeeping.

## Read first, in this order

1. `CLAUDE.md` — "Cross-file resolution" (especially "**A module name
   maps to a SET of files, not one file**"), the resolution
   CandidateSet paragraph, and the `VisibilityAxis` paragraph.
2. `git show b32814a0` — the whole commit message. It states the model
   and names the seams; do not re-derive them.
3. `docs/adr/resolution-candidate-set.md` — every user-visible verb is
   a projection; a fix that lands in a handler is in the wrong place.
4. `src/model/file_analysis/cross_file.rs` —
   `CrossFileLookup::visible_def_candidates` (the shared accessor,
   default = the full relation) and `ScopedLookup.closure_scoped`.
5. `src/index/module_index/queries.rs` + `lookup.rs` — where
   `get_cached` lives and who calls it.

## Current state — exact anchors (verify before editing)

| Thing | Where | Find it |
| --- | --- | --- |
| The landed relation | `index/module_index/index_core.rs` | `grep -n 'all_defs' src/index/module_index/*.rs` |
| The derived winner | `index/module_index/` | `grep -n 'fn best_candidate' src/index/module_index/` |
| The one mutation seam | `index/module_index/registration.rs` | `grep -n 'rebuild_name_registration' src/index/` |
| The shared candidate accessor | `model/file_analysis/cross_file.rs` | `grep -n 'visible_def_candidates' src/` |
| The residual call sites | everywhere | `grep -rn 'get_cached' src/ --include=*.rs \| grep -v tests \| wc -l` — triage this list; it is the epic's worklist |
| The visibility rule per language | `model/file_analysis/cross_file.rs` | `grep -n 'VisibilityAxis\|IncludeClosure\|SearchPath' src/` |

### The named contract-level residuals

Each of these *returns a shape that assumes one provider*, so no
amount of care at the call site fixes them:

- `module_declaring_method_in_package` returns a module **name**; the
  caller's `get_cached` then takes the winner, not the definer.
- ~~`parents_cached` is winner-keyed~~ — **landed**: it unions
  `declared_parents` over `visible_def_candidates`, pinned by
  `pkgid-06-inherited-method-via-losing-file-parent`.
- Enrichment's import scan, likewise.
- ~~The `@INC` tier is single-provider~~ — **landed** (PR #122, Phase E
  below): `resolve_module_paths` enumerates every root, and the
  `incdual-*` rows pin it.

## Phase breakdown

### Phase A — pin the wrong answers first

**Landed.** `gold-corpus/fixtures/perl-package-identity.json`
(`pkgid-01`..`06`) is all `gold`. New wrong-file rows for Phase C's
families still go here.

This is the move that worked twice already on this codebase, and it is
not optional here: a conversion this wide will otherwise land with no
evidence it fixed anything.

1. Build fixtures where the queried fact lives in the **losing** file:
   a reopened package whose second file declares the method being
   hovered / completed / chased; a multi-package file whose later
   packages hold the target.
2. Author gold rows as `xfail` with `gold-corpus/run.pl --emit <cap>`
   across the user-visible verb families: hover, completion,
   goto-def-through-a-chain, `MethodOnClass` type chases,
   `definitions`' PackageRef and import lanes.
3. **Acceptance:** every row RED for the right reason (a wrong file, or
   a confident wrong type — not an empty answer). A row that is merely
   empty is a different bug; move it out of this epic.

### Phase B — the contract-level returns

Fix the three shapes above before touching call sites; converting a
caller of a single-provider function just moves the bug.

1. `module_declaring_method_in_package` returns the **definer**, not a
   name to re-look-up. Prefer returning what the caller actually needs
   (the analysis, or a `(path, Arc<FileAnalysis>)`) over a name.
2. **Landed:** `parents_cached` unions ancestry edges across providers. This runs
   through `parents_of` — CLAUDE.md names it as the single
   ancestor-enumeration seam, and that stays true; what changes is
   what feeds it.
3. Enrichment's import scan, same treatment.
4. **Acceptance:** unit tests where the parent edge is declared only in
   the losing file and ancestry still finds it.

### Phase C — convert the user-visible consumers

In dependency order, each a commit, each with a Phase-A row flipping
green:

1. Hover cross-file arms (`model/file_analysis/hover.rs`,
   `lsp/symbols/hover.rs`).
2. Class-query member types (`class_queries.rs`).
3. Invocant / chain typing (`invocants.rs`, the registry's
   `PackageSymbol` chases in `model/witnesses/`).
4. Completion sources — through the CandidateSet's `complete()`, never
   a handler-side append.
5. `definitions()`' PackageRef and import lanes.

The mechanical shape is the same each time: `get_cached(name)` →
`visible_def_candidates(name, scope)` → ask each candidate the
question → merge. **Merging is the design decision, not the lookup.**
Record per family what "merge" means (first-answer-wins, union,
ranked) and why, in the commit message; a silent `.next()` on an
iterator of candidates is the single-winner bug with extra steps.

### Phase D — the winner keeps its honest job

`get_cached` is not deleted. Bookkeeping consumers — the
`module_index` internal queries, registration, CLI paths that want *a*
representative — keep it, and the epic ends with a comment at its
definition naming exactly which kind of caller it is for. Anything
user-visible that still calls it after this epic is a bug someone
must justify in review.

`prompt-scale-validation-hitlist.md` Tier 3 already lists "~10
bookkeeping `get_cached` sites — **OPEN**, deliberate". That number
should be roughly what survives; if it is much larger, Phase C is
incomplete.

### Phase E — the `@INC` / search-path axis

**Landed** (PR #122; `prompt-scale-validation-hitlist.md` Tier 3). The
residual is acquisition — a `use lib` root outside the workspace that is
not on the process `@INC` is ranked but never scanned
(`gold-corpus/KNOWN-GAPS.md`).

Only if C lands clean. Key `all_defs` by `(name, root)` and let
`VisibilityAxis::SearchPath` — which already ranks the asker's own
`use lib` roots ahead of the process set, longest-prefix then root
order — do the selection. This is the piece that makes `t/lib`
shadowing work correctly, and it is separable: ship A–D without it.

## Language-pack beat

**This is the epic where the seam must be language-neutral, and it is
the reason it goes first.** The single-provider assumption is not a
Perl bug wearing a general name — every language the engine serves has
its own spelling of it:

- **Perl:** a `package` may be declared in any file and reopened in
  several. Visibility is `VisibilityAxis::SearchPath` (`@INC`).
- **C/C++:** the same name is provided by a header and its
  implementation TU, by a prototype and a definition, and by every
  translation unit that includes it. Visibility is
  `VisibilityAxis::IncludeClosure` (flat linkage), and
  `ScopedLookup.closure_scoped` is the narrowing that already keeps a
  non-includer from seeing a header's names. The cpp arc already hit
  this from the other side — see-through goto-def preferring the
  **definition** over a prototype (`cpp-golive-map.md` item 6) is a
  merge policy over a candidate set, decided once.

The rule for every consumer converted here: **it must not ask what
language it serves.** It asks `visible_def_candidates` for candidates
and the scope's own `VisibilityAxis` for which of them are visible. If
a conversion needs a language branch, the branch belongs in
`VisibilityAxis::for_origin` — the ONE derivation — and nowhere else.

The payoff is that a language whose identity model is *leaf-keyed with
namespace claims layered on* (the shape a namespaced language wants)
becomes a `VisibilityAxis` variant plus a merge policy, not a second
resolution path. Write Phase C's merge policies so that is true.

## Scaling beat

**This epic touches the hottest relation in the system, and the FHEM
shape is its worst case.** `scaling-limits.md` §1 (measured
2026-08-17): FHEM declares `package main` in 534 of 614 files, so
`main` is 27% of package lookups and **94% of provider fetches**, and
the candidate set for it is 534 members wide. The relation is
*semantically correct* — those files genuinely share one stash — so
narrowing it would be wrong.

Which means: **converting a consumer from the winner to the candidate
set converts an O(1) read into an O(providers) one.** On well-behaved
code that is 1–2. On FHEM it is 534, at 94% of fetches.

Non-negotiable for every Phase C commit:

1. **Measure before and after on a monoculture**, not on `crm`. FHEM is
   in the corpus (`corpus/bootstrap.sh "" FHEM`), and it is the only
   corpus where this regression is visible at all. Three runs minimum,
   date the numbers, land them in `bench/` via `bench/measure.sh` —
   per the house rules, a single run is not a baseline.
2. **Ask for candidates lazily and stop early where the merge policy
   allows it.** A first-answer-wins merge must not materialize 534
   analyses to return the first. This is where the merge policy from
   Phase C stops being bookkeeping and starts being the performance
   design.
3. **The prefilters already exist — use them, do not rebuild them.**
   The relational row store (`adr/relational-ref-index.md`)
   SOUND-pre-prunes provably-unreferenced names; the closedness
   certificate (`index/closedness_store.rs`,
   `model/witnesses/closedness.rs`) makes a bake's silence a trusted
   `None` instead of a decode, and it *self-validates against the live
   index* so a stale certificate costs a failed validation, never a
   wrong answer. A candidate sweep that decodes providers the store
   could have answered is the regression this epic is most likely to
   ship.
4. `scaling-limits.md` names the honest fix for the sweep level —
   deduplicating provider decoding across a sweep (~13,456 rehydrates
   for ~500 distinct providers is ~27× redundant). That fix is **Epic
   15**, not this one. Do not build it here; do make sure this epic
   does not make its arithmetic worse.

Report in the PR: FHEM `--check` peak RSS and wall, cold and warm,
before and after, with `RAYON_NUM_THREADS=4
MALLOC_MMAP_THRESHOLD_=65536` as the documented workaround still
applied — that is the configuration the doc tells users to run.

## Invariants that MUST survive

- `rebuild_name_registration` stays the ONE mutation seam for name
  registration; sibling edges survive by construction, not by
  diligence.
- `parents_of` stays the single ancestor-enumeration seam.
- The CandidateSet stays the one resolution entry point. A merge
  policy is part of the set's construction, never a handler's loop.
- `VisibilityAxis::for_origin` stays the ONE derivation of a scope's
  visibility rule.
- Derived-not-inserted: the winner is a projection of the relation. If
  anything in this epic writes a winner into a map, the order
  dependence `b32814a0` killed comes straight back.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS with the
Phase-A rows promoted from xfail → gold, `lang-skip 0` in the summary ·
`./e2e/run.sh` · substrate audit at parity-or-better with every moved
count triaged · **the FHEM measurement above, in the PR body.**

## Sizing & sequencing

Medium, but wide. A → B → C strictly (A is the evidence, B is the
prerequisite); C is a string of independent commits and is where the
time goes; D is bookkeeping; E is separable and droppable.
