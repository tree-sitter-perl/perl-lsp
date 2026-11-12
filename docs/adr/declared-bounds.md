# ADR: Declared bounds

A declaration states what every value its binding holds is: `x: int`,
`Foo $p`, `T x;`, `private ?Logger $l`. That is not a value: it says
nothing about which value is there now, only what any value there must be.
So it is not a witness the reducers fold. It is a **bound** the registry
caps their answer by, the same way for every attachment and every
language.

## The shape

`WitnessPayload::Bound { ty, evidence, extent }` sits on the attachment it
bounds (a variable's `Variable` / `Place`, a member's declared `Place`).

- `ty: BoundType` is `Type(T)` or `Edge(TypeName(n))`: a class-shaped
  declared type edges into the alias graph so a typedef chases, exactly as
  the value-flow edge for the same spelling would (`BoundType::declared`,
  the one spelling of "declared type → bound").
- `evidence: Evidence` is why the bound holds. Today it has one variant,
  `Declared(Enforcement)`, and `Enforcement` is `Advisory` (Python: nothing
  at run time), `Checked` (PHP's declared types, a C++ pointer or
  reference binding: a value that does not fit is rejected) or `Converted`
  (a C++ value binding: what is written becomes the declared type, so
  `Base b = derived` holds a `Base`).
- `extent: Extent` is what it holds over: `Lifetime` (a local, past every
  write), `Entry` (a local until the next write, which retires it like any
  value) or `Slot` (a member, class-wide). A slot's bound is in force
  wherever the slot is asked, so its span decides nothing; a local's sits
  at its binding token.

A guard's region (`isinstance`, `instanceof`, `dynamic_cast`, an optional's
engagement test) carries a sibling `WitnessPayload::Proof` at the same
attachment and span, minted by the guard's producer and cut with the region
at the subject's first rebind. That is what makes a region a proof; the
shape of its span says nothing.

Consumers ask the bound and the proof, never the language or the source:
`Evidence::read_written` / `read_proven` (the rule below),
`Enforcement::of_binding` (a binding through a pointer or reference
converts nothing).

## The one rule

`ReducerRegistry::cap_by_bound`, one step in `query_rec_body` right after
the reducer loop. The reducers fold the value as if no bound were there
(the type fold reads a `Bound` only as "something was bound before").

1. The bound in force is the latest `Bound` stated at or before the query
   point. A `Slot` bound holds wherever it is asked; a `Lifetime` one from
   its statement on; an `Entry` one until the latest write marker the read
   reaches (`WitnessBag::latest_reset`: a binary search over the
   attachment's `SpanOrder` write markers, or a scan of an attachment too
   small to have one, so a read costs O(log writes)).
2. The value it caps:
   - a guard's proof holding the point → the fold's value, read by
     `read_proven`;
   - a slot → nothing: a member's writes and initializer are arms of one
     class-wide value no single write decides, so the slot reads as its
     declaration (`private ?Logger $l = null` reads `?Logger` whatever its
     writes put there);
   - a local → what the facts from its declaration on fold to (its
     initializer and the writes after it), read by `read_written`. A fact
     made before the declaration is another binding's.
3. No value, or an `Unknown` one, reads as the bound: `x: int = f()` reads
   `int`; `T x; x = g();` reads `T`.
4. A known value reads by the evidence and by how it fits:

| | written value | guard's proof |
|---|---|---|
| `Advisory` | the value | the proof |
| `Checked`, fits or undecided | the value | the proof |
| `Checked`, decidedly does not fit | the bound | the join of both |
| `Converted` | the bound | the proof if it fits or is undecided, else the bound |

So `y: Base = Sub()` reads `Sub`, `z: int = 1; z = 'x'` reads `str`
(nothing holds a Python value to its annotation), `def f(path: str):
path = Path(path)` reads `Path`, `Base* p = point()` reads the `Derived`
it points at, and `Base v = make()` reads `Base`.

**Fit is three-valued** (`Fit`, `fit(v, B)` in `model/file_analysis/types.rs`
beside `join`). `Yes` when `join(v, B) == join(B, B)` under the query's
`class_order` (local and cross-file ancestry): there is no second subtype
check. Otherwise `Undecided` when both are classes and the value's
ancestry runs past what the order sees (`TypeOrder::ancestry_closed`: every
class on its isa walk is declared in the file or has an index candidate),
since an edge the order does not hold could put it under the bound; and
`No` when the order sees all of it. An undecided value is read as a fit,
written or proven alike: a checked binding rejected anything that did not
fit, so a value there that the order cannot place did fit, and the gap is
the order's. A guard that decidedly does not fit a checked bound proves a
value that is both classes at once (`function f(Countable $c) { if ($c
instanceof Logger) … }`). The lattice has unions and no intersections, so
the read is their join, which for two unrelated classes is the union, and
dispatch reads a union arm by arm: `count` and `info` both resolve.

One driver step rather than a clamp in each reducer that would need one:
the reducers stay ignorant of declarations, and a new attachment kind is
bounded by construction.

## Monotonicity

The cap is monotone in the value only over known values. It maps a value
that is not yet known (`None`, `Unknown`) to the bound and a later, known
one to itself, so while a fixpoint is still finding values the capped
answer can descend from the bound to the value. The fold's worklist only
looks for motion, so it tolerates that. A pass that keeps every round's
facts does not: `settle_member_writes` files a member write under the
class its receiver reads as, and keeps every class a round gave it.
`p: Base = self.s; p.k = 1` would file `k` under `Base` in the round that
had not yet found `self.s`'s value, and under `Sub` in the next.

So such a pass reads with the bound set aside until its rounds settle
(`with_bounds_unfilled`): the cap leaves an absent or `Unknown` value as it
is, and the fold memo is closed meanwhile so no answer made either way is
reused the other. Once no round finds anything, the pass runs again with
the bound filling in, which files only the writes whose receiver nothing
but the declaration types. `declare_slots` runs after the member writes
settle, so a slot's owner is read off settled receivers too.

## Who states it

The query document states what is a declaration and how long it holds,
because both are facts about the declaration (rule #15):

- `@type.annot` is a `Lifetime` bound; `@type.annot.entry` an `Entry` one.
  PHP marks its parameters, promoted parameters and `catch` variables
  `.entry`: PHP checks a parameter's type when the call binds it, and a
  reassignment of `int $x` to a string is legal. Python parameters stay
  `Lifetime`: a checker holds an annotated parameter to its annotation for
  its whole life, and since the bound is `Advisory` a known rebinding reads
  as itself anyway, so the extent only decides what an untyped rebinding
  reads (`str`, not nothing).
- A member declaration declares the slot, so the extractor mints a bound
  on a member as `Slot` whatever the capture says (the promoted parameter's
  property half; its local half keeps the parameter's `Entry`). A
  member-block macro's members (`inject_member_blocks`) are slots the same
  way.
- A guard's producer mints its `Proof` (the pack narrowing pass, for the
  variable regions and the place narrowings alike).
- The enforcement of a language's own annotations is one `LangPack`
  constant, `annot_enforcement` (C++ `Converted`, PHP `Checked`, Python
  `Advisory`), and the extractor mints it onto each bound, adjusted per
  declaration: a declarator that binds through a pointer or reference
  (`Payload::Deref`, the same stack hover renders the `*` from) is
  `of_binding(true)`, so `Base b` converts and `Base* p` does not.

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
| Python | class-body `a: T` | on the member's place | Slot | Advisory |
| PHP | `function f(T $p)`, promoted `T $p` (local half), `catch (E $e)` | on the local | Entry | Checked |
| PHP | `private T $p`, promoted property half | on the member's place | Slot | Checked |
| C++ | `T x;`, `T x = v;`, `T x[n]` | on the local | Lifetime | Converted |
| C++ | `T* p`, `T& r`, any depth | on the local | Lifetime | Checked |
| C++ | data member `T m;` / `T* m;`, member-block macro members | on the member's place | Slot | Converted / Checked |
| Perl | `my $x` | none: Perl declares no type | — | — |

A C++ plain reassignment (`x = rhs`) is anchored like every other pack's
write (`@flow.assign`): a `Lifetime` bound outlives its marker.

## What a parameter takes

Signature help, an argument slot's expected type and `param_inferred_type`
ask what a parameter is declared to take, not what the body leaves in it:
`FileAnalysis::param_declared_type` reads the bound on the declaration at
the parameter's binding site (`ParamInfo::binding_site`), chasing its alias
edge, and they fall back to the body-end read only where no declaration
bounds the parameter (every Perl parameter). `function load(string $p) {
$p = realpath($p); }` takes `string`. `TODO(pack-params)`: no pack mints
`ParamInfo` yet, so these consumers reach no pack parameter today.

## What is not here

- **Contradictions.** A write that does not fit its bound reads by the
  rule above; nothing says so yet. `bound_contradictions` and the
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
  infers must never cap (a bound the fixpoint moves breaks even the
  monotonicity over known values), which is why `Evidence` is a closed
  enum: that kind arrives with its own rule, written in the exhaustive
  matches.
- **Perl's guards carry no proof.** Perl declares no type, so no Perl read
  is capped and its narrowing regions mint no `Proof`. The first Perl bound
  (a plugin override or a docblock as a bound) needs the builder's
  narrowing to mint them first, or a guard there reads by the write rule.
  `TODO(perl-guard-proofs)` in `build/builder/narrowing.rs`.
- **Definedness under a decided non-fit.** A `Checked` write that
  decidedly does not fit reads as the bound, `Optional` included, though
  the value is certainly defined.
- **A member declared in another file.** A write to a member whose
  declaration lives in another file (a C struct in its header) lands on
  the writing file's `Member` attachment, not the declared place, so the
  declaration's bound never caps it: a read of `l->head` joins the
  `Numeric` of `l->head = NULL` with the declared `cb *`.
  `TODO(member-bound-across-files)` at `settle_member_writes`.
- **A C string.** The C++ pack's `annot_type` sees the pointee's spelling,
  so `char *s` is bounded by `Numeric` and `s = "x"` reads `Numeric`, as
  an initialized `const char *s = "x"` does. `TODO(cpp-char-pointer-string)`
  in `build/packs/cpp.rs`.
- **A pointer behind a typedef.** Enforcement is read off the declarator,
  so `hisds s` (`typedef char *hisds`) is a value binding and converts.
  Through a class-pointer typedef that drops a fitting subclass to the
  declared class.
