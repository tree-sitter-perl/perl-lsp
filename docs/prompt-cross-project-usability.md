# Cross-project usability findings (de-overfit sweep)

> Six usability explorations — crm (in-house Mojolicious+Clove+DBIC) plus four
> cloned public projects spanning frameworks we don't use in-house: **Dancer2**
> (Moo + keyword DSL), **Catalyst-Runtime** (Moose MVC), **Moose** itself (385
> modules, pure Moose), and **metacpan-web** (a real Catalyst+Moose app). The
> question: did our crm-driven tuning overfit? Answer: **no** — the core
> generalizes; the gaps are completeness/symmetry, all framework-agnostic.

## What generalizes (no plugin, confirmed on Moose/Dancer/Catalyst)
Framework detection (Moo/Moose), `has` accessor synthesis, **string-form `isa`
typing** (`'Str'`, bare class names, parametric `'ArrayRef[X]'`), array-form
`has [qw/.../]`, `$self` typing, cross-file goto-def through `extends` AND
`with` (roles), hover with `from <role>` provenance + POD, outline, and
workspace-symbol at scale (930 files ~3s). The Moo→Moose jump is real: nothing
here was crm- or Mojo-specific.

## CORE gaps, ranked (framework-agnostic; fixing each lifts crm too)

### 1. Reverse cross-file `references` for methods — confirmed by ALL FOUR projects
goto-def resolves a call→def forward (incl. through inheritance/roles), but
`references` on a base/role/plain method returns 0 cross-file callers — only
same-file. The reverse index doesn't fan a method def out to call sites whose
invocant resolves to the defining class via the inheritance/role graph. A
**rename-corruption hazard** (rename touches def + same-file, leaves cross-file
callers dangling). Concrete: Moose `add_attribute` 0/284 refs; Dancer
`execute_hook` 0/dozens; metacpan `request` 0/11 (subclass); Catalyst
`Component::COMPONENT` 0. *In flight (`afa7dc2d`), scope confirmed to include
the inheritance/role reverse direction.*

### 2. Non-default `has` options not synthesized — the one Moo-vs-Moose hole
`predicate`/`writer`/`clearer`/`reader` (Dancer, Catalyst) and **`handles => {...}`
delegation** (Moose) emit no method symbols — only the default `is=>ro/rw`
accessor name. So `$self->has_x`/`$self->clear_x`/delegated methods →
unresolved-method + no goto-def. Clean, contained core fix in the `has`-synthesis
path; high frequency in Moose/Catalyst and used in crm too.

### 3. Dynamic export tracing (`Sub::Exporter` / `Moose::Exporter` / `Exporter::Tiny -base`)
The biggest *felt* pain on Moose: every internal util is exported via
`Sub::Exporter::setup_exporter` / `Moose::Exporter->setup_import_methods`, and
`Type::Library -base` registers type constants at runtime. The LSP sees the
`use X 'name'` import (goto-def lands on the use line) but never resolves it to
the exporting sub → cross-file refs/goto-def for exported functions break, and
the type/DSL keywords show as unresolved-function. Same root as the crm
`Clove::Types` residual (the `Str`/`Int`/`Maybe`/`InstanceOf` constants). Static
analysis can't run the exporter; needs modeling the common exporter shapes (or,
for Types::Standard, teaching the type-tiny plugin the standard constant vocab).

### 4. Smaller core items
- `around`/`before`/`after sub {...}` body: `$self` not typed (Catalyst).
- `\&subname` code-ref form: not a resolvable goto-def/references token (metacpan; pervasive in Promise/Future code).
- `not` named-unary operator misclassified as `unresolved-function` (metacpan) — parser/builder bug.
- `isa => 'Any'`/`'Item'`/`Maybe[X]` string types unmapped (Moose). *`Maybe[X]` in flight (`a51662bc`).*
- `__PACKAGE__->meta` returns an untyped MOP object → MOP-style method refs miss (Moose).
- `SUPER::method` flagged unresolved (metacpan, hint-level).

## PLUGIN opportunities (framework-specific, each kills large noise)
- **`dancer.rhai`** — `use Dancer2` exports ~90 DSL keywords (machine-readable in
  `Dancer2::Core::DSL.pm`'s `dsl_keywords`, with `is_global`/`prototype` metadata)
  + `to_app`/`dance`; `Dancer2::Plugin` re-exports Moo `has` + `plugin_keywords`.
  Kills ~1,128/1,723 (65%) of Dancer diagnostics. Mirrors `mojo-lite.rhai`.
- **`catalyst.rhai`** — type `$c` (the action's 2nd param) + `$c->model('X')→X`,
  `$c->req`/`stash`/`forward`/`res`; optionally `mk_classdata`/`mk_accessors` and
  action-attribute semantics. The #1 daily Catalyst idiom, dead without it.

## Takeaway for sequencing
The headline next-sprint theme is framework-agnostic: **#1 reverse references**
(in flight) + **#2 has-option/handles synthesis** + **#3 exporter tracing**.
These three lift every Moo/Moose/Mojo/Dancer/Catalyst codebase at once, crm
included. The Dancer/Catalyst plugins are high-ROI but framework-scoped; do them
after the core completeness items so the plugins inherit a stronger core.
