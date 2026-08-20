//! Scope/variable/type query methods: scope walks, bag-routed type lookups,
//! expression typing, member-op diagnostics.

use super::*;

impl FileAnalysis {
    // ---- Query methods ----

    /// Find the innermost scope containing a point.
    pub fn scope_at(&self, point: Point) -> Option<ScopeId> {
        let mut best: Option<(ScopeId, usize)> = None; // (id, span_size)
        for scope in &self.scopes {
            if contains_point(&scope.span, point) {
                let size = span_size(&scope.span);
                if best.is_none() || size <= best.unwrap().1 {
                    best = Some((scope.id, size));
                }
            }
        }
        best.map(|(id, _)| id)
    }

    /// Walk the scope chain from a scope upward to file root.
    pub fn scope_chain(&self, start: ScopeId) -> Vec<ScopeId> {
        scope_chain_of(&self.scopes, start)
    }

    /// Get the scope struct by ID.
    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    /// Get the symbol struct by ID.
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }

    /// Find all symbols visible at a point (walks scope chain).
    pub fn visible_symbols(&self, point: Point) -> Vec<&Symbol> {
        let scope = match self.scope_at(point) {
            Some(s) => s,
            None => return Vec::new(),
        };
        let chain = self.scope_chain(scope);
        let mut result = Vec::new();
        for scope_id in &chain {
            for sid in self.symbols.in_scope(*scope_id) {
                let sym = &self.symbols[sid.0 as usize];
                // Symbol must be declared before the point (or be a sub/package/class)
                if sym.span.start <= point || matches!(sym.kind, SymKind::Sub | SymKind::Method | SymKind::Package | SymKind::Class) {
                    result.push(sym);
                }
            }
        }
        result
    }

    /// Resolve a variable name to its declaration at a given point.
    /// Returns the innermost-scope match (Perl lexical scoping).
    pub fn resolve_variable(&self, name: &str, point: Point) -> Option<&Symbol> {
        let scope = self.scope_at(point)?;
        let chain = self.scope_chain(scope);
        for scope_id in &chain {
            for sid in self.symbols.in_scope(*scope_id) {
                let sym = &self.symbols[sid.0 as usize];
                if sym.name == name
                    && matches!(sym.kind, SymKind::Variable | SymKind::Field)
                    && sym.span.start <= point
                {
                    return Some(sym);
                }
            }
        }
        None
    }

    /// The pointer/reference declarator stack of the in-scope variable
    /// `name` at `point` — the receiver shape the member-access operator-DX
    /// reads. `Some([])` is a known value (expects `.`); `None` means no
    /// such variable resolved (don't offer a correction).
    pub fn var_deref_stack_at(&self, name: &str, point: Point) -> Option<&[DerefStep]> {
        self.resolve_variable(name, point).map(|s| s.deref_stack.as_slice())
    }

    /// Each READ of a variable that occurs after a `std::move` of it and
    /// before it is reassigned — a use-after-move bug. The moved-from region
    /// runs from the move call to the earliest FlowEdge rebinding the var (the
    /// SAME edge-driven cutoff narrowing uses, `earliest_rebind_in`), or the
    /// enclosing scope's end. Reads are matched on name + access AND filtered
    /// to the move's scope subtree, so a same-named var in another function
    /// never false-flags. Returns (var name, read span).
    ///
    /// The check is deliberately the DECIDABLE subset: it flags only a
    /// straight-line moved-then-used LOCAL. Three conservative gates keep it
    /// honest (false positives are worse than false negatives for a
    /// diagnostic — when unsure, stay silent); each is verified to zero the
    /// FPs on real library headers (spdlog/fmt), see
    /// `docs/adr/use-after-move.md`:
    ///  - GATE B (in-function): the move must be lexically inside a function
    ///    body. A member-initializer / delegating-ctor init-list move lands in
    ///    the class/namespace scope, not the ctor body, and can't be bounded.
    ///  - GATE C (straight-line): a move nested — relative to its scope —
    ///    inside a conditional / loop / switch / ternary / preproc branch is
    ///    not straight-line; proving the read is reached only after the move
    ///    needs path-sensitivity this tier lacks.
    ///  - GATE E (locals only): a moved PARAMETER is a forwarding /
    ///    subobject-move idiom, not flagged.
    /// What stays out: any use that needs true path-sensitivity (a use in a
    /// different branch arm, a loop-carried move, a reset via a by-mutable-ref
    /// call), and non-local / subobject moves.
    pub fn use_after_move_reads(&self) -> Vec<(String, Span)> {
        let key = |p: &Point| (p.row, p.column);
        let contains = |outer: &Span, inner: &Span| {
            key(&outer.start) <= key(&inner.start) && key(&inner.end) <= key(&outer.end)
        };
        let mut out = Vec::new();
        for (name, move_span, move_scope) in &self.pack.moved_from {
            let scope_span = self.scope(*move_scope).span;
            // GATE B (in-function): only a move that is lexically inside SOME
            // function body is executable straight-line code we can reason
            // about. A move whose scope chain has no Sub/Method — a member-
            // initializer / delegating-ctor init list lands in the class or
            // namespace scope, NOT the ctor body — can't be bounded, so it is
            // never flagged. This is what silences the move-constructor floods
            // on real headers (spdlog/fmt), where an init-list move shares a
            // broad scope with every same-named param and member.
            if !scope_chain_of(&self.scopes, *move_scope).iter().any(|s| {
                matches!(
                    self.scope(*s).kind,
                    ScopeKind::Sub { .. } | ScopeKind::Method { .. }
                )
            }) {
                continue;
            }
            // GATE C (straight-line): a move nested — relative to its enclosing
            // scope — inside a conditional / loop / switch / ternary / preproc
            // branch is not straight-line: the read may be reachable without the
            // move, or the move may run every loop iteration, and proving
            // otherwise needs path-sensitivity this tier doesn't have. Braced
            // if/else arms are their OWN scope, so their region starts BEFORE
            // the arm and is not `contains`ed by it — those stay flaggable
            // (a same-arm read is a real bug); only the non-scope constructs
            // (braceless arms, loop/switch bodies, ternary, preproc) gate here.
            let straight_line = !self.pack.control_regions.iter().any(|g| {
                contains(&scope_span, g) && contains(g, move_span)
            });
            if !straight_line {
                continue;
            }
            // GATE E (locals only): a moved PARAMETER is a forwarding /
            // subobject-move idiom — move-constructors and `operator=` move
            // the rvalue-ref param into a base/member subobject and then read
            // sibling members, which this tier can't tell from a real bug
            // without subobject + path analysis. Detect param-ness by the moved
            // var's declaration landing inside a parameter list; only moves of
            // LOCALS are flagged. This is what clears the last real-header FPs.
            let chain = scope_chain_of(&self.scopes, *move_scope);
            let moved_is_param = self.symbols.iter().any(|s| {
                s.name == *name
                    && chain.contains(&s.scope)
                    && self
                        .pack.param_regions
                        .iter()
                        .any(|pr| contains(pr, &s.selection_span))
            });
            if moved_is_param {
                continue;
            }
            let scope_end = scope_span.end;
            let region = Span { start: move_span.end, end: scope_end };
            let cutoff = earliest_rebind_in(&self.flow_edges, name, region).unwrap_or(scope_end);
            for r in self.refs() {
                if r.target_name != *name || r.access != AccessKind::Read {
                    continue;
                }
                let p = r.span.start;
                // strictly after the move, strictly before the rebind cutoff
                if key(&p) <= key(&move_span.end) || key(&p) >= key(&cutoff) {
                    continue;
                }
                // same variable: the read must sit inside the move's scope
                // subtree (the move scope is on the read's scope chain).
                if scope_chain_of(&self.scopes, r.scope).contains(move_scope) {
                    out.push((name.clone(), r.span));
                }
            }
        }
        out
    }

    /// Member accesses whose typed operator disagrees with their
    /// receiver's pointer depth — the single-level `.`↔`->` mismatches with a
    /// token-swap fix. DEEP receivers (`Box**`) fall to the peel partition
    /// (`member_op_deep_accesses`), not here.
    pub fn member_op_mismatches(&self) -> Vec<MemberOpMismatch> {
        let mut out = Vec::new();
        self.for_each_member_access(|_recv, typed, op_span, stack| {
            let Some(expected) = expected_member_op(stack) else {
                return; // DEEP — a wrap, not a swap; `member_op_deep_accesses` owns it
            };
            if typed != expected {
                out.push(MemberOpMismatch { op_span, typed, expected });
            }
        });
        out
    }

    /// Member accesses whose receiver is too deeply indirected for any single
    /// `.`/`->` token — the DEEP partition `member_op_mismatches` skips. Each
    /// carries the peeled receiver spelling (`(*pp)`) for a show-only hint (no
    /// auto-fix: the rewrite is an expression wrap, not a token swap).
    pub fn member_op_deep_accesses(&self) -> Vec<MemberOpPeel> {
        let mut out = Vec::new();
        self.for_each_member_access(|recv, _typed, op_span, stack| {
            if let Some((wrap, depth)) = deref_peel(stack, recv) {
                out.push(MemberOpPeel { op_span, wrap, depth });
            }
        });
        out
    }

    /// Shared walk over member-access refs carrying a `member_op` (a
    /// simple-variable receiver) joined with that receiver's `deref_stack`.
    /// Both op-DX queries project this — one ref scan, one join site.
    fn for_each_member_access(
        &self,
        mut f: impl FnMut(&str, MemberOp, Span, &[DerefStep]),
    ) {
        for r in self.refs() {
            let RefKind::MethodCall {
                invocant,
                invocant_span: Some(span),
                member_op: Some((typed, op_span)),
                ..
            } = &r.kind
            else {
                continue;
            };
            let recv = invocant.text();
            let Some(stack) = self.var_deref_stack_at(recv, span.start) else {
                continue;
            };
            f(recv, *typed, *op_span, stack);
        }
    }

    /// Raw Variable+InferredType lookup — returns the latest in-scope
    /// witness for `var_name` before `point`, with no framework rules,
    /// no branch fold, no narrowing.
    ///
    /// **NOT the canonical type query — test-only** (`#[cfg(test)]`
    /// enforces it; `layering_tests::inferred_type_has_no_production_caller`
    /// documents it). Use `inferred_type_via_bag` for any consumer that
    /// wants the answer the rest of the LSP uses; this exists solely for
    /// tests that assert on raw seed state.
    #[cfg(test)]
    pub fn inferred_type(&self, var_name: &str, point: Point) -> Option<&InferredType> {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload};
        let mut best: Option<(&InferredType, Point)> = None;
        for w in self.witnesses.all() {
            let WitnessAttachment::Variable { name, scope } = &w.attachment else { continue };
            if name != var_name { continue; }
            let scope_obj = &self.scopes[scope.0 as usize];
            if !contains_point(&scope_obj.span, point) { continue; }
            if w.span.start > point { continue; }
            let WitnessPayload::InferredType(t) = &w.payload else { continue };
            if best.is_none() || w.span.start > best.unwrap().1 {
                best = Some((t, w.span.start));
            }
        }
        best.map(|(t, _)| t)
    }

    /// Query the witness bag via the reducer registry for a variable at
    /// a point. Returns owned `InferredType` because the reducer may
    /// synthesize a value not stored anywhere.
    ///
    /// The bag is the canonical store: `push_type_constraint` (TC
    /// shape), `call_bindings` propagation, framework accessor
    /// synthesis, and cross-file enrichment all push witnesses here.
    pub fn inferred_type_via_bag(&self, var_name: &str, point: Point) -> Option<InferredType> {
        self.inferred_type_via_bag_ctx(var_name, point, None)
    }

    /// As `inferred_type_via_bag`, but with a `ModuleIndex` so a variable whose
    /// value is a cross-file method chain (`my $x = Foo->new->bar`) resolves —
    /// the chase keeps the index when it crosses the `Variable` edge instead of
    /// dead-ending. Pass the index from query-time callers (hover/completion);
    /// the bare wrapper keeps `None` for build-time / single-file callers.
    /// The registry-query context over this analysis. Every bag query
    /// threads the same field set; build it here so adding a field is one
    /// edit, not one per call site.
    pub(crate) fn bag_context<'a>(
        &'a self,
        module_index: Option<&'a dyn CrossFileLookup>,
    ) -> crate::model::witnesses::BagContext<'a> {
        crate::model::witnesses::BagContext {
            scopes: &self.scopes,
            package_framework: &self.packages,
            module_index,
            package_parents: &self.packages,
            app_surface_consumers: &self.plugin.app_surface_consumers,
        }
    }

    pub fn inferred_type_via_bag_ctx(
        &self,
        var_name: &str,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        let scope = self.scope_at(point)?;
        if let Some(t) = crate::model::witnesses::query_variable_type(
            &self.witnesses,
            &self.bag_context(module_index),
            var_name,
            scope,
            point,
        ) {
            return Some(t);
        }
        // Role-contract param types are gated on the enclosing package's
        // cross-file ancestry, so they resolve here (index in hand), not in
        // the bag the index-free builder seeded.
        self.gated_param_type_for(var_name, scope, point, module_index)
    }

    /// Resolve a `param_types()` role-contract TC for `var` at `point`: find a
    /// gated TC whose scope is on the chain and whose variable matches, then
    /// read its inner type ONLY if the enclosing package `isa` the rule's
    /// gate (`in_role`), resolved cross-file via `resolve_for` — so a
    /// controller whose `Catalyst::Controller` ancestry runs through a
    /// cross-file base still types its `$c`.
    fn gated_param_type_for(
        &self,
        var: &str,
        scope: ScopeId,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        if self.gated_param_types.is_empty() {
            return None;
        }
        let chain = self.scope_chain(scope);
        let pkg = self.package_at(point);
        for gated in &self.gated_param_types {
            if let GateResult::Applies(tc) =
                gated.resolve_for(pkg, &self.packages, module_index)
            {
                if tc.variable == var && chain.contains(&tc.scope) {
                    return Some(tc.inferred_type.clone());
                }
            }
        }
        None
    }

    /// Resolve the inferred return type of a method call by its ref index
    /// (into `refs`). Reads the `Expression(refidx)` witnesses seeded by
    /// the builder; `module_index` lets cross-file `PackageSymbol` edges
    /// (e.g. `$r->get(...)` where `get` lives in `Mojolicious::Routes`)
    /// chase through the registry's recursive walker.
    ///
    /// This is the piece that makes `$r->get('/x')->to(...)` fold across
    /// chain hops without needing an intermediate variable.
    #[allow(dead_code)] // documented type-query entry point; CLAUDE.md
    pub fn method_call_return_type_via_bag(
        &self,
        ref_idx: usize,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        // Occurs check + stack-scoped memo for the mutual recursion with
        // `expr_type_at_span` (the receiver chase re-enters it). A call
        // ref already on the stack is a cross-file return-type cycle →
        // answer None; one resolved earlier in this outermost query is
        // reused (see `expr_type_at_span` for the exponential rationale).
        let node = ResolveNode::MethodCall(self as *const Self as usize, ref_idx);
        if let Some(hit) = ResolveGuard::memo_get(&node) {
            return hit;
        }
        let Some(_guard) = ResolveGuard::enter(node) else {
            return None;
        };
        let result = self.method_call_return_type_via_bag_uncached(ref_idx, module_index);
        ResolveGuard::memo_put(node, result.clone());
        result
    }

    fn method_call_return_type_via_bag_uncached(
        &self,
        ref_idx: usize,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry,
            WitnessAttachment,
        };

        let att = WitnessAttachment::Expression(crate::model::witnesses::RefIdx(ref_idx as u32));
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context(module_index);
        // Thread the receiver's resolved type so a receiver-relative
        // return (`Operator(RowOf(Receiver))` — DBIC `find`/`search`)
        // projects at query time, exactly as the build-time chain typer
        // threads `q.receiver`. The receiver lives at the call ref's
        // `invocant_span`; resolve it tree-free via `expr_type_at_span`
        // (recurses through inner chain hops). This is what lets a
        // chained-method-return invocant — `$rs->find(1)->name`, where
        // `->name`'s receiver is the `find` call, not a variable — type
        // `find`'s return as the Row class without an intermediate var.
        let own_span = self.refs[ref_idx].span;
        let receiver = if let RefKind::MethodCall { invocant_span: Some(span), .. } =
            &self.refs[ref_idx].kind
        {
            // Only chase a receiver whose span is STRICTLY inside the
            // call's own span — a genuine inner chain hop. Equal-or-wider
            // spans (degenerate overlapping refs route branding can emit)
            // would recurse back onto this same call; skipping them keeps
            // the receiver `None` (build-time chain typing already pinned
            // those via `bag_query_expression`).
            let strictly_inside = (span.start.row, span.start.column)
                >= (own_span.start.row, own_span.start.column)
                && (span.end.row, span.end.column) <= (own_span.end.row, own_span.end.column)
                && *span != own_span;
            if strictly_inside {
                self.expr_type_at_span(*span, module_index)
            } else {
                None
            }
        } else {
            None
        };
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: None,
            receiver: receiver.clone(),
            args: Vec::new(),
            context: Some(&ctx),
        };
        let primary = match reg.query(&self.witnesses, &q) {
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        };
        // Fallback for a chain receiver whose class the BUILDER couldn't
        // pin (so `emit_method_call_return_edges` never emitted the
        // `Expression → Edge(PackageSymbol{package, method})` for this
        // call): resolve the method's return via the receiver's class at
        // QUERY time. This is what lets a receiver-relative projection
        // — `$rs->search({...})->first` (RowOf on the fluent-search
        // result, class only known once the fluent chain resolves) —
        // type the row without an intermediate variable. Only fires when
        // the primary Expression query is empty AND the receiver's class
        // is known, so ordinary calls (whose build edge already answered)
        // are untouched.
        let resolved = primary.or_else(|| {
            let class = receiver.as_ref()?.class_name()?.to_string();
            let method = crate::model::conventions::MethodToken::parse(
                &self.refs[ref_idx].target_name,
            )
            .name()
            .to_string();
            let arity = self.refs[ref_idx].arg_count.map(|c| c as u32);
            let moc = WitnessAttachment::PackageSymbol { package: class, name: method };
            let mq = ReducerQuery {
                attachment: &moc,
                point: None,
                framework: FrameworkFact::Plain,
                arity_hint: arity,
                receiver,
                args: Vec::new(),
                context: Some(&ctx),
            };
            match reg.query(&self.witnesses, &mq) {
                ReducedValue::Type(t) => Some(t),
                ReducedValue::FactMap(_) | ReducedValue::None => None,
            }
        });
        resolved.map(|t| {
            // If the return is FirstParam, surface it as the ClassName of
            // the enclosing package — callers chain against a concrete
            // class, not a role.
            if let InferredType::FirstParam { package } = t {
                InferredType::ClassName(package)
            } else {
                t
            }
        })
    }

    /// Registry query against `Expr(span)` — the bag attachment the
    /// builder seeds at every meaningful expression node (literals,
    /// variable reads, method-call invocants, ternaries). Mirror of the
    /// build-time `Builder::bag_query_expr_span`. `module_index` lets a
    /// recorded `Edge` (e.g. a method-call invocant pointing at an
    /// `Expression(refidx)`) chase cross-file.
    fn bag_query_expr_span(
        &self,
        span: Span,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry,
            WitnessAttachment,
        };
        let att = WitnessAttachment::Expr(span);
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context(module_index);
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: None,
            receiver: None,
            args: Vec::new(),
            context: Some(&ctx),
        };
        match reg.query(&self.witnesses, &q) {
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        }
    }

    /// The type of the expression occupying `span`, resolved tree-free
    /// from the bag. This is the single query-time entry that
    /// `method_call_invocant_class` and `resolve_expression_type` both
    /// route through: structure was discovered once in the builder
    /// (recorded as `Expr(span)` witnesses + the `Expression(refidx)`
    /// call axis), and every consumer reads it back by span.
    ///
    /// Resolution order:
    /// 1. A call ref starting at and contained in `span` (chain /
    ///    function-call receiver) — its bag-resolved return type. This
    ///    arm re-derives at enrichment, so cross-file chain receivers
    ///    whose class only becomes known once other modules load still
    ///    resolve here.
    /// 2. The `Expr(span)` witness the builder recorded for the
    ///    expression (variable reads via `Edge(Variable)`, `$arr[N]`
    ///    projections, ternaries, literals).
    pub fn expr_type_at_span(
        &self,
        span: Span,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        // Occurs check + stack-scoped memo for the `expr_type_at_span` ⇄
        // `method_call_return_type_via_bag` mutual recursion. A span
        // already on the stack is a return-type cycle (A::foo's return
        // depends on B->bar whose return depends on A->foo) → answer
        // None; a span resolved earlier in this same outermost query is
        // reused rather than recomputed (the graph is a dense DAG in mojo,
        // so recomputation is exponential without the memo).
        let node = ResolveNode::Expr(self as *const Self as usize, span);
        if let Some(hit) = ResolveGuard::memo_get(&node) {
            return hit;
        }
        let Some(_guard) = ResolveGuard::enter(node) else {
            return None;
        };
        let result = self.expr_type_at_span_uncached(span, module_index);
        ResolveGuard::memo_put(node, result.clone());
        result
    }

    fn expr_type_at_span_uncached(
        &self,
        span: Span,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        // A call whose span IS this expression — its return type. The
        // exact-span match is what distinguishes "the value of
        // `$f->get_bar()->get_name()`" (the outer call's return) from
        // "the inner receiver `$f->get_bar()`" (which has its own,
        // narrower span). `RefTable::call_at_start` deliberately points at the
        // innermost receiver, so we can't use it here — we want the ref
        // that exactly spans the queried expression.
        if let Some((recv_idx, kind)) = self.refs.iter().enumerate().find_map(|(i, r)| {
            if r.span == span && matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. }) {
                Some((i, &r.kind))
            } else {
                None
            }
        }) {
            match kind {
                RefKind::MethodCall { .. } => {
                    if let Some(t) =
                        self.method_call_return_type_via_bag(recv_idx, module_index)
                    {
                        return Some(t);
                    }
                }
                RefKind::FunctionCall { .. } => {
                    if let Some(t) = self.sub_return_type_at_arity_ctx(
                        &self.refs[recv_idx].target_name,
                        Some(self.refs[recv_idx].arg_count.unwrap_or(0) as u32),
                        module_index,
                    ) {
                        return Some(t);
                    }
                }
                _ => {}
            }
        }
        // Pack member-access chain: the queried span is `recv.member` /
        // `recv.member(...)` — a receiver expression whose value is the
        // member's. A pack member ref spans ONLY the member token
        // (`r.span == method_name_span`, the structural discriminator —
        // a Perl MethodCall ref spans the whole call and never matches),
        // so the exact-span arm above can't see it. Pick the RIGHTMOST
        // member ref whose invocant opens this span and whose token ends
        // inside it (the chain's last hop), type the receiver recursively
        // (strictly narrower span, bounded), and resolve the member's
        // value on it — methods through `PackageSymbol` with the receiver
        // threaded (`ParamOf` substitutes), fields through the declared
        // type with params substituted.
        if let Some((inv, member, arity)) = self
            .refs
            .iter()
            .filter_map(|r| {
                let RefKind::MethodCall {
                    invocant_span: Some(inv), method_name_span, ..
                } = &r.kind
                else {
                    return None;
                };
                if r.span != *method_name_span
                    || inv.start != span.start
                    || (method_name_span.end.row, method_name_span.end.column)
                        > (span.end.row, span.end.column)
                    || (inv.end.row, inv.end.column)
                        >= (method_name_span.start.row, method_name_span.start.column)
                {
                    return None;
                }
                Some((*inv, r, method_name_span.start))
            })
            .max_by_key(|(_, _, mstart)| (mstart.row, mstart.column))
            .map(|(inv, r, _)| (inv, r.unqualified_target_name().to_string(), None::<usize>))
        {
            if let Some(recv_ty) = self.expr_type_at_span(inv, module_index) {
                if let Some(t) = self.member_value_type(&recv_ty, &member, module_index, arity) {
                    return Some(t);
                }
            }
        }
        // Call-expression chain root (`make_widget().next()`, and a ctor call
        // on a temporary `Box().getInner()` falls out the same way — both are
        // plain `call_expression`s, not member accesses, so the pack
        // member-chain arm above never sees them as a receiver). The queried
        // span here is the INVOCANT'S full extent (callee + parens/args,
        // handed in by the member-chain arm's own recursion); the call's ref
        // only spans the callee token — same start, narrower end. Feeds the
        // call's own return into the chain exactly like a variable's type
        // does.
        if let Some((recv_idx, kind)) = self.refs.iter().enumerate().find_map(|(i, r)| {
            if r.span.start == span.start
                && (r.span.end.row, r.span.end.column) <= (span.end.row, span.end.column)
                && matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall { .. })
            {
                Some((i, &r.kind))
            } else {
                None
            }
        }) {
            match kind {
                RefKind::MethodCall { .. } => {
                    if let Some(t) =
                        self.method_call_return_type_via_bag(recv_idx, module_index)
                    {
                        return Some(t);
                    }
                }
                RefKind::FunctionCall { .. } => {
                    // Call-root chain arm: feed the call's real written arg
                    // count so an arity-discriminated overload types by the
                    // args actually passed, not a hardcoded 0.
                    if let Some(t) = self.sub_return_type_at_arity_ctx(
                        &self.refs[recv_idx].target_name,
                        Some(self.refs[recv_idx].arg_count.unwrap_or(0) as u32),
                        module_index,
                    ) {
                        return Some(t);
                    }
                }
                _ => {}
            }
        }
        self.bag_query_expr_span(span, module_index)
    }

    /// Resolve a sub's return type at a call site given the caller's arg
    /// count. Queries the arity-dispatch reducer; if no arity fact
    /// exists, falls back to `sub_return_type` (declared /
    /// inferred-from-returns).
    ///
    /// `arity` is the number of *additional* args passed after the
    /// invocant on methods (or simply the arg count for plain subs).
    pub fn sub_return_type_at_arity(
        &self,
        sub_name: &str,
        arity: Option<u32>,
    ) -> Option<InferredType> {
        self.sub_return_type_at_arity_ctx(sub_name, arity, None)
    }

    /// Index-threaded sibling of `sub_return_type_at_arity`. A free-function
    /// call whose callee's prototype lives in an INCLUDED header carries no
    /// local symbol and no Perl export edge; its return type crosses the file
    /// boundary only through `query_sub_return_type`'s include-closure arm,
    /// which needs the index. `expr_type_at_span`'s call arms route here so a
    /// pack `makeGadget()->field` types its receiver cross-file.
    pub fn sub_return_type_at_arity_ctx(
        &self,
        sub_name: &str,
        arity: Option<u32>,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        let ctx = self.bag_context(module_index);
        crate::model::witnesses::query_sub_return_type(
            &self.witnesses,
            self.symbols.as_slice(),
            sub_name,
            arity,
            None,
            Some(&ctx),
        )
    }

    /// List hash keys that have been written to on instances of `class`.
    /// Powers dynamic-key completion: `$self->{` completes
    /// with both `has`-declared keys and keys observed as write
    /// targets across the class's methods.
    #[allow(dead_code)] // documented type-query entry point; CLAUDE.md
    pub fn mutated_keys_on_class(&self, class: &str) -> Vec<String> {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload};
        let mut out: Vec<String> = Vec::new();
        for w in self.witnesses.all() {
            if let WitnessAttachment::HashKey { owner, name } = &w.attachment {
                let matches_class = match owner {
                    HashKeyOwner::Class(c) if c == class => true,
                    HashKeyOwner::Sub { package: Some(p), .. } if p == class => true,
                    _ => false,
                };
                if !matches_class {
                    continue;
                }
                if matches!(
                    &w.payload,
                    WitnessPayload::Fact { family, .. } if family == "mutation"
                ) && !out.contains(name)
                {
                    out.push(name.clone());
                }
            }
        }
        out
    }

    /// True when a closed literal shape on `var_text` is the variable's
    /// whole story in this file: the scalar is never reassigned. Key
    /// writes AND escapes are not gate clauses — both are modeled on
    /// the shape itself by the mutation-extension pass (writes extend
    /// or open; an escape is an open-switching write at the escape
    /// span, so reads before it keep their closed shape). The one
    /// remaining clause is the trust-gate stand-in for the unmodeled
    /// conditional-reassignment disagreement
    /// (docs/adr/structural-shapes.md); the unknown-hash-key
    /// diagnostic only fires behind it.
    pub fn closed_shape_is_whole_story(&self, var_text: &str) -> bool {
        !self.reassigned_scalars.contains(var_text)
    }

    /// Closed-shape hash-key typo sites: a READ of `$config->{typo}` where
    /// `$config`'s structural literal is CLOSED (no spread, no dynamic key)
    /// and doesn't define the key. Writes are skipped — assigning a new key
    /// extends the shape, it isn't a typo. Open shapes are skipped — the
    /// spread may carry the key. The whole-story gate skips reassigned/
    /// escaped vars (the trust-gate stand-in for the unmodeled lattice
    /// widenings — docs/adr/structural-shapes.md).
    //
    // TODO(dbic-row-deref): warn on `$row->{col}` where `$row` is a DBIC Result
    // class and `col` is a `Bridged` column — a column isn't a hash slot, so the
    // deref is `undef` (meant `$row->col`). Detection seam is here (invocant type
    // → class → `field_projections_named` has a bridged column for the key), but
    // it must gate on NOT-HashRefInflator first (where `$row->{col}` IS valid),
    // which we don't model yet. Spec: docs/adr/narrowing-diagnostics.md (Forward work).
    pub fn closed_shape_key_typos(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<KeyTypoSite> {
        let mut out = Vec::new();
        for r in self.refs() {
            let RefKind::HashKeyAccess { ref var_text, .. } = r.kind else { continue };
            // `$config->{k}` (scalar holding a hashref) and `$config{k}`
            // (literal `%config`, canonical var_text) — same model, both
            // spellings.
            if !(var_text.starts_with('$') || var_text.starts_with('%')) {
                continue;
            }
            if matches!(r.access, AccessKind::Write) {
                continue;
            }
            let Some(t) =
                self.inferred_type_via_bag_ctx(var_text, r.span.start, module_index)
            else {
                continue;
            };
            let InferredType::HashWithKeys { ref keys, open: false } = t else { continue };
            if keys.iter().any(|(k, _)| k == &r.target_name) {
                continue;
            }
            if !self.closed_shape_is_whole_story(var_text) {
                continue;
            }
            out.push(KeyTypoSite {
                span: r.span,
                key: r.target_name.clone(),
                known_keys: keys.iter().map(|(k, _)| k.clone()).collect(),
                spelling: Some(var_text.clone()),
            });
        }
        out
    }

    /// The expression-base spelling of the same typo: `cfg()->{kye}` /
    /// `$obj->get_config->{kye}` — no variable in hand, so the ref walk in
    /// `closed_shape_key_typos` can't see it. The drill's own Projected
    /// witness encodes exactly the (base, key) pair; materialize the base
    /// (the registry chases through call returns, cross-file included) and
    /// apply the same closed-shape check. No whole-story gate: the value is
    /// freshly produced, and the producer's own mutation/escape widening
    /// already rode along on its shape.
    pub fn projected_key_typos(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<KeyTypoSite> {
        use crate::model::witnesses::{ProjectionStep, WitnessAttachment, WitnessPayload};
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<(Span, &str)> = std::collections::HashSet::new();
        for w in self.witnesses.all() {
            let WitnessPayload::Projected {
                base: WitnessAttachment::Expr(base_span),
                step: ProjectionStep::HashKey(ref key),
            } = w.payload
            else {
                continue;
            };
            if !seen.insert((w.span, key.as_str())) {
                continue;
            }
            // A base that is a bare variable read (its Expr attachment
            // edges to a Variable) is `closed_shape_key_typos`'s territory —
            // it carries the whole-story gate this walk deliberately
            // doesn't. Materializing it here would bypass the gate
            // (the Compiler.pm conditional-reassignment FP).
            let base_is_variable = self
                .witnesses
                .for_attachment(&WitnessAttachment::Expr(base_span))
                .iter()
                .any(|bw| {
                    matches!(
                        bw.payload,
                        WitnessPayload::Edge(WitnessAttachment::Variable { .. })
                    )
                });
            if base_is_variable {
                continue;
            }
            let Some(t) = self.expr_type_at_span(base_span, module_index) else {
                continue;
            };
            let InferredType::HashWithKeys { ref keys, open: false } = t else { continue };
            if keys.iter().any(|(k, _)| k == key) {
                continue;
            }
            out.push(KeyTypoSite {
                span: w.span,
                key: key.clone(),
                known_keys: keys.iter().map(|(k, _)| k.clone()).collect(),
                spelling: None,
            });
        }
        out
    }

    /// Get the return type of a named sub/method (local definitions
    /// only). Routes through the bag — `Symbol(sid)` writeback witness
    /// is the post-field-deletion authority. Returns owned because
    /// the value is synthesized by the reducer, not stored.
    pub fn sub_return_type_local(&self, name: &str) -> Option<InferredType> {
        for sym in &self.symbols {
            if sym.name == name && matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                if let Some(t) = self.symbol_return_type_via_bag(sym.id, None) {
                    return Some(t);
                }
            }
        }
        None
    }

    /// Get the return type of a named sub/method. Local definitions
    /// first (via the bag's `Symbol(sym_id)` writeback), then imported
    /// sub returns (resolved lazily through `query_sub_return_type`'s
    /// walk of `module_index.find_exporters` into the cached module's
    /// own `Symbol(_)` witnesses).
    #[allow(dead_code)] // public type-query API; used by tooling/tests
    pub fn sub_return_type(&self, name: &str) -> Option<InferredType> {
        self.sub_return_type_at_arity(name, None)
    }

    /// Provenance of a symbol's return type — `Inferred` by default,
    /// `PluginOverride` for plugin-declared overrides. Always returns
    /// a value (never `None`) so debug tooling can ask "where did this
    /// come from?" without branching on missingness.
    #[allow(dead_code)] // debug-introspection accessor; consumed by tests
    pub fn return_type_provenance(&self, sym_id: SymbolId) -> TypeProvenance {
        self.type_provenance
            .get(&sym_id)
            .cloned()
            .unwrap_or(TypeProvenance::Inferred)
    }

}
