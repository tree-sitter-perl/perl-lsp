# Epic 3 — Openness: one answer to "is this unresolved name real?"

> **Status:** scheduled, third.
> **Design owner-docs:** `docs/prompt-graph-walking.md` §"Deferred:
> Scope nodes", `docs/open-problems.md` §"Qualified-name resolution
> suppression is coarse", `docs/adr/graph-walking.md` (the landed walker
> this builds on), `docs/adr/narrowing-diagnostics.md` (the promotion
> ladder this epic monetizes).

## Mission

Every "couldn't resolve X" decision in the diagnostics uses a
different, partial suppression rule: a `framework_imports` string set,
a hardcoded `universal_methods` list, an AUTOLOAD-in-MRO skip, an
unresolved-ancestor skip, a syntactic `SUPER::`/qualified-name skip,
and — the audit's remaining noise class — nothing at all for open-world
dispatch.

Replace the pile with ONE structural question the graph answers: *walk
outward from the reference site; if you reach an OPEN namespace before
exhausting CLOSED ones, stay silent; if every namespace on the chain is
closed and the name still does not resolve, warn.* Then use the trust
gained to flip diagnostic flags default-on per the promotion path.

## The vocabulary collision, up front

**There is already a `closedness` concept in this codebase, and it is
NOT this one.** Read both before naming anything:

- `model/witnesses/closedness.rs` + `index/closedness_store.rs` — a
  per-class **certificate** that a class's ancestry was fully
  enumerable at mint time, used to turn a conclusion bake's *silence*
  about a member into a trusted `None` instead of a full blob decode.
  It is a performance instrument on the type-chase path, it
  self-validates against the live index, and correctness never depends
  on it (`docs/adr/conclusion-layer.md` §"World-level closedness").
- This epic's **openness verdict** — a diagnostics-suppression
  question: may a name legitimately exist that this analysis cannot
  see?

They ask overlapping questions of the same graph and they are not
interchangeable: a class can be perfectly enumerable (certificate
valid) and still be Open to arbitrary method names because it has an
`AUTOLOAD`. Reuse the certificate as an *input* where it helps — a
class whose ancestry failed to certify is a strong `UnresolvedAncestor`
signal, and it is already computed — but do not fold the two verdicts
into one type. Name yours distinctly and say why in the ADR.

## Read first, in this order

1. `CLAUDE.md` — rule #10 (this epic exists because of it), the
   resolution CandidateSet paragraph, "Inheritance & frameworks".
2. `docs/adr/graph-walking.md` — `GraphView`, the closed `EdgeKind`,
   exhaustive `edges_from`, why file roles are NOT graph nodes.
3. `docs/adr/conclusion-layer.md` §"World-level closedness" — the
   collision above.
4. `docs/prompt-graph-walking.md` — the deferred Scope-node taxonomy;
   note its warning that scope parent-climbing is a linked list, so
   `Node::Scope` must be *earned* by Openness, not ported for its own
   sake.
5. `docs/adr/narrowing-diagnostics.md` — the flag ladder.
6. `docs/prompt-cfg-tier.md` §8.2 rung 1 — the path-sensitivity tier
   (Epic 16) kills a noise class this epic will meet. See the note
   under Phase D.
6. `src/model/graph.rs` (read all of it) and the unresolved-method /
   unresolved-function blocks in `src/lsp/symbols/diagnostics.rs`.

## Current state — exact anchors

| Suppression rule to subsume | Where | Find it |
| --- | --- | --- |
| `framework_imports` string set (unresolved-function) | `lsp/symbols/diagnostics.rs` | `grep -n 'framework_imports' src/lsp/symbols/diagnostics.rs` |
| `universal_methods` hardcoded list | `lsp/symbols/diagnostics.rs` | `grep -n 'universal_methods' src/lsp/symbols/diagnostics.rs` — **Epic 2 Phase A splits this**: the true `UNIVERSAL::` half stays, the framework half becomes `meta_methods`. Both are genuine facts and become INPUTS to the one walk, not a parallel path |
| AUTOLOAD-in-MRO skip | `lsp/symbols/diagnostics.rs` | `grep -n 'AUTOLOAD' src/lsp/symbols/diagnostics.rs` — this IS an openness fact; fold it in |
| Unresolved-ancestor skip | `model/file_analysis/` | `grep -rn 'class_has_unresolved_ancestor' src/` |
| `SUPER::`/qualified-name syntactic skip | `lsp/symbols/diagnostics.rs` | `grep -n 'SUPER::' src/lsp/symbols/` — replace with real resolution (Phase C) |
| Open-world dispatch noise | `model/file_analysis/` | `grep -rn 'fn guard_redundancies' src/model/` |
| Role-ness (already an openness fact) | `PackageFacts::is_role` | roles are Open by definition (`docs/adr/role-contracts.md`) |
| Plugin namespaces / app surface | `plugin_facts.rs` | `for_each_entity_bridged_to`, `app_surface_consumers` |
| Gated emissions (a class may gain content at query time) | `model/file_analysis/enrichment.rs` | `grep -n 'gated_emissions\|apply_gated_emissions' src/` — **landed**; a package with pending gated emissions is Open until its gate resolves |
| Descendant fan-out | `children_index` via `GraphView` INHERITS_INV | reuse for "is this method overridden below me" |
| The certificate (an input, not the verdict) | `index/closedness_store.rs` | `grep -n 'ClosednessCertificate' src/` |

## Non-goals — do NOT do these

- Do NOT build instance brands, `main::` program-boundary analysis, or
  a `Symbol.home_namespace` field migration. This epic needs a QUERY
  ("openness of package P in file F"), not a stored field. If you find
  yourself running a serde migration on `Symbol`, you have overreached.
- Do NOT delete `universal_methods` / `meta_methods` / `RoleMask` —
  inputs, not competitors.
- Do NOT make the walk recursive over arbitrary lexical scopes. Perl
  lexical scopes do not affect method resolution; the chain is package
  → ancestry (+ bridges + app surface), which `parents_of` already
  enumerates. `Node::Scope` is justified ONLY if the implementation
  genuinely needs lexical nodes — the expected outcome is that it does
  not. **Record the decision either way in the ADR.**
- Do NOT merge with the closedness certificate (see above).

## Phase breakdown

### Phase A — the openness verdict, as data

1. New Model-layer API (`src/model/graph.rs` or a sibling — check
   `src/layering_tests.rs`' layer map and assign the file if new; an
   `.rs` outside a layer directory fails the walk):
   ```rust
   pub enum Openness { Open(OpenCause), Closed }
   pub enum OpenCause { Autoload, Role, UnresolvedAncestor,
                        PluginNamespace, AppSurface, DynamicParent,
                        PendingGatedEmission }
   ```
   `openness_of(class, analysis, module_index) -> Openness` walks
   ancestry via the existing `parents_of` seam — **never a second
   parent enumeration** — and answers Open on the FIRST open fact.
2. `OpenCause` exists for diagnostics text and tests: a verdict should
   be able to say WHY it stayed silent.
3. **Also verify the applicability matrix** in
   `prompt-enrichment-inheritance-residual.md` here: `gated_emissions`
   landed, so the doc's "two real gaps" header is stale. Confirm which
   matrix rows are closed, update the doc, and add
   `PendingGatedEmission` above only if a package can genuinely still
   be awaiting a gate at diagnostic time. If it cannot, drop the
   variant and say so — a cause nothing produces is dead weight.
4. **Take the member name.** `prompt-catch-all-surface.md` narrows a
   catch-all to the names it answers, so `Autoload` openness is per
   name and per member family, not per class. Give `openness_of` the
   name and family now, even while every cause ignores them; adding
   them after the lanes call it is a retrofit across every caller.
5. **Acceptance:** unit tests per cause, plus a Closed case (plain
   class, full local MRO, no bridges) that stays Closed.

### Phase B — unresolved-method/function rewired

1. In the diagnostics block: after the universal/meta-method skips and
   the local/workspace gates, replace the AUTOLOAD skip and the
   unresolved-ancestor skip with one `openness_of(class) == Open →
   continue`.
2. unresolved-function: replace the `framework_imports.contains` skip
   with openness of the ENCLOSING package. Keep `framework_imports` as
   the implementation detail *behind* the verdict if that is the honest
   mapping — but the diagnostic consults ONE function.
3. **Behavior must not regress:** substrate audit before and after.
   `unresolved-method` / `unresolved-function` counts may only go DOWN
   or stay equal; always-on `undef-deref` at exact parity.
4. **Acceptance:** existing diagnostics tests green unchanged; new
   tests asserting AUTOLOAD suppression now reports through the same
   path (message/absence identical to before).

### Phase C — qualified names resolve instead of hiding

1. `conventions::MethodToken` already parses the qualifier (FQ / SUPER
   / main). For `Super`, resolve against the enclosing class's
   PARENTS (`resolve_method_in_ancestors` starting from parents, not
   self). For `Qualified(pkg)`, resolve against `pkg`'s MRO.
2. Resolved → no diagnostic, and the ref gains a proper goto-def
   target if it lacks one. Check whether `resolve.rs` already answers
   these; if it does, **reuse its answer** — the CandidateSet is the one
   resolution entry point, not a place to re-resolve beside.
3. Unresolved AND the target package is Closed → the diagnostic fires
   where it used to stay silent. Run the substrate audit and **triage
   every new hit before merging**: each is either a real find (document
   it) or an openness fact missed in Phase A.
4. **Acceptance:** `$self->SUPER::real_parent_method()` silent;
   `$self->SUPER::typo()` in a Closed chain fires; `Some::Pkg->method`
   against a Closed resolvable package with no such method fires.

### Phase D — the open-world-dispatch gate

A base class's own method types `$self->m` (`sub meta_name { undef }`),
but runtime receivers are subclasses that override it — so "this guard
can never pass" is wrong.

1. In `guard_redundancies`: when the belief deciding a verdict came
   from a method call on a `$self`-like receiver, and `children_index`
   shows ANY descendant of the enclosing class overriding that method,
   downgrade the verdict to silence.
2. If threading belief provenance is too invasive, the coarser sound
   gate: suppress definitive verdicts on subjects assigned from a
   `$self`-receiver method call when the enclosing class HAS
   descendants. **Prefer the precise gate; document which you shipped.**
3. **Acceptance:** a two-file regression test (base with `sub meta_name
   { undef }` + subclass overriding it; a `defined $meta` guard in the
   base must NOT flag) and a measured drop in the audit's
   contradictory-guard count.
4. **Know which noise class is yours and which is Epic 16's.** Open-world
   dispatch is an *openness* problem and belongs here. **Correlated
   branches** — `if ($ok) { $x = init() } … if ($ok) { $x->use }` — are
   a *path-sensitivity* problem, named in `prompt-cfg-tier.md` §8.2 as
   "the dominant real-world FP class", killed by that tier's rung 1
   (SAT over atoms along the candidate trail) with no dependencies.
   Do not build a correlated-branch heuristic here; it would be a
   partial enumeration of a shape the CFG tier answers structurally
   (rule #10). Triage them into the audit as a named, deferred class
   instead.

### Phase E — promotion

1. Rerun the full substrate audit; record the numbers.
2. Flip `optionalDeref` default-on (INFO severity): the
   `DiagnosticOptions` default, the serde/CLI tests, and the ladder
   text in `adr/narrowing-diagnostics.md`.
3. `redundantGuard` / `contradictory`: flip if Phase D put the noise
   classes near zero; otherwise record precisely which class remains
   and leave them opt-in. If what remains is the correlated-branch
   class, say so by name — that is Epic 16's rung 1, and naming it
   turns "still noisy" into a scheduled fix. Do NOT flip `unresolvedMethodCrossFile` here
   — its ladder promotes it last, and the named-helper first-param-self
   gap in `gold-corpus/KNOWN-GAPS.md` is still open.
4. Write `docs/adr/openness.md`: the verdict enum, the single
   `parents_of`-seam walk, what subsumed what, the `Node::Scope`
   decision, the deliberate separation from the closedness certificate,
   and the promotion results.

## Language-pack beat

**This epic is Perl-flavored, and that is honest — but the flavor must
live in the causes, not in the walk.**

Pack languages have no diagnostics today, by deliberate policy:
`prompt-multi-language.md` says diagnostics stay off for a pack
language until a calibrated substrate exists, and the zero-false-
positive sweep is the ship gate (Epic 13 Phase B owns that). The one
exception is the C++ use-after-move channel, off by default
(`lsp/symbols/diagnostics.rs`). So nothing here fires for C/C++ today.

What that means for the design:

1. **`openness_of` must not be reachable from a language-agnostic path
   that assumes Perl's causes.** `Autoload` and `Role` are Perl facts;
   `PluginNamespace` and `AppSurface` are plugin facts; `DynamicParent`
   is a runtime-`@ISA` fact. A C++ class is Open for entirely different
   reasons — virtual dispatch through a base pointer, a member injected
   by a macro the extractor blanked, a symbol declared only in a
   translation unit outside the include closure.
2. So: put the verdict in the Model layer (it walks the graph), but let
   the **cause set be produced per language**. The cheapest honest
   shape is that `openness_of` consults facts already on the
   `FileAnalysis` — which is language-tagged — and a pack language
   simply produces none of the Perl causes and gets `Closed`. That is
   correct only because nothing consults the verdict for pack
   languages yet. **Write that dependency down in the ADR**, because
   Epic 13 Phase B is exactly the moment it stops being true, and a
   silent `Closed` default there is a false-positive flood.
3. Concretely, leave Epic 13 a hook: the ADR should name what a pack
   language must supply before its diagnostics consult this verdict.
   Note that openness is not the only thing missing on that side —
   `adr/narrowing-diagnostics.md` records that "D1/D2/D3/D4/D6 have no
   cpp facts", which is Epic 16's job, not this one's. A pack code that
   fails to promote may be waiting on either; say which.
   The C++ analogue is already sitting in the tree —
   `class_has_unresolved_ancestor`'s condition is "an ancestor we
   cannot see", which for C++ means an unresolved `#include`. That is
   one cause, cross-language, and it is the natural first one to
   generalize.
4. Phase C's qualified-name resolution is genuinely cross-language:
   `MethodToken`'s qualifier parsing is Perl `&str` semantics
   (`conventions.rs`, deliberately Perl-scoped), but "a written
   qualifier names a scope, resolve against that scope's MRO" is the
   same rule C++ needs for `Base::method()`. Do not generalize it here
   — do name it in the ADR as a known shared rule so it is not
   rediscovered.

## Scaling beat

**`openness_of` runs per unresolved reference site, and it walks
ancestry.** That is the most dangerous shape in this epic: the current
suppression pile is mostly string-set membership (O(1)) and this
replaces it with a graph walk.

Where that bites, measured:

1. **`--check` is a batch verb over every workspace file**
   (`scaling-limits.md` §1). FHEM does not complete `--check` on a
   31 GB machine today — a diagnostics change that adds a per-site
   ancestry walk lands directly on the verb that is already
   pathological. FHEM has 534 files providing `main`; every `main`
   lookup consults that candidate set.
2. **Memoize per (class, file), not per site.** A file's diagnostics
   pass hits the same enclosing class hundreds of times. The verdict is
   pure over (class, analysis, index generation), so a per-pass memo is
   sound and is the difference between "one walk" and "one walk per
   reference".
3. **Consult the closedness certificate before walking.** It already
   exists, it is byte-bounded (`ClosednessStore`, 8 MiB default), and
   its whole purpose is to answer "was this class's ancestry fully
   enumerable" without a decode. A class that fails to certify is
   `UnresolvedAncestor` — Open — with no walk at all. This turns the
   expensive case into the *cheap* case, which is the right way round.
4. **Measure on the right corpus.** Not `crm`. FHEM for the monoculture
   shape and Koha (3,554 files, the only corpus hitting DBIC and Mojo
   plugin paths together — minutes per round) for the normal shape.
   Three runs, dated, into `bench/`.
5. Phase C **adds** work: qualified names that were skipped by token
   shape now resolve. That is a new MRO walk per qualified call site.
   Budget it, and if the audit shows it dominating, cache the
   per-`(pkg, method)` resolution for the pass — but only after
   measuring, per the house rules.
6. Phase E's flag promotions change what `--check` emits by default,
   which changes what every CI running it pays. Note the new default
   cost in the PR.

## Invariants that MUST survive

- `parents_of` stays the single ancestor-enumeration seam.
- The CandidateSet stays the one resolution entry point — Phase C
  reuses its answers, never a parallel resolve.
- The closedness certificate stays a performance instrument whose
  staleness costs a validation, never a wrong answer. If this epic
  makes a diagnostic depend on it for *correctness*, the design is
  wrong.
- Always-on `undef-deref` at exact parity in every audit re-run.
- Every count that goes UP in the audit gets a per-site triage note in
  the PR.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS · `./e2e/run.sh` ·
substrate audit before/after with per-code deltas and triage ·
FHEM + Koha `--check` wall and peak RSS, three runs, dated.

## Sizing & sequencing

A → B, A → D independent after A; C after B (shares the diagnostic
block); E last. Expect C to surface the surprises — budget triage time
for the new `SUPER::` hits.
