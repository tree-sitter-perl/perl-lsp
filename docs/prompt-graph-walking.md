# Graph walking — forward work

The walker landed: `src/graph.rs` (`GraphView` + `walk`), the closed
`EdgeKind` enum with exhaustive `edges_from`, and the INHERITS /
INHERITS_INV / BRIDGES edge kinds. The whole inheritance axis routes
through it; `children_index`'s descendant fan-out and plugin bridges
too. Landed design + rationale: `docs/adr/graph-walking.md`. This doc
is what's left.

## Branded edges — LANDED (mechanism + per-file), forward work below

The mechanism shipped: `PluginNamespace.brand` + `visible_under` (the
one additive rule), `GraphView` brand context (`new_branded`) into the
single BRIDGES edge, the cross-file `for_each_entity_bridged_to_branded`
primitive, and the same-file visibility sites gated on
`self.query_brand()`. The PER-FILE consumer is wired:
`FileAnalysis::home_brand` (= canonical path), assigned at registration
by `apply_home_brand` for self-contained Mojo::Lite apps, so two NAMED
lite-app packages in one workspace no longer merge helpers. Landed
design: `docs/adr/branded-edges.md`. A three-way bake-off (brand on the
bridge / on `InferredType` / on the namespace) picked the namespace
placement; the judges' grafts (visible_under-as-a-method, the Brand
context, the value-sourced consumer brand) are folded in.

What remains — both reuse the SAME mechanism, only the brand SOURCE
changes:

### Per-variable instances

`my $a = Minion->new; my $b = Minion->new` share a file, so the per-file
brand can't separate them. The same-file sites already gate on
`visible_under`, so the model is ready; what's missing is (a) EMISSION —
the plugin mints one branded namespace per construction site, brand =
the decl-site instance id (the `PluginNamespace.id`
`"minion:$minion@MyApp.pm:5"` scheme); and (b) QUERY — a per-variable
instance-brand resolver out of `method_call_invocant_class`, surfacing
the receiver's brand ON the resolved value (NOT a consumer-minted
string, NOT a new `InferredType` variant — carry it as a sibling datum,
per the lossy-`Option<String>` rule).

### Accessor chains — timing decided

`$app->minion` brand depends on what the accessor returns (cross-file
return type). Decision (bake-off + review consensus): **lazy at query
time**, derived from the resolved invocant — NOT a cross-file enrichment
post-pass. Enrichment runs OPEN-DOCUMENTS-ONLY and bakes one brand per
accessor, but the brand is query-receiver-dependent, so it would
re-merge on workspace/dependency files (recreating the bug). The
invocant resolution already runs lazily + cross-file at query time; the
brand rides out of it into `GraphView::new_branded` / the branded
primitive — the seam is already in place.

### Full-Mojo multi-app (out of scope, documented degradation)

Helpers on a `Mojolicious` app class consumed by SEPARATE controller
files stay merged: the controller file can't carry the app's brand
(`home_brand` is per-file and the controller is a different file). Not a
regression (today's behavior); a real fix needs controller→app
association, which doesn't exist statically.

## Deferred: Scope nodes (the future taxonomy)

`Node::Scope` + a PARENT edge would let the graph model lexical scopes,
packages, and plugin namespaces as one node space — the foundation for:

- **Openness diagnostic.** Walk the namespace chain from a ref's site:
  terminates `Closed` without resolving → warn; hits `Open`
  (role/plugin/abstract base) first → suppress. Replaces the bespoke
  `framework_imports` suppression and unifies unresolved
  function/method/stash-key/route/helper.
- **`Symbol.home_namespace`.** `Symbol.package: Option<String>` →
  `Symbol.home_scope: NodeId`; `Bridge::Class(...)` → a bridge to a
  framework scope node directly.

Deferred deliberately: the scope parent-climb is a linked-list, not a
walk (`adr/graph-walking.md`), so `Node::Scope` earns its keep only
when Openness / `home_namespace` are actually built — not to port the
trivial `scope_chain_of`. Build it when those land, with their needs
shaping the node.

## Out of scope (decided, not deferred)

- **File roles** stay `RoleMask` — enumeration, not traversal
  (`adr/graph-walking.md`).
- **Stack-graphs upstream** — evaluated, not adopted. The Rust-API
  construction path would restate Perl-specific emission (plugin
  namespaces, framework synthesis) in their pipeline; a custom
  typed-edge view integrates with the existing builder + witness/
  reducer pipeline at a few hundred lines. Revisit only if their path
  semantics + tooling become worth that boundary.
