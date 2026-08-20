# System-Level Rework Hitlist

Synthesis of an eight-lens architecture audit (layer placement, shape branches,
parallel structures, perl/pack duality, split quality, contract debt,
ownership/concurrency, data-model shape) with adversarial verification of every
finding. 41 findings audited: **27 confirmed**, 11 already owned by the ledger
(listed at the end), 3 rejected on re-read and dropped.

Ranking is leverage-per-effort: what unblocks or de-risks the most future work
per unit of migration cost. Effort tags: S (hours), M (days), L (a slice),
XL (an arc).

---

## Theme A — One resolution spine: projections, not parallel resolvers

The CandidateSet ADR's promise is "identity minted exactly once." Four
confirmed findings show verbs and tiers that still mint their own.

### A1. LANDED — `method_call_invocant_type` is THE invocant ladder; `method_call_invocant_class` is its dispatch projection

`method_call_invocant_type` (`src/model/file_analysis/resolution.rs`) is the
one invocant ladder — token-blind receiver-VALUE resolution (bridged /
positional / flow-narrowed place / function-call receiver / exact-span read
incl. Parametric-intact chain receivers / cross-file chain fallback / variable
/ bareword; a rung answers only when its type carries a dispatch class, so
classless answers fall through). `method_call_invocant_class` is its dispatch
projection: the SUPER/qualified method-token arm (a token overrides where
lookup STARTS, never what the receiver IS) + `dispatch_class_of` + the DBIC
source-moniker resolve. `method_call_invocant_class_raw` is deleted;
`fix_chain_receiver_hash_key_owners` asks `method_arg_owner` with the
unqualified method name, so `$rs->SUPER::search({k => 1})` now fills the
row-class key owner (pinned by
`super_qualified_search_still_fills_hash_key_owner`). The three-text-resolver
collapse (docs/prompt-cst-migration.md item 3) lands on this seam.

### A2. LANDED — documentHighlight and linkedEditingRange are CandidateSet projections; the in-file family is one occurrence union

`CandidateSet::highlights()` is the origin-file-narrowed image of
`references()` (same identity, same matcher — `refs_to_in_file` runs
`collect_from_analysis` + delegation aliases against the origin only),
carrying the now-live `RefLocation.access` for highlight kinds;
`linked_editing_spans()` is its co-edit subset (rewritable, bare-text
members only — affix-derived accessors join highlights but never co-edit),
equal to the rename image's site set by construction. Both LSP handlers and
CLI mirrors construct the set; `find_highlights` (and its bespoke
cross-file grouping fallback) is deleted. The in-file family collapsed to
`FileAnalysis::find_occurrences` — the one access-classified occurrence
union (`find_references` is its span projection, `rename_at` its edit
projection, the set's Local arm reads it directly), with declarations
always included (the set's Declaration-row convention) and `resolve_target_at`
reduced to its Local-arm resolver. Unifying on the references spelling
changed the drifted our-var linked-editing rows (PackageVar walk: FQ +
interpolated reads join, name-token spans) — re-pinned gold.
`candidate_set_visibility_axis_flows_to_every_projection` asserts both
verbs narrow with the others.

### A3. LANDED — Perl hover is a presenter over the set's resolution

Both hover handlers construct the CandidateSet (the same construction as
goto-def) and present: pack renders `hover_candidate()`, Perl renders
through the model hover primitives (`FileAnalysis::hover_info`) plus
`CandidateSet::function_binding()` — the one spelling of the cross-file
call-binding lanes (import classification first, then the FQ `Function`
package), consumed by BOTH `definitions()` and the Perl presenter
(`symbols::perl_hover`), so hover presents exactly what goto-def would
reach. The adapter's own resolution chain (`resolve_imported_function` +
FQ-arm re-derivation in `hover.rs`) is deleted; builtin hover keeps its B2
shape (membership from `model::builtins`, doc value from
`module_index.builtin_doc`). The renderer-*placement* half (Perl markdown
assembled in the model vs pack's in the adapter) stays owned by the parked
multi-language brief.

### A4. LANDED — the MCB pass is the MCB→bag bridge, edges not values

`emit_method_call_binding_edges` (finalize + the enrichment re-run,
append-only post-finalize like `emit_mutation_extension_witnesses`)
publishes each `$var = $invocant->method()` binding as a
`Variable → Edge(PackageSymbol{package, method})` witness (tag `mcb`); the
registry chases the return lazily with whatever index the QUERY holds, so
enrichment materializes no values and no early-out is needed — the fold's
own precedence arbitrates. `FileAnalysis::inferred_type` is `#[cfg(test)]`
(raw-seed-state assertions only;
`layering_tests::inferred_type_has_no_production_caller` pins it). MCB
hash-key ownership keys on {invocant class, method}: the fold fixup
resolves the invocant via the bag and uses the class-defining symbol's
package (name-only fallback only for untyped invocants / non-local
definers — the fold holds no MRO walker), and the query-time owner path
(`resolve_hash_key_owner`) walks `resolve_method_in_ancestors` to the
defining symbol. The two stale bag→legacy-fallback comments are gone;
`EXTRACT_VERSION` bumped for the bag rule change.

---

## Theme B — Analysis belongs to the model; adapters render

### B1. Diagnostic detection logic is semantic analysis embedded in the LSP adapter — **LANDED**

`unknown-hash-key` detection rides two FileAnalysis query seams next to
`closed_shape_is_whole_story` (`queries.rs`): `closed_shape_key_typos`
(the HashKeyAccess refs walk, sigil/write gating, bag query, closed-shape
match, whole-story trust gate) and `projected_key_typos` (the `Projected`
witness enumeration, the base-is-variable exclusion, the
`expr_type_at_span` materialization). Both return `KeyTypoSite` (span, key,
untruncated known_keys, base spelling — `dispatch.rs`, next to the other
diagnostic site structs). `collect_diagnostics` renders sites into
`Diagnostic`s (message wording, five-key elision, severity) and holds no
`crate::model::witnesses` import; the builder-layer helper test asserts on
`resolve_method_in_ancestors` instead of reaching up into lsp/.
docs/adr/narrowing-diagnostics.md names the seams alongside D1-D6.

### B2. The Perl builtin surface is three parallel encodings across three layers — **LANDED**

`model/builtins.rs` is THE Perl builtin surface: one sorted table
`name → (BuiltinKind, return-type, first-arg-type)` sourced from
perlfunc.pod. The adapter's `PERL_BUILTINS`/`is_perl_builtin` are deleted;
diagnostics suppression asks `is_builtin` (plus
`conventions::is_constructor_name` for indirect-object `new`), builtin hover
gates on the same membership, the builder's typed seeding reads the table's
type columns, and the BUILTIN RoleMask tier has its name source: `Function`
rows feed `complete()` as candidates (Perl origins only — the pack arm never
reaches the table). `index/builtins_pod.rs` stays the doc-VALUE store;
`builtins_pod_tests.rs` carries the anti-drift tripwire (perlfunc entries ⊆
table modulo a documented prose-noise set; `Function` rows ⊆ perlfunc). The
realized drift (`exp`, `fc`, `evalbytes`, `lock` typed/documented but
flagged unresolved) is pinned by tests in `model/builtins.rs` and
`diagnostics_tests.rs`.

---

## Theme C — Pack routing and capabilities decided at construction, not per handler

Both items are confirmed now-sized slices consistent with (and shrinking) the
parked full unification in docs/prompt-unify-language-paths.md.

### C1. Pack routing is a construction fact on the CandidateSet — **LANDED**

Pack routing splits into its two real facts, each with one owner. The POLICY
fact (pack semantics on the set: VISIBLE widening, rename full-or-refuse,
pack def_paths) is derived inside `resolve()` from the origin's stamped
`FileAnalysis.language` (`#[serde(default = "perl")]`, stamped by
`PackDriver::analyze_with_path`, read via
`LanguageRegistry::is_pack_language`) — `pack_routed()` is deleted, so no
handler declares or can forget it, and every projection inherits it by
construction. The STORE fact (hub vs pack sub-index) has one speller,
`ModuleIndex::lookup_for(language) -> RoutedIndex` (an owning hub-or-pack
value handlers hold and pass into `resolve()`); the
`pack_store_selection_stays_in_lookup_for` layering tripwire keeps
`pack_index()` out of the LSP layer. All ~13 handler/CLI preambles are
one-line `lookup_for` calls now. `resolve()` cannot take the hub and route
internally because a pack sub-index is an `Arc` out of the hub's registry —
the set borrows, so the caller must own the routed store's lifetime; that is
what `RoutedIndex` is.

### C2. LANDED — driver capabilities are asked of the pack from one shared home

`LanguageRegistry::has_include_tokens` / `has_preprocessor_macros`
(`build/language_driver.rs`, beside the registry) are THE boolean capability
askers; the LSP handlers and their CLI/--batch mirrors gate the include-token
lanes on the same call, so editor and gold answer identically by construction
(the CLI's `lang_id == Some("cpp")` probes are deleted — the server's
asked-never-named spelling was the correct side). Two askers is the recorded
ceiling (docs/PARKED.md): the third collapses to a generic
`pack_cap(lang, sel)`. `capability_askers_answer_by_language_id` pins the
by-id answers; the lifecycle `language != "perl"` policy branches stay owned
by the parked unification.

---

## Theme D — Serving-path ownership: state mutated by its owner, coordination proven once

### D1. LANDED — open-doc enrichment is a derived artifact with one writer

`FileStore::enrich_open(url, idx)` is THE open-doc enrichment writer:
clone-and-enrich off the store lock, ptr-guarded swap, returns the derived
`Arc<FileAnalysis>` for the caller to read. `publish_diagnostics` and the
bulk paths (the resolver `on_refresh` closure and the perl cold-open heal,
both through `refresh_open_diagnostics`) read the returned artifact — no
notification handler mutates a stored analysis, and `for_each_open_mut` is
deleted. The record-surface-BEFORE-publish ordering contract is retired
structurally: freshness records read `Document::baseline_surface` (projected
at every build seam from the pristine analysis, recorded via
`record_and_dirty_value`), so enrichment state cannot reach a surface record
no matter when either runs. Pinned by
`enrich_open_swaps_derived_copy_and_keeps_baseline_surface`.

### D2. LANDED — the resolver thread and ModuleIndex share one owned core (`IndexCore`)

`index/module_index/index_core.rs` owns the shared mutable state — cache,
edge indexes, loader-config shapes, stale/available sets, builtins, resolve
queue/notify, workspace-root channel, generation map + counter, long-lived
flag, bag-cache cell — as ONE struct held via a single `Arc` by `ModuleIndex`
(async side) and the resolver thread (blocking side); the 13-Arc plumbing and
the free-fn twins (`insert_into_cache`, `rebuild_reverse_index`,
`mint_registration_gen`, `stamp_missing_import_gens`) are deleted.
`IndexCore::insert_resolved` is the one spelling of "a resolution landed":
stale-pin clear → generation mint → whole-analysis projections (edge feed +
loader shapes) → registration-owned strip (`strip_import_copy`, core-owned) →
store, with the None-never-clobbers guard; `ModuleIndex::insert_cache` and
the thread both route through it. This fixes the realized drift (an
@INC-resolved plugin-carrying module never fed `loader_config_shapes` on the
thread path — the projection also had to move PRE-strip, since it reads the
witness bag the strip drops). `resolver_loop` is the single loop body,
parameterized by `Option<ServerSession>` (builtins hydrate, warm strip, stale
priority, cpanfile scan, dependency descent, progress are explicit server
gates); the test-loop divergences (no bag-cache stale-pin clear; memoized
None) are unified on the main spelling. Pinned by
`thread_path_resolution_feeds_loader_config_shapes` and
`insert_resolved_none_does_not_clobber_indexed_module`.

### D3. LANDED — pack invalidation is one index-side subsystem (`PackInvalidator`)

`src/index/pack_invalidator.rs` owns the serialization lock, the H9-2
bulk-index coordinator (`PackChangeCoordinator`, relocated), and the H9-1
source-generation guard (`claim_source_gen` + its map, relocated off
`ModuleIndex`). Entry points: `file_changed(root, hub, open_docs, path,
deleted) → InvalidationOutcome { deferred, refresh_open }` and
`begin_bulk_index`/`finish_bulk_index`; the eviction/re-analysis/swap worker
is private, so a new invalidation path cannot compile around lock,
coordinator, or guard. The include-closure consumer rule is ONE predicate
(`is_consumer`) applied to registered files and open docs alike; the
realized drift — Backend refreshed open consumers even on an Unchanged
surface verdict — is unified on the gated spelling (open consumers skip
too; the changed file's own open doc always refreshes), pinned by
`surface_gate_covers_registered_and_open_consumers`. Backend shrinks to
forwarding events and publishing `refresh_open` through the gather
single-flight. The H9 race tests live with the owner
(`pack_invalidator_tests.rs`).

### D4. LANDED — the blocking decision rides the query API (`Backend::run_query`)

`lsp/backend/query.rs` owns the single blocking hop: `run_query` mints a
`QueryCx` (store + hub Arcs) inside `spawn_blocking`, and every resolving
handler — goto-def, implementations, references, rename, prepareRename,
hover, documentHighlight, linkedEditingRange, workspace/symbol — reaches
set construction (`QueryCx::set`), pack routing (`QueryCx::routed`), the
relational row search (`QueryCx::sym_rows`), and the raw-word rehydration
lane (`QueryCx::pack_xfile_word_at`, relocated off `Backend`) only through
that context, off the reactor. The three verbs that did SQLite/fs I/O
inline (goto-def's and hover's raw-word lanes, workspace/symbol's sweep +
rows pass) now run behind the hop; references/rename's hand-rolled
spawn_blocking twins are deleted onto it.
`layering_tests::query_verbs_route_through_run_query` pins the raw
spellings out of `server.rs`, so a new verb cannot grow an inline I/O path.

### D5. LANDED — one spelling each for the bounded wait and the settle-window debounce (`lsp/backend/gates.rs`)

`ReadyGate` (one-way latch + Notify; `armed_wait` registers interest BEFORE
the final re-check — the lost-wakeup proof lives on the type, unit-tested
once) backs all three bounded-wait sites: the per-family `IndexReady` gates,
the per-URI `opening` map, and the per-URI `degraded_open` map — callers keep
their probe closures. `DebouncedLatest` (generation-captured settle-window
debounce; `fire` runs only the latest surviving fire, `Latest::still`
re-probes mid-job) backs both debounce sites: `spawn_debounced_rebuild`
(replacing the `change_gen` map of raw counters) and the resolver's
diagnostics-refresh callback, whose body moved out of `Backend::new` into
`make_on_refresh`. `GatherRegistry` stays distinct (single-flight, not a
debounce); `PackChangeCoordinator` and `claim_source_gen` were already eaten
by D3's `PackInvalidator`.

---

## Theme E — The data model carves its own joints

### E1. LANDED — Surface::project's mirror of FileAnalysis is now structural

`FileAnalysis::surface_feed(&self) -> SurfaceFeed`
(`model/file_analysis/surface_feed.rs`) destructures every FileAnalysis field
with no `..` rest pattern — 14 fields bound into the feed, the rest discarded
under grouped why-not-visible comments — so a new field is a compile error
until classified; `Surface::project` reads only through the feed (the
`analysis` handle carries the three derived queries). The two leaks are
projected: `export_tags` (sorted tag → members) and `dbic_source_name`, each
with an equality-net arm in `surface_tests.rs` proving a header-only edit
flips the verdict to Changed. STUB_VERSION bumped (Surface rides the stubs
table). R1 is restated as compiler-enforced in CLAUDE.md and
`docs/adr/storage-engine.md`.

### E2. LANDED — FileAnalysis carves along its own joints: five sub-structs, one owner each

`FileAnalysis` is a table of lanes, not a flat field list. `PackageFacts`
(`mod.rs`) is one entry per package — parents, uses, framework, requires,
role-ness, dynamic-parents — behind `FileAnalysis::packages`, so a
per-package join is one lookup and `Surface::project` destructures the
entry exhaustively. `RefTable` (`ref_table.rs`) and `SymbolTable`
(`symbol_table.rs`) are the reference and symbol axes: the vec, every
index over it, its `evict()`, its `heap_add()` arm and its enrichment
baseline in one owner, with the vec private so an index cannot survive its
rows. `PackFacts` (`pack_facts.rs`) is the pack lane (receiver names,
specialization edges, template params, macro defs, include directives and
closure, domain sites, move/control/param regions) and `PluginFacts`
(`plugin_facts.rs`) the plugin lane (namespaces, loader facts,
diagnostics, gated emissions, app-surface consumers) — both empty by
default, so "this is a plain Perl analysis" is two empty sub-structs.
`FileAnalysisParts` and `new()`'s destructure carry the lanes as
themselves (the symbol/ref axes stay flat vecs the builder appends to and
the tables adopt), `surface_feed` destructures every sub-struct
exhaustively so a new lane field is a compile error until its Surface fate
is decided, and each sub-struct assembles its own heap-probe bucket.
Phase commits: `rework(E2.1): PackageFacts`, `rework(E2.2): RefTable`,
`rework(E2.3): SymbolTable`, `rework(E2.3b): PackFacts and PluginFacts`,
`rework(E2.4): the Parts boundary moves lanes`.

### E3. LANDED — Ref's resolution outcome lives in one home: `Ref::binding`

`Ref::binding: Option<RefBinding>` (`core_types.rs`) is the one home for
every resolution outcome — `Symbol(SymbolId)`, `Function { package }`,
`Method(MethodTarget)`, `HashKey { owner, sym }`, `Handler { owner, sym }` —
replacing the deleted `resolves_to` / `resolved_method_target` flat columns
and the deleted `FunctionCall.resolved_package` / `HashKeyAccess.owner` /
`DispatchCall.owner` variant payloads (RefKind is pure written shape again;
`GatedRef` carries the same binding). Consumers read through the projection
accessors (`resolved_symbol` / `method_target` / `resolved_package` /
`hash_key_owner` / `handler_owner`) and post-passes stamp through the
`bind_*`/`link_owned_symbol` mutators, so no call site matches `RefBinding`
against `RefKind` itself. `row_seed` derives the same qual columns from the
binding (row format unchanged — no REF_ROWS_VERSION bump); EXTRACT_VERSION
bumped (175→176). The `Function { sym }` slot was dropped — no path mints a
FunctionCall→symbol link today, and a dead field is not a seam. `RefTable` owns
the field's home now; the binding shape is unchanged by that move.

### E4. LANDED — symbol presentation policy is one home: `Symbol.presentation`

`Presentation { hide_in_outline, display, label }` (`core_types.rs`,
`#[serde(default)]`) is minted at symbol synthesis — builder sites via
`presentation_mut`, plugin emit / gated emissions from the action-level
fields (`EmitAction::Symbol` grew `display`/`hide_in_outline` alongside
`Method`/`Handler`'s), the pack skeleton conversion stamps the
include-guard verdict. `SymbolDetail::Sub`/`Handler` no longer carry
`display`/`hide_in_outline`, `Symbol.outline_label` is
`presentation.label`, and every view (outline, workspace-symbol rows,
heatmap, completion icons, CLI dumps) reads the one home —
`hidden_in_outline()` is a plain read, `sub_display_override` is deleted.
Kind-semantic flags (`is_constant`, `opaque_return`, `lexical`) stay in
the detail. Row flags unchanged; EXTRACT_VERSION bumped.

---

## Theme F — The tree tells the truth: splits, placement, ledger hygiene

### F1. LANDED — file_analysis's query parts are cut by concern

Each concern lives in ONE part behind the unchanged mod.rs glob surface:
`hover.rs` holds every markdown hover renderer (`hover_info`, `member_hover`,
`format_handler_hover`, the `format_symbol_hover` pair — the file the parked
multi-language hoist lifts from); `ancestry.rs` holds the parent-enumeration
seam (`parents_of`), the bounded isa walkers (`walk_ancestry`, `class_isa`,
`class_isa_prefix`), the include-self MRO walk with its method resolution
(`for_each_ancestor_class`, `resolve_method_in_ancestors`,
`resolve_super_method`, `method_resolution_on_class`,
`class_has_unresolved_ancestor`), and the family/descendant walks (placement
only — the GraphView collapse stays a separate PARKED item); `sym_index.rs`
holds the raw symbol/ref index accessors, with `sym_row_seeds` beside the
Surface classification gate in `surface_feed.rs`; the resolution residue is
`invocants.rs` (target-at-cursor, the invocant/dispatch ladders, role
contracts, class-content predicates), un-shadowing `index/resolve/`.

### F2. LANDED — module_resolver and module_cache are directories of focused parts

`index/module_resolver/`: `thread.rs` (the one resolver loop), `inc.rs`
(@INC discovery + module-file parse with the parent-fallback memo),
`index_perl.rs` / `index_pack.rs` (the sibling bulk indexers — the
confrontable two-file diff for the parked language-path convergence),
`persist.rs` (residency policy + `run_persist_writer` + the stamp-guarded
analyze protocol); `index/module_cache/`: `conn.rs` (opens), `schema.rs`
(DDL + generation gates), `blob.rs` (codec + stamps + rehydration),
`rows.rs` (relational store), `stubs.rs`, `warm.rs` (the three warm
lanes). mod.rs keeps entry points and glob re-exports, so public paths and
the whole-copy registration allowlist counts are unchanged; the ordering
invariants (stub delete inside `save_to_db`; register-after-chunk-commit)
moved intact inside their functions. (`watch.rs` was already D3's
`pack_invalidator.rs`.)

### F3. LANDED — helpers.rs and infra.rs families live with their sibling seams

`build/builder/` has no grab-drawer: the bless family is `visit_bless.rs`,
POD doc capture is `docs.rs`, AUTOLOAD/__DATA__ synthesis sits beside the
pipeline synthesis passes in `pipeline.rs`, the DBIC resultset-parametric
family lives in `visit_method.rs` (one source file for the parked DBIC
phase-3 lift), and `add_fold_range` is in `visit_decl.rs`; the residue is
`extract.rs` under a tight tree-reading charter. `infra.rs` keeps scope /
symbol-minting / package-range / call-arg infrastructure plus
`coderef_return_edge_for`; flow-edge minting lives in `narrowing.rs`
(docs/adr/flow-narrowing.md maps to exactly one part) and the plugin
`ArgInfo` factory in `plugin_emit.rs`.

### F4. LANDED — the walk contract carries a prune verdict; the two bespoke ancestry walkers are GraphView walks

`GraphView::walk`'s visitor returns `WalkControl`
(Continue/PruneChildren/Stop, `model/graph.rs`), and the synthetic
app-surface parent is its own maskable edge kind (`EdgeKind::AppSurface`
/ `EdgeKindMask::APP_SURFACE`; `real_parents_of` + `app_surface_parent`
are the two component spellers `parents_of` composes). `trigger_view_at`
is `walk(Class(pkg), INHERITS, idx=None)` (surface masked, local-only);
`unfulfilled_role_requires` gathers over the INHERITS walk pruning at
every non-role node (role-contract edge semantics preserved). Both
hand-rolled BFSes are deleted and both inherit the walk's MAX_DEPTH;
full-MRO consumers pass `INHERITS | APP_SURFACE` — which is also the
mask the parked walk_ancestry→GraphView collapse was waiting on.
Prune + pathological-depth tests pin the contract in `graph_tests.rs`.

### F5. LANDED — small truth-telling fixes

`src/util/` is the neutral leaf tier (std-only, no crate paths — enforced
by `layering_tests::util_tier_is_std_only`) and holds `timings.rs`;
`cpp_obstacle_test_corpus.rs` is recognized as test fixture data by the
layering walk's `_test_corpus` stem predicate; the `refs_present` ghost is
scrubbed — comments, CLAUDE.md, and docs/adr/relational-ref-index.md all
state the real shape (no refs-axis reader; the backward walk goes through
`whole_present`, and a single-axis refs view must not be minted).

---

## Theme G — Plugin-owned vocabulary, not core allowlists

### G1. LANDED — Moo-family `has` semantics are one plugin-declared manifest

`frameworks/moo.rhai`'s `framework_mode_makers()` declares module → flavor
(`"Moo"`/`"Moose"`) + exported keyword surface, and `triggers()` is derived
from it, so the plugin's accessor-option gate and core's native-synthesis
gate share one declaration. The builder bakes `framework_mode_modules:
HashMap<module, (FrameworkMode, keywords)>` at plugin load; `visit_use`
looks consumers up (Mojo::Base stays a structural core arm) and the `option`
keyword is gated per-package via `package_imports_framework_keyword`. Core's
Moo/Moose match arms and the `package_uses_moox_options` module check are
deleted. The realized Mouse drift resolved toward full support (Moose
flavor: native accessor + plugin options), pinned by
`mouse_has_gets_both_native_accessor_and_plugin_predicate`; the open seam is
pinned by `test_plugin_declared_framework_mode_maker_grants_has_semantics`.

### G2. LANDED — name-dispatched action exemptions are per-rule plugin vocabulary

`plugin::ParamType.implicit_action_names` (`#[serde(default)]`) declares the
sub names a framework dispatches by name alone — they pass that rule's
`requires_action_attr` gate; catalyst.rhai declares
`begin`/`end`/`auto`/`default`/`index` on its two attribute-gated wildcard
rules, and core's `collect_param_type_matches` checks per-rule (the
`CATALYST_PRIVATE_ACTIONS` const is deleted). A non-Catalyst rule that
declares no names exempts nothing, pinned by
`attr_gated_rule_without_declared_names_exempts_nothing`.

---

## Suggested sequencing

1. **Cheap truth + drift stoppers (S):** F5 (timings, cpp_obstacle,
   refs_present), G2, C2, F3.
2. **Correctness-bearing seams (M):** E1 (SurfaceFeed + two backfills), A1
   (invocant ladder), B2 (builtins), B1 (diagnostics seams), G1 (Moo gate),
   C1 (pack routing — LANDED), D3 (PackInvalidator — LANDED), F1 (file_analysis
   recut — LANDED), E2 phase 1 (PackageFacts — LANDED).
3. **Structural slices (L):** D1 (enrichment as derived artifact), D2
   (IndexCore — LANDED), A2 (highlights/linked-editing projections — LANDED), F2
   (monolith directories — LANDED), A3/A4, D4/D5, E3 (LANDED).
4. **The arc:** E2 phases 2-3 (LANDED, + E4 alongside).

---

## Known-parked (owned elsewhere — not news)

Verified real, but deliberately owned by the ledger; act on them through their
owning doc, not this list.

- Cursor-context detection split across lsp/ (Perl) and build/ (pack), stitched
  by language branches — CLAUDE.md rule 6 + docs/prompt-unify-language-paths.md.
- Per-verb `language != "perl"` routing across ~12 files —
  docs/prompt-unify-language-paths.md (its three-file seam list is stale;
  amend to the current spread, and C1's `lookup_for` is the doc's own
  opportunistic-convergence slice).
- `universal_methods` DBIC/Moose meta-method allowlist in diagnostics —
  docs/prompt-dbic-as-plugin.md item 2.
- Runtime-exporter recognition module-name allowlist in visit_calls —
  docs/open-problems.md (deferral rationale recorded there).
- Three byte-capped LRU cache cores — docs/PARKED.md (re-examine on a fourth;
  add a cross-link to prompt-storage-residuals.md's R4 dedup residual, which
  is standing split-cost evidence).
- Pack completion lanes bypassing CompletionCandidate / two expected-type
  re-rankers — docs/PARKED.md + docs/prompt-unify-language-paths.md step 3.
- Slot consumers forked above the seam (two disjoint Slot projections) —
  docs/prompt-unify-language-paths.md step 3.
- Two hover renderers at two layers — docs/prompt-multi-language.md (names
  hover rendering as the driver-side move); add a hover row to
  prompt-unify-language-paths.md's table (cheap doc edit).
- Three divergent text→class invocant resolvers —
  docs/prompt-cst-migration.md item 3; sequence immediately after A1 and
  annotate the item with the re-typed-`$self` and missing-index divergences.
- Watcher re-registration pins whole copies (never re-strips) —
  docs/prompt-storage-residuals.md (design sketch recorded there).
- DBIC verb tables hard-coded in `ParametricType` beside plugin-declared
  `column_keyed_verbs` — docs/prompt-dbic-as-plugin.md phase 3; add a one-line
  addendum that both lists are consulted in the SAME builder pass today
  (fold.rs:592/617/644), which is sharper than the brief records.

## Rejected on verification (dropped)

- Mojo route-brand vocabulary in chain.rs — the residence is decided by
  docs/adr/route-branding.md; generalizing now is speculative seam-building.
- PERL_LSP_BENCH as a dead bootleg timer — it has live consumers in
  scripts/qa/ (run-bench.sh, analyze-bench.sh, README.md); migrate-not-delete
  if ever touched.
- Eviction-as-boolean-flags — the read side is policed at the documented
  boundary seam (docs/adr/relational-ref-index.md:198-223); `Evictable<T>`
  would tax hundreds of in-file readers for a hazard they cannot hit; the
  legitimate kernel (axis owns its indices) is delivered by E2.
