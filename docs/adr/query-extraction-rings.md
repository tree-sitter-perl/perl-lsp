# ADR: Query-driven extraction and the three rings

## Context

perl-lsp exists because tree-sitter-stack-graphs (TSG) failed at exactly
this problem: hosting real language intelligence on tree-sitter queries
alone. TSG pushed both syntactic extraction AND semantic synthesis into
its `.tsg` DSL — a large program in a weak language, with silent-failure
matching and no escape hatch into a real type system. Before committing
to a second (or Nth) language, the question had to be answered directly:
is our engine the piece TSG was missing, or does the same trap wait here?

## Decision: three rings, cleanly separated

Extraction work splits into three rings with sharply different
tractability under a query medium:

- **Ring 1 — syntactic skeleton.** Defs, scopes, namespaces, imports,
  most refs. Declarative tree-sitter queries win here: a **capture
  vocabulary is the language-neutral contract** (`@def.<kind>` /
  `@def.<kind>.name`, `@scope`, `@context.package`, `@ref.<kind>`,
  `@import`, plus later families `@expr.*`, `@flow.*`, `@shape.*`,
  `@cmd.*` as new packs needed them) — the driver knows these names,
  never a node kind. A ~70-line Perl query pack plus a ~200-line
  generic driver (`src/build/query_extract/`) reproduced 96–100% of
  what the hand-written walker extracts for packages, classes, subs,
  and variables (method defs are the ring-3 exception — see below).
- **Ring 2 — pattern-inexpressible syntax.** Small, nasty, individually
  enumerable gaps a tree-sitter pattern cannot express: positional
  pairing (`use constant { A => 1, B => 2 }` — "element 2k is a key" has
  no pattern spelling; the fat-comma trap), codegen loops (`*{$_} = sub`
  over a list minting N symbols from one span), dynamic method names,
  interpolated code, and field-queryability traps (a CST field like
  `variables:` that `child_by_field_name` serves but that same field
  name matches **zero** captures in the query engine on a different
  node kind — silent, not a test failure, and worth 45% of variable
  recall on one probe until measured). This ring is `cst.rs`'s own
  catalog, re-hosted in a medium with a *worse* failure mode: a wrong
  pattern doesn't fail loudly, it silently extracts nothing.
- **Ring 3 — semantic synthesis.** Framework accessors (`has`), DBIC
  relationship methods, plugin-synthesized helpers, requires markers —
  entities with no claimable syntax at all, plus everything downstream
  of extraction: witnesses, chain typing, provenance, cross-file
  enrichment. No query medium reaches this ring; it is computation over
  facts, not matching over trees. On one measured Perl file, 98% of
  Method symbols had no claimable syntax.

**Why this differs from TSG's failure.** This architecture already
separates the rings by construction: ring 3 lives in the engine
(witness bag + reducers, `FileAnalysis` queries, `resolve`/`refs_to`,
`ModuleIndex` — none of which import tree-sitter, enforced by the
layering test) and in plugins keyed on decision-ready shapes. The
walker's irreplaceable job is ring 2 plus ring-3 *emission* (translating
a language's semantics into witnesses/symbols) — a compiler front-end's
job, not an extraction rule. TSG tried to push exactly that job into its
query DSL; no query medium absorbs it.

## Decision: Perl stays walker-built; packs are the multi-language tier

The Perl path stays the hand-written builder — its ring-2/3 fidelity is
far ahead of what a query pack reaches, and porting it to queries would
re-encode ring 2 in a silently-failing medium to replace code that
already works, while ring 3 wouldn't move at all.

For every OTHER language, `src/build/query_extract/` (the generic
driver: sorts capture events, maintains the scope stack and sticky
namespace contexts, assembles `SkelSymbol`/`SkelRef` rows, knows no
target language) plus a per-language `LangPack` (a `.scm` query pack +
host predicates for what patterns can't express — name shaping, shape
classification, import-call recognition) is production: wired into
`LanguageDriver`/`LanguageRegistry`, serving C++/Python/R/CMake behind
opt-in Cargo features. The engine-touch list for adding a language is,
by construction, the number that matters: adding Python, R, and CMake
packs required **zero** edits to `file_analysis.rs`, `witnesses.rs`,
`resolve.rs`, or `module_index.rs`. Everything lived in the driver, a
`.scm` file, and pack predicates — cross-file refs, workspace rename,
and method dispatch through `resolve_method_in_ancestors` all ran on
production paths, untouched, the moment a pack fed real `Scope`/
`Symbol`/`WitnessBag` rows into a real `FileAnalysis`.

## What a pack cannot promise: the operator-orientation ceiling

Perl's inference ceiling is high because its syntax LEAKS types at
usage sites: mono-typed operators (`+` numeric, `.`/`eq` string),
sigils, deref shapes — a variable with an unknowable initializer still
types from `$x + 1` alone, through the production `FrameworkAwareTypeFold`
consuming `TypeObservation` witnesses a pack can emit from operator
patterns. Unannotated Python leaks almost nothing at usage sites (`+` is
polymorphic), so its unannotated ceiling is genuinely lower — not a
pack deficiency, a fact about the language. The witness bag is an
*evidence* framework: each pack harvests whatever its language leaks
(sigils + operators for Perl; annotations + constructors + literals for
Python, where `annot_type` on a declaration carries the load the way
typeshed carries it for pyright) and degrades honestly to navigation
where evidence runs out — no witness, no claim, no lie.

**Consequence for evaluating a new language pack:** ring-1 navigation
(outline, workspace symbols, lexical refs) is free for any language with
a tree-sitter grammar. The typed ceiling above that is proportional to
syntax leakiness × ecosystem annotation density, and estimating it
before writing a pack is a matter of asking those two questions, not
of guessing at engine capability.

## Command-dispatched languages: one pattern per command family

A language whose defs aren't syntax-dispatched (CMake's `set`,
`add_library`, and user functions are all the same `normal_command`
node) states what each command family does with one pattern per family:
the command names are the pattern's own `#match?` (case-insensitive where
the language is), and the effect is the ordinary capture on the argument
at its position (`@def.var.name`, `@ref.call`, `@import.arg`). `@cmd` /
`@cmd.arg` add only what every command has, a call of its name and its
argument count. Any future command-dispatched language (Tcl, shells,
other build DSLs) writes its families the same way, and the driver reads
no command name.

## Consequences

- A new pack language's ceiling is knowable in advance from the rings
  model: full ring 1, an enumerable and language-specific ring-2 gap
  list, and a ring-3 ceiling set by the language's own type-leakiness
  and annotation culture — never a re-litigation of whether packs work
  at all.
- `SymKind::Target` (a target as its own kind rather than riding `Sub`)
  is an open, small ergonomics gap surfaced by CMake's target-rename
  case — noted here as unbuilt, not urgent.
- Deliberately out of this ADR: per-pack implementation details
  (predicate names, `.scm` line counts) belong to the pack source and
  `docs/adr/reparse-stratification.md`/CLAUDE.md's file map, not here.
