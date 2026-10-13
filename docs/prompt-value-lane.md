# The value lane — constant folds as values on the witness bag

> Status: design brief, not ratified. Replaces the builder's fold table
> (`build/builder/consts.rs`). Rides the CFG tier
> (`docs/prompt-cfg-tier.md`, scheduled as `docs/epics/16-cfg-tier.md`):
> `Place` gives the identity, `Join` + `JoinFold` give the joins.

## The problem

A walk-time constant fold answers "which strings can this name hold
here" for:
- dynamic method names (`$self->$m`), and invocant classes
  (`$c->go`);
- loop and topic expansion, and interpolated strings;
- plugin arguments (the `str` projection, `docs/PLUGIN_AUTHORING.md`);
- export lists, `use constant` names and `@ISA`.

It feeds `Ref.folded_from` for rename (rule #9).

Today that answer comes from `ConstTable`, a side table beside the bag:
- Its key is the binding (`Lexical(SymbolId)` or `Package(pkg, name)`),
  so scoping is right.
- It has none of the bag's other machinery, so it gets everything else
  wrong or not at all:

| Shape | Today | Perl |
|---|---|---|
| `my $m = 'a'; if ($c) { $m = 'b' } $self->$m` | `b` | `{a, b}` |
| `for (...) { $self->$m; $m = 'x' }` (back edge) | first iteration only | `{prev, x}` |
| `my $a = 'p'; my $m = $a; $self->$m` (scalar copy) | nothing | `p` |
| `$h{k} = 'p'; $self->${\ $h{k}}` (element place) | nothing | `p` |
| `push @l, 'p'; $self->$_ for @l` | `@l`'s earlier value | earlier ∪ `p` |

The side table also means every syntax handler that writes a variable
must remember to call `write_constant`. The bag already records every
write as a flow edge, so that bookkeeping exists twice.

## The design

Values ride the same edges types do. The fold answer is a finite set of
literals, or `Unknown`.

1. **The producer mints the value.**
   - A string literal's `Expr(span)` already gets `InferredType(String)`
     from `emit_expr_witness`. It also gets the literal, as a payload or
     a marker (see Open decisions); a qw/list literal carries a set.
   - `use constant M => 'x'` already registers `M` as a sub
     (`register_constant_symbol`). It gains `Symbol(M) → Edge(Expr(value))`,
     so a bareword `M` resolves through `PackageSymbol{package, M}`. That
     scopes constants by package with no extra key.
2. **Writes are already edges.** Each assignment pushes
   `Variable → Edge(Expr(rhs))` plus a reset at the write. A scalar copy
   (`$m = $a`) is then just an edge chase; nothing special-cases it.
   - The gap is loop variables. `for my $x (LIST)` needs an element
     edge (`Extraction::Element`) to `Expr(LIST)`. That edge replaces
     the `$_` save/restore in `visit_postfix_for`.
   - `push`/`unshift` on an array is a write with an element edge: it
     unions into the array's value.
3. **Places.** The lane keys on the CFG tier's `Place`, a binding plus
   `Vec<PathStep>`. So `$h{k} = 'p'` is a write to the place
   `(%h, [Field(k)])`, and a read of `$h{k}` queries that place.
   - Kill rules are the place kill vocabulary (§3.3): a write to the
     place, to any base of its path, or to a key variable.
   - **Escapes are the hard part:** passing `\%h` or `%h` to a call, or
     writing through a computed key, invalidates every element below it.
     Until escape facts exist, any escape makes the place `Unknown`. The
     nested-key path ADR (key paths, "any element", writes at depth) is
     the same problem and should share the `PathStep` vocabulary.
4. **One query, a set answer.** A value query chases the same edges as
   the type query:
   - a literal contributes itself;
   - a non-literal source poisons the answer to `Unknown`;
   - `Join` arms union, and a cycle-cut arm (§3.6) is `Unknown`.

   Finite literal sets make the union safe even while the type
   lattice's union (`docs/open-forks.md`, "Union types in the
   lattice", option B) stays open. Walk-time callers query the
   registry mid-walk, as `chain.rs` already does.
5. **Consumers ask about the expression, not a name.** Each of the
   ~20 call sites of `resolve_constant_strings` /
   `constant_string_source` holds the CST node, so it queries
   `Expr(node span)`.
   - Nothing keys by spelling, and `ConstTable` and every
     `write_constant` call go away.
   - Plugins keep the `str` projection; it reads the same query.
6. **The answer lives on the ref.** A ref minted from a fold carries
   `folded_from: Vec<Span>`, one span per literal the chase reached.
   - For now there is still one ref per member.
   - Collapsing to one ref indexed under each member (the alias-key
     pattern) waits on the rename UX for sets.

Folding stays a build-time fact: `RefTable::by_key` and the relational
rows need names at index time. Folded refs are re-minted in a
clear-and-emit pass before `FileAnalysis::new`, so values that converge
only in the fold (loop back edges, branch joins) reach the refs.

## Sequencing

- **CFG tier Phase B (`Place`)** is the prerequisite for the lane on
  places. A first cut can ship on bindings alone, before `PathStep`s
  exist, and that already covers scalar copies and `use constant`.
- **CFG tier Phase C (`Join` + `JoinFold`)** is the prerequisite for
  branch- and loop-correct folds. Before it, the lane answers in source
  order, like the table.
- **Prerequisite bug:** `query_variable_with_visited`
  (`model/witnesses/registry.rs`) falls through to an outer scope when
  the inner declaration's answer is `None`.
  - So `my $c = 'B'; { my $c = $x->pick; $c }` types the inner `$c` as
    `String`.
  - The lane would inherit that: an inner declaration must shadow
    whatever its answer.
  - The fix is to stop at the first scope that binds the name at or
    before the point. It is a separate small change.
- **`docs/prompt-type-inference-residual.md` Part 3**
  (`DynamicDispatchLoop`) reads loop lists through
  `resolve_constant_strings`, so it should read the lane instead.

## Open decisions

- **Literal payload vs marker.**
  - A literal payload on every string `Expr` grows the bag, and the bag
    is persisted in the blob (`EXTRACT_VERSION`).
  - Folding is build-time only, so the payload could instead be a
    text-free marker, with the reducer reading the literal from the
    source span through `BagContext` during the build. That trades rule
    #11's "mint the fact" for zero blob growth.
  - Count literal witnesses against total witnesses on the corpora
    before choosing.
- **Rename UX for set-valued folds.** Deferred. Until it is settled a
  set mints one ref per member, each with its own `folded_from`.
- **Set size cap.** A loop over a large literal list mints one ref per
  member. Pick a cap past which the answer is `Unknown`, and measure the
  corpora for it.

## Non-goals

- Cross-file folds (a value assigned in another file).
- Numeric or arithmetic folding. The lane folds strings.
- Escape analysis beyond "an escape poisons the place". Precise effects
  are the CFG tier's interprocedural arc (§5).

## Tests and gates

- The `const_fold_tests` builder suite carries over unchanged. Add one
  test per row of the table above.
- `cargo test --features cpp`, `gold-corpus/run.pl` (`lang-skip 0`) and
  `./e2e/run.sh`.
- A corpus `--heatmap` A/B: fold changes show up as fan-in / fan-out
  moves on constants and dynamically called methods. Every move gets a
  traced explanation.
