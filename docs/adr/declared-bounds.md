# ADR: Declared bounds

A declaration states what every value its binding holds is: `x: int`,
`Foo $p`, `T x;`, `private ?Logger $l`. That is not a value: it says
nothing about which value is there now, only what any value there must be.
So it is not a witness the reducers fold. It is a **bound** the registry
caps their answer by, the same way for every attachment and every
language.

## The shape

`WitnessPayload::Bound { ty, evidence, extent }` sits on the attachment it
bounds (a variable's `Variable` / `Place`, a member's declared `Place`),
with its span at the binding token (a local, a parameter) or over the
class body (a member, which a method reads from anywhere in it).

- `ty: BoundType` is `Type(T)` or `Edge(TypeName(n))`: a class-shaped
  declared type edges into the alias graph so a typedef chases, exactly as
  the value-flow edge for the same spelling would.
- `evidence: Evidence` is why the bound holds. Today it has one variant,
  `Declared(Enforcement)`, and `Enforcement` is `Advisory` (Python: nothing
  at run time), `Checked` (PHP's declared types, a C++ pointer or
  reference binding: a value that does not fit is rejected) or `Converted`
  (a C++ value binding: what is written becomes the declared type, so
  `Base b = derived` holds a `Base`).
- `extent: Extent` is how long it holds: `Lifetime` (past every write) or
  `Entry` (until the next write, which retires it like any value).

Consumers ask the bound, never the language or the source:
`Evidence::keeps_written_type()` (false for `Converted`),
`Extent::survives_write()`, `Enforcement::through_reference()`.

## The one rule

`ReducerRegistry::cap_by_bound`, one step in `query_rec_body` right after
the reducer loop:

1. The bound in force is the latest `Bound` stated at or before the query
   point. A `Lifetime` bound ignores the write markers; an `Entry` bound
   is retired by them (`LiveWindow`).
2. `v` is what the reducers fold, with the bound set aside (no reducer
   claims a `Bound`).
3. `v` absent or `Unknown` → the bound. `x: int = f()` reads `int`; `T x;
   x = g();` reads `T`.
4. A **refinement** stands as it is: a region narrower than the bound's
   own extent that holds the point and states `v` — a guard's proof
   (`isinstance`, `instanceof`, `dynamic_cast`, an optional's engagement
   test). A guard is a run-time check of the value, at least as strong as
   the declaration, and it may name a class whose relation to the bound
   this file cannot see.
5. A value made after a local's declaration (its initializer, a write)
   stands when it **fits** the bound and the bound keeps the written
   type. That is narrowing within the bound: `y: Base = Sub()`
   reads `Sub`, `Base* p = point()` reads the `Derived` it points at.
6. Anything else reads as the bound: a write that does not fit
   (`z: int = 1; z = 'x'` reads `int`), a converting binding
   (`Base v = make()` reads `Base`), and a member's writes and
   initializer, which are as wide as its declaration and so never
   narrower than it (`private ?Logger $l = null` reads `?Logger` whatever
   its writes put there).

`fits(v, B)` (`model/file_analysis/types.rs`, beside `join`) reuses the
lattice: `v.is_known() && join(v, B) == join(B, B)`, under the query's
`class_order` (local and cross-file ancestry). There is no second subtype
check. A class whose ancestry the order cannot see does not fit a bound
naming another class, so a write of it reads as the bound: the declared
type is what governs member dispatch (`RCPV *r = FOO(…)`, where the
upper-case-call constructor guess names no class at all).

One driver step rather than a clamp in each reducer that would need one:
the reducers stay ignorant of declarations, and a new attachment kind is
bounded by construction.

## Who states the evidence

The query document states what is a declaration and how long it holds,
because both are facts about the declaration (rule #15):

- `@type.annot` is a `Lifetime` bound; `@type.annot.entry` an `Entry` one.
  PHP marks its parameters, promoted parameters and `catch` variables
  `.entry`: PHP checks a parameter's type when the call binds it, and a
  reassignment of `int $x` to a string is legal. Python parameters stay
  `Lifetime`, as every checker reads them.
- A member declaration declares the slot, so a bound on a member is
  `Lifetime` whatever the capture says (the promoted parameter's property
  half; its local half keeps the parameter's `Entry`).
- The enforcement of a language's own annotations is one `LangPack`
  constant, `annot_enforcement` (C++ `Converted`, PHP `Checked`, Python
  `Advisory`), and the extractor mints it onto each bound, adjusted per
  declaration: a declarator that binds through a pointer or reference
  (`Payload::Deref`, the same stack hover renders the `*` from) is
  `through_reference()`, so `Base b` converts and `Base* p` does not.

**Why a pack constant minted onto the payload, not a convention read by
language id (rule #14).** The rule says a per-language constant belongs on
the language's conventions, not in every blob. The enforcement is not a
per-language constant at the point the cap reads it: it varies by
declaration (value vs reference binding), and the evidence will gain
kinds that have no language at all (a plugin's assertion, a whole-program
inferred bound over callers). What the pack declares is the default the
extractor starts from; what the cap reads is a fact about one declaration,
so it rides the declaration's own witness. A reducer that looked the
language up instead would be the per-language branch rule #10 forbids.

## What a declaration does, per language

| Language | Declaration | Bound | Extent | Enforcement |
|---|---|---|---|---|
| Python | `x: T = v`, `x: T` | on the local | Lifetime | Advisory |
| Python | `def f(a: T)` | on the parameter | Lifetime | Advisory |
| Python | class-body `a: T` | on the member's place | Lifetime | Advisory |
| PHP | `function f(T $p)`, promoted `T $p` (local half), `catch (E $e)` | on the local | Entry | Checked |
| PHP | `private T $p`, promoted property half | on the member's place | Lifetime | Checked |
| C++ | `T x;`, `T x = v;`, `T x[n]` | on the local | Lifetime | Converted |
| C++ | `T* p`, `T& r`, any depth | on the local | Lifetime | Checked |
| C++ | data member `T m;` / `T* m;`, member-block macro members | on the member's place | Lifetime | Converted / Checked |
| Perl | `my $x` | none: Perl declares no type | — | — |

A C++ plain reassignment (`x = rhs`) is anchored like every other pack's
write (`@flow.assign`): its marker no longer has to be withheld to keep the
declared type standing, because a `Lifetime` bound outlives it.

## What is not here

- **Contradictions.** A write that does not fit its bound reads as the
  bound; nothing says so yet. `bound_contradictions` and the
  `contradicts-bound` diagnostic are the next part of this design: a
  query-time verdict over the writes the producer already mints, with
  severity from the evidence.
- **Declared returns.** A function's declared return is still its
  `Symbol`'s value (`member_writeback`), and the return arms of a declared
  function are not minted at all
  (`SkeletonAnalysis::into_file_analysis`'s undeclared gate). Bounding the
  return needs the arms always minted, which is the same next part; until
  then the declaration wins by being the only fact, and narrowing a
  declared return to its arms would be unsound for an overridable method
  anyway.
- **Docblocks** (`@var`, `@param`) have no producer. When one lands it is
  a bound of its own enforcement (`Advisory`), not a priority.
- **Other evidence.** A plugin's type override still short-circuits
  through `PluginOverrideReducer` and the source-priority axis; its target
  form is a bound with its own evidence. A bound the whole-program engine
  infers must never cap (the cap is monotone in the value for a fixed
  bound, not in a bound the fixpoint moves), which is why `Evidence` is a
  closed enum: that kind arrives with its own rule, written in the
  exhaustive matches.
- **Undecided fits.** `TypeOrder::isa` answers a bool, so a class whose
  ancestry is unindexed or dynamic does not fit: the read is the bound,
  which is sound for dispatch. A guard is exempt (rule 4), so this only
  costs a write of an unrelated-looking class its own type.
- **A member declared in another file.** A write to a member whose
  declaration lives in another file (a C struct in its header) lands on
  the writing file's `Member` attachment, not the declared place, so the
  declaration's bound never caps it: a read of `l->head` joins the
  `Numeric` of `l->head = NULL` with the declared `cb *`.
  `TODO(member-bound-across-files)` at `settle_member_writes`.
- **A C string.** The C++ pack's `annot_type` sees the pointee's spelling,
  so `char *s` is bounded by `Numeric` and `s = "x"` reads `Numeric` —
  what an initialized `const char *s = "x"` already read when the
  annotation outranked the flow. `TODO(cpp-char-pointer-string)` in
  `build/packs/cpp.rs`.
- **A pointer behind a typedef.** Enforcement is read off the declarator,
  so `hisds s` (`typedef char *hisds`) is a value binding and converts.
  Through a class-pointer typedef that drops a fitting subclass to the
  declared class.
