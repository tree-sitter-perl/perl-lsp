# ADR: Semantic handling of C/C++ macros

Status: accepted (incremental). Measured baseline: `docs/macro-semantic-map.md`.

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
| syntactic / statement / `##` | `do {…} while(0)` | **expand** (parse repair only) |

### The load-bearing move: expansion policy flip

Flip the gather gate from **"expand if harmless"** (`parse_damage(after) <=
before`) to **"expand only if it strictly *reduces* `parse_damage`"** — i.e.
the gather's job narrows to **parse repair**. Everything that parses fine
unexpanded (type / expression / function-like macros) stays in the tree and
becomes a first-class symbol. A function-like macro left unexpanded already
parses as a `call_expression`, so its call sites resolve through the **existing
sub-return bag path** with no new reducer — a function-like macro *is* a
package-global sub whose return type we infer from its body. Consequence: the
splice count collapses to genuinely-broken sites only, and most `SpliceMap`
span-remap bugs evaporate structurally. The existing `alias_only` fallback is
the baby step of this instinct; this generalizes it.

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

## Parked (we'll get there — correctness on a solid base, deepen as needed)

- **Parametric return** (`#define ID(x) (x)` → the arg's type) — arity/union tier.
- **Effects** — the second superposition dimension.

## Consequences

- Determinism precondition: the join-vs-chase winner must be a **principled,
  deterministic** rule, not witness/iteration order (owned by the flakiness
  fix). The function-like *return-typing* slice waits on that verdict.
- Slice order (independent slices first; the coupled pair last):
  1. **goto-def overhaul** — **LANDED** (959b388).
  2. **provenance-leaf hover display** — recover the config-active variant's
     concrete leaf (`unsigned short`) for hover while typing stays the
     abstraction (`Numeric`). Independent, self-contained; owns the deferred
     `op_type` xfail row (promotes it on landing).
  3. **resolution visibility = include-closure scope** — the lie above; reuses
     slice 1's reachability machinery. Correctness for vendored/monorepo name
     collisions. Independent of the expansion policy.
  4. **function-like implied return typing + expansion policy flip** — COUPLED:
     the "unexpanded macro parses as a `call_expression` → sub-return bag path"
     mechanism only exists once expansion is parse-repair-only. So the flip and
     the typing land together (biggest blast radius, last, after the
     splice/gather work stabilizes).
