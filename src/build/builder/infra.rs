//! Walk infrastructure: scope stack, symbol/ref minting, package-range
//! tracking, coderef return-edge derivation, and call-argument extraction.

use super::*;

impl<'a> Builder<'a> {
    // ---- Scope management ----

    pub(super) fn push_scope(&mut self, kind: ScopeKind, span: Span, package: Option<String>) -> ScopeId {
        let id = ScopeId(self.next_scope_id);
        self.next_scope_id += 1;
        let parent = self.scope_stack.last().copied();
        let pkg = package.or_else(|| {
            // Inherit package from current state or parent
            self.current_package.clone().or_else(|| {
                parent.and_then(|p| self.scopes[p.0 as usize].package.clone())
            })
        });
        self.scopes.push(Scope {
            id,
            parent,
            kind,
            span,
            owner: None,
            implicit_receiver: false,
            closed: false,
            captures: Vec::new(),
            direct: false,
            declaration_rules: Vec::new(),
            package: pkg,
        });
        self.scope_stack.push(id);
        id
    }

    pub(super) fn pop_scope(&mut self) -> Option<ScopeId> {
        self.scope_stack.pop()
    }

    /// Switch the declaration rule from the end of `span` on, in the scope
    /// `span` sits in.
    pub(super) fn switch_declaration_rule(&mut self, span: Span, required: bool) {
        let scope = self.scope_at_point(span.start);
        self.scopes[scope.0 as usize]
            .declaration_rules
            .push(crate::model::file_analysis::DeclarationRule { at: span.end, required });
    }

    pub(super) fn current_scope(&self) -> ScopeId {
        *self.scope_stack.last().expect("scope stack empty")
    }

    /// Package/class name surrounding `node`. Reads the innermost
    /// containing scope's `package` field — set on both
    /// `package Foo;` and `class Foo { … }` entries, so this works
    /// for either flavor of class declaration. Used by
    /// `invocant_type_at_node` for `$self` / `shift` / `__PACKAGE__`
    /// resolution post-walk, where `self.current_package` is stale
    /// (it holds the walk's last-opened package, not the one
    /// containing the node we're querying).
    pub(super) fn package_for_node(&self, node: Node<'a>) -> Option<String> {
        let scope_id = self.scope_at_point(node.start_position());
        let mut cur = Some(scope_id);
        while let Some(sid) = cur {
            let s = &self.scopes[sid.0 as usize];
            if let Some(ref pkg) = s.package {
                return Some(pkg.clone());
            }
            cur = s.parent;
        }
        None
    }

    /// A bare `shift` consumes `@_`'s head: from the call's end the window
    /// is the tail, latest-wins like any rebind. Only a shift that certainly
    /// ran keeps the window known (`cst::conditional_arm` is the
    /// syntactic stand-in for that until the CFG tier, `docs/epics/16-cfg-
    /// tier.md`); any other opens it, so later reads answer unknown.
    pub(super) fn consume_arg_head(&mut self, node: Node<'a>) {
        let Some(scope) = self.enclosing_sub_scope() else { return };
        let conditional = crate::cst::conditional_arm(self.syntax, node, crate::cst::ArmBound::Sub).is_some();
        let tail = match self.arg_window_at(node) {
            Some(InferredType::Sequence(v)) if !conditional => v.into_iter().skip(1).collect(),
            _ => Vec::new(),
        };
        let at = node.end_position();
        self.arg_head_consumes.push((scope, at, conditional));
        self.push_arg_window(scope, Span { start: at, end: at }, tail);
    }

    /// Seed `scope`'s argument window at `span` with `window`. A seed laid
    /// after the walk follows the walk's shifts in the bag, and the
    /// variable fold is latest-wins in bag order, so the scope's shifts
    /// are replayed after it: each consumes the head of the window it
    /// follows, as `consume_arg_head` did at its own point.
    // TODO(source-order-fold): evaluate making the variable fold latest-wins
    // by source position (on the whole-program branch, design/whole-program-residuals.md);
    // a late seed would then lose to the shifts below it and this replay goes.
    pub(super) fn seed_arg_window(&mut self, scope: ScopeId, span: Span, window: Vec<InferredType>) {
        self.push_arg_window(scope, span, window.clone());
        let consumes: Vec<(Point, bool)> = self
            .arg_head_consumes
            .iter()
            .filter(|(s, ..)| *s == scope)
            .map(|(_, at, conditional)| (*at, *conditional))
            .collect();
        let mut window = window;
        for (at, conditional) in consumes {
            window = if conditional { Vec::new() } else { window.into_iter().skip(1).collect() };
            self.push_arg_window(scope, Span { start: at, end: at }, window.clone());
        }
    }

    fn push_arg_window(&mut self, scope: ScopeId, span: Span, window: Vec<InferredType>) {
        self.push_type_constraint(TypeConstraint {
            variable: "@_".into(),
            scope,
            constraint_span: span,
            inferred_type: InferredType::Sequence(window),
        });
    }

    /// `@_`'s argument window at `node`'s own point: the sub-entry shape
    /// with every earlier straight-line `shift` consumed. `None` outside a
    /// sub, or after a shift that may not have run.
    pub(super) fn arg_window_at(&self, node: Node<'a>) -> Option<InferredType> {
        self.bag_query_variable("@_", self.scope_at_point(node.start_position()), node.start_position())
    }

    /// Innermost scope containing `point`. Mirrors
    /// `FileAnalysis::scope_at` but reads `&self.scopes` directly so
    /// it's callable from within Builder during and after the walk.
    /// Falls back to `ScopeId(0)` (the file scope) if no scope
    /// matches — a defensive default for cases where the walk hasn't
    /// produced any scope containing the point yet.
    pub(super) fn scope_at_point(&self, point: Point) -> ScopeId {
        let mut best: Option<(ScopeId, u64)> = None;
        for scope in &self.scopes {
            if !crate::model::file_analysis::contains_point(&scope.span, point) {
                continue;
            }
            let r = scope.span.end.row.saturating_sub(scope.span.start.row) as u64;
            let c = if scope.span.start.row == scope.span.end.row {
                scope.span.end.column.saturating_sub(scope.span.start.column) as u64
            } else {
                0
            };
            let size = r * 1_000_000 + c;
            if best.is_none() || size <= best.unwrap().1 {
                best = Some((scope.id, size));
            }
        }
        best.map(|(id, _)| id).unwrap_or(ScopeId(0))
    }

    // ---- Symbol/Ref creation ----

    pub(super) fn add_symbol(&mut self, name: String, kind: SymKind, span: Span, selection_span: Span, detail: SymbolDetail) -> SymbolId {
        self.add_symbol_ns(name, kind, span, selection_span, detail, Namespace::Language)
    }

    /// Where parameter `var` of the callable owning `scope` is bound — the
    /// site its `ParamInfo` carries. `None` when the scope owns no callable
    /// or the callable declares no such parameter (a `$_[0]` read).
    pub(super) fn binding_site_of(&self, var: &str, scope: ScopeId) -> Option<Point> {
        let owner = self.scopes.get(scope.0 as usize)?.owner?;
        match &self.symbols[owner.0 as usize].detail {
            SymbolDetail::Sub { params, .. } => params.iter().find(|p| p.name == var)?.binding_site,
            _ => None,
        }
    }

    /// Push the body scope of callable `owner`, recording the pairing both
    /// ways (`Scope::owner`, `owner_scope`).
    pub(super) fn push_callable_scope(&mut self, kind: ScopeKind, span: Span, owner: SymbolId) -> ScopeId {
        let id = self.push_scope(kind, span, None);
        self.scopes[id.0 as usize].owner = Some(owner);
        self.owner_scope.insert(owner, id);
        id
    }

    /// Record that `a` and `b` were minted from one declaration token
    /// (`Symbol::declared_with`), each way.
    pub(super) fn pair_co_declared(&mut self, a: SymbolId, b: SymbolId) {
        self.symbols[a.0 as usize].declared_with = Some(b);
        self.symbols[b.0 as usize].declared_with = Some(a);
    }

    pub(super) fn add_symbol_ns(
        &mut self,
        name: String,
        kind: SymKind,
        span: Span,
        selection_span: Span,
        detail: SymbolDetail,
        namespace: Namespace,
    ) -> SymbolId {
        let pkg = self.current_package.clone();
        self.add_symbol_in_package(name, kind, span, selection_span, detail, namespace, pkg)
    }

    /// `add_symbol` with an explicit package override. Cross-package
    /// glob installs (`*{'DateTime::'.$sub} = …`) name a target package
    /// in the glob string that differs from `current_package` (the file
    /// declares e.g. `package DateTime::PP`). The synthesized tail
    /// (`_ymd2rd`) must be keyed under the *named* package so
    /// `PackageSymbol{DateTime, _ymd2rd}` resolves — not the file's
    /// own package. Every other caller keeps `current_package` via
    /// `add_symbol_ns`.
    pub(super) fn add_symbol_in_package(
        &mut self,
        name: String,
        kind: SymKind,
        span: Span,
        selection_span: Span,
        detail: SymbolDetail,
        namespace: Namespace,
        package: Option<String>,
    ) -> SymbolId {
        let scope = self.current_scope();
        self.add_symbol_in_scope(scope, name, kind, span, selection_span, detail, namespace, package)
    }

    /// `add_symbol_in_package` into an explicit scope — for post-walk
    /// minting, after the scope stack has unwound.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_symbol_in_scope(
        &mut self,
        scope: ScopeId,
        name: String,
        kind: SymKind,
        span: Span,
        selection_span: Span,
        detail: SymbolDetail,
        namespace: Namespace,
        package: Option<String>,
    ) -> SymbolId {
        let id = SymbolId(self.next_symbol_id);
        self.next_symbol_id += 1;
        if matches!(kind, SymKind::Variable | SymKind::Field) {
            self.var_decls.entry((scope, name.clone())).or_default().push(id);
        }
        // Perl says "this constructs" with a name and nothing else, so the
        // convention is read here, at the mint, and consumers ask the flag.
        let flags = if matches!(kind, SymKind::Sub | SymKind::Method)
            && crate::model::conventions::is_constructor_name(&name)
        {
            crate::model::file_analysis::SymbolFlags::CONSTRUCTOR
        } else {
            Default::default()
        };
        // Every symbol attaches to the current lexical scope. Package
        // context lives separately in `namespace_ranges`; the variable
        // resolver gates `our` decls by package match at lookup time
        // (so bare `$version` from a sibling `package main;` doesn't
        // reach a Calculator-package `our $version`).
        self.symbols.push(Symbol {
            id,
            name,
            kind,
            span,
            selection_span,
            scope,
            package,
            detail,
            namespace,
            presentation: Default::default(),
            attributes: Vec::new(),
            flags,
            declared_with: None,
            deref_stack: Vec::new(),
            // Perl carries params in `SymbolDetail::Sub`; `param_arity()`
            // reads them. No pack-minted arity here.
            arity: None,
        });
        id
    }

    /// The just-minted symbol's presentation, for the synthesis sites
    /// that stamp policy (hidden twins, plugin display/label). SymbolId
    /// is the positional index, so this is O(1).
    pub(super) fn presentation_mut(
        &mut self,
        id: SymbolId,
    ) -> &mut crate::model::file_analysis::Presentation {
        &mut self.symbols[id.0 as usize].presentation
    }

    // ---- Namespace-lane tracking ----

    /// Record a `package Foo;` / `class Foo;` (statement form): in force to
    /// the end of the `{ … }` block it is written in (the file, at top
    /// level), until a successor at the same block depth ends it.
    pub(super) fn open_statement_package_range(&mut self, name: String, start: Point) {
        let region_end = self.package_regions.last().copied().unwrap_or_else(|| {
            self.scope_stack.first().map(|id| self.scopes[id.0 as usize].span.end).unwrap_or(start)
        });
        let depth = self.package_regions.len();
        self.open_sections.open_statement(&mut self.namespace_ranges, name, start, region_end, depth);
    }

    /// Record a `package Foo { … }` / `class Foo { … }` (block form). Block
    /// forms do NOT supplant any statement-form range that brackets them:
    /// `package Foo; package Bar { … }` leaves Foo covering everything
    /// outside the Bar block.
    pub(super) fn push_block_package_range(&mut self, name: String, span: Span) {
        crate::model::file_analysis::OpenSections::push_block(&mut self.namespace_ranges, name, span);
    }

    /// The package in force at `point`, as the walk has recorded it so
    /// far — `FileAnalysis::package_at`'s answer for Perl, before the
    /// analysis exists.
    pub(super) fn package_at_pos(&self, point: Point) -> Option<&str> {
        crate::model::file_analysis::namespace_range_at(&self.namespace_ranges, point).map(|r| r.namespace.as_str())
    }

    pub(super) fn add_ref(&mut self, kind: RefKind, span: Span, target_name: String, access: AccessKind) {
        self.add_bound_ref(kind, span, target_name, access, None);
    }

    /// `add_ref` with a walk-time resolution outcome already in hand (a
    /// `FunctionCall` package pin, a plugin-declared owner).
    pub(super) fn add_bound_ref(
        &mut self,
        kind: RefKind,
        span: Span,
        target_name: String,
        access: AccessKind,
        binding: Option<crate::model::file_analysis::RefBinding>,
    ) {
        self.refs.push(Ref {
            kind,
            span,
            scope: self.current_scope(),
            target_name,
            access,
            binding,
            folded_from: None,
            arg_count: None,
        });
    }

    // ---- Plugin dispatch helpers ----

    /// Normalize a call's `arguments` field into a flat list of argument
    /// nodes. Tree-sitter-perl wraps multi-arg lists in `list_expression`;
    /// single-arg calls present the arg directly.
    pub(super) fn extract_call_args(&self, call_node: Node<'a>) -> Vec<Node<'a>> {
        crate::cst::call_args(call_node)
    }

    /// The FLAT positional arg sequence plugins see — all grouping peeled.
    /// `has 'x' => (is => 'ro')`, `has 'x', is => 'ro'`, and the lisp-y
    /// `has(('x' => (is => ('ro'))))` are the same keyval sequence; only the
    /// parenthesization differs, and `list_expression`/`parenthesized_expression`
    /// are pure grouping in Perl. Delegates the recursive splice to the one
    /// `cst::flatten_list` primitive (shared with hash/array literals and
    /// pair walking); a non-group arg passes through whole. Plugin-facing
    /// view ONLY — arity stays on the un-peeled `extract_call_args`.
    pub(super) fn flat_call_args(&self, args_raw: Vec<Node<'a>>) -> Vec<Node<'a>> {
        let mut out = Vec::new();
        for n in args_raw {
            if crate::cst::is_list_literal(n) {
                crate::cst::flatten_list(n, &mut out);
            } else {
                out.push(n);
            }
        }
        out.into_iter().filter(|n| n.is_named()).collect()
    }

    /// Single derivation site for a CodeRef-shaped value's
    /// `return_edge`, given the source node. Used by `expr_payload`
    /// when emitting the bag witness for `anonymous_subroutine_expression`
    /// / `refgen_expression` — the bag is canonical and there's no
    /// second consumer that bypasses it.
    ///
    /// Two recognized shapes:
    ///   - `anonymous_subroutine_expression` → `Symbol(sub_id)`,
    ///     looked up in `anon_sub_symbol_by_span`. The bag's
    ///     symbol-keyed reducers (`ReturnExprReducer`,
    ///     `SubReturnReducer`) all see anon subs the same way they
    ///     see named subs — uniform attachment shape, no
    ///     special-case for "this is anonymous."
    ///     The symbol carries the body's return arms (`record_sub_tail`),
    ///     so an anon sub's return is read exactly as a named sub's.
    ///   - `refgen_expression` (`\&foo`, `\&Foo::bar`,
    ///     `\&$const_folded`) → `PackageSymbol { package, name }`.
    ///     Bag's MRO + `module_index` machinery resolves it,
    ///     including cross-file. Bare names default `class` to
    ///     the current package; qualified names split at the
    ///     last `::`. `\&$var` with a non-const-foldable name
    ///     returns `None`.
    ///
    /// Other node kinds return `None` (caller decides whether to
    /// wrap the result in `CodeRef { return_edge: None }` for
    /// opaque-coderef sources or fall through entirely).
    pub(super) fn coderef_return_edge_for(
        &self,
        node: Node<'a>,
    ) -> Option<crate::model::witnesses::WitnessAttachment> {
        match node.kind() {
            "anonymous_subroutine_expression" => {
                // The only caller (`expr_payload`) mints the symbol first.
                self.anon_sub_symbol_by_span
                    .get(&node_to_span(node))
                    .map(|sym_id| crate::model::witnesses::WitnessAttachment::Symbol(*sym_id))
            }
            "refgen_expression" => {
                let names = self.extract_names_from_refgen(node);
                let raw = names.into_iter().next()?;
                let (class, name) = match crate::model::file_analysis::split_qualified(&raw, &crate::model::conventions::PERL_SPELLINGS) {
                    (Some(c), n) => (c.to_string(), n.to_string()),
                    (None, _) => (self.current_package.clone()?, raw),
                };
                Some(crate::model::witnesses::WitnessAttachment::PackageSymbol { package: class, name })
            }
            _ => None,
        }
    }

    /// Resolve a bare `foo()` call to the package whose `sub foo` it
    /// refers to. Order mirrors Perl's name-lookup rule:
    ///
    ///   1. Explicit qualifier (`Foo::bar()` → `Foo`).
    ///   2. Enclosing package that declares `sub <name>` locally (so
    ///      `package Foo { sub bar {} bar(); }` resolves to `Foo`).
    ///   3. Most-recent import whose `imported_symbols` lists this
    ///      name (`use Bler qw/hi/` → `Bler`). Later imports win —
    ///      Perl's later `use` shadows earlier one.
    ///
    /// Returns `None` when none of those pin a package. Downstream
    /// class/package-scoped queries treat `None` as no-match rather
    /// than falling back to name-only union.
    pub(super) fn resolve_call_package(&self, call_name: &str) -> Option<String> {
        // (1) Qualified: `Foo::bar` → `Foo`.
        if let (Some(pkg), _) = crate::model::file_analysis::split_qualified(call_name, &crate::model::conventions::PERL_SPELLINGS) {
            return Some(pkg.to_string());
        }
        // (2) Enclosing package defines the sub locally.
        if let Some(ref pkg) = self.current_package {
            if self.symbols.iter().any(|s| {
                s.name == call_name
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && s.package.as_deref() == Some(pkg.as_str())
            }) {
                return Some(pkg.clone());
            }
        }
        // (3) Imports — walk in reverse order so later `use` wins.
        for imp in self.imports.iter().rev() {
            if let Some(sym) = imp.imported_symbols.iter().find(|s| s.local_name == *call_name) {
                // A renaming import (`use Exp beta => { -as => 'rb' }`) binds a
                // LOCAL alias the module doesn't define under that name. The
                // alias belongs to the CONSUMING package — keying its calls
                // there keeps rename/references local (and off the exporter's
                // unrelated symbols, e.g. a stray `Exp::rb`). goto-def still
                // reaches `Exp::beta` via the import binding's remote name,
                // which `resolve_imported_function` reads independently.
                if sym.remote_name.is_some() {
                    return self.current_package.clone();
                }
                return Some(imp.module_name.clone());
            }
        }
        None
    }
}
