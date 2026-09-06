//! Tree-reading extraction: LHS/variable/key/name extraction, access
//! classification, and the hash-key def/access emission built directly on
//! those readings. Charter: a helper belongs here only when it READS a
//! common CST shape into a value for several visitors — anything owning a
//! verb family's semantics lives with that family's part (visit_bless,
//! visit_method, docs, pipeline, …).

use super::*;

impl<'a> Builder<'a> {
    pub(super) fn get_decl_keyword(&self, var_decl: Node<'a>) -> Option<String> {
        for i in 0..var_decl.child_count() {
            if let Some(child) = var_decl.child(i) {
                let k = child.kind();
                if matches!(k, "my" | "our" | "state" | "field") {
                    return Some(k.to_string());
                }
            }
        }
        None
    }

    pub(super) fn collect_vars_from_decl(&self, node: Node<'a>) -> Vec<(String, Span)> {
        let mut vars = Vec::new();
        self.collect_vars_walk(node, &mut vars);
        vars
    }

    pub(super) fn collect_vars_walk(&self, node: Node<'a>, out: &mut Vec<(String, Span)>) {
        match node.kind() {
            "scalar" | "array" | "hash" => {
                if let Some(name) = self.build_var_name(node) {
                    out.push((name, node_to_span(node)));
                }
            }
            _ => {
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        self.collect_vars_walk(child, out);
                    }
                }
            }
        }
    }

    /// Build a variable's canonical name by reading the tree: sigil
    /// comes from the node kind (`scalar` → `$`, `array` → `@`, `hash`
    /// → `%`), bare name comes from the `varname` child. This keeps us
    /// correct for edge cases where the full node text isn't just
    /// `sigil + identifier` — e.g. `${foo}` (text `${foo}`, varname
    /// `foo`), `$:field` (whatever TSP aliases into varname), or any
    /// future TSP-added sigil-bearing syntax. The previous
    /// `node.utf8_text()` + caller-side sigil-stripping broke on every
    /// one of those shapes (`{foo}`, `:field`, etc.).
    ///
    /// Falls back to the full node text when the varname child is
    /// missing (ERROR recovery, partial parses).
    pub(super) fn build_var_name(&self, node: Node<'a>) -> Option<String> {
        let sigil = match node.kind() {
            "scalar" => '$',
            "array" => '@',
            "hash" => '%',
            _ => return node.utf8_text(self.source).ok().map(|s| s.to_string()),
        };
        let varname = find_varname_child(node).and_then(|v| v.utf8_text(self.source).ok());
        match varname {
            Some(name) => Some(format!("{}{}", sigil, name)),
            None => node.utf8_text(self.source).ok().map(|s| s.to_string()),
        }
    }

    pub(super) fn first_var_child(&self, node: Node<'a>) -> Option<String> {
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if matches!(child.kind(), "scalar" | "array" | "hash") {
                    return self.build_var_name(child);
                }
            }
        }
        None
    }

    /// Record a `$var->{key} = …` write for the mutation-extension
    /// pass. Walks subscript chains down to the scalar base: the FIRST
    /// hop's key is the one the write autovivifies onto the variable's
    /// own shape (`$v->{a}{b} = …` adds `a`); only a direct single-hop
    /// write types the key's value from the RHS. Plain `%foo` elements
    /// (`$foo{k}`, no arrow) are a different variable — skipped.
    /// Record an escape as an open-switching `KeyWrite` (`key: None`)
    /// at the escape span — the modeled form of escape widening: once
    /// the reference is out of our sight, any key may have been
    /// written, which is exactly what a dynamic-key write claims.
    pub(super) fn record_escape_write(&mut self, var_text: String, node: Node<'a>) {
        let span = node_to_span(node);
        self.key_writes.push(crate::model::file_analysis::KeyWrite {
            var_text,
            key: crate::model::file_analysis::WriteKey::Unknown,
            scope: self
                .scope_stack
                .last()
                .copied()
                .unwrap_or(crate::model::file_analysis::ScopeId(0)),
            span,
            rhs_span: None,
            conditional: true,
        });
    }

    pub(super) fn record_key_write(&mut self, left: Node<'a>, rhs: Option<Node<'a>>) {
        let mut innermost = left;
        loop {
            let Some(c) = innermost.named_child(0) else { return };
            match c.kind() {
                "hash_element_expression" | "array_element_expression" => innermost = c,
                "scalar" | "container_variable" => break,
                _ => return,
            }
        }
        // Direct `$v->[N] = …` — a Sequence slot write. Only the
        // direct, static-index, arrow-deref form is modeled (the pass
        // retypes the slot / appends at len); container `$arr[N]` and
        // nested array hops stay unmodeled — there's no open flag on
        // Sequence to widen into, and no array-index diagnostic to
        // protect.
        if innermost.kind() == "array_element_expression" {
            if innermost != left {
                return;
            }
            if !crate::cst::element_arrow_deref(innermost, self.source) {
                return;
            }
            let Some(container) = innermost.named_child(0) else { return };
            if container.kind() != "scalar" {
                return;
            }
            let Ok(t) = container.utf8_text(self.source) else { return };
            if !t.starts_with('$') {
                return;
            }
            let Some(idx_node) = innermost.child_by_field_name("index") else { return };
            let Ok(Ok(idx)) = idx_node.utf8_text(self.source).map(|s| s.parse::<i32>())
            else {
                return;
            };
            let span = node_to_span(idx_node);
            self.key_writes.push(crate::model::file_analysis::KeyWrite {
                var_text: t.to_string(),
                key: crate::model::file_analysis::WriteKey::Index(idx),
                scope: self
                    .scope_stack
                    .last()
                    .copied()
                    .unwrap_or(crate::model::file_analysis::ScopeId(0)),
                span,
                rhs_span: rhs.map(node_to_span),
                conditional: crate::cst::is_conditionally_executed(left),
            });
            return;
        }
        if innermost.kind() != "hash_element_expression" {
            return;
        }
        let Some(container) = innermost.named_child(0) else { return };
        // Container form `$h{k}` writes `%h` (canonical name); deref
        // form `$v->{k}` writes through scalar `$v` — arrow required
        // (`$foo{k}` without one would be `%foo` mis-keyed to `$foo`).
        let var_text: String = if container.kind() == "container_variable" {
            match crate::cst::canonical_container_name(container, self.source) {
                Some(n) => n,
                None => return,
            }
        } else {
            if !crate::cst::element_arrow_deref(innermost, self.source) {
                return;
            }
            let Ok(t) = container.utf8_text(self.source) else { return };
            if !t.starts_with('$') {
                return;
            }
            t.to_string()
        };
        let key_node = innermost.child_by_field_name("key");
        let key = key_node
            .and_then(|k| self.extract_key_text(k))
            .and_then(|(t, dynamic)| (!dynamic).then_some(t))
            .map_or(
                crate::model::file_analysis::WriteKey::Unknown,
                crate::model::file_analysis::WriteKey::Hash,
            );
        let direct = innermost == left;
        let span = key_node
            .map(node_to_span)
            .unwrap_or_else(|| node_to_span(innermost));
        self.key_writes.push(crate::model::file_analysis::KeyWrite {
            var_text: var_text.to_string(),
            key,
            scope: self
                .scope_stack
                .last()
                .copied()
                .unwrap_or(crate::model::file_analysis::ScopeId(0)),
            span,
            rhs_span: if direct { rhs.map(node_to_span) } else { None },
            conditional: crate::cst::is_conditionally_executed(left),
        });
    }

    pub(super) fn determine_access(&self, node: Node<'a>) -> AccessKind {
        if let Some(parent) = node.parent() {
            match parent.kind() {
                "variable_declaration" => return AccessKind::Declaration,
                "assignment_expression" => {
                    // Check if we're on the left side
                    if let Some(left) = parent.child_by_field_name("left") {
                        if node.start_byte() >= left.start_byte()
                            && node.end_byte() <= left.end_byte()
                        {
                            return AccessKind::Write;
                        }
                    }
                }
                _ => {}
            }
            // Check grandparent for assignment
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "assignment_expression" {
                    if let Some(left) = grandparent.child_by_field_name("left") {
                        if node.start_byte() >= left.start_byte()
                            && node.end_byte() <= left.end_byte()
                        {
                            return AccessKind::Write;
                        }
                    }
                }
            }
        }
        AccessKind::Read
    }

    /// Map a container/slice/keyval access node to the name of the
    /// variable it actually reads. The sigil on the access site is
    /// NOT the declared sigil:
    ///
    ///   $foo[0]         → @foo   (array element, under array_element_expression)
    ///   $foo{hi}        → %foo   (hash element, under hash_element_expression)
    ///   @foo[0..1]      → @foo   (array slice — parent `slice_expression` field `array:`)
    ///   @foo{qw/.../}   → %foo   (hash slice — parent `slice_expression` field `hash:`)
    ///   %foo[0..1]      → @foo   (KV slice of array — `keyval_expression` field `array:`)
    ///   %foo{a}         → %foo   (KV slice of hash — `keyval_expression` field `hash:`)
    ///
    /// For slice/keyval we ask the parent which *field* this node is
    /// filling, because the sigil on the child is always `@` (slice)
    /// or `%` (keyval) regardless of the underlying container. Bare
    /// name comes from the `varname` child so forms like `@{$ref}[0]`
    /// (ERROR/block-varname) don't produce garbage.
    pub(super) fn canonicalize_container(&self, node: Node<'a>, text: &str) -> String {
        crate::cst::canonical_container_name(node, self.source)
            .unwrap_or_else(|| text.to_string())
    }

    /// Extract the function name from a call expression (function_call or ambiguous_function_call).
    pub(super) fn extract_call_name(&self, node: Node<'a>) -> Option<String> {
        // Only match actual function calls, not method calls
        // (method calls are handled by MethodCallBinding).
        //
        // Qualified names (`Pkg::Sub::foo()`) pass through whole — the
        // downstream fixup strips the package prefix before the
        // `return_types` lookup, so rejecting them here only loses bindings.
        //
        // Dynamic calls like `my $fn = 'get_config'; $fn->()` — the parser
        // yields a function_call_expression with function="$fn". Mirror the
        // method-call path: try constant folding to recover the concrete
        // sub name; fall through to None when the variable isn't a known
        // compile-time constant.
        match node.kind() {
            "function_call_expression" | "ambiguous_function_call_expression" => {
                let name = crate::cst::extract_call_name(node, self.source)?;
                if let Some(stripped) = name.strip_prefix('&') {
                    // `&$fn()` syntax — same deal.
                    if stripped.starts_with('$') {
                        return self.resolve_constant_strings(stripped, 0)
                            .and_then(|names| names.into_iter().next());
                    }
                    return Some(stripped.to_string());
                }
                if name.starts_with('$') {
                    return self.resolve_constant_strings(&name, 0)
                        .and_then(|names| names.into_iter().next());
                }
                Some(name)
            }
            _ => None,
        }
    }

    pub(super) fn extract_constructor_class(&self, node: Node<'a>) -> Option<String> {
        let inv = crate::cst::constructor_invocant(node, self.source)?;
        if crate::model::conventions::is_current_package_token(inv) {
            return self.current_package.clone();
        }
        Some(inv.to_string())
    }

    /// The scalar variable names in a paren-list LHS (`my ($a, $b)` /
    /// `($a, $b) = ...`) — the list-context binding sites. Empty for a
    /// scalar LHS (`my $x`) or a non-declaration. Arrays/hashes in the list
    /// are skipped (they slurp, not bind a single row).
    pub(super) fn paren_list_scalars(&self, lhs: Node<'a>) -> Vec<String> {
        // `my ($a, $b)` is a `variable_declaration` with one `variables`
        // FIELD PER scalar (not a single list node); a bare `($a, $b)` LHS is
        // a grouping container whose scalars `list_elements` splices out.
        let mut cursor = lhs.walk();
        let elems: Vec<Node<'a>> = match lhs.kind() {
            "variable_declaration" => lhs.children_by_field_name("variables", &mut cursor).collect(),
            "parenthesized_expression" | "list_expression" => crate::cst::list_elements(lhs),
            _ => Vec::new(),
        };
        elems
            .into_iter()
            .filter(|c| c.kind() == "scalar")
            .filter_map(|c| c.utf8_text(self.source).ok().map(str::to_string))
            .collect()
    }

    /// Innermost scope id containing `point` (the same smallest-span rule
    /// `apply_chain_typing_assignments` uses), `ScopeId(0)` when none.
    pub(super) fn innermost_scope_id_at(&self, point: Point) -> ScopeId {
        self.scopes
            .iter()
            .filter(|s| crate::model::file_analysis::contains_point(&s.span, point))
            .min_by_key(|s| {
                let r = (s.span.end.row.saturating_sub(s.span.start.row)) as u64;
                let c = if s.span.start.row == s.span.end.row {
                    s.span.end.column.saturating_sub(s.span.start.column) as u64
                } else {
                    0
                };
                r * 1_000_000 + c
            })
            .map(|s| s.id)
            .unwrap_or(ScopeId(0))
    }

    pub(super) fn get_var_text_from_lhs(&self, lhs: Node<'a>) -> Option<String> {
        if lhs.kind() == "variable_declaration" {
            if let Some(var) = lhs.child_by_field_name("variable") {
                return var.utf8_text(self.source).ok().map(|s| s.to_string());
            }
            // A paren list (`my ($x) = ...`, the `variables` field) binds in
            // LIST context — its slots are `lhs_list_targets`' job, and
            // handing back the first slot here would type it as the whole
            // RHS.
            return None;
        }
        if matches!(lhs.kind(), "scalar" | "array" | "hash") {
            return lhs.utf8_text(self.source).ok().map(|s| s.to_string());
        }
        None
    }

    /// The targets of a LIST/destructuring assignment LHS (`my ($a, $b) =
    /// …`) with each one's positional extraction — `Some` ONLY for a paren
    /// list (the `variables` field); `None` for a single `my $x`, leaving the
    /// existing single-var path untouched. A scalar at slot N is `Positional(N)`,
    /// a slurpy `@rest`/`%opts` is `Slurpy(N)` (consumes the tail).
    pub(super) fn lhs_list_targets(
        &self,
        lhs: Node<'a>,
    ) -> Option<Vec<(String, crate::model::file_analysis::Extraction)>> {
        use crate::model::file_analysis::Extraction;
        // `my ($a, $b)` (variable_declaration) OR a bare `($a, $b) = …`
        // reassignment (a grouping container LHS — no `my`).
        let elems: Vec<Node<'a>> = match lhs.kind() {
            // A single `my $x` uses the `variable` field; a list `my ($a, $b)`
            // uses the (repeated) `variables` field. The former is not a list.
            "variable_declaration" => {
                if lhs.child_by_field_name("variable").is_some() {
                    return None;
                }
                let mut cursor = lhs.walk();
                lhs.children_by_field_name("variables", &mut cursor).collect()
            }
            "parenthesized_expression" | "list_expression" => crate::cst::list_elements(lhs),
            _ => return None,
        };
        let mut out = Vec::new();
        let mut pos = 0usize;
        for child in elems {
            let extraction = match child.kind() {
                "scalar" => Extraction::Positional(pos),
                "array" | "hash" => Extraction::Slurpy(pos),
                // `my (undef, $x) = @_` — the placeholder takes a slot.
                "undef_expression" => {
                    pos += 1;
                    continue;
                }
                _ => continue,
            };
            if let Ok(t) = child.utf8_text(self.source) {
                out.push((t.to_string(), extraction));
            }
            pos += 1;
        }
        (!out.is_empty()).then_some(out)
    }

    /// The per-element NODES of a literal-list RHS (`(10, "str")` → [10,
    /// "str"]), so a list assignment can edge each LHS var straight to its
    /// element (emit its witness + use its own span — no container projection).
    /// `None` when the RHS isn't a literal list (`@arr`, a call) — that path
    /// needs the source typed as a Positional container instead.
    pub(super) fn list_element_nodes(&self, node: Node<'a>) -> Option<Vec<Node<'a>>> {
        // A paren group is a literal list whatever it holds — `(5)` is a
        // one-element list, so `my ($x) = (5)` binds `$x` to the `5`.
        matches!(node.kind(), "parenthesized_expression" | "list_expression")
            .then(|| crate::cst::list_elements(node))
    }

    pub(super) fn get_hash_var_from_element(&self, node: Node<'a>) -> Option<String> {
        // hash_element_expression: first named child is the container variable
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if matches!(child.kind(), "container_variable" | "keyval_container_variable" | "scalar" | "hash") {
                    return child.utf8_text(self.source).ok().map(|s| s.to_string());
                }
            }
        }
        None
    }

    pub(super) fn extract_key_text(&self, key_node: Node<'a>) -> Option<(String, bool)> {
        match key_node.kind() {
            "autoquoted_bareword" => {
                key_node.utf8_text(self.source).ok().map(|s| (s.to_string(), false))
            }
            "string_literal" | "interpolated_string_literal" => {
                // Simple string: 'key' or "key"
                let text = key_node.utf8_text(self.source).ok()?;
                // Strip quotes
                if text.len() >= 2 {
                    let inner = &text[1..text.len()-1];
                    // Dynamic if interpolated and contains $/@
                    let is_dynamic = key_node.kind() == "interpolated_string_literal"
                        && (inner.contains('$') || inner.contains('@'));
                    Some((inner.to_string(), is_dynamic))
                } else {
                    None
                }
            }
            _ => {
                // Dynamic key (variable, expression)
                key_node.utf8_text(self.source).ok().map(|s| (s.to_string(), true))
            }
        }
    }

    pub(super) fn detect_anon_hash_owner(&self, anon_hash: Node<'a>) -> Option<HashKeyOwner> {
        let mut ancestor = anon_hash.parent()?;
        for _ in 0..5 {
            // Check if this is inside a bless call
            if self.is_bless_call(ancestor) {
                let pkg = self.current_package.clone()
                    .or_else(|| self.scopes[self.current_scope().0 as usize].package.clone());
                if let Some(pkg) = pkg {
                    // Inside a sub: register to `Sub{C, sub_name}` —
                    // that's the actual constructor (or whatever sub
                    // does the blessing). `has`-emitted HashKeyDefs
                    // use the same shape, so a single owner encoding
                    // covers both registration paths and call-site
                    // HashKeyAccess refs (Sub{C, method}) match
                    // strict. Top-level blesses (rare) keep the
                    // coarse `Class(C)` form.
                    return Some(match self.enclosing_sub_name() {
                        Some(name) => HashKeyOwner::Sub { package: Some(pkg), name },
                        None => HashKeyOwner::Class(pkg),
                    });
                }
            }
            // Check if this is inside a return expression of a sub
            if ancestor.kind() == "return_expression" {
                if let Some(name) = self.enclosing_sub_name() {
                    return Some(HashKeyOwner::Sub {
                        package: self.current_package.clone(),
                        name,
                    });
                }
            }
            // Check if this is the last expression in a sub body (implicit return)
            if ancestor.kind() == "expression_statement" {
                if self.is_last_statement_in_sub(ancestor) {
                    if let Some(name) = self.enclosing_sub_name() {
                        return Some(HashKeyOwner::Sub {
                            package: self.current_package.clone(),
                            name,
                        });
                    }
                }
            }
            ancestor = ancestor.parent()?;
        }
        None
    }

    /// Emit a `HashKeyAccess` ref at every odd-indexed (1st, 3rd, …)
    /// stringy arg inside a call's args node, owned by `owner`. In
    /// Perl, `foo(a => 1, "b", 2, c => 3)` is `foo("a", 1, "b", 2,
    /// "c", 3)` — `=>` is just an autoquoting comma. The keys are
    /// the even-position named args, regardless of which separator
    /// comes after them. Mirrors `collect_pair_keys` (callee
    /// side, emits HashKeyDef symbols); this is the caller side, so
    /// `ref_at` on the key token picks the narrow span over the
    /// broad MethodCall/FunctionCall ref. Without this, cursor on
    /// the key in `MooApp->new(name => 'alice')` lands on the
    /// method ref and rename clobbers the wrong token.
    ///
    /// Gated on a matching HashKeyDef already being registered for
    /// `owner`. Otherwise we'd shadow the broader MethodCall ref
    /// for cases the caller-side has no def to anchor on (`class
    /// Foo { field $x :param }` — Point->new(x => 3, …) needs the
    /// MethodCall ref's `find_param_field` fallback in
    /// `find_definition`).
    /// Emit `HashKeyAccess` refs for the keys of a `my %h = (k => …)` literal,
    /// keyed by the hash variable (`var_text = %h`, `owner: None`). The post-walk
    /// owner fixup resolves them to `Variable{%h, def_scope}` — the same owner
    /// the `$h{k}` accesses get — so they group with the accesses for rename.
    pub(super) fn emit_lexical_hash_literal_keys(&mut self, hash_name: &str, rhs: Node<'a>) {
        for (key_node, _value) in crate::cst::pair_nodes(rhs) {
            if !matches!(
                key_node.kind(),
                "bareword" | "autoquoted_bareword" | "string_literal" | "interpolated_string_literal"
            ) {
                continue;
            }
            let Some((key, is_dynamic)) = self.extract_key_text(key_node) else { continue };
            if is_dynamic {
                continue;
            }
            let span = if matches!(key_node.kind(), "string_literal" | "interpolated_string_literal") {
                self.string_content_span(key_node)
            } else {
                node_to_span(key_node)
            };
            self.refs.push(Ref {
                kind: RefKind::HashKeyAccess { var_text: hash_name.to_string() },
                span,
                scope: self.scope_at_point(span.start),
                target_name: key,
                access: AccessKind::Write,
                binding: None,
                folded_from: None,
                arg_count: None,
            });
        }
    }

    pub(super) fn emit_call_arg_key_accesses(&mut self, args_node: Node<'a>, gate: Gate) {
        // Unwrap one level into a hash literal / paren wrapper —
        // search's `{KEY=>...}` is `anonymous_hash_expression`
        // wrapping a `list_expression` of pairs; constructors are
        // paren-wrapped. Even-position iteration expects a flat
        // pair list. Constructor-style args without the wrapper
        // pass through unchanged.
        let mut effective = args_node;
        // A column-keyed verb's column hash is POSITIONALLY the first arg
        // (`search(\%cond, \%attrs)` → `\%cond`; `create(\%cols)`). Narrow to it
        // only when arg 0 is itself a hash literal — a scalar/arrayref cond
        // (`search($cond, \%attrs)`) carries no inline keys, and the trailing
        // `\%attrs` hash (`order_by`/`rows`/…) must never be mistaken for it
        // (picking "first hash among args" would walk it).
        if matches!(gate, Gate::ColumnKeyed(_)) {
            let arg0 = if effective.kind() == "anonymous_hash_expression" {
                Some(effective)
            } else {
                effective.named_child(0)
            };
            match arg0 {
                // Hashref cond/cols (`search({…})`, `create({…})`): narrow to it.
                Some(h) if h.kind() == "anonymous_hash_expression" => effective = h,
                // Flat constructor pairs (`new(name => 1)`): arg 0 is a stringy
                // key — walk the top-level pair list (effective unchanged).
                Some(k) if matches!(
                    k.kind(),
                    "bareword" | "autoquoted_bareword" | "string_literal" | "interpolated_string_literal"
                ) => {}
                // A positional non-key first arg (`search($cond, \%attrs)`): the
                // cond is a prebuilt ref with no inline keys, and the trailing
                // `\%attrs` hash is not column-keyed. Walk nothing.
                _ => return,
            }
        }
        // Pair-walk via the shared `cst::pair_nodes`: Perl tucks the tail pairs
        // of `a => 1, b => 2` into a right-nested `list_expression`, so manual
        // even/odd iteration over the top-level children sees only the FIRST
        // pair. `pair_nodes` flattens the nesting (and skips separators), so
        // every key is walked. (CLAUDE.md: don't re-derive pair-walking.)
        for (child, _value) in crate::cst::pair_nodes(effective) {
            if !matches!(
                child.kind(),
                "bareword" | "autoquoted_bareword" | "string_literal" | "interpolated_string_literal"
            ) {
                continue;
            }
            let Some((key, is_dynamic)) = self.extract_key_text(child) else { continue };
            if is_dynamic { continue; }
            // Gate decides whether to emit + what owner to record.
            // Strict checks the local symbol table for a matching
            // HashKeyDef (prevents `Foo::bar(name=>1)` from latching
            // onto unrelated `Sub{Foo,new}` keys). Open trusts the
            // caller (the receiver's flavor pinned the owner).
            // Deferred emits with `owner: None`; the post-walk
            // fixup in `fix_chain_receiver_hash_key_owners` fills
            // the owner once the receiver type resolves
            // (in-file or cross-file).
            let owner_to_emit: Option<HashKeyOwner> = match &gate {
                Gate::Strict(owner) => {
                    if !self.has_hash_key_def(&key, owner) { continue; }
                    Some(owner.clone())
                }
                Gate::ColumnKeyed(sub_owner) => {
                    // First-hashref narrowing already happened in `effective`.
                    // A key that's an actual column → the column owner; else fall
                    // back to the `Sub{class,verb}` owner if the verb declares it
                    // (Moo/Corinna ctor keys under a generic-named `new`); else
                    // skip (so `order_by` and friends never latch on).
                    let class = match sub_owner {
                        HashKeyOwner::Sub { package: Some(c), .. } => c.clone(),
                        _ => continue,
                    };
                    let col = HashKeyOwner::Bridged { class };
                    if self.has_hash_key_def(&key, &col) {
                        Some(col)
                    } else if self.has_hash_key_def(&key, sub_owner) {
                        Some(sub_owner.clone())
                    } else {
                        continue;
                    }
                }
                Gate::StrictOrDefer(owner) => {
                    if self.has_hash_key_def(&key, owner) {
                        Some(owner.clone())
                    } else {
                        None
                    }
                }
                Gate::Open(owner) => Some(owner.clone()),
                Gate::Deferred => None,
            };
            let access = self.determine_access(child);
            // `scope_at_point` instead of `current_scope` so this
            // works both inside the walk (function-call args path)
            // and post-walk (method-call args path; scope_stack is
            // empty by then). Equivalent at walk-time because a
            // node's innermost containing scope IS what
            // current_scope would return.
            //
            // A quoted key (`{ "name", 2 }`) must rename its CONTENT, not the
            // whole literal — rewriting the quotes turns it into a bareword
            // (`{ fullname, 2 }`), a `strict subs` error in a plain-comma list.
            let span = if matches!(
                child.kind(),
                "string_literal" | "interpolated_string_literal"
            ) {
                self.string_content_span(child)
            } else {
                node_to_span(child)
            };
            self.refs.push(Ref {
                kind: RefKind::HashKeyAccess {
                    var_text: String::new(),
                },
                span,
                scope: self.scope_at_point(span.start),
                target_name: key,
                access,
                binding: owner_to_emit
                    .map(|owner| crate::model::file_analysis::RefBinding::HashKey { owner, sym: None }),
                folded_from: None,
                arg_count: None,
            });
        }
    }

    /// Strict (no `found_by` broadening) check: is a HashKeyDef
    /// registered with this exact `owner` and `name`? Used by
    /// `emit_call_arg_key_accesses` to gate emission — broadening
    /// would let `Foo::bar(name => 1)` latch onto `name` keys
    /// registered to `Sub{Foo, new}`, which they don't logically
    /// belong to.
    pub(super) fn has_hash_key_def(&self, name: &str, owner: &HashKeyOwner) -> bool {
        self.symbols.iter().any(|s| {
            if s.name != name { return false; }
            matches!(&s.detail, SymbolDetail::HashKeyDef { owner: o, .. } if o == owner)
        })
    }

    /// Emit a `HashKeyDef` symbol for every key in a hash literal owned by
    /// `owner` (e.g. a blessed `{ ... }`). Keys are the even-position elements
    /// of the flat pair sequence — `{ a => 1, 'b', 2 }` is `{ 'a', 1, 'b', 2 }`,
    /// so the separator (`,` vs `=>`) is irrelevant; we pair positionally via
    /// the shared node-level walker and take each key node.
    pub(super) fn collect_pair_keys(&mut self, node: Node<'a>, owner: &HashKeyOwner) -> Vec<String> {
        let mut defs: Vec<(String, Span)> = Vec::new();
        for (k_node, _val) in crate::cst::pair_nodes(node) {
            if matches!(
                k_node.kind(),
                "autoquoted_bareword" | "string_literal" | "interpolated_string_literal"
            ) {
                if let Some((key, is_dynamic)) = self.extract_key_text(k_node) {
                    if !is_dynamic {
                        defs.push((key, node_to_span(k_node)));
                    }
                }
            }
        }
        let keys: Vec<String> = defs.iter().map(|(k, _)| k.clone()).collect();
        for (key, span) in defs {
            self.add_symbol(
                key,
                SymKind::HashKeyDef,
                span,
                span,
                SymbolDetail::HashKeyDef { owner: owner.clone(), is_dynamic: false },
            );
        }
        keys
    }

    /// Find the nearest enclosing Sub or Method scope from the current scope stack.
    pub(super) fn enclosing_sub_scope(&self) -> Option<ScopeId> {
        for &scope_id in self.scope_stack.iter().rev() {
            match &self.scopes[scope_id.0 as usize].kind {
                ScopeKind::Sub { .. } | ScopeKind::Method { .. } => return Some(scope_id),
                _ => {}
            }
        }
        None
    }

    /// Get the name of the enclosing sub/method, if any.
    pub(super) fn enclosing_sub_name(&self) -> Option<String> {
        let scope_id = self.enclosing_sub_scope()?;
        match &self.scopes[scope_id.0 as usize].kind {
            ScopeKind::Sub { ref name } | ScopeKind::Method { ref name } => Some(name.clone()),
            _ => None,
        }
    }

    /// Check if a node is the last statement in a sub/method body block.
    pub(super) fn is_last_statement_in_sub(&self, node: Node<'a>) -> bool {
        let parent = match node.parent() {
            Some(p) => p,
            None => return false,
        };
        // The parent should be a block that is a sub/method body
        if parent.kind() != "block" {
            return false;
        }
        if let Some(grandparent) = parent.parent() {
            if !matches!(grandparent.kind(),
                "subroutine_declaration_statement" | "method_declaration_statement"
                | "anonymous_subroutine_expression"
            ) {
                return false;
            }
        } else {
            return false;
        }
        // Check this is the last named child in the block
        if let Some(last) = parent.named_child(parent.named_child_count().saturating_sub(1)) {
            last.id() == node.id()
        } else {
            false
        }
    }
}
