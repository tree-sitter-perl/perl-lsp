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

`ClassOrder` is the only inheritance the join sees: `isa` for
subsumption, `common_class` for widening. In-file folds without a bag
context use `Unordered` (a class is an instance only of itself); a
`BagContext` answers through the local + cross-file isa seam
(`class_isa`, `least_common_class`).

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
(`ClassOrder::common_class`); if there is none, or several (multiple
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
every dispatch lane is unchanged by this ADR. Rendering spells `A|B`.
Dispatch over the arms (goto-def / references across every arm,
completion over the arms' shared members), PHP `A|B` docblocks, and the
`--dump-package` widening trace are the next layer.

Latest-wins reducers (`ExprReturn`, `SubReturnReducer`) are not joins:
two witnesses on one attachment keep the latest, union or not.
