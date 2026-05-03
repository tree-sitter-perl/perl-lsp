# Bag-residual punch list

Working notes from the spike/edge-facts review. Things that still bypass
the bag after Step 4 landed. Use as a checklist; cross out as each lands.

## Root cause

`Symbol.return_type` is still load-bearing — not just a cache. Three
downstream sites read it directly because there's no bag-emitted answer
for "plain sub with implicit-last-expr return" until writeback, and
because the writeback's NamedSub publish wasn't there originally.

After this PR's items land, the field becomes a true cache: every
inference-flavored consumer routes through the bag.

## Mechanical fixes (no semantic change)

- [x] **`seed_plugin_overrides_into_return_types`** (builder.rs:6315) —
      manual `max_by_key(priority)` scan re-implements PluginOverrideReducer.
      Replace with `reg.query()` so reducer-claim changes can't silently
      drift. Mechanical.

- [x] **Dead `Symbol.return_type` fallback in `query_sub_return_type`**
      (witnesses.rs:1102-1108) — writeback already pushes NamedSub for
      every sub with a resolved return_type; the NamedSub branch above
      always answers when this branch would have. Provably dead.

## Edge-emission migrations (replace procedural propagation)

- [x] **`propagate_via_delegation`** (builder.rs:6355) — fixed-point
      loop reading `sub_return_delegations: HashMap<String, String>`
      directly. Replace with re-emittable pass pushing
      `NamedSub(delegator) → Edge(NamedSub(delegate))`. Registry chase
      handles transitivity. Drop the loop.

- [x] **`propagate_via_self_method_tails`** (builder.rs:6398) — same
      shape, scope-keyed. Push `NamedSub(sub_name) → Edge(NamedSub(tail_method))`.
      Drop the loop.

- [x] **`apply_chain_typing_invocants` + `apply_chain_typing_assignments`**
      (builder.rs:6020 / 5937) — both still call
      `resolve_invocant_class_tree`, but the function's BODY is now
      bag-routed. Queries `Expression(refidx)` for method-call
      invocants (with constructor-pattern bake before bag consult),
      `Variable{name, scope}` for scalar invocants,
      `NamedSub(name, arity=Some(0/N))` for bareword + function-call
      invocants. Walk-time bag is sparser (TC mirroring is post-walk;
      plugin / framework synthesis pushes are immediate) so walk-time
      calls return None for variables and PostFold's invocant
      refresh fills them. Same function used in both contexts.
      `scope_at_point` lookup means it works post-walk after
      `scope_stack` is empty.

      The mistake along the way: I initially added a parallel
      `resolve_node_class_via_bag` function. Got pushed back on. Right
      answer: mutate the existing function. See
      `feedback_no_via_bag_siblings.md`.

- [x] **`receiver_type_for` bareword arm** (builder.rs:1373) —
      direct `Symbol.return_type` field read replaced with
      `bag_query_named_sub`. Variable arm still reads TCs directly
      because at walk time TCs ARE the canonical store (the bag
      mirrors them post-walk via `populate_witness_bag`); reading TCs
      here isn't a parallel path with the bag, it's reading the bag's
      input.

- [x] **Plugin synthesis pushes bag witnesses at synthesis time**
      (builder.rs:1606 in `apply_emit_action::Method`) — top-level
      plugin synth (`on_class.is_none()` — `app` from
      Mojolicious::Lite, etc.) pushes Symbol(sid) + NamedSub(name)
      Plugin-source witnesses immediately, mirroring the
      enrichment-time pattern for cross-file imports. Class-scoped
      synth (`on_class.is_some()`) skips the bag push entirely:
      same-named methods across nested namespaces (mojo-helpers
      emits `users` proxy on Controller AND inside `admin`'s
      namespace) would conflate via NamedSubReturn-latest-wins.
      Bridges remain the dispatch mechanism for class-scoped synth
      (per CLAUDE.md rule #8).

## Symbol.return_type readers (route through bag)

- [x] **`find_method_return_type_raw`** (file_analysis.rs:2126/2142) —
      replaced direct field reads with `symbol_return_type_via_bag`
      that queries `Symbol(sym_id)` through the registry. New
      `SubReturnReducer` claims `local_return` / `imported_return`
      witnesses, registered last so Plugin / branch-arm / arity
      dispatch get first crack. Multi-overload class-bound dispatch
      stays procedural (still picks sym_id by `params.len()`); the
      per-sym answer routes through the bag.

      Two subtleties baked into the reducer:
      - source-tag claim (only `local_return` / `imported_return`),
        not all `Symbol+InferredType` — branch_arm-source materialized
        witnesses must keep flowing through `BranchArmFold` so its
        "single arm or disagreement → None" yields properly;
      - fires only when `arity_hint.is_none()`, so an arity-specific
        query whose `FluentArityDispatch` answer is None doesn't
        get papered over by the getter sym's flat stored value
        (Mojo `level(1)` mustn't return the getter's String).

- [ ] *(Followup)* `resolve_invocant_class` bareword arm
      (file_analysis.rs:3644) — direct Sub.return_type read for
      bareword-as-zero-arg-call detection. The Builder-side mirrors
      (`resolve_invocant_class_tree`, `receiver_type_for`) are now
      bag-routed; this FA-side mirror is the last one. Migrate to
      `sub_return_type_at_arity` (which already routes through the
      bag).

## Polish

- [ ] *(Followup)* `BagContext::scope_point` (witnesses.rs:1026) — uses
      scope-end as the chase anchor. For Edge fired from a `branch_arm`
      witness mid-scope, this loses temporal precision (reassignment
      narrowing sees the latest binding instead of the point-of-emission
      one). Thread the chasing witness's span through `materialize`.

- [ ] *(Followup, hack)* **`emit_call_arg_key_accesses` is walk-time
      only** (builder.rs:5583, called from `visit_method_call` at
      ~line 4953 and `visit_function_call` at ~line 3987). Runs
      inside the live walk and gates on `invocant_class.is_some()` —
      which forces walk-time invocant_class resolution to fall back
      to syntactic text reads (bareword → just the text). That's a
      parallel path with the bag for invocants whose canonical class
      only the bag knows (e.g. `app->routes`: walk-time syntactic
      reads `app` as class "app", but the plugin-pushed
      `NamedSub("app") → ClassName(Mojolicious)` is the real answer).
      The bag-routed `resolve_invocant_class_tree` would do the right
      thing, but emit_call_arg_key_accesses' walk-time gating means
      we can't drop the syntactic walk-time set without losing the
      key emissions for `MooApp->new(name => 'alice')` and friends.

      Move `emit_call_arg_key_accesses` to a post-walk pass that
      reads each MethodCall ref's now-canonical `invocant_class`
      (filled by `apply_chain_typing_invocants` against the bag) and
      iterates the args node from `ChainTypingIndex` (or stores the
      args span on the ref). Once that's done, walk-time
      invocant_class becomes purely closed-under-syntax (constructor
      pattern + `__PACKAGE__` only) — no syntactic-text fallback for
      bareword, no parallel path with the bag.

## Don't do

- **`apply_type_overrides`** is fine. It already just emits a
  Plugin-priority witness; the seed pass that consumed it (item 1
  above) was the redundant piece.

- **Display/UI consumers of `Symbol.return_type`** (backend.rs,
  symbols.rs, hover, outline) — let them keep reading the field as a
  cache. They're not making inference decisions.

- **`enrich_imported_types_with_keys`'s `sub_return_type_local`
  early-out check** (file_analysis.rs:1592) — using the field as a
  cache to skip already-resolved bindings. Fine.

## Validation gate

After each phase: `cargo test`, `./run_e2e.sh`. 506 unit + 93 e2e at
baseline; both must stay green.
