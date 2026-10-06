# ADR: The conclusion layer — a persisted, point-free bake of the type chase

## Context

The witness bag's registry chase (`model/witnesses/registry.rs`) answers a
cross-file type query by decoding the provider's bag and walking reducers.
That is correct but requires a full blob decode per provider per query. A
**conclusion** is the same chase, partially evaluated ahead of time over
what one file's bag knows, with the three query binders (`ReducerQuery.point`
/ `.receiver` / `.arity_hint`) and the cross-file world left free. Free
binders residualize as syntax inside the stored form; the cross-file world
residualizes only as a `Link`. A per-file `ConclusionMap` persists next to
the blob and serves the cross-file consult path without decoding the bag;
the bag remains the derivation of record and the fallback for anything the
map cannot answer.

## Decision

```rust
/// Portable, cross-file-enterable key. Strings only — see "Keys" below.
pub enum ConclusionKey {
    MethodOnClass { class: String, name: String },
    SubByName(String),
    SlotType { class: String, key: String },
    TypeName(String),
}

/// One key's partially-evaluated answer.
pub enum Conclusion {
    Value(InferredType),
    ReturnOf(ReturnExpr),
    Timeline(Vec<TimelineSegment>),          // λ point — bake-internal, never persisted
    Link { target: ConclusionKey, arity: Option<u32>, receiver: ReceiverRule },
    Project { base: Box<Conclusion>, step: ProjectionStep },
    OpenNone,
}

pub struct TimelineSegment { pub span: Span, pub value: InferredType }

pub enum ReceiverRule {
    /// Pass the evaluating query's receiver through unchanged (inheritance hop).
    Thread,
    /// Keep the incoming receiver iff its class IS `class` or a subclass of
    /// it; otherwise substitute `ClassName(class)`.
    Dispatch(String),
}

/// Per-file, persisted with the blob, invalidated with it (same stamp), and
/// gated on a DERIVED fingerprint over the derivation sources rather than a
/// hand-maintained version (see Invalidation below).
pub struct ConclusionMap(HashMap<ConclusionKey, Conclusion>);
```

**Evaluation contract.** A conclusion query is `(key, receiver:
Option<InferredType>, arity: Option<u32>, args: Vec<InferredType>)` —
deliberately no `point` (see `Timeline`). The evaluator mirrors
`query_rec_body`'s consult ladder: per visible def candidate, look the key
up in that file's map; on a form, evaluate it; then the inheritance walk
over `PackageFacts::parents` (pinned fields, not the bag); then bridges.
Cycle guard and depth cap mirror the registry's `VisitedKey` (here `(file,
key, receiver, arity)`). Three outcomes per lookup:

- **a form** — evaluate per the sections below; no bag decode.
- **`OpenNone`** — this key is unbakeable here; decode the blob, rebuild
  `BagContext`, run the real registry chase. Full cost, paid for this key
  only.
- **absent** — the bag deterministically answers `None` for this key; serve
  `None` with no decode and let the ladder move on (parents, next
  candidate) — exactly what a local-reducer miss does in the live chase.

The absent-means-None split is the sharpest knife in the design: it is
sound only if the bake enumerates **every key the bag could answer** —
derivable from the bag's attachment index (keyed by attachment), the
`SymbolTable` names, and the file's declared bridges. A key-production site
the enumeration misses turns "the bag would answer" into a silent `None`.

### Keys: `ConclusionKey` vs. internal ids

The four variants are exactly the four cross-file consult shapes the chase
uses: `MethodOnClass` (the overwhelming majority of consults), `SlotType`,
bridged plugin-namespace lookups, and imported-sub-return lookups. Every
variant is strings-only: mintable by an asker holding nothing but names,
stable across any edit of the provider.

The internal ids — `SymbolId`, `ScopeId`, `RefIdx`, `Span` — are
per-`FileAnalysis` and shift on unrelated edits. They never appear in a
key. Every cross-file entry lands on a strings-only attachment
(`MethodOnClass`, `SlotType`, `TypeName`) or resolves a name to a local
`Symbol(sid)` on the provider's side (`SubByName` is that lookup given a
key spelling). The one exception is the bridged hop, which enters at
`Symbol(sym.id)` because per-FA `SymbolId`s can't be portably edge-encoded;
the bake closes it from the other side — the bridging file knows its own
bridges, so it writes those entries under `MethodOnClass{bridged_class,
name}`.

Internal ids MAY appear *inside* a persisted `Conclusion`'s `InferredType`
(e.g. `CodeRef { return_edge: Some(Expr(span)) }`): unlike the Surface, the
map is co-persisted and co-invalidated with the blob whose bag those ids
index, so they are stamp-consistent — but *interpreting* one later still
requires the bag (see the `Value` break below).

The in-RAM precedent: the resolution session's `CandidateKey` memoizes
cross-file consult answers keyed `(path, attachment, receiver, arity,
point, framework)` for one walk. The map is the persisted, point-free,
epoch-independent projection of that key.

### `Value(InferredType)`

The `InferredType` enum stored verbatim, constant in all three binders.
Example: `sub _build_ua { return LWP::UserAgent->new(timeout => 10) }`
bakes to `Value(ClassName("LWP::UserAgent"))` under both
`MethodOnClass{"My::Client","_build_ua"}` and `SubByName("_build_ua")`.
Evaluation is a hash lookup; `receiver`, `arity`, `point` are ignored — the
bake has proven the answer constant in them.

**Where it breaks.** Anything non-constant. Receiver-dependence (a fluent
accessor baked as `Value` would hand the *declaring* class to a subclass
call) must be `ReturnOf`; arity-dependence likewise; cross-file dependence
must be `Link` — a materialized cross-file value would freeze the world,
and the constraint is hops cheaper, never fewer. Second break: an embedded
non-portable attachment — `Value(CodeRef { return_edge: Some(Expr(span)) })`
serves the *type* fine, but a consumer that later invokes the coderef must
chase `Expr(span)`, not a `ConclusionKey`, so that drill falls back to a
full blob decode. The map hands out answers, not the ability to keep
deriving.

### `ReturnOf(ReturnExpr)`

The existing `ReturnExpr` (`Concrete` / `Receiver` / `ReceiverOr` /
`Operator(RowOf | ParamOf | InstanceOf)` / `UnionOnArgs { branches:
Vec<(ArgGuard, ReturnExpr)> }` / `Arg(u32)`) is already a dependent
conclusion — `(receiver, arity, args) → InferredType` — so the form stores
the payload verbatim and evaluation reuses `eval_return_expr` unchanged.
Example: `has ioloop => sub { Mojo::IOLoop->new }` on `Mojo::Base` bakes to
`ReturnOf(UnionOnArgs { branches: [(Empty, Concrete(ClassName("Mojo::IOLoop"))),
(AtLeast(1), Receiver)] })` — a getter/fluent-writer pair keyed on arity.

**Where it breaks.** `ReturnExpr` is a substitution language sealed over
`(receiver, args)` — no variant names another attachment or key. A default
sub that *chases* (`has ua => sub { shift->build_ua }`) is not a
substitution; its answer routes through edges and must residualize as
`Link`. A `UnionOnArgs` whose branches were frozen at fold time carries
only what the build-time fold resolved: an arm that chains through an
import resolves only under enrichment, which never persists, so the
persisted map misses it exactly as the persisted bag does.

### `Timeline` (λ point, bake-internal only)

`Vec<TimelineSegment>` — span-scoped verdicts, latest-emitted-wins within
containment, evaluated by picking the fold's verdict at a point. Keyed by
`Variable { name, scope }` — an internal id, so a `Timeline` has no
`ConclusionKey`: it exists only in the bake's working set and never on
disk. Segments carry spans, not bare start points, because narrowing is
2-D: a flow-narrowed guard region (`docs/adr/narrowing-diagnostics.md`)
contributes a verdict that *ends*, which a breakpoint-only step function
cannot express.

**Why never persisting it is free.** The key is internal, so no
cross-file asker can name a `Timeline`; every `ReducerQuery` construction
used for cross-file recursion is `point: None` (each rebuilds context from
the provider's own `scopes`/`packages`), and the sole `point: Some(..)`
construction in the registry is the intra-bag scope-chain walk. The binder
is always applied at the hop — an edge entering a `Variable` fixes the
point from bake-time constants (`Expr(span).start` when chased from an
expression, `scope_point(scope)` otherwise) — so composition through a
variable selects one segment and the `Timeline` collapses into the
composing conclusion at bake time. The only consumer of an *unapplied*
timeline is `inferred_type_via_bag(var, point)` (hover / inlay /
completion on an open document, where the bag is resident anyway).
Persisting timelines would cost roughly 16–32% of the bag's size to serve
a path that never decodes.

### `Link { target, arity, receiver }`

A portable `ConclusionKey` target, an optional arity override, and a
`ReceiverRule`. It subsumes every cross-file edge hop the live chase makes:
a fresh-dispatch edge from a non-`MethodOnClass` attachment
(`receiver: Dispatch(c)`), an inheritance-hop edge between `MethodOnClass`
entries (`receiver: Thread`), and a method `Invoke` (`arity: Some(n),
receiver: Dispatch(target.class)`, with an explicit target key for a
SUPER or qualified lookup).

Example: `sub active { return $self->search({ active => 1 }) }` on a DBIC
resultset bakes `MethodOnClass{"...Users","active"}` to `Link { target:
MethodOnClass{"...Users","search"}, arity: Some(1), receiver:
Dispatch("...Users") }` — the bake collapses every internal-id hop in the
return chain and stops at the first step that needs the world.

**Evaluation.** Rebind arity/receiver per the rule, then re-enter the
ladder at the target key — a local map miss walks `PackageFacts::parents`
exactly as the live chase does. The hop count is identical to the live
chase; each hop is a map lookup instead of a bag decode plus registry
recursion.

**Where it breaks.** (a) The target must be a *static* portable key — a
receiver-dependent target (`$self->$method(@_)` past constant folding,
`ref($x)->new`) has no key to write, so the entry is `OpenNone` (these
sites also pin no edge in the live chase and count into
`dynamic_dispatch_sites`). (b) Cycles/depth mirror the registry's visited
set and depth cap. (c) The enriched-tier hop — when a provider's raw answer
dead-ends because the provider's own imports need enriching — has no
conclusion form; the fallback still decodes and enriches the whole
provider bag. `Link` makes the raw-tier hop near-free and leaves the
enriched-tier hop exactly as expensive as it is.

### `Project { base, step }`

`base: Box<Conclusion>` (recursive), `step: ProjectionStep`
(`HashKey(String)` | `ArrayIndex(i32)`). Subsumes the live chase's
`Projected` payload. Example: `sub _dbh { return $self->config->{dbh} }`
bakes to `Project { base: Link { target: MethodOnClass{"My::Worker","config"},
arity: Some(0), receiver: Dispatch("My::Worker") }, step: HashKey("dbh") }`.

**Evaluation.** Evaluate `base`, then narrow: a `Value(HashWithKeys{...})`
base answers `key_value_type("dbh")` directly; a class-typed base mints a
follow-on `SlotType{class, "dbh"}` lookup up that class's ancestry —
exactly the `SlotType` fallback ladder the live chase uses. `ArrayIndex(i)`
uses `element_at(i)`.

**Where it breaks.** A base with no structure axis (bare `HashRef`,
rep-only evidence) has no per-key types → honest `None`. A dynamic key
(`->{$k}`, not constant-folded) never emits `Projected` in the first place,
so there is nothing to bake. `ArrayIndex` on a non-`Sequence` → `None`.

### `OpenNone`

A unit, payload-free variant meaning: *this key's local derivation is
unbakeable — the bag may still answer; decode it.* Any path whose fold
consumes what the algebra above cannot express writes this — a
plugin-emitted `Custom { family, json }` payload, a `Fact` of an
unrecognized family, or a chase that hits the registry's recursion depth
cap (a truncation, not a verdict — baking it, or baking anything derived
through it, would freeze a degradation).

**Evaluation.** Decode the blob, rebuild `BagContext`, run the live
registry chase — paid per-key. Sibling keys in the same file still answer
from the map.

**Residualization does not have a population here.** The tempting move is to
store the residual — `ReturnOf(Receiver)` — instead of `OpenNone(self_only)`,
and let the consult evaluate it with its own binders. Measured against the
substrate, it converts nothing: of 4,234 self-only decodes, **zero** land on a
key that could residualize. The self-only floor is made of `Edge` witnesses
(8,695, of which 8,676 point at a local `Symbol`), and following those edges —
seen-set, depth cap 8, truncation answering "not residualizable" so the
verdict under-approximates — partitions them exactly: 97.6% floor-confirmed,
1.34% residualizable, 1.06% depth-capped. The 116 residualizable keys are
never consulted cross-file.

Two things this pins. A bake-side share is the wrong denominator for a
question about decodes — a bake tally counts KEYS, and the three denominators
here (8,643 keys baked, 4,234 decodes, 2,140 provider decodes) differ enough
that picking the wrong one mis-sizes the work. And the verdict therefore rides
the row as `NoAnswerSelfOnlyResidualizable`, measurement-bearing like every
other `OpenReason`, so the consult's existing `reason.tag()` weights it with
no new instrument. `OpenNone(self_only)` is the honest floor; the instrument
stays armed, and a workload where that row is non-zero is the one that reopens
this.

**Where it breaks.** `OpenNone` is honest but blunt: it cannot say *how
much* of the derivation was open, so one unrecognized witness on a hot
class's method subgraph makes every consult of that key pay full price.
The `OpenNone`-vs-absent boundary is where the bake's soundness lives:
mislabeling a should-be-`OpenNone` key as absent serves `None` where the
bag would have answered — silently. The conservative rule: any doubt at
bake time (unrecognized payload family, cap hit, any non-default reducer
in play) writes `OpenNone`, never absence, never a value.

### The bake

Runs post-fold, post-finalize, in the persist path beside blob encode, on
non-degraded analyses only — degraded analyses are never persisted at all,
so the map inherits that gate. The bake's registry runs with
`module_index: None`: no cross-file value can be materialized, and every
fallback that would consult the index residualizes as `Link` instead. Key
enumeration is the bag's attachment index plus symbol names plus declared
bridges. Degradation rule: a cap hit or unrecognized payload writes
`OpenNone`, never a truncated or guessed answer.

## World-level closedness

A per-class **certificate**, consulted beside the map, that turns the bake's
silence about a member from "decode to find out" into a trusted `None`.

The population is `OpenReason::AbsentNotClosed`: a class the baking file never
declared, so its map's silence carries no information — 22.8–27.6% of open
reasons and 96.8% wasted, the forced decode answering nothing in nearly every
case.

**This is the one fact in this layer whose staleness yields a wrong answer.**
Everywhere else absence costs a decode; here, trusted silence about a method
that has since come into existence is a confident lie. What discharges it is
the rule the conclusion rows already use: the certificate self-validates
against the live index, correctness never depends on an eraser, and every path
out that is not a validated certificate reads as not-closed and falls open to
exactly the decode the caller was already going to do. There is no arm where
uncertainty becomes trust.

**The validity key is one structure**: per ancestor name, the sorted
`(provider path, surface fingerprint)` list. The sorted path list *is* the
provider-set identity, which catches a new file arriving to provide an
ancestor name — the case per-provider fingerprints structurally cannot see.
The fingerprints *are* the per-provider half, catching edits to providers
already known; a parent-list change surfaces here because `Surface` carries
parents. Both halves sit in one structure deliberately: there is no way to
record one without the other, so a later edit cannot drop the arrival case and
leave something that still looks like it validates.

**Every exclusion lives in `providers_of`**, the one path both `mint` and
`is_valid` take. An exclusion checked only at mint outlives the condition it
was minted under — a bridge arriving afterwards moves no provider and no
fingerprint, so nothing in the recorded key can see it, and only re-asking
catches it. The exclusions: a plugin bridge onto any class in the closure, a
declared-dynamic parent list, a provider the index holds no freshness record
for, and a closure wider than `MAX_CLOSURE`.

Consulted at the **consumption** site — the registry's `Decode` arm, where the
index is in hand — never where the verdict is produced, which is index-free
inside the bake. Maps stay index-free and both EQUIV disciplines stand
unchanged. Minting is lazy, at a consult already bound for a decode, which has
therefore already paid the ancestry walk a mint needs.

The store is byte-capped with coarse bulk-clear eviction and takes no
invalidation hooks: a re-mint is one walk the caller was about to pay anyway,
and validity is self-checked rather than maintained.

Measured on the substrate, warm: `absent_not_closed.wasted` 323 → 15, wall
flat. `PERL_LSP_CLOSED_EQUIV` runs the decode anyway on every trusted absence
and reports any answer the trust claimed could not exist — zero breaks on the
substrate and across gold. `PERL_LSP_NO_CLOSED` is the off switch and A/B
control.

## Constraints

1. **`MethodSurface::ret` (`model/surface.rs`) is not this layer.** It is a
   pre-enrichment LOCAL conclusion, and two providers with different
   enriched returns project byte-identical Surfaces. Only post-fold
   conclusions persist here, and invalidation rides the blob axis (per-file
   stamp), never Surface equality. Enriched answers are never written back.
2. **Hops cheaper, never fewer.** `Link` preserves every hop; the map never
   holds a materialized cross-file value. An answer still moves when a
   provider resolves.
3. **The invalidation gate is DERIVED, not remembered** (see below): a
   reducer change alters semantics without changing shape, so a hand-bumped
   version cannot be trusted to catch it.
4. **Never bake through a degradation** — a recursion-depth cap hit, a
   `degraded` analysis, a truncated chase: `OpenNone` or nothing.
5. **Unserved residue, permanent.** Rename transport and `--dump-package`
   consume the bag as a data structure, not through the registry — the
   honest claim is "the type chase never decodes the bag", never "nothing
   does". `Custom` payloads and non-default reducers are unbakeable by
   construction (the plugin-fingerprint hard-clear must cover the map). The
   enriched-overlay retry still decodes and enriches whole provider bags.

## Invalidation is structural, not remembered

A baked conclusion introduces a staleness class the bag does not have: a
reducer edit changes what the right answer IS while the stored bytes stay
well-formed. A version bump maintained by discipline is the failure shape
this codebase keeps paying for, so the gate is derived instead: `build.rs`
hashes the derivation sources (`model/witnesses/**` plus the `InferredType`
methods, `conventions.rs`, and the framework tables it calls into — the
boundary is fuzzy, so the whole source tree is hashed; over-invalidation is
the safe direction and nearly free, because the conclusion fingerprint is
independent of `EXTRACT_VERSION` — a source change drops the conclusion
column and keeps the blobs, so the next run re-bakes by decoding blobs it
already has) into a compile-time constant, and the cache checks it exactly
as it already checks the plugin fingerprint over `.rhai` files. The sibling
gates (`REF_ROWS_VERSION`, `STUB_VERSION`) stay hand-bumped because a shape
change breaks decode loudly; this gate protects against a change that does
not.

**Precondition: the fold must not depend on map iteration order.** A
fingerprint is worthless if the baked answer can differ from the live
answer on identical code purely from hash-map iteration order.
`witnesses_tests::the_fold_does_not_depend_on_map_iteration_order` and
`conclusions_tests::the_bake_does_not_depend_on_map_iteration_order` pin
this: they lean on `RandomState` seeding per instance (two independently
built analyses of one source carry differently ordered maps), each carries
a vacuity guard that fails loudly if seeding ever stops varying, and both
assertions are mutation-verified (making the fold order-dependent, and
making seeding invariant, each fail the test by name).

## Absence must not mean an answer

The three-way lookup — a form, `OpenNone`, or absent — makes "absent" a
definite `None`, which is sound only while key enumeration is complete.
Making absent mean "decode the bag" removes that precondition: a definite
negative is then only ever an explicitly stored one, and an unenumerated
key costs a decode instead of returning a wrong answer. When the
enumeration is right this costs nothing; when it is wrong the layer is
slow instead of incorrect, which is the trade to take.

## Sizing

The map lands between the flat one-return-per-sub table and the full bag,
estimated at low single digits of MB per file — a materially smaller
resident cost than the bag it partially evaluates, because the bake keeps
only the point-free, cross-file-enterable projection of the chase.

## The third absence verdict: `Outcome::NotLocal`

Absence of a key is not one fact — it splits by what the bake can prove
about the *class*, not the key:

| class | verdict | licence |
|---|---|---|
| closed (every ancestor declared here) | `None` | absence is a proven no-answer |
| enumerated but not closed | `NotLocal` | skip THIS candidate, ladder continues |
| never declared here | `Decode` | the bake never looked |

The dominant `OpenNone` population decodes a file only to discover the
method isn't there and walk to a parent — work a per-class fact already
settles. `ConclusionMap` carries each enumerated class's declared parents
and walks them before judging an absence (depth-capped, not
cycle-guarded: a declared-parent chain inside one file is short, so the
walk doesn't pay for a `HashSet` allocation on a path taken tens of
thousands of times per check).

**A verdict about a key cannot be validated against a chase about a
class.** The correct equivalence check compares `NotLocal` against an
INDEX-LESS chase of the same file (the context the bake actually ran
under) — comparing against the full cross-file chase instead reports
every correctly-skipped candidate as a failure, because the fuller
chase's parent walk finds an answer `NotLocal` was never claiming to
have.

**`NotLocal` must not short-circuit to a `Follow` at the class's
parents.** A reopened package's method can live in a *later* candidate
of the same name (e.g. a subclass reopening a base class's package) —
jumping straight to the parent walk on the first `NotLocal` verdict skips
those candidates. The ladder continues to the next candidate; only when
every candidate answers `None`/`NotLocal` does the parent walk run.

## Widening `Link`: rejected

`OpenNone` splits by cause (absent-but-not-closed, chase-was-opaque,
chase-named-a-linkable-rung, self-rung-only, binder-dependent). Only the
linkable-rung share is addressable by a `Link` that carries binders
(arity/receiver) through a call or qualified-call frame — and even that
share mostly abandons at the first downstream rung whose own verdict is
still `Decode`, because `OpenNone` dominates the rungs a `Link` would
land on. **Do not build binder-carrying `Link` residuals** — the
per-class absence work above addresses the population that actually
decodes; widening `Link`'s shape pays for a population that wouldn't
have completed anyway.

A `Link` chase must also treat certain sub-chase frames as opaque rather
than recordable rungs, or it launders a transformed answer into a false
`Link`: any frame that substitutes a different receiver or arity
(an `Invoke`, a re-dispatched `Edge`), folds
across sibling witnesses at one attachment, drills a value out of a
sub-chase's answer (`Projected`), or exhausts the depth cap. The key the
chase is currently baking is filtered from its own candidate list for
the same reason — recording it as a rung converts nearly the whole
`OpenNone` population into `Link`s that burn a hop and still abandon.

## Change propagation: the flush worklist

An edit to file C can move the ANSWERS a downstream consumer B's map
resolves to, without changing a single byte of B's own map — B's map is
index-free by construction, so its content is unaffected while what it
*evaluates to* (chased through C) has moved. Cutting propagation on map
equality would stop the wave at B and starve B's own consumers. The flush
driver therefore diffs the **evaluated surface** — a conclusion's answer
in the world being built — never the persisted map, and only a
multi-file chain distinguishes the two (a two-file fixture passes either
way).

The driver processes one dirty frontier per round against a FROZEN
generation, propagating until evaluated surfaces stop moving, then
publishes the next generation atomically:

- **Cycle termination.** A file revisited within one flush compares
  against the surface recorded EARLIER IN THAT FLUSH, not against the
  frozen baseline every time — comparing against the baseline on every
  visit would run any cycle until the round cap, since an unchanged
  re-derivation would never register as "no change from last visit."
- **Round cap (`MAX_FLUSH_ROUNDS = 32`).** The same role as the witness
  fold's `MAX_FOLD_ITERATIONS`: convergence is a property of the lattice,
  not of this number, and hitting the cap means a bug, not a deep
  dependency chain (real workspace chains are single digits deep). A
  non-convergent flush is abandoned rather than published — a
  half-propagated generation is worse than none, because a consult
  pinned to it composes answers from a wave that never finished.
- **`enqueued` vs. `changed`.** A consumer whose own conclusion answers
  didn't move can still need to re-dispatch, because dispatch targets
  resolve through the index rather than off a stored surface — so the
  driver tracks every file the wave touched (`enqueued`) separately from
  every file whose surface actually moved (`changed`); marking only the
  latter would silently skip the former.
- **Deduplication per round.** A file reached by three consumers in one
  round is one re-bake, not three — a wide fan-in must not multiply a
  round's cost by its width.

**Reader isolation.** The store is generation-stamped: a reader pins a
generation for its whole consult, so it never sees a half-built next
generation and cannot compose an answer out of two different worlds. A
round's writes land in one transaction, so a crash mid-flush leaves the
prior generation intact rather than a mixture. A missing publish (no
seeds, a non-convergent wave, a failed transaction) is always safe — the
store keeps the previous generation and a consult falls back to a
decode.

**Two independent staleness axes.** The blob stamp alone is not enough:
a conclusion also goes stale when the *derivation* that produced it
changes (a reducer edit), which leaves the bytes valid and the meaning
wrong — that is `validate_conclusion_fingerprint`'s job (see
Invalidation, above), and it clears conclusions while keeping blobs,
because the repair is a re-bake and a re-bake wants the blob.

## Parked

- **Bridge-exit poisoning is sound but far too pessimistic.** A chase
  that could exit through a plugin bridge is marked unbakeable at every
  file, but a bridge only actually yields an answer roughly 1.7% of the
  time it's consulted, concentrated in ~13 well-known app-surface
  classes (Mojo/Minion). Making bridge-existence knowable at bake time
  (an index-side set of classes any file bridges to, or the same fact
  recorded per class and consulted where the index is present) would
  let the exit poison only when it could really find something. Small
  and well-sized (recovers on the order of 2,000 consults concentrated
  in a handful of classes) but not built.
- **Verb-declared partial enrichment** (`EnrichmentProfile`, e.g.
  `--check`'s profile omitting the `MethodCall` dispatch-target
  re-stamp) is landed for the CLI, where one process serves exactly one
  verb and the overlay is resident, not persisted, so a partial copy
  cannot outlive the process. Extending it to a server verb needs the
  profile folded into the shared `enriched_snapshot` cache key — a
  profile-blind key would let a partial copy leak to a fuller-reading
  verb as a silently missing answer. Not built; the reason is recorded
  so its absence doesn't read as an oversight. `PERL_LSP_FULL_ENRICHMENT`
  forces the full profile back everywhere, and is the load-bearing
  control for measuring the partial profile at all.
