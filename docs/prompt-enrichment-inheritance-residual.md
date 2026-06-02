# Enrichment + inheritance residual — diagnosis

Does plugin/framework behavior reach a class through the **inheritance**
and **cross-file-dependency** chain? Two angles, both surfaced during the
cross-project usability sweep.

The short verdict:

- **Return-type / method resolution** through inheritance + cross-file
  bridges is **correct by design** — the `MethodOnClass{class, name}`
  query walks `parents_of` (the single seam) and reaches into cached
  modules' bags. Build-time edges (`inheritance` / `inheritance_cross`)
  optimize the common path; the `query_rec` structural fallback is the
  floor for any FA that skipped enrichment.

- **Plugin EMISSION gated on the class's ancestry** (`ClassIsa` triggers;
  `param_types` `in_role`) is **NOT cross-file aware**, and can't be
  without changing the index-free-builder contract (rule #1). Latent
  hazard, documented, deferred.

- **Dispatch-verb promotion in non-open files** is a real gap caused by
  the resolver lifecycle: only OPEN documents are enriched. Architectural,
  deferred.

Two `#[ignore]`-d failing repros pin the two real gaps
(`builder_tests.rs::param_types_manifest::probe_*`).

---

## A. Per-manifest applicability matrix

For each declarative manifest member (`docs/adr/plugin-system.md`
"Declarative manifests"), whether its applicability through inheritance is
(i) correctly local-only by Perl semantics, (ii) correctly
cross-file/isa-based already, or (iii) BROKEN.

| Manifest member | Applicability gate | Verdict |
|---|---|---|
| `overrides()` | Trigger-INDEPENDENT post-pass (`apply_type_overrides`, `builder.rs:320`). Applied to every build regardless of `use`s. | **(ii) correct.** Plugin-priority witness on `Symbol(sid)`; the home module picks it up even without `use Mojolicious`. Cross-file consumers reach it via the `MethodOnClass → Edge(Symbol)` writeback + cross-file primary lookup. |
| `dispatch_verbs()` | RECEIVER-isa at enrichment (`promote_provisional_dispatches`, `file_analysis.rs:2443`). `class_isa` walks local `package_parents` ∪ `module_index.parents_cached` (cross-file). | **(ii) correct by design** for OPEN files — receiver-driven, cross-file. **(iii) gap for NON-OPEN files** — see angle B. The emit-hook path (`EmitAction::DispatchCall`) covers files that `use Minion`; the manifest path covers cross-file/subclass receivers, but ONLY at enrichment, which non-open files never get. |
| `type_constraint_names()` / `type_constraint_inner()` | Syntactic gate on constraint-constructor call names where the `has`/`isa` appears. | **(i) correctly local.** The constraint vocabulary applies where the constructor expression is written. Not an inheritance question. |
| `param_types()` | `in_role` check via `transitive_parents(&pkg)` (`builder.rs:3442`), applied at the sub-declaration walk. `transitive_parents` walks **local `package_parents` only**. | **(iii) gap.** A class whose `in_role` ancestor is reachable only through a CROSS-FILE intermediate role/parent is not typed. Direct cross-file `with 'Role'` works (the role name is a direct local parent); a cross-file *grandparent* role does not. Same root cause as the `ClassIsa` gap. |
| `app_surface_consumers()` | Trigger-INDEPENDENT bake (`builder.rs:254`) onto `FileAnalysis.app_surface_consumers`; consumed by `parents_of` as the synthetic `APP_SURFACE_CLASS` edge. | **(ii) correct.** `parents_of` is the single seam; the `MethodOnClass` walk + ancestor walks all route through it, so the surface resolves cross-file from every consumer. |

### The `ClassIsa` trigger axis (not a manifest member, same root cause)

Several bundled plugins fire on `ClassIsa` triggers (mojo-events
`Mojo::EventEmitter`, minion `Minion`, mojo-helpers / mojo-routes
`Mojolicious`, …). `PluginRegistry::applicable` (`plugin/mod.rs:991`)
matches the trigger against the `parents` list, which the builder fills
from `transitive_parents` — **local `package_parents` only**
(`builder.rs:2121`, `:2175`, `:2200`).

Consequence: a class whose trigger-class ancestry is established
cross-file does NOT get the plugin's emit hooks. E.g. `package Leaf; use
parent 'Mid';` where `Mid` (another file) extends `Mojo::EventEmitter` —
`$self->on('ready', …)` in `Leaf` synthesizes no Handler symbol, because
the `ClassIsa("Mojo::EventEmitter")` trigger sees only `Mid`.

Confirmed by `probe_class_isa_trigger_through_cross_file_parent` (FAILS).

**Why it can't be a contained fix.** Plugin emit hooks (`on_method_call`,
`on_function_call`, `on_use`) run at PARSE TIME inside `build()`, where the
builder is index-free by rule #1. There is no module index to consult
mid-walk, and even if one were threaded in, indexing order isn't
guaranteed (the dependency carrying `Mojo::EventEmitter` may not be indexed
yet). This is the *exact* reason the `dispatch_verbs` path was moved to
enrichment (`promote_provisional_dispatches`): emission deferred to a phase
that owns the module index. Making `ClassIsa` cross-file-aware means moving
emit-hook firing out of the walk into a post-index pass — a large change
that re-implements the trigger evaluation against the cross-file graph and
re-runs every applicable hook at enrichment.

The in-file case composes correctly today
(`plugin_mojo_events_triggers_through_transitive_parent` — Mid + Leaf same
file). It's strictly the cross-file ancestry that's missed.

---

## B. Enrichment for dependencies / non-open files

`enrich_imported_types_with_keys` (which runs `promote_provisional_dispatches`,
the imported-hash-key synthesis, the cross-file `inheritance_cross` edge
projection, and `resolve_method_call_types(Some(idx))`) is called ONLY for:

- OPEN documents — `backend.rs:50` (resolver refresh, `for_each_open_mut`)
  and `backend.rs:72` (`publish_diagnostics`).
- The CLI focus file — `main.rs:469/513/556/761`.

**Dependency modules** (`module_resolver.rs::resolve_and_parse_inner:552`)
and **workspace-index files** (`index_workspace_with_index:655`) are built
via `crate::builder::build(...)` which ends at `finalize_post_walk()` —
they NEVER get `enrich_imported_types_with_keys`. The resolver refresh
callback re-enriches only open docs (`for_each_open_mut`,
`backend.rs:49`).

What this means per enrichment pass:

| Enrichment pass | Reaches non-open files? | Compensated at query time? |
|---|---|---|
| `inheritance_cross` edge projection | No | **Yes** — `query_rec`'s structural `MethodOnClass` fallback (`witnesses.rs:1190`+) walks `parents_of` + recurses into cached bags + `for_each_entity_bridged_to`, with `BagContext.module_index` set. So a dep class inheriting from another dep, or bridging a plugin namespace, still resolves method return types. The build-time edges are an optimization the dep misses, not a correctness floor. |
| imported hash-key synthesis + `resolve_method_call_types` | No | Partially — cross-file method-return queries route through the same `MethodOnClass` walk; imported-hash-key COMPLETION on a binding inside a non-open dep file would miss the synthetic `HashKeyDef`, but that's a completion-in-a-dependency scenario the LSP doesn't surface (you complete in open files). |
| `promote_provisional_dispatches` | No | **No.** Dispatch refs are only materialized at enrichment. There is no query-time path that synthesizes a `DispatchCall` ref on demand — `resolve::refs_to` for a Handler matches existing `RefKind::DispatchCall` refs in each file's analysis (`resolve.rs:362`). A `$minion->enqueue('task')` in a non-open workspace file whose receiver isa-Minion is cross-file (and which doesn't `use Minion`, so the emit-hook path doesn't fire either) is invisible to references-to-handler. |

Confirmed by `probe_dispatch_promotion_in_unenriched_workspace_file`
(FAILS).

### Verdict on B

The cross-file `MethodOnClass` walk (the structural `query_rec` fallback
the working-bag-residual doc flagged as "transitional pending
graph-walking") is doing exactly the job it was kept for: it is the
correctness floor for return-type / method resolution when a caller
bypasses enrichment. The "what if the caller bypasses enrichment?"
question from D3's residue is **answered correctly** for type resolution.

The genuine gap is **dispatch promotion**, because it produces a *ref*
(a side-effecting structural emission), not a *type answer*, and there is
no query-time fallback that re-derives the ref. The fix is to run
enrichment over workspace files in a post-index pass and re-enrich on
change — which mutates Arc'd analyses in the FileStore / module index and
introduces an ordering + invalidation lifecycle. That is the
"large/architectural" class; deferred with the failing repro.

A narrower middle option (NOT taken, noted for the implementer): emit the
`ProvisionalDispatch` candidates as a query-time source that `refs_to`
consults — i.e. teach the Handler ref query to also scan
`provisional_dispatches` whose receiver isa-resolves at query time, the
same way the `MethodOnClass` walk resolves types lazily. That keeps the
"no enrichment over deps" invariant but moves promotion to the query, like
the type path. It's the principled shape (lazy, index-at-query-time) but
touches `resolve.rs` + the dispatch model and warrants its own PR.

---

## What is correct-by-design vs broken vs latent hazard

- **Correct by design:** `overrides`, `app_surface_consumers`,
  `type_constraint_names`, and all return-type/method resolution through
  inheritance + cross-file bridges (the `parents_of` seam + `MethodOnClass`
  walk). The structural `query_rec` fallback is the deliberate floor for
  enrichment-bypassing callers.

- **Latent hazard (deferred):** cross-file `ClassIsa`-trigger emission and
  `param_types` `in_role` via a cross-file ancestor. Both stem from
  `transitive_parents` being local-only, which is forced by the
  index-free-builder contract. Fixing means moving emit-hook firing into a
  post-index pass — the same migration `dispatch_verbs` already made.

- **Broken (deferred, architectural):** dispatch-verb promotion in non-open
  files. No query-time fallback re-derives the `DispatchCall` ref, so
  references-to-handler miss call sites in unenriched workspace/dependency
  files.

## Repros

`src/builder_tests.rs`, module `param_types_manifest`:

- `probe_class_isa_trigger_through_cross_file_parent` — `#[ignore]`,
  FAILS. Cross-file `ClassIsa` trigger.
- `probe_dispatch_promotion_in_unenriched_workspace_file` — `#[ignore]`,
  FAILS. Dispatch promotion without enrichment.

Both are `#[ignore]`-d (not deleted) so the gap stays visible and a future
fix has its acceptance test ready.
