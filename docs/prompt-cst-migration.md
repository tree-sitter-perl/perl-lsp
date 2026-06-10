# cst/conventions migration backlog

The typed CST layer (`src/cst.rs`) and the name-semantics module
(`src/conventions.rs`) landed June 2026 with the highest-noise call sites
migrated. This is the remaining backlog, ranked. The rule going forward is
in CLAUDE.md rule #1: new visitor code speaks `cst.rs`; a fresh
`kind() == "..."` probe or `child_by_field_name(..).utf8_text(..)` chain
for a shape cst already models is a bug.

## Already migrated (for orientation)

`pair_nodes`/`pair_nodes_in` (separator-agnostic pair walking),
`call_args`, `varname_child`, `canonical_var_name` (braced-spelling
normalization via the grammar's varname child), `canonical_container_name`
(sigil canonicalization), `constructor_invocant`,
`is_conventional_invocant_scalar`, `NodeExt` (text/field_text/span/named),
`typed_node!` wrappers (MethodCall, FunctionCall, AmbiguousFunctionCall),
node utils (`node_to_span`, `extract_call_name`, `fq_tail_span`,
`find_ancestor`). Conventions: `MethodToken`, `InvocantText`,
`InvocantName`, `is_constructor_name`, `is_conventional_invocant_name`,
`is_current_package_token`, `split_qualified` (still in file_analysis —
fine, it's str-level).

## Backlog, ranked

1. **`invocant_type_at_node`'s `$self` short-circuit** checks the literal
   string `"$self"` only — the last invocant-name site not routed through
   `is_conventional_invocant_name`. (`$class`/`$this`/`$proto` invocants
   skip the enclosing-package short-circuit and pay a bag query that may
   miss.) One-line fix; verify with a `$proto->method` test.

2. **Positional-receiver detection, node-side.** `is_shift_call`,
   `is_positional_receiver`, `shift_is_invocant_here` (builder) answer the
   node-level version of `InvocantText::PositionalReceiver`. One cst
   predicate (`is_positional_receiver_node(node, src)`) should own the
   shape; the builder keeps only `shift_is_invocant_here`'s
   context-sensitivity (needs sub-body position).

3. **Three near-duplicate text→class resolvers.**
   `FileAnalysis::invocant_text_to_class`,
   `FileAnalysis::resolve_invocant_class`, and cursor_context's
   `resolve_text_invocant` all dispatch on `InvocantText` and answer
   "class of this invocant text" with slightly different fallbacks
   (package_at vs scope-chain vs analysis-optional). Collapse to one
   FileAnalysis seam; cursor_context composes it (rule #3/#5).

4. **String extraction trio.** `Builder::extract_node_string`,
   `extract_string_content` (quote-flavor-agnostic content child),
   `extract_key_text` re-derive "the string value of this node" with
   overlapping match arms. cst should own one
   `string_value(node, src) -> Option<(String, Span)>` encoding the
   quote-flavor trap (`string_content` child; empty literal has none);
   `arg_info_for`'s inline copy of the same logic folds in.

5. **Remaining pair-walk variants.** `for_each_has_option_pair` and the
   export-pair detectors (`is_export_pair_call`/`detect_export_pair_call`)
   pre-date `pair_nodes` and partially re-derive it. Route through cst.

6. **The long tail: ~400 raw CST pokes in builder visitors.** Strangler
   rule — migrate when touching a visitor. Worst offenders by density:
   `visit_use` / `visit_class_tiny_use` (import-list walking),
   `visit_has_call` (Moo option walking), `visit_assignment` (RHS shape
   dispatch), `visit_dbic_*` (dies with DBIC-as-plugin — don't invest),
   `extract_params` / `extract_signature_params`.

7. **`typed_node!` coverage.** Only three wrappers exist. Add as visitors
   migrate: `SubDecl` (name/body/lexical), `VariableDecl`
   (variable/variables — the paren-list trap), `Assignment` (the
   `child_by_field_name("right")`-returns-paren trap), `AnonHash`,
   `UseStatement` (module field).

## Not cst work (don't confuse)

- DBIC shape knowledge in builder/file_analysis → `prompt-dbic-as-plugin.md`.
- Mojo `"to"` method special-case (builder) → plugin `on_method_call` hook.
- The three lazy tree walkers in file_analysis → die with phase 5
  (eager `Ref.target`), not by migration.
