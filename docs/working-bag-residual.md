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

- [ ] *(Followup)* `apply_chain_typing_invocants` (builder.rs:6030) —
      currently dispatches through `resolve_invocant_class_tree`.
      Should query `Expression(refidx)` from the bag for method-call
      invocants and `Variable{name, scope}` for scalar invocants.
      Bigger refactor — needs constructor-pattern handling on the bag
      side (NamedSub("new") doesn't carry per-class info).

- [ ] *(Followup)* `apply_chain_typing_assignments` (builder.rs:5937) —
      emit `Variable{$x, scope} → Edge(Expression(rhs_refidx))` and
      let the registry chase. Drop direct `resolve_invocant_class_tree`
      calls. Needs clear-and-emit pattern (idempotency in the worklist).

- [ ] *(Followup)* Delete `resolve_invocant_class_tree` and
      `receiver_type_for` once both chain-typing passes are bag-routed.

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
      bareword-as-zero-arg-call detection. Three-way mirror with
      `resolve_invocant_class_tree` and `receiver_type_for`. All three
      go away when chain typing migrates above.

## Polish

- [ ] *(Followup)* `BagContext::scope_point` (witnesses.rs:1026) — uses
      scope-end as the chase anchor. For Edge fired from a `branch_arm`
      witness mid-scope, this loses temporal precision (reassignment
      narrowing sees the latest binding instead of the point-of-emission
      one). Thread the chasing witness's span through `materialize`.

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
