# C++ templates — extraction, instance typing, and specialization identity

C++ templates resolve through the same witness/substitute/worklist machinery
the type tier already runs; the template-specific work is param-indexed
substitution, instantiation-witness collection, and specialization identity.
This ADR owns the design decisions; `docs/adr/parametric-types.md` owns the
`ParametricType` shape, `docs/adr/return-expr.md` owns the substitution reducer,
and `docs/adr/graph-walking.md` owns the edge taxonomy that carries
`Specializes`.

## The instance is a `ParametricType`

A declared template type `Box<Widget>` is
`Parametric(Instance { base: "Box", args: [ClassName("Widget")] })`, produced by
one structural peel (`ParametricType::instance_from_spelling`, Model layer)
shared by the cpp `annot_type` path and the `TypeName` alias-chase terminal — so
a `typedef`/`using` landing on a template spelling chases to the same Instance.
`class_name()` projects `"Box"` (the dispatch axis), so member gd / completion /
refs / hover light up through the existing `PackageSymbol` path with no
projection logic. `FileAnalysis::dispatch_class_of` adds the one index-aware
refinement: an instance whose exact canonical spelling names a per-spec class
dispatches there, exact-or-primary only; hover keeps the full spelling
(`Symbol::display_type` prefers `exact_spelling()`).

The args are carried deliberately uninterpreted (`int` stays `ClassName("int")`,
not `Numeric`) so the exact-spelling key reconstructs — they are the projection
witness that instance typing consumes. Recursive args are free: `vector<vector<int>>`
is the same tree as `HashRef[ArrayRef[Str]]`, the recursion `parametric-types.md`
chose day one. This is the closed-flavor, per-axis-policy design that ADR
mandates — a parallel `WitnessAttachment::TemplateInst{..}` index is exactly the
drift it exists to avoid. Any shape change here bumps `EXTRACT_VERSION`.

## Param-indexed substitution (the one genuinely new mechanism)

Nothing else in the type tier can say "this member's return *is the class's
first type parameter*." Two halves:

- **Template-def side.** `@tmpl.param` / `@tmpl.owner` skeleton captures feed
  `FileAnalysis.pack.template_params` (per-class ordered param names — primaries by
  base, partial specs by canonical spelling). The writeback translates a
  param-mentioning member return into a deferred `ReturnExpr` on `Symbol(sid)`:
  a bare param → `Operator(ParamOf { index, of: Receiver })`; a param one hop
  under a template spelling (`vector<T>`) → `Operator(InstanceOf { base, args })`.
  Trailing returns (`auto f() -> T*`) extract via sibling skeleton patterns with
  rettype-preferring dedup.
- **Reducer side.** `ParamOf` evaluates beside `RowOf` in `eval_return_expr` —
  the receiver's i-th instance arg, lazily at query time; the receiver rides the
  existing `PackageSymbol` chase (inheritance hops included, so
  `basic_memory_buffer<char>` reaching `buffer<T>::data()` substitutes `char`).
  Fields substitute value-side through `substitute_type_params` in
  `FileAnalysis::member_value_type` — the one receiver-typed member entry the
  sentinel completion, member hover, and the tree-free pack member-chain arm of
  `expr_type_at_span` all route through. Chains compose: `b.get().spin()` /
  `b.v_.spin()` resolve.

Instantiation witnesses (a `template_type` in a declared type, a
`template_function` in a call, a `template_instantiation` at file scope) are
collected as the witness stream and mint rule-#7 refs regardless of whether they
type anything.

## Specialization is dispatch, not hierarchy

A specialization (`template<> struct X<A>`, `struct X<T*>`) gets its **own**
per-spec `Class` symbol plus an `EdgeKind::Specializes` edge to the primary. The
decisive property is member fallthrough: inheritance augments-and-overrides, but
specialization **replaces wholesale** — a spec inherits nothing from the primary
(fmt's primary `formatter` is body-less), so routing specs through
a `PackageFacts::parents` edge would corrupt member resolution. Specs share only the name and
the parameter contract, which is a selection relationship, not a composition
one. It is the third instance of the selection seam: `UnionOnArgs` selects by
arity, `ReceiverGated` by receiver, specialization by type-arg pattern.

Consequences, by construction:

- Member resolution does **not** traverse `Specializes`; goto-implementation /
  family-view **does**. So `references` on the primary finds uses of the primary;
  the specialization family surfaces through the implementations projection.
- A spec may **also** carry a real parent edge (the fmt idiom
  `formatter<X> : formatter<string_view>`) — inheritance is opt-in per spec, and
  per-spec identity carries both edges for free.
- Selection is presented via the ranked, never-pruned multi-location discipline:
  `dispatch_ladder_of` ranks exact-spelling spec > partial-pattern spec
  (`match_template_pattern` — a structural walk binding the spec's params from
  the concrete spelling; specificity is literal-structure count, **not** C++'s
  full partial-ordering algorithm) > primary. A partial match rebinds the
  receiver into the spec's param space so its members substitute the pattern's
  bindings. `definitions()` presents the family ranked, matching spec first,
  primary kept.

The two template jobs map to the two mechanisms: bodied generic code is
PROJECTION (lazy substitution); a body-less typeclass / extension point is
SELECTION (this seam).

## One projection engine

The projection engine design: one worklist + seen-set +
root-chained-provenance spine, with per-domain closures over it — Perl
generator synthesis (strings, eager symbols, plugin-declared) and template
instantiation-to-fixpoint (types, whole-program, syntax-derived). The PoC
modules that proved it (`projection.rs`, `perl_generators.rs`,
`cpp_templates.rs`) were experiment-only and rest in git history (removed
at the spike GC, 2026-07-13); a producer arc re-lands the engine from this
design. Emission policy
and seen-set granularity stay the caller's — expressed at the call boundary, not
by a branch inside the engine — because eager per-declaration symbol minting is
right for a finite Perl generator group and wrong for a template × every
instantiation spelling (combinatorial). The shared spine is the discipline; the
emission is per-language.

Projection is **lazy** for LSP queries: one template symbol, substitute at query
time in the reducer. Per-instantiation materialization is the parallel store
"edges, not values" bans; an eager whole-program monomorphizer (the
`cpp_template_join` PoC, same git-history resting place) runs on the same
engine and stays unbuilt until a call-graph / heatmap consumer pulls it. Outline / workspace-symbols show
primaries and explicit instantiations (the latter are deliberate, enumerable,
and the whole content of an explicit-instantiation TU), not every witnessed
instantiation.

## Extraction hygiene

Template shapes extract on the same skeleton machinery as plain classes:
per-spec class identity, explicit-instantiation outline items (a *reference* to
the primary, not a def, and its params no longer leak as `@def.local`),
out-of-line member joins (`Buf<T>::grow` normalizes the `template_type`
qualifier to the base class by structural peel, not string-splitting on `<`),
`using`-alias and concept symbols, and `base_class_clause` `@parent` patterns
that cover namespace-qualified template bases (`: public detail::buffer<T>`).
Pack function scopes classify as sub-body (`ScopeKind` at query_extract.rs) so
params/locals leave the outline — the general fix for pack outline noise, not
template-specific.

## Deferred

Recorded, not queued: dependent types one hop (`T::value_type` — needs the alias
graph), deduction from value args (`ident(4)` inferring `T=int` — rides the
call-graph/overload lane), SFINAE selection and the full overload-ranking
lattice (gold-roadmap Tier 2: `exact ≻ promotion ≻ standard-conversion ≻
user-defined`, partial-ordering), concept *checking* (names extract; `requires`
is not evaluated), variadic packs, template-template params (a param in base
position never pattern-matches), constexpr/NTTP evaluation, template members
behind declarator-position macros (`FMT_CONSTEXPR auto data()` — the macro
lane), and the combinatorial call-graph join.
