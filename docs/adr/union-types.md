# ADR: Union types and the one join

`InferredType::Union(UnionArms)` is `A|B`: a value that is one of several
types no single type sits above. It exists because the folds that read a
value's arms (return arms, ternary and `||`/`//` arms, slot writes,
hash-key values) need a total join, and a fixpoint over cycles (the
whole-program engine) needs that join on a finite-height lattice.

## The join

`types.rs::join(a, b, order)` is the one least upper bound; every fold
reads it, through `join_all` / `join_return_arms`. It is total:

- equal → that type; `Unknown` absorbs (it is the lattice's top)
- an `Undef` or `Optional` arm makes the result `Optional<the rest>`
- otherwise each arm folds into an arm it joins precisely with
  (`join_precise`: a class into its ancestor, an object over a hash
  shape, keyed hashes merged, hash/array shapes coarsened,
  `Bool` into `Numeric`), and the rest stand side by side as a union

`None` is bottom (no evidence) and is never produced by a join of
two types; `join_all` answers `None` only for no arms.

`TypeOrder` is the only inheritance the join sees: `isa` for
subsumption, `common_class` for widening. In-file folds without a bag
context use `Unordered` (a class is an instance only of itself); a
`BagContext` answers through the local + cross-file isa seam
(`class_isa`, `least_common_class`).

## Per-language subsumption

The join's core (equality, the union and its canonical form, undef,
`Unknown`, the cap, structural shape merges) is the same in every language.
What differs is which family of types one language's values let absorb
another, and that is a closed set, `Subsumption`:

- `ObjectAbsorbsHash` — an object over a hash shape is the object. Perl
  (a blessed hashref). Not PHP, where an array and an object are distinct.
- `BoolIntoNumeric` — `Bool` folds into `Numeric`. Perl (`1`/`''`), Python
  (`bool <: int`), C++ (promotion), R (logical arithmetic).

A language declares its set as data: Perl in `conventions::PERL_LATTICE`,
beside its other conventions, and a pack on `LangPack::lattice`. The model
cannot see the packs, so the build layer registers each pack language's
set by id (`conventions::register_lattice`, from the driver registry and
from each pack analysis); Perl's is declared, never registered. Every
reader, the Perl builder's fold included, asks `lattice_for(language)`,
and a `BagContext` reads the set of the analysis it queries
(`lattice_for(&fa.language)`). Nothing is stored per file beyond the
language id the analysis already carries, and a cross-file recursion into
a cached module's bag joins under that module's language. A language
nobody registered holds no rules (and debug builds assert): it never
borrows another language's.
`TypeOrder::holds` is how the join asks; it never asks which language it
serves. A context-free join (`Unordered`) holds no
rule. A new rule is a new variant, and every language's declaration is
where it is decided.

## Canonical form

`UnionArms`' field is private; `join` is the only constructor. Its arms:

- are two or more, sorted by the derived `Ord`, deduped;
- hold no union, `Optional`, `Undef` or `Unknown` (`A|B|undef` is spelled
  `Optional(Union[A, B])`, so `optional-types.md` holds unchanged);
- never hold a class beside one of its ancestors;
- are at most `UNION_WIDTH_CAP` (4).

The answer depends only on the SET of arms, not the order they were
joined in, so a fold over callers in path order and one over arms in
source order agree, and two spellings of one union serialize to the same
bytes (the engine hashes bincode bytes; there is no `HashSet` in the
type). Keyed-hash merges sort their keys for the same reason.

## Widening

Past the cap, the class arms collapse to their one least common class
(`TypeOrder::common_class`); if there is none, or several (multiple
inheritance — the answer must not depend on which parent a walk reached
first), or the union is still too wide, the join is `Unknown`. Because
the class order is fixed while the arms grow, widening early and adding
arms later lands where adding first and widening later does, save for
the ambiguous-common-class case.

Two related classes stay `A|B` below the cap rather than collapsing to
their parent: goto-def reaches both overrides, and the collapse would
cost a cross-file ancestry read on every join.

## Consumers

A union is not a class: `class_name()` is `None`, as for `Optional`, so
no single-class dispatch lane reads one. Rendering spells `A|B`.

A consumer of a receiver's VALUE reads each value the receiver may be —
`InferredType::receiver_arms`: the arms of its defined part, so an
`Optional<A|B>` distributes exactly like `A|B`, and `Optional<Foo>` reads
`Foo` (the dispatch lanes' long-standing leniency; a deref diagnostic owns
"might be undef") — and joins what the arms answer
(`InferredType::join_over_arms`). No consumer matches the `Union` variant
to decide whether to distribute. A call on a union receiver
dispatches on every arm with that arm as the receiver its method sees
(the `Invoke`), a drill (`->{k}`, `->[i]`, element, key, slot) projects
every arm, and so do the query-time member reads (`member_value_type`,
`field_value_type`). An arm that answers nothing adds no evidence, the
same as any other `None`: `$pet->purr` on `Cat|Bird` is Cat's `purr`, the
Bird there a runtime error rather than a type.

Every per-arm query runs in an opaque frame, so a conclusion bake that
crosses files under a union poisons to a decode instead of minting a
first-answer `Link` for what is a join.

The build-time chain walker (`invocant_type_at_node`) does not distribute
yet (`TODO(union-chain-walker)`); the registry's projections above are
the query-time answer.

Python annotations mint unions: `A | B`, `Union[A, B]`, `Optional[A]`
(`A | None`). PHP `A|B` docblocks land with the PHP pack.

Latest-wins reducers (`ExprReturn`, `SubReturnReducer`) are not joins:
two witnesses on one attachment keep the latest, union or not.
