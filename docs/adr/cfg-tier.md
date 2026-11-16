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

## Later-running bodies

A callable's body (`ControlView::bodies`: a sub, a def, a lambda, a php
closure) runs when it is called, not where it is written (`body_joins`). A
Perl named sub exists before its text runs (`CallableBody::hoisted`), so it
can run anywhere its variables are bound.

- *Reads.* A variable bound outside the body and read inside it can have
  been written between the two: at any write from the body's making on,
  inside the body or past it (any write at all, for a hoisted body). Where
  such a write exists, the body opens on an `Entry` join, landed as a region
  over the callable from the body's start, whose arms are the variable where
  the body was made and after each of those writes. A guard around the
  making is then one arm among several inside the body, and still holds
  outside it. Where none exists, nothing can change the variable before the
  body runs: nothing is minted, and the read sees it as made, narrowing
  included.
- *Writes.* A write inside the body stays a region fact there, and reaches
  the variable wherever the body is called: the variable gets one
  `MayBe(Join{.., Writes})` past the body's making (at its binding, for a
  hoisted body), whose arms are the variable after each of the body's
  writes. A `MayBe` is not windowed: every read past its site, outside the
  body, joins the value standing there with it, so a later write never
  retires it, and an untyped write leaves the read untyped. That is the
  sound reading without knowing which calls run the body
  (`TODO(closure-call-writes)`: the write reaches the variable only where
  the body is called, owned by the whole-program epic).

A closed body (php's) holds what it captures as it stood at its making and
writes none of it back, so it mints neither (`TODO(capture-by-reference)`
for `use (&$x)`, owned by the captures work).

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

A loop's recurrence shows up at query time as a cycle through a join, and
the registry chases it to a fixed point, local to the frame that found the
cycle (the dispatcher owns termination; no reducer and no fold driver
iterates). An arm whose chase closes at its own join and comes back untyped
is chased again with the join answering what its other arms hold, then with
what that pass joined, until a pass adds nothing (`rechase_recurring`): `$n
= $n->next` round three classes holds all three. One still untyped after a
pass (`$p = $p->{up}`) makes the join untyped, as any untyped arm does. A
read inside the loop asked first closes the cycle at its own frame instead:
the arm drops out of that pass, and the frame is asked again seeded with its
last answer until it stops moving (`pass_round_loop`). The lattice is finite
and a union widens past its arm cap, so the passes end; `MAX_LOOP_PASSES`
guards it. Passes nest two deep (a loop in a loop); a deeper one answers
nothing, which is sound. Completion reads the same
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
  nothing and the value coming in stands. A builtin is a gate only where
  the builtin table says it may not come back (`die`, `exit`); Perl's
  `croak` and `confess` are Carp's subs, read through the index like any
  other callee another file defines.
- **Inference.** A callable mints how it ends from its body: each leave
  in it, any `return` anywhere in it, and its end where control reaches
  it, gated by the calls it makes there. What sits in a body nested inside
  it is the nested callable's. A body the parser could not read states
  nothing. A pack document names the bodies (`@ctl.body`) and the calls
  that are whole statements (`@ctl.gate`), and every body it names mints;
  Perl mints the bodies its gates reach. An arm whose path runs into a call
  that does not come back ends the way that call does, so a wrapper round
  `die` unwinds; a catch between the call and the body's end is not seen
  yet (`TODO(effect-leaving-bits)`).
- **Cycles.** A callable already on the path is assumed to have no way of
  ending; since "comes back" is inductive, the frame that asked first gets
  the exact least fixed point. An answer worked out under that assumption
  is forgotten when the assuming frame closes, so the same question asked
  later in the query is asked again.
- It rides the Surface (`MethodSurface.ends`, what the file alone can tell:
  `None` where a path runs through a callee elsewhere), so a callee edit
  that changes how it ends dirties its consumers and nothing else.
- **Across files**, a gate on a function another file defines never chases
  that file's bag: the index keeps, per callable name, the modules whose
  local answer never comes back (`ModuleEdgeIndexes::ends`, fed with
  `names` and purged with the module), joined over every module declaring
  the name; any declarer without a row is the top. A name with no row
  passes unresolved, which is the pre-filter. Two approximations ride it:
  every declaring module is a candidate, not only those the caller sees
  (`TODO(effect-visibility)`), and a callee whose own end runs through a
  callee elsewhere is the top until the whole-program engine carries it
  (`TODO(effect-wrappers)`). A file's row for a name joins every
  callable of it the file defines, in any package.
- **Dispatch.** A method call gates too. Its receiver may be any class an
  arm names or any descendant of one, so each of those resolves the name
  up its MRO and all of them join: a class this file defines answers from
  its member, one another file defines from the index (`member_ends`: the
  files providing the class that declare the name, each by its row). A
  receiver arm naming no class, a class nothing holds, parents set at run
  time, or a name no class defines is the top. A name no callable known
  may fail to come back from passes without resolving the receiver.
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
fold asking the effect lane. A short-circuit whose fallback can only
leave (`$x // die`, `shift or croak`) peels to its left side the same way.

## What it retires

`is_exit_expression`'s name list, PHP's name-matched `die`/`exit` calls,
`narrow_block_remainder`, `@narrow.exit`'s region rule, #434's single-block
`fall_through`, the `@ctl.loop` on PHP's `while`, the per-level reset markers' `Unknown`
in `fold.rs`, and every `TODO(cfg-exit-join)`, `TODO(cross-file-noreturn)` and
`TODO(cfg-loop-locals)`. A pack's runtime functions that never return
(`sys.exit`, `std::abort`) wait on the pack runtime-function document
(`TODO(noreturn-callee)`); PHP's `die` and `exit` are reserved words, so
its document spells them.

## Not in this tier yet

A loop condition's
polarity (`TODO(cfg-loop-polarity)`: an `until` body runs where its test
fails), captures,
try/catch arms, readonly-after-constructor, effects through a wrapper in
another file (waits on the whole-program engine's `EffectsOf` key), and the
epic's original diagnostics (must/may use-after-move, the cpp D-codes, D9).
