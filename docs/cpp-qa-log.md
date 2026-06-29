# C++ QA log — real-codebase sweeps, gaps found, fixes

A running record of QA-ing the C++ support against real projects, the gaps
found, and their resolution. Loop: sweep `--outline` (and goto/completion)
across a real codebase → bucket files (OK / EMPTY / STRUCTURE-CORRUPT /
CRASH) → minimal-repro each interesting case → fix or codify as gold
(xfail for deferred). Gold fixtures live under `gold-corpus/fixtures/cpp-*`.

## Done

| gap | found in | status | fixture |
|---|---|---|---|
| cross-file namespace macro (`SPDLOG_NAMESPACE_BEGIN` in another header) | spdlog | **FIXED** — cross-file macro resolution (gather #defines from #included headers) | cpp-cross-file-namespace-macro-resolved (gold), ns_macro (xfail: unincluded) |
| self-referential macro OOM (`#define M x // M M`) | Dear ImGui | **FIXED** — blue-paint guard + comment strip + size cap | cpp-selfref-macro-no-oom (gold) |
| out-of-line `Class::method` loses qualifier (attributed to namespace) | leveldb, fmt | **FIXED** — `@qualifier` capture → package | cpp-out-of-line-method-qualifier (gold) |
| chained METHOD calls (`box.getX().`, incl. inherited) | (design) | **FIXED** — method-return writeback-lite through MethodOnClass | cpp-chained-method-call, cpp-inherited-method-return-chain (gold) |
| template member fn classified as `Sub` | json, fmt, range-v3 | **FIXED** — a sub owned by a class is a method (into_file_analysis) | cpp-template-member-is-method (gold) |
| macro-recovered spans in original coords | (design) | FIXED earlier | — |

## Open (scout-found, not yet fixed)

| gap | found in | freq | plan |
|---|---|---|---|
| `concept X = ...` emits no symbol | range-v3 (raw C++20) | low | new symbol kind — deeper |

## Robustness
spdlog 107 headers: 0 crashes; after cross-file macros, structure-corrupt
4→1, empty 14→12. Self-ref OOM was the only crash across ~360 files (6
projects) — now guarded.

## Re-sweep validation (after cross-file macros + template fix)

- **nlohmann/json** (47 headers): 40 ok, 6 empty (all macro-only/forward-decl
  — legit), **0 real structure-corrupt (was 4), 0 crash**. The cross-file
  namespace-macro fix fully resolved json's corruption.

## Deferred (xfail — need a model change, tracked → XPASS when fixed)

- **`concept X = ...` emits no symbol** (cpp-xfail-concept-symbol). Needs a
  `Concept` SymKind (or a least-wrong mapping) — a model ripple. Low
  frequency (libs hide concepts behind their own macros).
- **`auto`/deduced return types** (cpp-xfail-auto-deduced-return). The
  return depends on the body, which needs the iterative fold the cpp pack
  doesn't run. Declared returns work; `auto` is the tail.

## Note: cpp cross-file resolution

Method-return resolution flows through MethodOnClass, which is cross-file
CAPABLE (the reducer recurses into a cached module's bag). But C++ has no
workspace/module index yet — only the queried file + its #included macros
are analyzed. So cross-file method returns + goto-def into another header
aren't active yet; that's the next cross-file piece (a cpp workspace
indexer, mirroring the Perl module_resolver). Single-file + inheritance
within the file work today.

## Sweep: abseil + the macro-soup tail

abseil sample (88 headers): 79 ok, 6 empty, 3 structure-corrupt, 0 crash.
The 3 corrupt (blocking_counter.h, …) are abseil's **conditional,
multi-definition, version-mangled macros** (`ABSL_NAMESPACE_BEGIN` →
`namespace absl { inline namespace <version> {`, `ABSL_GUARDED_BY`
`#if`-guarded) gathered across a huge transitive include tree. Every
*self-contained* repro works — nested inline-namespace macros, GUARDED_BY,
the exact private-member combo. The corruption only appears with abseil's
real config.h soup, where which `#define` wins is order/cap-sensitive. A
deep library-specific tail, NOT a general gap (spdlog/json/re2 clean).

Fix landed regardless: the transitive macro gather is now **breadth-first**
(closest includes win the budget) instead of depth-first, where a deep
early include could starve a direct sibling. Correctness improvement;
spdlog/json/cross-file all still green.

## Follow-ups from live perl5 use (op.c)
- **goto-def on local vars** — C local variable references don't resolve to
  their declaration: the pack emits `@expr.read.var` only as a *type*
  witness and `@flow.target` only for *type tracking* — neither a Symbol nor
  a resolvable Ref. Implementation plan (a real feature, ~the size of the
  type-witness join):
  1. Distinguish local/param `@flow.target` (from `declaration` /
     `parameter_declaration`) from FIELD `@flow.target` (from
     `field_declaration` — already a `@def.var` Symbol). They share the
     capture today; add a distinct capture (`@def.local`) or tag by source
     pattern so we don't double-emit fields.
  2. Emit local/param decls as Variable Symbols (scoped to the body).
  3. Emit `@expr.read.var` reads ALSO as Refs; resolve each to the nearest
     decl by walking `Scope.parent` (name match, declared-before by point).
     Only emit the Ref when a local decl matches — else leave it for the
     existing function-by-name goto-def.
  4. Outline: `outline_children_of` recurses into Sub/Method bodies, so
     local Variables would clutter. Skip Variables whose enclosing scope is
     a Sub/Block (not a Class) — a general fix (Perl locals shouldn't be in
     the outline either; verify no Perl regression).
- **function defs hidden behind signature macros** — perl5 functions are
  `OP * Perl_newOP(pTHX_ I32 type, ...)`; if `pTHX_`/`pTHX` (thread-context
  macros from perl.h) aren't expanded, the function_declarator corrupts and
  the function symbol vanishes. THE dominant perl5 idiom — investigate.
- **goto labels** — C `label:` / `goto label;` are real navigation targets
  (label defs + goto refs). Add nav entries (def + goto-def from the
  `goto`). Not yet handled.
- **perf**: cold macro gather at completion on perl5 ~6s (CLI cold). The LSP
  warms the header cache at analyze, but the per-completion re-gather over
  perl.h's closure is heavy — needs a cached macro table (compute once on
  open/change, not per keystroke).
