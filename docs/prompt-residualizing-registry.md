# Design brief: a residualizing mode for the reducer registry

**Status: in flight.** The question section below is the original brief.
The first **Design answer** settles the five sub-questions and stages the
work; slices 0 and 1 are landed (local-context bake, `Link` follow scored
by the extended checker). The **Slice 0 measurement** found the bridge
poison rate fatal-as-designed, and **Design answer, round 2** resolves it:
the bridge arm becomes a consult-side guard on the existing class-keyed
bridge map — never bake-time knowledge. The conclusion layer
(`docs/prompt-conclusion-layer.md`) is built and landed; this brief is the
one thing standing between it and most of its value.

## The gap, in one number

Over a warm substrate `--check`, the consult path's conclusion lookups split:

| outcome | count | what it costs |
|---|---|---|
| `OpenNone` → decode | **91,525** | full blob decode + full chase |
| absent → proven `None` | 60,947 | nothing |
| `Link` → follow | 4 | — |
| `Value` / `ReturnOf` → answered | 1,301 | a hash lookup |

**57% of lookups land in `OpenNone`.** The layer exists to remove decodes, and
on more than half of its opportunities it declines to.

`Link` — the form whose entire purpose is "the answer is in another file" —
fires **44 times across 2,693 files**. It is, in practice, not implemented.

## Why the bake cannot mint the `Link`

The bake runs with `module_index: None`, deliberately: a materialized
cross-file value would freeze a world that can change without this file
changing (`docs/prompt-conclusion-layer.md`, "Edges, not values"). So a chase
that leaves the file returns `None` at bake time.

The bake then cannot distinguish two cases that look identical from inside:

1. **The bag genuinely has no answer.** The live path also answers `None`.
2. **The chase would have exited cross-file.** The live path has a real answer.

Today both become `OpenNone`, which is sound and expensive.

Treating them both as absent instead is unsound, and measurably so — **56
equivalence breaks per substrate check** under `PERL_LSP_CONCL_EQUIV`, all the
same shape:

```
MethodOnClass{ Log::Log4perl, get_logger }        => ClassName("Log::Log4perl::Logger")
MethodOnClass{ URI, new }                         => ClassName("URI::_foreign")
MethodOnClass{ Dist::Zilla::Role::TextTemplate, fill_in_string } => Optional(String)
MethodOnClass{ Plack::Request, uri }              => ClassName("URI")
```

Each is case 2. Each would have been served as "no answer" forever.

## Why `sole_foreign_edge` does not reach them

The existing `Link` minting looks for an attachment whose sole witness is
`Edge(MethodOnClass{class, name})` with `class` not declared locally. That
catches a direct foreign edge and nothing else — hence 44.

The cases above hold `Edge(Symbol(sid))`: a **local** symbol whose own chase
leaves the file, through its imports. Nothing local names the target, so there
is no key to point a `Link` at without re-deriving the chase — which is the
thing being avoided.

## The question for the session

**Can the registry report where it would have gone, instead of only what it
found?**

Concretely: a mode in which a chase that reaches a point requiring
`module_index` returns not `None` but something like `Residual(ConclusionKey)`
— the portable key it was about to consult. The bake stores that as `Link`;
the consult path follows it into the target file's map instead of decoding.

Sub-questions worth settling there, in rough priority:

1. **Is the exit point always nameable as a `ConclusionKey`?** The four
   observed shapes are `MethodOnClass`, which is portable. Are there exits that
   can only be named by a file-internal attachment (`Expr(span)`,
   `Expression(refidx)`)? Those must stay `OpenNone`, and knowing the ratio
   decides how much of the 57% is actually reachable.

2. **One residual, or several?** A chase may branch — several candidates, an
   inheritance fan-out. `Link` as specified holds one target. Either the form
   grows a set, or a multi-exit chase degrades to `OpenNone` and we measure how
   often that is.

3. **Where does the mode live?** A flag on `ReducerQuery`, a distinct entry
   point beside `query_rec`, or a `BagContext` whose `module_index` is a
   recording stub rather than `None`. The third is appealing — the stub records
   the key and answers `None`, so no reducer changes — but it makes the chase's
   *first* exit the recorded one, which may not be the one that would have
   answered.

4. **Termination and cost.** The bake is 592 µs/file today (~1.2% of gold
   wall). A residualizing chase does strictly more work per key than the one
   that bails at `None`. Budget before building.

5. **Does the `Link` follow need a cycle guard of its own?** The consult path
   would now traverse map-to-map across files. `VisitedKey` guards the live
   chase; the projection needs the equivalent, and `(file, key, receiver,
   arity)` is the shape the spec already names.

## Acceptance test, ready-made

`PERL_LSP_CONCL_EQUIV=1` with `PERL_LSP_ABSENT_ON_NO_ANSWER=1`.

That flag combination is currently **56 breaks**. It is exactly the population
this work must convert: every one of those is a chase that exits cross-file and
should have produced a `Link`. When the residualizing mode is right, those 56
become `Link`s and the run goes green — with the no-answer case now genuinely
meaning no answer.

Then the win is measurable as `consult.baked_open` falling from 91,525.

## What is already true, so the session does not re-derive it

- The bake is deterministic (`the_bake_does_not_depend_on_map_iteration_order`,
  mutation-verified), which the diff-propagation driver depends on.
- Absence is sound, and only because closedness is asked of the INDEX via
  `parents_of` — a per-file bake cannot establish it, because Perl packages are
  open and any file may reopen one without repeating its `@ISA`.
- End-to-end checks cannot score this work. Gold stayed 502/0 and
  `--dump-package` stayed byte-identical across 312 KB under a version with 633
  soundness breaks, because the ladder routes around a missing answer and only
  the cost differs. **Score changes here with `PERL_LSP_CONCL_EQUIV`, which
  compares at the point of the claim.**

---

# Design answer

The registry can report where it would have gone, and the recording-stub
idea is the right instinct pointed at the wrong seam. The stub fails for
the reason the brief names — it records the chase's *first* exit — but the
deeper defect is that a side effect cannot say whether the exit's answer
would have been the chase's answer. Residualization is sound exactly when
the frames between the exit and the root are **answer-preserving**, and
only the chase itself knows that. So the mode lives in the chase's state,
not behind the `CrossFileLookup` trait.

## First, a correction to the bake's starting position

`bake_one` queries with `context: None`. That withholds two different
things: the module index (deliberate — the whole design) and the file's
own local context — scopes, per-package framework facts, `package_parents`
(accidental, as far as the written rationale goes). With no context at
all, `query_rec_body`'s fallback block never runs, so the bake cannot even
walk a **local** parent chain, and some share of today's
`bake.no_bare_answer` → `OpenNone` population is methods a purely local
inheritance walk would have answered.

**Slice 0 is therefore: bake with a local-only `BagContext`** —
`module_index: None`, everything else real (the persist path holds the
whole analysis, so scopes/packages/parents are in hand). This is
independently useful (converts some `OpenNone` to `Value`/`ReturnOf`
before any residualization exists), and it is a precondition for the
residual design: with local work exhausted inside the chase, the *only*
unreachable arms left are genuine index consults — which makes "where
would the chase have gone" a well-posed question with a small answer.

## The mechanism: a residual accumulator on `QueryState`, gated by an accessor

Add to `QueryState` (bake mode only, flagged on the context):

```
residual: Vec<ResidualExit>,   // ordered — ladder order is MRO order
poisoned: bool,
```

and make `BagContext::module_index` reachable only through one accessor:

```
fn consult_index(&self, would_ask: Option<ConclusionKey>) -> Option<&dyn CrossFileLookup>
```

Live mode: returns the index, argument ignored. Bake mode: returns `None`
and either records `would_ask` (nameable exit) or sets `poisoned`
(caller passed `None` because its consult has no portable name). This is
the structural enforcement, same shape as `ReceiverGated`'s
resolve-or-nothing: **a future index-consulting site cannot silently
bypass residualization**, because reaching the index requires declaring
what you would ask it — and a site that forgets to exist at all cannot
compile, because the field is private. Without this, a new consult site
added next year returns `None` un-recorded, the bake reads the empty
residual as a genuine no-answer, and — once no-answer means absent — a
silent wrong `None` ships. The accessor turns that failure mode from
silent-unsound into poisoned-thus-slow. A layering test pins that no
direct field access exists outside the accessor.

Classifying today's five consult sites:

| site | nameable? | exit |
|---|---|---|
| `PackageSymbol` primary (`visible_def_candidates`) | yes | `MethodOnClass{pkg, name}` |
| foreign parent hop (parent name declared locally, class not) | yes | `MethodOnClass{parent, name}` |
| `SlotType` primary + ancestry | yes | `SlotType{class, key}` |
| `TypeName` cross-file alias | yes | `TypeName(name)` |
| plugin-namespace bridges (`for_each_entity_bridged_to`) | **no** — per-file `SymbolId` | poison |
| dispatch-receiver subclass test (`fresh_dispatch_receiver`) | **no** — a predicate, not a key | poison |

Note the parent case: the parent's *name* is local knowledge
(`PackageFacts.parents`), so the exit is nameable even though the parent's
file is not reachable. The one cross-file fact the bake cannot see is a
parent added by another file reopening the package — and that is already
the closedness problem, solved on the consult side by asking the index's
`parents_of`. Residualization inherits that solution unchanged: the baked
`Link` covers the declared chain, and the consult-side closedness check
covers the reopened tail, exactly as it does for absence today.

## The soundness rule: clean ladders link, transforming frames poison

The chase's result is `Link`-able only when every frame between the exit
and the root would return the exit's answer verbatim. Two frame classes:

**Ladder frames are answer-preserving.** First-non-`None` wins: the
candidate loop, the DFS-MRO parent walk, the reducer sequence when
exactly one arm is live. Index-absent, the arms before the exit answered
`None`; index-present, the exit answers and the ladder returns it
unchanged; arms after the exit are skipped in both worlds. And if an
earlier arm could have answered differently with the index, it would
itself have recorded an exit — making the residual non-singleton, which
is detected. So: **a chase returning `None` with `poisoned == false` and
an ordered residual list whose entries are all ladder-positioned is a
sound `Link`.**

**Combining frames poison.** `materialize` splicing a chased edge's
answer into a witness list that has other members; `SymbolReturnArmFold`
agreeing across multiple arms; `BranchArmFold`; anything that folds. If a
sub-chase carried a residual and the frame would have *combined* its
answer rather than returned it, set `poisoned` — the conclusion stays
`OpenNone`. Implementation is local: a frame snapshots
`state.residual.len()` before recursing; if the count grew and the frame
is not a pure ladder position (other live witnesses/arms at that
attachment), poison. Start conservative — the clean set is sole-edge
chains plus the two ladder walks — and widen only against measurement.

This rule is what the stub could never express, and it is also why the 56
observed breaks are the easy majority: every one is a sole-edge chain
(`PackageSymbol → Edge(Symbol) → return arm → Edge(Expr) → foreign
edge`), ladder all the way down.

## `Link` grows an ordered fan-out (sub-question 2)

```
Link { targets: Vec<(ConclusionKey, Option<u32>, ReceiverRule)> }
```

first-answer-wins. This is not a generalization for generality's sake:
Perl's DFS-MRO **is** an ordered ladder, so a class whose method resolves
somewhere up a multi-parent chain residualizes as the ordered list of
foreign parent exits, and first-answer-wins is exactly the live
semantics. A singleton stays the common case and the serialized form.
Multi-exit shapes that are *not* ladder-ordered (a fold over branches)
are already poisoned by the rule above — the vector never holds them.

## The `Follow` machinery (sub-question 5)

`consult.baked_follow_unhandled` gets its implementation regardless of
how wide `Link` minting becomes:

- Cycle guard: a visited set of `(path, ConclusionKey, receiver_key,
  arity)` carried per top-level consult — the spec's shape, mirroring
  `VisitedKey` — plus a hop cap sharing `QUERY_REC_DEPTH_CAP`'s spirit.
  Map-to-map traversal has real cycles (mutual imports) and the guard is
  what makes them terminate at `None` instead of recursing.
- A `Follow` landing on a target key that is `OpenNone` in the target's
  map decodes **the target file only** — one decode, at the file that
  actually holds the open derivation, which is strictly no worse than
  today's decode-at-the-origin.
- A `Follow` landing on an *absent* target key applies the target map's
  own absence rule (closedness and all) — no special case.
- Budget: `Follow` hops are consults; they spend the session's existing
  consult budget so a pathological link chain degrades through the same
  honest channel as everything else.

## The instrument comes first, twice over

**`PERL_LSP_CONCL_EQUIV` must learn to score `Follow` before `Link`
widens.** A wrong absence costs a decode; a wrong `Link` serves a wrong
*answer* — it is the first conclusion form whose failure is unsound
rather than slow. The flag's contract extends naturally: under it, a
followed answer also runs the real chase and any disagreement is an
equivalence break. The arc's own lesson applies to the arc: end-to-end
checks stayed green under 633 breaks, so the checker compares at the
point of the claim, and the claim here is "the exit's answer is the
chase's answer".

**And measure the denominator before building the machinery.** Slice 0's
accessor gives residual classification nearly free: counters
(`residual.nameable`, `residual.poisoned`, `residual.multi_nonladder`)
over one substrate `--check` say how much of the 91,525 is actually
reachable by clean-singleton + ladder `Link`s. If the nameable share is
small, stop after slice 1 and this document records why. The brief's
sub-question 1 is answered by that run, not by argument — the four
observed break shapes are nameable, but 56 breaks are the *unsound*
population, not the 91,525 `OpenNone` population, and only the counter
sees the second one.

## Staging

1. **Slice 0** — local-only `BagContext` at bake; the `consult_index`
   accessor with recording counters; no behavior change to conclusions.
   Gates: bake still deterministic (the seeded-map test), bake µs/file
   re-measured (the residualizing chase does strictly more work than
   bail-at-`None`; today's 592 µs/file and ~1.2% of gold wall are the
   baseline, and the budget question is settled here, before anything is
   built on it).
2. **Slice 1** — `Follow` implementation with guard + budget;
   `PERL_LSP_CONCL_EQUIV` extended to score followed answers. Gate: the
   existing 44 `Link`s served through the new path, equivalence clean.
3. **Slice 2** — residual-to-`Link` minting for clean singletons. Gate:
   `PERL_LSP_CONCL_EQUIV=1 PERL_LSP_ABSENT_ON_NO_ANSWER=1` goes 56 → 0,
   `consult.baked_open` falls measurably from 91,525.
4. **Slice 3** — ordered multi-target `Link` for the parent ladder; then
   flip `ABSENT_ON_NO_ANSWER` to default. That flip is the prize: 10,577
   no-answer bakes per substrate move from `OpenNone`-decode to trusted
   absence, and no-answer finally means no answer.

## Why this matters beyond the decode count

A `Link` is the conclusion layer's only *stable* representation of
cross-file dependence: when a provider's return type changes, a consumer
whose map says `Link{provider, key}` needs **no re-bake** — its map is
byte-identical, its conclusion *diff is empty*, and the changed answer
flows at `Follow` time. Under the diff-propagation driver
(`docs/prompt-enrichment-alternatives.md`), that is the difference
between a provider edit re-baking its whole consumer cone and re-baking
nothing: residualization is what keeps the generational worklist's
frontiers small. `OpenNone` hides the same dependence inside an opaque
decode, where the diff cannot see it. The 91,525 number is the decode
cost today; the same population is the propagation cost tomorrow, and
`Link` retires both.

---

## Slice 0 measurement: the bridge poison is sound and nearly always vacuous

Instrumented per the design answer's sub-question 1 (`residual.nameable` /
`residual.poisoned` / per-site), over one substrate `--check`.

| exit site | count | nameable? |
|---|---|---|
| `moc_primary` | 46,590 | yes |
| `parent_walk` | 46,590 | yes |
| `bridge` | 46,572 | **no — poisons** |
| `slot_type` | 533 | yes |

Read per EXIT that is 66.8% nameable. Read per CHASE it is far worse, and the
per-chase reading is the one that governs: the three big sites are sequential
fallbacks of the SAME chase (primary → parents → bridges), so a chase that ends
with no answer has hit all three, and one poisoned exit poisons the chase.
46,572 of 46,590 — **99.96% of chases touch the poisoning site.**

That would have ended this line of work. It is also wrong, and the thing that
makes it wrong is not visible from the bake.

**In LIVE mode, the bridge consult yields nothing 131,658 times against 2,251
that yield — it is vacuous 98.3% of the time.** A would-be consult that would
have returned nothing is not a dependence, and counting it as one makes the
poison rate look total when the real one is ~1.7%.

So the bake-time rule "a bridge exit poisons" is SOUND but pessimistic by a
factor of ~59, and the pessimism costs essentially the whole reachable
population. The bake cannot currently do better, because whether any file
bridges to class C is index-side knowledge and the bake has no index — by
design.

**Proposed refinement to the staging.** Make bridge-existence knowable at bake
time, so the exit poisons only when it would really have found something:

- an index-side set of classes that ANY file bridges to, consulted at bake —
  cheap to build (the bridge registry already exists for
  `for_each_entity_bridged_to`), and a set membership test rather than a walk;
- or the same fact recorded per class in the map, decided consult-side where
  the index is present — the shape the closedness check already uses, and it
  has the same "the property is global, not per-file" character that made
  closedness wrong to compute locally.

Either way the measurement to re-run afterwards is the per-chase poison rate,
not the per-exit ratio. **Whoever picks this up should not size the work from
the 66.8%.**

---

# Design answer, round 2: the bridge arm becomes a consult-side guard

Of the two refinements above, take the second — and take it in a stronger
form than either as written: **the bake learns nothing about bridges, the
map stores nothing about them, and the consult side guards every trust
decision with a live O(1) test.**

## Why the bake-time set is closedness attempt 2 again

Consulting an index-side bridged-to set at bake bakes a **negative global
fact** — "no file bridges to C" — into the map. The map's invalidation
covers exactly two things: the derivation code (the `build.rs`
fingerprint) and the file's own stamp. Foreign registry state is covered
by neither, by design. So when a newly indexed file (or a new `.rhai`)
starts bridging to C, nothing about the consumer file changed, no re-bake
fires, and its map serves a `Link` that skips the bridged answer —
durably, not per-file. That is the closedness mistake ("a global property
computed from one file's view") in baked form, and it would also be the
bake's first index read, breaking the invariant every soundness argument
in this layer leans on. The staleness has no cheap fix: covering it means
a new invalidation axis (bridge-set changes → every map baked under the
old set), which is heavy machinery guarding a rare event.

The consult side is closedness attempt 3, verbatim: the property is
global, so ask it where the global union lives, at query time, where the
index is present. It is self-healing in both directions with zero new
invalidation machinery — a new bridge makes the guard fail → decode →
right answer; a removed bridge makes it pass → trust.

## The guard is already built

`IndexCore.edges.bridges` is a class-keyed `DashMap<String, ModuleBucket>`
(`module_index/parts.rs`), fed and purged on the one sanctioned edge
write path — it is what `for_each_entity_bridged_to` reads. The guard is
a non-empty-bucket test on that map: O(1), and NOT a new parallel reverse
index (which the plugin ADR retired a class of). Use bucket-non-empty
rather than key-exists — a purge can leave an empty bucket, and while a
false "bridged" only costs a decode (sound, slow), there is no reason to
pay it forever. The `CrossFileLookup` method carrying the predicate
follows the delegate-each discipline `ScopedLookup` already enforces for
the residency views.

## Which conclusions the guard covers — and which it need not

The live ladder is local reducers → primary → parents → **bridges**. A
baked `Value` came from an arm that beats the bridge arm in every world,
so `Value` (and `ReturnOf`) need no guard. The two forms that encode
"everything before the bridge arm said None" are exactly the ones that
must be guarded before trusting:

- **absence** (trusted `None`), and
- **`Link`** (and its coming fan-out — the ladder it encodes is
  primary → parents, with the bridge arm deliberately outside it).

With the guard in place, the bridge exit stops being a poison AND stops
being a recorded residual: it leaves the residual's obligations entirely.
The `Link` means "the primary→parents ladder"; the guard covers the third
arm globally at trust time. The per-chase poison rate falls from 99.96%
to the true per-class bridge rate.

## The guard closes a latent hole that predates residualization

Trusted absence today requires only "class has no ancestors"
(`parents_of` empty). The bridge arm runs regardless of ancestry. So a
parentless class that some file bridges to has its absence trusted RIGHT
NOW — served `None` while the live chase answers through the bridge.
`PERL_LSP_CONCL_EQUIV` shows 0 breaks on the substrate, which means the
substrate happens to contain no parentless bridged class — corpus luck,
not soundness. The guard closes that hole as a side effect, which is why
it should land ahead of any `Link` widening, not with it.

## Two measurements for whoever implements

1. **The 98.3% vacuity is per-call; the guard is per-class.** If the
   2,251 real yields concentrate in a few hot classes (Mojo app surfaces
   are the obvious suspects), those classes' conclusions stay permanently
   guarded-off — correct, but the decode cost then concentrates exactly
   where bridges are real. A per-class yield histogram sizes the follow-on
   below before anyone commits to it.
2. **Placement:** there is one `map.evaluate` call site today
   (the `PackageSymbol` primary in `registry.rs`); the guard goes there,
   for `MethodOnClass` keys only — bridges do not apply to `SlotType`
   ("slot writes are real code, not plugin entities") or `TypeName`. If a
   second call site appears, wrap it (`evaluate_guarded` taking the
   predicate) so the obligation is typed rather than remembered.

## Round 3: the poison half, made concrete against the measured breaks

Minting without the poison half produced a 40% wrong-answer rate (44
follow breaks / 65 correct follows), and all four break shapes are the
same violation: **the residual was recorded at a frame BELOW a combining
frame, so the link serves the exit's answer while the chase's answer is
the fold's.**

The `chase=None` majority is the fold-disagreement case read through
that lens: the live `SymbolReturnArmFold` sees the method's arms
disagree (hashref arm, undef arm, `$self` arm) and correctly answers
`None`; the minted link jumps past the fold to one arm's deep exit and
serves that arm's answer alone. `Email::MIME::create`
(link=parent's answer, chase=own answer) is the same violation on the
candidate ladder: the LOCAL arm answers live, but its bake-time
derivation was itself residual-bearing, so the bake saw local-`None` and
minted the parent exit — a rung the live ladder never reaches.
Receiver substitution was checked and cleared; no shape needs it.

The rule, stated implementably. Per `query_rec` frame: snapshot
`state.residual.len()` on entry; if it grew during the frame AND the
frame is not a pure ladder position, set `poisoned`. Frame
classification for this codebase:

**Clean (ladder) frames — residual may pass through:**
- the candidate loop (primary consult, first-answer-wins);
- the DFS-MRO parent walk;
- `materialize` of an attachment whose ONLY witness is the chased edge
  (the sole-edge passthrough — the chain the 56-break population lives
  on: `PackageSymbol → Edge(Symbol) → single return arm → Edge(Expr) →
  foreign edge`);
- `SymbolReturnArmFold` with exactly ONE arm (the implicit-return chain
  is a passthrough).

**Combining frames — residual inside them poisons:**
- `SymbolReturnArmFold` with MORE than one arm, even when the resolved
  arms agree: an unresolved residual arm can change the agreement
  verdict, so no link can stand in for the fold;
- `materialize` splicing a chased edge into a witness list with any
  other member — the reducers fold the splice with its siblings;
- `BranchArmFold` and `FrameworkAwareTypeFold` whenever a residual is
  raised inside them (agreement and observation-folding respectively);
- any frame with residuals recorded in MORE than one of its arms
  (non-singleton per-arm is the candidate-ladder version of the
  `Email::MIME` break: two rungs each needed the index, so no single
  rung's answer is the chase's).

The acceptance shape is two-sided, and both sides matter: with the
poison on, EQUIV follow-breaks must go to 0 **and `Link` must stay far
above 44**. If the poison also kills the sole-edge-chain population, the
clean-frame set is drawn too coarse — the 56-break shapes are the
canary in both directions. Each of the four measured break shapes should
disappear for an identifiable reason (a specific combining frame on its
path); a break that disappears without one is the checker being dodged,
not the rule working.

One induction worth writing down: even a clean singleton link asserts
only "the chase's answer IS the exit's answer". The follow evaluates the
exit against the TARGET's map, while the live chase evaluates it against
the target's bag and index — those agree exactly when the target map is
itself sound for that key. Link soundness is therefore inductive on map
soundness, and `PERL_LSP_CONCL_EQUIV` is the induction check; there is
no additional mechanism to build, but a follow-break must be triaged as
"my frame rule" vs "the target's map" before either is blamed.

## Round 4 — measured outcome: sound, and parked

Both halves landed (recording `f337fc7`, poisoning `699488d8` — an
opaque-frame counter, nesting, with a memo companion bit so a subtree
first reached transparently cannot launder itself into a rung when
re-reached from inside a combining frame). Follow breaks went 44 → 0;
the 8 residual disagreements are cycle-guard artifacts, classified and
reproducible.

**And the feature buys nothing at today's map composition.** Decodes do
not move (4,103 → 4,104): `follow_one` abandons at the first rung whose
target map says `Decode`, and with ~84k `OpenNone` still in the maps
that is nearly every walk — 7,992 incomplete follows against 34
answered. The 91k `OpenNone` population is NOT cross-file exits waiting
to be named; it is keys that genuinely need the bag.
`PERL_LSP_MINT_LINKS` stays off, now for a measured cost reason rather
than a soundness one.

What this round retains: the poison machinery and its frame taxonomy
(the opaque-frame table in the `699488d8` message), EQUIV scoring for
`Follow`, the self-rung exclusion (left in, self-rungs converted nearly
the whole `OpenNone` population into two-hop abandons: 14,923 incomplete
vs 15 answered), and this negative result with its numbers. Do not
re-open `Link` widening against the 91k aggregate.

Where the arc points instead, in order:

1. **The re-bake driver is now the blocking piece, and it has a
   customer.** The open defect — a fingerprint change clears conclusions
   and nothing re-bakes them, so the layer stays dark until a manual
   full clear (`conclcache.known_absent` 156k in that state) — is the
   first concrete job for the generational flush driver
   (`docs/prompt-enrichment-alternatives.md` §3c′): on fingerprint
   mismatch, clear and enqueue every file as generation 1's frontier;
   "absent means decode" keeps the interim honest. The store is already
   generation-stamped and idle. A source edit takes the same path, so
   this is not a special case — it is the driver's cold-start.
2. **Attribute `OpenNone` by cause before widening any conclusion
   form.** The candidate widening is binder-carrying residuals (the
   `CallReturn` shape — one arity and one receiver rule is exactly what
   a call frame substitutes away, so `Link` cannot express it; the
   algebra's dependent form `ReturnOf` is the precedent). Whether it is
   worth building depends on the 91k's composition — per-cause counters
   on the bake's demotions (`no_bare_answer` / binder-probe demotion /
   poisoned residual / multi-arm) decide it, not the aggregate. The
   round-3 lesson generalizes: every sizing error in this arc came from
   reading a population by its total.

## Round 5 — the ruling on the per-class form: build it, in a smaller shape than proposed

The attribution settled two things at once: the binder-carrying-residual
widening is retired (its only convertible population is 9.3%, and
follows mostly abandon), and the dominant waste — 43,465 of 58,326
wasted decodes per warm check — is absences on classes the map concludes
about but cannot prove closed, where the decode's whole outcome is
"not here, walk to a parent" (98.7% answer nothing; the
`binder_dependent` control row at 98.8% PAID is what makes the table
believable).

**The ruling: build it — but the proposed form ("one per-class fact:
absent keys inherit from these parents", evaluated into a consult-time
`Follow`) reduces to something smaller and sounder. No new conclusion
form, no constructed `Follow`, no per-class parent list.** The live
ladder already IS the follow: candidates → parents → bridges, with
`parents_of` asking the index. The only missing piece is a third
absence verdict:

- closed class: absent = **proven None** (today's rule, unchanged);
- enumerated-but-not-closed class: absent = **proven not-LOCAL** — the
  consult skips THIS candidate's decode and lets the existing ladder
  continue (next candidate, then the parent walk, which itself consults
  maps);
- unknown class: absent = decode (today's rule, unchanged).

Why this form wins:

1. **It converts the 633-break population instead of fighting it.**
   Attempt 1's breaks were inherited methods whose absence was read as
   None — the ladder *stopped*. Under not-local semantics those same
   absences *continue* to the parent walk and resolve correctly. The
   failure mode that killed trust-every-absence is exactly the case
   this verdict handles.
2. **Ordering correctness is inherited, not re-proven.** A constructed
   `Follow` returned from candidate 1's map would short-circuit
   candidates 2..n — and a reopened package's method lives in a later
   candidate. Not-local just continues the loop, so the
   candidates-before-parents order and the bridge guard survive by
   construction. Pin it anyway: a test where class C is reopened in a
   second file that defines the method, asserting candidate 1's absence
   does not skip candidate 2.
3. **The soundness core is exactly one property**: per-candidate-file
   local-enumeration completeness — "if K is absent from this file's
   map, this file does not answer K locally." That is the same
   enumeration question trusted absence already leans on for closed
   classes (three attempts, 0 breaks under EQUIV), now load-bearing for
   non-closed classes where today's decode has been silently covering
   any gap. The residual failure shape is the bad one: a local override
   missed by the bake serves the PARENT's answer — wrong, not slow — so
   `PERL_LSP_CONCL_EQUIV` scores this from the first commit, not after
   (the flag's third campaign, and the reason it exists).

Expected coverage: the 43,465 directly; the other wasted rows (opaque /
linkable / self-rung) are keys PRESENT as `OpenNone` — a chase that had
local material it couldn't bake — and stay decodes; they are not
absence's business.

**Outcome, and one correction the build found.** Landed as specified —
three verdicts, continue-the-loop, reopened-package test mutation-
verified — for a 74% chase cut (`consult.attempt` 1,715 → 454 ms,
provider fetches 105,670 → 33,986; decodes barely move because the
resident tier was absorbing repeats — the chase was what paid).
Coverage matched the stated expectation. The correction: the rule above
stated soundness per KEY ("absent from this file's map ⇒ this file
doesn't answer it locally"), and the chase composes per CLASS — a file's
bag can carry witnesses on a PARENT's key
(`PackageSymbol{Mojo::Server, app}` answering
`{Mojo::Server::Daemon, app}` from the same map), so key-absence alone
misjudged 40 consults per check. The fix: the map walks the enumerated
class's declared parents WITHIN ITSELF before judging an absence
(depth-capped, not cycle-guarded — a same-file declared-parent chain is
short, and a truncation degrades to the verdict the absence gave
anyway). Also from the build, two instrument rules worth keeping: an
equivalence check for a bake-time verdict must compare against an
INDEX-LESS chase (the context the bake ran under), or it fires on
correct behavior at scale; and a verdict about a key cannot be validated
against a chase about a class — when a checker disagrees at scale,
suspect the question before the mechanism.

## The follow-on that empties the guard's decode arm (later, not now)

The bridge declaration is **local to the bridging file** — its plugin
namespace names the target class. So the bridging file's own bake can
evaluate its bridged entities and store them under portable
`MethodOnClass{C, name}` keys. Note what that buys: `Symbol(sid)` is the
attachment the live bridge consult cannot encode portably, but it is
resolvable *locally at bake time* — the bake is exactly the place where
file-internal attachments become portable. Then "bridged → decode"
refines to "bridged → consult the bridging files' maps via the same
`bridges` bucket," and the 1.7% pays a map lookup instead of a decode.
Size it from the per-class histogram first.
