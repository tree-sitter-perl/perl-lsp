# ADR: The CFG tier — joins on the bag, control in the document, effects on the callee

Status: accepted for 0.10.0, being built as one topic stack. The long-form
review, the alternatives and the sizing are in the sprint design
(`/mnt/project-files/design/cfg-sprint-design.md`); `docs/prompt-cfg-tier.md`
stays the background brief. This page is the contract the stack builds to.

## The two constraints (unchanged from the brief)

- **Spans are the only program-point currency.** No `BlockId` is ever a
  consumer key; an arm names a point, never a block.
- **The bag is monotone.** Nothing is killed: a write is a `Reset` marker at
  its site and a read takes the latest marker at or before it (`LiveWindow`).

## Joins are edges in the bag

A body's control is a tree of constructs plus statements that leave. Two
histories of a place meet only at: the end of a branching construct, the top
of a loop body, the end of a loop, and the start of a body that runs later.
At each such point, for each place whose value can differ between the
histories, the build mints a **join**:

- on the place: the existing `Reset` at the join point plus
  `Edge(Join{place, of, role})`;
- on the `Join` collector: one witness per arm that reaches the point, sited
  where the value leaves the arm (rule #14). An arm is `Edge(Place)` (the
  place as it stands there), or, for a bodiless arm, the value its condition
  proves. Each arm records its condition as `(Span, bool)` and may carry
  **gates**: statement-position calls control must return from to reach it.

`JoinFold` folds the surviving arms with `types::join`. Nothing is concluded
at build time; types reach arms through edges at query time, so a join sees
across files like every other edge. Joins are minted once, after
`resolve_variable_refs` (Perl) or after variable binding (packs), and are not
re-emitted by the fold. They are sparse: none is minted where no arm can
change the place.

Joins answer ordinary type queries (hover, completion, narrowing), not only
diagnostics. A diagnostic that reports on a narrowed type asks
`place_state_at`, which also says whether every path was accounted for, and
stays silent when it was not.

## Control structure is declared by each language's query document

One `@ctl.*` vocabulary (branch / cond / then / else, loop and its body,
step, else and once, try / body / handler / finally, and `leave.*` for
return, throw, break, continue and jump) replaces `@narrow.cond/then/else/exit`,
`@arm` and the branch and loop kinds of `@guard.region`. Perl declares its
own in `queries/perl/ctl.scm` on the combined walk, so Perl's exit keywords
stop being a Rust table. The narrowing atoms (`@narrow.is/set/unset/...`)
describe conditions and stay. The constructs are read once, by the join
builder, through a tree-free `ControlTree` view; nothing about them persists.
A body that runs later is already a callable scope (`ScopeKind::is_callable`).

## Effects: how a call ends

An effect is what running a callable does besides yielding a value. v1 has
one family, a set:

```rust
bitflags! { pub struct Ends: u8 { const COMPLETES = 1; const UNWINDS = 2; const EXITS = 4; } }
```

- It lives on its own key: `SymbolEffects(sid)` in the file and
  `Member{.., aspect: Effects}` across files, mirrored by the writeback that
  mirrors return types. `EffectFold` joins the arms that survive their gates.
- **Bottom and top are different values.** The empty set is the cycle
  assumption; the full set is "no evidence". An unknown callee, an open
  dispatch set, a depth cap and an unsettled fixpoint all answer the top, so
  they fall through.
- Producers mint it where they know it: syntax, a column in the builtin table
  (`die` unwinds, `exit` exits), the pack's runtime-function data document,
  declarations (`never`, `NoReturn`, `[[noreturn]]`), a plugin override field,
  and inference from a body whose end and returns are unreachable.
- A call's effect is the callee's effect, read through the call's existing
  `Invoke` on the `Effects` aspect, memoized per callee. Dispatch joins the
  candidates, including overrides in indexed subclasses.
- It rides the Surface (`MethodSurface.ends`), so a callee edit that changes
  how it ends dirties its consumers and nothing else.
- Later variants (`OnParam` writes, moves, kills) go on the same lane with
  their own join; set union is the join of `Ends`, not of the lane.

`InferredType::Never` is the value-level projection of a non-completing
effect: the registry answers it for an expression that states it leaves
and for a call whose callee cannot come back, and nothing stores it
(`layering_tests::never_is_made_only_by_the_registry`; the bag refuses it
in debug builds). Only an evaluation answers it: a callable whose every
arm leaves, or a slot written only by expressions that leave, answers
nothing. Being the identity of `join`, it drops a non-completing arm from
every value fold (`//`, ternaries, `or return`, return arms) without any
fold asking the effect lane. Until effects cross files, the short-circuit
fold keeps its spelled exit flag for callees another file decides
(`Carp::croak`).

## What it retires

`is_exit_expression`'s name list, PHP's name-matched `die`/`exit` calls,
`narrow_block_remainder`, `@narrow.exit`'s region rule, #434's single-block
`fall_through`, the `@ctl.loop` on PHP's `while`, the per-level reset markers' `Unknown`
in `fold.rs`, and every `TODO(cfg-exit-join)`, `TODO(noreturn-callee)` and
`TODO(cfg-loop-locals)`.

## Not in this tier yet

Real loop fixed points (v1's answer for a self-referential loop write, the
entry type flagged incomplete, awaits approval), later-running bodies and captures,
try/catch arms, readonly-after-constructor, effects through a wrapper in
another file (waits on the whole-program engine's `EffectsOf` key), and the
epic's original diagnostics (must/may use-after-move, the cpp D-codes, D9).
