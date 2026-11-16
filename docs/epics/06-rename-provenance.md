# Epic 6 — Rename provenance: the residual

> **Status:** Phases A and B open; Phase C's mechanism is landed and
> only its negative pin is owed (audited 2026-11-12). The headline
> phase (`folded_from`) landed before this epic was cut.
> **Design owner-doc:** `docs/prompt-ref-provenance.md` §"What's still
> missing" (three items; the doc has already been trimmed to them).

## What landed, so nobody rebuilds it

**`Ref.folded_from` is done.** Constant-fold rename provenance works:
`my $m = 'process'; $self->$m()` — renaming the sub rewrites the source
string literal, not just the call site. The field is on `Ref`
(`model/file_analysis/core_types.rs`), the edit site is added during
CandidateSet construction (`index/resolve/collect.rs`), and it is pinned
by a rename-group test (`index/resolve/tests/groups_tests.rs`).

The framework-attribute unified rename group also landed via the
attr-projection machinery. **Verify before assuming otherwise** — this
epic's first job is to confirm what is already green rather than to
write it again.

## Mission

Three residuals, each independently shippable:

1. **Import-list rename** — verify and pin.
2. **Package rename → file rename** — the LSP `RenameFile` operation.
3. **Inheritance override scoping** — rename `Animal::speak`, offer
   `Dog::speak`, never touch `Unrelated::speak`.

## Read first

1. `docs/prompt-ref-provenance.md` — whole doc (it is short now).
2. `docs/adr/resolution-candidate-set.md` — rename is a projection
   (`rename_edits()` / `renameable()`). **ALL new grouping goes into
   CandidateSet construction, NEVER into the rename handler.** This is
   the epic's one architectural landmine.
3. `CLAUDE.md` rule #9 + the CandidateSet paragraph; `src/index/resolve/`
   module docs. Per-feature policy on a target is a method on
   `TargetRef` (e.g. `supports_cross_file_rename`) — never a
   `RenameKind`→`TargetRef` map inlined in a handler.

## Phase breakdown

### Phase A — import-list rename (verify, pin)

Write the owner doc's `test_import_list_renamed_with_sub`: renaming
`sub bar` in `Foo` updates `use Foo qw(bar)` plus call sites. If green,
commit the pin and move on. If not, the import-spec ref already exists
— the gap will be in CandidateSet **membership**, so fix it there.

### Phase B — package rename → file rename

LSP `WorkspaceEdit.documentChanges` supports `RenameFile`. When the
rename target is a package/class symbol whose defining file's path
agrees with the package name, append a `RenameFile` operation for the
computed new path.

**Guards, all mandatory:**

- Only when the file exists and the new path does not.
- Never move a file whose path did NOT agree with the old name —
  out-of-convention layouts stay untouched.
- **Only when the package has exactly one provider.** Epic 1 makes this
  question answerable: a reopened package declared in several files has
  no single "defining file" to rename, and moving one of them is a
  confident wrong action on a user's disk. Ask
  `visible_def_candidates`; if it returns more than one, refuse the
  file operation and rename the symbol only. This is the interlock that
  makes Epic 1 a soft prerequisite.

**Acceptance:** an e2e test if the nvim harness supports
`documentChanges`; if it does not, assert the `WorkspaceEdit` JSON shape
in a unit test and note the e2e gap explicitly.

### Phase C — inheritance override scoping

Renaming `Animal::speak` should offer `Dog::speak` (the override family)
and must NOT touch `Unrelated::speak`.

**The mechanism is landed.** `OverrideScope` (`index/resolve/target.rs`)
defaults to `Hierarchy` — the override family, gathered over `@ISA`
edges, never by name — and LSP rename reads it from the client's
`rename.overrideScope` setting (`lsp/backend/server.rs`); `Dispatch` is
the precise alternative. `override_scope_hierarchy_unions_dispatch_is_precise`
(`index/resolve/tests/groups_tests.rs`) pins the family.

What is owed is the name-collision NEGATIVE test, which is still the
acceptance bar: `Unrelated::speak`, a same-named method on a class
outside the hierarchy, stays untouched.

## Non-goals

- Cross-file rename of DEPENDENCY files (`RoleMask::EDITABLE` stands).
- Renaming through dynamic dispatch that constant folding could NOT
  resolve — an honest miss, never a guess.

## Language-pack beat

**Rename is the verb where a Perl-only assumption becomes a corrupted
source file, so the pack side is not optional here.**

- Rename already serves pack languages through the same projection.
  C++ rename went through a "full-or-refuse" hardening
  (`cpp-golive-map.md` wave 1, finding C2): a rename that cannot rewrite
  every site **refuses** rather than rewriting some. Phase B must obey
  the same rule — a `RenameFile` that lands while some reference to the
  old path does not get rewritten is exactly that failure with a worse
  blast radius.
- **Phase B is the phase to think hardest about.** "Package name agrees
  with file path" is a Perl convention (`Foo::Bar` → `Foo/Bar.pm`). The
  equivalent exists in other languages with entirely different rules,
  and some have none at all. So: **derive the agreement from the
  language, not from a hardcoded `::`→`/` transform.** The natural home
  is beside the existing per-language path knowledge — the driver
  already knows a language's file extensions and its visibility axis.
  If a language declares no path convention, Phase B does nothing for
  it, which is the correct answer.
- Phase C is genuinely cross-language: `children_index` /
  `GraphView` INHERITS_INV is the shared descendant walk, and C++ has
  the same override-family question for virtual methods. Do not build a
  Perl-side override walk — use the graph, and the pack languages
  inherit it. `EdgeKind` is closed and `edges_from` is exhaustive, so
  this is enforced rather than hoped for.
- Known pack residual, do not re-report it: **pack rename through
  aliases** is listed in `gold-corpus/KNOWN-GAPS.md` under "Refs
  symmetry" as non-rewritable and deliberately not renamed.

## Scaling beat

**Rename's cost is references' cost, and references is the one Tier-1
scaling row still open.**

`prompt-scale-validation-hitlist.md` Tier 1 #3, measured 2026-08-17 and
re-confirmed on the server path at 138k files: `references` **RETURNS**
(it used to never return, at 7+ GB) but takes **265–368 s at 2.8 GB
peak**, and marks the answer incomplete. Rename is that walk plus edit
construction.

Obligations:

1. **Do not widen the reference walk.** Phase C's override family is a
   fan-out over descendants; on a deep hierarchy that multiplies the
   candidate set. Bound it — the graph walk is lazy (`GraphView::walk`),
   so consume it lazily rather than collecting descendants first.
2. **Phase B is cheap and must stay cheap.** The single-provider check
   is one `visible_def_candidates` call; do not turn it into a workspace
   scan for "who references this path".
3. **Measure rename, not just references.** `bench/lsp_bench.py`
   scenarios drive the editor surface; if no rename scenario exists,
   add one and land the baseline in `bench/baselines.jsonl` via
   `seed-baselines.py`. Three runs, dated, quiet box.
4. The honest framing to preserve in any doc this epic touches: rename
   at 138k scale is *bounded and slow*, not fast. Epic 15 owns making it
   fast. This epic must not make it slower.

## Verification gate

`cargo test` (both feature sets) · gold, including a rename row authored
with `--emit rename` · `./e2e/run.sh` · substrate audit at parity.

**Rename is the highest-blast-radius verb: every phase lands with its
negative tests** — what must NOT be edited. Phase B: disagreeing paths
not moved, multi-provider packages not moved. Phase C: unrelated
same-name subs untouched.

## Sizing

Small-to-medium. A is verification-only if it passes. B and C are
independent and individually droppable if QA pull shifts.
