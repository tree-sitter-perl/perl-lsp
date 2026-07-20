# Epic 11 — CLI analysis subcommands + `--migrate`

> **Status:** scheduled (11th).
> **Design owner-doc:** `docs/prompt-cli-tools.md` §"Analysis
> subcommands still missing" and §"`--migrate`" (targets table,
> implementation shape, phase order).

## Mission

Round out the CLI: the thin-wrapper analysis subcommands over existing
`FileAnalysis` queries, then the marquee `--migrate` (framework
translation via span-based edits), in the owner doc's easy→ambitious
order.

## Read first

1. `docs/prompt-cli-tools.md` — the two sections.
2. `src/main.rs` — the CLI dispatch `match`, `cli_full_startup`,
   and an existing thin subcommand (`cli_heatmap` or
   `cli_workspace_symbol`) as the template.
3. For `--migrate`: `CLAUDE.md` on `FileAnalysis` being the semantic
   model; how `--rename` builds span-based `WorkspaceEdit`s (grep
   `cli_rename` in main.rs) — migrate reuses that edit-application
   shape.

## Phase breakdown

### Phase A — thin analysis subcommands

Each is a small `cli_*` fn + a `--help` entry + tests; implement in
this order and keep each a separate commit:

1. `--completions <root> <file> <line> <col>` — the LSP completion
   engine at a position (mirror `--signature-help`'s plumbing; the
   0-based input / 1-based output coordinate contract from `--help`
   applies — copy an existing position-taking subcommand EXACTLY).
2. `--dependency-graph <root> [--format dot|json|list]` — module
   import edges from each FA's `imports` (+ `package_parents` as a
   distinct edge kind in dot/json); cycles via DFS reported in all
   formats.
3. `--export-api <root> <module> [--format json|markdown]` — exports,
   params, return types, parents, framework, from the cached FA
   (`--dump-package` is the debugging sibling; this is the
   user-facing one — share extraction where trivial).
4. `--impact <root> <file-or-module>` — reverse deps: who imports /
   inherits from this (the reverse index + `children_index` already
   answer both; this is formatting).
5. `--framework-report <root>` — classes by framework
   (`package_framework` + parents), counts + list.
6. `--unused-exports` / `--dead-code`: implement as ALIASES over the
   Epic 8/9 machinery if those landed (PL005/PL006 + heatmap guards);
   if this epic runs first, SKIP them here and leave the pointer —
   do not build a third dead-code path.
7. `--repl-complete` — stdin-accumulated source, complete at EOF.
   Lowest priority; drop if time-boxed out.

**Acceptance per subcommand:** a golden-output test on a fixture
workspace; `--help` text updated; stderr/stdout discipline preserved
(chatter to stderr, data to stdout — the heatmap's pattern).

### Phase B — `--migrate` step 1: `use base` → `use parent`

The deliberately-trivial first target to build the harness:

1. Harness: `cli_migrate(root, target, from, to, dry_run)` — index,
   select files by `--from` detection (the FA knows its frameworks),
   produce span edits, `--dry-run` prints a unified diff (reuse or
   hand-roll minimal diff output), else write files.
2. The edit itself goes through the semantic model (the `use base`
   statement's span from the FA/refs — NOT a regex), preserving
   everything else byte-for-byte.
3. **Acceptance:** golden diff test; idempotence (re-run produces no
   edits); `--dry-run` writes nothing (assert file mtimes/content).

### Phase C — Moose → Moo, Moo → Moose

Mostly removals / small additions per the owner table. Every
construct the writer cannot translate emits
`# TODO: manual migration needed — <reason>` at the site (the owner
doc's rule) rather than guessing. **Acceptance:** golden diffs both
directions on a fixture exercising `has` flavors, `extends`, `with`,
method modifiers; untranslatable constructs produce TODOs not edits.

### Phase D — Moo/Moose → core `class` (the headline)

The owner doc's table row-by-row: `class`/`:isa`/`:does`,
`has` → `field` with `:param :reader (:writer)`, defaults, lazy via
`ADJUST`, `sub`→`method` with invocant dropped. Untranslatable list
(BUILD/BUILDARGS→ADJUST note, DEMOLISH, complex isa constraints,
coercions, triggers, delegation) → TODO comments. Span-based, never
whole-file regeneration; comments and non-framework code
byte-identical. **Acceptance:** golden diffs on a fixture per table
row; a `perl -c` syntax check of migrated output in the test (system
perl ≥ 5.38 has `class` behind `use experimental` — emit the
`use v5.38; use experimental 'class';` preamble and assert compile).

### Phase E — bless → Moo (heuristic; LAST, gated)

Only attempt when the FA's evidence is strong: a conventional
constructor blessing a hash literal + accessor-shaped subs. Anything
below full confidence → TODO comment, no edit. If the heuristic's
false-edit rate on the substrate is nonzero, ship it behind
`--allow-heuristic` or drop the phase; record the measurement.

## Non-goals

- Diagnostic codes/config/SARIF (Epic 8 owns those).
- Editing files the semantic model didn't fully parse (any ERROR node
  in a target file → skip file with a warning; never edit around
  broken syntax).

## Verification gate

cargo test + gold untouched (these are additive CLI surfaces); for
migrate phases, the golden-diff suite IS the gate plus `perl -c`
compile checks of outputs. Run `--migrate --dry-run` for each target
over the gold substrate and attach the summary (files matched, edits,
TODO counts) to the PR — the substrate is the honesty check that the
writers do not touch what they should not.

## Sizing

Phase A is a string of small wins (good for ramping a new
implementer). B small; C medium; D large; E small-but-risky. Separate
PRs per phase.
