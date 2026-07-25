# Epic 7 — Rename provenance: derived refs update their sources

> **Status:** scheduled (7th).
> **Design owner-doc:** `docs/prompt-ref-provenance.md` — the residual
> list this epic implements, including its proposed shapes and its
> three sketched regression tests (write all three).

## Mission

CLAUDE.md rule #9: derived refs trace to source. Today rename FINDS
derived call sites but cannot UPDATE what they were derived from:
`my $m = 'process'; $self->$m()` renames the sub and finds the call,
but leaves the `'process'` string literal behind — silently breaking
the program. This epic lands `Ref.folded_from`, the unified
framework-attribute rename group, verifies import-list rename, and
takes the two stretch goals (package→file rename, inheritance override
scoping) as explicitly separable phases.

## Read first

1. `docs/prompt-ref-provenance.md` — whole doc.
2. `docs/adr/resolution-candidate-set.md` — rename is a projection
   (`rename_edits()` / `renameable()`); ALL new grouping goes into
   CandidateSet construction, NEVER into the rename handler. This is
   the epic's one architectural landmine — the owner doc predates the
   CandidateSet; translate its "rename_sub checks folded_from" language
   into "the set's construction includes the folded-source span as an
   edit site".
3. `CLAUDE.md` rule #9 + the CandidateSet paragraph; `src/resolve.rs`
   module docs.
4. `grep -n 'attr_projections\|AttrProjection' src/file_analysis.rs | head`
   — the projection-group machinery that already unions attribute
   spellings; the framework-attribute phase EXTENDS this, it does not
   build a parallel group.

## Phase breakdown

### Phase A — `Ref.folded_from`

1. Add `folded_from: Option<Span>` to `Ref` (serde-default;
   EXTRACT_VERSION bump). The builder sets it when a ref's target name
   came from constant folding: `constant_strings` must carry the source
   literal's span alongside the value
   (`grep -n 'constant_strings' src/builder.rs` — change the map's
   value type to carry `(String, Span)`; the span follows the value
   through fold chains, FIRST source wins and document why: the
   literal the user must edit is where the name is spelled).
2. CandidateSet construction: when a member ref carries `folded_from`,
   the rename edit set includes an edit at that span (the string
   CONTENT span, not the quotes — reuse the content-span machinery
   `ArgInfo.content_span` uses).
3. Same shape for the lexical-hash-literal dynamic key the owner doc
   describes (`my $k = 'name'; my %h = ($k => 1);`):
   `emit_lexical_hash_literal_keys` currently skips non-literal keys;
   with `folded_from` it can emit the key ref carrying the `'name'`
   literal's span. Renaming the key rewrites the source literal, NOT
   `$k` — that inversion is the whole point; test it explicitly.
4. **Acceptance:** the owner doc's
   `test_constant_fold_rename_updates_source_string` — def + call site
   + string literal all edited; a references query on `process` lists
   the call site (unchanged behavior) — and gold rename rows stay green.

### Phase B — framework-attribute unified rename (verify-first)

The owner doc predates the attr-projection groups; much of this may
already work (this branch's history landed inheritance-aware attribute
groups). VERIFY FIRST:

1. Write the owner doc's `test_framework_attribute_unified_rename`
   (accessor + constructor key + internal `$self->{name}` + call
   sites, renamed from EVERY position) and run it. For each position
   that already passes, done. For failures, extend the projection
   group (`AttrProjection` + the group machinery in resolve.rs) —
   never a rename-handler special case.
2. **Acceptance:** the test green from all four positions; prepareRename
   returns the right range at each.

### Phase C — import-list rename (verify, pin)

Owner doc says "may already work". Write
`test_import_list_renamed_with_sub` (rename `sub bar` in Foo updates
`use Foo qw(bar)` + call sites). If green: commit the pin. If not: the
import-spec ref exists (`emit_refs_for_strings`) — the gap will be in
CandidateSet membership; fix there.

### Phase D — package rename → file rename (stretch, separable)

LSP `WorkspaceEdit.documentChanges` supports `RenameFile`. When the
rename target is a package/class symbol whose defining file's path
agrees with the package name (reuse Epic 5's path–package agreement
rank if it has landed; else a local check), append a `RenameFile`
operation to the computed new path. Guard: only when the file exists
and the new path does not; never move files whose path did NOT agree
with the old name (out-of-convention layouts stay untouched).
**Acceptance:** e2e test (this is client-visible surface — verify the
nvim harness supports documentChanges; if it does not, assert the
WorkspaceEdit JSON shape in a unit test and note the e2e gap).

### Phase E — inheritance override scoping (stretch, separable)

Renaming `Animal::speak` should offer `Dog::speak` (override family)
and must NOT touch `Unrelated::speak`. The pieces exist:
`children_index` (descendants) and the override-family machinery the
heatmap already counts through. Verify current behavior first —
`OverrideScope` env knob exists (`grep -rn 'OverrideScope' src/`);
this phase may be "wire the knob into LSP rename + default it
sanely" rather than new analysis. The name-collision NEGATIVE test
(`Unrelated::speak` untouched) is the acceptance bar.

## Non-goals

- Cross-file rename of DEPENDENCY files (RoleMask::EDITABLE stands).
- Renaming through dynamic dispatch that constant folding could NOT
  resolve — honest miss, never a guess.

## Verification gate

cargo test + gold (rename rows especially; author a folded_from gold
row via `--emit rename`) + the substrate audit at parity. Rename is
the highest-blast-radius verb: every phase lands with its negative
tests (what must NOT be edited) — Phase A: `$k` not renamed in the
hash-literal case; Phase D: disagreeing paths not moved; Phase E:
unrelated same-name subs untouched.

## Sizing

Medium. A is the bulk; B/C may be verification-only; D/E are
independent and individually droppable if QA pull shifts.
