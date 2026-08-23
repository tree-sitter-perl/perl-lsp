# Design brief: a residualizing mode for the reducer registry

**Status: designed, not implemented.** The question section below is the
original brief; the **Design answer** section at the end settles the five
sub-questions and stages the work. The conclusion layer
(`docs/prompt-conclusion-layer.md`) is built and landed; this is the one
thing standing between it and most of its value.

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
