# The CFG tier — sparse guarded value-flow on the witness bag

**Status: design brief, not landed.** The path-sensitivity tier the
diagnostics arc is parked on: `docs/adr/use-after-move.md` ships a
decidable subset *because* "we don't have a CFG"; the cpp D-codes
(`docs/adr/narrowing-diagnostics.md` §C/C++ applicability) are blocked
on a nullability layer "along cpp control flow"; D9 needs a
reachability pass that doesn't exist. This doc is the design for that
tier: what it is, what new entities it needs, and — load-bearing —
what it does NOT add (a second engine).

Prereq reading: `docs/adr/flow-narrowing.md`, `docs/adr/use-after-move.md`,
`docs/adr/narrowing-diagnostics.md`, `docs/adr/bag-canonical.md`,
`src/witnesses.rs` (attachments, `query_rec`), `src/builder/narrowing.rs`
(`NarrowSubject`), `file_analysis.rs::FlowEdge` / `earliest_rebind_in`,
CLAUDE.md worklist invariants.

## 1. The two constraints that shape everything

**Spans are the program-point currency.** A narrowing is a scoped
assertion over a region; witnesses carry spans for temporal ordering;
the rebind cutoff is span arithmetic over `flow_edges`. A CFG design
that makes consumers key on block IDs introduces a second program-point
currency beside spans — the parallel-store disease. Everything below
stays span/point-keyed at the seams; no `BlockId` is ever a consumer
key.

**The bag is monotone; textbook dataflow is not.** Per-point IN/OUT
state tables with kill/gen transfer functions violate the bag's
invariants (witnesses append, nothing rewrites them). The escape is the
same one compilers found: **SSA form converts mutation into immutable
names plus structural joins.** A kill stops being "delete the nullness
fact" and becomes "a newer def is the one that reaches you" —
superseded by structure, not destroyed. SSA-shaped dataflow is monotone
and append-only, i.e. bag-native. The design below is chosen backward
from that fact.

## 2. Options considered, and the pick

- **A — region algebra++** (typed control regions, no graph): buys arm
  membership, inverted conditions, better cutoffs. Cannot buy joins,
  loops, or reachability — span algebra is a tree, control flow is a
  DAG with cycles; the second doesn't embed in the first.
- **B — dense classic CFG** (basic blocks + per-block bitvector
  dataflow): the textbook menu, but a whole second spine with per-block
  mutable state — fights both the bag invariants and the residency
  discipline. Wrong prominence.
- **C — sparse guarded value-flow** (φ nodes on the `FlowEdge` spine):
  `FlowEdge` is already the def-site record; `earliest_rebind_in` is
  already a poor-man's dominance query. Add the two things SSA adds to
  a def list — join points and guard labels on their arms — and
  dataflow runs sparse, per place, through the existing edge-chase.

**Pick: A as the persisted substrate, C as the analysis.** B's worklist
exists only as the already-landed `fold_to_fixed_point` driver; dense
blocks never persist, never exist as a resident structure.

## 3. New entities

### 3.1 `ControlRegion` (typed) — upgrades `control_regions: Vec<Span>`

`{ span, kind, condition: Option<Span>, arms: Vec<Span>, guard: Option<GuardRef> }`
with `kind` a closed enum: `If | Ternary | Loop { has_back_edge } |
Switch | PreprocIf | Catchy` (eval/try — the arm that catches unwind).
Per-language recognition (rule #1: Perl in the builder next to
`narrowing.rs`; packs via new capture vocabulary `@ctl.region` /
`@ctl.cond` / `@ctl.arm`, the `@flow` pattern). UAM's Gate C keeps
reading it kind-blind (containment still works), then gets arm-scoping
for free. Persisted, `#[serde(default)]`, `EXTRACT_VERSION` bump.

### 3.2 `ExitFact`

`{ span, kind: Return | Throw | Break(label) | Continue(label) | Goto(label) }`
per scope. Perl: `last`/`next`/`redo`, `die`; cpp: `goto` (reuse the
label-nav refs), `return`, `throw`. Return *types* already ride
`SymbolReturnArm`; the *exit* is a separate control fact. Feeds
reachability (D9) and "does this arm fall through".

### 3.3 `Place` — the promotion of `NarrowSubject`

Narrowing already found the right semantics
(`builder/narrowing.rs::NarrowSubject`): a stable place path, with
`key_vars` as an honest aliasing story (a place reached through a
dynamic key stops being trustworthy when the key rebinds). Three
promotions, all representational:

- **Spelling → binding identity.** `Variable(String)` becomes
  (name, declaring `ScopeId`) — what `resolve_variable_refs` /
  `FlowEdge.target_scope` already compute. Joins merge defs across
  sibling arm scopes; same-spelled variables in sibling scopes must
  not merge.
- **Flat key → structured path.** Root binding + `Vec<PathStep>`
  (`Field(name) | Index | Deref`), bounded depth, spelling retained for
  display. Equality was all narrowing asked; control flow also asks
  *disjointness* (the UAM subobject-move FP: moved `other`'s `Base`
  subobject, read `other.msg_type` — disjoint paths, safe). A flat
  string faking prefix queries is the lossy-string projection (rule
  #10).
- **Builder-transient → Model-layer serde entity.** Lives in
  `file_analysis.rs`; `NarrowSubject` becomes its recognition-side
  constructor. Converges the three current spellings of the same
  concept: `GuardSite.subject: String`, `moved_from`'s
  `(String, Span, ScopeId)`, `ArrowDerefSite.receiver`.

Kill vocabulary generalizes from `key_vars`: a place's governing def is
invalidated by a write to the place, a write to any base of its path,
or a write to any key-var. Narrowing already enforces the third.

### 3.4 `PredicateAtom` — the closed guard algebra

`IsDefined | IsNull | IsClass(C) | HasRep(RepKind) | Engaged | Moved |
Config(macro) | Opaque`, plus polarity, on a `Place`. Normalizes what
`GuardSite` half-stores so a predicate can label a φ arm and be met at
a join without re-parsing what the guard meant. Closed and tiny (rule
#10: consumers ask the atom, never the syntax). `Opaque` is the honest
arm for unrecognized guards. `Config(macro)` is the superposition
bridge: cpp `preproc_if` arms are control regions whose guard is a
config atom the 3-valued reachability machinery already evaluates —
dataflow verdicts become config-qualified ("null deref when
`PERL_DEBUG` is off"), a checker class no built-config-only incumbent
has.

### 3.5 The φ — `Join { place, at }` attachment + `JoinFold` reducer

**The bag already contains one φ:** `BranchArm(Span)` is a hand-built
join for the one control join Perl spells as an expression (ternary
arms → per-arm collector → `BranchArmFold` agrees them). The general φ
is `BranchArmFold` grown up: arms are statement regions, **bypass
edges**, and **back edges** — joins with no owning expression node —
so an assembler pass mints them from `ControlRegion` × `FlowEdge`,
keyed on (place, join point), each arm an edge to a def-or-previous-φ
labeled with the arm's `PredicateAtom`.

Why the existing machinery can't fake it:

- **The bypass path.** `my $x = "s"; if ($c) { $x = 5; } f($x);` — the
  bag holds Str@decl and Int@assign; latest-wins answers Int; on the
  ¬c path `$x` is Str. The missing fact — the old value flows *around*
  the if — has no token to hang a witness on. Region-bounding can't
  express "executions that took the then-arm".
- **The back edge.** A loop-head read sees the previous iteration's
  textually-later write; "skip witnesses past the query point" is an
  approximation of dominance that is false across a back edge.
- **Why not plain `Edge` witnesses:** an Edge asserts identity,
  unconditionally, and multiple edges on one attachment merge by
  *reducer policy* (latest/narrowest/priority) — control-flow-blind. A
  φ asserts "exactly one of these, selected by which path executed";
  the arm-set and selection structure ARE the payload. Flatten it to
  plain edges and the path information is destroyed.

`JoinFold` claims the attachment shape (the `BranchArmFold` /
`SymbolReturnArmFold` pattern), folds arms by lattice join, refines
each arm by its guard atom — single-subject path sensitivity, no path
enumeration. Diagnostics ask a thin entry point
(`place_state_at(place, point)`, the `inferred_type_via_bag` shape)
that resolves the governing def-or-φ and queries the registry.

Representation choice: v1 keeps φs **build-transient** (minted in the
assembler, folded during the same build; only conclusions persist as
region-bounded witnesses under a `cfg_flow` source tag). Promote to a
persisted `FileAnalysis` row only when a query-time consumer (hover
provenance: "possibly-null because the else-arm never assigned")
demands the chain — residency discipline says don't persist a graph
nobody rehydrates.

### 3.6 The one shared-machinery change: explicit cycle-cuts

`query_rec`'s visited guard resolves an on-path revisit to nothing —
the cyclic arm **vanishes from the fold**. Harmless for types (a cyclic
edge carries no info). Wrong in the dangerous polarity for must/may
facts: a loop-head φ whose back-edge arm is silently dropped folds over
the remaining arm and answers "must be the init value" / "must not
moved" — a *stronger* claim than the paths justify, i.e. the mechanism
for a manufactured false positive. And since the edge chase resolves
edges into synthetic witnesses before reducers see the list, a reducer
cannot distinguish "arm resolved to nothing" from "arm was cycle-cut".

Fix: a cycle-cut leaves an **explicit unknown-marker witness** in the
materialized list instead of dropping silently. `JoinFold` folds the
marker as ⊤ ("a path exists whose value I cannot name" → silence).
Type reducers ignore the marker and behave exactly as today. This is
the single point where the tier touches shared engine semantics rather
than adding beside it.

### 3.7 Verdict outputs

Solver conclusions that persist: region-bounded `Variable`/`Place`
witnesses (`Builder("cfg_flow")`, clear-and-emit) — the existing D1–D6
seams and `inferred_type_via_bag` consume them with **zero** consumer
changes, which is the strongest property of the design. Plus
`unreachable_regions: Vec<Span>` (D9, dead-arm diagnostics) as a plain
`FileAnalysis` row.

## 4. "The solver" is not a component

The word names a phase boundary, not a thing. The tier decomposes onto
existing patterns, all under the existing driver:

1. **Assembler** — build pass minting φ arms + guard labels from
   `ControlRegion` × `FlowEdge` (`populate_witness_bag`'s sibling;
   re-derives per fold iteration under a `cfg_join` clear-and-emit
   tag, since guards reference types that converge).
2. **`JoinFold`** — a reducer, registered like any other.
3. **Effect application** — build pass in the fold orbit
   (`propagate_call_bindings_to_constraints` is the precedent: it
   already pushes call-site-derived facts per iteration).
4. **Iteration** — `fold_to_fixed_point`, unchanged. The loop-φ
   recurrence reaches fixpoint the way every lattice fact does.

Precision ladder that falls out: **v1 needs no fixpoint at all** — a
single reducer chase with cycle-cut-as-unknown reproduces today's
honesty on loops (loop-carried → unknown → silence; what UAM Gate C
does by blunter means) while gaining correct branch joins and the
bypass path. v2 lets fold iterations refine back-edge arms toward the
real fixpoint — riding the existing driver. Never a new engine.

Backward-direction questions (liveness, dead stores) don't chase
naturally (edges point value←source); `resolves_to`'s def→uses index
answers them as index scans, not reducer work.

## 5. Call flow: effects

**Effects, not summary-values.** A callee fact splits in two: *return-
value facts* (Optional-ness of the return — already the type lattice's
job) and *effects proper* — what the call does to caller state
(`Moves(param)`, `Derefs(param)`, `Sets(place-from-param, atom)`,
`Kills(place-from-param)`, `MayThrow`/`MustThrow`). An effect is a
transfer-function fragment consumed by the flow passes at a **call
site**, not a value folded by a reducer at a query. The codebase
already has one effect in disguise: `WitnessPayload::ReturnExpr(Receiver)`
— a payload that is a function of the call site, applied by
substitution (`UnionOnArgs`/arity likewise). Parameter-directed effects
extend that family.

Decisions:

- **Transport**: effect witnesses on `Symbol(sid)` — same serde rails,
  same cross-file transport (`MethodOnClass` edges, enrichment
  overlay). No new store. Their only reader is the effect-application
  pass; no existing reducer ever folds them.
- **Closed, unconditional vocabulary, v1.** Conditional effects
  ("derefs param 0 iff param 1 nonzero") are where effect systems
  explode. `Opaque` is the escape hatch.
- **`Opaque` = havoc on everything reachable from the arguments.**
  Note the polarity: havoc *suppresses* diagnostics (all bets off →
  silence). The zero-FP discipline is preserved by the default, not by
  vigilance. Perl's `AUTOLOAD` / string `eval` / `goto &sub`, cpp fn
  pointers → `Opaque`, honestly silent.
- **Dispatch is a join over the CandidateSet.** The effect of a method
  call is the lattice join of the candidates' effects; an open set
  (unresolvable receiver, `$obj->$m` unfolded) joins `Opaque`. Policy
  on the application step, keyed by what resolution answers — never a
  per-call-shape branch.
- **SCC ordering.** Summaries compute bottom-up over call-graph SCCs,
  cycles iterated to fixpoint (finite vocabulary; MRO's depth-cap
  precedent as backstop). Intra-file: one more pass in
  `fold_to_fixed_point`'s orbit. Cross-file: the resolver thread.
- **Summaries join the Surface — load-bearing.** An effect summary is
  a cross-file-visible fact: if `Callee::f` gains `Moves(param)`,
  every caller's flow verdicts are stale. That invalidation machinery
  is `Surface` equality + `FreshnessIndex::dirty_consumers`; effects
  become a Surface field **with an equality-net arm** (R1 applies). A
  body edit that doesn't change effects fans out to nobody. Bypassing
  Surface means rebuild-the-world or serve-stale — both disqualifying.
- **Unwind flow.** `MayThrow`/`MustThrow` effects + the `Catchy`
  region kind: the join after a guarded region gains an incoming arm
  from every may-throw call inside it. Payoffs: D9 after a must-throw
  call (`croak` then code), and honest suppression of undef-deref
  inside `eval { }` where the deref IS the check.
- **No context sensitivity v1.** The two cheap forms that matter
  already exist: arity-keyed returns (`UnionOnArgs`) and
  receiver-keyed dispatch (`MethodOnClass`).

### 5.1 `CallBinding` upgrade

Applying `Moves(param)` at a site needs "which caller expression — and
which caller *place* — feeds that parameter": the existing
`call_bindings` upgraded to carry, per argument, its span and
`Option<Place>` (`f($x)` binds a place; `f($x + 1)` binds only a
value — effects on the latter are vacuous). This is where each
language's calling convention is encoded once. cpp by-ref params make
every callee write a caller-place effect; Perl's `@_` aliasing is the
same trap in older clothes.

### 5.2 ⚠ OPEN HOLE: parameter identity for dependent effects

**Deliberately not designed here.** `Moves(param)` needs a way to NAME
a parameter that survives every language's calling convention, and the
conventions disagree about what a parameter even is:

- **Positional index** — cpp's natural key; but overloads mean the
  index is per-signature (per-`SymbolId`? per arity-family?), and
  default arguments make trailing indices optional at the site.
- **The invocant** — Perl's `my ($self) = @_` / shift, Python's
  explicit `self`, cpp's implicit `this`. Is the receiver "param 0" or
  its own axis? (`emit_return_fuel`'s `implicit_this_members`
  capability and `conventions.rs::is_conventional_invocant_scalar`
  already model invocant-ness — the effect key must agree with them,
  not re-derive.)
- **Perl `@_` flattening + aliasing** — there are no declared
  positions; the builder infers params from unpacking idioms, and `@_`
  elements alias caller storage, so an effect on `$_[0]` IS an effect
  on the caller's argument place.
- **Named/kwargs** — Python kwargs and Perl fat-comma pair-lists are
  *key*-identified, not position-identified (and the fat-comma rule
  applies: the pair walk is separator-agnostic). R adds partial name
  matching.
- **Unpacking at the boundary** — destructuring binds (`my ($a, @rest)
  = @_`, Python tuple params) mean one caller argument can feed a
  projected slice of a formal (the `Extraction` vocabulary —
  `Positional`/`Slurpy`/`KeyOf` — already models this direction for
  flow edges; the effect key likely wants the same steps).

The open question: is the effect's parameter key an ordinal, a name,
an invocant-tag, or a per-language `ParamKey` the pack defines with a
core-generic binding step (the `cursor_slot` shape: one vocabulary,
per-language recognition)? Requirements any answer must satisfy:
authored once per callee, applied at call sites of any spelling; stable
enough to ride Surface equality without spurious dirtying; degrades to
`Opaque` when the binding can't be proven. Whoever picks this up:
survey how the four packs' `CallBinding`s actually bind before choosing
— the key must be derivable on BOTH ends (decl side for the summary,
call side for application) in every language served.

## 6. Sequencing

1. **Typed `ControlRegion` + `ExitFact`** — D9 reachability, UAM
   class-3 arm-scoping, `unless`/`until` correctness. No φ, no solver.
2. **`Place` promotion** — UAM class-2 (subobject moves), unified
   subjects. Mechanical, wide.
3. **Assembler + `JoinFold` + cycle-cut markers + nullability atoms** —
   cpp D1/D2/D6 along real control flow, must/may UAM. v1: no
   fixpoint; v2: loop precision via the existing driver.
4. **Effects + `CallBinding` upgrade + Surface membership** —
   interprocedural. Own arc; the §5.2 hole gates its design round.

## 7. Consumers this is built for (the automation tier)

Kept here so the entities serve them; none of this is engine work.
**Finding fingerprints** keyed on (rule, function symbol, `Place`
path) — never line numbers; baselines + inline suppression with
reasons ride them (the gold harness's gold/xfail/provisional taxonomy
is the same lifecycle idea). **must/may from the solver** surfaces as
a structured confidence field: must gates CI, may informs review —
the zero-FP discipline and a Klocwork-style recall mode become one
engine at two thresholds, and the finding says which it is. **SARIF**
output over the existing diagnostics seams (highest leverage-to-effort
item on the list). **Differential CI** is already built: Surface
equality + `dirty_consumers` + a persisted `modules.db` cache artifact
make PR analysis cost proportional to blast radius by construction —
once summaries are Surface fields (§5). **Determinism** graduates to
contractual (two runs → byte-identical output; the RandomState hunt
already paid for it). Cyclomatic complexity is `ControlRegion`
arithmetic; dead code/exports already ship in `--heatmap`.
