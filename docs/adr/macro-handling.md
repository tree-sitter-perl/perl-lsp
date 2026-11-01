# ADR: Semantic handling of C/C++ macros

Status: accepted (incremental).

## Context

Macros are pervasive in C / perl5 and carry real program meaning, but a macro
is not one thing. The gather historically **expanded every harmless macro**
(keep the rewrite if `parse_damage` doesn't increase), which (a) destroys the
macro's identity for hover/goto-def, (b) generates a splice per use — bloating
the no-copy `SpliceMap` and its span-remap bugs, and (c) still leaves macro
goto-def broadly broken (a use resolves to itself; cross-file `#define`s aren't
registered by name; the reachability ranking is computed but never consulted).

The type lane already works (object-like **type** macros → `TypeName` edges;
config-variant → variant capture + reachability + join, `cpp_macro_model.rs`).
Everything else — identity, navigation, function-like typing — does not.

## Decision

Treat a macro as a **named entity with (optional) params and a body**, and
lane it by what the body *is*. Model, don't expand, wherever the body is
already valid syntax.

### Lanes

| kind | body | treatment |
|---|---|---|
| object-like, type-valued | `#define X U16` | `TypeName(X)` alias edge (landed) |
| object-like, value | `#define MAX 100` | typed constant (infer type from body) |
| object-like, marker | `#define FLAG` | flag symbol — refs/goto, no type |
| function-like, expression | `#define MAX(a,b) …` | **global sub, implied return typing** |
| function-like, delegation | `#define F(x) G(x)` | as above + **see-through value-witness** |
| **member-block** | `#define BASEOP … op_type:9; …` (used in a struct body) | **role: a `PackageFacts::parents` edge; blank the use** |
| syntactic / statement / `##` | `do {…} while(0)` | **expand** (parse repair only) |

### The classifier: usage position, not body-parse

A macro body is an ambiguous token blob; its grammatical ROLE is fixed by
**where it is used**, not by parsing the body in isolation. Use position is the
classifier; the body-parse only supplies contents *once the role is known*:

| use position | role |
|---|---|
| type position (`FOO x;`) | type macro → `TypeName` |
| expression / call `FOO(args)` | value / function-like → sub-return |
| standalone in a struct/class body (`FOO;`) | **member-block → role** |
| statement position | syntactic → expand |

Usage-driven and lazy: a never-used macro is just a navigable macro symbol
until a use classifies it. Parked zero-usage fallback — *test-parse the body in
each context, take the one that parses clean* — a real but ambiguous signal,
heuristic not primary.

### The load-bearing move: three expansion modes, keyed by role

The gather no longer "expands if harmless." Each use is **leave / blank /
expand** by its classified role:

- **leave** — parses clean unexpanded (type → `type_identifier`, function-like →
  `call_expression`). Zero splices; the macro is a first-class symbol/edge, and
  a function-like macro *is* a package-global sub typed from its body.
- **blank** — inline tokens would parse as a phantom, but the meaning is an edge
  (member-block → role). Replace the use with whitespace so it parses clean
  (`struct op { BASEOP };` → `struct op { };`); one splice; members come from
  the role. **Blank is only in the transformed/parse view — the original source
  keeps the token, so goto-def-on-`BASEOP` identity is untouched.**
- **expand** — syntactic/statement macros that must be present for a clean parse
  (`parse_damage` strictly decreases).

The splice count collapses to blank+repair sites only; most `SpliceMap`
span-remap bugs evaporate structurally. (`alias_only` was the baby step.)

### Member-block macros = roles (the copypasta = inheritance)

`#define BASEOP …fields…` pasted into every op struct is a **role** — the shape
of a Perl `with`, already modeled as a `PackageFacts::parents` edge. Mint **one**
visible synthetic base per macro (a navigable, reusable symbol), members parsed
from the body under the field-block role, taken from the **config-active
variant** (reachability reuse). Add a parent edge per pasting struct
(`struct op → BASEOP`). The *existing* ancestor walk then delivers everything —
`o->op_type` resolution, completion, hover, and the **references splat**: every
`->op_type` across every pasting struct converges on the one `BASEOP::op_type`
def (op.h:55), which is both the copypasta anchor and the inheritance anchor.
Roles all the way (rule #10) — even a one-member macro is a role, never a
special-cased inline.

**Edges handle the re-sourcing.** op_type's typing moves from expanded-field to
role-member, but because the model is edges, the role member simply emits the
same `TypeName` edge the expanded field did; the registry chase is agnostic to
the emission site, so slice 2's hover leaf and the type chase keep working
unchanged. The emission site is fungible; the edge is canonical.

### Config-variant = superposition, join over the flowing dimension

A config-variant macro is N `#if` variants = **return arms** of a superposition.
The existing `SymbolReturnArmFold` unions them ("arms agree → the type, disagree
→ widen"). The *flowing dimension* is the return type today; when effects are
modeled they are a second dimension with their own fold — same invariant, "flow
over the blob, union what flows." The blob is never collapsed; the reducer
unions per query.

### Typing vs. display: abstraction for one, concrete leaf for the other

The type that *flows* is the join **abstraction** (an integer). Hover recovers
the **concrete leaf via provenance** — the terminal spelling of the
**reachability-ranked** (config-active) variant (`PERL_BITFIELD16 → U16 →
unsigned short`). Abstraction for inference, concrete leaf for the human.

### Goto-def / navigation

- **Register** object-like / type / function-like macros as **named cross-file
  symbols** in the pack index (fixes `PERL_BITFIELD16` / `SvREFCNT_inc`
  cross-file "No definition found").
- **Prefer the `#define`** over the use's self-span (fixes bare `BASEOP`
  resolving to itself).
- **Multi-location, reachability-ranked** — return **all** def sites (never
  prune: portability), ranked config-active first, others kept + labeled
  ("unreachable: WIN32 undefined"). This finally *consumes* the `MacroVariants`
  ranking (fixes win32-wins).
- **See-through** — a direct-delegation wrapper's goto-def reaches the delegate
  (`SvREFCNT_inc → Perl_SvREFCNT_inc`) via a delegation value-witness.
  The backward half is minted at the use: the build resolves each used
  macro name's transitive delegate chain through the file's own `#define`s
  and its gathered include closure (every config variant counts —
  `Macro::all_delegates`, read the same way from both tables) and stamps it
  on each use site as a `RefTable` column (`RefDelegates`: a per-ref id
  into chains interned per macro, so the table is empty outside C and no
  `Ref` grows). The stamp runs over the final refs before the indices
  build, so the use is indexed and row-shredded under each delegate's key
  in the one index build: references on `Perl_SvREFCNT_inc` find
  `SvREFCNT_inc(x)` through the ordinary key bucket, with no query-time
  macro scan and no query-time visibility check — the file's own
  preprocessor view already decided it. A site gets the chain only where
  the preprocessor expands the name there: an object-like macro at any
  call, read or type-position ref; a function-like one only at a call or
  an expanded site (an erased use, a token inside a `#define` body), so a
  same-named variable is not a reference to the delegate. Freshness rides
  the Surface: macro bodies are on it, so a header edit that changes a
  delegate re-analyzes its includers. Known over-match: a use outside the
  `#define`'s live range (before it, or after an `#undef`) still lists.

### Resolution visibility = the include-closure lie (generalizes beyond macros)

C linkage is globally flat, so today every symbol registers into one global
namespace — and two unrelated translation units that both declare `class Box`
(fixtures, vendored deps, multiple independent binaries, generated variants)
**collide**. The determinism fix made the collision *stable* (order-independent
winner) but not *correct*: a file can still resolve a name to a same-named
symbol it can't actually see. This surfaced as the autoret flakiness — the real
bug was "cross-file resolution of two same-named classes is arbitrary."

The fiction that fixes it: **scope cross-file *resolution* by include-
reachability.** A file resolves names only against symbols reachable from *its*
include closure — `autoret.cpp` sees *its* `Box`, `methodchain.cpp` sees *its*.
It's a lie (linkage is global) but it's the visibility the programmer reasons
with ("what's in scope *here*"), and it's the **same lie the gather already
tells for macros** (a file's macros = its include closure). Making the symbol
model tell it too is consistency, not novelty.

Shape: keep the **global registry** (find-anywhere, portability) and add a
**reachability filter/rank** on resolution — identical to the macro multi-def
ranking above (all defs kept, ranked by config-reachability). It reuses that
machinery (`cpp_macro_model::classify` + the per-file include closure the
gather already computes). Symbol resolution and macro resolution converge on
one pattern: **global set + reachability scope.**

### Declarations ranked by their `#if` arm

Every arm of a header is extracted, whatever the toolchain: a C view and a
C++ view of `string.h` hold the same declarations. So the answer to "which
`strchr` does a `.c` file mean" is not a second copy of the header but a fact
on each declaration — the arm it sits in — ranked against the asker's config
at query time, the way macro variants already are. Nothing is pruned.

- `Symbol::arm` and `MacroDef::arm` are bindings: the innermost arm, an id
  into the file's arm table. `NONE` is unconditional, and is what every
  language whose document declares no arms mints. The extractor and the
  gather's `#define` walk both place arms from the `@cond.*` captures of
  `queries/cpp/cond.scm` (`docs/adr/capture-vocabulary.md`), so an include
  guard is recognised in one place.
- A trail term is `CondTerm { test, negated }`: an `#if`'s source text or an
  `#ifdef`'s name, and whether the code sits where it fails. The negation is a
  field, so the evaluator never re-reads a rendered `!(…)` (rule #13). A
  gathered `Macro` carries its terms directly, because a gathered table merges
  defines from many headers' arm tables.

Two `PackFacts` fields (`layering_tests::pack_facts_fields_are_ratcheted`):

- `cond_arms` — the intern table those bindings point into, one trail per
  arm. Not a witness (it types nothing), not a ref binding (the binding is the
  id on the symbol), not a convention (each file has its own conditionals).
  Projected onto the Surface through each entry's trail: a declaration moving
  into an `#ifdef` changes which configs see it.
- `toolchain` — the toolchain the file was built with, minted by the cpp
  driver from `unit_toolchain` (`None` for every other language). Per file,
  not per language: one project mixes `.c` and `.cpp` units, and a database
  entry names its own driver and `-std`, so it is no convention reached by
  language id. Not a witness or a binding: it is an input the build read, the
  config a query asked from this file ranks under. A Surface discard: it moves
  only this file's own queries, and the store's input fingerprint folds every
  toolchain the project probes.

The config a trail is ranked against is built in one place,
`KnownConfig::from_defines` (`build/cpp_macro_model.rs`), from every
`#define` site the closure holds (name + trail) and the toolchain's predefined
names; build-side variant selection and navigation both reach it through
`cpp_reparse::unit_config`. It then SETTLES: a knob whose every `#define`
sits under a provably-false trail cannot be on, so it leaves the universe, and
the pass repeats until nothing moves. It only shrinks the universe, so it
terminates, and it never promotes a knob to ON (a later `#undef` could undo
that; #255's undef spans are what would feed it). glibc's
`__CORRECT_ISO_CPP_STRING_H_PROTO` is defined only under `defined
__cplusplus && …`, so from a `.c` unit the `extern "C++"` overloads it
guards rank unreachable, labelled `… undefined (__cplusplus undefined)`.
The evaluator reads a term it does not model — a comparison, arithmetic, a
function-like call such as `__GNUC_PREREQ (4, 4)` — as an opaque Unknown
INSIDE the boolean structure, so `False && opaque` is still False; only a
condition that does not parse is Unknown as a whole. A `\` line splice is
whitespace.

A query ranks under the ASKER's config: `resolve::ranking_config` reads the
asker's `PackFacts::toolchain` (never its path —
`layering_tests::toolchains_are_derived_only_in_the_build_tier`) and every
`#define` site the asker and the index hold. It is built per question over
only the names the question reaches (`KnownConfig::over_reach`): the names
the candidates' guards read, then every name a reached knob's own `#define`
trails read. A knob settles on exactly those names, pass for pass, so each
verdict and its reason are the whole-universe config's. The sites come from
the index's `#define` table (`resolve::DefineTable`), which follows the
index one changed file at a time — a whole-index config rebuilt per epoch
made every macro goto-def after an index change pay a sweep over every
file's macro table plus a full settle (~35 ms warm on curl, measured
2026-11-01). Macro variants
(`ranked_macro_variants`) and declarations share it: reachability is the
first sort key of the overload ranking and of `pack_symbol_def_location`, its
verdict rides `RefLocation::label`, hover presents the top candidate, and
completion offers a name whose every declaration is unreachable at
`PRIORITY_UNREACHABLE`. `ArmRanker` builds the config only when a candidate
sits in an arm, so a language that mints no arms never builds one.

### Parametric return

An identity/projection macro (`#define ID(x) (x)`, `#define SEL2(a,b) (b)`,
cast-wrapped `#define CID(x) ((T*)(x))`) returns its n-th **argument's** type.
`cpp_reparse::classify_param_return` peels paren/cast wrappers to the bare
parameter (the cast type isn't recovered — "record what's cheap") and reports
its index, minted as `MacroReturnHint::Param(n)` in the driver's macro lane.
The lowering declares `ReturnExpr::Arg(n)` on the macro's `Symbol` (the
positional mirror of `ReturnExpr::Receiver` — `eval_return_expr` substitutes
`q.args[n]`, `None` at a bare probe), and each call site edges `Expr(call) →
Edge(Expr(arg_n))` so the call chases the argument's **own** value witness
(edges-not-values; `docs/adr/return-expr.md`). `ID(w)->m()` on `Widget *w`
resolves `m` on `Widget`. Config-variant macros whose first `#define`
delegates (`likely`/`unlikely` → `__builtin_expect` under `#if __GNUC__`)
stay in the delegation lane; projection macros returning a member
(`((n)->value)`) are not bare params → not claimed.

## Parked

- **Effects** — the second superposition dimension.

## Consequences

- Determinism precondition: the join-vs-chase winner must be a **principled,
  deterministic** rule, not witness/iteration order (owned by the flakiness
  fix). The function-like *return-typing* slice waits on that verdict.
- **Function-like implied return typing** rides the expansion-policy flip: a
  function-like macro whose use already parses as a clean `call_expression` is
  left unexpanded, and the existing sub-return bag path types the call for
  free (`SkeletonAnalysis::macro_returns`, landed). Parametric return
  (`Param(n)`) is gold-tested end-to-end.
