# ADR: Transitive export surface — re-export edges, walked at query time

A module's export surface can be built *from other modules' surfaces*.
Test::Most re-exports Test::More's `ok`/`is`/`isa_ok`; Moose/Exporter::Tiny
re-export via a declarative `also`. Without modelling this, every re-exported
name flags as unresolved-function and never navigates.

The shape mirrors inheritance exactly: a module carries **edges** to the
modules whose surface it folds in, and the closure is computed at **query
time** through `module_index` — never baked. This is the same OPEN-only /
enrichment asymmetry the inheritance walk avoids: depth must stay a query-time
edge property so workspace-index and dependency files (built without
enrichment) resolve identically to open files.

## Build time: mint edges (three statically-recognized forms)

`FileAnalysis.reexport_modules: Vec<String>` — the re-export edges, serde-cached
(bump `EXTRACT_VERSION`). All minting is in `builder.rs`, recognized by CST
shape (rule #10 — never a module allowlist):

1. **Static splice** — `our @EXPORT = ('own', @Test::More::EXPORT)`. Any `array`
   deref element whose varname is `Pkg::EXPORT` / `Pkg::EXPORT_OK` in an
   `@EXPORT`/`@EXPORT_OK` RHS → edge to `Pkg`. `record_static_splice_reexports`
   recurses the RHS, so the deref is caught even inside a ternary branch (as in
   real Test::Most line 41-44).

2. **Loop-push** — `for my $m (qw(A B)) { push @EXPORT, @{"${m}::EXPORT"} }`.
   Reuses the loop-list constant fold: the list resolves statically (literal
   `qw`/list, or a same-file `my @mods = (...)` chased via `constant_strings`)
   → edge per module. `body_has_symbolic_export_push` confirms the
   `push @EXPORT, @{"${var}::EXPORT"}` pattern by shape. Dynamic/unresolvable
   list → no edge (honest; real Test::Most line 166 uses `keys %hash` and is
   deliberately skipped — forms 1 already cover the headline case).

3. **Declarative `also`** — `Moose::Exporter->setup_import_methods(also => [...])`
   and the Exporter::Tiny equivalent. The literal module-name list under the
   `also` key → edges. In `detect_exporter_setup_call`, recognized by the key,
   not the invocant class.

**Deferred (form 4):** runtime `Mod->import` / `import_to_level` / `import::into`
delegation with no `@EXPORT` manipulation. It's control-flow-dependent and we
don't model conditionals; Test::Most stays covered via forms 1+2 so this costs
nothing for the headline case. Regex/negation export selectors also deferred.

## Query time: walk transitively, bounded

`FileAnalysis::export_surface_with_index(module_index)` materializes the
producer's default / optional / tag sets to include every re-exported module's
surface, BFS over `reexport_modules` cross-file. Bounded exactly like the
ancestry walk: a **seen-set** absorbs cycles (A re-exports B re-exports A) and a
**fan-out budget** (`MAX_REEXPORT_MODULES = 256`) caps pathological breadth.
When a module has no edges the result is identical to `export_surface`
(own-only, zero extra storage).

`ModuleIndex::defining_module_cached(entry, name)` walks the same edges to find
the module that actually *defines* a re-exported sub, so goto-def/hover land on
the real def (Test::Most's `ok` → Test::More.pm), not the re-exporter.

## The consumer evaluator is untouched

`imported_names(import, &surface)` is **unchanged**. It binds whatever the
surface reports; transitivity lives entirely in how the surface is built. The
`reexport_imported_names_evaluator_unchanged` test pins this: the same evaluator
over an own-only surface omits the re-exported name and over an index-walked
surface includes it.

## Resolution must follow the edges

Batch `--check` (`cli_full_startup`) and the live resolver thread both resolve
modules transitively: a re-exporting module enqueues its `reexport_modules` so
the producers (Test::More) get resolved even though no file `use`s them
directly. Without this the edge dangles and the surface walk finds nothing
cached.
