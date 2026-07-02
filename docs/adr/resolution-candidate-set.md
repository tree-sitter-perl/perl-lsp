# ADR: The resolution CandidateSet — one semantic core, features as projections

Status: accepted (design). Implementation targets **main first** (see Landing
strategy). Motivated by a recurring bug *class* on `spike/cpp-support`.

## Context: the recurring asymmetry disease

Five instances of the same bug wearing different costumes:

1. gd resolved kinds that gr couldn't mirror (the refs-symmetry audit's whole
   matrix: macros, enum variants, members, globals, typedefs).
2. Resolution was cross-file while completion *gathering* was same-file-only
   (the `OP_NULL` editor find).
3. The symmetry audit fixed gr on the **name** key but missed the
   **visibility** key — gd was closure-gated, gr wasn't (arc-review C1,
   CRITICAL: 85% noise on real queries).
4. Rename didn't follow what refs knew (arc-review C2, CRITICAL: silent
   partial edits).
5. Reachability ranking was computed but goto-def never consulted it (the
   win32-wins residual).

Root cause: **each LSP feature owns its own resolution path.** gd, gr,
rename, completion, hover, implementations share *data* (`FileAnalysis`,
`ModuleIndex`) but each independently re-composes the pipeline:
identify → gather → visibility-filter → rank → project. Every new **axis**
(include-closure visibility, language boundaries, delegation edges, macro
variants, family walks) is wired into the feature that motivated it, and
every other feature silently misses it. `ScopedLookup` is the smell made
visible: a *decorator* each entry point must remember to apply — C1 is
"the gr entry points didn't wrap." Symmetry is maintained by per-feature
diligence, N times per axis. Diligence always misses one.

## The proof the fix works: the tiers that already flow

The **witness bag** is single-sourced by decree ("production is push,
consumption is query through the registry, there is no second source") —
and the type tier has never had a cross-feature asymmetry. Same for
`parents_of` (one parent-enumeration seam: the app-surface edge injected
once, visible everywhere) and `cst.rs` (each grammar trap encoded once).
Single-seam tiers flow; N-path tiers leak. The resolution tier never got
its bag. `resolve_symbol`/`refs_to` (docs/adr/file-store-and-resolve.md)
was the right instinct — "the one entry point" — but it only unified the
identify step of references+rename. gd, completion gathering, and the
cross-cutting axes remained per-feature.

## Decision

One semantic core: **`resolve(from_context, name_or_position) →
CandidateSet`** — the canonical answer to "what does this name mean, from
here." The CandidateSet carries, computed ONCE, inside:

- **candidates** — every def site (never pruned; multi-def is normal:
  macro variants, specializations, decl+def),
- **visibility** — closure/import/language gating applied at construction
  (not decorated at entry points),
- **edges** — delegation, `specializes`, family/inheritance relations the
  walk may traverse (each consumer *declares* which edge kinds it follows),
- **ranking** — reachability/specificity/proximity, total-ordered,
  deterministic.

Every feature is a **projection** of the same CandidateSet:

| feature | projection |
|---|---|
| goto-def | forward-best; ranked multi-location (never prune) |
| references | the backward image of the same set |
| rename | references + rewritability policy (full edit or REFUSE — partial is unrepresentable) |
| completion | prefix-enumeration of the same visible universe |
| implementations | the family-edge walk over the same set |
| hover | the top candidate's presentation (provenance chain intact) |

Symmetry becomes **by construction**: an axis added to CandidateSet
construction is inherited by every projection. C1 ("gd gated, gr not") and
C2 ("rename edits a subset of refs") become unrepresentable states.
The audit's gold *pairs* remain as the verification net — pairs verify,
the seam prevents.

## Landing strategy: main first, spike rebases onto it

The seam is not cpp-specific — main's Perl features have the same N-path
shape. Building on main means the spike's remaining work lands ON TOP of
the seam rather than the seam being excavated out of the spike later.

1. **Interim (spike, now):** the arc-review C1/C2 fixes land as per-path
   patches — stop the CRITICAL bleeding; the seam subsumes them later.
2. **Core (main):** implement CandidateSet + migrate Perl gd / gr / rename /
   completion-gathering onto it. PR to main on its own timeline.
3. **Merge main → spike:** the cpp axes (include-closure visibility /
   `ScopedLookup`, delegation edges, `FileScopeValue`, macro variants)
   migrate INTO CandidateSet construction as pluggable axes. This migration
   is the opening slice of the template arc — which then adds its own axes
   (spec selection, `Specializes` walks, lazy projection instances) to the
   seam instead of to N feature paths.

## Consequences

- New-axis review question shrinks from "did every feature get it?" to
  "is it in CandidateSet construction?"
- The template arc's family/selection machinery lands once, not per-feature.
- Migration risk is real (resolve.rs is hot); the gold pairs + the arc
  review's repro battery are the migration net.
