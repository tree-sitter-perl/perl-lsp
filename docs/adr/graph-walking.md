# ADR: Graph walking — one lazy walker over the visibility edges

Several queries answered "what is visible from here" by walking
conceptually-one graph through different bespoke code: the class MRO
(`for_each_ancestor_class`), the composer fan-out (`children_index`),
plugin-namespace bridges (`for_each_entity_bridged_to`). Forgetting one
was a class of "cross-file feature mysteriously degraded" bug. `walk`
is the single traversal those collapse into.

## The shape: a lazy derived view, not a built graph

`src/model/graph.rs::GraphView` holds two borrows — `&FileAnalysis` and
`Option<&dyn CrossFileLookup>` — and stores nothing. There is no node
list, no adjacency map, no build step. `walk(origin, mask, visit)`
DFS-chases edges on demand: `edges_from` derives a node's neighbours at
the moment the walker asks, by calling the stores that already exist
(`real_parents_of`, `direct_children_of`, `for_each_entity_bridged_to`).
The visitor answers per node with a `WalkControl` verdict —
`Continue`, `PruneChildren` (skip this node's own expansion; the rest
of the traversal proceeds), or `Stop` — so gated gathers (the
role-requires walk prunes at every non-role node) and scoped views
(the trigger view's local-only parent chain) are expressible as THE
walk instead of bespoke BFSes. A
walk materialises only its transient state (seen-set + per-node
neighbour vec), discarded on return. "Chasing an edge" *is* a map
lookup against `PackageFacts::parents` — the same primitive the ported
consumers used, so a port is parity by construction.

`walk` is the caching seam: every consumer goes through it and never
sees how edges are produced, so caching slots in behind `edges_from`
with zero consumer change. Per-edge-kind, not all-or-nothing —
`children_index` is the materialised form of the INHERITS_INV edge (the
reverse walk was expensive, precomputed once), while INHERITS stays a
lazy `parents_of` lookup. The home for an edge cache is `ModuleIndex`,
which every `ScopedLookup` (the visibility-scoping decorator production
resolution wraps around it) delegates through, behind the trait method
— the view never owns cross-query state. Lazy is the default so nothing pays for
that machinery until a profiler asks.

## `EdgeKind` is a closed enum; `edges_from` is exhaustive

The edge kinds are a closed `enum EdgeKind { Inherits, AppSurface,
InheritsInv, Bridges, Specializes }`, and `edges_from` matches it
**exhaustively** — adding a kind is a compile error until its
derivation is written. This is the "one match site, never a parallel
walker" rule with compiler teeth. The synthetic app-surface parent is
its OWN kind: walks that must not treat a manifest-declared consumer as
a descendant of the surface (trigger views, the isa gates) pass bare
`INHERITS`; full-MRO consumers pass `INHERITS | APP_SURFACE`. The two
component spellers (`real_parents_of`, `app_surface_parent`) also
compose `parents_of` for the direct (non-graph) enumeration sites, so
the edge condition is spelled once either way.
The `EdgeKindMask` bitflags exists only for set membership + ergonomic
`|` (a walk traverses several kinds at once); `EdgeKind` is the source
of truth, and `EdgeKind::ALL` + `flag()` keep the two in lockstep (a
test pins that the `ALL`-union covers every mask bit, the one hole the
fixed-length array can't catch at compile time).

## Model layer — why the view sits below the index

`GraphView` imports only `file_analysis` (the `CrossFileLookup` trait +
`parents_of`) — zero Index-layer deps — so it lives at the Model layer.
That lets the model-internal walkers (`for_each_ancestor_class` and the
method/dispatch/bridge resolution that funnels through it) call `walk`
directly, no up-layer import. The DAG test permits this: graph and
`file_analysis` mutually reference at the same layer (only a strictly-
upward import is a violation). The alternative — relocating the
ancestry query functions up out of the model — is available if the
model ever gets fat, but is not needed: the view stays pure, and any
caching lives at the Index layer behind the trait.

## The inheritance axis is one walk

Every inheritance consumer funnels through `for_each_ancestor_class`,
which is now `visit(self)` then `walk(class, INHERITS | APP_SURFACE)`.
Self-handling
lives in that one wrapper — it runs the consumer's own closure on the
origin, which is consumer-specific (`class_isa` checks equality,
`resolve_method_in_ancestors` checks the method) and so cannot move
into `walk`, which holds no closure for the origin. `SUPER::` — parents
only, never self — is the bare `walk`; there is no `skip_self` flag,
because *checking self is not a walk*. The cycle guard is `walk`'s
path-depth cap (the seen-set already breaks cycles); a class with many
ancestors is not silently truncated.

## Boundaries — what is NOT a walk

Two of the "parallel models" the graph was meant to absorb are not
edge-following traversals, and forcing them in would be contortion:

- **File roles** (`refs_to`/`references_mask_for` open→workspace→
  dependency tiers) ENUMERATE every file and scan its analysis — there
  is no origin and no edge to chase. The completeness this guarantees
  (re-exports, framework DSL, dynamic dispatch produce refs with no
  clean import edge) is the whole point of the scan. `RoleMask` is a
  partition, not a traversal, and stays.
- **Lexical scope chains** are a single-parent linked-list climb to the
  file scope — no branching, no cycles, no cross-file. `walk`'s
  machinery buys nothing. The two climbs are `scope_chain_of(&[Scope],
  start)` (containment) and `lookup_chain` (name lookup, which states
  each scope's own rule), both free functions over a `&[Scope]` slice so
  the witness-bag query path, which holds no `&FileAnalysis`, shares
  them (`docs/adr/capture-vocabulary.md`).

The migration ends when `PackageFacts::parents` and `PluginNamespace.bridges`
have no direct consumers — every *reachability* question is a `walk`
with a mask. `RoleMask` is not in that set.
