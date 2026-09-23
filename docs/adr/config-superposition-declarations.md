# ADR: Config superposition over declarations

Two C/C++ misparse tiers where a conditional-compilation region corrupts
downstream analysis: **Case A**, a directive inside a declaration
(json.hpp:21396, `#if` in ctor-initializer position) whose local misparse
carries unbounded blast radius — class attribution never re-anchors, so
~4400 lines (~80% of `basic_json`) lose membership. **Case B**, config-twin
regions (perl5 op.c) where a struct-field access resolves at an
unconditional site but is dark inside `#ifdef PERL_DEBUG_READONLY_OPS`.

## Decision

**Case A** is fixed by two layers: the declaration-position directive
repair (`strip_declaration_position_directives`, the isolated ctor-`#if`)
plus the attribution-layer re-anchor
(`SkeletonAnalysis::reanchor_truncated_containers`, gated by
`LangPack::brace_scoped_members`, run post-`remap_spans`): it brace-matches
the ORIGINAL source (balanced — the macro-expansion transform is what
unbalances braces) to recover each container's true extent, then
re-attributes members to the innermost container that textually encloses
them (upgrade-only; a `::`-qualifier attribution and a macro-defined-
namespace scope are the two guarded cases). `basic_json` member
attribution: 92 → 763 (both amalgamated and split forms); nested
`json_value`/`data`/`patch_operations` members preserved. This bounds the
blast radius for any future misparse that truncates a container, not just
this construct.

**Case B** is fixed by narrowing the macro-expansion exclusion query
(`EXCLUDE_QUERY` in `build/cpp_reparse/defs.rs`), not by a config-variant
model. See Mechanism below. `EXCLUDE_QUERY_WIDE` (whole-region exclusion,
the pre-narrowing shape) is kept as an opportunistic fallback: when the
narrowed query raises parse damage on a file — a huge macro-heavy source
like perl.h/op.c — that file re-excludes its region bodies and keeps the
prior fast expansion instead of paying the salvage cliff for the widened
scope.

**Config-superposition variant tags — first-class variant declarations,
one symbol with multiple arm-tagged def-sites, arm-fold typing through the
existing reducers — are deferred**, scoped only to genuinely superposed
DECLARATIONS (a field/def whose shape differs per config, an `#else`-twin
function with a different body) where the payoff is labeled multi-arm
navigation and arm-fold typing on true twins. They are not needed for
Case B (below) and do not fix Case A's blast radius (a parse corruption,
not a typing disagreement). See `docs/PARKED.md`.

## Mechanism — Case B

Case B is not a config-superposition problem. It is the macro-expansion
exclusion over-reaching: the exclusion query originally captured the WHOLE
`preproc_ifdef` / `preproc_if` node — body included, not just the
directive/condition line — so every macro use between `#ifdef` and
`#endif` was skipped by the global expansion walk. perl5's `pTHX_`
context-param macro therefore stayed a literal token inside any
`#ifdef`-wrapped function; tree-sitter-cpp parsed `pTHX_ OP *o` as a
parameter typed `pTHX_`, the receiver `o` mistyped as `pTHX_`, and
`o->op_slabbed` went dark. The struct field, its def, and the parse were
all fine — the one broken link was the receiver's type.

The exclusion is condition-blind: `#ifdef`/`#ifndef`/`#if defined(...)`
are all `preproc_ifdef`/`preproc_if` nodes and were all whole-node
excluded, so the config-INACTIVE arm and the config-ACTIVE arm went dark
equally — a macro use in the active arm is dark too. A config *picker*
would not have helped (the active config is dark); "config-twin" was a
misnomer, no twin is required. Single-arm `#ifdef` reproduces with no
`#else` at all.

The fix: `EXCLUDE_QUERY` captures only the `condition:`/`name:` field —
the directive tokens — leaving the region body expandable, so a macro use
between `#ifdef` and `#endif` expands normally. The condition/name stays
excluded so a macro name on the directive line (`#ifdef FOO`, `#if
defined(FOO)`) is never rewritten. This clears the op.c
`PERL_DEBUG_READONLY_OPS` darkness tier entirely — a one-query,
model-independent robustness fix with no unbounded blast radius (the
damage was local to one mistyped receiver, so the re-anchor invariant per
se isn't what saves it).

## Rejected

- **Pick-a-config** (clangd-style): against the project's grain —
  inactive-arm code is code developers navigate ("you frequently DO care
  about portability"); library corpora are deliberately all-configs; and
  someone must own config-selection UX. This is the failing status quo
  with ceremony.
- **Flatten both arms into one parse**: produces invalid syntax in exactly
  the hard cases (concatenated ctor-initializer lists, colliding
  `#else`-twin definitions) and pushes contradictory facts onto the same
  witness attachment with no provenance separating them — the
  parallel-truth drift the bag exists to prevent. Named and refused as the
  rule-#10 "smallest diff right now" temptation.

## Variant-tag cost (measured, for reviving the deferred model)

Conditional regions become variant spaces: arms parse separately (bounded
local reparse, existing splice machinery); every fact minted inside an arm
carries a variant tag = the interned condition-term stack `cpp_reparse`
already computes. Consumers would fold: navigation unions, labeled (the
macro-def gd rendering); typing folds by agreement through the existing
arm-fold reducers (a config arm is a branch arm whose condition is a
preprocessor expression — the reducer vocabulary does not grow, the walker
emits arm-tagged witnesses from a new arm kind); unified symbol identity
across arms (one symbol, multiple tagged def-sites — forced by rename
correctness, since a rename must edit every arm atomically, including
configs the user isn't looking at); nobody evaluates conditions, ranking
may peek (platform-obvious signals / non-`#else` weak prior rank first,
all arms always shown — ranks-never-prunes). Statement-level twins are out
of scope; all measured pain is declaration-granularity (fields,
ctor-initializers, defs).

Measured on perl.h / op.h / op.c (`gold-corpus/cpp-fixture/cfgtwin/cfgtwin.c`
isolates pTHX_-vs-plain × #ifdef-vs-#if/#else × struct-field-in-#ifdef;
distinct-stack counting mirrors `guard_trail` with header-guard
suppression; depth is guard-suppressed nesting):

| file  | regions | arms | max depth | distinct stacks | cond lines | sym / ref / witness | bincode | zstd blob |
|-------|--------:|-----:|----------:|----------------:|-----------:|--------------------:|--------:|----------:|
| perl.h| 747 | 1059 | 5 | 946 | 58.5% | 2336 / 4580 / 10581 | 2.12 MB | 286 KB |
| op.h  |  29 |   42 | 2 | 23 | 17.5% | 424 / 483 / 698 | 204 KB | 34 KB |
| op.c  |  78 |  106 | 2 | 36 | 3.6% | 1794 / 22128 / 29023 | 5.5 MB | 807 KB |

The tagging cost is linear, not a cross-product: perl.h's interning table
is 946 entries — ≈1.27× the region count, ≈0.89× the arm count — bounded
by document structure, not by facts×regions (which would be
17497×747 ≈ 13M). Max depth 5 admits a theoretical 2^5 combos per nest,
but realized distinct stacks stay ≈ arm count. The variant tag is one
small integer (a u16 index suffices — 946 ≪ 65536) per fact; tagging is
O(facts), the table is O(distinct stacks).

Cache delta on perl.h (17497 facts), per-fact tag as `Option<u16>`: +1
byte (discriminant) on every fact, +2 more on the ~58% inside a
conditional → ≈38 KB added to the 2.12 MB uncompressed bincode (+1.8%;
worst case all-tagged u32 = +87 KB / +4.1%). The interning table itself is
a few KB uncompressed. Tag ids are small and highly repetitive within a
region, so zstd crushes them: the compressed blob delta is well under 1%
of the 286 KB (single-digit KB). Reviving variant tags requires an
`EXTRACT_VERSION` bump regardless of the delta's size.
