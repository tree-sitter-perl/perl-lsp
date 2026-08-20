//! Build entry points and the fixed-order phase driver
//! (`build_with_plugins_inner`), plus the walk-time `TypeConstraint`
//! push helpers and the one-shot witness-bag seed.

use super::*;

/// Walk the tree once, indexing the three node kinds the chain-typing
/// reducer cares about. Pure: reads only tree-sitter structural data,
/// no Builder state. Same recursion shape (depth-first via
/// `named_child(i)`) the three former independent walks all used.
pub(super) fn build_chain_typing_index<'a>(tree: &'a Tree) -> ChainTypingIndex<'a> {
    let mut idx = ChainTypingIndex {
        assignment_nodes: Vec::new(),
        return_nodes: std::collections::HashMap::new(),
        invocant_nodes: std::collections::HashMap::new(),
        method_call_args: std::collections::HashMap::new(),
        method_call_nodes: Vec::new(),
        chained_hash_elements: Vec::new(),
    };
    // Explicit stack, like every other tree pass here: a recursive descent
    // costs one frame per CST level, and a stack overflow is a fatal abort
    // no `catch_unwind` can net.
    fn walk<'t>(root: Node<'t>, idx: &mut ChainTypingIndex<'t>) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
        match node.kind() {
            "assignment_expression" => {
                idx.assignment_nodes.push(node);
            }
            "return_expression" => {
                idx.return_nodes
                    .insert((node.start_position(), node.end_position()), node);
            }
            "method_call_expression" => {
                idx.method_call_nodes.push(node);
                if let Some(inv) = node.child_by_field_name("invocant") {
                    idx.invocant_nodes
                        .insert((inv.start_position(), inv.end_position()), inv);
                }
                if let Some(args) = node.child_by_field_name("arguments") {
                    idx.method_call_args
                        .insert((node.start_position(), node.end_position()), args);
                }
            }
            "hash_element_expression" => {
                // Container is the first named child; index only the
                // chained shape where it's a method call — plain
                // `$var->{key}` is handled by the walk.
                if node
                    .named_child(0)
                    .map(|c| c.kind() == "method_call_expression")
                    .unwrap_or(false)
                {
                    idx.chained_hash_elements.push(node);
                }
            }
            _ => {}
        }
        // Reversed: popping from the end yields named child 0 first, so the
        // ordered `Vec` fields keep document order.
        for i in (0..node.named_child_count()).rev() {
            if let Some(c) = node.named_child(i) {
                stack.push(c);
            }
        }
        }
    }
    walk(tree.root_node(), &mut idx);
    idx
}

pub fn build(tree: &Tree, source: &[u8]) -> FileAnalysis {
    build_with_plugins(tree, source, default_plugin_registry())
}

/// Sanity ceiling on CST depth, checked before the walk starts.
///
/// This is no longer what keeps the process alive. The walk, the chain-typing
/// index and the expression-shape typing are all bounded independently of CST
/// depth (`walk.rs`, `build_chain_typing_index`, `MAX_EXPR_TYPE_DEPTH`), so a
/// deep file is analyzed rather than refused. What remains is a bound on the
/// absurd: past this point a file is not source anyone wrote, and analyzing it
/// buys nothing while costing time and heap proportional to its depth.
///
/// The number follows from measurement, not inheritance. On a 2 MiB stack
/// (the rayon worker size), release build, nested `[`:
///
/// | | deepest that yields a real analysis |
/// |---|---|
/// | recursive walk (before) | 1,803 — aborts by 2,503 |
/// | iterative walk (now) | **1,000,003**, and that was the probe's ceiling, not the code's |
///
/// Against real input: the deepest of 138,806 CPAN files is 247 levels, and
/// the deepest generated artifact seen is 5,336. 100,000 sits ~19× above
/// anything observed and 10× below the verified survivable depth — the
/// headroom is on the side where being wrong used to abort the server.
pub(crate) const MAX_CST_DEPTH: usize = 100_000;

/// Maximum node depth of the tree, measured iteratively (`TreeCursor`, no
/// recursion — safe to run on exactly the trees the walk cannot handle).
pub(crate) fn cst_depth(tree: &Tree) -> usize {
    let mut cursor = tree.root_node().walk();
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    loop {
        if cursor.goto_first_child() {
            depth += 1;
            if depth > max_depth {
                max_depth = depth;
            }
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return max_depth;
            }
            depth -= 1;
        }
    }
}

/// Honest degradation for a tree too deep to walk: no analysis, and a
/// first-line diagnostic saying so. Killing the server (the alternative)
/// is never the right trade for one generated/non-Perl file.
fn too_deep_analysis(tree: &Tree, depth: usize) -> FileAnalysis {
    log::warn!(
        "CST depth {} exceeds MAX_CST_DEPTH ({}); skipping analysis of this file \
         (deeply nested generated or non-Perl content)",
        depth,
        MAX_CST_DEPTH
    );
    let mut fa = FileAnalysis::new(crate::model::file_analysis::FileAnalysisParts {
        scopes: vec![Scope {
            id: ScopeId(0),
            parent: None,
            kind: ScopeKind::File,
            span: node_to_span(tree.root_node()),
            package: Some("main".to_string()),
        }],
        plugin: crate::model::file_analysis::PluginFacts {
            diagnostics: vec![PluginDiagnostic {
                message: format!(
                    "analysis skipped: parse tree depth {} exceeds the {} limit \
                     (deeply nested generated or non-Perl content)",
                    depth, MAX_CST_DEPTH
                ),
                span: Span {
                    start: Point { row: 0, column: 0 },
                    end: Point { row: 1, column: 0 },
                },
                severity: "warning".to_string(),
                code: "cst-too-deep".to_string(),
                plugin_id: "core".to_string(),
            }],
            ..Default::default()
        },
        ..Default::default()
    });
    fa.finalize_post_walk();
    fa
}

/// The compiled `@flow` query (`queries/perl/flow.scm`), compiled once.
/// `Query::new` is expensive and `build` runs per file — see `warm_flow_query`
/// for why this is warmed at startup rather than lazily on the first build.
pub(super) fn flow_query() -> Option<&'static tree_sitter::Query> {
    use std::sync::OnceLock;
    static FLOW_SCM: &str = include_str!("../../../queries/perl/flow.scm");
    static FLOW_QUERY: OnceLock<Option<tree_sitter::Query>> = OnceLock::new();
    FLOW_QUERY
        .get_or_init(|| {
            let lang: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
            tree_sitter::Query::new(&lang, FLOW_SCM).ok()
        })
        .as_ref()
}

/// Force the flow query to compile now, off the parallel per-file path.
pub(crate) fn warm_flow_query() {
    let _ = flow_query();
}

/// Build with a caller-provided plugin registry. Tests use this to swap in
/// deterministic plugin sets; the global default is otherwise shared.
pub fn build_with_plugins(
    tree: &Tree,
    source: &[u8],
    plugins: Arc<PluginRegistry>,
) -> FileAnalysis {
    build_with_plugins_inner(tree, source, plugins, false)
}

/// Test-only entry: build the file, then re-run the worklist fold
/// driver (`fold_to_fixed_point`) one extra time before finalizing.
///
/// The fold is fully idempotent: the resulting
/// `FileAnalysis` is byte-identical to a plain `build_with_plugins(...)`
/// call — same `type_provenance`, same `sub_return_type_at_arity`
/// answers, same witness counts. The two re-emittable passes inside
/// `resolve_return_types` (arity-return emission, call-binding
/// propagator) clear their prior outputs before re-emitting, so each
/// fact lands in the bag exactly once regardless of iteration count.
/// The `post_walk_fold_is_observably_idempotent` invariant test
/// asserts the answer-level guarantee directly.
#[cfg(test)]
pub(crate) fn build_with_plugins_extra_re_fold(
    tree: &Tree,
    source: &[u8],
    plugins: Arc<PluginRegistry>,
) -> FileAnalysis {
    build_with_plugins_inner(tree, source, plugins, true)
}

pub(super) fn build_with_plugins_inner(
    tree: &Tree,
    source: &[u8],
    plugins: Arc<PluginRegistry>,
    extra_re_fold: bool,
) -> FileAnalysis {
    let fa = build_once(tree, source, plugins.clone(), extra_re_fold);
    // `PERL_LSP_WALK_EQUIV=1 cargo test` re-builds every file the suite
    // touches with the recursive descent and asserts the two agree. Running
    // it over the whole suite — not a fixture list — is the point: it is the
    // only corpus that covers every visitor arm.
    #[cfg(test)]
    if super::walk::equivalence_check_enabled()
        && super::walk::recursive_walk_forced().is_none()
        && cst_depth(tree) <= super::walk::MAX_COMPARABLE_DEPTH
    {
        super::walk::assert_walks_agree(&fa, || {
            super::walk::with_walk_mode(true, || {
                build_once(tree, source, plugins.clone(), extra_re_fold)
            })
        });
    }
    // `PERL_LSP_PD_EQUIV=1 cargo test` additionally rebuilds with pattern
    // dispatch's per-spec traversals and asserts the whole FileAnalysis
    // matches — the byte-identical-dispatch-output bar, over every file the
    // suite touches. The per-round collection check inside
    // `collect_walk_matches` covers the corpus; this covers the model.
    #[cfg(test)]
    if super::pattern_dispatch::collection_equiv_enabled()
        && super::pattern_dispatch::combine_forced().is_none()
    {
        super::walk::assert_analyses_agree("pattern-combine", &fa, || {
            super::pattern_dispatch::with_combine(false, || {
                build_once(tree, source, plugins.clone(), extra_re_fold)
            })
        });
    }
    fa
}

fn build_once(
    tree: &Tree,
    source: &[u8],
    plugins: Arc<PluginRegistry>,
    extra_re_fold: bool,
) -> FileAnalysis {
    // Cheap structural bound, measured iteratively so it is safe on exactly
    // the trees it screens. See `MAX_CST_DEPTH` for why the number is what
    // it is and what it does — and no longer does.
    let depth = cst_depth(tree);
    if depth > MAX_CST_DEPTH {
        return too_deep_analysis(tree, depth);
    }
    let topic_dsls: Vec<plugin::TopicRouteDsl> =
        plugins.all().filter_map(|pl| pl.topic_route_dsl()).collect();
    let mut b = Builder {
        source,
        scopes: Vec::new(),
        symbols: Vec::new(),
        refs: Vec::new(),
        deferred_var_types: Vec::new(),
        deferred_named_sub_param_types: Vec::new(),
        fold_ranges: Vec::new(),
        imports: Vec::new(),
        return_infos: Vec::new(),
        pending_array_pushes: Vec::new(),
        last_expr_span: std::collections::HashMap::new(),
        slot_write_rhs_span: std::collections::HashMap::new(),
        call_bindings: Vec::new(),
        method_call_bindings: Vec::new(),
        pod_texts: Vec::new(),
        package_parents: std::collections::HashMap::new(),
        package_uses: std::collections::HashMap::new(),
        use_dedup: std::collections::HashSet::new(),
        dispatch_dedup: std::collections::HashSet::new(),
        sub_return_delegations: std::collections::HashMap::new(),
        framework_modes: std::collections::HashMap::new(),
        framework_imports: std::collections::HashSet::new(),
        constant_strings: std::collections::HashMap::new(),
        constant_string_source: std::collections::HashMap::new(),
        declared_constants: std::collections::HashMap::new(),
        export_member_sites: Vec::new(),
        export: Vec::new(),
        export_ok: Vec::new(),
        export_tags: std::collections::HashMap::new(),
        reexport_modules: Vec::new(),
        lib_roots: Vec::new(),
        plugin_namespaces: Vec::new(),
        type_provenance: std::collections::HashMap::new(),
        bag: crate::model::witnesses::WitnessBag::new(),
        unresolved_expr_nodes: Vec::new(),
        package_framework: std::collections::HashMap::new(),
        non_oo_packages: std::collections::HashSet::new(),
        scope_stack: Vec::new(),
        // Perl's implicit top-level package. Without this seed,
        // top-level scripts (`Mojolicious::Lite` apps, one-off
        // `.pl` files) have `current_package = None` until they
        // hit an explicit `package` statement — which means
        // `package_uses` never records the file's `use` lines and
        // `Trigger::UsesModule` plugin triggers don't fire. Same
        // as Perl's own runtime: every script starts in `main`.
        current_package: Some("main".to_string()),
        next_scope_id: 0,
        next_symbol_id: 0,
        package_ranges: Vec::new(),
        open_statement_package: None,
        plugins,
        dispatch_manifest: std::collections::HashMap::new(),
        load_manifest: std::collections::HashMap::new(),
        type_constraint_names: std::collections::HashSet::new(),
        app_surface_consumers: Vec::new(),
        param_type_manifest: std::collections::HashMap::new(),
        param_type_wildcards: Vec::new(),
        plugin_loads: Vec::new(),
        loader_config_params: Vec::new(),
        flow_edges: Vec::new(),
        any_requires_action_attr: false,
        provisional_dispatches: Vec::new(),
        gated_emissions: Vec::new(),
        gated_param_types: Vec::new(),
        method_call_invocant: std::collections::HashMap::new(),
        attr_projections: Vec::new(),
        escape_recorded: std::collections::HashSet::new(),
        role_requires: std::collections::HashMap::new(),
        contract_symbols: std::collections::HashSet::new(),
        dynamic_parent_packages: std::collections::HashSet::new(),
        dynamic_dispatch_sites: 0,
        role_maker_modules: std::collections::HashSet::new(),
        framework_mode_modules: std::collections::HashMap::new(),
        role_packages: std::collections::HashSet::new(),
        dbic_source_name: None,
        topic_group_spans: Vec::new(),
        plugin_diagnostics: Vec::new(),
        topic_dsls,
        reassigned_scalars: std::collections::HashSet::new(),
        key_writes: Vec::new(),
        method_call_arity: std::collections::HashMap::new(),
        parametric_emitted_refs: std::collections::HashSet::new(),
        method_call_ref_dedup: std::collections::HashSet::new(),
        route_branded_refs: std::collections::HashSet::new(),
        defined_narrowings: Vec::new(),
        pending_narrowings: Vec::new(),
        guard_sites: Vec::new(),
        arrow_deref_sites: Vec::new(),
        anon_sub_symbol_by_span: std::collections::HashMap::new(),
        modifier_invocant_pos: None,
        expr_type_depth: 0,
        walk_stack: Vec::new(),
        recursive_walk: super::walk::recursive_walk_forced()
            .unwrap_or_else(super::walk::recursive_walk_requested),
    };
    b.dispatch_manifest = b
        .plugins
        .dispatch_verbs()
        .map(|d| (d.verb.clone(), d.clone()))
        .collect();
    b.load_manifest = b
        .plugins
        .load_verbs()
        .map(|d| (d.verb.clone(), d.clone()))
        .collect();
    b.type_constraint_names = b
        .plugins
        .type_constraint_names()
        .map(|s| s.to_string())
        .collect();
    b.app_surface_consumers = b
        .plugins
        .app_surface_consumers()
        .map(|s| s.to_string())
        .collect();
    b.role_maker_modules
        .extend(b.plugins.role_makers().map(|s| s.to_string()));
    b.framework_mode_modules = b
        .plugins
        .framework_mode_makers()
        .filter_map(|m| match FrameworkMode::from_flavor(&m.flavor) {
            Some(mode) => Some((m.module.clone(), (mode, m.imports.clone()))),
            None => {
                log::error!(
                    "framework_mode_makers: unknown flavor `{}` for module `{}` — entry dropped",
                    m.flavor,
                    m.module
                );
                None
            }
        })
        .collect();
    for pt in b.plugins.param_types() {
        match &pt.method {
            Some(name) => {
                b.param_type_manifest
                    .entry(name.clone())
                    .or_default()
                    .push(pt.clone());
            }
            None => b.param_type_wildcards.push(pt.clone()),
        }
    }
    b.any_requires_action_attr = b
        .param_type_manifest
        .values()
        .flatten()
        .any(|r| r.requires_action_attr)
        || b.param_type_wildcards
            .iter()
            .any(|r| r.requires_action_attr);

    // Create file-level scope and walk
    let file_scope = b.push_scope(ScopeKind::File, node_to_span(tree.root_node()), None);
    bphase!("walk", b.drive_walk(tree.root_node()));
    // Still inside the file scope: synthesize Sub symbols for AutoLoader /
    // SelfLoader packages whose real definitions live in the `data_section`
    // after `__END__` (or `__DATA__`). Runs here so `package_uses` /
    // `package_parents` (the AutoLoader-backed gate) are fully populated and
    // the synthesized symbols attach to the file scope, like every other
    // top-level sub.
    b.synthesize_autoloader_data_subs(tree);

    // Query-declared plugin capture (SPIKE): dispatch plugin patterns
    // against the finished tree. Post-walk so package ranges, uses,
    // and constant folds are complete; still inside the file scope
    // (emissions need an open scope stack) and BEFORE the VarType /
    // named-sub flushes below so pattern emissions ride the same
    // machinery as walk-interleaved hook emissions.
    bphase!("pattern_dispatch", b.dispatch_pattern_plugins(tree.root_node()));

    b.pop_scope();
    let _ = file_scope;

    // Flush plugin-emitted VarType constraints now that every scope
    // has been pushed. Each uses scope_at on the declared anchor point
    // so a `$app->helper(... sub { my ($c) = @_; ... })` emission
    // lands inside the callback body rather than the outer file scope.
    let deferred = std::mem::take(&mut b.deferred_var_types);
    for d in deferred {
        let scope = b
            .scopes
            .iter()
            .rev()
            .find(|s| crate::model::file_analysis::contains_point(&s.span, d.at.start))
            .map(|s| s.id)
            .unwrap_or(ScopeId(0));
        b.push_plugin_type_constraint(
            TypeConstraint {
                variable: d.variable,
                scope,
                constraint_span: d.at,
                inferred_type: d.inferred_type,
            },
            d.plugin_id,
        );
    }

    // Named-sub param typing (`->helper(_ => \&sub)`): same flush window —
    // every sub scope + its params now exist, including forward-declared
    // ones.
    b.flush_deferred_named_sub_param_types();

    // Post-pass 1: resolve variable refs -> Symbol bindings
    bphase!("resolve_variable_refs", b.resolve_variable_refs());

    // Value-flow capture: run the declarative `@flow` query (the assignment
    // SHAPES) and mint FlowEdges with the builder's own scope. Provenance-only
    // for now (no lowering) — the shapes' types still come from the walk; this
    // proves the query path before it subsumes the manual minting.
    bphase!("flow_query", b.mint_flow_edges_via_query(tree));

    // Narrowing cutoffs: now that the FlowEdges exist, truncate each recognized
    // narrowed region at the first edge that rebinds its subject (the
    // edge-driven replacement for the `cst::rebinds_scalar` walk) and emit.
    bphase!("narrowing_cutoffs", b.apply_narrowing_cutoffs());

    // Export-list member refs: a `@EXPORT` / `@EXPORT_OK` / `%EXPORT_TAGS`
    // member naming a local sub gets a FunctionCall ref back to it. Runs
    // post-walk because subs are usually declared after the export list.
    b.emit_export_member_refs();

    // Pin forward-reference calls (call above its `sub`) to the local def's
    // package — order-independent, so goto-def/references/rename match them
    // like backward calls (the walk-time pin only saw subs declared earlier).
    b.pin_unresolved_call_packages();

    // Post-pass 2: resolve hash key owners from type constraints
    b.resolve_hash_key_owners();

    // Compute per-package framework facts BEFORE return-type fold so
    // the bag-aware reducer has the right context. Mirrors the data
    // the framework-accessor synthesis already consumed during the walk.
    b.package_framework = b
        .framework_modes
        .iter()
        .map(|(pkg, mode)| {
            let ff = match mode {
                FrameworkMode::Moo => crate::model::witnesses::FrameworkFact::Moo,
                FrameworkMode::Moose => crate::model::witnesses::FrameworkFact::Moose,
                FrameworkMode::MojoBase => crate::model::witnesses::FrameworkFact::MojoBase,
            };
            (pkg.clone(), ff)
        })
        .collect();

    // Plugin `overrides()` manifests run first. They pin return
    // types inference can't reach (`Mojolicious::Routes::Route::_route`
    // returning $self via an array-slice idiom). Provenance is
    // recorded in `type_provenance` (PluginOverride) so
    // `--dump-package` can answer "why does this return X?".
    bphase!("apply_type_overrides", b.apply_type_overrides());

    // Post-walk bag-population pass: ref-derived facts that don't
    // need walk-time visibility — `HashRefAccess` observations from
    // `$v->{k}` refs and invocant-mutation facts from hash-key
    // writes. Variable witnesses for TCs and walk-time idiom witnesses
    // (branch arms, arity gating) are already in `b.bag` — pushed
    // live during the walk.
    bphase!("populate_witness_bag", b.populate_witness_bag());

    // Forward-reference resolution: walk-time `expr_payload` arms for
    // `function_call_expression` / `bareword` / `scoped_identifier` did
    // a `self.symbols.iter().find` against a partial symbol table.
    // Forward-defined callees (Perl's `sub a { b() } sub b {…}` pattern,
    // canonically Carp's `longmess` → `longmess_heavy`) silently
    // produced no witness. The walk queued each missing lookup; resolve
    // them now against the final symbol table and push the
    // `Expr(span) → Edge(Symbol(sid))` witness the walk would have.
    bphase!("resolve_fwd_expr_witnesses", b.resolve_forward_expr_witnesses());

    // Worklist driver: one fixed-point loop over chain typing +
    // reducer dispatch (rather than a manually-ordered
    // `fold → chain → fold → chain` sequence). Each iteration runs
    // `ChainPassMode::PreFold` (assignment + return-arm refresh)
    // followed by `resolve_return_types`; the loop exits when the
    // snapshot of Sub/Method return types and bag length stops
    // moving. Invocant-class refresh runs once after the lattice
    // settles.
    //
    // The two re-emittable passes inside `resolve_return_types`
    // (arity-return witnesses, call-binding propagator) became
    // clear-and-emit in this same commit, so the bag stays canonical
    // regardless of how many iterations the loop runs — each fact
    // lands exactly once at the end. Chain typing's TC-existence
    // check keeps it idempotent on the same assignment span.
    //
    // For shallow files (no through-chain dependencies on inferred
    // sub return types) the loop terminates in two iterations: one
    // to derive the initial fold answer, one to confirm stability.
    // Deeper chains take more iterations; `MAX_FOLD_ITERATIONS`
    // (debug-only) catches dependency-tracking bugs that would
    // otherwise spin forever.
    let chain_idx = bphase!("build_chain_typing_index", build_chain_typing_index(tree));
    bphase!("fold_to_fixed_point", b.fold_to_fixed_point(&chain_idx));
    // PostFold filled `invocant_class` on MethodCall refs after the
    // worklist exited; re-emit method-call return edges so
    // Expression(refidx) chases resolve through to
    // PackageSymbol{package, method} for any invocant freshly known.
    // Then push array contributions: spans queryable through the
    // freshly-published edges.
    bphase!("emit_mc_return_edges", b.emit_method_call_return_edges());
    bphase!("emit_array_push_witns", b.emit_array_push_witnesses());
    // Record each method-call invocant's resolved type at its span so
    // the tree-free query entry (`FileAnalysis::expr_type_at_span`) can
    // answer "what is this expression?" without a CST. Runs after array
    // pushes so `$arr[N]` invocants project against the settled
    // `Variable{@arr}` Sequence. The build-time symbolic executor
    // (`invocant_type_at_node`) is the single structure-discovery site;
    // this pass records its answer.
    bphase!("emit_invocant_expr_witns", b.emit_invocant_expr_witnesses(&chain_idx));

    // Fold-phase pattern dispatch: patterns declared `phase: "fold"`
    // run HERE — after PostFold, so their projections read settled
    // chain typing (route brands, resolved invocants). Matches
    // dispatch in document order with the topic-route base replayed
    // from the walk's recorded group spans; `SetRouteBase` emissions
    // update the replay base instead of the (stale) walk stack.
    bphase!("pattern_dispatch_fold", b.dispatch_pattern_plugins_fold(tree.root_node()));

    // Test-only: re-run the worklist fold one more time to pin
    // idempotency. Production callers always pass `false`; only
    // `build_with_plugins_extra_re_fold` flips this on. Re-running
    // `fold_to_fixed_point` against a settled state should land in
    // 1 iteration (loop sees `prev == cur` immediately) and produce
    // a byte-identical FileAnalysis — including witness counts,
    // unlike the pre-Phase-6 pipeline.
    if extra_re_fold {
        b.fold_to_fixed_point(&chain_idx);
    }

    // Post-pass: emit `HashKeyAccess` refs for even-position stringy
    // args on every resolved `MethodCall` ref (`MooApp->new(name => 'alice')`,
    // helper-emitted controllers, etc.). Runs after `fold_to_fixed_point`
    // so `invocant_class` is canonical against the bag — was a walk-time
    // emission gated on the partially-resolved walk-time class, now it's
    // a single post-walk pass that joins refs to args via the chain
    // typing index.
    bphase!("emit_mc_arg_keys", b.emit_method_call_arg_keys(&chain_idx));

    // Post-pass: chained hashref-key accesses (`$obj->get_config->{host}`).
    // Runs post-fold so the method's return type is canonical — the
    // owner class is the chain receiver's type, unknowable until then.
    bphase!("emit_chained_hk_refs", b.emit_chained_hash_key_refs(&chain_idx));

    // Post-pass: upgrade Variable-owned hash-key derefs whose variable's
    // type settled to a class DURING the fold (`my $row = $rs->find(1);
    // $row->{name}` — the RowOf projection lands mid-fold, after
    // resolve_hash_key_owners ran). A Class owner routes the key to the
    // class's defs (DBIC columns, Moo slots); variables without a class
    // type keep their lexical grouping.
    bphase!("upgrade_var_hk_owners", b.upgrade_variable_hash_key_owners());

    // Post-pass 5: fill in tail POD docs for subs that didn't get preceding doc
    bphase!("resolve_tail_pod_docs", b.resolve_tail_pod_docs());

    let mut fa = FileAnalysis::new(crate::model::file_analysis::FileAnalysisParts {
        scopes: b.scopes,
        symbols: b.symbols,
        refs: b.refs,
        fold_ranges: b.fold_ranges,
        imports: b.imports,
        call_bindings: b.call_bindings,
        packages: crate::model::file_analysis::PackageFacts::fold(
            b.package_parents,
            b.package_uses,
            b.package_framework,
            b.role_requires,
            b.role_packages,
            b.dynamic_parent_packages,
        ),
        method_call_bindings: b.method_call_bindings,
        framework_imports: b.framework_imports,
        export: b.export,
        export_ok: b.export_ok,
        export_tags: b.export_tags,
        reexport_modules: b.reexport_modules,
        lib_roots: b.lib_roots,
        plugin: crate::model::file_analysis::PluginFacts {
            namespaces: b.plugin_namespaces,
            loads: b.plugin_loads,
            diagnostics: b.plugin_diagnostics,
            gated_emissions: b.gated_emissions,
            app_surface_consumers: b.app_surface_consumers,
        },
        // The pack lane is empty for Perl: no macros, no include graph,
        // no template params, no `std::move`.
        pack: crate::model::file_analysis::PackFacts::default(),
        type_provenance: b.type_provenance,
        package_ranges: b.package_ranges,
        witnesses: b.bag,
        provisional_dispatches: b.provisional_dispatches,
        guard_sites: b.guard_sites,
        arrow_deref_sites: b.arrow_deref_sites,
        attr_projections: b.attr_projections,
        gated_param_types: b.gated_param_types,
        reassigned_scalars: b.reassigned_scalars,
        key_writes: b.key_writes,
        contract_symbols: b.contract_symbols,
        dynamic_dispatch_sites: b.dynamic_dispatch_sites,
        dbic_source_name: b.dbic_source_name,
        column_keyed_verbs: b.plugins.column_keyed_verbs().map(|s| s.to_string()).collect(),
        loader_config_params: b.loader_config_params,
        flow_edges: b.flow_edges,
    });
    // Finalize: the MCB→bag bridge (`emit_method_call_binding_edges`)
    // publishes `Variable → Edge(PackageSymbol{...})` for every recorded
    // `$var = $invocant->method()` binding — the registry chases the
    // return lazily, cross-file once a query holds the index. Enrichment
    // re-runs the same bridge without a tree. Then owner fixup, target
    // stamping, and the base-count seals.
    bphase!("finalize_post_walk", fa.finalize_post_walk());

    fa
}

impl<'a> Builder<'a> {
    /// Push a `TypeConstraint` shape into the witness bag — Variable
    /// `InferredType` + class-assertion / FirstParam observation when
    /// the type is a class identity. Walk-time and worklist callers go
    /// through here so `bag_query_variable` sees seeded types
    /// immediately. Mirrors `FileAnalysis::push_type_constraint`'s
    /// shape (the FA helper handles enrichment-time pushes after the
    /// builder's bag has been moved into the FA).
    pub(crate) fn push_type_constraint(&mut self, tc: TypeConstraint) {
        self.push_type_constraint_from(tc, crate::model::witnesses::WitnessSource::Builder("type_constraint".into()));
    }

    /// `push_type_constraint` with a plugin source so the witness carries
    /// `Plugin` priority. A plugin that knows a variable's type
    /// (`->helper(_ => sub/\&sub)` → `$c` is a controller) must dominate
    /// builder heuristics for that variable — the `my $c = shift` idiom
    /// otherwise types `$c` as the enclosing class. `FrameworkAwareTypeFold`
    /// prefers the higher-priority class assertion (source-priority axis,
    /// CLAUDE.md "Source priority breaks ties").
    pub(crate) fn push_plugin_type_constraint(&mut self, tc: TypeConstraint, plugin_id: String) {
        self.push_type_constraint_from(tc, crate::model::witnesses::WitnessSource::Plugin(plugin_id));
    }

    pub(super) fn push_type_constraint_from(
        &mut self,
        tc: TypeConstraint,
        source: crate::model::witnesses::WitnessSource,
    ) {
        use crate::model::witnesses::{
            TypeObservation, Witness, WitnessAttachment, WitnessPayload,
        };
        let TypeConstraint { variable, scope, constraint_span: span, inferred_type: ty } = tc;
        self.bag.push(Witness {
            attachment: WitnessAttachment::Variable { name: variable.clone(), scope },
            source: source.clone(),
            payload: WitnessPayload::InferredType(ty.clone()),
            span: Span { start: span.start, end: span.start },
        });
        match ty {
            InferredType::ClassName(n) => {
                self.bag.push(Witness {
                    attachment: WitnessAttachment::Variable { name: variable, scope },
                    source,
                    payload: WitnessPayload::Observation(TypeObservation::ClassAssertion(n)),
                    span,
                });
            }
            InferredType::FirstParam { package } => {
                self.bag.push(Witness {
                    attachment: WitnessAttachment::Variable { name: variable, scope },
                    source,
                    payload: WitnessPayload::Observation(TypeObservation::FirstParamInMethod {
                        package,
                    }),
                    span,
                });
            }
            _ => {}
        }
    }


    /// Post-walk pass: ref-derived facts that don't need walk-time
    /// visibility — `HashRefAccess` observations from `$v->{k}` refs
    /// and invocant-mutation facts on hash-key writes. Variable
    /// witnesses for TCs and walk-time idiom witnesses (branch arms,
    /// arity gating) are already in the bag — pushed live during the
    /// walk via `push_type_constraint` and `bag.push` from the emit
    /// sites.
    ///
    /// Method-call return edges (`Expression(refidx) → Edge(PackageSymbol{package, method})`)
    /// are emitted later — by `emit_method_call_return_edges` from
    /// inside the worklist, once `invocant_class` is filled.
    pub(super) fn populate_witness_bag(&mut self) {
        use crate::model::witnesses::{
            TypeObservation, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };

        // Rep observations from `$v->{k}` access. Method-call return
        // edges on `Expression(refidx)` are emitted later — by the
        // chain-typing PostFold pass once `invocant_class` is filled —
        // as `Edge(PackageSymbol{package, method})`. Without a known
        // class there's no class-keyed answer to chase to, so the
        // emission is gated by chain-typing's own progress.
        let mut hash_obs: Vec<(String, ScopeId, Span)> = Vec::new();
        for r in self.refs.iter() {
            if let RefKind::HashKeyAccess { var_text, .. } = &r.kind {
                if var_text.starts_with('$') {
                    hash_obs.push((var_text.clone(), r.scope, r.span));
                }
            }
        }
        for (var, scope, span) in hash_obs {
            self.bag.push(Witness {
                attachment: WitnessAttachment::Variable { name: var, scope },
                source: WitnessSource::Builder("hash_ref_access".into()),
                payload: WitnessPayload::Observation(TypeObservation::HashRefAccess),
                span,
            });
        }

        // Invocant mutations on hash keys.
        //
        // Two seeds per typed-owner write: the untyped `mutation` Fact
        // (key-name completion via `mutated_keys_on_class`) and — when
        // the owner resolves to a CLASS and the RHS has a recorded span
        // and a bag-resolved type — a typed `SlotType{class, key} →
        // Edge(Expr(rhs_span))`. The edge routes through the same
        // canonical chase as implicit-return chains; `SlotTypeFold`
        // agrees the per-write arms. Honest-skip if the owner is a
        // `Sub` (not a class), or the RHS is unknown — never a bare
        // SlotType seed.
        let mut mutations: Vec<(HashKeyOwner, String, Span)> = Vec::new();
        let mut slot_writes: Vec<(String, String, Span, Span)> = Vec::new();
        for r in &self.refs {
            if let (RefKind::HashKeyAccess { var_text }, AccessKind::Write) =
                (&r.kind, r.access)
            {
                let resolved_owner = match r.hash_key_owner() {
                    Some(o @ (HashKeyOwner::Class(_) | HashKeyOwner::Sub { .. })) => Some(o.clone()),
                    _ => {
                        if var_text == "$self" {
                            let scope = &self.scopes[r.scope.0 as usize];
                            scope.package.clone().map(HashKeyOwner::Class)
                        } else {
                            None
                        }
                    }
                };
                if let Some(o) = resolved_owner {
                    if let HashKeyOwner::Class(class) = &o {
                        if let Some(rhs_span) = self.slot_write_rhs_span.get(&r.span) {
                            slot_writes.push((
                                class.clone(),
                                r.target_name.clone(),
                                r.span,
                                *rhs_span,
                            ));
                        }
                    }
                    mutations.push((o, r.target_name.clone(), r.span));
                }
            }
        }
        for (owner, key, span) in mutations {
            self.bag.push(Witness {
                attachment: WitnessAttachment::HashKey { owner, name: key.clone() },
                source: WitnessSource::Builder("invocant_mutation".into()),
                payload: WitnessPayload::Fact {
                    family: "mutation".into(),
                    key: "written_at".into(),
                    value: crate::model::witnesses::FactValue::Str(key),
                },
                span,
            });
        }
        for (class, key, span, rhs_span) in slot_writes {
            // Only seed when the RHS actually resolves to a type — a bare
            // `Edge(Expr(rhs_span))` to an unresolved span folds to None,
            // which is honest, but emitting nothing for `= shift` / `= $param`
            // keeps the attachment absent entirely (no guess).
            if self.bag_query_expr_span(rhs_span).is_none() {
                continue;
            }
            self.bag.push(Witness {
                attachment: WitnessAttachment::SlotType { class, key },
                source: WitnessSource::Builder("slot_type".into()),
                payload: WitnessPayload::Edge(WitnessAttachment::Expr(rhs_span)),
                span,
            });
        }

        // Implicit-last-statement return edges. For each user-defined
        // sub/method scope with NO explicit `return` statements, push
        // `Symbol(sid) → Edge(Expr(last_expr_span))` so registry
        // queries on `Symbol(sid)` materialize the implicit return
        // through the canonical edge-chase path. Subs with explicit
        // returns route via the `Edge(SymbolReturnArm(sid))` chain
        // `publish_return_arm_witnesses` pushes — those claim the
        // same attachment shape first via `SymbolReturnArmFold`.
        // Framework / plugin-synthesized syms have no Scope and thus
        // no entry in `last_expr_span`; they're invisible to this
        // loop, which is the right behavior (their answer comes from
        // the synth-pushed Symbol witness directly).
        //
        // Invariant: `return_infos` is walk-final by the time
        // `populate_witness_bag` runs — it's populated only by
        // `visit_node`'s `return_expression` arm during the live walk
        // and never mutated after. No clear-and-emit tag on the implicit-return
        // edge is therefore needed; the gate `return_infos.is_empty()
        // for this scope` is a one-shot decision.
        let mut implicit_edges: Vec<(SymbolId, Span, Span)> = Vec::new();
        for scope in &self.scopes {
            if !matches!(scope.kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. }) {
                continue;
            }
            if self.return_infos.iter().any(|ri| ri.scope == scope.id) {
                continue;
            }
            let Some(span) = self.last_expr_span.get(&scope.id).copied() else { continue };
            let Some(sym_id) = self.find_sub_symbol_for_scope(scope.id) else { continue };
            implicit_edges.push((sym_id, span, scope.span));
        }
        for (sym_id, expr_span, sym_span) in implicit_edges {
            self.bag.push(Witness {
                attachment: WitnessAttachment::Symbol(sym_id),
                source: WitnessSource::Builder("implicit_return".into()),
                payload: WitnessPayload::Edge(WitnessAttachment::Expr(expr_span)),
                span: sym_span,
            });
        }
    }


    /// Synthesize `Sub` symbols for AutoLoader / SelfLoader packages whose
    /// real sub definitions live in the file's `data_section` (the text after
    /// `__END__` / `__DATA__`). Those subs are loaded on demand at runtime by
    /// `AUTOLOAD`, so they are live code — but tree-sitter parks the whole
    /// region in one opaque `data_section` node, leaving them un-navigable.
    ///
    /// Gate: only packages that actually use AutoLoader/SelfLoader (via `use`
    /// or `@ISA`/`use base`/`use parent`) get this treatment, so genuine
    /// `__DATA__` payloads and trailing POD on ordinary modules synthesize
    /// nothing. This is a framework semantic (like Moo/Mojo detection), not a
    /// shape-branch: the property "package is autoload-backed" rides on the
    /// package's recorded uses/parents, and the data section is only mined when
    /// the owning package answers yes.
    ///
    /// Re-parsing the data-section text as Perl is the single-build-entry way
    /// to recover the sub shapes (rule #1: all CST traversal stays in build()).
    /// Spans are offset back to real file coordinates so goto-def lands.
    pub(super) fn synthesize_autoloader_data_subs(&mut self, tree: &Tree) {
        let Some(data) = find_data_section(tree.root_node()) else { return };

        // Which package is in effect at the data section? `__END__`/`__DATA__`
        // terminates compilation, so the owning package is whichever range
        // brackets the marker — for the typical single-package AutoLoader
        // module that's the only package in the file.
        let data_start = data.start_position();
        let owner_pkg = self
            .package_ranges
            .iter()
            .rev()
            .find(|r| crate::model::file_analysis::contains_point(&r.span, data_start))
            .map(|r| r.package.clone())
            .or_else(|| self.current_package.clone());

        let Some(pkg) = owner_pkg else { return };
        if !self.is_autoload_backed(&pkg) {
            return;
        }

        let section_text = match data.utf8_text(self.source) {
            Ok(s) => s.to_string(),
            Err(_) => return,
        };

        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&ts_parser_perl::LANGUAGE.into()).is_err() {
            return;
        }
        let Some(sub_tree) = parser.parse(&section_text, None) else { return };
        let sub_src = section_text.as_bytes();

        // tree-sitter reports re-parse positions relative to the section text;
        // map them back to file coordinates. Row N of the section is file row
        // (data_start.row + N); the section's first row continues the
        // `__END__` line, so its column carries the marker offset.
        let offset = |p: Point| -> Point {
            if p.row == 0 {
                Point { row: data_start.row, column: data_start.column + p.column }
            } else {
                Point { row: data_start.row + p.row, column: p.column }
            }
        };
        let offset_span = |s: Span| -> Span {
            Span { start: offset(s.start), end: offset(s.end) }
        };

        let prev_pkg = self.current_package.take();
        self.current_package = Some(pkg.clone());

        let mut found: Vec<Node> = Vec::new();
        collect_data_section_subs(sub_tree.root_node(), &mut found);
        let mut synth_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for sub_node in found {
            let Some(name_node) = sub_node.child_by_field_name("name") else { continue };
            let Ok(name) = name_node.utf8_text(sub_src) else { continue };
            let params = extract_data_section_params(sub_node, sub_src);
            self.add_symbol(
                name.to_string(),
                SymKind::Sub,
                offset_span(node_to_span(sub_node)),
                offset_span(node_to_span(name_node)),
                SymbolDetail::Sub {
                    params,
                    is_method: false,
                    doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                },
            );
            synth_names.insert(name.to_string());
        }

        self.current_package = prev_pkg;

        // Bareword calls to these subs were visited during the walk before the
        // synthesized symbols existed, so `resolve_call_package` left their
        // `FunctionCall` ref unpinned (no `Function` binding). Pin any such
        // ref — within the owning package's source region and naming a sub we
        // just synthesized — to `pkg`, so goto-def lands on the data-section
        // definition. Scoped to autoload-backed packages: we only touch refs
        // whose target is one of the freshly minted names.
        for r in self.refs.iter_mut() {
            if matches!(r.kind, RefKind::FunctionCall) {
                if r.binding.is_none() && synth_names.contains(&r.target_name) {
                    let in_pkg = self
                        .package_ranges
                        .iter()
                        .rev()
                        .find(|pr| crate::model::file_analysis::contains_point(&pr.span, r.span.start))
                        .is_some_and(|pr| pr.package == pkg);
                    if in_pkg {
                        r.bind_function_package(pkg.clone());
                    }
                }
            }
        }
    }

    /// True when `pkg` pulls in AutoLoader or SelfLoader — either via a `use`
    /// line or by inheriting from it (`@ISA`/`use base`/`use parent`). Both
    /// reach `package_parents`; `use` lines reach `package_uses`.
    pub(super) fn is_autoload_backed(&self, pkg: &str) -> bool {
        let is_loader = |m: &str| m == "AutoLoader" || m == "SelfLoader";
        self.package_uses
            .get(pkg)
            .map_or(false, |us| us.iter().any(|m| is_loader(m)))
            || self
                .package_parents
                .get(pkg)
                .map_or(false, |ps| ps.iter().any(|p| is_loader(p)))
    }
}
