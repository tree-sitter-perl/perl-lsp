//! Post-passes and the worklist fold: `fold_to_fixed_point`, chain-typing
//! reducer passes, return-type seeding/writeback, bag query wrappers.

use super::*;

impl<'a> Builder<'a> {
    // ---- Post-passes ----

    pub(super) fn resolve_variable_refs(&mut self) {
        // Build a temporary scope-to-symbols map for efficient lookup.
        //
        // `gating_package` is `Some(pkg)` for `our` decls — they're
        // package-globals with a lexical alias, so a use site only
        // resolves to them when the use's enclosing package matches
        // (`$Calculator::version` is reachable as bare `$version`
        // only inside `package Calculator;`). It's `None` for `my` /
        // `state` / `field`, which are pure-lexical: they resolve
        // wherever the lexical scope chain reaches them, regardless
        // of which `package X;` section the use sits under.
        let mut scope_symbols: std::collections::HashMap<
            ScopeId,
            Vec<(String, SymbolId, Point, Option<String>)>,
        > = std::collections::HashMap::new();
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Variable | SymKind::Field) {
                let gating_package = match &sym.detail {
                    SymbolDetail::Variable { decl_kind: DeclKind::Our, .. } => sym.package.clone(),
                    _ => None,
                };
                scope_symbols
                    .entry(sym.scope)
                    .or_default()
                    .push((sym.name.clone(), sym.id, sym.span.start, gating_package));
            }
        }

        for idx in 0..self.refs.len() {
            if !matches!(self.refs[idx].kind, RefKind::Variable | RefKind::ContainerAccess) {
                continue;
            }
            let ref_span_start = self.refs[idx].span.start;
            let ref_target = self.refs[idx].target_name.clone();
            let ref_scope = self.refs[idx].scope;

            // Fully-qualified read (`$Foo::Bar::x`): resolve by
            // `(package, sigil+basename)` against the declaring symbol, not
            // by lexical scope — the qualifier names the package directly,
            // so the lexical chain is irrelevant (same seam as FQ calls).
            // Cross-package goto-def for non-local packages happens at query
            // time via `qualified_var_target()` + module_index.
            if let Some((pkg, name)) = self.refs[idx]
                .qualified_var_target()
                .map(|(p, n)| (p.to_string(), n))
            {
                if let Some(sym) = self.symbols.iter().find(|s| {
                    matches!(s.kind, SymKind::Variable | SymKind::Field)
                        && s.name == name
                        && s.package.as_deref() == Some(pkg.as_str())
                }) {
                    self.refs[idx].bind_symbol(sym.id);
                }
                continue;
            }

            let use_pkg = self.package_at_pos(ref_span_start).map(|s| s.to_string());

            // Walk scope chain to find the innermost matching declaration
            let mut current = Some(ref_scope);
            while let Some(scope_id) = current {
                if let Some(symbols) = scope_symbols.get(&scope_id) {
                    // Find the best match: declared before this ref, matching name,
                    // and (for `our`) sharing the use's enclosing package.
                    if let Some((_, sym_id, _, _)) = symbols.iter()
                        .filter(|(name, _, decl_point, gating_package)| {
                            if name != &ref_target { return false; }
                            if *decl_point > ref_span_start { return false; }
                            match gating_package {
                                // A package-less script (no `package` stmt) puts
                                // both the `our` decl and bare uses in the default
                                // `main`, but `package_at_pos` yields `None` there
                                // — so an absent enclosing package reads as `main`.
                                Some(decl_pkg) => {
                                    use_pkg.as_deref().unwrap_or("main") == decl_pkg.as_str()
                                }
                                None => true,
                            }
                        })
                        .last()
                    {
                        self.refs[idx].bind_symbol(*sym_id);
                        break;
                    }
                }
                current = self.scopes[scope_id.0 as usize].parent;
            }
        }
    }

    /// The post-walk **ChainTypingReducer**: one CST walk builds a
    /// `ChainTypingIndex` (assignment, return-expression, and invocant
    /// nodes by span); this drains it in two modes — `PreFold`
    /// (assignments + return arms feed the next `resolve_return_types`)
    /// and `PostFold` (invocants are query-time outputs, so they need
    /// every sub return type resolved first). The worklist driver
    /// (`fold_to_fixed_point`) calls `PreFold` each iteration and
    /// `PostFold` once after the lattice settles. Assignment + invocant
    /// typing run through `resolve_invocant_class_tree`; return arms
    /// feed the fold via `return_infos`.
    ///
    /// Idempotent across calls — assignments skip if a TC already
    /// exists, return arms only upgrade `None → Some`, invocants skip
    /// if `invocant_class` is already pinned. Running the reducer twice
    /// in `PostFold` mode types strictly the same set as one call.
    pub(super) fn run_chain_typing_reducer(
        &mut self,
        idx: &ChainTypingIndex<'a>,
        mode: ChainPassMode,
    ) {
        match mode {
            ChainPassMode::PreFold => {
                self.apply_chain_typing_assignments(idx);
                // Refresh `invocant_class` each iteration too.
                // Variable invocants whose TC just landed in the
                // worklist's previous iteration become resolvable
                // here — earlier this only ran in PostFold, which
                // meant the bag's `method_call_return` edges
                // (re-emitted from filled invocant_class) couldn't
                // see them until the loop already terminated.
                // `apply_chain_typing_invocants` is idempotent (skips
                // refs whose class is already pinned).
                self.apply_chain_typing_invocants(idx);
            }
            ChainPassMode::PostFold => {
                self.apply_chain_typing_invocants(idx);
            }
        }
    }

    /// Build the per-iteration lookup maps the fold passes reuse.
    /// `refs`/`symbols` are stable across the fold, so this runs once.
    /// Returns `(ref_by_span, method_sym_by_name)` as locals owned by
    /// `fold_to_fixed_point`; callers that need them receive `&`-refs.
    pub(super) fn build_fold_lookup_indices(
        &self,
    ) -> (
        std::collections::HashMap<(Point, Point), usize>,
        std::collections::HashMap<String, Vec<usize>>,
    ) {
        let mut ref_by_span = std::collections::HashMap::with_capacity(self.refs.len());
        for (i, r) in self.refs.iter().enumerate() {
            if matches!(r.kind, RefKind::MethodCall { .. }) {
                // First-wins matches the prior `position(...)` semantics.
                ref_by_span
                    .entry((r.span.start, r.span.end))
                    .or_insert(i);
            }
        }
        let mut sym_by_name: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, s) in self.symbols.iter().enumerate() {
            if matches!(s.kind, SymKind::Sub | SymKind::Method) {
                sym_by_name.entry(s.name.clone()).or_default().push(i);
            }
        }
        (ref_by_span, sym_by_name)
    }

    /// Fixed-point loop driving chain typing + reducer dispatch. Each
    /// iteration runs `ChainPassMode::PreFold` (assignment typing +
    /// return-arm refresh) followed by `resolve_return_types` (the
    /// reducer-dispatch driver); when the snapshot of Sub/Method return
    /// types and the bag length stops moving, the lattice has settled
    /// and the loop exits.
    ///
    /// The two re-emittable passes inside `resolve_return_types`
    /// (arity-return witnesses, call-binding propagator) are
    /// **clear-and-emit**: they drop their prior outputs at the start
    /// of every call so the bag stays canonical regardless of how many
    /// iterations the loop runs. Chain typing's bag-existence check
    /// keeps it idempotent on the same assignment span. The result:
    /// each fact lands in the bag exactly once at the end of the loop,
    /// no matter how deep the chain is.
    ///
    /// `MAX_FOLD_ITERATIONS` is the all-builds safety net: the lattice
    /// argument (witnesses are monotonically appended; reduced answers
    /// refine within a finite enum) guarantees termination, but a
    /// dependency-tracking bug could let the snapshot oscillate. The
    /// `debug_assert!` fires in dev so the regression is caught
    /// immediately; in release we log and break to keep the LSP
    /// responsive instead of spinning forever.
    ///
    /// `ChainPassMode::PostFold` (invocant-class refresh on
    /// `MethodCall` refs) runs once after the loop terminates, since
    /// invocant typing is a query-time write that doesn't feed back
    /// into the bag — a single pass against the now-final symbol
    /// table is sufficient.
    pub(super) fn fold_to_fixed_point(&mut self, idx: &ChainTypingIndex<'a>) {
        use crate::model::witnesses::ReducerRegistry;
        const MAX_FOLD_ITERATIONS: usize = 64;
        let (ref_by_span, method_sym_by_name) = self.build_fold_lookup_indices();
        // One registry for the entire fold — stateless/immutable, so sharing
        // across snapshot queries and seed pass is safe.
        let reg = ReducerRegistry::with_defaults();
        let mut iters = 0usize;
        let mut prev = self.fold_state_snapshot(&reg);
        loop {
            iters += 1;
            debug_assert!(
                iters < MAX_FOLD_ITERATIONS,
                "type-inference fold did not converge in {iters} iterations — \
                 lattice argument or dependency tracking is broken"
            );
            if iters >= MAX_FOLD_ITERATIONS {
                // Name the input: at corpus scale an un-located bail is
                // unactionable. The bulk indexers set the thread's current
                // file; other build sites (open docs) leave it unset.
                let whom = crate::util::timings::current_file()
                    .map(|f| format!(" in {f}"))
                    .unwrap_or_default();
                eprintln!(
                    "perl-lsp: type-inference fold exceeded {MAX_FOLD_ITERATIONS} iterations{whom}; \
                     bailing out to keep the LSP responsive"
                );
                break;
            }
            self.run_chain_typing_reducer(idx, ChainPassMode::PreFold);
            self.resolve_return_types(idx, &reg, &ref_by_span, &method_sym_by_name);
            // Mutation extension: fold key writes into variable shapes
            // (re-emittable, clear-and-emit). After the return-type
            // passes so call-binding-propagated shapes are visible.
            {
                let ctx = crate::model::witnesses::BagContext {
                    scopes: &self.scopes,
                    package_framework: &self.package_framework,
                    module_index: None,
                    package_parents: &self.package_parents,
                    app_surface_consumers: &self.app_surface_consumers,
                };
                crate::model::witnesses::emit_mutation_extension_witnesses(
                    &mut self.bag,
                    &ctx,
                    &self.key_writes,
                    true,
                );
            }
            let cur = self.fold_state_snapshot(&reg);
            if cur == prev {
                break;
            }
            prev = cur;
        }
        self.run_chain_typing_reducer(idx, ChainPassMode::PostFold);
        // Totals, not a line per file: at corpus scale the per-file form is
        // 138k unreadable lines, and the average is what says whether the
        // lattice is settling. `build::fold_to_fixed_point`'s sample count is
        // the divisor.
        crate::util::ghost_stats::count_by("build.fold_iterations", iters as u64);
        crate::util::ghost_stats::count_by("build.fold_bag_len", self.bag.len() as u64);
    }

    /// Snapshot of the answers the worklist driver tracks for fixed
    /// point detection: every Sub/Method's return type + the bag
    /// length. Two consecutive iterations producing the same snapshot
    /// means no fold pass changed any sub's answer AND chain typing
    /// pushed no new witnesses — the lattice has settled.
    ///
    /// `bag.len()` captures chain typing's monotonic grow. The
    /// re-emittable passes inside `resolve_return_types` (arity,
    /// call-binding) are clear-and-emit, so their bag contribution is
    /// stable across iterations and a flat total bag length means no
    /// new chain-assignment pushes either.
    pub(super) fn fold_state_snapshot(
        &self,
        reg: &crate::model::witnesses::ReducerRegistry,
    ) -> (Vec<(SymbolId, Option<InferredType>)>, usize, usize) {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, WitnessAttachment,
        };
        let ctx = self.bag_context();
        let mut answers: Vec<(SymbolId, Option<InferredType>)> = self
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
            .map(|s| {
                let att = WitnessAttachment::Symbol(s.id);
                let q = ReducerQuery {
                    attachment: &att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: Some(&ctx),
                };
                let resolved = match reg.query(&self.bag, &q) {
                    ReducedValue::Type(t) => Some(t),
                    ReducedValue::FactMap(_) | ReducedValue::None => None,
                };
                // A brand is a route VALUE identity, never a sub
                // return contract. Project it to its base class for
                // the fixed-point snapshot so a branded
                // implicit-return doesn't oscillate against the
                // brandless writeback push.
                (s.id, resolved.map(Self::debrand))
            })
            .collect();
        answers.sort_by_key(|(id, _)| id.0);
        // Count entries in the build-time invocant cache so
        // progressive chain-typing inside the loop registers as
        // movement — without this, an iteration that only newly
        // fills the invocant cache (driving the next iteration's
        // `emit_method_call_return_edges` to publish a new edge)
        // would produce the same answers/bag snapshot and the loop
        // would terminate prematurely.
        let invocant_filled = self.method_call_invocant.len();
        (answers, self.bag.len(), invocant_filled)
    }

    /// Symbolically execute the rhs of every `my $X = <expr>` and
    /// push the resulting class type into the bag. ONE recursive typer
    /// (`resolve_invocant_class_tree`) handles every expression shape
    /// it knows — scalar lookup, method-call chain, bareword, shift
    /// idiom, function call. No "is it a chain" branch. Whatever the
    /// rhs is, the typer descends.
    ///
    /// Idempotent: skips an assignment if a Variable witness for `$X`
    /// already exists at the assignment's start point. Walk-time
    /// inference covers literals/constructors via the existing path;
    /// this pass fills in anything that was unresolvable at walk time
    /// (specifically chains whose links' return types only became
    /// known after the first `resolve_return_types`).
    ///
    /// Provenance: each pushed witness carries
    /// `WitnessSource::Builder("type_constraint")` (via
    /// `push_type_constraint`) so a future debug dump can answer "why
    /// does $X have this type?" without re-running the typer.
    ///
    /// Reads from the shared `ChainTypingIndex.assignment_nodes` (built
    /// by `build_chain_typing_index`); does not walk the tree itself.
    ///
    /// The recursive typer (`resolve_invocant_class_tree`) uses
    /// `self.current_package` for the `$self` / `shift` / `$_[0]`
    /// fallback. After the live walk, `current_package` is stale
    /// (= last package opened in the file). To make the typer
    /// package-correct at every assignment site, we query
    /// `package_ranges` for the package at the assignment's
    /// position and override `current_package` for the call.
    ///
    /// Why `package_ranges` and not "track package_statement
    /// nodes": `package X;` is a SIBLING of the subs that follow
    /// it in the AST, not a parent — its scope extends forward
    /// through siblings until the next `package`. Walking the
    /// AST and saving/restoring on package_statement entry/exit
    /// doesn't model that. `package_ranges` is the flat record
    /// populated at walk time and trimmed by successor decls;
    /// a point-query gives the right answer at any byte.
    pub(super) fn apply_chain_typing_assignments(&mut self, idx: &ChainTypingIndex<'a>) {
        let mut to_push: Vec<(String, ScopeId, Span, InferredType)> = Vec::new();
        for &node in &idx.assignment_nodes {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else { continue };
            let span = node_to_span(node);

            // List-context row extraction: `my ($a, $b, ...) = $rs->search(
            // ...)` / `= $rs->all` — a resultset evaluated in LIST context
            // yields ROWS, so each scalar binds a row (not the resultset).
            // Runs BEFORE `get_var_text_from_lhs` (which returns None for a
            // paren-list decl, so these sites got no typing at all before).
            // The row moniker resolves to the FQ class at query time exactly
            // like the `->first`/`->create` scalar case.
            let list_scalars = self.paren_list_scalars(left);
            if !list_scalars.is_empty() {
                let saved = self.current_package.clone();
                self.current_package = self.package_at_pos(span.start).map(|s| s.to_string());
                let rhs = self.invocant_type_at_node(right);
                // The row class the list binds to: the RHS is either the
                // resultset itself (fluent `->search` / a bare `$rs`) or a
                // row-list verb (`->all`/`->populate`) whose INVOCANT is the
                // resultset — both yield rows of that resultset's row class.
                // (Row-list verbs are DBIC's; this small set lives here with
                // `extract_resultset_parametric` until the DBIC plugin owns
                // the parametric semantics.)
                let row_from = |t: &Option<InferredType>| match t {
                    Some(InferredType::Parametric(
                        crate::model::file_analysis::ParametricType::ResultSet { row, .. },
                    )) => Some(row.clone()),
                    _ => None,
                };
                let row = row_from(&rhs).or_else(|| {
                    if right.kind() != "method_call_expression" {
                        return None;
                    }
                    let m = right
                        .child_by_field_name("method")
                        .and_then(|m| m.utf8_text(self.source).ok())?;
                    if m != "all" && m != "populate" {
                        return None;
                    }
                    let inv_ty = right
                        .child_by_field_name("invocant")
                        .and_then(|inv| self.invocant_type_at_node(inv));
                    row_from(&inv_ty)
                });
                self.current_package = saved;
                if let Some(row) = row {
                    let row_ty = InferredType::ClassName(row);
                    let sid = self.innermost_scope_id_at(span.start);
                    for name in list_scalars {
                        let already = self.bag.all().iter().any(|w| {
                            let crate::model::witnesses::WitnessPayload::InferredType(t) = &w.payload else {
                                return false;
                            };
                            matches!(&w.attachment, crate::model::witnesses::WitnessAttachment::Variable { name: n, .. } if n == &name)
                                && w.span.start == span.start
                                && t.subsumes_narrowing(&row_ty)
                        });
                        if !already {
                            to_push.push((name, sid, span, row_ty.clone()));
                        }
                    }
                    continue;
                }
            }

            let Some(var) = self.get_var_text_from_lhs(left) else { continue };
            // Compute the fresh type up front so the idempotency check
            // can compare informativeness. (Cheap: a bag chase on the
            // already-resolved RHS.)
            let saved_pkg_probe = self.current_package.clone();
            self.current_package = self.package_at_pos(span.start).map(|s| s.to_string());
            let fresh = self
                .invocant_type_at_node(right)
                .or_else(|| self.resolve_invocant_class_tree(right).map(InferredType::ClassName));
            self.current_package = saved_pkg_probe;

            // Idempotency: skip if an already-pushed Variable witness at
            // this assignment's start is at least as informative as the
            // fresh answer. A plain `ClassName(Route)` does NOT subsume a
            // `BrandedRoute`, so the route brand legitimately upgrades the
            // walk-time materialization on a later fold iteration; once
            // the brand is in the bag it subsumes the next (identical)
            // brand and the loop settles.
            let already_typed = self.bag.all().iter().any(|w| {
                let crate::model::witnesses::WitnessPayload::InferredType(t) = &w.payload else {
                    return false;
                };
                matches!(&w.attachment, crate::model::witnesses::WitnessAttachment::Variable { name, .. } if name == &var)
                    && w.span.start == span.start
                    && fresh.as_ref().map_or(true, |f| t.subsumes_narrowing(f))
            });
            if already_typed {
                continue;
            }
            // Innermost scope containing this assignment.
            let scope_idx = self
                .scopes
                .iter()
                .enumerate()
                .filter(|(_, s)| crate::model::file_analysis::contains_point(&s.span, span.start))
                .min_by_key(|(_, s)| {
                    let r = (s.span.end.row.saturating_sub(s.span.start.row)) as u64;
                    let c = if s.span.start.row == s.span.end.row {
                        s.span.end.column.saturating_sub(s.span.start.column) as u64
                    } else {
                        0
                    };
                    r * 1_000_000 + c
                })
                .map(|(i, _)| i);

            let scope_pkg = self.package_at_pos(span.start).map(|s| s.to_string());

            let saved_pkg = self.current_package.clone();
            if scope_pkg.is_some() {
                self.current_package = scope_pkg;
            }
            // Read the RHS's full `InferredType` first — Parametric
            // shapes need to land on the variable as Parametric, not
            // unwrapped to their `base` via `class_name()`. Falls
            // back to the class-only `resolve_invocant_class_tree`
            // for the bareword-degrade tail (a non-class type on a
            // bareword maps to "treat the syntactic text as a
            // class") which the type-aware path doesn't model.
            let ty_opt = self
                .invocant_type_at_node(right)
                .or_else(|| self.resolve_invocant_class_tree(right).map(InferredType::ClassName))
                .or(fresh);
            self.current_package = saved_pkg;

            if let Some(ty) = ty_opt {
                let sid = scope_idx.map(|i| self.scopes[i].id).unwrap_or(ScopeId(0));
                to_push.push((var, sid, span, ty));
            }
        }

        for (variable, scope, constraint_span, ty) in to_push {
            self.push_type_constraint(TypeConstraint {
                variable,
                scope,
                constraint_span,
                inferred_type: ty,
            });
        }
    }

    /// Re-resolve `invocant_class` on every MethodCall ref using the
    /// tree + the now-final symbol table (return types have been
    /// filled in by the second `resolve_return_types`). This catches
    /// function-call chains like `get_foo()->bar()` where the
    /// invocant's class can only be pinned after `get_foo`'s
    /// return_type is known.
    ///
    /// Reads from the shared `ChainTypingIndex.invocant_nodes`. Refs
    /// whose class was already pinned during the walk keep their value.
    pub(super) fn apply_chain_typing_invocants(&mut self, idx: &ChainTypingIndex<'a>) {
        // Collect ref indices + their invocant nodes first so we
        // don't borrow `self.refs` mutably while also calling the
        // bag-routed resolver (which reads `&self`).
        let mut pending: Vec<(usize, Node<'a>)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if let RefKind::MethodCall {
                invocant_span: Some(sp),
                ..
            } = &r.kind
            {
                if self.method_call_invocant.contains_key(&i) {
                    continue;
                }
                if let Some(n) = idx.invocant_nodes.get(&(sp.start, sp.end)).copied() {
                    pending.push((i, n));
                }
            }
        }
        for (i, node) in pending {
            if let Some(class) = self.resolve_invocant_class_tree(node) {
                self.method_call_invocant.insert(i, class);
            }
        }
    }

    /// Post-walk: emit even-position stringy args of every resolved
    /// `MethodCall` ref as `HashKeyAccess` refs owned by
    /// `Sub{invocant_class, method_name}`. Pairs with the
    /// `HashKeyDef` symbols that `has` / `bless { … }` synthesize on
    /// the callee side; without these refs, `ref_at` on a constructor
    /// arg only finds the broad `MethodCall` ref and rename clobbers
    /// the wrong token.
    ///
    /// Runs post-PostFold so `invocant_class` is already filled
    /// against the canonical bag — moves the keys-emission gating
    /// off the walk-time `invocant_class.is_some()` shortcut that
    /// previously forced syntactic walk-time class resolution to
    /// keep working.
    pub(super) fn emit_method_call_arg_keys(&mut self, idx: &ChainTypingIndex<'a>) {
        // Snapshot first so we can `&mut self` the emit loop below.
        // `parametric` = true → Parametric receiver, the type is
        // the gate (cross-file producer's HashKeyDef isn't visible
        // here; the Parametric witness already pinned the class).
        // false → strict has_hash_key_def gating (prevents
        // `Foo::bar(name=>1)` from latching onto `Sub{Foo,new}`
        // keys when `name` isn't actually a `bar` arg).
        // Three paths for hash-key arg owner:
        //
        //  * **Parametric receiver claims the method.** Ask the
        //    flavor: `p.method_arg_owner(method)`. `Some` means
        //    "I claim — emit unconditionally with this owner;
        //    the type IS the gate." Cross-file producer's
        //    HashKeyDef may not be visible at consumer build;
        //    the type already pinned the class.
        //  * **Non-Parametric receiver, locally typed.** Fall
        //    through to `Sub { package: receiver, name: method
        //    }` with `has_hash_key_def` strict-eq gating —
        //    prevents `Foo::bar(name=>1)` from latching onto
        //    `Sub{Foo,new}` keys.
        //  * **Chain receiver, type unresolvable at build.**
        //    Build doesn't have `module_index` access, so a
        //    chain hop whose link-type lives in another file
        //    (helper-style: `$c->sner_r->search({...})`) returns
        //    None at this point. Emit `HashKeyAccess` refs
        //    eagerly with `owner: None`; enrichment runs later
        //    with `module_index` and fixes the owner once the
        //    receiver type is resolvable. Same precedent as
        //    cross-file invocant refresh (PR #34): build emits
        //    what it can; enrichment fills gaps.
        enum Path<'a> {
            Claimed(HashKeyOwner, Node<'a>),
            Strict(HashKeyOwner, Node<'a>),
            StrictOrDefer(HashKeyOwner, Node<'a>),
            Deferred(Node<'a>),
            ColumnKeyed(HashKeyOwner, Node<'a>),
        }
        // Plugin-declared verbs whose first hashref arg is column-keyed (owned
        // so it doesn't borrow `self` across the `&mut self` emit loop below).
        let column_keyed: std::collections::HashSet<String> =
            self.plugins.column_keyed_verbs().map(|s| s.to_string()).collect();
        let mut pending: Vec<Path<'a>> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if !matches!(r.kind, RefKind::MethodCall { .. }) {
                continue;
            }
            let Some(args) = idx
                .method_call_args
                .get(&(r.span.start, r.span.end))
                .copied()
            else {
                continue;
            };
            let invocant_node = idx
                .invocant_nodes
                .get(&match r.kind {
                    RefKind::MethodCall { invocant_span: Some(s), .. } => (s.start, s.end),
                    _ => continue,
                })
                .copied();
            let inv_ty = invocant_node.and_then(|n| self.invocant_type_at_node(n));
            if let Some(claimed) = inv_ty
                .as_ref()
                .and_then(|ty| ty.as_parametric())
                .and_then(|p| p.method_arg_owner(&r.target_name))
            {
                pending.push(Path::Claimed(claimed, args));
                continue;
            }
            if let Some(cls) = self.method_call_invocant.get(&i) {
                // Definitions only — a `use Point` import symbol
                // (SymKind::Module) does not make the class local.
                let local_class = self.symbols.iter().any(|s| {
                    matches!(s.kind, SymKind::Package | SymKind::Class) && s.name == *cls
                });
                // A column-keyed verb (`search`/`create`/…) on a locally-defined
                // class: link the first hashref's column keys to the class's
                // columns. Cross-file (class elsewhere) stays `StrictOrDefer` —
                // the deferred owner mints the column owner at query time.
                let owner = HashKeyOwner::Sub {
                    package: Some(cls.clone()),
                    name: r.target_name.clone(),
                };
                // A class that defines its OWN `sub <verb>` has overridden DBIC's
                // verb — the call dispatches to the user's method, whose hash arg
                // is not column-keyed — so don't column-key it (fall to Strict).
                let user_shadows_verb = self.symbols.iter().any(|s| {
                    matches!(s.kind, SymKind::Sub | SymKind::Method)
                        && s.name == r.target_name
                        && s.package.as_deref() == Some(cls.as_str())
                });
                if local_class && !user_shadows_verb && column_keyed.contains(&r.target_name) {
                    pending.push(Path::ColumnKeyed(owner, args));
                    continue;
                }
                pending.push(if local_class {
                    Path::Strict(owner, args)
                } else {
                    // The class lives elsewhere — a local def miss can't
                    // veto; defer the gate to query time.
                    Path::StrictOrDefer(owner, args)
                });
                continue;
            }
            // Receiver type unresolvable at build. Only defer for
            // chain receivers — bareword/variable receivers that
            // didn't type are likely user-error or untyped code,
            // not cross-file. Chain receivers (invocant is a
            // method-call expression) are exactly the case
            // post-enrichment can fix.
            let is_chain_receiver = invocant_node
                .map(|n| n.kind() == "method_call_expression")
                .unwrap_or(false);
            if is_chain_receiver {
                pending.push(Path::Deferred(args));
            }
        }
        for path in pending {
            match path {
                Path::Claimed(owner, args) => {
                    self.emit_call_arg_key_accesses(args, Gate::Open(owner));
                }
                Path::Strict(owner, args) => {
                    self.emit_call_arg_key_accesses(args, Gate::Strict(owner));
                }
                Path::StrictOrDefer(owner, args) => {
                    self.emit_call_arg_key_accesses(args, Gate::StrictOrDefer(owner));
                }
                Path::Deferred(args) => {
                    self.emit_call_arg_key_accesses(args, Gate::Deferred);
                }
                Path::ColumnKeyed(owner, args) => {
                    self.emit_call_arg_key_accesses(args, Gate::ColumnKeyed(owner));
                }
            }
        }
    }

    /// Emit a `HashKeyAccess` ref for a hash-key access applied to a
    /// method-call result whose return type is known
    /// (`$obj->get_config->{host}`). Mirrors `resolve_hash_owner_from_tree`'s
    /// query-time logic at build time so the stored ref carries a
    /// resolvable owner (cross-file `refs_to` + the O(1) linked-symbol
    /// link both consult the stored owner, not the tree fallback):
    ///   - method returns a Sub-keyed hash (`return { host => … }`) →
    ///     `Sub{package, method}` owner, matching the implicit-return
    ///     HashKeyDefs.
    ///   - method returns a class instance → `Class(C)` via
    ///     `hash_key_class()` (Parametric row-class or dispatch class).
    /// Honest about ignorance: a chain whose return type doesn't resolve
    /// to either emits nothing, so a should-miss stays a miss rather
    /// than a wrong-owner latch.
    ///
    /// Post-fold owner upgrade for variable derefs: `$row->{name}` where
    /// `$row`'s class only settled during the worklist fold (RowOf
    /// projections, chain assignments). `resolve_hash_key_owners` ran
    /// pre-fold and could only stamp the lexical `Variable` owner; this
    /// re-asks the canonical bag and promotes to `Class(C)` when the
    /// variable's type yields a class. Untyped variables keep their
    /// lexical grouping — plain `%config` hashes are untouched.
    pub(super) fn upgrade_variable_hash_key_owners(&mut self) {
        let mut upgrades: Vec<(usize, HashKeyOwner)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            let RefKind::HashKeyAccess { ref var_text } = r.kind else {
                continue;
            };
            if !matches!(r.hash_key_owner(), Some(HashKeyOwner::Variable { .. })) {
                continue;
            }
            if !var_text.starts_with('$') {
                continue;
            }
            let Some(t) = self.bag_query_variable(var_text, r.scope, r.span.start) else {
                continue;
            };
            let Some(class) = t.class_name() else { continue };
            upgrades.push((i, HashKeyOwner::Class(class.to_string())));
        }
        for (i, o) in upgrades {
            self.refs[i].bind_hash_key_owner(o);
        }
    }

    /// Runs post-fold (after `emit_method_call_arg_keys`) so
    /// `invocant_type_at_node` answers against the canonical bag.
    pub(super) fn emit_chained_hash_key_refs(&mut self, idx: &ChainTypingIndex<'a>) {
        // Snapshot resolved (key_span, key_text, access, owner) first
        // so the emit loop can `&mut self`.
        let mut pending: Vec<(Span, String, AccessKind, HashKeyOwner)> = Vec::new();
        for &node in &idx.chained_hash_elements {
            let Some(container) = node.named_child(0) else { continue };
            let Some(key_node) = node.child_by_field_name("key") else { continue };
            let Some((key_text, is_dynamic)) = self.extract_key_text(key_node) else { continue };
            if is_dynamic { continue; }

            // First the Sub-keyed-return path: the called method's
            // implicit/explicit `return { k => … }` registers its keys
            // under `Sub{package, method}`. If such a def exists for our
            // key name, that's the precise owner. This case has no class
            // identity (the value is a plain HashRef), so it must be
            // tried before the `hash_key_class()` fallback.
            let method_name = container
                .child_by_field_name("method")
                .and_then(|n| n.utf8_text(self.source).ok())
                .map(|s| s.to_string());
            let sub_owner = method_name.as_ref().and_then(|name| {
                let mut candidates: Vec<HashKeyOwner> = self
                    .symbols
                    .iter()
                    .filter(|s| {
                        matches!(s.kind, SymKind::Sub | SymKind::Method) && &s.name == name
                    })
                    .map(|s| HashKeyOwner::Sub {
                        package: s.package.clone(),
                        name: name.clone(),
                    })
                    .collect();
                // Imported synthetic defs carry a None package.
                candidates.push(HashKeyOwner::Sub { package: None, name: name.clone() });
                candidates
                    .into_iter()
                    .find(|owner| self.has_hash_key_def(&key_text, owner))
            });

            let owner = match sub_owner {
                Some(o) => o,
                None => {
                    // Class-instance return: project to the hash-key
                    // class (Parametric row-class else dispatch class).
                    let Some(class) = self
                        .invocant_type_at_node(container)
                        .and_then(|ty| ty.hash_key_class().map(|s| s.to_string()))
                    else {
                        continue;
                    };
                    HashKeyOwner::Class(class)
                }
            };
            pending.push((
                node_to_span(key_node),
                key_text,
                self.determine_access(node),
                owner,
            ));
        }
        for (span, key_text, access, owner) in pending {
            self.refs.push(Ref {
                kind: RefKind::HashKeyAccess {
                    var_text: String::new(),
                },
                span,
                scope: self.scope_at_point(span.start),
                target_name: key_text,
                access,
                binding: Some(crate::model::file_analysis::RefBinding::HashKey {
                    owner,
                    sym: None,
                }),
                folded_from: None,
                arg_count: None,
            });
        }
    }


    /// The tiny reducer-dispatch driver. Each step is a named helper
    /// below. The call-binding propagator and hash-key-owner fixup are
    /// post-fold sync passes — conceptually "not reducers" and stay
    /// procedural,
    /// but factored out as named methods. The reducer registry
    /// (`PluginOverrideReducer`, `ReturnExprReducer`,
    /// `MethodOnClassReducer`, `SubReturnReducer`) lives in
    /// `witnesses.rs` and folds the bag at query time.
    pub(super) fn resolve_return_types(
        &mut self,
        idx: &ChainTypingIndex<'a>,
        reg: &crate::model::witnesses::ReducerRegistry,
        ref_by_span: &std::collections::HashMap<(Point, Point), usize>,
        method_sym_by_name: &std::collections::HashMap<String, Vec<usize>>,
    ) {
        self.emit_arity_return_witnesses();
        // Brand BEFORE method-call edges so `route_branded_refs` is
        // current when `emit_method_call_return_edges` consults it to
        // skip route calls — otherwise the skip set lags one iteration
        // and the bag oscillates (the fold never reaches a fixed point).
        self.emit_route_brand_witnesses(idx, ref_by_span);
        self.emit_method_call_return_edges();
        self.emit_defined_narrowing_witnesses();
        let (return_types, return_provenance) = self.seed_return_types_from_bag(reg, method_sym_by_name);
        self.write_back_sub_return_types(&return_provenance);
        self.propagate_call_bindings_to_constraints(&return_types);
        self.fixup_call_bound_hash_key_owners(&return_types);
    }

    /// Re-emittable: stamp the resolved `BrandedRoute` onto the
    /// `Expression(refidx)` of every route-builder `method_call_expression`,
    /// so a `my $x = $r->...->to('ctrl#')` declaration (which types via
    /// `Edge(Expression(refidx))`) carries the brand, and the next
    /// iteration's chained calls / partial `->to('#action')` read the
    /// inherited controller off it. The brand is computed by the single
    /// build-time symbolic executor (`invocant_type_at_node`); this pass
    /// only publishes its answer onto the bag. Recomputed each iteration
    /// (clear-and-emit on tag `route_brand`) because the receiver type it
    /// reads converges as the fold progresses.
    pub(super) fn emit_route_brand_witnesses(
        &mut self,
        idx: &ChainTypingIndex<'a>,
        ref_by_span: &std::collections::HashMap<(Point, Point), usize>,
    ) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        self.bag.remove_by_source_tag("route_brand");
        self.route_branded_refs.clear();

        // Snapshot (refidx, brand) first — `invocant_type_at_node`
        // borrows `&self`, so we can't push while iterating.
        let mut brands: Vec<(usize, InferredType)> = Vec::new();
        for &node in &idx.method_call_nodes {
            let span = node_to_span(node);
            let Some(&refidx) = ref_by_span.get(&(span.start, span.end)) else {
                continue;
            };
            let ty = self.invocant_type_at_node(node);
            if let Some(b @ InferredType::BrandedRoute { .. }) = ty {
                brands.push((refidx, b));
            }
        }
        for (refidx, brand) in brands {
            let r_span = self.refs[refidx].span;
            // Claim this ref so `emit_method_call_return_edges` skips
            // its `Edge(MethodOnClass{Route, to})` — that edge folds to
            // a plain `ClassName(Route)` and `FrameworkAwareTypeFold`
            // (which runs before `ExprReturn`) would answer with it,
            // masking the brand. Same precedent as
            // `parametric_emitted_refs`.
            self.route_branded_refs.insert(refidx);
            self.bag.push(Witness {
                attachment: WitnessAttachment::Expression(crate::model::witnesses::RefIdx(refidx as u32)),
                source: WitnessSource::Builder("route_brand".into()),
                payload: WitnessPayload::InferredType(brand),
                span: r_span,
            });
        }
    }

    /// Re-emittable: for every `MethodCall` ref whose
    /// `invocant_class` is filled (walk-time syntax-known invocants
    /// like `Foo->m`, plus PostFold-resolved variable invocants),
    /// publish `Expression(refidx) → Edge(MethodOnClass{class, method})`
    /// so the chain typer's `bag_query_expression` chases the
    /// receiver-and-method-resolved type through the class-keyed
    /// attachment. Refs without a filled class skip emission —
    /// without a known class there's no class-keyed slot to target.
    ///
    /// Resolve a qualified method token to the class(es) the lookup starts
    /// at. Qualifier semantics live on `MethodToken`; the SUPER arm is the
    /// only one needing builder state (the enclosing package's parents —
    /// possibly several). `Bare` has no qualifier → empty.
    pub(super) fn qualified_dispatch_classes(
        &self,
        token: crate::model::conventions::MethodToken<'_>,
        enclosing: &str,
    ) -> Vec<String> {
        match token {
            crate::model::conventions::MethodToken::Super(_) => {
                self.package_parents.get(enclosing).cloned().unwrap_or_default()
            }
            t => t.literal_package().map(|p| vec![p.to_string()]).unwrap_or_default(),
        }
    }

    /// Clear-and-emit on tag `method_call_return` so repeat calls
    /// inside the worklist driver stay idempotent.
    pub(super) fn emit_method_call_return_edges(&mut self) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

        self.bag.remove_by_source_tag("method_call_return");

        let mut edges: Vec<Witness> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if !matches!(r.kind, RefKind::MethodCall { .. }) {
                continue;
            }
            // Refs we've already handed a Parametric witness keep
            // their custom InferredType — publishing the receiver-
            // class's plain return-type edge would mask the
            // row-class arg via FrameworkAwareTypeFold's
            // class-axis short-circuit. See
            // `parametric_emitted_refs` doc on the Builder struct.
            if self.parametric_emitted_refs.contains(&i) {
                continue;
            }
            // Route-branded calls own their `Expression(refidx)` type
            // (the `BrandedRoute` from `emit_route_brand_witnesses`);
            // the method-on-class edge would fold to a brandless
            // `ClassName(Route)` and mask it.
            if self.route_branded_refs.contains(&i) {
                continue;
            }
            // A qualified method token names an EXPLICIT dispatch class —
            // Perl looks the method up on the named class, not the
            // invocant's. Either way the call still blesses into the
            // INVOCANT's class, so the result is typed relative to the
            // invocant (falling back to the enclosing package).
            let token = crate::model::conventions::MethodToken::parse(&r.target_name);
            if !matches!(token, crate::model::conventions::MethodToken::Bare(_)) {
                let method = token.name();
                let Some(encl) = self.package_at_pos(r.span.start) else { continue };
                let receiver_class = self
                    .method_call_invocant
                    .get(&i)
                    .cloned()
                    .unwrap_or_else(|| encl.to_string());
                let arity = self.method_call_arity.get(&i).copied().unwrap_or(0);
                let lookup_classes = self.qualified_dispatch_classes(token, encl);
                for class in lookup_classes {
                    edges.push(Witness {
                        attachment: WitnessAttachment::Expression(crate::model::witnesses::RefIdx(
                            i as u32,
                        )),
                        source: WitnessSource::Builder("method_call_return".into()),
                        payload: WitnessPayload::QualifiedCallReturn {
                            method_lookup: WitnessAttachment::MethodOnClass {
                                class,
                                name: method.to_string(),
                            },
                            receiver_class: receiver_class.clone(),
                            arity,
                        },
                        span: r.span,
                    });
                }
                continue;
            }
            let Some(class) = self.method_call_invocant.get(&i) else {
                continue;
            };
            let target = WitnessAttachment::MethodOnClass {
                class: class.clone(),
                name: r.target_name.clone(),
            };
            // Pin the call's arity so the chase dispatches the right
            // overload arm (fluent writer vs getter) regardless of the
            // outer query's hint. Plugin-emitted refs that never went
            // through `visit_method_call` have no recorded arity — fall
            // back to a plain edge (hint-less union dispatch) for them.
            let payload = match self.method_call_arity.get(&i) {
                Some(&arity) => WitnessPayload::CallReturn { target, arity },
                None => WitnessPayload::Edge(target),
            };
            edges.push(Witness {
                attachment: WitnessAttachment::Expression(crate::model::witnesses::RefIdx(i as u32)),
                source: WitnessSource::Builder("method_call_return".into()),
                payload,
                span: r.span,
            });
        }
        for w in edges {
            self.bag.push(w);
        }
    }

    /// Single-attachment registry query for `Expr(span)`. Used by
    /// `emit_arity_return_witnesses` (per-RI) and the implicit-last-
    /// expression fallback in `seed_return_types_from_bag`. Threads the
    /// file's scope topology + per-package framework as a `BagContext`
    /// so Edge chases through `Variable{...}` use scope-chain +
    /// framework-aware semantics.
    pub(super) fn bag_query_expr_span(&self, span: Span) -> Option<InferredType> {
        self.bag_query_attachment(&crate::model::witnesses::WitnessAttachment::Expr(span))
    }

    /// Generic registry query against any attachment shape.
    /// `bag_query_expr_span` is a thin wrapper for the common
    /// `Expr(span)` case; the coderef-call arm of
    /// `invocant_type_at_node` uses this to chase a CodeRef's
    /// `return_edge` (which can be `Expr(body_last)` for anon
    /// literals or `MethodOnClass{class, name}` for `\&foo`).
    pub(super) fn bag_query_attachment(
        &self,
        att: &crate::model::witnesses::WitnessAttachment,
    ) -> Option<InferredType> {
        self.bag_query_attachment_with(att, None, None)
    }

    /// Same chase as `bag_query_attachment`, but threads
    /// `arity_hint` and `receiver` into the registry query so
    /// `ReturnExprReducer` (UnionOnArgs branches, Receiver
    /// substitution, Operator(RowOf, Receiver) projections) can
    /// answer with the right arm. Used by the chain typer's
    /// coderef-call / dynamic-method-call arms where the call
    /// site contributes both pieces of context.
    pub(super) fn bag_query_attachment_with(
        &self,
        att: &crate::model::witnesses::WitnessAttachment,
        arity_hint: Option<u32>,
        receiver: Option<InferredType>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry,
        };
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context();
        let q = ReducerQuery {
            attachment: att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint,
            receiver,
            args: Vec::new(),
            context: Some(&ctx),
        };
        match reg.query(&self.bag, &q) {
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        }
    }

    /// Bag-routed lookup for a variable's type at a point. Walks the
    /// scope chain via `query_variable_type` (FrameworkAwareTypeFold,
    /// branch-arm fold, etc. all apply). Used by the post-walk chain
    /// typer once the bag is fully populated by `populate_witness_bag`.
    /// The registry-query context at build time: same field set every bag
    /// query threads, always index-free (single-file until enrichment).
    pub(super) fn bag_context(&self) -> crate::model::witnesses::BagContext<'_> {
        crate::model::witnesses::BagContext {
            scopes: &self.scopes,
            package_framework: &self.package_framework,
            module_index: None,
            package_parents: &self.package_parents,
            app_surface_consumers: &self.app_surface_consumers,
        }
    }

    pub(super) fn bag_query_variable(
        &self,
        name: &str,
        scope: ScopeId,
        point: Point,
    ) -> Option<InferredType> {
        crate::model::witnesses::query_variable_type(&self.bag, &self.bag_context(), name, scope, point)
    }

    /// Bag-routed lookup for a sub's return type by name. Goes through
    /// `query_sub_return_type` so symbol-declarative arity dispatch
    /// (`ReturnExprReducer` on `UnionOnArgs`), the per-Symbol stored
    /// return (`SubReturnReducer`), and cross-file imports (recurse
    /// into the cached module's bag) all compose. `arity_hint` is
    /// `None` for the
    /// chain-typer's invocant resolution since chain typing wants
    /// "what does this sub return when called as I see it being
    /// called" — for invocant resolution that's typically the
    /// zero-arg form (a chain like `app->routes` calls `app()` then
    /// `routes()` on its return).
    pub(super) fn bag_query_named_sub(&self, name: &str, arity_hint: Option<u32>) -> Option<InferredType> {
        let ctx = self.bag_context();
        crate::model::witnesses::query_sub_return_type(
            &self.bag,
            &self.symbols,
            name,
            arity_hint,
            None,
            Some(&ctx),
        )
    }

    /// Bag-routed lookup for a method-call expression's return type
    /// via its ref index. Mirrors `FileAnalysis::method_call_return_type_via_bag`
    /// but reads `&self.bag` (the in-progress builder bag). Includes
    /// the `FirstParam → ClassName` projection so chain-typer
    /// consumers see a concrete class instead of a parametric type.
    ///
    /// `receiver` populates `q.receiver` so `ReturnExpr::Receiver` /
    /// `Operator(RowOf(Receiver))` substitution evaluates correctly
    /// at the chase. Direct method calls (`$rs->find(...)`) pass the
    /// invocant's resolved type — for DBIC, that's the
    /// `Parametric(ResultSet)` flowing through chain typing.
    pub(super) fn bag_query_expression(
        &self,
        ref_idx: crate::model::witnesses::RefIdx,
        arity_hint: Option<u32>,
        receiver: Option<InferredType>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry,
            WitnessAttachment,
        };
        let att = WitnessAttachment::Expression(ref_idx);
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context();
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint,
            receiver,
            args: Vec::new(),
            context: Some(&ctx),
        };
        match reg.query(&self.bag, &q) {
            ReducedValue::Type(InferredType::FirstParam { package }) => {
                Some(InferredType::ClassName(package))
            }
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        }
    }


    /// Step 2: emit a `UnionOnArgs` `ReturnExpr` per arity-
    /// discriminated sub on `Symbol(sub_id)` and
    /// `MethodOnClass{class, name}`. The bag's `ReturnExprReducer`
    /// dispatches the call's `arity_hint` against the union's
    /// branches.
    ///
    /// Walk-time arity classification stays a `Some(_)` enum on
    /// `ReturnInfo`; per-arm types are read out by querying each
    /// body's `Expr(span)` against the current bag. Only fires for
    /// arity-DISCRIMINATED subs (≥ 1 `Zero`/`Exact(_)` arm) — plain
    /// subs leave the per-Symbol slot empty so SubReturnReducer's
    /// stored-return read is what answers.
    ///
    /// Idempotent across re-runs — clears every prior
    /// `arity_detection` witness from the bag before re-emitting.
    /// The worklist driver calls `resolve_return_types` repeatedly
    /// until fixed point; without this clear-and-emit, each
    /// iteration would duplicate every arity witness in the bag.
    pub(super) fn emit_arity_return_witnesses(&mut self) {
        use crate::model::witnesses::{
            ArgGuard, ReturnExpr, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };

        self.bag.remove_by_source_tag("arity_detection");

        // Group return arms by scope. Only scopes with at least one
        // Zero / Exact arm are arity-discriminated.
        let mut by_scope: std::collections::HashMap<
            ScopeId,
            Vec<(ArityBranch, Span)>,
        > = std::collections::HashMap::new();
        // A `Vec` + membership set rather than a bare `HashSet`: the emission
        // loop below walks this in order, and a `HashSet` would walk it in
        // hash order — different between two builds of the same file, which
        // makes the bag (and the cache blob minted from it) nondeterministic.
        // Document order, since `return_infos` is already in walk order.
        let mut discriminated: Vec<ScopeId> = Vec::new();
        let mut discriminated_seen: std::collections::HashSet<ScopeId> =
            std::collections::HashSet::new();
        for ri in &self.return_infos {
            let Some(branch) = ri.arity_branch else { continue };
            if matches!(
                branch,
                ArityBranch::Zero
                    | ArityBranch::Exact(_)
                    | ArityBranch::AtMost(_)
                    | ArityBranch::AtLeast(_)
            ) {
                if discriminated_seen.insert(ri.scope) {
                    discriminated.push(ri.scope);
                }
            }
            if let Some(span) = ri.body_span {
                by_scope.entry(ri.scope).or_default().push((branch, span));
            }
        }

        let mut to_push: Vec<Witness> = Vec::new();
        // Symbols whose arity union governs: their non-arity `return_arm_chain`
        // fallback must be retracted so a query at an arity the union DOESN'T
        // cover honestly answers None instead of the arm-join's arity-blind
        // merge (which subsumes `HashRef` under a fluent `$self` return — the
        // exact leak that made Mojo::DOM::attr report the invocant class at
        // arity 1).
        let mut authoritative_syms: Vec<SymbolId> = Vec::new();
        for scope in &discriminated {
            let arms = match by_scope.get(scope) {
                Some(a) => a,
                None => continue,
            };
            // Sort arms so narrow guards come before broad ones —
            // ReturnExprReducer's UnionOnArgs picks first-match.
            // Empty / Exact(N) before Default; ties broken by
            // source order (stable).
            // The KNOWN arm types (drop the unresolvable ones). Whether they
            // genuinely agree decides if the arity union must be authoritative
            // (retract the non-arity fallback) — see the retraction below.
            let known_arm_types: Vec<InferredType> = arms
                .iter()
                .filter_map(|(_, body_span)| self.bag_query_expr_span(*body_span))
                .collect();
            let mut sorted: Vec<(ArgGuard, ReturnExpr)> = Vec::new();
            // Pass 1a: exact-match guards (Empty / Exact) — most specific,
            // must precede the magnitude bands so a point arity claims its own
            // arm first (`unless @_` before `unless @_ > 1` at arity 0).
            for (branch, body_span) in arms {
                let Some(t) = self.bag_query_expr_span(*body_span) else { continue };
                match branch {
                    ArityBranch::Zero => {
                        sorted.push((ArgGuard::Empty, ReturnExpr::Concrete(t)));
                    }
                    ArityBranch::Exact(n) => {
                        sorted.push((ArgGuard::Exact(*n), ReturnExpr::Concrete(t)));
                    }
                    _ => {}
                }
            }
            // Pass 1b: magnitude bands (AtMost / AtLeast) — narrower than the
            // fluent Any arm, broader than an exact point.
            for (branch, body_span) in arms {
                let Some(t) = self.bag_query_expr_span(*body_span) else { continue };
                match branch {
                    ArityBranch::AtMost(n) => {
                        sorted.push((ArgGuard::AtMost(*n), ReturnExpr::Concrete(t)));
                    }
                    ArityBranch::AtLeast(n) => {
                        sorted.push((ArgGuard::AtLeast(*n), ReturnExpr::Concrete(t)));
                    }
                    _ => {}
                }
            }
            // Pass 2: Default arm(s). Fold to a single fall-through branch
            // — multiple Default arms with disagreeing types lose
            // their disagreement signal here; the per-arm fold
            // runs separately (`seed_return_types_from_bag`) and is
            // what surfaces ambiguity in the writeback.
            //
            // The fall-through fires only when NO earlier `unless`-guarded
            // arm returned. When an `AtMost(N)` early-return arm precedes it
            // (the Mojo `attr` getter guard), the fall-through can't fire at
            // arity ≤ N — so its guard is `AtLeast(N+1)`, not `Any`. This is
            // what keeps the fluent `return $self` from wrongly claiming the
            // low-arity getter slot: if the `AtMost` arm's own type didn't
            // resolve (dynamic key), that arity honestly answers None rather
            // than the fluent class. Without an AtMost peel, `Any` stands.
            let atmost_ceiling = arms
                .iter()
                .filter_map(|(b, _)| match b {
                    ArityBranch::AtMost(n) => Some(*n),
                    _ => None,
                })
                .max();
            let mut default_t: Option<InferredType> = None;
            for (branch, body_span) in arms {
                if matches!(branch, ArityBranch::Default) {
                    if let Some(t) = self.bag_query_expr_span(*body_span) {
                        default_t = Some(t);
                    }
                }
            }
            if let Some(t) = default_t {
                let guard = match atmost_ceiling {
                    Some(n) => ArgGuard::AtLeast(n.saturating_add(1)),
                    None => ArgGuard::Any,
                };
                sorted.push((guard, ReturnExpr::Concrete(t)));
            }
            if sorted.is_empty() {
                continue;
            }
            let Some(sym_id) = self.find_sub_symbol_for_scope(*scope) else { continue };
            // Retract the non-arity `return_arm_chain` fallback ONLY when the
            // known arms genuinely DISAGREE (Mojo::DOM::attr: HashRef vs the
            // fluent $self). Then a gap arity honestly answers None instead of
            // the arm-join's fluent leak. When the arms AGREE (Path::Tiny::path
            // — every branch returns the invocant class), the arm-join is the
            // correct answer at any arity AND at the no-hint query hover uses,
            // so the fallback must stay. "Agree" excludes the lossy
            // Object-subsumes-HashRef dominance — that's a merge, not agreement.
            if arms_genuinely_agree(&known_arm_types).is_none() {
                authoritative_syms.push(sym_id);
            }
            let return_expr = ReturnExpr::UnionOnArgs { branches: sorted };
            // The Symbol attachment carries it for in-file Symbol-
            // keyed lookups. The MethodOnClass attachment mirrors
            // for class-keyed dispatch (cross-file inheritance,
            // `\&Class::method` chase).
            let body_span = arms[0].1;
            to_push.push(Witness {
                attachment: WitnessAttachment::Symbol(sym_id),
                source: WitnessSource::Builder("arity_detection".into()),
                payload: WitnessPayload::ReturnExpr(return_expr.clone()),
                span: body_span,
            });
            if let Some(sym) = self.symbols.get(sym_id.0 as usize) {
                if let Some(class) = sym.package.clone() {
                    to_push.push(Witness {
                        attachment: WitnessAttachment::MethodOnClass {
                            class,
                            name: sym.name.clone(),
                        },
                        source: WitnessSource::Builder("arity_detection".into()),
                        payload: WitnessPayload::ReturnExpr(return_expr),
                        span: body_span,
                    });
                }
            }
        }
        for sym_id in authoritative_syms {
            self.bag.remove_attachment_source(
                &WitnessAttachment::Symbol(sym_id),
                "return_arm_chain",
            );
        }
        for w in to_push {
            self.bag.push(w);
        }
    }

    /// The seed pass — pure read. For every Sub/Method scope, query
    /// `Symbol(sub_id)` through the registry; the registry
    /// materializes through whatever edge chain the bag carries
    /// (`Edge(SymbolReturnArm(_))` for explicit returns,
    /// `Edge(Expr(last_expr_span))` for implicit returns —
    /// pushed by `populate_witness_bag`,
    /// `ReturnExpr(UnionOnArgs{..})` for arity-discriminated subs,
    /// `Plugin + InferredType` for plugin overrides). No bag
    /// writes — the seed pass only builds the name-keyed
    /// `return_types` map for downstream consumers (call-binding
    /// propagation, hash-key fixup) and the per-sym provenance
    /// entries.
    ///
    /// Provenance: records `ReducerFold { reducer: "return_arms" }`
    /// unless `self.type_provenance` already carries a stronger entry
    /// (`PluginOverride`, `Delegation`) — those are written upstream
    /// (`apply_type_overrides`, synthesis sites) and must survive the
    /// seed pass, otherwise `--dump-package` would lose the
    /// "why did this come from a plugin?" story.
    pub(super) fn seed_return_types_from_bag(
        &mut self,
        reg: &crate::model::witnesses::ReducerRegistry,
        method_sym_by_name: &std::collections::HashMap<String, Vec<usize>>,
    ) -> (
        std::collections::HashMap<String, InferredType>,
        std::collections::HashMap<String, crate::model::file_analysis::TypeProvenance>,
    ) {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, WitnessAttachment,
        };
        let mut return_types: std::collections::HashMap<String, InferredType> =
            std::collections::HashMap::new();
        let mut return_provenance: std::collections::HashMap<
            String,
            crate::model::file_analysis::TypeProvenance,
        > = std::collections::HashMap::new();

        let ctx = self.bag_context();

        let mut updates: Vec<(SymbolId, String, InferredType)> = Vec::new();

        for scope in &self.scopes {
            let sub_name = match &scope.kind {
                ScopeKind::Sub { name } | ScopeKind::Method { name } => name.clone(),
                _ => continue,
            };

            let sub_sym_id = method_sym_by_name
                .get(&sub_name)
                .and_then(|cands| {
                    cands.iter().map(|&i| &self.symbols[i]).find(|s| {
                        s.span.start <= scope.span.start && scope.span.end <= s.span.end
                    })
                })
                .map(|s| s.id);

            // Single source: registry query on `Symbol(sub_id)`.
            // Materialization handles every edge:
            //   - `Edge(SymbolReturnArm(_))` for explicit returns
            //   - `Edge(Expr(last_expr_span))` for implicit returns
            //     (pushed by `populate_witness_bag`)
            //   - `ReturnExpr(UnionOnArgs{...})` for framework /
            //     arity-discriminated subs
            //   - `Plugin + InferredType` for plugin overrides
            let resolved = sub_sym_id.and_then(|id| {
                let att = WitnessAttachment::Symbol(id);
                let q = ReducerQuery {
                    attachment: &att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: Some(&ctx),
                };
                match reg.query(&self.bag, &q) {
                    ReducedValue::Type(t) => Some(t),
                    ReducedValue::FactMap(_) | ReducedValue::None => None,
                }
            });

            if let (Some(rt), Some(sid)) = (resolved, sub_sym_id) {
                updates.push((sid, sub_name, rt));
            }
        }

        for (sid, sub_name, rt) in updates {
            return_types.insert(sub_name.clone(), rt);
            // Don't overwrite a stronger upstream provenance —
            // `PluginOverride` (from `apply_type_overrides`) and
            // `Delegation` (from synthesis sites) record *why* a
            // type came in by a path the reducer fold doesn't see.
            // The bag's answer is the same value, but the
            // provenance story would lose its source if we
            // clobbered it with `ReducerFold { return_arms }`.
            let preserve = matches!(
                self.type_provenance.get(&sid),
                Some(
                    crate::model::file_analysis::TypeProvenance::PluginOverride { .. }
                    | crate::model::file_analysis::TypeProvenance::Delegation { .. }
                )
            );
            if !preserve {
                // Tag an optional return so `--dump-package` explains the
                // `{T, undef}` join that produced it.
                let mut evidence = vec!["symbol_bag".to_string()];
                if matches!(
                    return_types.get(&sub_name),
                    Some(InferredType::Optional(_))
                ) {
                    evidence.push("optional_join".into());
                }
                return_provenance.insert(
                    sub_name,
                    crate::model::file_analysis::TypeProvenance::ReducerFold {
                        reducer: "return_arms".into(),
                        evidence,
                    },
                );
            }
        }
        (return_types, return_provenance)
    }

    /// Step 7: writeback. Mirror per-sym answers onto
    /// `MethodOnClass{class, name}` for the primary sym of each
    /// `(class, name)` pair, plus plugin-namespace bridges and
    /// inheritance edges. The primary mirror is published as
    /// `Edge(Symbol(sid))` — pure edge, no value duplication. The
    /// registry's materialization routes class-keyed queries through
    /// to the sym's own bag answer (UnionOnArgs, plugin override,
    /// arm fold, implicit-return edge), so any shape the sym carries
    /// surfaces uniformly. ReturnExpr declarations on
    /// `MethodOnClass{class, name}` (from
    /// `publish_class_accessor_union`) claim first and answer
    /// arity-aware queries; the writeback's Edge fills the
    /// no-arity-hint and Edge-fallback slots.
    ///
    /// Cross-file imports do not get a writeback push here — they
    /// resolve lazily via `query_sub_return_type` walking
    /// `module_index.find_exporters` and recursing into the cached
    /// module's `Symbol(cached_sid)`. Same registry, same rules; no
    /// local mirror needed.
    ///
    /// Plugin overrides surface through the seed pass's registry
    /// query (PluginOverrideReducer is registered first);
    /// `seed_return_types_from_bag` preserves the existing
    /// `PluginOverride` entry in `self.type_provenance` so the
    /// plugin source story survives `--dump-package`.
    ///
    /// Idempotent across re-runs: `bag.remove_by_source_tag("local_return")`
    /// (and `"plugin_bridge"` / `"inheritance"`) at the start of every
    /// call drops the prior pass's witnesses before re-emitting. The
    /// worklist driver calls `resolve_return_types` repeatedly until
    /// fixed point; without this clear-and-emit, each iteration would
    /// duplicate every sub's writeback witnesses.
    pub(super) fn write_back_sub_return_types(
        &mut self,
        return_provenance: &std::collections::HashMap<
            String,
            crate::model::file_analysis::TypeProvenance,
        >,
    ) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

        self.bag.remove_by_source_tag("local_return");
        self.bag.remove_by_source_tag("plugin_bridge");
        self.bag.remove_by_source_tag("inheritance");

        // The bag is canonical — walk-time synthesis pushes
        // `Symbol(sid)` witnesses directly (Plugin overrides, plugin
        // synth, return arm chains, implicit-return edges from
        // `populate_witness_bag`, ReturnExpr arity unions). The seed
        // pass's job is purely to read the registry's answer per sym
        // and surface it in the name-keyed `return_types` map for
        // downstream consumers (call-binding propagation, hash-key
        // fixup). Writeback (below) mirrors per-sym answers onto
        // `MethodOnClass` via edges.
        for sym in &self.symbols {
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                continue;
            }
            if let Some(prov) = return_provenance.get(&sym.name) {
                self.type_provenance.insert(sym.id, prov.clone());
            }
        }
        // Publish `MethodOnClass{class, name} → Edge(Symbol(sid))`
        // for the primary sym of each (class, name) pair. The edge
        // routes class-keyed queries to the sym's own bag answer
        // (UnionOnArgs, plugin override, arm fold, implicit-return
        // edge — whichever the sym carries); the registry's
        // materialization handles every shape uniformly. No value
        // copying — the edge IS the mirror.
        //
        // Primary-dedup: the FIRST sym for `(class, name)` claims
        // the slot. Secondary syms (Mojo getter+writer sharing
        // `(class, name)`) participate via the cross-symbol
        // `ReturnExpr::UnionOnArgs` declaration pushed by
        // `publish_class_accessor_union` on the same attachment.
        // ReturnExprReducer runs first; the Edge below is a no-op
        // when UnionOnArgs answers and a useful fallback otherwise.
        // Walking every sym (not just ones with answers) is what
        // keeps the primary slot stable: a Mojo getter without a
        // default still occupies primary for `(class, name)` so
        // the writer's `ClassName(C)` doesn't surface as the
        // no-arity-hint default.
        let mut writeback_witnesses: Vec<Witness> = Vec::new();
        let mut method_on_class_seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for sym in &self.symbols {
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                continue;
            }
            let Some(class) = sym.package.clone() else { continue };
            let is_primary = method_on_class_seen.insert((class.clone(), sym.name.clone()));
            if is_primary {
                writeback_witnesses.push(Witness {
                    attachment: WitnessAttachment::MethodOnClass {
                        class,
                        name: sym.name.clone(),
                    },
                    source: WitnessSource::Builder("local_return".into()),
                    payload: WitnessPayload::Edge(WitnessAttachment::Symbol(sym.id)),
                    span: sym.span,
                });
            }
        }
        // Plugin-namespace bridges: each `PluginNamespace` declares
        // `bridges: [Class(C1), Class(C2), ...]` for entities that
        // should be reachable from those classes' method dispatch.
        // Emit `MethodOnClass{C_i, entity.name} → Edge(Symbol(entity.id))`
        // edges so `find_method_return_type` resolves bridged
        // entities through the same bag path it uses for direct
        // class methods. Without these edges,
        // `find_method_return_type("Mojolicious", "admin", _, _)`
        // would miss helpers emitted on Mojolicious::Controller and
        // bridged into the Mojolicious lookup namespace.
        let zero = Span {
            start: Point { row: 0, column: 0 },
            end: Point { row: 0, column: 0 },
        };
        for ns in &self.plugin_namespaces {
            for b in &ns.bridges {
                let crate::model::file_analysis::Bridge::Class(class) = b;
                for sym_id in &ns.entities {
                    let Some(sym) = self.symbols.get(sym_id.0 as usize) else { continue };
                    if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                        continue;
                    }
                    writeback_witnesses.push(Witness {
                        attachment: WitnessAttachment::MethodOnClass {
                            class: class.clone(),
                            name: sym.name.clone(),
                        },
                        source: WitnessSource::Builder("plugin_bridge".into()),
                        payload: WitnessPayload::Edge(WitnessAttachment::Symbol(*sym_id)),
                        span: zero,
                    });
                }
            }
        }
        // Inheritance edges: for every (child, parent) entry in
        // `package_parents`, emit `MethodOnClass(child, m) →
        // Edge(MethodOnClass(parent, m))` for each method `m` known
        // on the parent (locally-declared or framework-synthesized).
        // The registry's edge-chase walks these the same way it
        // walks any other Edge — no procedural ancestor walker
        // needed in `query_rec`. Cross-file parents inherit via
        // the registry's existing cached-bag recursion in
        // `query_rec`'s `MethodOnClass` arm: an edge into
        // `MethodOnClass(P_cross, m)` re-enters `query` with the
        // same attachment shape, and the shared visited set closes
        // any mutual loops.
        //
        // Methods on the parent are enumerated locally — for parents
        // that are themselves cross-file the recursion delivers
        // their methods via the cached bag. Each local method-name
        // emission is enough: when child's `MethodOnClass(C, m)`
        // bag has no local witness, the inheritance Edge points at
        // `MethodOnClass(P, m)` and the registry follows it.
        // Sorted by child: `package_parents` is a `HashMap`, so iterating it
        // raw lands these witnesses in hash order — which differs between two
        // builds of the same file, making the bag (and the cache blob minted
        // from it) nondeterministic. Parent order within a child is `@ISA`
        // order and is left alone; that one IS semantic.
        let mut parents_snapshot: Vec<(String, Vec<String>)> = self
            .package_parents
            .iter()
            .map(|(c, ps)| (c.clone(), ps.clone()))
            .collect();
        parents_snapshot.sort_by(|a, b| a.0.cmp(&b.0));
        for (child, parents) in &parents_snapshot {
            // Per-child: track which methods already got an
            // inheritance edge, so the FIRST parent in `@ISA` order
            // wins (Perl's default DFS-MRO is left-to-right). Without
            // this dedup, two parents both defining `m` would push
            // two edges on `MethodOnClass(child, m)` and the
            // materializer's latest-wins reducer would silently pick
            // the second-emitted parent.
            let mut emitted_for_child: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for parent in parents {
                if parent == child {
                    continue;
                }
                for sym in &self.symbols {
                    if sym.package.as_deref() != Some(parent.as_str()) {
                        continue;
                    }
                    if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                        continue;
                    }
                    if !emitted_for_child.insert(sym.name.clone()) {
                        continue;
                    }
                    writeback_witnesses.push(Witness {
                        attachment: WitnessAttachment::MethodOnClass {
                            class: child.clone(),
                            name: sym.name.clone(),
                        },
                        source: WitnessSource::Builder("inheritance".into()),
                        payload: WitnessPayload::Edge(WitnessAttachment::MethodOnClass {
                            class: parent.clone(),
                            name: sym.name.clone(),
                        }),
                        span: zero,
                    });
                }
            }
        }
        for w in writeback_witnesses {
            self.bag.push(w);
        }
    }

    /// `CallBindingPropagator` — not a witness reducer, just the
    /// bag-and-TC sync pass that runs after the fold. For each
    /// `my $cfg = get_config()` binding recorded
    /// during the walk, push BOTH the legacy `TypeConstraint` and the
    /// corresponding `Variable` witness so any later bag query about
    /// `$cfg` sees the call-resolved type without a separate sync pass.
    /// Inline expression propagation (`get_config()->{key}` without an
    /// intermediate variable) is a separate code path — not handled here.
    ///
    /// Idempotent across re-runs — each binding's witness is REPLACED
    /// (targeted remove at its own attachment+point, then re-push) when
    /// the callee's return type is currently known, so refinement flows
    /// through without duplication. A binding whose callee answer is
    /// currently unknown KEEPS its previously-published witness: in a
    /// recursive cluster (`my $t = FetchObject(...); return $t;` —
    /// Config::Universal, File::stat::Extra) the published witness is
    /// itself what resolves the recursive return arm, and the arms then
    /// DISAGREE → the sub's answer drops to None → a wholesale
    /// clear-and-emit would retract the witness → the arm un-resolves →
    /// the answer comes back → period-2 oscillation, and the fold rides
    /// the 64-iteration bail every build. Never retracting is the
    /// monotone repair: the cluster settles (deterministically) instead
    /// of flipping.
    pub(super) fn propagate_call_bindings_to_constraints(
        &mut self,
        return_types: &std::collections::HashMap<String, InferredType>,
    ) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

        for binding in &self.call_bindings {
            let rt = return_types
                .get(&binding.func_name)
                .cloned()
                .or_else(|| crate::model::builtins::builtin_return_type(&binding.func_name));
            if let Some(rt) = rt {
                let att = WitnessAttachment::Variable {
                    name: binding.variable.clone(),
                    scope: binding.scope,
                };
                self.bag
                    .remove_attachment_source_at(&att, "call_binding", binding.span.start);
                self.bag.push(Witness {
                    attachment: att,
                    source: WitnessSource::Builder("call_binding".into()),
                    payload: WitnessPayload::InferredType(rt),
                    span: Span {
                        start: binding.span.start,
                        end: binding.span.start,
                    },
                });
            }
        }
    }

    /// Step 9: hash-key-owner fixup for variables bound to sub calls
    /// that return HashRef. Two normalizations beyond the naive name
    /// match:
    ///   1. Call names may be qualified (`Pkg::foo`) — strip the
    ///      package prefix since `return_types` and the symbol table
    ///      key on the bare name.
    ///   2. The bound func may itself just `return other()` — walk
    ///      the delegation chain to the sub that actually declares
    ///      the hash literal. Otherwise `sub chain { return
    ///      get_config() }` leaves `$cfg = chain(); $cfg->{host}`
    ///      with an owner that has no matching HashKeyDefs.
    pub(super) fn fixup_call_bound_hash_key_owners(
        &mut self,
        return_types: &std::collections::HashMap<String, InferredType>,
    ) {
        let binding_map: std::collections::HashMap<&str, String> = self
            .call_bindings
            .iter()
            .filter(|b| {
                return_types
                    .get(bare_name(&b.func_name))
                    .map_or(false, |t| t.is_hash_shaped())
            })
            .map(|b| (b.variable.as_str(), bare_name(&b.func_name).to_string()))
            .collect();

        let sub_package: std::collections::HashMap<&str, Option<String>> = self
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
            .map(|s| (s.name.as_str(), s.package.clone()))
            .collect();

        let subs_with_own_keys: std::collections::HashSet<String> = self
            .symbols
            .iter()
            .filter_map(|s| {
                if let SymbolDetail::HashKeyDef {
                    owner: HashKeyOwner::Sub { name, .. },
                    ..
                } = &s.detail
                {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();

        // Method-call bindings: `my $c = $obj->method()` (including
        // dynamic `$obj->$m()` where $m was constant-folded during
        // method_call_binding emission). Same ownership logic as
        // function calls — point $c's hash-key accesses at the
        // HashKeyDefs inside `method` — but the package keys on
        // {invocant class, method}: the invocant's bag-resolved class
        // (resolved here, outside the `&mut self.refs` loop below) names
        // the owner when it defines the sub, so a same-named method on
        // an unrelated package can't claim ownership. The MRO walker is
        // FileAnalysis machinery; the residuals (inherited definer,
        // untyped invocant) keep the name-only fallback, and the
        // query-time owner path (`resolve_hash_key_owner`) applies the
        // full ladder.
        let method_binding_map: std::collections::HashMap<&str, (String, Option<String>)> = self
            .method_call_bindings
            .iter()
            .map(|mcb| {
                let class = self
                    .bag_query_variable(&mcb.invocant_var, mcb.scope, mcb.span.start)
                    .and_then(|t| t.class_name().map(str::to_string));
                (mcb.variable.as_str(), (mcb.method_name.clone(), class))
            })
            .collect();

        for r in &mut self.refs {
            if let RefKind::HashKeyAccess { ref var_text } = r.kind {
                let new_owner = if let Some(func_name) = binding_map.get(var_text.as_str()) {
                    let resolved = walk_return_delegation_chain(
                        func_name,
                        &self.sub_return_delegations,
                        &subs_with_own_keys,
                    );
                    Some(HashKeyOwner::Sub {
                        package: sub_package.get(resolved.as_str()).cloned().unwrap_or(None),
                        name: resolved,
                    })
                } else if let Some((method_name, invocant_class)) =
                    method_binding_map.get(var_text.as_str())
                {
                    let resolved = walk_return_delegation_chain(
                        method_name,
                        &self.sub_return_delegations,
                        &subs_with_own_keys,
                    );
                    subs_with_own_keys.contains(&resolved).then(|| {
                        let class_defines = invocant_class.as_deref().is_some_and(|cn| {
                            self.symbols.iter().any(|s| {
                                matches!(s.kind, SymKind::Sub | SymKind::Method)
                                    && s.name == resolved
                                    && s.package.as_deref() == Some(cn)
                            })
                        });
                        let package = if class_defines {
                            invocant_class.clone()
                        } else {
                            sub_package.get(resolved.as_str()).cloned().unwrap_or(None)
                        };
                        HashKeyOwner::Sub { package, name: resolved }
                    })
                } else {
                    None
                };
                if let Some(o) = new_owner {
                    r.bind_hash_key_owner(o);
                }
            }
        }
    }

    /// Apply every `TypeOverride` in the registry to local Sub/Method
    /// symbols. Plugin-asserted return types win over inference; the
    /// override carries a `reason` that we record in
    /// `type_provenance` so a debugger can later answer "why does the
    /// LSP think `_route` returns `Mojolicious::Routes::Route`?"
    /// without re-running the build.
    ///
    /// Targets are matched on (name, package). Methods require an
    /// exact package match — overrides describe the home class, not
    /// the inheritance chain (a base class's override wins for that
    /// base's symbol; subclasses get it via the existing cross-file
    /// resolution path).
    ///
    /// Mechanism: pushes a Plugin-source `InferredType` witness onto
    /// `Symbol(sym_id)`. The `PluginOverrideReducer` priority
    /// short-circuit (witnesses.rs) makes that witness dominate any
    /// inferred Symbol+InferredType evidence in the same fold. Direct
    /// writes to `Symbol.return_type` happen later in
    /// `resolve_return_types`, sourced from the bag — this keeps the
    /// override flow uniform with the rest of the type-inference
    /// pipeline (no parallel "pinned by override" path).
    pub(super) fn apply_type_overrides(&mut self) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

        // Snapshot first — can't borrow self.plugins while mutating
        // self.bag + self.type_provenance below.
        let pairs: Vec<(String, plugin::TypeOverride)> = self.plugins
            .overrides()
            .map(|(id, o)| (id.to_string(), o.clone()))
            .collect();
        if pairs.is_empty() {
            return;
        }
        let zero = Span {
            start: Point { row: 0, column: 0 },
            end: Point { row: 0, column: 0 },
        };
        for (plugin_id, ov) in pairs {
            // A `Method` override is a framework fact about a class —
            // "method `name` on `class` returns T" — independent of
            // whether that class is defined locally. Publish it on the
            // class-keyed attachment so a method call resolves through
            // `MethodOnClassReducer` even when the home class (e.g.
            // `Mojolicious::Routes::Route`) lives in external @INC and
            // isn't indexed: the declarative type IS the answer, no
            // local symbol required. This is what lets a route-builder
            // chain (`$r->any(...)->to(...)`) keep its receiver typed —
            // and therefore brand — without a vendored Mojolicious.
            // Sub overrides stay symbol-keyed (they name a package
            // function, not a class method).
            if let plugin::OverrideTarget::Method { class, name } = &ov.target {
                self.bag.push(Witness {
                    attachment: WitnessAttachment::MethodOnClass {
                        class: class.clone(),
                        name: name.clone(),
                    },
                    source: WitnessSource::Plugin(plugin_id.clone()),
                    payload: WitnessPayload::InferredType(ov.return_type.clone()),
                    span: zero,
                });
            }
            // Collect target SymbolIds in a snapshot so we can mutate
            // self.bag + self.type_provenance below without holding
            // an aliasing borrow on self.symbols.
            let mut targets: Vec<SymbolId> = Vec::new();
            for sym in &self.symbols {
                if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                let target_matches = match &ov.target {
                    plugin::OverrideTarget::Method { class, name } => {
                        sym.name == *name && sym.package.as_deref() == Some(class.as_str())
                    }
                    plugin::OverrideTarget::Sub { package, name } => {
                        sym.name == *name && sym.package == *package
                    }
                };
                if !target_matches { continue; }
                if matches!(sym.detail, SymbolDetail::Sub { .. }) {
                    targets.push(sym.id);
                }
            }
            for sym_id in targets {
                // Zero-extent span — core-synthesized witness, no
                // user-visible "because: …" anchor needed beyond the
                // provenance entry below.
                self.bag.push(Witness {
                    attachment: WitnessAttachment::Symbol(sym_id),
                    source: WitnessSource::Plugin(plugin_id.clone()),
                    payload: WitnessPayload::InferredType(ov.return_type.clone()),
                    span: zero,
                });
                self.type_provenance.insert(
                    sym_id,
                    TypeProvenance::PluginOverride {
                        plugin_id: plugin_id.clone(),
                        reason: ov.reason.clone(),
                    },
                );
            }
        }
    }

    pub(super) fn resolve_hash_key_owners(&mut self) {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload};
        // Build type constraint lookup from the bag — Variable
        // witnesses with `InferredType` payloads are the seed-time
        // type-constraint shape (`push_type_constraint` mirrors every
        // TC into one of these). The bag is canonical at this phase.
        let mut type_map: std::collections::HashMap<String, Vec<(ScopeId, InferredType, Point)>> =
            std::collections::HashMap::new();
        for w in self.bag.all() {
            if let (
                WitnessAttachment::Variable { name, scope },
                WitnessPayload::InferredType(t),
            ) = (&w.attachment, &w.payload)
            {
                type_map
                    .entry(name.clone())
                    .or_default()
                    .push((*scope, t.clone(), w.span.start));
            }
        }

        // Build variable def lookup
        let mut var_defs: std::collections::HashMap<String, Vec<(ScopeId, SymbolId)>> =
            std::collections::HashMap::new();
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Variable | SymKind::Field) {
                var_defs
                    .entry(sym.name.clone())
                    .or_default()
                    .push((sym.scope, sym.id));
            }
        }

        for r in &mut self.refs {
            if let RefKind::HashKeyAccess { ref var_text } = r.kind {
                if r.hash_key_owner().is_some() { continue; }

                let vt = var_text.clone();
                // Canonicalize: $hash → %hash for lookup
                let lookup_name = if vt.starts_with('$') {
                    format!("%{}", &vt[1..])
                } else {
                    vt.clone()
                };

                // Try type constraints first
                if let Some(constraints) = type_map.get(&vt).or(type_map.get(&lookup_name)) {
                    // Find best constraint: in scope chain and before ref
                    let mut scope = Some(r.scope);
                    'outer: while let Some(sid) = scope {
                        for (tc_scope, tc_type, tc_point) in constraints {
                            if *tc_scope == sid && *tc_point <= r.span.start {
                                // Hash-key owner: read
                                // `hash_key_class()` so a Parametric
                                // TC narrows to its row-class arg.
                                // For non-Parametric this is the
                                // dispatch class. CLAUDE.md #10.
                                if let Some(cn) = tc_type.hash_key_class() {
                                    r.bind_hash_key_owner(HashKeyOwner::Class(cn.to_string()));
                                    break 'outer;
                                }
                            }
                        }
                        scope = self.scopes[sid.0 as usize].parent;
                    }
                    if r.hash_key_owner().is_some() { continue; }
                }

                // Fall back to variable identity
                if let Some(defs) = var_defs.get(&vt).or(var_defs.get(&lookup_name)) {
                    // Find the innermost declaration before this ref
                    let mut scope = Some(r.scope);
                    while let Some(sid) = scope {
                        if let Some((def_scope, _sym_id)) = defs.iter().find(|(s, _)| *s == sid) {
                            r.bind_hash_key_owner(HashKeyOwner::Variable {
                                name: vt.clone(),
                                def_scope: *def_scope,
                            });
                            break;
                        }
                        scope = self.scopes[sid.0 as usize].parent;
                    }
                }

                // MEASUREMENT: a ref that reaches here with no owner is the
                // "unowned" class — 17.4% of hash-key refs on CPAN, 54.7% on
                // Koha. Classify WHY, to separate a genuine gap (the fact was
                // available and the scope/point test missed it) from a design
                // (nothing in this file could have attributed it).
                if crate::util::ghost_stats::enabled() && r.hash_key_owner().is_none() {
                    let known_tc = type_map.contains_key(&vt) || type_map.contains_key(&lookup_name);
                    let known_def = var_defs.contains_key(&vt) || var_defs.contains_key(&lookup_name);
                    crate::util::ghost_stats::count("unowned.total");
                    if crate::model::conventions::is_conventional_invocant_name(&vt) {
                        crate::util::ghost_stats::count("unowned.self_like");
                    }
                    if !vt.starts_with(['$', '@', '%']) {
                        crate::util::ghost_stats::count("unowned.not_a_variable");
                        if vt.is_empty() {
                            crate::util::ghost_stats::count("unowned.notvar_empty");
                        } else {
                            crate::util::ghost_stats::count_distinct("unowned.notvar_shape", &vt);
                            crate::util::ghost_stats::count("unowned.notvar_shape");
                        }
                    }
                    match (known_tc, known_def) {
                        // The name IS known here; only the scope-chain / point
                        // test rejected it. That is the gap-shaped bucket.
                        (true, _) => crate::util::ghost_stats::count("unowned.tc_known_scope_missed"),
                        (false, true) => {
                            crate::util::ghost_stats::count("unowned.def_known_scope_missed")
                        }
                        // Nothing in this file mentions the name as a typed
                        // value or a declaration — genuinely unattributable.
                        (false, false) => crate::util::ghost_stats::count("unowned.name_unknown"),
                    }
                }
            }
        }
    }
}
