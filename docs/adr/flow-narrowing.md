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
one file's build to 100 s). A variable's cutoff is its first rebind
(`RebindIndex`, from the FlowEdges, shared with the pack narrowing and the
pack observations); a place's is its first disturbance
(`PlaceDisturbanceIndex`, one pass over the file's trees, `s///e` code
included). Conservative by construction — it under-narrows, never lies. The bias is
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
the region decides where a fact holds. A method call mints its receiver's
place on the ref (`receiver_place`), so the receiver query and the deref
diagnostics read it without re-reading the spelling. Plain variables stay
on `Variable { name, scope }` until the variable lane moves onto bindings.

Truncation is `Place::disturbed_by`: a **write** disturbs the place and
everything under it — the place itself, a proper prefix, or an
overlapping slice (a slice may write every slot it names, so it is a
write wherever it appears); an **opaque use** (a call, an argument, a
whole-container op) disturbs only what lies strictly under it, so a
method call on the slot's value leaves the slot narrowed. A read that
steps further in is not an access. Two steps overlap when they can reach
one slot: distinct constants never do, a key variable may hold anything.
Rebinding a key variable moves the place, so its narrowing ends there
(`RebindIndex`, by name, until variables move onto bindings).

Accessor places (`$self->name`) are out: an accessor isn't a stable slot
(it can return a different object per call, with side effects), so
soundness needs a stricter no-call-between-guard-and-use model.

## Polarity and the negative lattice

`GuardFact { subject, op, asserts_when_true }`; `op` is `NarrowOp::To(T)`
(`isa`/`ref-eq`) or `StripOptional` (`defined`/`blessed`). The region a
guard dominates either asserts the guard TRUE or FALSE;
`op_for_region(holds)` returns the fact's op when polarity matches, else
`op.negated()`. `To(_).negated()` is `None` ("not Foo" has no positive
lookup target → emit nothing, stay wide); `StripOptional.negated()` is
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
diagnostics see each statement-level guard once, as before.

A pack guard states its region one way: the pattern that recognizes the
guard tags the arm it enters `@narrow.arm` (an `if`/`elif` body, a
ternary's consequence, the right of `&&`/`and`), and the extractor attaches
the refinement in the guard's own scope over that arm, cut at the
subject's next rebind. The guard and its arm join by match, so neither
needs to be a scope. Pack guards (`isinstance`, `dynamic_cast`, optional
engagement) have no complement, so no arm narrows on a false test; the
early-exit fall-through (`if not isinstance(x, T): return`) waits on a
pack-side negation (TODO(pack-exit-guard)).

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

## `defined`/`blessed` strip via a re-emittable fold pass

`StripOptional` can't resolve at walk time: the subject's `Optional`
often comes from a sub return that only converges during the fold. So
`recognize_*` records the guard (subject, region, and the guard's own
`query_point` — *before* the region, so the read sees the un-narrowed
type and the pass's own output can't feed back), and
`emit_defined_narrowing_witnesses` re-derives `Optional<T> → T` each fold
iteration (clear-and-emit on tag `defined_narrowing`).

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
