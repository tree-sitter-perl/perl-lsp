# Epic 11 — Program boundaries: file→program assignment + `main::` unification

> **Status:** scheduled (11th). Half-gated: the file→program assignment
> and MAIN-1 are unblocked NOW; the instance-brand consumer additionally
> waits on Epic 4 plus constructor/field flow.
> **Design owner-docs:** `docs/prompt-entrypoint-analysis.md` (the
> program-boundary concept and the landed conservative fallback) and
> `docs/open-problems.md` §"`main::` aggregation across `require` of
> package-less scripts" (the require-edge design and its recommendation,
> which this epic implements as option 1).

## Mission

Give the analyzer the notion of a **program**: which entrypoint(s) a
file belongs to, via each entrypoint's statically-resolvable
`use`/`require`/`do` closure. Two consumers land with it:

1. **MAIN-1** — package-less files `require`d into a script share its
   `main::`; unqualified calls resolve along require edges (the AWStats
   shape, ~270 false positives each direction) **without** unifying
   unrelated scripts' `main::` — every `t/*.t` keeps its own.
2. **`main::` rename fan-out lift** — the deliberately-file-local `main`
   fallback in resolve widens to program-scoped.

## Read first

1. Both owner docs, whole.
2. `CLAUDE.md` §"Workspace indexing" — `scan_entrypoint_scripts` and its
   `extra: &[String]` seam (reserved for a workspace-config
   `entrypoint_dirs`; if Epic 7's config landed, this epic wires that
   key). Note the standing warning: **do not broaden the entrypoint scan
   to a recursive walk** — it would enumerate non-Perl source trees.
3. `src/index/resolve/` — the `package == "main"` file-local arm
   (`grep -rn '"main"' src/index/resolve/`).
4. `docs/adr/heatmap.md` §failure modes — entrypoint-script free-subs
   are deliberately listed as dead candidates *pending this tier*; this
   epic upgrades that floor.

## Phase breakdown

### Phase A — the require/do edge, as data

1. During the walk (rule #1), record load edges the builder can see:
   `require "literal/path.pl"`, `require Bare::Module`, `do
   "literal/path"`, and constant-folded variable forms (`require $file`
   where `$file` folds — the same folding `Ref.folded_from` rides). A
   dynamic path that does not fold is an honest miss; degrade silently.
   **`require Foo::Bar` has NO `module:` field** unlike `use_statement`
   — match structurally, and note `require 5.010` / `require v5.36` are
   a different kind entirely (`require_version_expression`).
2. Store on the FA in its lane: `load_edges: Vec<LoadEdge { target,
   span }>`, serde-default, `EXTRACT_VERSION` bump. **`use` already
   populates `imports` — do NOT duplicate it**; `LoadEdge` is for the
   require/do path forms `imports` does not carry.
3. `surface_feed` will not compile until the field's Surface fate is
   decided: load edges ARE cross-file-visible (they change what a
   program contains), so they belong in the projection — which means a
   Surface equality-net arm too.
4. **Acceptance:** unit tests per form, including the folded-variable
   case and a non-folding dynamic path producing nothing.

### Phase B — program assignment

1. At workspace-index completion (the resolver's post-index hook),
   compute programs: for each entrypoint from `scan_entrypoint_scripts`
   (+ configured `entrypoint_dirs` if available), BFS its closure over
   `imports` ∪ `load_edges`. Resolve paths conservatively — try the
   entrypoint's own dir first, then the workspace root; unresolvable →
   skip. A file reachable from N entrypoints belongs to all N; a file
   reachable from none belongs to a synthetic "unassigned" program.
2. Store where resolve-time code can read it (`path → SmallVec<ProgramId>`
   on the index or a sibling), rebuilt on watcher changes with the
   hooks indexing already uses. **This is derived state — never
   serialized into per-file FAs.** It is workspace-shaped, not
   file-shaped, and recompute is cheap.
3. **Acceptance:** a fixture workspace with two scripts requiring
   disjoint plugin files plus one shared library — assignments come out
   `{A: script1}`, `{B: script2}`, `{lib: both}`.

### Phase C — MAIN-1 consumption

1. Unqualified-call resolution for `main`-package files consults the
   program's other `main` files: extend the resolve `main` arm —
   same-file first (current behavior), then same-program `main::`
   symbols. Bounded by the program set; **NO workspace-wide `main`
   union, ever** (the owner doc: modeling it wrong is worse than the
   false positive).
2. The unresolved-function diagnostic gains the same visibility, so the
   AWStats-shaped FPs drop.
3. Rename fan-out for `main` globals/subs widens file-local →
   program-scoped, still never cross-program. **The negative test is
   mandatory:** two scripts each with `our $x` / `sub helper` — rename
   in one MUST NOT touch the other.
4. **Acceptance:** an AWStats-shaped fixture (host script + `require`d
   package-less plugin, calls both directions) — goto-def, references
   and diagnostics all resolve across the pair; the negative pair above;
   substrate audit at parity-or-better.

### Phase D — heatmap upgrade + docs

1. Entrypoint-reachable `main` free-subs: a sub whose program's
   entrypoint calls it is reachable — fan-in flows through the normal
   reference graph now that same-program `main` calls resolve. **No new
   guard needed**; update `adr/heatmap.md`'s deliberate-listing note to
   reflect the improved floor.
2. Update `prompt-entrypoint-analysis.md` (fallback lift landed;
   instance brands remain parked on their own gate) and
   `open-problems.md` (MAIN-1 → landed; keep the duplicate-package
   section's own state accurate — Epic 1 owns it).
3. ADR `docs/adr/program-boundaries.md`: the closure rules, the
   relative-path resolution order, multi-membership, the "unassigned"
   program, and the non-goals below.

## Non-goals

- Instance brands / per-app helper surfaces — parked on Epic 4 plus
  constructor/field flow. This epic only supplies the file→program key
  they will eventually use.
- No runtime `@INC` emulation; no execution of config to resolve dynamic
  require paths.
- **No cross-program anything.** The entire point is isolation by
  default, sharing only along proven edges.

## Language-pack beat

**A program boundary is not a Perl concept, and the C++ analogue is
already in the tree — under a different name, with a different owner.
Read it before designing Phase B.**

The C++ equivalent of "which entrypoint's closure is this file in" is
**the include closure**, and it is landed:

- `VisibilityAxis::IncludeClosure` is the visibility rule that already
  models C's flat linkage, sitting beside Perl's `SearchPath` in the one
  derivation (`VisibilityAxis::for_origin`).
- `PackFacts` carries include directives **and the closure**.
- `index/pack_invalidator.rs` is the ONE owner of pack-file
  invalidation, and it holds "the single `is_consumer` include-closure
  rule" — which is exactly Phase B's question ("which files does a
  change here affect") answered for the pack side.

So the honest framing for this epic:

1. **Perl's program closure and C++'s include closure are the same
   abstraction with different edge sources.** Both are: a derived,
   workspace-shaped, watcher-rebuilt reachability relation over
   per-file load edges, consulted at resolve time and never serialized
   per file. Phase B is building the second instance.
2. **Do not unify them in this epic** — the pack side is landed, in
   production, and owns invalidation semantics this epic has no reason
   to touch. But **do write Phase B so a unification is possible**: keep
   the closure computation separate from the *Perl* edge extraction,
   and give the ADR a section naming the pack sibling and what a
   future merge would need. `prompt-scale-validation-hitlist.md` Tier 3
   already lists "merge the two index families" as OPEN; this is the
   same shape of debt, and adding a third unmergeable closure is how it
   gets worse.
3. **Concretely reusable now:** the incremental-rebuild discipline.
   `PackInvalidator` already solved "recompute a closure on a watcher
   event without a storm" — serialization lock, bulk-index defer/
   reconcile, source-generation guard. Phase B's watcher rebuild should
   study it rather than re-derive it, and the ADR should say which parts
   it adopted.
4. `scan_entrypoint_scripts` is Perl-specific (a shebang scan over root
   + `bin/` + `script/`). A pack language's entry points come from its
   own entry document — the heatmap already reads those
   (`entry_markers_for`). Keep the Perl scanner where it is; if Phase B
   needs "the entry points for this workspace", route through the entry
   documents so a pack language answers for itself.

## Scaling beat

**Phase B computes a BFS closure over the whole workspace and rebuilds
it on file changes. That is a new workspace-scale derived structure,
and the corpus that will break it is FHEM.**

The measurement (`scaling-limits.md` §1, 2026-08-17): FHEM is 973 Perl
files, 991k LOC, with **`fhem.pl` `do`-loading all of them into a single
interpreter** and 361 of them calling into `fhem.pl`'s `main`. That is
not a pathological input to this epic — **it is this epic's canonical
input**, and it is a single program with ~600 members.

Obligations:

1. **Measure Phase B on FHEM first, not last.** A closure with a
   600-member program is the design point, not the edge case. Report the
   closure computation's wall and the resulting map's size.
2. **Phase C's `main` resolution consults the program's other `main`
   files.** On FHEM that is ~534 files providing `main`, and `main` is
   27% of package lookups and **94% of provider fetches**. This is the
   same relation Epic 1 is converting consumers onto, and this epic adds
   a consumer. Two consequences:
   - **Coordinate with Epic 1.** If Epic 1 has landed, Phase C uses
     `visible_def_candidates` narrowed by program membership — which is
     strictly *better* than today, because the program set is smaller
     than the workspace set. If Epic 1 has not landed, Phase C must not
     introduce a second candidate-enumeration path.
   - The program boundary is, for FHEM, a **narrowing**: it replaces
     "any `main` in the workspace" with "any `main` in this program".
     For most workspaces that is a big win. For FHEM it is a no-op,
     because there is one program. Say that plainly in the ADR — this
     epic does not fix FHEM.
3. **The rebuild must be incremental, not a full recompute per watcher
   event.** A `did-change` on one file in a 973-file program that
   recomputes the whole closure is an editor stall. Study
   `PackInvalidator`'s defer/reconcile coordination (Language-pack beat,
   point 3).
4. **The map is derived and must be bounded like every other derived
   store.** CLAUDE.md's residency discipline: bounded caches only, byte
   accounted. `path → SmallVec<ProgramId>` for 138k files is small, but
   state the number rather than assuming it.
5. Phase C changes what the unresolved-function diagnostic emits, which
   changes `--check`'s output on every workspace with entrypoint
   scripts. Substrate audit, per-code deltas, triage anything up.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS ·
`./e2e/run.sh` · substrate audit at parity-or-better · the fixture
suite above, **including the cross-program negative rename test** ·
watcher incrementality: touch a require line in the fixture and assert
reassignment without a full restart · FHEM closure wall + map size,
three runs, dated.

## Sizing

Medium. A small; B is the design core; C is the payoff; D is cleanup.
One PR per phase, or A+B together.
