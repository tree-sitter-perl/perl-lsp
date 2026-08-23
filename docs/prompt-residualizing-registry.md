# Design brief: a residualizing mode for the reducer registry

**Status: open question, evidence gathered, no implementation.** Written for a
design session. The conclusion layer (`docs/prompt-conclusion-layer.md`) is
built and landed; this is the one thing standing between it and most of its
value.

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
