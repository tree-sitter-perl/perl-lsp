# C++ TEMPLATE arc — tee-off map

Orientation for the template arc: what extracts/resolves today (measured),
how the PR #100 projections model maps onto C++ templates, the slice
sequence, and the design forks that are the user's to call. No
implementation here — this is the brief the arc fires from.

**Measured on:** `spike/cpp-support` @ `887840c` (refs-symmetry landed),
`cargo build --release --features all-langs`, probing with the CLI
(`--parse` / `--outline` / `--definition` / `--references` / `--hover`)
against `/home/veesh/personal/cpp-bench/` (fmt, abseil-cpp, folly, json)
plus minimal fixtures. Every row below was probed live, not inferred.

---

## 1. Current state, measured

Headline correction to the queue notes: **"template-wrapped defs aren't
extracted at all" is stale.** The skeleton's def patterns are unrooted, and
`template_declaration` wraps ordinary `function_definition` /
`struct_specifier` / `class_specifier` nodes (`--parse` confirms — the CST
is clean), so primary templates extract today. The KNOWN-GAPS "Refs
symmetry" row and both fmt hitlist items need re-anchoring:

- `thousands_sep_result` (format.h:1161): **gr = 7 refs across 3 files, gd
  works both directions** (use→def at 1169→1161; format.cc:27→format.h).
  Measured green on the spike tip — the symmetry arc's
  `(type_identifier) @ref.type` → PackageRef emission closed it. Promote
  to a gold regression row; don't chase it.
- fmt `src/format.cc` outline = params only (`loc`, `x`): **confirmed
  dark**, but the mechanism is *explicit instantiation*, not
  template-wrapped definition. format.cc contains 10
  `template_instantiation` nodes and **zero** `function_definition`s — an
  explicit instantiation mints no symbol and no reference, while its
  `parameter_declaration`s still mint `@def.local` Variables that float to
  top level.

### Shape matrix

| # | shape | extract | gd | gr | evidence |
|---|-------|---------|----|----|----------|
| 1 | template function, classic return (`template<T> T max(T,T)`) | ✅ Sub | ✅ from calls, incl. explicit-arg `f<int>(x)` | ✅ same-file (2/2); ⚠️ cross-file from a prototype under-collects (`thousands_sep_impl` decl → 1 hit; real sites: def in format-inl.h + call at format.h:1170 + 2 extern-template) | minimal fixture + fmt |
| 2 | template function, trailing return (`template<T> auto f(..) -> T`) | ✅ Sub | ✅ | (as #1) | `max2` fixture; fmt format.h:1169 |
| 3 | template struct/class + members | ✅ Class + members (`@context.class` tags them) | ✅ both directions, cross-file | ✅ 7 refs on `thousands_sep_result`; 44 Classes / 159 Methods from format.h | fmt format.h |
| 4 | template member fn in a plain class | ✅ classifies Method (class-owned Sub reclassification, query_extract.rs ~323) | ✅ | ✅ | `Holder::convert` fixture |
| 5 | **full/partial class specialization** (`template<> struct X<A>`, `struct X<T*>`) | ❌ name is a `template_type`, not `type_identifier` — no Class symbol; **members extract orphaned** (empty package) | ❌ | ❌ | `Traits<T*>` fixture; **fmt: 38 `formatter<...>` specializations invisible** — fmt's user-facing extension point |
| 6 | **out-of-line template member def** (`template<T> void Buf<T>::grow(..)`) | ⚠️ extracts, but `package = "Buf<T>"` verbatim ≠ class `Buf` — never joins the class; decl↔def don't unify | ❌ | ❌ | `Buf` fixture: outline shows `Method Buf grow` AND `Method Buf<T> grow` as strangers |
| 7 | **explicit instantiation / `extern template`** | ❌ no symbol, no ref to the template; param decls leak as top-level Variables | ❌ | ❌ | fmt src/format.cc — the whole file is this shape; outline = `loc`, `x` |
| 8 | alias template + plain `using X = T` | ❌ no symbol (KNOWN-GAPS row); alias witness only | ❌ name token reads as self-referencing type use | ❌ | format.h: 46 `alias_declaration`s → 0 symbols |
| 9 | variable template (`template<T> constexpr bool v = ..`) | ✅ Variable (leaks into outline like all pack locals — see below) | — | — | fixture |
| 10 | concept (`template<T> concept C = requires(T a) {..}`) | ❌ name unextracted; **requires-expr params leak as top-level Variables** | ❌ | ❌ | `Addable` fixture: outline shows `a`, not `Addable` |
| 11 | **member access on a template instance** (`Box<Widget> b; b.size()`) | n/a | ❌ **dark** — control (plain class) works | ❌ member refs don't include these sites | see below |
| 12 | inheritance through a template base (`struct D : base<T>`) | ❌ no `@parent` edge — `base_class_clause` pattern only matches `(type_identifier)`, `base<T>` is a `template_type` | ❌ inherited-member gd fails | ❌ | `derived : base<T>` fixture |
| 13 | instantiation-aware typing (`Box<int> b; b.get()` → int) | — | ❌ fully dark — no projection machinery is wired | — | spike exists (`src/cpp_templates.rs`), unwired |

### Row 11 is the DX headline

```
template <typename T> class Box { T get(); int size(); T v_; };
Box<Widget> b;
b.size();          // gd: NOTHING   (plain-class control: works)
b.                 // completion: no members offered
```

Mechanism: the declared type text `Box<Widget>` flows through the cpp
pack's `annot_type` untouched (`!tag.contains(' ')` passes, so it becomes
`ClassName("Box<Widget>")` — hover confirms `b: Box<Widget>`). Members are
keyed under class `Box`, so `MethodOnClass{“Box<Widget>”, size}` misses.
**Every variable of every template-class type in every workspace hits
this** — `std::string`-adjacent house types, `vector<>`, fmt's
`basic_memory_buffer<char>`, all of it. Fixing just the *base-name join*
(no arg semantics at all) lights up member gd / completion / refs through
machinery that already works for plain classes.

### The outline-noise mechanism (general, not template-specific)

Params/locals appear in `--outline` for *plain* functions too: pack
extraction mints every scope as `ScopeKind::Block`
(query_extract.rs:1432), and the outline filter drops Variables only when
`scope_within_sub_body` — which requires a `Sub`/`Method` scope kind — so
cpp function scopes never shield their locals. The format.cc case is worse
(there's no function scope at all around an explicit instantiation's
params) but the fix is shared: classify pack function scopes as sub-body.

### Scale (how much surface is invisible)

fmt `include/fmt/format.h` (4415 lines, 223 `template_declaration`s):
CST has 321 `function_definition` / 59 named class+struct specifiers / 46
`alias_declaration`s; extraction yields 144 Sub + 159 Method + 44 Class +
0 aliases. So primaries mostly land; the loss is concentrated in
specializations (13 `template <>` + the partial-spec family — incl. all 38
`formatter<...>`), aliases, and the 1041 leaked Variables drowning the
outline. Broader corpus (one-file spot checks): abseil `span.h` 31 ERROR
nodes, folly `Expected.h` 46, nlohmann `json.hpp` 185 ERRORs with 64
class-specifiers → 5 extracted — on torture code the *first* loss is still
macro parse damage (the macro arc's lane), with template shapes second.
Template work should not be blamed for what `NLOHMANN_*` macros break.

### Acceptance anchors for the arc

1. **fmt `src/format.cc` outline**: shows the 10 instantiation targets (as
   references/nav, per fork #2 below) and zero stray `loc`/`x` Variables.
2. **`formatter<...>` specializations**: extracted, members owned, gr from
   the primary (or the family — fork #4) finds them. `--references` on
   `formatter` def in base.h currently can't see 38 defs.
3. **`Box<Widget> b; b.size()`** gd/completion/refs — the row-11 fixture,
   plus a real-corpus pin (e.g. `basic_memory_buffer<char>` member nav).
4. **regression guard**: `thousands_sep_result` gd/gr stays green (author
   the gold rows now — it works today and nothing should regress it).

---

## 2. The projections model, applied (PR #100 → templates)

PR #100 (`feat/perl-generators`, DRAFT — design input, not settled API)
lands the Perl half of the metaprogram-witness tier: a plugin-declared
`GeneratorDef` is projected over call-site witnesses by a fixpoint
worklist (`src/generators.rs::project`) — per-call seen-set, `${param}`
interpolation, provenance chained to the root call. The C++ half already
exists as an unwired spike: `src/cpp_templates.rs` (collect templates +
seed instantiations + `instantiate_to_fixpoint`) and
`src/cpp_template_join.rs` (per-witness overload dispatch). Put side by
side, the two are the *same function* with different substitution domains:

| | Perl generators (PR #100) | C++ templates (spike) |
|---|---|---|
| generator def | `GeneratorDef { params, actions }` (plugin manifest) | `Template { params, dependent_types, body_instantiations, .. }` (from syntax — core-native, no plugin needed) |
| witness | call site + literal string args | instantiation site + concrete type args (`f<int>(..)`, `Box<Widget> b`, explicit/extern instantiation) |
| substitution | `${name}` string interpolation | type-param → concrete type; `T::value_type` dissolves |
| projection output | **eager real symbols** (`Namespace::Framework`, span = call site) | (open — fork #1/#2) types and/or symbols |
| engine | worklist + per-call seen-set + root-chained provenance | worklist + seen-set (identical discipline, separate code) |

**What reuses existing machinery:**

- **Witness bag + `MethodOnClass`** — the projection consumer. A
  `Box<Widget>` receiver asking for `get` is a `MethodOnClass{Box, get}`
  query whose *answer* needs one extra step: substitute the receiver's
  type args into a param-shaped return.
- **`ReturnExpr` substitution seam** (`docs/adr/return-expr.md`) — the
  reducer already substitutes `q.receiver` for `Receiver` placeholders.
  "Return type is the receiver's i-th type arg" is the same move with a
  param index: a `ReturnExpr::Operator`-family shape (call it
  `ParamOf(i)`), evaluated lazily at query time exactly like `RowOf`.
- **`ParametricType`** (`docs/adr/parametric-types.md`) — the type-tier
  home for an instance. `Parametric(Instance { base: "Box", args:
  [ClassName("Widget")] })` with `class_name() = "Box"` (dispatch axis)
  is precisely the sealed-flavor, per-axis-policy design the ADR mandates
  — DBIC's `ResultSet{base, row}` is the same "one value, two axes" shape.
  Recursive args come free (the ADR chose recursion day-one for
  `HashRef[ArrayRef[Str]]`; `vector<vector<int>>` is the same tree).
- **Graph edges** — `: base<T>` inheritance rides the existing
  `@parent`/`package_parents` walk once the pattern matches
  `template_type` bases; specialization-family edges (fork #4) would ride
  `GraphView`'s closed `EdgeKind` if we mint them.
- **The worklist discipline** — seen-set on the dispatcher, monotone
  witnesses, clear-and-emit for re-emittable passes (CLAUDE.md worklist
  invariants). The spike's `instantiate_to_fixpoint` already obeys it.

**What is genuinely new:**

- **Param-indexed substitution in the type tier.** Nothing today can say
  "this member's return *is the class's first type parameter*." Template
  defs must record, per member, which slots of the signature mention
  which params (the spike's `dependent_types`/`value_params` are the
  sketch); the reducer needs the `ParamOf(i)` evaluation.
- **Instantiation-witness collection as emission.** `template_type` in a
  declared type, `template_function` in a call, `template_instantiation`
  at file scope — each is today either a plain type-ref or invisible.
  They become the witness stream (and rule-#7 refs regardless).
- **Specialization identity** — no Perl analog. A `formatter<float128>`
  both *is* `formatter` (for the family splat) and *isn't* (its own def
  site, own members). The macro arc's config-variant multi-def model
  (every `#define` variant is a decl; ranked, never pruned) is the closest
  in-house precedent.

**Flags on PR #100 from the template arc's seat** (it's a draft; these are
requests, not blockers):

1. `GenWitness.args: Vec<String>` — the C++ witness carries *types*
   (`TypeArg::Concrete/Param`), and even Perl wants richer args soon
   (follow-up #3 in the PR). If the engines unify (fork #3), `project`
   should be generic over the substitution domain the way it already is
   over provenance `P`.
2. `GenSymbol.kind: String` (`"method"`/`"accessor"`) with no
   params/return payload — templates need typed emissions
   (`SymKind` + return-type template). The PR's follow-up #4 already
   points here; worth designing the payload once for both.
3. Eager symbol minting is the right default for Perl (one call site =
   one finite group) but the wrong default for templates (one template ×
   every instantiation spelling = combinatorial). The shared spine is the
   worklist + provenance discipline, **not** the emission policy — keep
   emission per-language (fork #1).
4. The PR's trigger-independence gap (fires without the defining module
   in scope) has no C++ analog — a template witness is structurally tied
   to a resolvable template name. No action needed for this arc; the gate
   remains Perl's #1 follow-up.

---

## 3. Slice sequence

**(a) Extraction hygiene through template shapes — CHEAP, no projection
machinery.** Smaller than originally briefed (primaries already extract);
what's left is the enumerated residue of the shape matrix:

  - a1. **Explicit instantiation / `extern template`** → mint a
    *reference* to the primary template (it's a use, not a def) and stop
    minting its params as `@def.local`. Closes acceptance anchor #1.
  - a2. **Class specializations** (`template<> struct X<..>` + partial):
    extract a def whose name joins base `X`; `@context.class` tags the
    members so they stop orphaning. (Identity semantics = fork #4, but
    extraction can land first — members owned, def findable.) Closes
    anchor #2.
  - a3. **Out-of-line template member** (`Buf<T>::grow`): normalize the
    qualifier to the base class name so it unifies with the in-class
    decl (structural peel of `template_type` in the qualifier — not
    string-splitting on `<`).
  - a4. **`using` alias name** (template and plain) → a Class-kinded
    symbol carrying the existing alias edge (also clears the KNOWN-GAPS
    "using alias" row).
  - a5. **Concept names** → symbols; requires-expr params stop leaking.
  - a6. **Pack function scopes read as sub-body** (`ScopeKind` at
    query_extract.rs:1432) so params/locals leave the outline — the other
    half of the fmt outline-noise item, and it benefits every pack
    language, not just templates.

**(b) The instance joins the class — the "class with unbound params"
slice.** Declared type `Box<Widget>` becomes
`Parametric(Instance { base: "Box", args: [...] })` (peeled structurally
from the `template_type` node at `@type.annot` — the node has
`name`/`arguments` fields; do NOT regex the text). `class_name()`
projects `"Box"`, so member gd / completion / refs / hover light up
through the *existing* `MethodOnClass` path with zero projection logic —
plus the `base_class_clause (template_type ...)` `@parent` pattern so
`: base<T>` inheritance walks. The args are carried, unused-for-now: they
are the projection witness slice (c) consumes. Closes anchor #3 at the
"resolve the member" level (returns still untyped where they mention
params). This is where most of the *felt* DX lives.

**(c) Instantiation-aware typing — the real projections slice.** Member
signatures that mention template params substitute the receiver's args:
`Box<int>::get()` → `int`; `vector<string>::front()` → `string`. Scoped
honestly as an additive-depth ladder (evaluate cost per level, per the
golive map's framing):

  - c1. Template-def side: record param-mentions in member return types
    (the `T get()` case — a bare param) + emit `ParamOf(i)`-shaped
    `ReturnExpr` witnesses on `MethodOnClass{base, member}`.
  - c2. Reducer side: substitute from the receiver's `Instance.args` at
    query time (lazy, like `RowOf` — see fork #1). Chains compose free:
    `b.get().spin()` works once `get()` answers `Widget`.
  - c3. Dependent types one hop (`T::value_type` where `T`'s witness has
    a member/alias by that name) — needs the alias graph; only if a
    corpus case demands it.
  - c4. Deduction from value args (`ident(4)` infers `T=int` → the
    template-join spike's lattice lane). Explicit-args and
    declared-variable witnesses (c1/c2) cover the LSP-navigation bulk;
    deduction mostly matters for call-graph/overload work — likely
    deferred with (d).

**(d) Stays parked** (recorded, not queued): SFINAE selection, the full
overload ranking lattice (gold-roadmap Tier 2 — `exact ≻ promotion ≻
standard-conversion ≻ user-defined`, partial-ordering), concept
*checking* (we extract the name; we don't evaluate `requires`), variadic
packs, template-template params, constexpr/NTTP evaluation, and the
combinatorial call-graph join (`cpp_template_join.rs` stays a spike until
a heatmap/call-graph consumer pulls it).

Sequencing note: (a) and (b) are independent of PR #100 entirely — they
need no generator machinery and can tee off immediately. (c) is where the
projection design commitments (forks below) bind.

---

## 4. Design forks — DECIDED (user, 2026-07-02) except #4

- **1 LOCKED: lazy** ("lazy for certain").
- **2 LOCKED: primaries + explicit instantiations in outline.**
- **3 LOCKED — OVERRIDES the rec below: unify NOW.** The engines merge
  immediately; PR #100 is subordinate — at maximum we close the open PR and
  re-extract the Perl generator surface from the unified engine. The
  unification leads; the PR follows.
- **4 PENDING — the user's call**, tradeoffs on the table: (A) one family
  (the macro config-variant model) vs (B) per-spec symbols + `specializes`
  edges. Recommendation: **B's identity with A's presentation** — per-spec
  symbols (specializations are all live simultaneously with DISTINCT member
  tables, so member resolution demands real identity; a config-variant macro
  is a SUPERPOSITION — config-exclusive alternatives — where rank/join is
  honest, but siblings-coexisting is a different beast), `specializes` edge
  in GraphView, and the macro arc's ranked never-prune multi-location as the
  *presentation* when a use resolves against the family (matching spec first,
  primary + siblings kept). Spec-match specificity = the pragmatic ladder
  (exact args > partial pattern > primary), NOT C++'s full partial-ordering
  algorithm — additive depth, later.
- **5 LOCKED: the `ParametricType::Instance` flavor.**

Original fork writeups (kept for the reasoning):

1. **Lazy vs eager projection.** Eager = pre-materialize per-instantiation
   answers at build (what `instantiate_to_fixpoint` does); lazy = keep one
   template symbol + substitute at query time in the reducer (what `RowOf`
   does). House priors lean lazy — "edges, not values" explicitly bans
   materialized parallel stores, and per-instantiation copies of every
   member is exactly that — but eager is what the call-graph/heatmap
   consumer will eventually want, and the spike already proves it. A
   hybrid (lazy for LSP queries; eager fixpoint only when a whole-project
   consumer runs) is plausible. **Recommendation: lazy for this arc;
   revisit when the call-graph consumer is real.**

2. **Do projected entities appear in outline / workspace-symbols?** Perl
   precedent says synthesized = real symbols (Moo `has`, DBIC, PR #100) —
   but those are per-*declaration* and finite; template projections are
   per-*use*. Options: (i) primaries only (projections resolve on demand,
   invisible in outline); (ii) explicit instantiations also get outline
   entries (they're deliberate, enumerable, and format.cc's whole content);
   (iii) every witnessed instantiation. **Recommendation: (i) + (ii) —
   and (ii) is required anyway for acceptance anchor #1.** Analogous to
   the macro arc's blank/visible-base forks.

3. **One projection engine or two disciplined twins?**
   `generators.rs::project` (strings, plugin-declared) and
   `cpp_templates.rs::instantiate_to_fixpoint` (types, syntax-derived) are
   parallel code today. Unify now (make `project` generic over the
   substitution domain; templates become a core-native `GeneratorDef`
   producer) — or keep both and unify when a third consumer (macro
   parametric returns? Python generics?) forces the shape. Unifying now is
   the rule-#10 "larger commit"; but PR #100 is unreviewed, and freezing
   its API by building C++ on it inverts the review order.
   **Recommendation: keep the discipline shared (worklist/seen-set/
   provenance-chain as a written contract), the code twin, until PR #100
   is reviewed/landed — then fold `cpp_templates.rs` onto whatever
   survives.** This is also the "does the Perl plugin API carry as-is"
   answer: the *manifest* doesn't (templates aren't plugin-declared); the
   *engine* should, eventually.

4. **Specialization identity.** Is `formatter<float128, Char>` (i) a
   member of one `formatter` symbol family — gr on the primary splats all
   38 specializations, rename renames the family, goto-def is
   multi-location ranked (the macro config-variant model); or (ii) its own
   symbol with a `specializes` edge to the primary — gr on the primary
   finds *uses of the primary only*, and the family view is
   goto-implementation? Affects refs counts, rename semantics, and the
   gold rows we author. **Genuinely the user's call**; (i) matches how
   the macro arc treated multi-defs, (ii) matches how inheritance
   (override) is treated.

5. **Where the instance type lives.** `ParametricType::Instance { base,
   args }` flavor (recommended — reuses the sealed-enum, per-axis-policy
   ADR and every existing consumer of `class_name()`) vs a new
   `WitnessAttachment::TemplateInst{..}` (a parallel index the ADR pattern
   exists to avoid). Also binds the Perl side: if Perl ever grows
   parameterized generators-of-types, it lands in the same flavor.
   **Recommendation: the flavor.** Cache note: any of this rides the
   bincode blob → `EXTRACT_VERSION` bump.

---

## 5. Cross-language check — real vs aspirational

**Real today:**
- The witness/substitute/worklist/seen-set/provenance-chain *discipline*
  is genuinely shared and independently implemented twice
  (`generators.rs` PR #100; `cpp_templates.rs` spike) with matching
  semantics (per-call seen-set, root-chained provenance, never execute).
- `ParametricType` + the lazy-projection reducer precedent (`RowOf`) and
  the `ReturnExpr` receiver-substitution seam exist, serde-clean, in
  production for Perl (DBIC) — slice (c) is "one more shape," not a new
  tier.
- The `parents_of` seam, `MethodOnClass` inheritance walk, and the
  refs-symmetry machinery are language-neutral and already carry cpp.

**Aspirational (don't claim it yet):**
- A *single* projection engine (fork #3) — today it's two twins.
- Perl generators and C++ templates sharing an emission model — eager
  symbols vs (recommended) lazy types are different policies on purpose.
- Any consumer of `cpp_templates.rs` — the spike is measured by its own
  tests and wired to nothing; the join spike likewise.
- Python generics riding the same flavor — plausible (`list[int]` is an
  `Instance`), unprobed.

**Bookkeeping when the arc tees off:** update the KNOWN-GAPS "Refs
symmetry" template row (stale as measured), author the anchor #4 gold
rows, and promote/retire the `cpp-template-member-is-method` xfail if not
already promoted.
