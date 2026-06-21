# Flow-sensitive narrowing — design

**Status: design, not built.** Active workstream (ROADMAP "Now" #1).
Playground / executable spec: `test_files/narrowing_playground.pl`.

The feature: a guard expression (`$x->isa('Foo')`, `ref($x) eq 'HASH'`,
`return unless blessed $x`) refines a variable's type for the region of
code that the guard dominates, then the type widens back at the region's
exit.

## The engine already exists — this is an emission feature

The query path already does flow-sensitive lookup. `ReducerRegistry`'s
`reduce` (`witnesses.rs:581`) on a `Variable` attachment with a `point`:

- **Narrowest-span-containing-point wins** (`witnesses.rs:596–613`): of
  the non-zero-extent `InferredType` witnesses whose span contains the
  query point, the smallest-area one is returned.
- **Temporal ordering** (`witnesses.rs:640–644`): a witness whose span
  *starts after* the point is skipped (a later reassignment can't poison
  an earlier read).
- **Scoped-witness exclusion** (`witnesses.rs:648–651`): a non-zero
  `InferredType` witness whose span does **not** contain the point is
  skipped entirely.

That third rule **is the un-narrowing**. There is nothing to remove or
widen at block exit — a narrowing witness scoped to the guarded region
simply stops containing the point past the region, so the outer (wide,
zero-extent) witness wins again. Monotone bag, no retraction. This is
why `prompt-flow-narrowing`'s old "un-narrowing at block exit against
monotone witnesses" worry dissolves: it was never a retraction problem,
it's a **span-computation** problem.

Proof the substrate works today: `witnesses_tests.rs::
narrowed_span_wins_over_outer_witness_at_inside_point` pushes a
`HashRef` zero-extent outer witness and an `ArrayRef` span-extent
witness over rows 4..8, then asserts `ArrayRef` inside and `HashRef`
before. That test is exactly one hand-built narrowing; this feature
makes the builder emit those witnesses from guard syntax.

### The one trap: do NOT route through `push_type_constraint`

`push_type_constraint` (`file_analysis.rs:3819`) **zero-extents** the
primary `InferredType` witness (`span.start..span.start`,
`file_analysis.rs:3828`) — by design, because an assignment's type holds
from that point forward via temporal ordering, not over a bounded span.
A narrowing witness is the opposite: it must carry the **full region
span** so narrowest-span-wins picks it inside and scoped-exclusion drops
it outside.

So narrowing gets a dedicated emission helper, not the TC path:

```rust
// builder.rs — pushes a SPAN-EXTENT InferredType witness on the
// variable's home scope. Span = the dominated region; the span does the
// lifetime slicing, the scope stays the variable's (mirrors the proven
// test, which keeps ScopeId(0) for both outer and narrowing witnesses).
fn emit_narrowing(&mut self, var: &str, scope: ScopeId,
                  ty: InferredType, region: Span)
```

Source tag `Builder("narrowing")`. Emitted during the **live walk**
(rule #1), so it lands before `finalize_post_walk` seals
`base_witness_count` and therefore survives enrichment truncation
(enrichment truncates to the base and re-derives only enrichment
witnesses). It is **not** re-emittable — it's a once-derived syntactic
fact, monotone — so it needs no clear-and-emit (worklist invariants).

## Guard catalog (v1 — native, dependency-free)

Each guard recognizes a `(variable, narrowed_type, polarity)` triple
from the condition CST. The variable must be a plain scalar (`Variable`
attachment); guards on hash elements / method returns are deferred (see
Open questions). Name→type mapping is pure `&str` and belongs in
`conventions.rs`; the CST recognition (which call shape) is builder-side
via `cst.rs` accessors.

| Guard syntax | narrows `$x` to |
| --- | --- |
| `$x->isa('Foo')`, `$x->isa(Foo::)` | `ClassName("Foo")` |
| `$x->DOES('Role')` | `ClassName("Role")` |
| `ref($x) eq 'Foo'` (Foo a class) | `ClassName("Foo")` |
| `ref($x) eq 'HASH'` | `HashRef` |
| `ref($x) eq 'ARRAY'` | `ArrayRef` |
| `ref($x) eq 'CODE'` | `CodeRef { return_edge: None }` |
| `ref($x) eq 'Regexp'` | `Regexp` |
| `blessed($x)` | *(object, class unknown)* — weak, see below |
| `defined($x)` | *(removes undef)* — weak, see below |

**`ref` reftype-token vs class** is a *closed grammar constant*, not an
open behavior set — the reftype strings are a fixed Perl language list
(`HASH ARRAY CODE SCALAR REF GLOB LVALUE FORMAT IO Regexp`). Classifying
`ref($x) eq S` by "is `S` in that constant set → rep variant, else →
`ClassName(S)`" is therefore *not* a rule-#10 shape-table (which is about
open sets of behaviors); it's reading a language constant. Put the
constant in `conventions.rs` with a comment saying so.

**`blessed` / `defined` are recognized-but-weak in v1.** Neither has a
refinement target in today's lattice — there is no "some object" type
and no undef/Optional. Recognize them (compute their variable + polarity
+ span) but emit nothing, OR emit only the negative-space removal once
the lattice can express it. They become load-bearing the moment Optional
lands (next section): `defined $x` / `blessed $x` then narrow
`Optional<T> → T`. Wiring the recognition now means the refinement is a
one-line target swap later, not new guard plumbing.

## Span + polarity — the actual design content

Two guard **positions**, two span rules:

**A. Block guard** — `if (G) { BODY }`, `unless`, `elsif`. Narrowing
span = the **BODY block span**. The narrowing asserts G over BODY.

**B. Early-exit guard** — a statement-level exit (`return` / `die` /
`croak` / `next` / `last` / `goto`) gated by a postfix/`or`/`and`
condition. Narrowing span = **[guard-statement-end .. enclosing-block
-end]** — the "rest of the block" after the guard. This is the assert
idiom and the user-confirmed shape: *scope the witness to the remainder
of the block.*

**Polarity** — does the span assert G-true or G-false?

| Form | dominated region | asserts |
| --- | --- | --- |
| `if (G) { T }` | T | **G true** ✅ |
| `if (G) { } else { E }` | E | G false ✘ |
| `unless (G) { T }` | T | G false ✘ |
| `return unless G;` | remainder | **G true** ✅ |
| `G or return;` | remainder | **G true** ✅ |
| `return if G;` | remainder | G false ✘ |
| `G and return;` | remainder | G false ✘ |

A guard-internal negation (`if (!G)`, `return unless !G`) flips the row.

**v1 ships only the G-true (✅) rows** — the guarded block and the
early-return assert, the two dominant idioms. They yield a *positive*
refinement (`$x` IS Foo), expressible in today's lattice.

The G-false (✘) rows need a **negation / union** the lattice lacks ("`$x`
is anything-but-Foo"). Deferred with the same dependency as the else
branch — they light up when Optional/union lands. Skipping them is
sound: emitting nothing leaves the wide outer type, which is correct if
imprecise.

**Compound conditions:** a top-level `&&`/`and` chain narrows the body by
the *intersection* — emit one narrowing per recognized conjunct
(`if ($x->isa('Foo') && $x->can('m'))` narrows on the `isa` conjunct,
ignores `can`). A top-level `||`/`or` chain narrows nothing (neither
disjunct is guaranteed). v1: walk top-level `&&`, skip `||`.

## Sum / optional types — the user's question

**Not scope creep — it's the complement of narrowing — but it is a
separate, sequenced lattice change, and I'd scope it to Optional/Maybe
first, not arbitrary unions.**

Why it pairs with narrowing: a function that early-returns undef
(`return undef unless $ok; return Foo->new`) produces `Foo | undef`.
Today `SymbolReturnArmFold` / `BranchArmFold` collapse arm *disagreement*
to `None` (the "1+ arms agree → Some, disagree → None" rule in
`witnesses.rs`). An `Optional(Box<InferredType>)` variant captures it
precisely instead of dropping it — and then `defined $r` / `blessed $r`
at the call site has something to bite on: `Optional<Foo> → Foo`. So the
two features are two halves of one story: **optionals create the
imprecision that narrowing resolves.** The `defined`/`blessed` guards in
the catalog above are wired *for* this.

Why it's separate and *after* narrowing v1:

- It's a **lattice-wide** change. New variant (appended at the enum END
  for bincode index stability, bump `EXTRACT_VERSION` — same discipline
  as `Sequence`/`TypeConstraintOf`/`BrandedRoute`). Every reducer fold
  that today does "agree → T, disagree → None" must instead **join** into
  `Optional`. `subsumes_narrowing` gains a meet. That's a type-system
  commit, not a narrowing rider — bundling them makes narrowing
  un-shippable until the join is right.
- Narrowing v1 stands alone against the existing lattice (ClassName /
  HashRef / ArrayRef / CodeRef / Regexp) and ships small. The weak
  guards no-op until Optional arrives, then flip their target in one
  line.

Why **Optional, not full unions**: arbitrary `Foo | Bar` is a bigger
lift (n-ary join, method dispatch over a union receiver) and is YAGNI
until a non-undef union has a motivating case. `Maybe<T>` (value-or
-undef) covers the dominant Perl idiom and maps 1:1 onto Type::Tiny's
`Maybe[T]` / `Optional[T]`, so it also feeds the `type_constraint_names()`
seam the Type::Tiny guards will use. It also *is* the join for the
already-queued "Conditional-reassignment disagreement-to-widen" item and
for the arm-disagreement `None` — all three are the same missing-join
gap.

**Recommendation:** land narrowing v1 against the current lattice; open a
sibling roadmap entry for `Optional<T>` (its own design doc) as the
immediate follow-on; park arbitrary sum types until a `Foo|Bar` case
demands them.

## Open questions

1. **Non-variable guard subjects** — `ref($self->{x}) eq 'Foo'`,
   `if ($obj->thing->isa(...))`. The narrowing subject isn't a
   `Variable` attachment; it needs `Expr(span)`-keyed narrowing (the
   query path narrows on `Variable` only — `witnesses.rs:590`) or it's
   the untyped-boundary problem (`open-problems.md`). v1: variables only.
2. **Loop-condition narrowing** — `while (my $x = shift) { ... }` (defined
   in body). Ties to Optional; defer with it.
3. **Reassignment inside the region** — sound for free: the reassignment
   pushes a later-starting witness; temporal + narrowest-span pick the
   live one at each point. Worth a playground row to pin.
4. **Negative / else / `return if`** — deferred to Optional/union (above).
5. **Postfix-on-block-exit precision** — does the early-exit span end at
   the lexical block, or follow `last`/`next` semantics into the loop?
   v1: lexical enclosing block (the conservative, common case).

## Test plan

- Substrate is proven: `narrowed_span_wins_over_outer_witness_at_inside
  _point` (keep as the engine regression).
- `test_files/narrowing_playground.pl` is the authoring source — every
  row carries a `# => Type` annotation at a point. Drive with
  `perl-lsp --dump-package` / the gold harness `--emit`.
- Per-row unit tests in `builder_tests.rs`, red-pinned `#[ignore]` until
  v1 lands (repo precedent: the cross-file ClassIsa probe), unignored as
  each row goes green. A row that should NOT narrow (the ✘ polarity
  rows, the `||` case) gets a negative-space test asserting the wide
  type still wins — these are the guardrails against over-narrowing.
