# Epic 2 — Openness: one answer to "is this unresolved name real?"

> **Status:** scheduled, second. Runs after Epic 1 (no code dependency,
> but Epic 1 finishes the Now list first).
> **Design owner-docs:** `docs/prompt-graph-walking.md` §"Deferred:
> Scope nodes", `docs/open-problems.md` §"Qualified-name resolution
> suppression is coarse", `docs/adr/graph-walking.md` (the landed
> walker this builds on), and the promotion-audit note in
> `docs/ROADMAP.md` Now #1 (the evidence this epic monetizes).

## Mission

Every "couldn't resolve X" decision in the diagnostics currently uses a
different, partial suppression rule: a `framework_imports` string set, a
`universal_methods` + `meta_methods` list, an AUTOLOAD-in-MRO skip, an
unresolved-ancestor skip, a syntactic `SUPER::`/qualified-name skip, and
(the audit's remaining noise class) nothing at all for open-world
dispatch. This epic replaces the pile with ONE structural question the
graph answers — *walk outward from the reference site; if you reach an
OPEN namespace before exhausting CLOSED ones, stay silent; if every
namespace on the chain is closed and the name still doesn't resolve,
warn* — then uses the trust gained to flip diagnostic flags default-on
per the promotion path in `docs/adr/narrowing-diagnostics.md`.

## Read first, in this order

1. `CLAUDE.md` — rules #10 (this epic exists because of it), the
   resolution CandidateSet paragraph, "Inheritance & frameworks".
2. `docs/adr/graph-walking.md` — `GraphView`, the closed `EdgeKind`,
   exhaustive `edges_from`, why file roles are NOT graph nodes.
3. `docs/prompt-graph-walking.md` — the deferred Scope-node taxonomy;
   note its warning that scope parent-climbing is a linked list, so
   `Node::Scope` must be earned by Openness, not ported for its own sake.
4. `docs/adr/narrowing-diagnostics.md` — the flag ladder and promotion
   path.
5. `src/graph.rs` (172 lines, read all of it) and the unresolved-method
   block in `src/symbols.rs` (grep `unresolved-method`).

## Current state — exact anchors

| Suppression rule to subsume | Where | Find it |
| --- | --- | --- |
| `framework_imports` string set (unresolved-function) | `src/symbols.rs` | `grep -n 'framework_imports' src/symbols.rs` |
| `universal_methods` + `analysis.meta_methods` | `src/symbols.rs` | `grep -n 'universal_methods' src/symbols.rs` — KEEP both (they are genuine UNIVERSAL:: + plugin-declared facts), but they become inputs to the one walk, not a parallel path |
| AUTOLOAD-in-MRO skip | `src/symbols.rs` | `grep -n 'AUTOLOAD' src/symbols.rs` — this IS an openness fact (AUTOLOAD makes a class Open); fold it in |
| Unresolved-ancestor skip | `src/file_analysis.rs` | `grep -n 'class_has_unresolved_ancestor' src/` — an unresolvable parent makes the chain Open |
| `SUPER::`/qualified-name syntactic skip | `src/symbols.rs` | `grep -n 'SUPER::' src/symbols.rs` — replace with real resolution (below) |
| Open-world dispatch noise (D4) | `src/file_analysis.rs` | `grep -n 'fn guard_redundancies' src/file_analysis.rs` — the Software::License case from the audit: `$self->meta_name` typed by the base class's own all-undef method while runtime receivers are subclasses |
| Role-ness (already an openness fact) | `FileAnalysis.role_packages` / `is_role_package` | roles are Open by definition (`docs/adr/role-contracts.md`) |
| Descendant fan-out (already landed) | `children_index` via `GraphView` INHERITS_INV | used by goto-implementation; reuse for "is this method overridden below me" |

## Non-goals — do NOT do these

- Do NOT build instance brands, `main::` program-boundary analysis, or
  `Symbol.home_namespace` field migrations. `home_namespace` is listed
  in the owner doc as a possible future; this epic needs only a QUERY
  (`openness of package P in file F`), not a stored field. If you find
  yourself running a serde migration on `Symbol`, you have overreached.
- Do NOT delete `universal_methods`/`meta_methods`/`RoleMask` — they
  are inputs, not competitors.
- Do NOT make the walk recursive over arbitrary scopes. Perl lexical
  scopes don't affect method resolution; the namespace chain here is
  package → ancestry (+ bridges + app surface), which `parents_of`
  already enumerates. `Node::Scope` is justified ONLY if the
  implementation genuinely needs lexical nodes — the expected outcome
  is that it does NOT, and the taxonomy stays package-level. Record
  the decision either way in the ADR you write.

## Phase breakdown

### Phase A — the openness verdict, as data

**Goal:** a single function answers Open/Closed for a class, from facts
that already exist.

1. New module-level API (suggest `src/graph.rs` or a sibling that stays
   in the Model layer — check `src/layering_tests.rs` `layer_map` and
   assign the file if new):
   ```rust
   pub enum Openness { Open(OpenCause), Closed }
   pub enum OpenCause { Autoload, Role, UnresolvedAncestor,
                        PluginNamespace, AppSurface, DynamicParent }
   ```
   `openness_of(class, analysis, module_index) -> Openness` walks the
   ancestry via the existing `parents_of` seam (NEVER a second
   parent enumeration — CLAUDE.md names `parents_of` as the single
   seam) and answers Open on the FIRST open fact:
   - `AUTOLOAD` resolves anywhere in the MRO,
   - any ancestor is a role (`is_role_package`) or unresolvable
     (`class_has_unresolved_ancestor`'s condition, inlined here so the
     old helper can eventually retire),
   - the class participates in a plugin namespace bridge or the app
     surface (`for_each_entity_bridged_to` non-empty /
     `app_surface_consumers`),
   - `dynamic_parent_packages` contains it (runtime `@ISA` mutation).
2. `OpenCause` is for diagnostics text and tests — the verdict message
   should say WHY it stayed silent when a `--verbose`-ish path wants it.
3. **Acceptance:** unit tests per cause, plus a Closed case (plain
   class, full local MRO, no bridges) that stays Closed.

### Phase B — unresolved-method/function rewired to the verdict

1. In the `symbols.rs` diagnostic block: after the existing universal/
   meta-method skips and the local/workspace gates, replace the
   AUTOLOAD skip + unresolved-ancestor skip with one
   `openness_of(class) == Open → continue`.
2. unresolved-function: replace the `framework_imports.contains` skip
   with openness of the ENCLOSING package (a package that `use`s a
   framework whose plugin injects keywords is Open to bare-name calls —
   derive this from `package_uses` × plugin triggers, which
   `framework_imports` already approximates; keep `framework_imports`
   as the implementation detail behind the verdict if that is the
   honest mapping, but the diagnostic consults ONE function).
3. **Behavior must not regress:** run the substrate audit (commands in
   Epic 1 Phase E) before and after; `unresolved-method` /
   `unresolved-function` counts may only go DOWN or stay equal, and the
   always-on `undef-deref` must be at exact parity.
4. **Acceptance:** existing `symbols_tests.rs` d8/meta tests green
   unchanged; new tests: AUTOLOAD-suppression now reports through the
   same path (assert message/absence identical to before).

### Phase C — qualified names resolve instead of hiding

The `open-problems.md` item: `SUPER::method` and `Pkg::method` refs are
skipped by token shape. Fix:

1. `conventions::MethodToken` already parses the qualifier (FQ / SUPER /
   main). For `MethodToken::Super`, resolve the method against the
   enclosing class's PARENTS (`resolve_method_in_ancestors` starting
   from parents, not self). For `MethodToken::Qualified(pkg)`, resolve
   against `pkg`'s MRO.
2. Resolved → no diagnostic (and, bonus, the ref gains a proper target
   for goto-def if it lacks one — check `resolve.rs` handles these; if
   it already does, reuse its answer rather than re-resolving: the
   CandidateSet is the one resolution entry point).
3. Unresolved AND the target package is Closed → the diagnostic now
   fires where it used to stay silent. Run the substrate audit; triage
   every new hit BEFORE merging (each is either a real find — document
   it — or an openness fact you missed in Phase A).
4. **Acceptance:** unit tests: `$self->SUPER::real_parent_method()`
   silent; `$self->SUPER::typo()` in a Closed chain fires;
   `Some::Pkg->method` against a Closed resolvable package with no such
   method fires.

### Phase D — the open-world-dispatch gate on D4

The audit's remaining contradictory-guard noise: a base class's own
method types `$self->m` (e.g. `sub meta_name { return undef }`), but
runtime receivers are subclasses that override it, so "guard can never
pass" is wrong.

1. In `guard_redundancies`: when the belief that decides a verdict came
   from a method call on `$self`-like receivers (the belief's
   provenance is a `MethodOnClass{enclosing_class, m}` resolution — you
   will need to thread WHERE the belief came from; the cheapest honest
   signal is: the guarded subject was assigned from
   `$self->method(...)` and `children_index` shows ANY descendant of
   the enclosing class overriding `method`), downgrade the verdict to
   silence.
2. If threading provenance is too invasive, the coarser sound gate:
   suppress definitive verdicts on subjects assigned from a
   `$self`-receiver method call when the enclosing class HAS
   descendants (in workspace or index). Prefer the precise gate;
   document which you shipped.
3. **Acceptance:** a two-file unit test reproducing Software::License
   (base with `sub meta_name { undef }` + subclass overriding it;
   `defined $meta1` guard in the base must NOT flag); the substrate
   audit's contradictory-guard count drops by at least the
   open-world-dispatch entries recorded in the ROADMAP audit note.

### Phase E — promotion

1. Rerun the full substrate audit. Update the ROADMAP audit note with
   the new numbers.
2. Flip `optionalDeref` default-on (INFO severity): change the
   `DiagnosticOptions` default, the serde/CLI tests in
   `symbols_tests.rs`, and `docs/adr/narrowing-diagnostics.md`'s ladder
   text. The evidence bar (from the existing audit note): ~35 hits over
   the whole substrate, all honest productions.
3. `redundantGuard`/`contradictory`: if Phase D's numbers put the
   noise classes at ~zero, flip them too; otherwise record precisely
   which class remains and leave them opt-in. Do NOT flip
   `unresolvedMethodCrossFile` in this epic (its ladder says it
   promotes last; the named-helper first-param-self gap in
   `gold-corpus/KNOWN-GAPS.md` is still open).
4. Write the ADR: `docs/adr/openness.md` — the verdict enum, the single
   `parents_of`-seam walk, what subsumed what, the Node::Scope
   decision from the Non-goals box, and the promotion results.

## Invariants that MUST survive

- `parents_of` stays the single ancestor-enumeration seam.
- The CandidateSet stays the one resolution entry point — Phase C
  reuses its answers, never a parallel resolve.
- Always-on `undef-deref` at exact parity in every audit re-run.
- New diagnostics behavior = fewer or equal false positives, never a
  silent new class of them: every count that goes UP in the audit gets
  a per-site triage note in the PR.

## Sizing & sequencing

A → B and A → D are independent after A; C after B (shares the
diagnostic block); E last. Expect C to surface the surprises — budget
triage time for the new SUPER:: hits.
