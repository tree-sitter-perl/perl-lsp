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
- *Writes.* A write inside the body stays a region fact there (from its
  site to the body's end; one the body ends on holds nowhere in it,
  `confined_writes`), and reaches the variable wherever the body is called:
  the variable gets one `MayBe(Join{.., Writes})` past the body's making (at
  its binding, for a hoisted body), whose arms are the value each of the
  body's writes stores (the variable after the write, for one that stores
  no whole value). A `MayBe` is not windowed: every read past its site, outside the
  body, joins the value standing there with it, so a later write never
  retires it, and an untyped write leaves the read untyped. That is the
  sound reading without knowing which calls run the body
  (`TODO(closure-call-writes)`: the write reaches the variable only where
  the body is called, owned by the whole-program epic).

**Captures.** A body says which names it takes from outside it
(`Scope::captures`: a `Capture { name, from, by }`, stated by the
document's `@closure.capture[.ref|.global]`). By value (php's `use ($x)`)
the body holds the variable as it stood at its making: a closed body ends
the walk past it for every other name, writes none of it back, and mints
neither join; a write to the copy binds a local. By reference (php's
`use (&$x)`, python's `nonlocal`) the body's name IS the variable around
it: no local is declared, the read and write chains continue past the body
(`variable_lookup_chain`, `write_lookup_chain`), and the two rules above
apply as to any variable the body reaches. From the module (php's and
python's `global`) the chains go on past every callable around the body to
the file's top level. A by-reference capture of a name nothing around it
declares yet declares it where the write chain ends: `use (&$x)` in the
function around the closure, `global` at the file's top level.

A python comprehension (list, set and dict, and a generator expression) is
a callable scope of its own whose loop variables are its locals, declared
for the whole comprehension (the element is written ahead of the `for`
that binds it). An assignment expression inside one (`:=`) binds in the
function around it, which is not modelled.

## Write-once members

A write-once member (php `readonly`) its own class writes through a
method's receiver has a state as the class's methods run
(`FileAnalysis::member_states`). It is unset (`Undef`) where the
constructor starts, unless a promoted parameter of the constructor is the
member, which sets it as the call begins. Every other method starts it on
an `Entry` join of the ways the constructor ends: its body's end and each
`return` of its own (a throw builds no object to run a method on). A
method marked `@mark.unconstructed` (php `__clone`, `__wakeup`,
`__unserialize`) runs on an object no constructor built and starts it on
nothing. Each write is a reset and an edge to the value written, and joins
the member past the branches and loops around it as a variable's write
does. A write in a closure the method makes sets the member only if the
closure runs, so past where the closure is made the member says nothing
(`TODO(readonly-closure-writes)`).

`misplaced_member_writes` asks the state where a write lands. Unset is a
lazy initialisation; a value that is known and cannot be unset is
`AlreadySet`, a write that fails at runtime. A value nothing typed says
neither, so it is no finding. The state is the constructor's in this file
only: a member a parent declares, or a class whose constructor another file
declares, has none. A method the constructor calls starts as the
constructor's end leaves the member, though it runs before that end
(`TODO(readonly-construction-calls)`).

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

## Try

A `try` is a construct of four arm roles: the tried body, its handlers
(`catch`, `except`), the body that runs when the tried one ends (Python's
`else`) and `finally`. Perl's `eval BLOCK` is a quiet `try`: no handler,
but it swallows what its body throws (`@ctl.try.quiet`).

- **A handler starts** from any point the tried body could have stopped
  at: the value entering the `try`, or past a write in the body that a
  *raise* follows. Each language's control document names its raises
  (`@ctl.raise`, any depth): Perl and PHP a call, a construction, a
  `require`/include, a throw and a division (both die on a zero divisor);
  Python, where almost every operation raises, each statement and each
  call. A raise follows a write when it ends past the write's site (a
  call in the write's own right side ends before it) or sits in a loop
  around the write inside the arm, which runs again. A runtime error the
  document does not name (Perl's strict-refs deref of a string, PHP's
  `TypeError` from an operator) is outside the rule.
- **Past the `try`** comes the tried body's end (or its `else` end), each
  handler's end, and for a quiet `try` the points its body could have
  stopped at. A `finally` runs on every one of those paths and on the
  unwinding ones, and starts from all of them; only the completing ones go
  on past it. Each path out of the `finally` is read where a `finally`
  write stands on every way to it; where none stands on any, it carries
  the completing paths; between the two, it is taken back through the
  latest construct before it that writes, each of whose paths is read the
  same way. A path inside a loop of the `finally` with no write of its own
  level before it is read where it stands, which joins the unwinding entries
  too (`TODO(finally-loop-paths)`).
- A `try` with no path out leaves the arm holding it.
- A throw in the tried body of a `try` that catches is no leave of the
  callable; a throw in a handler or a `finally` leaves as anywhere else.
- The tried body and every handler are arms, so an observation inside
  them is bounded to them.

## Unbinding

A write can leave a name bound to nothing: Python's `del x`, and the `as`
name of an `except` once its handler ends. The document says so with
`@flow.unbind` on the name, landing at its token or at the end of the
match's `@flow.lasts` node. The write mints its `Reset` and then an
`Unbind` marker at the same site. A read whose latest write is one raises,
so the registry answers `Never` for it, and every join it reaches drops
that path: past `if c: del x` the read holds what the other paths leave.
A read straight after `del x` answers `Never` too.

An `except` clause is its handler arm whole, so the name it binds
(an instance of what it catches) is a write inside the arm. A tuple of
classes (`except (E, F) as e`) binds nothing typed yet
(`TODO(except-tuple-binds)`).

## Control structure is declared by each language's query document

One `@ctl.*` vocabulary (branch / cond / then / else, loop and its body,
step, else and once, try / body / handler / finally, and `leave.*` for
return, throw, break, continue and jump) replaces `@ctl.cond/then/else/exit`,
`@arm` and the branch and loop kinds of `@guard.region`. Perl declares its
own in `queries/perl/ctl.scm`, so Perl's exit keywords stop being a Rust
table. The narrowing atoms (`@narrow.is/set/unset/...`) describe
conditions and stay. The join builder reads the constructs through a
tree-free `ControlView`; nothing about them persists. The walk asks the
same document what runs conditionally: `control::arm_around` climbs from a
node and tries each ancestor as a match's root (an observation's extent, a
shift or a key write that may not run), and `control::branch_chain` reads
an `if` chain's arms in order. Rooting each match at one node keeps the
cost the climb's: the document read over a whole file costs what the walk's
own patterns do.
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
  `die` unwinds. A throw or a call inside the body of a `try` that catches
  (it has a handler, or it is an `eval`) leaves nothing: the try's own
  paths out carry it (§Try).
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
nothing. The one slot read that answers it is a read of a name a write
left unbound (§Unbinding): that read itself raises. Being the identity of `join`, it drops a non-completing arm from
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
fails), R's `<<-` (`TODO(write-capture-declarations)`: a write capture
nothing states yet, so the write binds a local), a comprehension's `:=`, effects through a wrapper in
another file (waits on the whole-program engine's `EffectsOf` key), and the
epic's original diagnostics (must/may use-after-move, the cpp D-codes, D9).
