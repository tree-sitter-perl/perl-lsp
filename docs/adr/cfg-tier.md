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

An arm control never comes out of is no arm of the join: one holding a
leaving statement with no arm between them, or a branch with both ways
covered whose every arm leaves (`JoinPlan`, from the control view's
leaves). A test changes nothing its subject holds, so a join a test
makes, rather than a write, lands as a region from past the construct to
the end of the scope around it instead of a `Reset`: where it answers
nothing, the value coming in stands. It is minted only where some path is
gone and every path out carries a proof, and each branch around it takes
one on the same terms, so a narrowing leaves a block one construct at a
time. A place with steps has no writes to order a `Reset` against, so its
joins are regions too, cut at its first disturbance.

## Loops

A loop holding a write mints three joins for the place. Its `Head` lands
where each test runs (the condition, or the body when there is none or it
follows the body), fed by the value entering the loop and by each pass's
end: past the step when there is one, else the body's end and each
`continue`. A step's entry (`ArmStart`) is fed by the body's end and each
`continue`. Its `Exit` lands just past the loop, fed by where the test fails
(read just past the test, so a test that writes the place feeds what it
wrote; past the loop's `else`, which runs there) and by each `break`. An arm
whose end a branch or loop closes takes that construct's paths out.

A `foreach` tests for a next item as it binds its variable, so the binding
is its condition and its head lands there; the binding replaces what the
variable held (`@flow.assign` on the target), so nothing a pass wrote
reaches the next. A variable bound at or past the head and ahead of the
body (`while (my $x = ...)`, `for my $x`) is bound afresh each pass, and
the loop joins none for it (`JoinPlan::binds_each_pass`). A use where the
head lands (the test's first read) reads the joined value: an untyped join,
like an untyped write, yields to a use at or after it.

A `break` or `continue` leaves the innermost construct it can leave, the one
carrying its name (`last OUTER`), or the n-th (`break 2`). A `switch` takes
a `break` (`@ctl.block`), and Perl's bare block and php's `switch` take a
`continue` too (`@ctl.block.next`); a jump that leaves one of those, or names
nothing around it, is modelled as falling through, since no join is minted
past them. A narrowing never joins out past a loop: what the place holds
there is the loop's exit, which no test inside decides.

A loop's recurrence shows up at query time as a cycle through a join. An
arm whose chase closes at its own join and comes back untyped is chased once
more with the join answering what its other arms hold: one pass round the
loop (`rechase_recurring`). Typed then, it says what a pass adds (`$n =
$n->parent` keeps the class); still untyped (`$p = $p->{up}`), it makes the
join untyped, as any untyped arm does. A pass met inside a pass is not
taken (the arm stays untyped), so the work stays one pass deep. An arm
whose chase closes above its join (a read inside the loop asked first)
comes back untyped and drops out: it adds nothing the join's other arms do
not. Both stop short of a fixed point (`TODO(cfg-loop-fixpoint)`: a value a
later pass changes is missed, and only the fixed point can tell a consumer
the answer is complete). Completion reads the same
joins: a local first written further down a loop's body is offered at its
top, where a later pass carries it (`FileAnalysis::in_completion_view`).

Joins answer ordinary type queries (hover, completion, narrowing), not only
diagnostics. A diagnostic that reports on a narrowed type asks
`place_state_at`, which also says whether every path was accounted for, and
stays silent when it was not.

## Control structure is declared by each language's query document

One `@ctl.*` vocabulary (branch / cond / then / else, loop and its body,
step, else and once, try / body / handler / finally, and `leave.*` for
return, throw, break, continue and jump) replaces `@ctl.cond/then/else/exit`,
`@arm` and the branch and loop kinds of `@guard.region`. Perl declares its
own in `queries/perl/ctl.scm` on the combined walk, so Perl's exit keywords
stop being a Rust table. The narrowing atoms (`@narrow.is/set/unset/...`)
describe conditions and stay. The constructs are read once, by the join
builder, through a tree-free `ControlView`; nothing about them persists.
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
- **Gates.** A statement calling a callable of the file is a gate on every
  path past it (`ControlView::gates`): a join arm, or an arm of how the
  caller ends, is `Gated` by the calls on its path, and the registry drops
  it where one of them does not come back. In a test's region a path past
  a gate that comes back speaks for nothing, so there the region answers
  nothing and the value coming in stands. A call to nothing the file
  defines is no gate: it comes back, except a name the exit list still
  spells (`TODO(cross-file-noreturn)`).
- **Inference.** A callable mints how it ends from its body: each leave
  in it, any `return` anywhere in it, and its end where control reaches
  it, gated by the calls it makes there. What sits in a body nested inside
  it is the nested callable's. A body the parser could not read states
  nothing. A pack document names the bodies (`@ctl.body`) and the calls
  that are whole statements (`@ctl.gate`), and every body it names mints;
  Perl mints the bodies its gates reach. How it leaves through a callee is not derived:
  a body whose every way out runs into a call that does not come back has
  no way of ending it names (`TODO(effect-leaving-bits)`).
- **Cycles.** A callable already on the path is assumed to have no way of
  ending; since "comes back" is inductive, the frame that asked first gets
  the exact least fixed point. An answer worked out under that assumption
  is forgotten when the assuming frame closes, so the same question asked
  later in the query is asked again.
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

Real loop fixed points (`TODO(cfg-loop-fixpoint)`), a loop condition's
polarity (`TODO(cfg-loop-polarity)`: an `until` body runs where its test
fails), later-running bodies and captures,
try/catch arms, readonly-after-constructor, effects through a wrapper in
another file (waits on the whole-program engine's `EffectsOf` key), and the
epic's original diagnostics (must/may use-after-move, the cpp D-codes, D9).
