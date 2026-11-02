# Systems inventory

One entry per system that claims to be "the one X". The review agent maps a
diff hunk to an entry by lookup:

- A hunk that touches an entry's **owner** must keep the entry's tripwires
  green and run its nets.
- A hunk that matches an entry's **second-copy signature** anywhere else is a
  duplicate of that system until it is routed through the owner, or its entry
  says why not.

A new owner adds its entry in the same PR that creates it. An entry whose
tripwire is "none" is a prose-only claim. That is where the audit found every
false one (`docs/` audit, 2026-10-28; second sweep 2026-10-31), so add a
tripwire or drop the "one".

**Nets** used below:

- `cargo test`: unit and integration tests.
- gold: `gold-corpus/run.pl`, fixtures in `gold-corpus/fixtures/*.json`.
- e2e: `e2e/run.sh`, suites in `e2e/*.lua`.
- L1: the whole-analysis equivalence dump, base vs head. Empty for a
  refactor.
- L2: the behaviour census, a ledger of diagnostic and verb deltas on a real
  corpus.
- bench: `bench/measure.sh` / the `edit-bench` skill.

Every entry has the same bullets, in the same order.

## Layering and the tree

### Layer DAG

- **Decides:** which layer a module is in, and that imports only flow down.
- **Owner:** `src/layering_tests.rs::layer_of_segment` (a module's layer is its top-level directory).
- **Tripwires:** `imports_flow_down_only`, `util_tier_is_std_only`, `model_layer_cannot_walk_trees`, `grammar_stays_in_the_builder_layer`.
- **Nets:** `cargo test`.
- **Second-copy signature:** an `.rs` file directly under `src/` other than `main.rs` / `cst.rs`; a `use crate::lsp` / `crate::index` from `model/` or `util/`; `tree_sitter::` in `model/`.
- **Known debt:** none.

### Typed CST view

- **Decides:** how a Perl node is read: field text, spans, pair walks, call args, variable identity.
- **Owner:** `src/cst.rs` (`NodeExt`, `pair_items` / `pair_nodes`, `call_args`, `list_elements`, `nth_element`).
- **Tripwires:** `positional_reads_skip_grammar_extras`, `kind_comparisons_name_real_grammar_kinds`, `perl_pair_walks_go_through_pair_items` (with #367).
- **Nets:** `cargo test`, gold (all Perl fixtures), L1.
- **Second-copy signature:** `child_by_field_name(..).utf8_text(..)` chains in a visitor; raw `named_child(i)` for a position; `i += 2` over list elements; a `kind() == "fat_comma"` gate on a pair walk.
- **Known debt:** none.

### Perl parser construction

- **Decides:** the Perl grammar and parser settings every parse uses.
- **Owner:** `src/build/builder/mod.rs::create_parser`.
- **Tripwires:** `perl_parsers_come_from_create_parser` (with #367).
- **Nets:** `cargo test`.
- **Second-copy signature:** `set_language(&ts_parser_perl::LANGUAGE.into())` anywhere but the owner.
- **Known debt:** `frameworks.rs::isa_type_of_string` builds a parser per `isa` string. That is a perf item, which needs an A/B before it gets a cache.

## Language spellings

### Perl's spellings

- **Decides:** `::`, the sigils, `__PACKAGE__`, `SUPER::`, method-token and invocant shapes.
- **Owner:** `src/model/conventions.rs` (`PERL_SPELLINGS`, `PERL_SEP`, `MethodToken`, `InvocantText`).
- **Tripwires:** `language_spellings_have_one_home`, `qualified_names_split_through_the_spellings` (with #367), `packs_do_not_borrow_perls_current_package_token`.
- **Nets:** `cargo test`, gold.
- **Second-copy signature:** a `"::"`, `'$'` or `'\\'` literal outside the owner and the packs; `rsplit("::")` / `contains("::")` / `starts_with('$')` on a name.
- **Known debt:** `ParametricType::instance_from_spelling` (a C++ template parse in the model); `resolve::qualifier_at_point` (C++'s owner operator, which `NameSpellings` cannot spell yet).

### Qualified-name split

- **Decides:** whether a name is qualified, and its (namespace, leaf).
- **Owner:** `src/model/conventions.rs::split_qualified` / `join_qualified`, under the spellings of the name's language.
- **Tripwires:** `qualified_names_split_through_the_spellings` (with #367), `rendered_strings_are_not_reparsed`.
- **Nets:** `cargo test`, gold (`perl-package-identity`, `pkgid-*`).
- **Second-copy signature:** a `split` / `rsplit_once` on a separator in a consumer; a `format!("{a}::{b}")` join.
- **Known debt:** as for Perl's spellings.

### Attribute vocabulary → SymbolFlags

- **Decides:** which spelling sets which declaration flag.
- **Owner:** `src/model/file_analysis/core_types.rs::impl TryFrom<&str> for SymbolFlags`; Perl maps onto it through `conventions::field_attribute_flag`; a pack through its `@mark.*` captures (`vocab::MarkCap`).
- **Tripwires:** `one_table_maps_spellings_to_flags` (with #367), `language_spellings_have_one_home` (attribute half), `vocab_tests::a_mark_spelled_as_an_attribute_mints_that_attribute`.
- **Nets:** `cargo test`, gold.
- **Second-copy signature:** a `"name" => SymbolFlags::X` arm outside the owner; a consumer comparing an `attributes` string.
- **Known debt:** none.

### Capture vocabulary

- **Decides:** what a query capture name means.
- **Owner:** `src/build/query_extract/vocab.rs::Capture::parse`.
- **Tripwires:** `queries_recovery_tests::flow_document_captures_are_the_minters_vocabulary` (with #363); a misspelt capture fails the load.
- **Nets:** `cargo test`, gold (pack fixtures), L1.
- **Second-copy signature:** a capture name matched as a string (`== "flow.target"`, `capture_names()[i] == …`).
- **Known debt:** none.

### Pack vocabulary tables

- **Decides:** which strings a `LangPack` may declare (rule #15).
- **Owner:** the pack's `.scm` query document; `LangPack::declared_strings`.
- **Tripwires:** `pack_fields_name_no_grammar_shapes`, `pack_string_tables_are_ratcheted`, `pack_facts_fields_are_ratcheted`.
- **Nets:** `cargo test`, gold (pack fixtures).
- **Second-copy signature:** a Rust table of node kinds, field names or callee names in `build/packs/`.
- **Known debt:** the allowlist entries.

### Type-label rendering

- **Decides:** how a type reads to a human, in the language's vocabulary.
- **Owner:** `FileAnalysis::render_type` / `display_type_of`.
- **Tripwires:** `type_labels_go_through_the_language_vocabulary` (with #362).
- **Nets:** gold (`hover`, `signature-help`, `cpp-hover`), e2e (`types.lua`, `cpp_labels.lua`).
- **Second-copy signature:** `format_inferred_type(` or a vocabulary-free `.display_type(` above the model layer.
- **Known debt:** the CLI's JSON output keeps the engine spelling on purpose.

## Analysis model

### Namespace lane

- **Decides:** which namespace section is in force at a point.
- **Owner:** writes: `core_types.rs::OpenSections`; reads: `namespace_range_at`.
- **Tripwires:** `namespace_lane_is_written_through_open_sections` (with #367).
- **Nets:** gold, L1.
- **Second-copy signature:** a `NamespaceRange { … }` literal or a `namespace_ranges.push(`.
- **Known debt:** none.

### Name lookup walk

- **Decides:** which scopes a name lookup sees.
- **Owner:** `FileAnalysis::lookup_chain`.
- **Tripwires:** `raw_scope_climbs_are_containment_only`.
- **Nets:** gold, L1.
- **Second-copy signature:** a loop over `scope.parent` that resolves a name.
- **Known debt:** none.

### Ref resolution outcome

- **Decides:** where a ref's resolution landed.
- **Owner:** `Ref::binding`, read through the `Ref` projection accessors.
- **Tripwires:** `the_bake_gate_has_one_reader`, `a_narrow_axis_view_is_never_asked_a_bag_question`.
- **Nets:** gold (`definition`, `references`), L1.
- **Second-copy signature:** a new resolution field on `Ref`; a side map keyed by ref index.
- **Known debt:** none.

### Facts table lanes

- **Decides:** which lane owns a fact, and what crosses into the Surface.
- **Owner:** `model/file_analysis/mod.rs` (`RefTable`, `SymbolTable`, `PackageFacts`, `PackFacts`, `PluginFacts`); `surface_feed` destructures every field.
- **Tripwires:** the compiler (`surface_feed` has no `..`); `pack_facts_fields_are_ratcheted`; `source_tags_are_provenance_only`.
- **Nets:** L1, `cargo test`.
- **Second-copy signature:** a new `Vec` on a facts struct joined at query time (rule #14); a `WitnessSource` tag read for meaning.
- **Known debt:** none.

## Types

### Witness bag and reducers

- **Decides:** every type.
- **Owner:** `model/witnesses/` (`ReducerRegistry::with_defaults`, `query.rs::query_variable_type` / `query_sub_return_type`).
- **Tripwires:** `inferred_type_has_no_production_caller`, `every_reduced_value_match_names_its_variants`.
- **Nets:** gold (`type-at`, `hover`, `longdist-*`), L1 (registry answers), e2e (`types.lua`).
- **Second-copy signature:** a new `ReducerRegistry::with_defaults()` at a query site; a direct `InferredType` write that bypasses the bag; a materialized type re-pushed on an edge-reachable attachment.
- **Known debt:** F15. About a dozen sites build their own registry and query it directly, and `registry.rs` / `mutation_extension.rs` walk the scope chain themselves. This needs Veesh's rule call.

### Value-flow join

- **Decides:** the least upper bound of two types.
- **Owner:** `model/file_analysis/types.rs::join` / `join_all` / `join_return_arms`.
- **Tripwires:** none.
- **Nets:** `cargo test` (`witnesses_tests`), gold, L1.
- **Second-copy signature:** a reducer that folds arms by equality or "latest wins" where it means a join.
- **Known debt:** none.

### Walk-time expression typing

- **Decides:** the type of a closed-syntax expression at build time.
- **Owner:** `build/builder/emit.rs::expr_payload` via `emit_expr_witness`, then `bag_query_expr_span`.
- **Tripwires:** none.
- **Nets:** gold, L1.
- **Second-copy signature:** a walk-time function returning an `InferredType` without emitting a witness first.
- **Known debt:** G4. `visit_method.rs:199` and `chain.rs:264` call `infer_expression_result_type` directly.

### Build-time symbolic execution

- **Decides:** chain typing, return-arm refresh and invocant filling.
- **Owner:** `build/builder/chain.rs::resolve_invocant_class_tree` / `invocant_type_at_node`.
- **Tripwires:** none.
- **Nets:** gold, L1.
- **Second-copy signature:** a second builder function that walks a call chain and returns a class.
- **Known debt:** none.

## Ancestry and graph

### Graph walk

- **Decides:** the traversal over inheritance, bridges and descendants.
- **Owner:** `src/model/graph.rs::walk` (`GraphView`, `EdgeKind`).
- **Tripwires:** none.
- **Nets:** gold (`implementations`, `type-hierarchy`, `inheritance`), e2e (`inheritance.lua`, `roles.lua`), L1.
- **Second-copy signature:** a recursive or stack walk over `parents_of` / `declared_parents` outside `graph.rs`.
- **Known debt:** F7. `collect_ancestor_methods` (`ancestry.rs`), `ConclusionMap::inherited` (`conclusions.rs`) and `transitive_parents` (`chain.rs`) are their own walks. Plugin triggers ask isa through the last one.

### App-surface edge

- **Decides:** whether a class gets the synthetic app-surface parent.
- **Owner:** `ancestry.rs::app_surface_parent`.
- **Tripwires:** `app_surface_edge_has_one_speller` (with #367).
- **Nets:** e2e (`mojo_plugins.lua`), gold.
- **Second-copy signature:** `consumers.iter().any(|c| c == class)`.
- **Known debt:** none.

## Resolution

### CandidateSet

- **Decides:** what a cursor names, for every navigation verb.
- **Owner:** `index/resolve/mod.rs::resolve` and its projections (`definitions`, `references`, `rename_edits`, `highlights`, `implementations`, `complete`).
- **Tripwires:** `query_verbs_route_through_run_query`, `slot_detection_dispatches_on_driver_caps_not_language_names`.
- **Nets:** gold (`definition`, `references`, `rename`, `implementations`, `document-highlight`, `linked-editing`, cpp/php equivalents), e2e (`cross_file.lua`, `core.lua`).
- **Second-copy signature:** an LSP handler or CLI verb iterating `OpenDocs` or the module index; a `match RenameKind` in a handler.
- **Known debt:** none since #359. Completion's slot detection stays outside by design (the ADR's honest-boundary list).

### Visibility scope

- **Decides:** which files a query can see from an origin.
- **Owner:** `cross_file.rs::VisibilityAxis::for_origin` and `ScopedLookup::for_origin`; `resolve::origin_scope`.
- **Tripwires:** the private `ScopedLookup::new` (with #359).
- **Nets:** gold (`cpp-visibility-gate`, `incdual-definition`), e2e (`cpp.lua`).
- **Second-copy signature:** `VisibilityAxis::IncludeClosure` / `SearchPath` named in a consumer.
- **Known debt:** `TODO(perl-scanned-scope)`: Perl scanned files stay on the plain index in the backward walk.

### Include target

- **Decides:** which header an `#include` names.
- **Owner:** `build/cpp_reparse/gather.rs::resolve_include`, minted as `IncludeDirective::target` at build.
- **Tripwires:** none.
- **Nets:** gold (`cpp-cross-file`), e2e (`cpp_header_edit.lua`).
- **Second-copy signature:** a filesystem probe for a header path outside `gather.rs`.
- **Known debt:** none.

### Import binding

- **Decides:** whether a name is bound by a `use`.
- **Owner:** `model/file_analysis/imports.rs::imported_names`, classified in `index/resolve/imports.rs`.
- **Tripwires:** none.
- **Nets:** gold (`reexport`, `diagnostics`), e2e (`cross_file.lua`).
- **Second-copy signature:** a consumer reading `import.names` / `@EXPORT` directly to decide binding.
- **Known debt:** none.

## Index and storage

### Edge index writes

- **Decides:** the reverse maps (names, bridges, children, specs, includers).
- **Owner:** `index/module_index/parts.rs::ModuleEdgeIndexes::feed` / `publish_spec` / `publish_child` (plus `publish_includer`, with #359).
- **Tripwires:** none.
- **Nets:** `cargo test` (`module_index_tests`), gold (cross-file fixtures).
- **Second-copy signature:** a `.insert` into an `edges.*` map outside `parts.rs`.
- **Known debt:** `rebuild_reverse_index` clears and re-feeds only the name-keyed winners, which can drop pack `children` / `specs` edges on the CLI (found in #359).

### Name-keyed winner slot

- **Decides:** which provider `get_cached(name)` answers.
- **Owner:** `index_core.rs::IndexCore::set_slot` / `clear_slot` / `offer_slot` (with #367), with `best_candidate` as the tie-break.
- **Tripwires:** `name_slot_writes_move_the_epoch` (with #367).
- **Nets:** `cargo test`, gold (warm pass), e2e.
- **Second-copy signature:** `.cache.insert(` / `.cache.remove(` / `.cache.entry(` in `index/`.
- **Known debt:** none.

### Residency and strip

- **Decides:** which axes a resident copy has evicted.
- **Owner:** `lifecycle.rs::FileAnalysis::evict_to(Residency)`; `registration.rs::prepare_pack_parts` / `prepare_workspace_parts` for the read-whole-before-evict order.
- **Tripwires:** `whole_copy_registration_sites_are_allowlisted`; the post-bulk `persist.rs::residency_tripwire`; private `evict_*` setters (with #367).
- **Nets:** gold (warm pass; `warm-FAIL` is the signal), bench (RSS).
- **Second-copy signature:** a call to an `evict_*` setter; a registration path that strips before projecting the feed or the Surface.
- **Known debt:** the residuals in `docs/prompt-storage-residuals.md`.

### Cache sizing

- **Decides:** how big each derived-copy pool may grow.
- **Owner:** `index/cache_policy.rs` (`sweep_bag_cache_cap`).
- **Tripwires:** none.
- **Nets:** bench (RSS, wall).
- **Second-copy signature:** a `_MB` constant or a `* 1024 * 1024` cap with an env override outside the owner.
- **Known debt:** G1. Stock caps and env reads are spelled per pool: `module_index/queries.rs:108`, `registration.rs:755-764` and `:1289`, `cpp_reparse/cache.rs:322-361`, `persist.rs:140`, `backend/mod.rs:34`.

### SQLite access

- **Decides:** the one connection open and the registered statements.
- **Owner:** `index/module_cache/conn.rs`; `sql.rs` statements.
- **Tripwires:** `sql_runs_only_registered_statements`, `persist_queue_producers_are_allowlisted`.
- **Nets:** `cargo test` (`module_cache_tests`), gold (warm pass).
- **Second-copy signature:** `Connection::open` outside `conn.rs`; an inline SQL string.
- **Known debt:** none.

### Open-document enrichment

- **Decides:** the enriched copy of an open document.
- **Owner:** `OpenDocs::enrich_open`; closed files through `ModuleIndex::enriched_snapshot`.
- **Tripwires:** none.
- **Nets:** e2e (`cross_file.lua`, `cold_window_heal.lua`), gold.
- **Second-copy signature:** `enrich_imported_types_with_keys(` in the server; a deep-copy-and-enrich in a handler.
- **Known debt:** F22. The CLI enriches by hand at nine sites in `cli/query.rs`. It is skipped because the engine retires enrichment.

## Instrumentation

### Process exit

- **Decides:** that measured runs flush before the process ends.
- **Owner:** `util/ghost_stats.rs::exit_flushed` (with #364); `cli::exit_with` delegates.
- **Tripwires:** `process_exits_flush_instrumentation` (with #364).
- **Nets:** `cargo test`.
- **Second-copy signature:** `std::process::exit(` anywhere but the owner.
- **Known debt:** none.

### Timing and counters

- **Decides:** how a region is timed or counted, and its output format.
- **Owner:** `util/timings.rs` (`phase`, `PhaseGuard`, `tphase!`, `bphase!`), `util/ghost_stats.rs` (`count`, `count_by`, `timed`).
- **Tripwires:** `time_valued_env_vars_name_their_unit`.
- **Nets:** bench.
- **Second-copy signature:** an env-gated `eprintln!` timer; a `"[PHASE]"` string outside `util/`.
- **Known debt:** G2. `index/document.rs:83` prints its own `[PHASE]` line.
