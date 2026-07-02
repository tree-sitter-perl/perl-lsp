# ADR: The resolution CandidateSet — one semantic core, features as projections

Status: accepted; core landed on main (`resolve.rs::CandidateSet`).
Originally motivated by a recurring bug *class* on `spike/cpp-support`; the
seam is main-first and the spike's axes merge onto it.

## Context: the recurring asymmetry disease

Five instances of the same bug wearing different costumes (verified on the
spike):

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

One semantic core: **`resolve(files, origin, key, point, index, scope) →
CandidateSet`** — the canonical answer to "what does this name mean, from
here." The CandidateSet owns, computed once at the set level:

- **identity** — what the cursor resolves to (`resolve_symbol_scoped`'s
  Target / Group / Local verdict), minted exactly once,
- **visibility** — the RoleMask verdict (`references_mask_for`) memoized on
  the set, with a construction-time override (`with_visibility`) that every
  projection inherits — the plug point for future axes (closure/import
  gating, language boundaries), never an entry-point decorator,
- **edges** — the override family / dispatch chain on `TargetRef.method_classes`,
  group members with per-member rename rules, and the descendants walk
  (`implementations_of` over `GraphView`); each projection declares which
  edges it follows,
- **per-site policy** — `RefLocation.rewritable`, `MemberRename` texts:
  policy rides the candidates, handlers never re-derive it.

Every feature is a **projection** of the same CandidateSet:

| feature | projection |
|---|---|
| references | `references()` — the backward image of the set |
| rename | `rename_edits(new)` — references + rewritability policy; an edit outside the references image is unrepresentable |
| prepareRename | `renameable()` — mirrors `rename_edits`' arms |
| implementations | `implementations()` — the family/descendants walk |
| goto-def | `definitions()` — forward-best projection |
| completion gathering | (next step) prefix-enumeration of the same visible universe |
| hover | (future) the top candidate's presentation |

Symmetry becomes **by construction**: an axis added to CandidateSet
construction is inherited by every projection — the test
`candidate_set_visibility_axis_flows_to_every_projection` demonstrates the
one-knob property. C1 ("gd gated, gr not") and C2 ("rename edits a subset
of refs") become unrepresentable states. The audit's gold *pairs* remain as
the verification net — pairs verify, the seam prevents.

## Landing notes (main)

- The set lives in `resolve.rs`, extending the existing
  `resolve_symbol`/`refs_to` seam — not a parallel module. `refs_to`,
  `group_refs`, `references_mask_for` are now the set's internals; handlers
  and CLI mirrors construct the set and project.
- Projections only READ the stores (`FileStore::for_each_open`), so an LSP
  handler may hold its open-doc guard across a projection — the old
  `drop(doc)`-before-walking discipline (a deadlock trap) is gone.
- Behavior-preserving by design: each projection reproduces the exact
  pre-seam composition. Known pre-existing asymmetries surfaced by the
  migration are documented in the PR/commit trail rather than silently
  fixed (e.g. group rename does not consult `rewritable` while target
  rename does; `definitions()` returns the first winning path rather than
  the never-pruned ranked multi-set).

## Merge plan: spike rebases onto this

The cpp axes (include-closure visibility / `ScopedLookup`, delegation
edges, `FileScopeValue`, macro variants) migrate INTO CandidateSet
construction as pluggable axes:

- closure visibility → the `with_visibility`/`target_visibility` seam
  (a per-origin gate computed at construction, not per entry point),
- delegation / `Specializes` edges → alongside `method_classes` and the
  group-member edge set,
- macro variants / multi-def → additional candidates (the set never
  prunes; projections rank),
- ranking (reachability/specificity/proximity) → a total order on
  candidates that `definitions()` consults for forward-best.

The template arc's family/selection machinery then lands once, on the
seam, instead of per-feature.

## Consequences

- New-axis review question shrinks from "did every feature get it?" to
  "is it in CandidateSet construction?"
- Migration risk is real (resolve.rs is hot); the gold pairs + e2e are the
  migration net — full net green after every migration step.
