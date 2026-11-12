# ADR: Flow-sensitive narrowing

A guard (`$x->isa('Foo')`, `ref($x) eq 'HASH'`, `return unless defined
$x`) refines a subject's type over the region it dominates, then the
type widens back at the region's exit. Lives in its own file,
`src/builder/narrowing.rs` — a child module of `builder` so its
`impl Builder` methods keep private-field access while staying the single
tree-sitter consumer (rule #1: `build()` drives it).

## It's an emission feature, not a query change

The query path already does flow-sensitive lookup: on a `Variable`
attachment with a point, `ReducerRegistry` returns the **narrowest-span
`InferredType` witness containing the point**, skips witnesses whose span
*starts after* the point (temporal), and **excludes** witnesses whose
span doesn't contain it. That exclusion *is* the un-narrowing — a
witness scoped to the guarded region stops containing points past the
region, so the wide (zero-extent) witness wins again. Monotone bag, no
retraction. So narrowing only had to **emit** span-extent witnesses;
"widen at block exit" is span arithmetic, not a deletion.

The one trap: narrowing does **not** route through `push_type_constraint`
(which zero-extents the witness, `span.start..span.start`). A narrowing
witness must carry the **full region span** so narrowest-span-wins picks
it inside and scoped-exclusion drops it outside. Source tag
`Builder("narrowing")`, emitted in the live walk so it survives
enrichment truncation.

## Soundness: truncate at the first disturbance

A span-extent witness would shadow a *later* reassignment's temporal
witness, so the region is truncated at the first op that could disturb
the subject. Both cutoffs are lookups in an index built once per file,
never a search per fact — every use observes, so a per-fact scan is a
scan per use (Znuny `PDF.t`: a whole-block CST scan per place use took
one file's build to 100 s). A variable's cutoff is its first rebind, a
write of its whole place in the same index (`rebind_index`, from the
FlowEdges, shared with the pack narrowing and the pack observations); a
place's is its first disturbance (`PlaceDisturbanceIndex`, one pass over the
file's trees, `s///e` code included). Conservative by construction — it under-narrows, never lies. The bias is
load-bearing for the diagnostics built on top: they miss some real bugs
but never invent a false `Undef`/`Optional`.

## A write is a reset

Temporal ordering answers a read from the newest witness at or before it.
That is only right while every write leaves one. A rebind whose RHS nothing
can type (`$ct = f($_)` with `f` unresolved) would otherwise leave nothing,
and the read falls back on the newest belief it *can* see — the one the
write replaced. `my $ct = undef; … $ct = f(); defined $ct` read `Undef`
and every `defined` guard on it read as one that can never pass.

So every write mints a **reset marker** — `WitnessPayload::Reset`, a
zero-width witness at the write's site — a declaration and a plain
assignment alike; there is no second kind of write. Perl's walk mints it in
the assignment visit, before anything downstream is typed; the flow lane
mints the same site again post-walk (idempotent), so the two lanes agree on
every write by construction, and its edge is the RHS's value, a plain
`Edge` that simply drops out when nothing types it. The marker is a
PAYLOAD, never a source tag a reducer inspects (CLAUDE.md rule #14).

A reassignment's site is the END of the assignment expression, because
every read inside it happens before the write: the RHS of `$n = $n->parent`
and the implicit read of `$x ||= …` both see the value being replaced, and a
guard's region runs up to the write. A declaration writes at its binding
token instead, since its RHS cannot name the variable it declares. One
speller, `Builder::assignment_write_point`, serves the walk, the flow lane
and chain typing.
A pack's document says which of its assignments are writes of this kind
by anchoring the pattern on `@flow.assign` (php's `$x = …`, python's, R's
and cpp's assignments): every target of the match is written at the
assignment node's end, with its marker there, so `$node = $node->parent`
reads the old `$node` on the right. An annotated write (`x: int = 5`) is
anchored too, so its declared type lands at the write it is declared with.
A target with no anchor binds at its token and mints no marker: a
declaration (`T x = …`), whose RHS cannot name it. A later write's marker
retires a declared type like any other value; what a declaration should
still hold past it is the bounds design's, not a per-language rule.
`FrameworkAwareTypeFold` reads it three ways:

- **It is a cutoff.** At the query point, every witness strictly before
  the latest reset is dead, on every axis — the class assertion, the rep
  observations, the first-param constraint. Companions minted at the site
  itself survive. This is the only retirement there is: `my $x = Foo->new;
  $x = {}` reads `HashRef` because the marker killed `Foo`, not because a
  hash fails to subsume a class. A subsumption test cannot do that job —
  a deref's `HashRef` and an assignment's are the same value, and only the
  marker tells a write from an observation. Identity-over-rep still holds
  for the deref, which mints no marker.
- **Its `Unknown` has supremacy over its past only.** The marker is a value
  when nothing typed stands at or after its site; a typed RHS at the site
  and any later write displace it, and rep or scalar-context evidence
  landing after it revives the variable through those axes
  (`$r = f(); $r->{k}` reads the hash). An annotation-priority `Unknown`
  (`@var A|B`) is a claim about the value and keeps its place.
- **A first binding resets nothing.** A marker with no value-binding
  witness before it (`WitnessPayload::binds_value`) is not a value: the
  write is the declaration — `my $x = f()`, or a pack's first `$x = f()`
  — and its untypable RHS stays absent. Absence keeps the evidence door
  open; a reset closes it on what came before.

Because a declaration is a write, every fact about a variable that must
survive its declaration is anchored AT the binding token, never before it:
the first-param assertion (`my ($self) = @_` — the `$self` token), a
plugin's parameter claim (`binding_site_of`), a pack's declared type. A
fact anchored at a sub's start would be retired by the declaration's own
marker, and the only alternative — exempting declarations — would make a
language without declaration syntax a special case.

`Unknown` is a real answer where it stands, and each half of that matters:
the scope walk stops on it (a shadowed inner `$x` with its belief gone
never answers with the outer's), and consumers ask the value
(`InferredType::is_known()`) rather than matching the variant.

A rebind that may not run is old-or-new no matter what its RHS is, and
lands as `Unknown` outright. "May not run" is asked at two granularities,
both spelled once in `cst.rs` (`cst::conditional_arm`, one climb, two
`ArmBound`s). The climb is exact: a condition, a loop list, a `for`
initialiser and a short-circuit's left operand run whenever their construct
does, so only the arm that may be skipped counts.

- **Relative to the block the witness lands on**
  (`ArmBound::Block`): a statement modifier, a
  ternary arm, a short-circuit. `$x = undef if COND` anchors at the
  statement start, so the guard's own condition reads no value the write
  has not produced, and the successor reads no value it may never
  produce. The block's own `if`/loop does not count — inside the block,
  the block has run, so `if (…) { $x = Foo->new; $x->m }` still types
  the call.
- **Relative to the variable's binding scope.** A write inside a nested
  block sits on the block's scope, which a read after the block never
  walks through, so that read would keep the pre-block belief. The honest
  answer there is `old ⊔ new`, and no reducer can compute it: a reducer
  sees ONE attachment, and the write landed on the block's. So the fold
  stands in for the join and puts a reset marker on the binding scope —
  the one every later read in the variable's extent walks through.
  **This is an approximation of a join and is deleted when Epic 16's
  `JoinFold` lands** (`docs/epics/16-cfg-tier.md`, Phase C); its anchor
  is in that epic's table.

An **observation** is the other half of the same question: a use of a
variable or place that reveals what it holds — any deref (`@$x`, `$$x{k}`,
`$x->[0]`, `&$x`), a numeric, string or regex operator, a builtin's
argument. It is no write, so it mints no marker and resets nothing; it says
what the subject holds on the paths that reach it. Every use reaches the
bag through one producer, `Builder::observe`, as a `TypeObservation` —
evidence, which a value and a class identity outrank, never a proof. Its
region is the narrowing rule's: from the use to the end of the
conditionally-run arm around it (`cst::conditional_arm`: the statement a
postfix modifier guards, a ternary branch, a short-circuit RHS), else to
the end of its block, cut at the subject's next rebind
(`RegionFact::Observe`, sharing `PendingNarrow`'s cutoffs, place subjects
included). Past an arm the value is a join of the arm and its bypass,
which `JoinFold` will answer; until then the arm says nothing outside
itself.

Declarations do not mint a marker, and that is placement rather than
principle: a declaration's companions may legitimately sit before it (a
plugin's parameter assertion is minted at the sub's start, ahead of the
`my $c = shift` it describes), and a cutoff there would erase them. Once
every binding witness sits at or after its binding site, declarations can
mark too and the rule needs no syntax at all.

## Subjects: variables and places

A guard subject is a variable (`$x`) or a **place**: a part of a
variable's value, named by what its root binds and the steps from it
(`model/file_analysis/place.rs`, cfg-tier §3.3). `cst::read_place` reads
the steps off the spelling — `Deref`, `Hash(slots)`, `Array(slots)` — and
the builder binds the root and every key variable where they are written
(`binding_key_here`), so the spelling never keys a fact:

- `$self->{a}{b}`, `$self->{a}->{b}`, `$$self{a}{b}` and `${$self}{a}{b}`
  are one place; `{'a'}`, `{"a"}` and `{a}` are one key.
- `our %h; $h{a}` in package `P` and `$P::h{a}` are one place; an inner
  `my $self` is another place from the outer one.
- A subject's every subscript names its slot: a constant, or a plain
  scalar (`{$k}`). A computed subscript is no subject, and as a
  disturbance it reaches every slot.

Place facts attach to `WitnessAttachment::Place`, temporal like
`Variable` but with no scope walk: the place is its own identity, and
the region decides where a fact holds. A member access carries its
receiver's place on its invocant (`Invocant::Place`), and a deref with no
ref of its own carries it on its `ArrowDerefSite`, so the receiver query
and the deref diagnostics read it without re-reading the spelling. Plain
variables are `Variable { name, scope }` keyed to their binding (below).

A pack builds the same places from its captures at skeleton assembly
(`query_extract/places.rs`): a member read whose receivers lead down to a
resolved variable read is that variable's declaration plus a `Member` step
per read. A guard whose subject is a member read (`isinstance(self.x, T)`,
`dynamic_cast<D*>(o.b)`) narrows that place, cut by the same index: a
member write is a write of its place, and a variable or member read that no
further member read reads through is an opaque use. A bare member name in
a member function is rooted at the body's elided receiver
(`Binding::Receiver`), and so is a spelled receiver (`@receiver`, C++'s
`this`) in that body, so `this->x` and a bare `x` are one place. A guard
on a bare name records both a variable and a place narrowing; assembly
keeps the place only when the name roots at the receiver. A receiver carries its place (`Invocant::Place`) only
where a narrowing gives that place a value; elsewhere the place would
answer nothing, so the ref keeps its plain receiver.

Truncation is `Place::disturbed_by`: a **write** disturbs the place and
everything under it — the place itself, a proper prefix, or an
overlapping slice (a slice may write every slot it names, so it is a
write wherever it appears; `delete`, `undef` and `local` on a slot are
writes too, since each leaves it without the guarded value); an **opaque use** (a call, an argument, a
whole-container op) disturbs only what lies strictly under it, so a
method call on the slot's value leaves the slot narrowed. A read that
steps further in is not an access. Two steps overlap when they can reach
one slot: distinct constants never do, a key variable may hold anything.
Rebinding a key variable moves the place, so its narrowing ends there
(at a rebind of the key variable's binding).

Accessor places (`$self->name`) are out: an accessor isn't a stable slot
(it can return a different object per call, with side effects), so
soundness needs a stricter no-call-between-guard-and-use model.

## Variables are bindings

Every fact about a variable attaches to the place its name binds to where
the fact is made, `Place{root, []}`: a lexical is its declaration
(`Binding::Lexical`), a package variable its package and name
(`Binding::Package`), and `@_` the arguments of the callable it is read in
(`Binding::Arguments`), never a symbol, since `@_` cannot be declared. A
token that declares two symbols in one scope (python's class-body binding,
`Symbol::declared_with`) binds one value. The builder binds the walk's
facts once the walk is done, since a fact can precede the declaration it is
about (`bind_walk_witnesses`), and every later push as it goes
(`Builder::push_witness`); a pack binds the assembled analysis once, before
the seal (`FileAnalysis::bind_variable_witnesses`); enrichment binds its own
pushes. One rule serves all three (`bind_variable_witness`), over the two
binders `Builder::binding_of` and `FileAnalysis::variable_binding_of`.

A read is then name resolution, never a merge: the reader binds the name
where it reads it and queries that one place. A shadowing declaration is its
own binding, so an inner `my $x` never sees the outer one's facts, and a
redeclaration in the same scope is a fresh binding too: a read after it sees
only the new variable's value. On the assembled analysis, a fact made in the
scope declaring the name belongs to that declaration wherever it sits in the
scope (a member's declared type spans its class). An edge that reaches a
variable from a read takes the read's point; one with no read position (a
member's declared type) asks at the end of the scope the binding lives in.

A rebind cuts a fact about the variable it binds, not every variable of the
name: each FlowEdge target is a write of the whole variable in the place
disturbance index (`rebind_index`), so a write to a shadowing `$x` leaves
the outer `$x`'s narrowing standing. A pack's variable regions are cut
the same way once its analysis is assembled and bound
(`FileAnalysis::bind_variable_regions`), since the extractor has no
bindings to key them by.

**A fact made in a nested block is a region.** It holds from where it is made
to the end of the block it was made in, so the else arm never sees the then
arm's write, and past the `if` the write is gone. What the variable holds
after the `if` is the join of "the write ran" and "it did not": a write in a
nested block also mints a reset marker on each enclosing level, at the end
of the statement holding that level, up to the binding. With no join of
values, that marker reads `Unknown` after a write that may have run, the
same answer the flat walk gave. A join that keeps the value both arms agree
on is `JoinFold`'s job (`docs/prompt-cfg-tier.md`).

A key write (`$h->{k} = …`) in a nested block extends the shape on the
binding with no end: it opens the shape instead (`mutation_extension`), which
already says the write may not have run.

## Polarity and the negative lattice

`GuardFact { subject, op, asserts_when_true, read }`; `op` is
`NarrowOp::To(T)` (`isa`/`ref-eq`) or `Defined { read }` (`defined`/`blessed`). The region a
guard dominates either asserts the guard TRUE or FALSE;
`op_for_region(holds)` returns the fact's op when polarity matches, else
`op.negated()`. `To(_).negated()` is `None` ("not Foo" has no positive
lookup target → emit nothing, stay wide); `Defined.negated()` is
`To(Undef)`. So the `defined`-family negatives (`else` of `if (defined
$x)`, `return if defined $x`, `unless (defined $x)`) narrow to the bottom
`Undef`, lighting up the early-exit rows **for free** through the
existing polarity plumbing. `narrow_block_guard` walks the whole
`if`/`elsif`*/`else` chain: each arm narrows by its own condition plus the
cumulative negation of every preceding condition, so an elsif-chain `else`
(and the intervening elsif blocks) light up the `defined`-family negatives
too — non-representable negations (`isa`) contribute nothing, making the
cross-condition intersection automatic. General negation stays parked
(see `optional-types.md`).

A condition is read through its connectives before any atom is
(`facts_for_region(cond, holds)`, the one reader every arm, fall-through
and chain calls). `!X` / `not X` read X at the flipped polarity. `A && B`
(or `and`) proves both sides only where it holds, and `A || B` (or `or`)
proves both sides false only where it fails; the other outcome of each
proves nothing about either side alone, so `return if (defined($x) && $y)`
leaves `$x` as it was rather than `Undef`. D3/D4 judge each guard literal
(an atom with the negations applied directly to it) at its own span and
polarity, since a verdict on one conjunct is not a verdict on the
condition.

A statement modifier is a one-arm region: `STMT if G` narrows STMT by G
TRUE, `STMT unless G` by G FALSE, exit or not. A ternary's branches and a
logical operator's right side are arms the same way: `G ? A : B` narrows A
by G TRUE and B by G FALSE, `G && R` / `G and R` narrow R by G TRUE, and
`G || R` / `G or R` by G FALSE (`narrow_arm`, the one spelling).
A loop tests its condition before every run of its body, so the body is
an arm too: `while (G)`, `STMT while G` and a C-style `for (…; G; …)`
narrow by G TRUE, `until` by G FALSE. `do BLOCK while G` is the one
exception — perlsyn runs the block once before the first test. Nothing
narrows after a loop, since `last` leaves with G unchanged. When STMT is an exit, the
rest of the block is the fall-through and takes the opposite polarity.
Only the fall-through records a `GuardSite`, so the D3/D4 redundancy
diagnostics see each statement-level guard once.

"Falls through" is a fact about statements. A block leaves when any of
its statements leaves, wherever that statement sits, and what follows it
there is dead: a statement leaves when it is an exit, or when it is an
`if`/`unless` chain with an `else` whose every arm leaves
(`block_leaves`). A chain with no `else` has an implicit one, which falls
through. After a chain whose arms all leave but one, the rest of the
enclosing block runs where that arm does, through the same
`narrow_block_remainder`, narrowed as that arm's block is: the first
arm where its block is entered, an `elsif` where its own guard holds and
every earlier arm was not entered, the `else` where no earlier arm was
(`unless ($x->isa('Foo')) { return }`, `if ($x->isa('Foo')) { … } else
{ die }`, `if (!defined $x) { die; warn … }`). Each arm's guard is recorded once,
as the arm's. Where no arm falls through, the rest of the block never
runs and nothing narrows it; where two or more do, the rest runs where
any of them does, and nothing narrows past the enclosing block either:
both are the CFG's block-exit join (`docs/prompt-cfg-tier.md` §3.5,
`TODO(cfg-exit-join)`). An exit is a word read by its spelling
(`is_exit_expression`'s table: `return`, `die`, `exit`, `croak`,
`confess`, `last`, `next`, `redo`, `goto`), so a file's own sub named
`croak` counts as one, and so does a `goto` whose label lies in the rest
of the block; a callee that never returns is an effect on the callee,
which replaces the table once effects and the CFG exist
(`TODO(noreturn-callee)`).

A pack's document states its guards in three parts, and the extractor
(`query_extract/extract/narrowing.rs`) joins them by containment, the
polarity rule above ported over captures. An **atom** is a match naming
its subject (`@narrow.var`) and what it proves on the atom's node:
`@narrow.is` (the class `@narrow.type` names where it holds, nothing where
it fails), `@narrow.set` (a value where it holds, the null value where it
fails) or `@narrow.unset` (the swap). A **connective** is a node marked
`@narrow.not`, `@narrow.and`, `@narrow.or` or `@narrow.group` (a
parenthesis). A **position** match marks a condition `@narrow.cond` and
what it decides: `@narrow.then` runs where it holds, `@narrow.else` where
it fails. An `else` arm is the statement it holds, so a later clause of
the chain (`elseif`, `elif`, C++'s `else if`) is the else arm of the
condition before it. `@narrow.loop` marks a position's construct that
tests its condition again after each run of its arm (PHP's `while`): a
`break` in the arm lands after the loop too, so its arms decide nothing
about what follows. The fall-through derivation and `@narrow.loop` are a
single-block stand-in for CFG reachability and retire with it
(`TODO(cfg-exit-join)`). Apart from guards, each document marks every
statement that leaves its block unconditionally `@flow.leaves` (return,
throw or raise, `break`, `continue`, C++'s `co_return`, PHP's `die` and
`exit`, which the language reserves, so their spelling names them), and
the extractor derives the rest by containment, the Perl rule above: a
condition's statement is the node holding it and its then arm; a chain
is that statement with every condition whose statement is an else arm of
the chain; its arms are the narrowest regions its conditions decide, each
known by every condition deciding a region around it. An arm leaves when
it, or one of the statements it runs in sequence (its named children
outside every field: a loop's body is a part of the loop, not a
statement of the arm), is marked `@flow.leaves` or is a chain that
leaves. Where exactly one arm falls through, the rest of the block holding
the chain runs where that arm does, a region of every condition that
decides it. A comment is no statement, so where it sits does not matter. A
call that never returns by its callee's nature (`sys.exit()`,
`std::abort()`, `[[noreturn]]`) is an effect on the callee, to mark once
effects and the CFG exist (`TODO(noreturn-callee)`). An
atom climbs to each condition above it through connectives alone (an
atom inside a call argument is no part of the condition), and the
connectives on the way decide its value in each of the condition's
regions, or that the region says nothing about it. The refinement
attaches in the atom's scope over the region, cut at the subject's next
rebind (a variable) or disturbance (a place). A failed `is` proves
nothing. A value proved set is what it held coming in without its null,
which only the query knows, so the region carries an edge rather than a
type: `Projected { base: Expr(the subject's read in the test), step:
Defined }`. The query materializes the base and keeps each arm it may be
once set (`?Foo` → `Foo`, `A|B|null` → `A|B`; a value never nullable is
itself). The test and the regions it decides are disjoint, so the base
never reads its own narrowing. Where the subject token is a name inside
its read (php's `w` in `$w`), the base is the read the token names.

PHP's document declares its atoms (`instanceof`; a single-argument
`isset`, `!== null`; `=== null`, `is_null`), its connectives and its
positions; a property of `$this` is a place of the method's object
(`Binding::Receiver`), so a guard on it narrows the property's receiver,
and a property of a local is a place of that local. A variable read roots
its places at its value (`$x`, the node a member read names as its
receiver), while its reference stays on the name token (`x`). `self`,
`static` and `parent` on the right of `instanceof` carry the
`@type.enclosing|called|super` marks a written type does, so the atom
names the class around the guard or the one it extends; `static` names the
class around it, since the class the call is made on lies beneath it.
A write ends a place's narrowing where it lands, as it ends a variable's
(an assignment's end, after every read it makes), so the written access
reads the place as it held coming in. A member value access carries its
own place where a narrowing gives that place a value
(`Ref::access_place`), and the `readonly-write` rule asks it: a write to
a member a guard proves unset (`!isset($this->x)`, `$this->x === null`)
is a lazy init, as `??=` is.

Python's document declares `isinstance` and C++'s `dynamic_cast` as
`@narrow.is` atoms naming their `@narrow.type`, read through the
language's type spelling first (`isinstance(x, str)` is a string, not a
class named `str`); `x is not None` is a `set` atom and `x is None` an
`unset` one. C++'s `std::optional` engagement (the subject's own
truth, or `has_value()`) names no type: its class is what the pack's
`narrow_guard` peels off the subject's declared type. So `if not
isinstance(x, T): return`, `if x is None: return` and `if (!opt) return;`
narrow the rest of their block. The peel is a stand-in: an optional is a
class whose `->` and `*` reach its value, and `.` its own members
(`TODO(cpp-wrapper-deref)`).

## A tag is a value: `ref` names its subject

`ref $x`, `reftype $x` and `blessed $x` return a string that says what
`$x` held — so their `Expr` witness is `RefTagOf { subject, site, via }`,
a `String` to every consumer (`is_string()`, `String` on the Surface and
on the wire) that also names the variable it was asked of. It flows like
any value: `my $ref = ref $x` reaches `$ref` by its ordinary flow edge,
with no fold table and no name lookup.

`ref` is a builtin and core knows it; `reftype` and `blessed` are
Scalar::Util's, declared by `frameworks/scalar-util.rhai`'s
`ref_tag_functions()` and recognised only where the file imported the
name from that module or called it fully qualified. A file's own
`sub blessed` is just a sub. The same gate decides the `blessed $x`
strip below.

So `ref($x) eq 'HASH'` and `$ref eq 'HASH'` are one guard, read two ways:
an inline call is its own tag; a variable is whatever tag its value is.
The variable form resolves in the fold (`emit_tag_narrowing_witnesses`,
clear-and-emit on `tag_narrowing`), since `$ref`'s value is a fold
answer. A tag guards its subject only while it is still true:

- **Same sub.** A tag returned from another sub names that sub's
  variable; the caller's namesake is a different binding.
- **No rebind since the tag.** A write to the subject between the `ref`
  and the guard voids it; the region also truncates at the next write,
  as every narrowing does.

`via` says what the literal can prove: `ref` a reftype token or a class,
`reftype` only a token, `blessed` only a class. A string-context use of
the tag (`$ref eq 'HASH'`) is an observation, which the tag — a value —
outranks. Two tags that disagree join to
plain `String`. Under unions the same rule holds arm-wise: a guard reads
a subject only when every arm of `$ref`'s value names the same one.

## `defined`/`blessed` strip as an edge to the guard's read

A `defined`/`blessed` guard's region holds `Projected { base: Expr(read),
step: Defined }` on the subject's place, minted at walk time like any other
region fact and cut at the same rebinds and disturbances. `read` is the
subject as the guard reads it; the walker types that read
(`emit_expr_witness`), so the strip resolves when asked, through whatever
the read holds then: a sub return the fold converges, an import only the
index types. Nothing re-derives it per fold round. The read sits in the
guard, outside every region the guard decides, so the projection never
reads its own narrowing. The pack tier states the same strip the same way.

`Defined` answers only over a base known to be possibly undef (`Optional`,
`Undef`). Over anything else, a class or an `Unknown`, it is no evidence,
so the region never outranks what the read already knows: a deref's rep
inside `if (defined $c)` still reads, and an `isa` beside the guard still
holds. A conjunction that proves a type for a subject drops its `defined`
of that subject (`conjoin`): the type is already defined, and two regions
on one span would otherwise tie.

## Evaluation order: a modifier runs before its statement

A read asks at its own site, so it sees every fact made before it in the
source. A statement modifier inverts that: `STMT if G` (`unless`, `while`,
`until`) evaluates `G` before `STMT`, and `STMT for LIST` evaluates `LIST`
before it, though both are written after it. The builder states the order
where it emits the read: a variable read inside a condition or loop list
carries `EvaluatedBefore(stmt)` on its `Expr` beside its value edge
(`cst::evaluated_before` finds the statement). Its span stays the read's
site (rule #14); what changes is which facts reach it. The registry asks
such a read at a `ReadPoint { at, skips }`: every fact at or before `at`
except those made inside `skips`. So in `$x = f($x) if defined $x` the
strip reads the value `defined` tested, not the one the assignment
leaves, and `$o = Bar->new if $o->alive` calls `alive` on the old `$o`.
This holds for every read in the condition, `defined` or not.

The skip is a span, not an earlier point: a condition's own writes and
narrowings still reach the reads after them (`last unless defined($n =
$n->{parent}) and ref $n`; `... if defined $x && $x->ok`). The constant
fold reads by the same rule. A receiver's place, hover on a read and the
fold's binding lookup all ask the read's `ReadPoint`
(`FileAnalysis::read_point`), never their own span's start.

`do BLOCK while G` runs its block first, so its `G` reads where it is
written and carries no order fact. A `while`/`until` modifier's condition
skips the statement as a prefix loop's condition skips its body: no loop
join yet (`docs/prompt-cfg-tier.md`).

A skip can only remove facts that precede the read. The mirror case
cannot be stated with one point: the statement's reads of a write its
condition makes (`push @out, $l while defined($l = <$fh>)`), and a
C-style `for`'s step reading its body's writes, would need a fact made
after the read to reach it (`TODO(evaluation-order-later-writes)`). In
the pack tier no expression that precedes a condition in the source can
write today: Python's `a if c else b` and comprehension clauses would
need the fact once `:=` is a write (`TODO(walrus-evaluation-order)`).
Cursor-time completion on text with no recorded read asks at the cursor.

## Reaching writes

A read of `Variable@point` finds the value it sees by folding every witness
on the variable and discarding those past the point or behind the latest
write — `LiveWindow` in `FrameworkAwareTypeFold`, which the registry's edge
chase also asks so it never chases an edge the fold will discard. That is
reaching definitions computed at query time, once per read. A chain of
self-reads (`$x = $x->next` × N) still costs N² hops: the value at write N
depends on write N-1's, and the chase's memo dies with each top-level query.

The fix is to mint the facts at the producer (rule #11). The flow lane
records, per read, the set of writes that reach it, with refinement nodes
where a guard narrows the variable (SSA's π), and a read edges to its
reaching writes' value attachments instead of to `Variable@point`. Each
write's value is then computed once per fold iteration, and the chase
needs no temporal rule at all. A loop makes the reaching set plural, which
is the one join (`docs/adr/union-types.md`). Until then, `TODO(def-use)` marks
the query-time window.

## Forward work

The remaining residuals — accessor places and the general
`Not`/`Difference` negation tier — live in
`docs/prompt-flow-narrowing.md` (which also records the dynamic-key
soundness knob, Option A vs B). Diagnostics the lattice enables:
`docs/adr/narrowing-diagnostics.md`.
