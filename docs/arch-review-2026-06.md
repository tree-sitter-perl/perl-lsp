# Architecture review — June 2026

Requested as a no-punches-pulled review. Verdict up front: **the
architecture is good; the discipline around its edges is not.** The four-layer
design, the witness bag, the reducer registry, and the plugin seam are all
sound and worth keeping. The rot is concentrated in three places: the builder's
relationship to the CST, special-case string matching that rule #10 explicitly
bans, and rules in CLAUDE.md that the code quietly violates while the doc
claims otherwise.

**A clean rewrite is not warranted.** ~28k lines of tests encode hard-won
edge-case knowledge (sigil canonicalization flavors, fat-comma traps,
right-assoc pair nesting, ERROR-node recovery). A rewrite discards that
institutional memory to fix problems that are localized and strangleable
in place. What *is* warranted is a typed layer between tree-sitter and the
builder so the 13k-line builder stops being 470 ad-hoc CST pokes in a trench
coat.

## 1. The builder has no language between it and the CST (worst problem)

`builder.rs`: 139 `child_by_field_name`, 208 `named_child` loops, 123
string-typed `kind() == "..."` comparisons. Every visitor re-derives the same
facts from raw nodes:

- "text of this field" is spelled `child_by_field_name(..).and_then(|n|
  n.utf8_text(self.source).ok()).map(|s| s.to_string())` dozens of times.
- The fat-comma/pair-walking knowledge is correctly centralized
  (`for_each_pair_node_in_children`) but lives as private methods on `Builder`
  where nothing else (plugins, future passes) can reach it.
- Semantic micro-decisions are re-made inline at each site: "is this var
  potentially the invocant/receiver" exists as a hardcoded
  `"$self" | "$class" | "$this" | "$proto"` match in `builder.rs:3718`, a
  *different* canonical-name check in `bless_class_is_receiver`, and
  name-string compares in `symbols.rs:1833` and `:1931` — four spellings of
  one question, already one observed bug (`$c` receiver not recognized,
  gold-corpus SUPER residual).
- Same story for "is this a constructor": `== "new"` appears at
  `file_analysis.rs:3496`, `:5287`, `:6991`, `builder.rs:5874`, `:10477`.
  Five sites; when "constructor-ness" ever becomes configurable (BUILD,
  plugin-declared factories) all five must be found.

**Fix (landing now): `src/cst.rs` typed-node layer.** Zero-copy newtypes over
`tree_sitter::Node`, declared by macro, that answer questions instead of
exposing structure: `MethodCall::invocant()`, `node.text(src)`, `node.span()`,
`PairList::pairs()` (separator-agnostic by construction), plus semantic
accessors — `canonical_varname()`, `is_conventional_invocant()`,
`is_constructor_name()`. One spelling per question; the gotchas
(fat-comma, paren-RHS, right-assoc nesting) get encoded once in the layer
instead of once per visitor. The builder migrates strangler-style: new code
must use it, old visitors convert opportunistically.

## 2. Rule #10 violations in core (the rule the codebase most preaches)

The audit found 21. The serious ones:

- **DBIC method-name allowlists in core type logic.**
  `file_analysis.rs:757` (`"search" | "search_rs" | "find" | ...` deciding
  row-class key ownership) and `:794` (return-projection list), plus
  `symbols.rs:2605` (DBIC meta-method diagnostic suppression list) and
  `builder.rs:10619` (`"::Result::" → "::ResultSet::"` namespace rewriting).
  This is exactly the antipattern rule #10 names, sitting in the files that
  state the rule. Full fix is `docs/prompt-dbic-as-plugin.md` (move DBIC to a
  plugin manifest); interim fix is consolidating each list to one named,
  doc-pointed table so it's one place to delete, not four to forget.
- **Hardcoded invocant names** (see §1) — fixed by the semantic accessor.
- **UI checks on `p.name == "$self"`** where `is_invocant` already exists on
  the param (`symbols.rs:1833`) — pure oversight, trivial fix.

## 3. CLAUDE.md tells comforting lies

- "**`file_analysis.rs` … No `tree_sitter` imports.**" It has ~180 lines of
  tree walking: `resolve_expression_type`'s degradation walk,
  `resolve_hash_owner_from_tree`, `call_arg_key_at`, `fq_tail_span`,
  `node_to_span`. Some of this is genuinely position-dependent
  (cursor_context.rs is its sanctioned home), some is span utility that
  belongs in the typed layer. Either move the code or fix the rule; a rule
  that's false is worse than no rule.
- "**`symbols.rs` is a thin adapter**" — at 2897 lines it contains a tree walk
  (`string_content_span_at`) and Handler-resolution semantics
  (`dispatch_target_completions`).
- "**A unified `resolve_symbol` … is planned but not landed**" — honest, but
  the cost is concrete: four divergent copies of the cursor→TargetRef glue.
  LSP references handles owned hash keys cross-file; **CLI references silently
  doesn't** — same question, different answer depending on entry point.
  Rename policy (which kinds rename cross-file) is implicit in each copy.

## 4. Layering: directionally right, two knots, and the case for crates

The measured intra-module dependency graph (who `use crate::`s whom) matches
the documented four layers — data flows down, no adapter-level imports leak
into the model. Two knots:

- **`file_analysis ↔ module_index` is a genuine cycle.** 55 `FileAnalysis`
  method signatures take `Option<&ModuleIndex>` (the query-time cross-file
  seam) while `ModuleIndex` stores `Arc<FileAnalysis>`. Inside one crate
  this is invisible; it's the thing that would block a crate split. The fix
  is classic dependency inversion: the data-model crate defines the
  capability trait (`trait CrossFileLookup { fn module(&self, name) ->
  Option<Arc<FileAnalysis>>; ... }`), `ModuleIndex` implements it, and the
  55 signatures take `Option<&dyn CrossFileLookup>`. Mechanical but wide.
- **`symbols → builder`** exists only for `default_plugin_registry()` — a
  constructor that belongs in `plugin`, not the builder. Trivial.

**Should it be a workspace of crates? Eventually yes — because Cargo turns
rules #1/#2 from doc-enforced into compiler-enforced.** A `model` crate that
doesn't depend on the parser *cannot* grow a tree walk; reviewers stop
needing to police it. Target DAG:

```
cst  ──────────►  tree-sitter + grammar     (typed view, src/cst.rs)
model             file_analysis, witnesses, conventions  (no parser dep*)
build ──► cst, model                        builder, plugin, pod, cpanfile
index ──► model, build                      file_store, module_index/_resolver/_cache, resolve
lsp   ──► all                               backend, symbols, cursor_context, main
```

*`Span` wraps `tree_sitter::Point` (already through a serde shim, `PointDef`)
— the model either owns a two-field `Point` or keeps tree-sitter as a
types-only dep (weaker guarantee).

Prerequisites, in order: (1) evict the ~180 tree-walk lines from
`file_analysis.rs` into `cursor_context.rs`/`cst.rs` (§3); (2) the
`CrossFileLookup` inversion; (3) move `default_plugin_registry` to `plugin`.
After those, the split is a mechanical move. Don't split before — you'd just
relocate the violations. No other inversion is needed; the layer *order* is
correct.

**Status (June 2026):** (2) and (3) are DONE — `file_analysis.rs` /
`witnesses.rs` no longer name `ModuleIndex` (object-safe `CrossFileLookup`
trait in `file_analysis`, `CachedModule`/`SubInfo` moved there,
`BagContext.module_index` is `Option<&dyn CrossFileLookup>`). (1) is done
for the pure node utils (`node_to_span` & co. live in `cst.rs`), but the
three lazy walkers (`resolve_expression_type`'s degradation walk,
`resolve_hash_owner_from_tree`, `call_arg_key_at`) are called from
`find_definition` / `collect_refs_for_target` / `resolve_target_at`'s lazy
tree paths and **cannot move until phase 5 (eager `Ref.target`) retires
lazy tree resolution**. The split is therefore gated on phase 5; executing
it earlier means the model crate keeps a tree-sitter dependency and the
headline guarantee (a model that *cannot* walk trees) isn't delivered.
Phase 5 is the next big rock.

## 5. What's actually fine (don't touch)

- The witness bag + reducer registry. Monotone, edge-based, single query
  path. This is the best-designed part of the codebase.
- The FileStore/RoleMask unification. `refs_to` is clean.
- The plugin system and the ReceiverGated seam.
- Parser-bug workarounds (`recover_subs_from_error_text`, `bless_args`,
  bareword-filehandle guard) — scoped, documented, upstream-blocked. Leave
  them; track upstream.
- The planned-debt docs. The phase plan (Namespace enum, eager Ref.target,
  Openness) is coherent; the pluggability decision gating phase 1 is a real
  decision, not procrastination.

## Audit corrections (where the rule-10 sweep over-fired)

`ParametricType::method_arg_owner` / `return_method_declarations` were
flagged as DBIC allowlists in core — they are actually rule-10 *compliant*:
the lists are methods **on the type**, which is exactly where the
parametric-types ADR puts them. The residual issue is only that the DBIC
flavor is native instead of plugin-declared (`prompt-dbic-as-plugin.md`).
Similarly, `resolve_invocant_class_tree` and `method_call_invocant_class`
are not retirable back-compat shims — they're the canonical build-time and
query-time entry points respectively.

## What landed from this review

1. `resolve_symbol` in `resolve.rs` — single cursor→target entry point;
   backend references/rename + CLI references/rename all route through it.
   Cross-file rename policy moved onto `TargetRef::supports_cross_file_
   rename` (ask the value). Fixes the CLI/LSP owned-hash-key references
   divergence. Owner extraction became `FileAnalysis::hash_key_owner_at`.
2. `src/cst.rs` typed-node layer (`typed_node!` macro, `NodeExt`,
   `pair_nodes`, `call_args`, `varname_child`, `canonical_container_name`,
   `is_conventional_invocant_scalar`) + `src/conventions.rs` (pure-string
   convention predicates, importable by tree-free layers). Builder's
   pair-walking trio, `extract_call_args`, `canonicalize_container`, and
   `visit_method_call` migrated; remaining visitors migrate strangler-style.
3. Rule-10: ten invocant-name compare sites routed through the one
   predicate (sig-help now prefers the authoritative `is_invocant` flag);
   five `== "new"` sites routed through `is_constructor_name`; the
   `universal_methods` framework entries pinned as debt with owning docs.
4. `default_plugin_registry` moved to `plugin` (kills the only
   adapter→builder dependency edge).
5. Prose-cutting pass: spec-part labels and history narration removed,
   the worst multi-paragraph comment blocks restated at a third the size.
6. CLAUDE.md updated: `resolve_symbol` documented as landed, typed-layer
   rule added under rule #1, new modules in the file map.

## Recommended next moves (not landed)

1. Evict the ~180 tree-walk lines from `file_analysis.rs` (§3) — also the
   first crate-split prerequisite.
2. `CrossFileLookup` trait inversion for the `file_analysis ↔ module_index`
   cycle (§4).
3. Then the workspace split (§4) — compiler-enforced layering.
4. DBIC-as-plugin (`prompt-dbic-as-plugin.md`) retires the genuine
   framework knowledge still in core (`::Result::` rewriting,
   `visit_dbic_class_method` dispatch, diagnostics meta-method entries).
