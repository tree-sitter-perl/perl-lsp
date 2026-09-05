# Open problems

Genuinely-unsolved problems, stated abstractly — the underlying gap,
not a feature request. Each is deliberately out of scope until a
motivating case makes the fix's shape clear. Forward *design* corpus
lives in the `prompt-*.md` docs and `ROADMAP.md`; this file is for the
hard boundaries those designs run into.

## Untyped param / hash-element boundaries break value-flow chains

A value that arrives as a **sub parameter** or a **hash element with no
inferable type** is a dead end for any analysis that flows along the
value. The chain can't start, so everything downstream of the boundary
stays dark — even when the rest of the chain is fully modeled.

The motivating case is the Alerts partial-route boundary: crm's route
plugins root their route chain from `my $r = $conf->{root}`. `$conf` is
an untyped sub param and `root` is a hash element of it, so the
`BrandedRoute` chain (`docs/adr/route-branding.md`) never gets its
starting brand, and partial `->to('#action')` calls hanging off that
root never resolve their inherited controller. This is *not*
route-specific — the same gap swallows any chain whose origin is an
untyped param or hash element (a DBIC resultset handed in as an arg, a
helper-returned object passed through `%opts`, etc.).

The fix is not local to any one feature. It needs either declared param
types at the boundary (the `param_types()` manifest in
`docs/adr/plugin-system.md` is the landed first cut, but it types by
role/callback contract, not arbitrary hashref params) or cross-procedure
value-flow that propagates a type *into* a param from its call sites. The route doc enumerates the option
space and explicitly chose to leave boundary #4 (param/hashref) out;
see `adr/route-branding.md` (the unbranded-root boundary). Deferred until
a value-flow story exists; the in-`register` local case is the dominant
idiom and resolves without it.

## Qualified-name resolution suppression is coarse

`SUPER::method` and other `::`-qualified call forms are unconditionally
skipped by the unresolved-method diagnostic, before any resolution is
attempted. The skip keys on syntactic shape (any non-bare `MethodToken`)
rather than asking "does this qualified name resolve through the MRO /
the named package?" — so a genuinely broken `SUPER::` or `Class::method`
dispatch goes undiagnosed right alongside the legitimate ones. The
honest fix resolves the qualified target against the same ancestor /
package graph method dispatch already walks, rather than special-casing
the `SUPER::`/`::` token shape (a rule-#10 smell). Hint-level today, so
low blast radius; revisit alongside the Openness diagnostic rework
(`docs/prompt-graph-walking.md`, Openness), which owns the
"when is an unresolved call real?" question.

## Static analysis can't run runtime export generators

`Sub::Exporter::setup_exporter`, `Moose::Exporter->setup_import_methods`,
and `Exporter::Tiny -base` install exported subs (and type-library
constants) at *runtime*, by executing the exporter. The analyzer sees
the `use X 'name'` import line but can't follow it to the exporting sub,
so cross-file references / goto-def for dynamically-exported names break
and the names show as unresolved-function. This is the same root as the
crm `Clove::Types` constant residual (`Str` / `Int` / `Maybe` /
`InstanceOf`).

Static analysis fundamentally can't evaluate the generator. The
tractable path is modeling the *common exporter shapes* declaratively (a
plugin that knows "a `Type::Library` exports its registered type
constants", "a `Sub::Exporter` config's `-as` renames map to these
subs") rather than executing anything — the same "plugin owns the
vocabulary, core owns the mechanism" split the framework plugins use.
Out of scope until the exporter-shape catalog is worth building; the
explicit-`qw` import path is fully handled in the meantime.

**Rule-#10 debt (recorded, not yet paid):** the *which module/verb is an
export declaration* decision is still a module-name allowlist in core —
`package_uses_exporter_declare_family()` (Exporter::Extensible /
Exporter::Declare), `package_uses_moose_exporter_or_type_library()`
(Moose::Exporter / Type::Library / Exporter::Tiny), and the verb dispatch
in `detect_exporter_setup_call` / the `export` / `exports` /
`default_export` / `setup_exporter` arms. Both call families are now
*gated* on the enclosing package having `use`d the matching exporter
(so an unrelated `$x->add_type(...)` no longer pollutes `export_ok`), but
the gate's vocabulary lives in core, not a plugin manifest. The
principled shape, analogous to `param_types()` / `dispatch_verbs()` /
`type_constraint_names()`: a plugin manifest declaring `(exporter_module,
setup_verb, extraction_shape)` triples; core keeps the CST walk (rule #1)
and dispatches on the manifest. Deferred over the A1 gate because each
verb's name-extraction shape (`with_meta`/`as_is` vs `name`/positional vs
`exports` arrayref/generator-hashref) is genuinely per-verb CST code that
stays in core regardless — manifest-izing only the recognition gate, not
the extraction, so the win is modest until the exporter-shape catalog
above lands and the two can be designed together.

## Residual single-store cleanups (low-leverage, no motivating bug)

A few small parallel-store / dead-state items surfaced alongside the
bag-canonical refactor and never earned a landing because nothing
depends on them yet:

- **`SubInfo` arity accessors** (`param_counts`, `return_type_for_arity`,
  `primary_id`, `id_for_arity`) are `#[allow(dead_code)]` — a public API
  surface with no current caller. Delete when a cross-file caller wants
  them (route through `symbol_return_type_via_bag`) or when a dead-code
  sweep reaches them.
- **`fold_state_snapshot`'s hand-coded convergence tuple** could become
  a single monotonic `bag_generation` counter bumped on every bag
  `push` / `remove_by_source_tag`, so any future re-emittable pass
  participates in fixed-point detection automatically instead of needing
  a manual tuple entry. Cosmetic until a new re-emittable pass forgets
  the entry and the worklist exits early.
- **Forward-call resolution duplicates its lookup rule** across
  walk-time (`find_callee_symbol`) and a post-walk retry. The principled
  shape pushes a name-keyed witness (`CalleeByName`) at walk time and
  resolves it in one post-walk pass against the final symbol table.
  Worth it only when the lookup rule has to grow (package-aware lookup,
  plugin-synthesized callees).

These are recorded so the next refactor in the area picks them up; none
justifies a standalone PR.

## `main::` aggregation across `require` of package-less scripts

Legacy CGI (AWStats) `require`s package-less `.pm`/plugin files into the
running script; with no `package` statement every sub lands in `main::`. Host
and plugins call each other's subs — all `main` at runtime — but each file is
analyzed in isolation, so cross-file `main::` symbols never unify (~270 FPs
both directions in `awstats.pl` and its `require "$pluginpath"` plugins).

Cross-file resolution keys on a *named* package (`package_parents`, the
module→file map, the reverse index). `main` is the implicit, unnamed package,
and many unrelated scripts each define their own `main::` subs, so naïvely
unifying all `main` symbols workspace-wide cross-links unrelated files (every
`t/*.t` has its own `main`). The real edge is `require`-induced — file A
`require`s file B, so B's `main` subs are visible in A: a file-level dependency
edge the engine doesn't model, distinct from `@ISA` (not inheritance) and
`use`-import (no export list; everything in `main` is just visible). Modeling
it wrong (union all `main`) is worse than the FP.

The principled fix models the `require`-dependency edge: on a static `require`
(literal path, or a `$var` tracing to a constant path) add a directed edge
A→B and resolve unqualified calls in A against B's `main::` subs along it
(bounded, seen-set) — only require-reachable files unify. The dynamic
`require "$pluginpath"` (path from config) degrades silently. Gated on
legacy-CGI support being in scope; modern code uses packages, and the
dynamic-path require defeats static analysis anyway.

## Duplicate-package resolution — two files `package Foo;`, which wins?

Two files declare `package Bugzilla;` (`contrib/Bugzilla.pm` shadows the root
`Bugzilla.pm`); picking the wrong one breaks the singleton's type inference and
exports. "Which file owns `package Foo`?" has no static ground truth — at
runtime `@INC` order decides, and the LSP has no single `@INC`. A heuristic is
unavoidable but must be principled and stable (rule #10: not "is this path
`contrib/`"). It interacts with the shadowed-`@EXPORT` bug and with the
documents → workspace_index → module_index tier priority (duplicates *within* a
tier are the gap).

The principled shape ranks by a typed `FileRole` computed once at index time
and carried on the entry: prefer the file whose path best matches the package
name (`Bugzilla::Foo` → `lib/Bugzilla/Foo.pm`), then `lib/` over
`t/`/`contrib/`/`xt/`/`examples/` — so the resolver asks the entry "are you
canonical?" and the entry answers, no path-string branch at the resolution
site. A real `@INC` / `.perl-lsp` config order overrides when present.
