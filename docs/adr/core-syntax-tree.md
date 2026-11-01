# ADR: The core syntax tree

tree-sitter's tree is the wrong shape to analyse on. `Node::parent` descends
from the root on every call (`ts_node_child_with_descendant` at each level),
so its cost is linear in each level's width, and the sibling steps go through
it. `start_position` is recomputed per call. A `Node` carries no identity a
fact can be keyed by. Every feature that needed to navigate or re-find a node
grew its own answer to that: a per-use parent climb, an index keyed by span
or by spelling, a pass of its own over the tree.

The cost showed up on Webmin (2026-10-31): flat `package main` files with
450–750 top-level statements, where each `parent()` scans hundreds of
siblings. Between 0.8.0 and 0.9.0 the per-use helpers added by several
features (`conditional_arm`, `enclosing_block`, `list_assignment_target_of`,
`is_access_base`, …) took `parent()` from ~60k to ~150k calls on three files,
56% of a 23% build-time growth (callgrind; per-merge table in the project's
`design/webmin-check-slowdown.md`).

## Decision

Each parse gets one flat preorder array, built by a single `TreeCursor`
pass (`src/cst/syntax.rs`). A node's id is its index. The structure lives
in the tree itself, and so do the facts consumers keep per node; nothing
that answers "where is this node" is built beside it.

- **Parent is a load.** Each entry stores its parent's id. Ancestors,
  `prev_sibling` (the preorder predecessor climbed back to the parent's
  level, O(depth)) and the parse's root follow from it.
- **A subtree is an id interval.** Each entry stores its subtree's end id
  and its end byte, so "everything under this node" is `id..end`. A node's
  *site* is the head of the same-range chain it sits in (a node and the
  parent it fills exactly); a fact about a range lives on its site, and the
  sites holding a node are its enclosing sites.
- **Facts are a half of the tree.** `NodeFacts<T>` is a consumer's facts
  per node, in node order, one slice per node, kept apart from the
  structure so a walk can hold node handles while it writes its own
  per-fact state. The pack extractor's captures are its first facts
  (`docs/adr/capture-vocabulary.md`); beside the tree it keeps only match
  → captures, because a match spans nodes.
- **A raw node maps to its id by identity, not position.** The preorder is
  tree-sitter's descendant order and start bytes never decrease along it,
  so `id_of` binary-searches the start byte and then matches tree-sitter's
  node id among the nodes starting there. A zero-width MISSING node stays
  distinct from its same-start neighbours, and a node of another parse
  never matches.
- **A forest, not a tree, per build.** The Perl builder also parses each
  `s///e` replacement as code over the file's bytes. `SyntaxForest` holds
  every parse and answers a node from the one that holds it; a node no
  registered parse holds is a bug (debug builds stop on it).
- **Lazy where a consumer may not need it.** Query predicates (`#parent?`,
  `#ancestor?`) take the forest through a closure, so a pattern without
  structural predicates never builds one; the pack extractor builds its
  forest on the first match that asks.

`layering_tests::tree_steps_go_through_the_syntax_tree` counts raw
`parent` / sibling / `child_with_descendant` steps under `cst`, `build/` and
`lsp/`. The sites left are cursor-time (one climb per request), `Path`
reads, or the C++ reparse's own trees. It only shrinks.

## Cost

One entry per CST node for the length of one file's build, freed when the
build returns; never resident, never persisted. An entry today is the raw
node plus a parent id, a subtree end id and its start and end bytes (48 B).
Webmin, all 1,397 files, single thread (2026-10-31, at 40 B): 3.38 M nodes,
the largest file 141k nodes (5.6 MB; 6.8 MB at 48 B), and the cursor pass
costs about 0.4 s against 2.3 s of parsing.

## Where it goes next

The entry keeps the raw node as the migration hatch: every reader not yet on
the tree still gets a `Node`. Fields move into the entry as their readers
port, and the node goes with the last raw reader:

- kind, field-in-parent, flags (named / extra / error / missing), end
  points, and next sibling as a load (`end` is its id), with the typed
  wrappers in `cst.rs` moving from `Node` to a `SyntaxNode` handle;
- the indices built off the tree retire as their consumers move:
  `ChainTypingIndex`'s span maps, `PlaceDisturbanceIndex`'s spelling keys,
  `find_local_sub_body`'s whole-file scan per loop;
- the injected `s///e` trees spliced under their replacement node, which
  closes `TODO(injected-trees)` (post-walk passes that traverse from the
  main root miss replacement code).
