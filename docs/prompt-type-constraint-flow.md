# Type constraint flow — the import installs the sub

> Design brief for retiring the constraint-constructor name gate and
> the core `isa`-string table that duplicated the plugin's vocabulary. Owning
> ADR after landing: `adr/type-constraints.md` (its "Retiring the name
> gate" deferral is superseded by this brief). Sibling: `epics/12-type-
> tiny-completeness.md` — Phase A (check-guards) is unbuilt; §6 below
> amends where its baked map lives, nothing else.

## 1. The claim

A Type::Tiny constraint constructor is a **sub**. `use Types::Standard
qw(Str)` installs `Pkg::Str` as a glob alias to a generated sub; `Str` in
value position is a call; `Str[...]` / `InstanceOf['Foo']` is that same
call with one arrayref argument, and the sub parameterizes itself with
the arrayref's contents. Nothing about this is a *name*. The analyzer
carries a name gate only because the generator runs at runtime and leaves
no static `sub Str` for an import binding to reach (`open-problems.md`
§"Static analysis can't run runtime export generators").

The plugin layer exists to repair exactly that invisibility — Moo's `has`
mints accessors, DBIC's `add_columns` mints columns, `monkey_patch` mints
methods at the registration site. For Type::Tiny the registration site
the consumer can see is the `use`. So:

**The `on_use` hook mints the sub the runtime would install, at the site
that installs it. Parameterization is a property asked of the callee, not
of its name.**

Under that model the gate has nothing left to do and is deleted whole —
`type_constraint_names()`, `Builder.type_constraint_names`,
`constraint_name_imported`, and the pre-lookup arm in `expr_payload`'s
call branch. Question 1's answer is yes, entirely; §4 says how the
parameterized constructors follow.

## 2. Ground truth this design stands on

Verified 2026-09-02 against `main` at `fe449fc0`; re-verify anything
load-bearing before implementing.

- **Syntax.** `Maybe[Int]` → `ambiguous_function_call_expression`,
  `arguments: anonymous_array_expression → bareword Int`.
  `InstanceOf['Foo']` → same, element `interpolated_string_literal`.
  `isa => Int` / `my $c = Str` → a bare `bareword` node, no call node.
  `use Types::Standard qw(...)` → `quoted_word_list` with a
  `string_content` child (per-name spans are derivable;
  `extract_qw_word_spans` derives them).
- **`expr_payload`'s call arm emits `Edge(Symbol(sid))` only when
  `find_callee_symbol` finds a LOCAL sub** (`visit_use.rs:1537` — first
  match in declaration order over `self.symbols`, `Sub | Method`,
  qualifier-admitted). A call to an imported name gets **no `Expr`
  witness at all**; its type is reached only by the query-time
  name-keyed path (`query.rs` "Cross-file imports: walk the module_index
  for exporters", `sub_return_type_at_arity`).
- **The builder has no module index.** Every `module_index` mention under
  `src/build/builder/` is a comment. Walk-time typing sees this file's
  symbols and bag, nothing cross-file.
- **`ReducerQuery.args` is `Vec::new()` at every construction site**
  (`fold.rs`, `query.rs`, `pattern_dispatch.rs`, `conclusions.rs`).
  `ReturnExpr::Arg(n)` is declared by the cpp skeleton
  (`query_extract/skeleton.rs:753`) and evaluated by
  `reducers.rs:909`, but no Perl call site threads argument types into a
  query. `Arg(n)` is inert for Perl at query time — it cannot carry
  `Maybe[T]` even in principle, and would not carry `assert_Str($x)`
  either.
- **`ReturnExpr` shapes** (`witnesses/types.rs:295`): `Concrete`,
  `Receiver`, `ReceiverOr`, `Operator(RowOf | ParamOf | InstanceOf{base,
  args})`, `UnionOnArgs`, `Arg(u32)`. Every operand is a *type*. There is
  no shape that reads a literal *value* out of an argument, and
  `InstanceOf['Foo']` needs one: the class name is the string `'Foo'`,
  which types as `String` and is gone.
- **`WitnessPayload::Projected { base, step }`** is single-step, sealed
  (`ProjectionStep::{HashKey, ArrayIndex}`), evaluated in
  `registry.rs:2015` after the base materializes. Minted by Perl-side
  code only (`emit.rs`, `core_types.rs:501`, `queries.rs:843`).
- **`EmitAction::Symbol { return_type }` from `on_use` is a live
  precedent**: `mojo-lite.rhai` mints `app` — hidden, `display:
  Function`, typed `ClassName("Mojolicious")`. `plugin_emit.rs:428`
  applies it as `add_symbol_ns(.., Namespace::Framework{id})` plus a
  `Symbol(sid) → InferredType` witness with `WitnessSource::Plugin`
  (priority 100 — `PluginOverrideReducer` claims it).
- **`use constant` mints hidden local `Sub` symbols** (`visit_use.rs:690`,
  `is_constant: true`). Constants-as-subs is native, not a plugin
  invention.
- **Method completion lists every `Sub | Method` symbol in the class's
  package** (`ancestry.rs:771`). `namespace::clean` / `autoclean` are not
  modeled anywhere in `src/`.
- **`has`'s isa projection is walk-time**: `visit_has_call` and the
  option-tail isa reader beside it (`build/builder/frameworks.rs`) do `emit_expr_witness(n); bag_query_expr_span(..)?.constrained_inner()`
  and bake the result into the accessor's return type.
- **The `isa` vocabulary is the plugin's, once.** Both spellings — the
  bareword constructor and Moose's type STRING — share `base_constant_type`,
  `type_constraint_inner` and one parameter walk (`walk_constraint_params`,
  generic over how a leaf resolves). A string is re-parsed and folded
  through the same path; only leaf resolution differs. Core holds no type
  table.

## 3. The model

### 3.1 The mint

`type-tiny.rhai`'s `on_use` already computes the effective import list
(explicit names verbatim; `-all` / `:all` / `-default` / any `:tag` /
bare `use` → the module's whole vocabulary) and emits one `Import` for
it. It keeps emitting that `Import` — the binding is what diagnostics
suppression, `resolve_imported_function` and Phase 5's goto-def-to-home
read — and **additionally emits one `Symbol` per name**:

```
Symbol {
  name:            <local name>,
  kind:            Sub,
  span / selection_span: ctx.span     // the `use` statement; §7 Phase 6 narrows it
  detail:          Sub { params: [], is_method: false, doc: <one line>, .. },
  display:         Function,
  hide_in_outline: true,               // resolvable, never listed — the `app` / anon-sub precedent
  return_type:     TypeConstraintOf(base_constant_type(name))   // §3.3 for the None case
}
```

One vocabulary, three projections — the export list, the mint, and the
companion typing (`is_X` → `Bool`, `to_X` → the base) come from the same
`types_standard_exports()` / `base_constant_type()` tables. No second
hand-kept list, which is the discipline Epic 12 Phase A already demands.

What this buys, with zero new machinery: `Str` / `Int` / `ArrayRef` in
value position resolve through `find_callee_symbol` to a local symbol and
type through `Edge(Symbol(sid))` like any local sub; hover names the
symbol; bareword completion lists it (with the `Framework{type-tiny}`
bucket); the unresolved-function diagnostic is silent because the call
resolves; a package that never imported `Str` has no such symbol, so the
user's own `sub Str` is found by the very same lookup that finds the mint
elsewhere. **The import is the scoping.** `constraint_name_imported` has
no residue.

Degradation: **none for the primary path.** The vocabulary ships in the
plugin, so consumer minting works whether or not `Types::Standard` is on
`@INC`. Only §7 Phase 5 (home minting, goto-def landing in
`Types/Standard.pm`) depends on the home being indexed.

### 3.2 Parameterization is asked of the callee

`expr_payload`'s call arm becomes:

```
let called = forward_callee_name(node)?;
let sid    = find_callee_symbol(&called)?;            // locals and mints compete by the one rule
if node has an `arguments` child {
    if let Some(inner) = self.plugins.parameterize(owner_of(sid), &symbols[sid].name, params) {
        return InferredType(TypeConstraintOf(inner));
    }
}
Edge(Symbol(sid))
```

where `owner_of(sid)` is `symbols[sid].namespace` and `params` is
`extract_constraint_params(node)` — unchanged, still the only CST walk
(rule #1), still flattening the arrayref and typing nested constructors
through the same `expr_payload` recursion (`Maybe[InstanceOf['Foo']]`
resolves because the inner call resolves to *its* mint first).

`PluginRegistry::parameterize(owner, name, params)` routes to the plugin
whose id is `owner` and calls its `type_constraint_inner(name, params)` —
the fold that exists, unchanged in signature. A `Namespace::Language`
callee has no owner plugin: the call types as `Edge(Symbol(sid))`, which
is exactly Perl's semantics for a user sub that ignores its arrayref
argument. A plugin without a fold declines the same way. **Core never
sees a constructor name.** The name reaches the plugin that minted the
symbol, where owning the vocabulary is the plugin's job (rule #8) — the
same provenance dispatch `Namespace` is documented for ("per-plugin
filtering, rename coordination, diagnostic attribution").

This is the rule-#10 shape the ADR asks for: the callee *value* (a
symbol with an owner) answers "what does applying `[...]` to you mean";
the consumer branches on nothing. It is not the "real vs synthetic"
antipattern — no worker skips a step for synthetic input; the mint runs
the ordinary lookup and the ordinary edge, and gains one *extra*
capability its owner declares.

Cost: one `namespace` field read per local call site before anything
else runs; the fold runs only for calls whose callee has an owner with a
fold. Check `--timings` on the substrate — the slowest-modules tail must
not move.

### 3.3 A constraint whose inner is unknown is still a constraint

The unparameterized generics — `InstanceOf`, `ConsumerOf`, `Maybe`,
`Enum`, `Dict`, `Tuple`, `Object`, `Any`, `Defined`, … — are real Perl
values (`my $t = InstanceOf; $t->of('Foo')`), and every declined fold
(`Enum['a','b']` today) produces one too. `TypeConstraintOf(Box<
InferredType>)` cannot say "a Type::Tiny value whose constrained type I
do not know", so those expressions go **untyped**, losing even the fact
that `$t->check` dispatches to `Type::Tiny`.

**Widen the variant's payload**: `TypeConstraintOf(Option<Box<
InferredType>>)`. Not a new variant — the enum's "never `_ =>`"
invariant is untouched; the 14 match sites (`constrained_inner`,
`surface.rs::despan`, `completion.rs` ×2, the builder wrap) each gain an
`Option` and `constrained_inner()` already returns `Option<&_>`. Bincode
shape changes → `EXTRACT_VERSION` bump (the rhai edit bumps the plugin
fingerprint in the same PR, so caches rebuild regardless). The mint for a
name `base_constant_type` cannot express returns `TypeConstraintOf(None)`;
a declining fold returns `TypeConstraintOf(None)`; the `has` projection
of a `None` inner is an honest untyped accessor exactly as today.

Rejected alternative: keep the payload and leave generics untyped. It
forces §3.2 to dispatch on something other than the value (the return
type would be absent) and keeps `Type::Tiny` dispatch unreachable for
half the vocabulary.

### 3.4 What the model does with each spelling

| Source | Path |
|---|---|
| `isa => Int` (bareword) | `find_callee_symbol` → mint → `Edge(Symbol)` → `TypeConstraintOf(Numeric)`; `has` projects `Numeric`. |
| `InstanceOf['Foo']` | mint owner = type-tiny → `parameterize` → `TypeConstraintOf(ClassName Foo)`. |
| `Maybe[InstanceOf['Foo']]` | inner call types first (recursion in `constraint_param_for`); outer fold lifts → `TypeConstraintOf(Optional<Foo>)`. |
| `ArrayRef[Int]` | fold → base rep `ArrayRef` (sequence-types phase 3 is the waiting caller for the element). |
| `Enum['a','b']`, `Dict[...]` | fold declines → `TypeConstraintOf(None)`; a value, dispatchable, inner unknown. |
| `my $t = InstanceOf['Foo']; $t->name` | `$t` keeps the constraint type; dispatch → `Type::Tiny` when indexed. |
| `sub Str {…}` in a package with no import | no mint in that package → the user's sub, by the one lookup. |
| `isa => 'Str'` (quoted) | re-parsed and folded through the plugin vocabulary; unknown identifiers still name a class under Moose. |

## 4. Why not `ReturnExpr`, stated once

`Maybe[T]` *is* expressible as type-level operators — `Optional(
ConstrainedInner(ElementAt(0, Arg(0))))` — at the price of three new
`ParametricOp` arms in a cross-language enum, plus threading `q.args`
from every Perl call site (§2: nothing threads them), plus lazy
`Sequence` element edges (`Sequence(Vec<InferredType>)` materializes
eagerly, so a cross-file element degrades to `ArrayRef` before any
operator sees it). `InstanceOf['Foo']` is **not** expressible at any
price short of a literal-value type: the operand is the string `'Foo'`,
and `InferredType::String` carries no value.

And it would be the wrong layer. "InstanceOf takes a class-name string;
Maybe lifts to Optional; Dict is a hash shape over its pairs" is
Types::Standard's vocabulary — the plugin's, by rule #8 — not a property
of the receiver-relative return machinery C++ shares. `ReturnExpr` is not
touched by this design. `extract_constraint_params` + the rhai fold stay
where they are; they already are the machinery, and the gate around them
was the only part that was wrong.

## 5. Where the symbols live (question 2)

**Primary: the consumer, at `use` time.** Secondary, additive, later:
the home (§7 Phase 5).

| | Consumer mint (`on_use`) | Home mint (pattern on `add_type` / `declare`) |
|---|---|---|
| Walk-time type in the consumer | yes — the callee is in `self.symbols` | no — cross-file, and the builder has no index |
| Parameterization fold (`Name[...]`) | works (needs the callee at walk time) | cannot run in the consumer |
| Nested params (`Maybe[Int]`) | works | `Int`'s type unavailable at walk time |
| Depends on `@INC` | no | yes — Types::Standard must be indexed |
| House library's OWN types (`declare MinionRef, as …`) | no (kit plugin knows no house names) | yes — the natural owner |
| goto-def on `Str` | the `use` statement (→ the `Str` token, Phase 6) | the `add_type` / `declare` site in the library file |
| Duplication | one hidden symbol per imported name per consumer | one per type per library |
| Coverage of Types::Standard itself | complete (plugin-shipped vocabulary) | partial: the `$meta->$add_core_type` spelling is a dynamic method call on a closure and does not fold |

The consumer mint is the only placement under which the parameterized
constructors type at all, because parameterization is a walk-time fold
over the CST and the callee's type must be in hand when the fold runs.
That settles the primary. The home mint is worth having for two things
the consumer mint cannot do — house-declared types, and goto-def landing
on a real declaration — and it composes with the consumer mint rather
than competing: Perl's runtime state after `use Types::Standard 'Str'` is
*both* a generated sub in `Types::Standard` *and* an alias in the
consumer, and the model mirrors that. The `Import` binding is the edge
between them; `definitions()` on the CandidateSet is where "local alias
∪ home declaration" is minted once for every verb (construction axis,
never a handler — `adr/resolution-candidate-set.md`).

**Duplication, honestly.** `use Types::Standard -all` mints ~152 hidden
symbols (37 base names × 4 companions + 4 standalone) per consumer file;
`-types` / an explicit `qw(Str Int ArrayRef)` mints 3. Explicit lists are
the overwhelming spelling in the wild. Resident copies are
symbol-evicted after persist, so the cost is per-open-file plus blob
size. Measure the substrate's `--timings` tail and per-file heap
(`PERL_LSP_HEAP_JSON`) before and after; if `-all` measures as a problem,
the mitigation is the lazy design in §9(b), not a name list.

**Method-completion noise, honestly.** Every `Sub` in a class's package
is a method candidate (§2), so `$obj->` on a class that imports `Str`
offers `Str` — precisely as `use constant` constants are offered today,
and precisely as Perl behaves without `namespace::clean`. Modeling
`namespace::clean` / `autoclean` (drop imported subs from the method
surface at the `use namespace::clean` line) is the general fix and is
adjacent, not part of this brief.

## 6. Check-guards under this model (question 3)

Yes — `is_X` / `assert_X` / `to_X` are minted symbols too (they are in
the vocabulary the `Import` already emits). Epic 12 Phase A step 2 bakes
a `name → resolved type` map onto the plugin lane and step 3 gates the
recognizer on "the baked map contains this name". Under the mint, **the
guard is a property of the callee symbol**: the mint for `is_X` carries
`guard: { to: base_constant_type(X), asserts: false }`, `assert_X`
carries `asserts: true`, and `recognize_guards`' function-call arm does
`find_callee_symbol(name)` and asks the symbol. Storage: a `Fact { family:
"type_guard", .. }` witness on `Symbol(sid)` pushed by `plugin_emit`
(the bag already carries per-symbol facts and `ReducedValue::FactMap`
reads them), or a field beside `is_constant` on `SymbolDetail::Sub` —
pick whichever the recognizer can read cheapest at walk time; the fact
form needs no `FileAnalysis` field and no `surface_feed` decision.

This is an amendment to Phase A, not a conflict: steps 1, 3–7 and both
CFG-tier obligations are unchanged, and the Language-pack beat's third
rule ("gate on the baked map, not on 'a plugin declared this'") is met
more directly — any language's symbol can carry a guard, minted by
whoever knows. A non-imported user `is_Foo` has no guard fact and narrows
nothing (Phase A's acceptance test still holds). The object form
`$type->check($x)` asks the invocant's type and gains from §3.3: a
`TypeConstraintOf(None)` invocant is still recognizably a constraint;
only its `To(..)` is unavailable.

`assert_X($x)` returning its argument is `ReturnExpr::Arg(0)` in truth,
and `EmitAction::Symbol` has no `ReturnExpr` slot. Do not add one for
this: `q.args` is never threaded (§2), so the declaration would be inert.
Leave `assert_X` return-untyped and record it; it becomes live the day a
Perl call site threads argument types, which is the cpp macro lane's
work, not this brief's.

## 7. Phased ladder

Each phase is one PR, behaviour-frozen on the regression net in §2 plus
the new pins it names. Run `cargo test` (both feature sets), gold cold+
warm, `./e2e/run.sh` before calling any phase verified.

**Phase 1 — widen the inner.** `TypeConstraintOf(Option<Box<InferredType>>)`;
update the 14 sites; `EXTRACT_VERSION` bump. `surface_feed` is untouched
(no new field); `despan` gains the `None` arm. Pin: a declined fold
(`Enum['a','b']`) types as a constraint with no inner, not as nothing.

**Phase 2 — mint + value-dispatched parameterization + delete the gate.**
`on_use` emits `Symbol`s alongside `Import`; `PluginRegistry::parameterize
(owner, name, params)`; the call arm per §3.2; delete
`type_constraint_names()` (trait method, rhai reader, registry union,
`Builder` field, pipeline bake), `constraint_name_imported`, and the
`type_constraint_names` manifest in the rhai. Companions typed from the
same tables (`is_X` → `Bool`, `to_X` → base). Pins: every existing
`synthetic_isa_tests.rs` row unchanged; `-all` mints and `Str` types;
bare `InstanceOf` types `TypeConstraintOf(None)`; hover on `Str` names a
`type-tiny` symbol; a `--definition` probe on `Str` lands on the `use`
statement (record this as a gold row so Phase 6 and 5 move it
deliberately); `--timings` tail and per-file heap on the substrate, three
runs, dated.

**Phase 3 — guard facts on the mint** (Epic 12 Phase A, amended per §6).

**Phase 4 — the isa projection rides the bag.** `ProjectionStep::
ConstrainedInner`; `visit_has_call` publishes `Symbol(accessor) →
Projected { base: Expr(isa_span), step: ConstrainedInner }` instead of
baking `bag_query_expr_span(..)?.constrained_inner()` at walk time. Pure
Perl-side sealed-enum growth (§2: every `Projected` minter is Perl code;
the evaluator is one `match` arm). This is what lets a cross-file
constraint — a house type imported from `GenericCo::Types`, or any
`isa => $t` whose `$t` resolves later — project onto the accessor at
query time through the import chase. Pin: `has m => (isa => MinionRef)`
where `MinionRef` is declared in another indexed file types the
accessor.

**Phase 5 — home minting.** A `patterns()` entry in `type-tiny.rhai`
(trigger `UsesModule("Type::Library")` / `Type::Utils`) on the
`add_type({ name => X, .. })` and `declare X, as T` registration sites
(the same arms `detect_exporter_setup_call` feeds `export_ok` from) mints
`X` + companions in the library's own package — `return_type` from
`base_constant_type(X)` for the standard names, else from the `as`
argument's `inferred_type` (already an `ArgInfo` projection). Then
`definitions()` unions the consumer alias with the home declaration
reached through the `Import` binding, so goto-def on `Str` offers
`Types/Standard.pm` when indexed. Degradation stated in §5: the
`$meta->$add_core_type` spelling does not fold; the consumer mint covers
those names for typing regardless.

**Phase 6 — `UseContext` grows per-name spans and local/remote pairs.**
The mint's span becomes the name's token in the `qw(...)` list (rename
edits the right token; goto-def lands on the name, not the statement),
and a renamed import (`Str => { -as => 'S' }`) mints `S` and
parameterizes as `Str`. Until this lands, a renamed import folds by its
local name and declines — recorded boundary.

## 8. Non-goals

- `ReturnExpr` / `ParametricOp` growth for this problem (§4).
- Element typing for `ArrayRef[T]` / `HashRef[T]` — sequence-types phase
  3; the fold's base-rep answer is its waiting caller.
- `Enum` / `Dict` / `Tuple` folds — each is a fold entry (Dict's natural
  answer is `HashWithKeys`, `adr/structural-shapes.md`); §3.3 makes the
  declined case honest in the meantime.
- Coercions (`to_X` semantics beyond a return type, `coerce => …`).
- Executing `Type::Library` / `setup_import_methods` — the runtime-
  generator boundary stands; this brief models the *installation site*,
  not the generator.
- `namespace::clean` / `autoclean` modeling (§5).
- Threading `q.args` from Perl call sites (§6).

## 9. Options weighed

**(a) Witness only, no symbol.** `on_use` pushes `PackageSymbol{consumer,
Str} → TypeConstraintOf(..)` and the call arm emits
`Edge(PackageSymbol{pkg, name})` for unresolved locals. Lighter (one
witness, no symbol) and it would route *every* imported call through the
bag uniformly. Rejected: it types a name without declaring it — no
hover, no goto-def target, no completion row, no diagnostic resolution —
and the parameterization dispatch has no owner to ask. The symbol is the
honest model of what `import` installs; the witness-only form is the
type-without-a-declaration shape.

**(b) Fully lazy parameterization on the bag.** A `WitnessPayload::
Parameterized { generic, params }` reduced at query time against a sealed
`ParameterizeShape` carried by the home mint, so only the home mints and
the consumer holds a binding. This is the only design under which
per-consumer duplication disappears, and it is what §5 points at if
`-all` ever measures as a cost. Rejected now: it moves a Perl library's
vocabulary into a model-layer reducer as a closed enum (the rhai fold
cannot run at query time), adds a payload variant and a cross-file chase
at every `isa`, and still needs the home indexed to answer at all.

**(c) A core-side `ConstraintShape` manifest replacing the rhai fold.**
Declarative (`InstanceOf: ClassOfParam(0)`, `Maybe: OptionalOfParam(0)`),
so core could fold without calling the plugin. Rejected: the vocabulary
leaves the plugin for a core enum that grows per constructor kind — the
manifest family's rule is that the plugin owns the vocabulary and core
the mechanism, and the mechanism here (extract params, hand them to the
owner) is already in core.

**(d) Shrink the gate to the three parameterized names** (the ADR's
"where to draw the line"). Rejected: two mechanisms, and a global gate on
`Maybe` is still a name a user sub can wear. §3.2 costs no more and has
no list.

**Pick: §3 — mint at `use`, dispatch parameterization to the owner,
widen the inner.** One mechanism for every name; the gate has no residue.

## 10. House library (question 4)

`GenericCo::Types` is a `Type::Library -base` that declares house types and
re-exports Types::Standard's. Under this model:

- `use GenericCo::Types qw(Str MinionRef)` — the kit plugin emits
  `SyntheticUse "Types::Standard"` with the same import list (the seam
  that exists); type-tiny's `on_use` mints `Str` in the consumer.
  `MinionRef` is not in type-tiny's vocabulary → no mint; core's own
  `Import` binds it to `GenericCo::Types`.
- `GenericCo/Types.pm` itself is a *consumer* of Types::Standard, so its
  `declare MinionRef, as InstanceOf['GenericCo::Minion']` folds at walk time
  there (Phase 2), and Phase 5's home pattern mints `MinionRef` +
  companions in `GenericCo::Types` with that fold's result as return type.
- Back in the consumer, `has m => (isa => MinionRef)` reaches the home
  mint through the import chase at query time — Phase 4's lazy projection
  is what makes the accessor see it; goto-def on `MinionRef` lands on the
  `declare`.
- Boundary: `Maybe[MinionRef]` *in the consumer* folds at walk time with
  `params[0].ty = None` (cross-file) and declines to
  `TypeConstraintOf(None)` — a constraint, inner unknown. The kit plugin
  cannot close this without enumerating house names, which it must not;
  §9(b) is the general answer if it ever matters.

## 11. Honest boundary

- **Same-package redefinition.** `use Types::Standard 'Str'; sub Str
  {…}` in one package is a Perl "Subroutine redefined" warning; the
  runtime keeps the later definition, `find_callee_symbol` returns the
  earlier. Not arbitrated — it is a bug in the source, and the case the
  gate existed for (a `sub Str` in a package that never imported it) is
  handled structurally.
- **Renamed imports** fold by local name and decline until Phase 6.
- **`assert_X`** is return-untyped until argument types thread.
- **Home coverage of Types::Standard** is partial (the closure spelling);
  typing does not depend on it, navigation does.
- **`-all` heap** is measured, not assumed; §9(b) is the escape.
- **Method-completion noise** matches `use constant` and Perl; the fix is
  `namespace::clean` modeling, out of scope.
- **Declined folds** (`Enum`, `Dict`, `Tuple`, `HasMethods`, …) type as
  constraints with no inner — visible as "constraint, inner unknown" in
  `--dump-package` provenance, never as silence.

## 12. Corrections to the current record

- `epics/12-type-tiny-completeness.md` mission item 2 describes the gate
  as global; it is import-scoped (`f256c679`). This brief deletes it, so
  the item closes rather than lands.
- The same epic's Phase C calls `completion-typetiny-imported-blessed`
  an xfail to promote; the fixture row's status is `gold`.
- `adr/type-constraints.md` "Both isa spellings agree" — they agre