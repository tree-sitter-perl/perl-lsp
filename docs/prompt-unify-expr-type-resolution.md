# Unify expression-type resolution: one chase, no tree

> Two functions resolve "what type is this expression?" and they drift:
> `FileAnalysis::resolve_expression_type` (tree-walking) and
> `method_call_invocant_class[_with_tree]` (refs + bag). They answer the same
> question by different means, so a fix to one silently leaves the other
> behind — most recently, `$c->minion->enqueue` resolved on the tree path
> (hover) but not the non-tree path (option-B enrichment) until two separate
> patches. This is a standing note on *why* the split exists and a plan to
> collapse it to a single, tree-free chase.

## The two chases

- **`resolve_expression_type(node, source, idx)`** (`file_analysis.rs`) — a
  recursive **tree walker**. Matches `node.kind()` (`scalar`,
  `method_call_expression`, `array_element_expression`, `function_call…`,
  `hash_element_expression`), extracts sub-parts from the CST (invocant node,
  array name + index), recurses, and asks the **bag** for the leaf types
  (`inferred_type_via_bag`, `sub_return_type_at_arity`,
  `find_method_return_type`). Callers: completion (`cursor_context.rs`), hover,
  and the tree-aware fast path inside the other chase.
- **`method_call_invocant_class_with_tree(r, tree, source, idx)`**
  (`file_analysis.rs`) — resolves a `MethodCall` ref's **invocant** class. It
  rediscovers the same structure from **refs**: `call_ref_by_start` for chain
  receivers, the ref's `invocant` text for variables/barewords, `shift`/`$_[0]`
  pseudo-invocants — then asks the **same bag** for the types. It has a
  tree-aware fast path that just calls `resolve_expression_type` *when a tree
  is available*. Callers: goto-def (`symbols.rs`), references, **enrichment**
  (`promote_provisional_dispatches`), and its own chain recursion.

## The structural reason (the note)

Both chases get **types from the bag** — that part is already unified and
canonical. They differ only in where they get **structure** (what shape is
this expression, what are its parts):

- At **hover / completion / build** the caller holds the **CST node**, so
  structure is read off the tree.
- At **enrichment / cross-file query** the caller holds a **`FileAnalysis`**
  (refs + bag) and **no tree** — the tree is transient (built, walked, dropped;
  `FileAnalysis` is serde/bincode-cacheable and deliberately carries no
  `tree_sitter` state). So structure must come from refs + witnesses.

That's the whole reason for the divide: **structure source differs because
tree availability differs.** Nothing about the *type* answer needs the tree.
So maintaining two structure-discovery implementations is the avoidable cost —
and they drift (the `$c->minion` divergence is the proof: the non-tree path's
variable arm had dropped the module index, and its goto-def arm fell through to
`symbol_at`; the tree path had neither bug).

## Why unify to the non-tree side

The non-tree side is the strictly more general home: it works whether or not a
tree exists, and it aligns with "the bag is the only source of types" + "no
`tree_sitter` outside the builder" (CLAUDE.md rules #1/#2). The tree is only
ever needed to **locate** an expression from a raw cursor (completion mid-typing
at a spot with no ref) — i.e. cursor → node → **span**. Once you have a span,
the bag answers. So the tree's legitimate job shrinks to "cursor → span," a
thin adapter, not a parallel resolver.

## Target shape

One query-time entry, span-keyed, tree-free:

```
FileAnalysis::expr_type_at_span(span) -> Option<InferredType>
```

It reads the bag's `Expr(span)` / `Expression(refidx)` witnesses the builder
already emits at "every meaningful expression node" (`emit_expr_witness`),
materializing edges through the registry exactly like the other query entries.
Then:

- `method_call_invocant_class` → `expr_type_at_span(invocant_span)?.class_name()`.
  The bespoke `call_ref_by_start` chain walk, the `invocant`-text variable
  lookup, and the `shift`/`$_[0]` special cases all collapse into "the builder
  recorded the invocant's type at its span; read it." (Pseudo-invocants like
  `shift` keep a small carve-out unless the builder emits an `Expr` witness for
  them too.)
- `resolve_expression_type(node, …)` → `expr_type_at_span(node_to_span(node))`.
  Keep the function as a **thin tree→span adapter** for completion's raw-cursor
  case; it stops being recursive and stops re-deriving structure.

Net: structure is discovered **once**, in the builder, and recorded as
`Expr(span)` witnesses; every consumer reads the bag by span. No second walker.

## What it needs (audit before committing)

1. **`Expr(span)` coverage.** `emit_expr_witness` must fire for *every* invocant
   shape the tree path handles today — crucially `array_element_expression`
   (`$users[0]->m`), chained method calls, `hash_element_expression`. Audit the
   builder; add emissions where missing. This is the load-bearing precondition:
   if a shape has no `Expr(span)` witness, the span query returns nothing and
   we'd regress that shape.
2. **The span-keyed query.** Generalize the existing `Expression(refidx)` /
   `inferred_type_via_bag` entries into one `expr_type_at_span` that resolves
   `Expr(Span)` (and falls back to `Expression`/`Variable` attachments that
   share the span). Build-time `bag_query_expr_span` already does this shape on
   the builder side — mirror it at query time.
3. **Migrate callers** (`symbols.rs` goto-def, `backend.rs`, enrichment,
   `cursor_context.rs`) and delete the recursion + `_with_tree` fast path.
4. **Pseudo-invocants** (`shift`, `$_[0]`, `__PACKAGE__`) — either emit `Expr`
   witnesses for them or keep the tiny enclosing-class carve-out in the one
   entry.

## Risks / subtleties

- **Build-time vs query-time bag.** Enrichment truncates + re-derives the bag;
  the span query must read post-enrichment state, like the other `_via_bag`
  entries already do.
- **Spans as keys.** Two expressions never share a span, so `Expr(span)` is a
  safe key — but synthesized/zero-extent spans (plugin emissions) must not
  collide. The builder already uses zero-extent spans deliberately; the query
  must tolerate them.
- **Don't regress completion.** Completion sometimes sits on an incomplete /
  ERROR node with no recorded witness; the tree→span adapter must degrade to
  the current node-kind handling for those, or return None gracefully.

## Acceptance

- `method_call_invocant_class` and `resolve_expression_type` produce identical
  answers for every shape (a table test over scalar / chain / array-element /
  function-call / hash-element invocants), with `tree: None`.
- The `$c->minion->enqueue` family resolves through the single path (the pins
  in `provisional_dispatch_resolves_helper_returned_receiver` and the chain
  hover test stay green).
- No `tree_sitter` node is required to type an expression once its span is
  known; the only tree use left is cursor→span in completion.
- `call_ref_by_start`'s chain-walk role shrinks or disappears (it may still
  index refs, but resolution no longer hand-walks it).
