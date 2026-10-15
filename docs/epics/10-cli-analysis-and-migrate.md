# Epic 10 — CLI analysis subcommands + `--migrate`

> **Status:** scheduled (10th).
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
2. `src/lsp/cli/mod.rs` (dispatch), `positions.rs` (the coordinate
   contract), `query.rs` (the verb bodies), `heatmap.rs` (the best
   template for a full report verb).
3. `CLAUDE.md` §"A verb indexes only the language families its answer
   can consult" and §"Startup is not one thing" — both are directly
   about writing a new CLI verb, and both are easy to get quietly wrong.
4. For `--migrate`: how `--rename` builds span-based `WorkspaceEdit`s
   (`grep -rn 'fn cli_rename' src/lsp/cli/`) — migrate reuses that
   edit-application shape.

## The two contracts every new verb must honor

**1. Coordinates.** 0-based input, 1-based output, per `--help`. Copy an
existing position-taking subcommand EXACTLY; `positions.rs` owns the
conversion and has its own tests.

**2. One language per process.** `cli_full_startup` indexes the served
language only. A verb with a target file calls `lock_for_file` before
startup (its file picks the language unless `--lang` did); a sweep verb
serves the `--lang` language, Perl without one.

**3. Streams.** Chatter to stderr, data to stdout — the heatmap's
pattern.

## Phase breakdown

### Phase A — thin analysis subcommands

Each a small `cli_*` fn + a `--help` entry + tests; separate commits, in
this order:

1. `--completions <root> <file> <line> <col>` — the completion engine at
   a position. Mirror `--signature-help`'s plumbing (`lock_for_file`).
2. `--dependency-graph <root> [--format dot|json|list]` — module import
   edges from each FA's `imports`, plus parent edges as a distinct edge
   kind; cycles via DFS, reported in all formats. Sweeps the served
   language.
3. `--export-api <root> <module> [--format json|markdown]` — exports,
   params, return types, parents, framework, from the cached FA.
   `--dump-package` is the debugging sibling; this is the user-facing
   one — share extraction where trivial.
4. `--impact <root> <file-or-module>` — reverse deps: who imports /
   inherits from this. The reverse index and `children_index` already
   answer both; this is formatting.
5. `--framework-report <root>` — classes by framework, counts + list.
6. `--unused-exports` / `--dead-code`: implement as **ALIASES** over the
   Epic 7/8 machinery if those landed (PL005/PL006 + the heatmap
   guards). If this epic runs first, SKIP them and leave the pointer —
   **do not build a third dead-code path.**
7. `--repl-complete` — stdin-accumulated source, complete at EOF.
   Lowest priority; drop if time-boxed out.

**Acceptance per subcommand:** a golden-output test on a fixture
workspace; `--help` updated; stream discipline preserved; a file-target
verb locks to its file's language (`lock_for_file`).

### Phase B — `--migrate` step 1: `use base` → `use parent`

The deliberately-trivial first target, to build the harness.

1. `cli_migrate(root, target, from, to, dry_run)` — index, select files
   by `--from` detection (the FA knows its frameworks), produce span
   edits; `--dry-run` prints a unified diff, else write files.
2. **The edit goes through the semantic model** — the statement's span
   from the FA/refs, NOT a regex — preserving everything else
   byte-for-byte.
3. **Acceptance:** golden diff test; idempotence (re-run produces no
   edits); `--dry-run` writes nothing (assert content and mtimes).

### Phase C — Moose → Moo, Moo → Moose

Mostly removals and small additions per the owner table. **Every
construct the writer cannot translate emits
`# TODO: manual migration needed — <reason>` at the site** rather than
guessing. **Acceptance:** golden diffs both directions on a fixture
exercising `has` flavors, `extends`, `with`, method modifiers;
untranslatable constructs produce TODOs, not edits.

### Phase D — Moo/Moose → core `class` (the headline)

The owner doc's table row by row: `class` / `:isa` / `:does`, `has` →
`field` with `:param :reader (:writer)`, defaults, lazy via `ADJUST`,
`sub` → `method` with the invocant dropped. Untranslatable
(BUILD/BUILDARGS, DEMOLISH, complex isa constraints, coercions,
triggers, delegation) → TODO comments. Span-based, never whole-file
regeneration; comments and non-framework code byte-identical.
**Acceptance:** golden diffs per table row, plus a `perl -c` compile
check of migrated output in the test (emit the
`use v5.38; use experimental 'class';` preamble and assert it compiles).

### Phase E — bless → Moo (heuristic; LAST, gated)

Only when the FA's evidence is strong: a conventional constructor
blessing a hash literal plus accessor-shaped subs. Anything below full
confidence → TODO comment, no edit. **If the false-edit rate on the
substrate is nonzero, ship it behind `--allow-heuristic` or drop the
phase — and record the measurement either way.**

## Non-goals

- Diagnostic codes / config / SARIF (Epic 7).
- **Editing files the semantic model did not fully parse.** Any ERROR
  node in a target file → skip the file with a warning. Never edit
  around broken syntax.

## Language-pack beat

**Phase A is where a language decision gets made per verb, and Phases
B–E are Perl-only by their nature. Both need saying out loud.**

1. **Every Phase-A verb answers in the served language.** A process
   serves one language, so `--dependency-graph`, `--impact` and
   `--framework-report` sweep that language's files only; a mixed
   workspace runs the verb once per `--lang`. Decide per verb what the
   answer means outside Perl:
   - `--dependency-graph`: a C++ `#include` edge is a dependency edge
     like a Perl `use`; under `--lang cpp` the graph is the include
     graph.
   - `--impact`: the served language's reverse index.
   - `--export-api`: `of_file`-ish — it names one module. But "module"
     is a Perl noun; for a pack language the addressable unit is a
     file/header. Either accept a path for pack languages or scope the
     verb to Perl and say so in `--help`.
   - `--framework-report`: Perl's `package_framework` has no pack
     analogue today. Scope it, and note that Epic 13's framework tier
     is what would widen it.
2. **`--migrate` is Perl-only, permanently, and should say so in
   `--help`.** It translates between Perl object systems. There is no
   general "framework migration" abstraction hiding here and building
   one would be inventing a requirement. The *harness* (index → select →
   span edits → dry-run diff → write) is generic and worth keeping
   clean, but the writers are Perl.
3. The one shared risk: **`--migrate` writes to disk in a workspace that
   may contain pack files.** The file-selection step must never pick up
   a non-Perl file, even one whose extension is ambiguous. Select by the
   analysis's `language` tag, not by extension.

## Scaling beat

**Every verb here calls `cli_full_startup`, and that is O(corpus) in
time and RAM. This epic multiplies the surface that inherits that
cost.**

The measurements (2026-08-17): the CLI one-shot at 138k files was DNF,
killed at 42:32 / 7.11 GB; PR #125 made it finite — **exit 0 in 350 s**
with a real answer. `prompt-scale-validation-hitlist.md`'s verdict is
that the CLI's "act like the LSP just started" semantics bound
`--check` / `--heatmap` / `--workspace-symbol` / batch **as
workspace-scale tools**. Warm LSP ready, by contrast, is scale-free
(1.06 s at 138k).

Obligations:

1. **Position-taking verbs (`--completions`) lock to their file's
   language** and should be *fast* — they are the ones a user might
   script in a loop.
2. **Sweeping verbs inherit the batch-verb caveat, and their `--help`
   should not pretend otherwise.** Match the honest framing already in
   `scaling-limits.md` §5 for `--heatmap`.
4. **`--migrate --dry-run` over the gold substrate is both the honesty
   check and the scaling check.** Run it for each target and attach the
   summary (files matched, edits, TODO counts, wall) to the PR — the
   substrate is where the writers prove they do not touch what they
   should not, and where a pathological cost would show.
5. Report, for each new sweeping verb, its wall and peak RSS on Koha,
   three runs, dated. If a verb cannot finish on Koha, it cannot finish
   on a customer monorepo, and that belongs in `--help` before it
   belongs in a bug report.

## Verification gate

`cargo test` (both feature sets) · gold untouched (these are additive
CLI surfaces) · for migrate phases the golden-diff suite IS the gate,
plus `perl -c` compile checks of outputs · `--migrate --dry-run`
substrate summary in the PR · Koha wall + peak RSS for each sweeping
verb, three runs, dated.

## Sizing

Phase A is a string of small wins (good for ramping a new implementer).
B small; C medium; D large; E small-but-risky. Separate PRs per phase.
