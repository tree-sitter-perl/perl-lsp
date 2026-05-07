# Sub-literal body return-type inference for plugin-synthesized methods

**Status:** **LANDED.** PR #35 added the four pieces this needs:

1. `ArgInfo.sub_body_last_expr_span` — populated by builder when
   the arg is an anonymous-sub literal.
2. `EmitAction::Method.return_via_edge: Option<Span>` — plugin
   passes the body's last-expr span through; builder emits
   `Symbol(sid) → Edge(Expr(span))` instead of an
   `InferredType` payload.
3. Anonymous-sub Sub-scope push (`visit_anonymous_sub`) so
   `enclosing_sub_scope()` returns Some inside `sub { ... }`
   args; `publish_return_arm_witnesses` fires for the body's
   `return` / implicit-last-expression, populating
   `Expr(body_last_expr_span)` for the bag's edge-chase
   resolver to follow.
4. `write_back_sub_return_types` mirrors `Symbol(sid)` Edge
   payloads to `MethodOnClass{class, name}` for class-scoped
   plugin synth, so cross-file `find_method_return_type`
   finds them via the class-keyed attachment.

`mojo-helpers.rhai` now sets `return_via_edge: args[1].sub_body_last_expr_span`
on the synthesized Method. In-file + cross-file helper tests
both pass (`mojo_helper_returning_resultset_composes_in_file`,
`mojo_helper_returning_resultset_composes_cross_file`).

The doc below is the original spec — kept for context. The
"adjacent gaps" section still applies (first-class function
types, receiver-typed helper bodies via `ReturnExpr`).

---

**Original status:** queued. Surfaced by the Part 5c composition stress
test. Const-folding + Parametric + RowOf compose end-to-end for
direct calls (`$schema->resultset($sner)->search(...)`); the same
chain through a Mojo helper synthesis (`$app->helper(sner_r =>
sub { $schema->resultset($sner) })`, then cross-file
`$c->sner_r->search(...)`) does NOT compose because the helper's
synthesized Method symbol carries `return_type: ()`.

## The gap

`frameworks/mojo-helpers.rhai` emits an `EmitAction::Method` per
`$app->helper(name => sub {...})` with `return_type: ()`. The
synthesized Method is correctly registered cross-file, completion
on `$c->name` finds it, gd lands at the helper registration —
all the structural pieces work.

What's missing: the Method's return type. Without it, chain
typing through `$c->name->search(...)` breaks at the first hop.
The helper's body — `$schema->resultset(...)` — has a
Parametric type the bag knows; the Method just doesn't inherit
it.

The user's framing ("should be roughly free to have mojo
fallthru to the arg's type") is exactly right architecturally —
the natural rule is "a synthesized Method's return type IS its
sub-literal body's return type, unless the plugin overrides."
The plugin should declare that fallthrough; the builder fills
the type at the right moment.

## Why it's not Phase 1 free

The walker visits a `method_call_expression` for `$app->helper
(name, sub { ... })` — runs plugins **before** recursing into
children (the sub literal is a child). At plugin-emission
time, the sub's body hasn't been walked, so its return type
isn't in the bag yet. Fixing this means one of:

### Option A — walker-order flip

Walk children first, run plugins after. Plugins see all
inferred-from-body data on `ArgInfo` (including
`sub_return_type` for sub-literal args). Cleanest emit-time
story.

**Cost:** real. Today's plugins assume "I see the call before
the sub body walks" semantics in some places (DBIC
`load_components`, Moo `has`, etc. don't depend on this — they
emit data from arg shape, not from inner-block inference, so
flipping order is probably safe). Need to audit every plugin
to confirm. Ordering also affects ref emission — the plugin
might want to emit a ref BEFORE a sub body's contents add
their own.

### Option B — back-pointer + post-walk fill

Each `EmitAction::Method` carries an optional
`infer_return_from_span: Option<Span>` field — the span of the
sub-literal body the plugin wants the return type from. Builder
post-walk: for each Method symbol with `return_type: None` and
a back-pointer span, query the bag at that span for the
expression's type, write back as a Symbol(sid) witness.

**Cost:** smaller. Adds one optional EmitAction field, one
post-walk pass. No walker reorder. Plugins that want
fallthrough-from-body opt in by setting the field; existing
plugins unchanged.

### Option C — defer to ReturnExpr (Section 2 of the
parametric-redesign)

`ReturnExpr` (the receiver-relative return-type machinery) is
the structural answer. A helper symbol's `return_type` becomes
a `ReturnExpr` referencing the body's Expr(span). The bag
substitutes / evaluates lazily.

This is the right place architecturally. Bundles with the
arity-dispatch retirement and per-method projection
declaration. **Cost:** biggest, but composes more.

## Recommendation

Option B (back-pointer + post-walk) is the cheap intermediate.
Lands the Mojo helper composition for the lifetime of this
codebase before ReturnExpr arrives, with reuse: the post-walk
fill machinery moves into ReturnExpr's evaluation when that
lands.

## Test to land alongside the fix

Was drafted in `src/parametric_resultset_tests.rs` as
`mojo_helper_returning_resultset_composes_cross_file`,
removed when the gap was identified (per "no #[ignore]'d
tests" rule). Resurrect verbatim from PR #35's history when
the fix is in flight.

## Adjacent gaps

- **First-class function types.** `my $sub = sub {};
  $app->helper(thing => $sub)` — the helper's arg is a scalar
  binding, not an inline sub literal. Today we don't
  propagate sub-literal type through scalar bindings; the
  helper plugin sees `args[1]` as a scalar with `inferred_type:
  Some(CodeRef)`, no body span. Same options A/B apply but
  the back-pointer needs to chase through the variable's
  binding site.
- **Helper bodies that close over schema-typed locals.** The
  body's return type often depends on receiver (`$c`) or
  closure captures (`$schema`). For now const-folding handles
  the local-scalar case; receiver-typed helper bodies are
  ReturnExpr territory.