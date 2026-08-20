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
            package: pkg,
        });
        self.scope_stack.push(id);
        id
    }

    pub(super) fn pop_scope(&mut self) -> Option<ScopeId> {
        self.scope_stack.pop()
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

    /// Is a bare `shift` / `$_[0]` here the method invocant (→ enclosing class),
    /// or just `arg[0]`? OO-by-convention is the default (a base class like
    /// `DateTime` types `bless {...}, ref $_[0]` even without declared parents),
    /// EXCEPT in a package that explicitly opted out of class machinery via
    /// `use Mojo::Base -strict`. There the first `@_` element is an ordinary
    /// argument, so typing it as the class produced bogus `unresolved-method`
    /// diagnostics (`$tx = shift; $tx->res` in `Mojo::WebSocket`). (rule #10:
    /// the opt-out is recorded as a package property at the `use` site, not
    /// re-derived from the `shift` shape here.)
    pub(super) fn shift_is_invocant_here(&self, node: Node<'a>) -> bool {
        match self.package_for_node(node) {
            Some(pkg) => !self.non_oo_packages.contains(&pkg),
            None => true,
        }
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
        let id = SymbolId(self.next_symbol_id);
        self.next_symbol_id += 1;
        // Every symbol attaches to the current lexical scope. Package
        // context lives separately in `package_ranges`; the variable
        // resolver gates `our` decls by package match at lookup time
        // (so bare `$version` from a sibling `package main;` doesn't
        // reach a Calculator-package `our $version`).
        self.symbols.push(Symbol {
            id,
            name,
            kind,
            span,
            selection_span,
            scope: self.current_scope(),
            package,
            detail,
            namespace,
            presentation: Default::default(),
            attributes: Vec::new(),
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

    // ---- Package-range tracking ----

    /// Record a `package Foo;` / `class Foo;` (statement form). Trims
    /// the previously-open statement range to end at `start`, then
    /// pushes a new range whose end is seeded with the file end —
    /// trimmed in turn when a successor appears, or left at file end
    /// if none does.
    pub(super) fn open_statement_package_range(&mut self, name: String, start: Point) {
        use crate::model::file_analysis::{PackageKind, PackageRange};
        if let Some(idx) = self.open_statement_package.take() {
            self.package_ranges[idx].span.end = start;
        }
        let file_end = self
            .scope_stack
            .first()
            .map(|id| self.scopes[id.0 as usize].span.end)
            .unwrap_or(start);
        self.package_ranges.push(PackageRange {
            package: name,
            span: Span { start, end: file_end },
            kind: PackageKind::Statement,
        });
        self.open_statement_package = Some(self.package_ranges.len() - 1);
    }

    /// Record a `package Foo { … }` / `class Foo { … }` (block form).
    /// Span is the node's own span — no successor-trimming required.
    /// Block forms do NOT supplant any statement-form range that
    /// brackets them: `package Foo; package Bar { … }` leaves Foo
    /// covering everything outside the Bar block.
    pub(super) fn push_block_package_range(&mut self, name: String, span: Span) {
        use crate::model::file_analysis::{PackageKind, PackageRange};
        self.package_ranges.push(PackageRange {
            package: name,
            span,
            kind: PackageKind::Block,
        });
    }

    /// Build-time mirror of `FileAnalysis::package_at`. Used by the
    /// variable resolver to gate `our` decls by package context — the
    /// builder can't call into FileAnalysis (it hasn't been
    /// constructed yet).
    pub(super) fn package_at_pos(&self, point: Point) -> Option<&str> {
        let mut best: Option<&crate::model::file_analysis::PackageRange> = None;
        for r in &self.package_ranges {
            if !crate::model::file_analysis::contains_point(&r.span, point) {
                continue;
            }
            let win = match best {
                None => true,
                Some(prev) => {
                    let cur_start = (r.span.start.row, r.span.start.column);
                    let prev_start = (prev.span.start.row, prev.span.start.column);
                    let cur_size = (
                        r.span.end.row - r.span.start.row,
                        r.span.end.column.saturating_sub(r.span.start.column),
                    );
                    let prev_size = (
                        prev.span.end.row - prev.span.start.row,
                        prev.span.end.column.saturating_sub(prev.span.start.column),
                    );
                    cur_start > prev_start || (cur_start == prev_start && cur_size < prev_size)
                }
            };
            if win {
                best = Some(r);
            }
        }
        best.map(|r| r.package.as_str())
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
            if matches!(n.kind(), "list_expression" | "parenthesized_expression") {
                crate::cst::flatten_list(n, &mut out);
            } else {
                out.push(n);
            }
        }
        out.into_iter().filter(|n| n.is_named()).collect()
    }

    /// Span of the body's last expression on an
    /// `anonymous_subroutine_expression`. Mirrors
    /// `infer_anonymous_sub_return_type`'s body-walk — the last
    /// statement, unwrapped from `expression_statement` /
    /// `return_expression` if necessary, gives us the expression
    /// whose type IS the sub's return when called. Plugins use
    /// this to emit a back-edge from the synthesized Method's
    /// Symbol to that Expr, deferring return-type inference to
    /// query time.
    pub(super) fn anonymous_sub_body_last_expr_span(&self, node: Node<'a>) -> Option<Span> {
        if node.kind() != "anonymous_subroutine_expression" {
            return None;
        }
        let body = node.child_by_field_name("body")?;
        let mut node = body.named_child(body.named_child_count().checked_sub(1)?)?;
        // Peel through `expression_statement` and `return_expression`
        // wrappers (an explicit `return $expr;` shows up as
        // `expression_statement → return_expression → $expr` in
        // tree-sitter-perl). One unwrap isn't enough.
        loop {
            match node.kind() {
                "expression_statement" | "return_expression" => {
                    node = node.named_child(0)?;
                }
                _ => break,
            }
        }
        Some(node_to_span(node))
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
    ///     Falls back to `Expr(body_last_expr_span)` only when the
    ///     symbol stash misses, which would mean a parse-error /
    ///     ERROR-recovery path where `visit_anonymous_sub` didn't
    ///     run; the body-span chase is still meaningful in that case.
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
                let span = node_to_span(node);
                if let Some(sym_id) = self.anon_sub_symbol_by_span.get(&span) {
                    return Some(crate::model::witnesses::WitnessAttachment::Symbol(*sym_id));
                }
                self.anonymous_sub_body_last_expr_span(node)
                    .map(crate::model::witnesses::WitnessAttachment::Expr)
            }
            "refgen_expression" => {
                let names = self.extract_names_from_refgen(node);
                let raw = names.into_iter().next()?;
                let (class, name) = match crate::model::file_analysis::split_qualified(&raw) {
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
        if let (Some(pkg), _) = crate::model::file_analysis::split_qualified(call_name) {
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
