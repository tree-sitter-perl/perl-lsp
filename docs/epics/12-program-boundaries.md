# Epic 12 — Program boundaries: file→program assignment + `main::` unification (MAIN-1)

> **Status:** scheduled (12th, last of the current slate). Half-gated:
> the file→program assignment and MAIN-1 are unblocked NOW; the
> instance-brand consumer additionally waits on Epic 3 + the
> constructor/field-flow follow-on.
> **Design owner-docs:** `docs/prompt-entrypoint-analysis.md` (the
> program-boundary concept and the landed conservative fallback) and
> `docs/qa-design-items.md` §MAIN-1 (the require-edge design and its
> recommendation, which this epic implements as option 1).

## Mission

Give the analyzer the notion of a **program**: which entrypoint(s) a
file belongs to, via each entrypoint's statically-resolvable
`use`/`require`/`do` closure. Two consumers land with it:

1. **MAIN-1** — package-less files `require`d into a script share its
   `main::`; unqualified calls resolve along require edges (the
   AWStats shape, ~270 FPs each direction), WITHOUT unifying unrelated
   scripts' `main::` (every `t/*.t` keeps its own).
2. **`main::` rename fan-out lift** — the deliberately-file-local
   `main` fallback in `resolve.rs` widens to program-scoped.

## Read first

1. Both owner docs, whole.
2. `CLAUDE.md` "Workspace indexing" — `scan_entrypoint_scripts` and
   its `extra: &[String]` seam (reserved for workspace-config
   `entrypoint_dirs`; if Epic 8's `.perl-lsp.json` landed, this epic
   wires that key).
3. `src/resolve.rs` — the `package == "main"` file-local arm
   (`grep -n '"main"' src/resolve.rs`).
4. `docs/prompt-heatmap.md` §"Honest failure modes" — entrypoint-script
   free-subs are deliberately listed as dead candidates pending THIS
   tier; this epic upgrades that.

## Phase breakdown

### Phase A — the require/do edge, as data

1. During the walk (rule #1), record load edges the builder can see:
   `require "literal/path.pl"`, `require Bare::Module`,
   `do "literal/path"`, and the constant-folded variable forms
   (`require $file` where `$file` folds via `constant_strings` — the
   same folding rename provenance uses; a dynamic path that doesn't
   fold is an honest miss, per the owner doc's "degrade silently").
   Store on FA: `load_edges: Vec<LoadEdge { target: PathOrModule,
   span }>` (serde-default; EXTRACT_VERSION bump). `use` already
   populates `imports` — do NOT duplicate it; LoadEdge is for
   require/do path forms that `imports` doesn't carry.
2. **Acceptance:** unit tests per form incl. the folded-variable case
   and a non-folding dynamic path producing nothing.

### Phase B — program assignment

1. At workspace-index completion (module_resolver's post-index hook —
   where the resolver refresh callback lives), compute programs:
   for each entrypoint from `scan_entrypoint_scripts` (+ configured
   `entrypoint_dirs` if available), BFS its closure over
   `imports` ∪ `load_edges` (paths resolved workspace-relative and
   against the entrypoint's own dir — match Perl's `do`/`require`
   relative semantics conservatively: try entrypoint-dir first, then
   workspace root; unresolvable → skip). A file reachable from N
   entrypoints belongs to all N; a file reachable from none belongs
   to a synthetic "unassigned" program.
2. Store the assignment where resolve-time code can read it (the
   ModuleIndex or a sibling map `path → SmallVec<ProgramId>`), rebuilt
   on watcher changes with the same incremental hooks indexing uses.
   This is derived state — never serialized into per-file FAs (it is
   workspace-shaped, not file-shaped; recompute is cheap).
3. **Acceptance:** fixture workspace with two scripts requiring
   disjoint plugin files + one shared library — assignments come out
   {A: script1}, {B: script2}, {lib: both}.

### Phase C — MAIN-1 consumption

1. Unqualified-call resolution for `main`-package files consults the
   program's other `main` files: extend the `resolve.rs` `main` arm —
   same-file first (current behavior), then same-program `main::`
   symbols. Bounded by the program set; NO workspace-wide `main`
   union, ever (the owner doc's "modeling it wrong is worse than the
   FP").
2. The unresolved-function diagnostic gains the same visibility, so
   the AWStats-shaped FPs drop.
3. Rename fan-out for `main` globals/subs widens file-local →
   program-scoped, still never cross-program. The negative test is
   mandatory: two scripts each with `our $x` / `sub helper` — rename
   in one MUST NOT touch the other.
4. **Acceptance:** an AWStats-shaped fixture (host script +
   `require`d package-less plugin, calls both directions) — goto-def,
   references, and diagnostics all resolve across the pair;
   the negative-pair test above; substrate audit (if any substrate
   module is affected) at parity-or-better.

### Phase D — heatmap upgrade + docs

1. Entrypoint-reachable `main` free-subs: a sub in a program whose
   entrypoint's top-level statements call it is reachable — feed
   fan-in through the normal reference graph now that same-program
   `main` calls resolve (no new guard needed; the guard table's
   deliberate listing note in `prompt-heatmap.md` gets updated to
   reflect the improved floor).
2. Update `docs/prompt-entrypoint-analysis.md` (the fallback-lift
   landed; instance brands remain parked on their own gate) and
   `docs/qa-design-items.md` (MAIN-1 → landed, keep H1's own state
   accurate).
3. ADR: `docs/adr/program-boundaries.md` — the closure rules, the
   relative-path resolution order, multi-membership, the "unassigned"
   program, and the explicit non-goal below.

## Non-goals

- Instance brands / per-app helper surfaces — still parked on the
  value-provenance tier (Epic 3) plus constructor/field flow; this
  epic only supplies the file→program key they will eventually use.
- No runtime `@INC` emulation; no execution of config to resolve
  dynamic require paths.
- No cross-program anything: the entire point is isolation by default,
  sharing only along proven edges.

## Verification gate

cargo test + gold + substrate audit at parity-or-better + the fixture
suite above. Watcher incrementality: touch a require line in the
fixture, assert reassignment without full restart (mirror how existing
watcher tests assert index updates, if any exist — else a unit test on
the recompute function).

## Sizing

Medium. A small; B the design core; C the payoff; D cleanup. One PR
per phase or A+B together.
