# ADR: The resolution session — one memo across a walk's consults

`QueryState` dedups within ONE `ReducerRegistry::query`. A backward
reference walk issues one such query per candidate call site, and each
re-derives the same `PackageSymbol{package, method}` lattice from scratch.
At 138k files that re-derivation is combinatorial — 5–12 files declare a
common package name, the chase is keyed by that name rather than by an
import edge, and it recurses through every provider — and the verb does
not return. Measured: one `references`
request performed **10.7M cross-file consults in 15 minutes** and never
reached the projection, with the blob caches sized so large that
rehydration hit ~100% and the query still did not finish. Cache sizing
moves that wall; it cannot remove it.

The session is the outer memo, plus the bound that catches whatever the
memo does not.

## Shape

`ResolutionSession::enter(index)` is an RAII guard, thread-local, opened
once around a WALK — `CandidateSet::references()` (target minting resolves
invocants too, so it belongs inside), `refs_to` for its other callers, and
`FileStore::enrich_open`, because the open-doc heal runs the same cascade on
a background thread. Nested enters ride the open one. While it is open the
`PackageSymbol` /
`SlotType` cross-file hops answer from a memo of **candidate
contributions**: "what does the file at `path` contribute to this query".
It also shares `visible_def_candidates` (a clone + sort of the whole
candidate vec per call, asked millions of times per walk) and carries the
walk's consult budget.

Keyed on the candidate's **path**, never on a bag address. A bag-keyed
memo that outlives one query has an ABA hazard the moment an evicted
analysis is dropped and a rehydrated one lands at the same address — and
avoiding it would mean pinning every consulted analysis for the walk
(gigabytes at this scale). A path is stable and free.

## The soundness gates

**One visibility scope.** Entries are used only under the same
`&dyn CrossFileLookup` the session was opened with. A pack file's
`ScopedLookup` is a different object, so a closure-narrowed candidate view
never reads a memo minted under the unscoped index, nor writes one. Pack
behaviour is therefore unchanged by construction.

**The epoch.** Validity rides `CrossFileLookup::resolution_epoch()` — the
same additive counter (`gen_counter` + `shape_bumps` + freshness writes)
the enrichment-key memo validates against. Any index mutation moves it and
the session drops memo, candidates and interned paths wholesale. A new
mutation path must move one of the counter's legs; it never needs to know
this memo exists.

**Truncated values are remembered like any other — measured, not assumed.**
The first design refused to store a value the cycle guard fed by cutting a
key above the evaluation's own root, on the theory that such a value is
path-dependent. At CPAN scale the split-package fan-out makes those cuts
near-universal: **508,319 refusals against 5,870 stores** in one walk, so the memo
remembered nothing and the walk still did not return. It is also a gate the
codebase does not otherwise apply — `QueryState`'s own memo stores every
off-path resolution regardless of ancestor cuts and reuses it elsewhere in
the query. The session widens that existing window from one query to one
walk; it does not open a new class of answer. The next person to reach for
this gate should read this paragraph first.

**Full query identity in the key.** Attachment, receiver identity, arity
hint, point and framework all ride the key, for the same reason they ride
`QueryState`'s: two queries differing in any of them resolve differently.
Receiver is the whole structural identity, not a variant tag —
`ReturnExpr::Receiver` substitutes the receiver, so `ClassName("Foo")` and
`ClassName("Bar")` must not share a slot.

## The self-skip

Hop (1) skips a candidate whose bag IS the querying bag: the reducers
above already ran on it, and re-entering would recurse. A memo HIT can't
make that check — it has no bag in hand. It doesn't need to: the stored
value for candidate `X` is `query_rec(X.bag, q, ctx_X)`, which is exactly
what a query already running on `X.bag` with the same key computes. Serving
it early is the fixpoint, not a different answer. Self-skipped candidates
are never STORED, so an entry always describes a full evaluation.

## What actually made the verb return

The memo alone did not. Four changes did, and no single one of them was
enough — the attribution matters more than the total:

- **the memo** — breadth: the same candidate answered once per walk instead
  of once per call site;
- **the budget at the cross-file BOUNDARY, not per hop** — gating hops
  individually (the `PackageSymbol` candidate loop only) let the cheap hops
  through and the walk kept running. One gate at the entry to the whole
  cross-file fallback region is the honest placement;
- **an enrichment depth cap** — depth: `ENRICHING` refuses only a path
  already on this thread's stack, so a chain of DISTINCT files recurses as
  deep as the dependency graph is long, deep-copying and enriching a whole
  analysis per level. Measured at 138k files: one consult descended 220+
  frames of enrich → query → enrich and never came back. A memo cannot help
  a walk that never comes back up to the hop where the memo is consulted;
- **a session around `enrich_open`** — the open-doc heal runs the identical
  cascade on a background thread with no walk to bound it.

The depth cap is a containment measure, not a tuned value. It exists to
bound a recursion whose real defect is that the cycle guard is
context-dependent (whether a file comes back enriched or raw depends on who
asked first, which is also why a cyclic build can never be cached). Stratified
"level-indexed" enrichment removes that defect and should make the cap a high
backstop.

## The consult budget, and how a bounded answer says so

Even memoized, some query at some scale exceeds any bound, and degrading
honestly beats running forever. The budget has **two units** because neither
alone can be sized. A COUNT is deterministic — the same query degrades in the
same place on every run — but a consult costs microseconds on a warm small
project and ~2.5 ms at 138k files (~9 blob rehydrations each), so any count
generous enough for a healthy walk (Koha: ~5k consults) is already tens of
seconds at scale. A DEADLINE is scale-free and gives the verb a real latency
contract, at the cost of being load-dependent. `PERL_LSP_RESOLVE_FUEL`
(5M, the deterministic backstop) and `PERL_LSP_RESOLVE_BUDGET_MILLISECONDS` (30s, what
actually fires); `0` disables either.

The gate sits at the entry to the cross-file fallback region, so a spent walk
still answers from what each bag knows locally and only stops CHASING.

**A bounded answer must be visible to the person reading it.**
`textDocument/references` returns `Location[]` — the protocol has no
`isIncomplete` for it — so the verdict leaves the walk out of band:
`ResolutionSession` publishes it on the owning guard's drop,
`take_last_walk_degraded()` reads it on the projection's own thread, and the
handler raises one `window/showMessage` WARNING per server session (a toast
per query is how a signal is trained into noise; the log still records
every occurrence). The enrichment depth cap feeds the same marking — a
decline serves a raw bag where an enriched one was due, and that is exactly
the quietly-smaller-answer failure mode the marking exists to prevent.

Over-marking is deliberate. At Koha the cap declines 130 builds, marks the
answer incomplete, and the answer is byte-identical anyway — a user told
"possibly incomplete" about a complete answer will learn to discount the
signal, and that cost is accepted rather than traded for silence.

## Where policy goes

A new cross-cutting axis belongs in the session or in CandidateSet
construction — never in a handler. The session is per-walk state, so it is
the natural home for anything a walk must budget or remember across its
consults.
