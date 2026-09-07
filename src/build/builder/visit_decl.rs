//! Declaration visitors: the `visit_node` dispatcher, ERROR recovery,
//! packages/classes/subs, parameter extraction, variable decls and loops.

use super::*;

impl<'a> Builder<'a> {
    // ---- Main visitor ----

    pub(super) fn visit_node(&mut self, node: Node<'a>) {
        match node.kind() {
            "package_statement" => self.visit_package(node),
            "class_statement" => self.visit_class(node),
            "subroutine_declaration_statement" => self.visit_sub(node, false),
            "anonymous_subroutine_expression" => self.visit_anonymous_sub(node),
            "method_declaration_statement" => self.visit_sub(node, true),
            "variable_declaration" => self.visit_variable_decl(node),
            "for_statement" => self.visit_for(node),
            "postfix_for_expression" => self.visit_postfix_for(node),
            "use_statement" => self.visit_use(node),
            "require_expression" => self.visit_require(node),
            "assignment_expression" => self.visit_assignment(node),

            // A bare `{ ... }` statement is its own block node in this
            // grammar (no separate `block` child), so the `block` arm below
            // never sees it. It is both a package boundary (`{ package
            // Inner; }` must not leak Inner past the close) and a lexical
            // scope (a `my` inside must not shadow past the close).
            "block_statement" => {
                self.add_fold_range(node);
                self.push_scope(ScopeKind::Block, node_to_span(node), None);
                self.walk_block_package_scoped_then(node, |b| { b.pop_scope(); });
            }

            // Blocks create scopes (but only standalone blocks, not sub/class/for bodies)
            "block" | "do_block" => {
                // Only create a Block scope if parent isn't already a scope-creator
                let parent_kind = node.parent().map(|p| p.kind()).unwrap_or("");
                if !matches!(parent_kind,
                    "subroutine_declaration_statement" | "method_declaration_statement" |
                    "class_statement" | "for_statement" | "foreach_statement" |
                    "varname" // block-deref: @{expr}, %{expr}, &{expr}
                ) {
                    self.add_fold_range(node);
                    self.push_scope(ScopeKind::Block, node_to_span(node), None);
                    self.walk_block_package_scoped_then(node, |b| { b.pop_scope(); });
                    return;
                }
                self.add_fold_range(node);
                self.queue_children(node);
            }

            // Foldable statements
            "if_statement" | "unless_statement" | "while_statement" | "until_statement" => {
                self.add_fold_range(node);
                self.queue_children(node);
            }

            // Flow-sensitive narrowing: a block guard refines the then-block;
            // a statement-level exit guard refines the rest of the block.
            "conditional_statement" => {
                self.narrow_block_guard(node);
                self.queue_children(node);
            }
            "postfix_conditional_expression" => {
                self.narrow_postfix_exit(node);
                self.queue_children(node);
            }
            "lowprec_logical_expression" => {
                self.narrow_logical_exit(node);
                self.queue_children(node);
            }

            // Variable references
            "scalar" | "array" | "hash" => self.visit_var_ref(node),
            "container_variable" | "slice_container_variable" | "keyval_container_variable" => {
                self.visit_container_ref(node);
            }
            // $#foo — scalar-shaped but resolves to the underlying @foo.
            // The sigil is `$#`; the varname child holds the bare name.
            "arraylen" => self.visit_arraylen_ref(node),

            // Call expressions
            "function_call_expression" | "ambiguous_function_call_expression" => {
                self.visit_function_call(node);
            }
            // Built-in calls: abs($x), length($s), time(), etc.
            "func1op_call_expression" | "func0op_call_expression" => {
                if self.is_shift_call(node) {
                    self.consume_arg_head(node);
                }
                self.visit_func1op(node);
            }
            "method_call_expression" => self.visit_method_call(node),

            // Code-ref capture: `\&handler` or `\&Pkg::handler`.
            // Emits a FunctionCall ref at the name span so goto-def /
            // references resolve to the sub definition. The `expr_payload`
            // path handles the CodeRef type witness; this arm adds the
            // navigation ref that `expr_payload` doesn't emit.
            "refgen_expression" => self.visit_refgen(node),

            // Hash access
            "hash_element_expression" => self.visit_hash_element(node),

            // Dereference expressions → type constraints on operand
            "array_element_expression" => {
                self.infer_deref_type(node, InferredType::ArrayRef);
                // Only the arrow form `$x->[i]` has a scalar-ref receiver; the
                // direct `$arr[i]` indexes the named array.
                if crate::cst::element_arrow_deref(node, self.source) {
                    self.record_arrow_deref(node, crate::model::file_analysis::DerefForm::ArrayIndex);
                }
                self.queue_children(node);
            }
            "coderef_call_expression" => {
                // Walk-time: just narrow the operand to CodeRef.
                // The callable-return propagation onto this call's
                // value-type happens at *query* time — `invocant_
                // type_at_node`'s `coderef_call_expression` arm
                // chases the operand's `CodeRef.return_edge`
                // through the bag every time it's asked. Chain
                // typing already re-asks on each worklist
                // iteration, so monotone refinement of the
                // operand's TC lifts the call's type for free as
                // the lattice settles. No witness emission here;
                // no post-walk pass.
                self.infer_deref_type(node, InferredType::CodeRef { return_edge: None });
                self.record_arrow_deref(node, crate::model::file_analysis::DerefForm::Call);
                self.queue_children(node);
            }
            // Symbolic code-deref: `&{ EXPR }` / `&{ EXPR }(...)`. The operand
            // (the EXPR inside the block) is a coderef. Narrow it, then visit
            // children so the inner scalar still gets its read ref.
            "code_deref_expression" => {
                if let Some(operand) = code_deref_operand(node) {
                    if let Some(existing) = self.invocant_type_at_node(operand) {
                        if !existing.subsumes_narrowing(&InferredType::CodeRef { return_edge: None }) {
                            self.push_var_type_constraint(
                                operand,
                                node,
                                InferredType::CodeRef { return_edge: None },
                            );
                        }
                    } else {
                        self.push_var_type_constraint(
                            operand,
                            node,
                            InferredType::CodeRef { return_edge: None },
                        );
                    }
                }
                self.queue_children(node);
            }
            "array_deref_expression" => {
                self.infer_deref_type(node, InferredType::ArrayRef);
                self.queue_children(node);
            }
            "hash_deref_expression" => {
                self.infer_deref_type(node, InferredType::HashRef);
                self.queue_children(node);
            }

            // Binary operators → type constraints on variable operands
            "binary_expression" => {
                self.infer_binary_op_type(node);
                self.queue_children(node);
            }
            "equality_expression" | "relational_expression" => {
                self.infer_comparison_type(node);
                self.queue_children(node);
            }

            // Unary operators
            "postinc_expression" | "preinc_expression" => {
                // $x++ / $x-- / ++$x / --$x → Numeric
                if let Some(operand) = node.named_child(0) {
                    self.push_var_type_constraint(operand, node, InferredType::Numeric);
                }
                self.queue_children(node);
            }

            // Return expressions → record structural facts pre-visit
            // (scope, arity branch, body span); emit per-expression +
            // per-sub witnesses POST visit_children so the body's refs
            // are already allocated. `expr_payload` for method-call
            // bodies returns `Edge(Expression(refidx))`; finding
            // refidx requires the ref to exist, which requires the
            // walker to have visited the method-call expression.
            "return_expression" => {
                let body_span = node.named_child(0).map(node_to_span);
                let scope = self.enclosing_sub_scope();
                if let Some(scope) = scope {
                    let arity_branch = classify_arity_branch(node, self.source);
                    self.return_infos.push(ReturnInfo {
                        scope,
                        arity_branch,
                        body_span,
                    });
                    // If the return body is `return other()` (a direct call),
                    // record the delegation so hash-key ownership can walk
                    // through the intermediate.
                    if let Some(sub_name) = self.enclosing_sub_name() {
                        if let Some(delegated) = extract_delegated_call_name(node, self.source) {
                            self.sub_return_delegations.insert(sub_name, delegated);
                        }
                    }
                }
                self.queue_children_then(node, move |b| {
                    if let Some(scope) = scope {
                        b.publish_return_arm_witnesses(node, scope);
                    }
                });
            }

            // Expression statements inside sub bodies → track last
            // expression's body span for the implicit-return path.
            // Perl returns the last statement's value, so this IS
            // the sub's implicit return when there's no explicit
            // `return`. Each top-level statement we visit overwrites
            // the prior entry, so when the walk leaves the sub the
            // map points at the genuinely-last statement.
            //
            // IMPORTANT: only statements at the sub body's TOP
            // level count — the outer block must be the sub/method's
            // direct body. The bag-routed delegation chain (`Symbol(_) ←
            // branch_arm Edge → Expr(body) → Edge(call_target)`) handles
            // self-method tails for type inference; no separate map.
            "expression_statement" => {
                self.queue_children_then(node, move |b| {
                    let Some(scope) = b.enclosing_sub_scope() else { return };
                    let is_body_top_level = node
                        .parent()
                        .filter(|p| p.kind() == "block")
                        .and_then(|block| block.parent())
                        .map(|gp| {
                            matches!(
                                gp.kind(),
                                "subroutine_declaration_statement"
                                    | "method_declaration_statement"
                                    | "anonymous_subroutine_expression"
                            )
                        })
                        .unwrap_or(false);
                    if !is_body_top_level {
                        return;
                    }
                    if let Some(child) = node.named_child(0) {
                        // Make sure the expression has Expr(span)
                        // witnesses populated — `bag_query_expr_span`
                        // resolves through them in the implicit-return
                        // fallback. No-op for compound nodes whose
                        // payload doesn't bake to a witness shape.
                        b.emit_expr_witness(child);
                        b.last_expr_span.insert(scope, node_to_span(child));
                        // Classify how this statement exits, using the SAME
                        // undef vocabulary the return side uses. A `return`
                        // reaches here too — the grammar wraps it in an
                        // `expression_statement` like any other — while a
                        // postfix-conditional return (`return undef if $x`)
                        // is a `postfix_conditional_expression`, NOT a
                        // `return_expression`, which is the right answer:
                        // the sub can still fall past it.
                        use crate::model::witnesses::tags::UndefArm;
                        let exit = match child.kind() {
                            "return_expression" => TailExit::Return,
                            "undef_expression" => TailExit::Undef(UndefArm::Scalar),
                            "stub_expression" => TailExit::Undef(UndefArm::EmptyList),
                            _ => TailExit::Value,
                        };
                        b.last_stmt_exit.insert(scope, exit);
                    }
                });
            }

            // Standalone bareword usage of a `use constant` name:
            // `my $n = MAX_RETRIES`, `foo(TIMEOUT)`, `$n > MAX_RETRIES`.
            // The def is a local parameterless Sub symbol; emit a FunctionCall
            // ref so goto-def reaches it and references lists the usage (rule
            // #7). Recognized by membership in `declared_constants`, never by
            // name pattern (rule #10). The call-name position (`MAX_RETRIES()`)
            // is already reffed by `visit_function_call`; skip it here so the
            // narrowest-span ref isn't duplicated.
            "bareword" => self.visit_const_usage(node),

            // Hash construction
            "anonymous_hash_expression" => self.visit_anon_hash(node),

            // POD blocks: collect text for tail-POD post-pass
            "pod" => {
                if let Ok(text) = node.utf8_text(self.source) {
                    self.pod_texts.push(text.to_string());
                }
            }

            // Descend so interpolated variables inside the pattern still get
            // refs. No semantic token is emitted for the literal itself — see
            // `FileAnalysis::semantic_tokens` (#63).
            "quoted_regexp" | "match_regexp" => {
                self.queue_children(node);
            }
            "substitution_regexp" => {
                // `s///e`: the replacement is Perl *code*, but the grammar
                // emits it as a plain `replacement` node (not parsed as code).
                // Re-parse it so calls/vars inside resolve (rule #7), mapping
                // spans back to the file. Same idea as the __END__/ISA re-parses.
                let repl = self.subst_replacement_is_eval(node).then(|| {
                    (0..node.named_child_count())
                        .filter_map(|i| node.named_child(i))
                        .find(|c| c.kind() == "replacement")
                }).flatten();
                if let Some(repl) = repl {
                    self.emit_refs_in_eval_replacement(repl);
                }
                self.queue_children(node);
            }

            // ERROR nodes: recover structural declarations (the file's skeleton)
            // but skip expressions/refs which are unreliable inside broken regions
            "ERROR" => self.recover_structural_from_error(node),

            _ => self.queue_children(node),
        }
    }

    /// Recover structural declarations from ERROR nodes.
    /// Only recovers the file's skeleton (packages, imports, subs, classes) —
    /// expressions and refs inside ERROR are unreliable and skipped.
    pub(super) fn recover_structural_from_error(&mut self, error_node: Node<'a>) {
        // Queued, not called: a malformed file is exactly where nesting depth
        // is least predictable, so ERROR recovery must not stay the one
        // island that still grows the native stack per level.
        let mut steps: Vec<Box<dyn FnOnce(&mut Builder<'a>) + 'a>> = Vec::new();
        for i in 0..error_node.child_count() {
            if let Some(child) = error_node.child(i) {
                match child.kind() {
                    "package_statement" => steps.push(Box::new(move |b: &mut Builder<'a>| b.visit_package(child))),
                    "use_statement" => steps.push(Box::new(move |b: &mut Builder<'a>| b.visit_use(child))),
                    "subroutine_declaration_statement" => {
                        steps.push(Box::new(move |b: &mut Builder<'a>| b.visit_sub(child, false)))
                    }
                    "method_declaration_statement" => {
                        steps.push(Box::new(move |b: &mut Builder<'a>| b.visit_sub(child, true)))
                    }
                    "class_statement" => steps.push(Box::new(move |b: &mut Builder<'a>| b.visit_class(child))),
                    "ambiguous_function_call_expression" => {
                        steps.push(Box::new(move |b: &mut Builder<'a>| b.visit_function_call(child)))
                    }
                    "ERROR" => steps.push(Box::new(move |b: &mut Builder<'a>| {
                        b.recover_structural_from_error(child)
                    })),
                    _ => {}
                }
            }
        }
        // Token-stream bleed insurance: when a mis-lexed string (`"${@}…"`,
        // an unterminated heredoc, etc.) swallows the closing delimiter, the
        // bleed dissolves every `sub`/`method` declaration in the trailing
        // region into stray tokens — they survive neither as
        // subroutine_declaration_statement children (the structural loop
        // above) nor anywhere else in the tree. The file is "decapitated":
        // a 36-sub module indexes 0 subs, cascading to total loss of
        // inherited goto-def/references across subclasses. Recover those
        // declarations from raw source text inside the ERROR span. Generic:
        // gated only on "we are inside a parse ERROR", never on any module.
        // See docs/parser-shortcomings.md (G7 — `"${@}"` bleed) and
        // docs/adr/error-recovery.md. KLUDGE: removable once upstream fixes G7.
        steps.push(Box::new(move |b: &mut Builder<'a>| {
            b.recover_subs_from_error_text(error_node)
        }));
        self.queue_sequence(steps);
    }

    /// Source-text fallback for declarations a token-stream bleed destroyed.
    /// Scans the ERROR node's raw bytes for statement-position `sub NAME` /
    /// `method NAME` and synthesizes minimal Sub/Method symbols for any not
    /// already captured (structurally, or on this row). Only declarations
    /// inside the ERROR span are considered — outside it tree-sitter parsed
    /// correctly and owns the symbol.
    ///
    /// KLUDGE (medium, re-evaluate) — this is a regex-ish raw-byte rescue for
    /// the `"${@}"` block-interp lexer bleed (docs/parser-shortcomings.md G7).
    /// It only recovers the declaration *skeleton* (params/return/POD are lost
    /// because the bodies are dissolved). It exists only because the bug
    /// dissolves subs entirely rather than leaving them as ERROR children, so
    /// the normal structural recovery can't see them. **Delete / re-evaluate
    /// once upstream tree-sitter-perl fixes the `"${@}"` lex** — at that point
    /// the subs parse normally and this byte-scan is dead weight (and a latent
    /// source of false-positive "sub" matches in pathological ERROR text).
    pub(super) fn recover_subs_from_error_text(&mut self, error_node: Node<'a>) {
        let start_row = error_node.start_position().row;
        let start_col = error_node.start_position().column;
        let text = match error_node.utf8_text(self.source) {
            Ok(t) => t,
            Err(_) => return,
        };
        let pkg_is_subclass = self.current_package
            .as_ref()
            .map_or(false, |p| self.package_parents.contains_key(p));

        for (line_off, line) in text.lines().enumerate() {
            let row = start_row + line_off;
            // The ERROR text's first line starts at the node's column; later
            // lines start at column 0. Offset the recovered spans so they map
            // back to true file columns.
            let col_base = if line_off == 0 { start_col } else { 0 };
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            let (kw_is_method, rest) = if let Some(r) = trimmed.strip_prefix("sub ") {
                (false, r)
            } else if let Some(r) = trimmed.strip_prefix("method ") {
                (true, r)
            } else {
                continue;
            };
            let name_pad = rest.len() - rest.trim_start().len();
            let rest = rest.trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            // The next non-space char after the name must look like a
            // declaration: `{` (body), `(` (signature/prototype), `:`
            // (attribute), `;` (forward decl), or EOL. Filters bareword noise
            // like `$x->sub foo` that is not a real declaration.
            let after = rest[name.len()..].trim_start();
            let looks_decl = after.is_empty()
                || after.starts_with('{')
                || after.starts_with('(')
                || after.starts_with(':')
                || after.starts_with(';');
            if !looks_decl {
                continue;
            }

            // Dedup: skip if a Sub/Method symbol already lands on this row
            // (recovered structurally above, or by an overlapping ERROR).
            if self.symbols.iter().any(|s| {
                matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && s.selection_span.start.row == row
            }) {
                continue;
            }

            let kw_len = if kw_is_method { "method ".len() } else { "sub ".len() };
            let name_col = col_base + indent + kw_len + name_pad;
            let name_span = Span {
                start: Point { row, column: name_col },
                end: Point { row, column: name_col + name.len() },
            };
            // The bleed makes the true body extent unknowable; a single-line
            // declaration span is enough for goto-def / references / outline.
            let decl_span = Span {
                start: Point { row, column: col_base + indent },
                end: Point { row, column: col_base + line.len() },
            };
            let is_method = kw_is_method || pkg_is_subclass;
            self.add_symbol(
                name,
                if is_method { SymKind::Method } else { SymKind::Sub },
                decl_span,
                name_span,
                SymbolDetail::Sub {
                    params: Vec::new(),
                    is_method,
                    doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                },
            );
        }
    }

    /// Walk a `{ ... }` block's children, reverting package context at block
    /// close. `package Foo;` is file-scoped in Perl, but a `{ }` block is a
    /// hard boundary: `{ package Inner; }` must not leak Inner to the
    /// statements that follow. Saves the walk-time package name and the open
    /// statement-range cursor, restores both on exit, and repairs the
    /// `package_ranges` spans so `package_at` reverts past the block too.
    pub(super) fn walk_block_package_scoped(&mut self, node: Node<'a>) {
        self.walk_block_package_scoped_then(node, |_| {});
    }

    /// `walk_block_package_scoped`, with `work` run after the package context
    /// is restored — the block-scope arm closes its lexical scope there.
    pub(super) fn walk_block_package_scoped_then(
        &mut self,
        node: Node<'a>,
        work: impl FnOnce(&mut Builder<'a>) + 'a,
    ) {
        let saved_pkg = self.current_package.clone();
        let saved_stmt_range = self.open_statement_package;
        self.queue_children_then(node, move |b| {
            if b.open_statement_package != saved_stmt_range {
                // A `package Inner;` inside the block opened a range to file-end;
                // trim it to block close so it doesn't shadow the enclosing pkg.
                if let Some(idx) = b.open_statement_package {
                    b.package_ranges[idx].span.end = node.end_position();
                }
                // The enclosing `package Outer;` range (if any) was truncated to
                // Inner's start when Inner opened — resume it to file-end so
                // Outer covers the post-block tail.
                if let Some(idx) = saved_stmt_range {
                    let file_end = b
                        .scope_stack
                        .first()
                        .map(|id| b.scopes[id.0 as usize].span.end)
                        .unwrap_or_else(|| node.end_position());
                    b.package_ranges[idx].span.end = file_end;
                }
                b.open_statement_package = saved_stmt_range;
            }
            b.current_package = saved_pkg;
            work(b);
        });
    }

    // ---- Node visitors ----

    pub(super) fn visit_package(&mut self, node: Node<'a>) {
        let name = match node.child_by_field_name("name") {
            Some(n) => match n.utf8_text(self.source) {
                Ok(s) => s.to_string(),
                Err(_) => return,
            },
            None => return,
        };
        let name_node = node.child_by_field_name("name").unwrap();
        // Capture the pre-existing package BEFORE touching
        // current_package — the block form needs to restore to this
        // value after visiting children. The previous code took()
        // from current_package AFTER already mutating it, so
        // prev_package would hold the NEW value and the restore was
        // a no-op. That leaked `package Foo { ... }`'s name to
        // every statement that followed at the same file scope.
        let prev_package = self.current_package.clone();

        self.add_symbol(
            name.clone(),
            SymKind::Package,
            node_to_span(node),
            node_to_span(name_node),
            SymbolDetail::None,
        );

        let has_block = (0..node.child_count())
            .any(|i| node.child(i).map_or(false, |c| c.kind() == "block"));
        if has_block {
            // `package Foo { ... }` — record the block as a package
            // range and set current_package for the walk, then
            // restore. The block doesn't push a lexical scope on its
            // own (children will, e.g. via subs/methods inside).
            self.add_fold_range(node);
            self.push_block_package_range(name.clone(), node_to_span(node));
            self.current_package = Some(name);
            self.queue_children_then(node, move |b| b.current_package = prev_package);
        } else {
            // `package Foo;` — package context flows to the next
            // sibling `package X;` / `class X;` or end of file.
            // `package_ranges` carries that for `package_at`; the
            // walk-time `current_package` drives synthesised
            // sub/method packages. No lexical scope is pushed —
            // `package Foo;` is not a lexical boundary in Perl.
            self.current_package = Some(name.clone());
            self.open_statement_package_range(name, node.start_position());
        }
    }

    pub(super) fn visit_class(&mut self, node: Node<'a>) {
        let name_node = match node.child_by_field_name("name") {
            Some(n) => n,
            None => return,
        };
        let name = match name_node.utf8_text(self.source) {
            Ok(s) => s.to_string(),
            Err(_) => return,
        };

        // Parse :isa and :does
        let mut parent = None;
        let mut roles = Vec::new();
        if let Some(attrlist) = node.child_by_field_name("attributes") {
            for i in 0..attrlist.named_child_count() {
                if let Some(attr) = attrlist.named_child(i) {
                    if attr.kind() == "attribute" {
                        let attr_name = attr.child_by_field_name("name")
                            .and_then(|n| n.utf8_text(self.source).ok());
                        let attr_value = attr.child_by_field_name("value")
                            .and_then(|n| n.utf8_text(self.source).ok());
                        match (attr_name, attr_value) {
                            (Some("isa"), Some(val)) => parent = Some(val.to_string()),
                            (Some("does"), Some(val)) => roles.push(val.to_string()),
                            _ => {}
                        }
                    }
                }
            }
        }

        // Collect fields from the block for the Class detail
        let mut field_details = Vec::new();
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "block" {
                    self.collect_field_details(child, &mut field_details);
                }
            }
        }

        // Write to package_parents for unified inheritance resolution
        if let Some(ref p) = parent {
            self.package_parents
                .entry(name.clone())
                .or_default()
                .push(p.clone());
        }
        // Roles via :does(Role) are also parents for method resolution
        if !roles.is_empty() {
            self.package_parents
                .entry(name.clone())
                .or_default()
                .extend(roles.iter().cloned());
        }

        self.add_symbol(
            name.clone(),
            SymKind::Class,
            node_to_span(node),
            node_to_span(name_node),
            SymbolDetail::Class {
                parent,
                roles,
                fields: field_details,
            },
        );

        let has_block = node.child_by_field_name("body").is_some()
            || (0..node.child_count()).any(|i| node.child(i).map_or(false, |c| c.kind() == "block"));

        if has_block {
            // Block class: push/pop scope, restore package after block
            self.add_fold_range(node);
            let prev_package = self.current_package.take();
            self.push_block_package_range(name.clone(), node_to_span(node));
            self.current_package = Some(name.clone());
            self.push_scope(ScopeKind::Class { name: name.clone() }, node_to_span(node), Some(name));
            self.queue_children_then(node, move |b| {
                b.pop_scope();
                b.current_package = prev_package;
            });
        } else {
            // Flat `class Foo;` — same semantics as non-block
            // `package Foo;`: package context flows in
            // `package_ranges`; no lexical scope is pushed. The
            // Class SYMBOL was already emitted above.
            self.current_package = Some(name.clone());
            self.open_statement_package_range(name, node.start_position());
        }
    }

    pub(super) fn collect_field_details(&self, block: Node<'a>, out: &mut Vec<FieldDetail>) {
        for i in 0..block.child_count() {
            if let Some(child) = block.child(i) {
                if child.kind() == "expression_statement" {
                    if let Some(fd) = self.try_parse_field_detail(child) {
                        out.push(fd);
                    }
                }
            }
        }
    }

    pub(super) fn try_parse_field_detail(&self, expr_stmt: Node<'a>) -> Option<FieldDetail> {
        for i in 0..expr_stmt.named_child_count() {
            let child = expr_stmt.named_child(i)?;
            let var_decl = if child.kind() == "variable_declaration" {
                child
            } else if child.kind() == "assignment_expression" {
                child.child_by_field_name("left").filter(|n| n.kind() == "variable_declaration")?
            } else {
                continue;
            };

            let keyword = self.get_decl_keyword(var_decl)?;
            if keyword != "field" {
                return None;
            }

            let var_node = var_decl.child_by_field_name("variable")?;
            let full_name = var_node.utf8_text(self.source).ok()?;
            let sigil = full_name.chars().next()?;

            let mut attributes = Vec::new();
            if let Some(attrlist) = var_decl.child_by_field_name("attributes") {
                for j in 0..attrlist.named_child_count() {
                    if let Some(attr) = attrlist.named_child(j) {
                        if attr.kind() == "attribute" {
                            if let Some(name_node) = attr.child_by_field_name("name") {
                                if let Ok(attr_name) = name_node.utf8_text(self.source) {
                                    attributes.push(attr_name.to_string());
                                }
                            }
                        }
                    }
                }
            }

            return Some(FieldDetail {
                name: full_name.to_string(),
                sigil,
                attributes,
            });
        }
        None
    }

    pub(super) fn visit_sub(&mut self, node: Node<'a>, is_method: bool) {
        let name_node = match node.child_by_field_name("name") {
            Some(n) => n,
            None => { self.queue_children(node); return; }
        };
        let name = match name_node.utf8_text(self.source) {
            Ok(s) => s.to_string(),
            Err(_) => { self.queue_children(node); return; }
        };

        // A bodyless `sub NAME;` / `method NAME;` is a forward declaration, not
        // a definition (ts-parser-perl 1.1.1 parses these as real decl
        // statements). Emitting a symbol would duplicate the real definition in
        // the outline and shadow it in goto-def with a body-less target. Skip
        // it — the navigable symbol is the actual definition (in this file,
        // cross-file, or installed via AUTOLOAD/XS).
        if node.child_by_field_name("body").is_none() {
            return;
        }

        // Extract params
        let mut params = self.extract_params(node);

        // Invocant detection for Perl-native subs:
        //   * `method foo { ... }` (v5.38) is always a method — first
        //     positional is the invocant.
        //   * Regular `sub` bodies use two Perl-native signals:
        //       - first positional named `$self`/`$class`/`$this`/`$proto`
        //       - or the enclosing package declares inheritance (a sub
        //         in a subclass is, by Perl OO convention, a method)
        //     Either triggers invocant marking; name stays free so the
        //     user can call it `$c`/`$ctx`/whatever.
        // Framework-specific invocant markers (`as_invocant_params` from
        // a plugin) stack on top via EmittedParam → ParamInfo.
        if let Some(first) = params.first_mut() {
            let name_says_invocant =
                crate::model::conventions::is_conventional_invocant_name(&first.name);
            let pkg_is_subclass = self.current_package
                .as_ref()
                .map_or(false, |p| self.package_parents.contains_key(p));
            if is_method || name_says_invocant || pkg_is_subclass {
                first.is_invocant = true;
            }
        }

        // Extract preceding POD/comment documentation
        let doc = self.extract_preceding_doc(node, &name);

        // `my sub helper { … }` — the grammar's `lexical` field marks a
        // block-scoped sub: real in-file structure (document symbols
        // keep it) but not a workspace-addressable entity (workspace
        // search drops it).
        let lexical = node.child_by_field_name("lexical").is_some();
        self.add_symbol(
            name.clone(),
            if is_method { SymKind::Method } else { SymKind::Sub },
            node_to_span(node),
            node_to_span(name_node),
            SymbolDetail::Sub { params: params.clone(), is_method, doc, opaque_return: false, is_constant: false, lexical },
        );

        // Exporter::Extensible method-attribute export form: `sub foo :Export`.
        // The sub's name is the export; recognizing the attribute is builder
        // parsing of framework syntax. The attribute can appear before the
        // package's `use Exporter::Extensible` is seen in source order, so we
        // don't gate on package_uses here — `:Export` is unambiguous enough.
        self.detect_export_attribute(node, &name);

        // Importer's advertise hook: a module implements `IMPORTER_MENU` to
        // tell `Importer` (and Exporter::Tiny) its export list. Best-effort:
        // pull the `export` / `export_ok` keys' name arrays from the return
        // list when statically present. `export_anon` (name → coderef) and
        // any computed menu are unmodeled (runtime).
        if name == "IMPORTER_MENU" {
            self.detect_importer_menu(node);
        }

        // Push sub scope
        let scope_kind = if is_method {
            ScopeKind::Method { name: name.clone() }
        } else {
            ScopeKind::Sub { name: name.clone() }
        };
        self.push_scope(scope_kind, node_to_span(node), None);

        // Record signature params as Variable symbols in the sub scope
        self.record_signature_params(node, &params);

        // Perl 5.38 methods: synthesize implicit $self with type → enclosing class
        if is_method {
            if let Some(pkg) = self.current_package.clone() {
                let span = node_to_span(name_node);
                self.add_symbol(
                    "$self".to_string(),
                    SymKind::Variable,
                    span,
                    span,
                    SymbolDetail::Variable { sigil: '$', decl_kind: DeclKind::Param },
                );
                self.push_type_constraint(TypeConstraint {
                    variable: "$self".to_string(),
                    scope: self.current_scope(),
                    inferred_type: InferredType::ClassName(pkg),
                    constraint_span: span,
                });
            }
        }

        // Detect first-param-is-self pattern
        self.detect_first_param_type(&params, node);

        // Role-contract param typing: a plugin `param_types()` rule may type
        // a named param (e.g. `$app` in a `GenericCo::Upgrade::OneTime` doer's
        // `run_upgrade`). Same mechanism as `detect_first_param_type`.
        self.apply_param_type_manifest(&name, &params, node);

        // Visit children (body, etc.)
        self.queue_children_then(node, |b| { b.pop_scope(); });
    }

    /// Walk an `anonymous_subroutine_expression` (a `sub { ... }`
    /// arg literal). Ensures a Symbol of kind `Sub` exists with the
    /// conventional name `(anon)` so the per-scope arity / return
    /// machinery (`emit_arity_return_witnesses`,
    /// `seed_return_types_from_bag`, plugin-priority writeback) finds
    /// the sub uniformly with named subs — no special-case for "this
    /// is an anonymous body." The Symbol's `span` matches the scope's
    /// span exactly so `find_sub_symbol_for_scope` resolves by
    /// containment, and `coderef_return_edge_for` can stash a
    /// `Symbol(sym_id)` edge for the value-side `CodeRef.return_edge`
    /// on `\&foo`-equivalent invocations.
    ///
    /// Symbol creation goes through `ensure_anon_sub_symbol` which
    /// is idempotent — `expr_payload` may have already created the
    /// symbol when handling `my $cb = sub {...}` (the assignment's
    /// RHS extraction runs before the walker descends into the
    /// body), so visit-time creation must be a no-op in that case.
    /// Multiple anon subs all share the name `(anon)`; uniqueness
    /// is by SymbolId, not name. Cross-file lookup by name is
    /// suppressed by the `(` prefix (Perl identifiers can't start
    /// with a paren), so a workspace search for `(anon)` won't
    /// surface them.
    pub(super) fn visit_anonymous_sub(&mut self, node: Node<'a>) {
        let mut params = self.extract_params(node);
        // If the caller set `modifier_invocant_pos` (from `around`/`before`/`after`
        // in a Moo/Moose context), mark the designated param as the invocant so
        // `detect_first_param_type` types it as the enclosing class. Consume the
        // position immediately — it applies only to this next anon sub, not to any
        // nested lambdas the body might contain.
        let modifier_pos = self.modifier_invocant_pos.take();
        if let Some(pos) = modifier_pos {
            if let Some(p) = params.get_mut(pos) {
                if p.name.starts_with('$') {
                    p.is_invocant = true;
                }
            }
        }
        let span = node_to_span(node);
        self.ensure_anon_sub_symbol(node, &params);
        self.push_scope(
            ScopeKind::Sub { name: "(anon)".into() },
            span,
            None,
        );
        self.record_signature_params(node, &params);
        self.detect_first_param_type(&params, node);
        self.queue_children_then(node, |b| { b.pop_scope(); });
    }

    /// Ensure an `(anon)` Symbol exists for the given anon-sub node
    /// and return its `SymbolId`. Called by both the rvalue-side
    /// `expr_payload` (so `my $cb = sub {...}`'s TC sees a
    /// `return_edge: Symbol(_)` from the start) and `visit_anonymous_sub`
    /// (covers anon subs that aren't on the rhs of an assignment —
    /// `helper(name => sub {...})`, `Carp::confess(sub {...}->())`,
    /// etc.). Keyed by node span, so re-entries for the same node
    /// return the existing id.
    pub(super) fn ensure_anon_sub_symbol(
        &mut self,
        node: Node<'a>,
        params: &[ParamInfo],
    ) -> SymbolId {
        let span = node_to_span(node);
        if let Some(id) = self.anon_sub_symbol_by_span.get(&span) {
            return *id;
        }
        let sym_id = self.add_symbol(
            "(anon)".into(),
            SymKind::Sub,
            span,
            self.sub_keyword_span(node).unwrap_or(span),
            SymbolDetail::Sub {
                params: params.to_vec(),
                is_method: false,
                doc: None,
                opaque_return: false,
                is_constant: false,
                lexical: false,
            },
        );
        // Not a nameable entity — resolvable, never listed.
        self.presentation_mut(sym_id).hide_in_outline = true;
        self.anon_sub_symbol_by_span.insert(span, sym_id);
        sym_id
    }

    /// Span of the `sub` keyword itself for an anon-sub node — the
    /// natural selection span (where goto-def / hover should land
    /// when targeting the value, not the body). Returns None if the
    /// keyword is missing (incomplete-source recovery wraps anon
    /// subs in ERROR nodes).
    pub(super) fn sub_keyword_span(&self, node: Node<'a>) -> Option<Span> {
        for i in 0..node.child_count() {
            if let Some(c) = node.child(i) {
                if c.kind() == "sub" {
                    return Some(node_to_span(c));
                }
            }
        }
        None
    }

    pub(super) fn extract_params(&self, sub_node: Node<'a>) -> Vec<ParamInfo> {
        // Try signature syntax first
        for i in 0..sub_node.child_count() {
            if let Some(sig) = sub_node.child(i) {
                if sig.kind() == "signature" {
                    return self.extract_signature_params(sig);
                }
            }
        }

        // Fallback: scan body for shift, @_, and $_[N] patterns
        if let Some(body) = sub_node.child_by_field_name("body") {
            let mut shift_params: Vec<ParamInfo> = Vec::new();

            for i in 0..body.named_child_count() {
                let stmt = match body.named_child(i) {
                    Some(s) => s,
                    None => continue,
                };
                let assign = if stmt.kind() == "expression_statement" {
                    stmt.named_child(0).filter(|n| n.kind() == "assignment_expression")
                } else if stmt.kind() == "assignment_expression" {
                    Some(stmt)
                } else {
                    None
                };
                let assign = match assign {
                    Some(a) => a,
                    None => break, // stop at first non-assignment statement
                };

                if let Some(right) = assign.child_by_field_name("right") {
                    // Pattern: my (...) = @_
                    if right.utf8_text(self.source).ok() == Some("@_") {
                        if let Some(left) = assign.child_by_field_name("left") {
                            let at_params: Vec<ParamInfo> = self.collect_vars_from_decl(left)
                                .into_iter()
                                .map(|(name, _)| {
                                    let is_slurpy = name.starts_with('@') || name.starts_with('%');
                                    ParamInfo { name, default: None, is_slurpy, is_invocant: false }
                                })
                                .collect();
                            // Combine any preceding shift params with @_ params
                            if !shift_params.is_empty() {
                                shift_params.extend(at_params);
                                return shift_params;
                            }
                            return at_params;
                        }
                    }

                    // Pattern: my ($a, $b, ...) = (shift, shift, ...) — Mojo's
                    // `my ($self, $name) = (shift, shift)`. Each `shift` binds
                    // the next @_ element, so the LHS vars are positional params
                    // in order. Gate on every RHS element being a shift call so
                    // a real list value (`my ($a,$b) = foo()`) isn't misread.
                    let rhs_elems = assign
                        .child_by_field_name("right")
                        .and_then(|r| self.list_element_nodes(r));
                    let all_shifts = rhs_elems
                        .is_some_and(|elems| !elems.is_empty() && elems.iter().all(|c| self.is_shift_call(*c)));
                    if all_shifts {
                        if let Some(left) = assign.child_by_field_name("left") {
                            let list_params: Vec<ParamInfo> = self.collect_vars_from_decl(left)
                                .into_iter()
                                .map(|(name, _)| {
                                    let is_slurpy = name.starts_with('@') || name.starts_with('%');
                                    ParamInfo { name, default: None, is_slurpy, is_invocant: false }
                                })
                                .collect();
                            if !list_params.is_empty() {
                                shift_params.extend(list_params);
                                return shift_params;
                            }
                        }
                    }

                    // Pattern: my $var = shift; or my $var = shift || default; or my $var = shift // default;
                    if let Some((var_name, default)) = self.extract_shift_param(assign, right) {
                        shift_params.push(ParamInfo {
                            name: var_name,
                            default,
                            is_slurpy: false,
                    is_invocant: false,
                        });
                        continue;
                    }

                    // Pattern: my $var = $_[N];
                    if let Some(var_name) = self.extract_subscript_param(assign, right) {
                        shift_params.push(ParamInfo {
                            name: var_name,
                            default: None,
                            is_slurpy: false,
                    is_invocant: false,
                        });
                        continue;
                    }
                }

                // Not a recognized param pattern — stop collecting
                break;
            }

            if !shift_params.is_empty() {
                return shift_params;
            }
        }

        Vec::new()
    }

    /// Extract a shift-based parameter: `my $var = shift` or `my $var = shift || default`.
    pub(super) fn extract_shift_param(&self, assign: Node<'a>, right: Node<'a>) -> Option<(String, Option<String>)> {
        let (shift_node, default) = if self.is_shift_call(right) {
            (right, None)
        } else if right.kind() == "binary_expression" {
            // my $var = shift || default  or  my $var = shift // default
            let op = self.get_operator_text(right);
            if matches!(op.as_deref(), Some("||" | "//")) {
                let lhs = right.named_child(0)?;
                if self.is_shift_call(lhs) {
                    let default_node = right.named_child(1)?;
                    let default_text = default_node.utf8_text(self.source).ok()?.to_string();
                    (lhs, Some(default_text))
                } else {
                    return None;
                }
            } else {
                return None;
            }
        } else {
            return None;
        };
        let _ = shift_node;

        // Get variable name from LHS
        let left = assign.child_by_field_name("left")?;
        let var_name = self.get_var_text_from_lhs(left)?;
        Some((var_name, default))
    }

    /// Extract a $_[N]-based parameter: `my $var = $_[N]`.
    pub(super) fn extract_subscript_param(&self, assign: Node<'a>, right: Node<'a>) -> Option<String> {
        if right.kind() != "array_element_expression" {
            return None;
        }
        // Check that it's $_ (container_variable for @_) being subscripted
        let container = right.named_child(0)?;
        if container.kind() != "container_variable" {
            return None;
        }
        // container_variable text is "$_" for @_ subscript
        let ct = container.utf8_text(self.source).ok()?;
        if ct != "$_" {
            return None;
        }
        let left = assign.child_by_field_name("left")?;
        self.get_var_text_from_lhs(left)
    }

    /// Check if a node is a `shift` call (bare or with parens).
    pub(super) fn is_shift_call(&self, node: Node<'a>) -> bool {
        match node.kind() {
            "bareword" => node.utf8_text(self.source).ok() == Some("shift"),
            "func1op_call_expression" => {
                node.named_child_count() == 0
                    && node.child(0).and_then(|c| c.utf8_text(self.source).ok()) == Some("shift")
            }
            "ambiguous_function_call_expression" | "function_call_expression" => {
                use crate::cst::NodeExt;
                node.field_text("function", self.source) == Some("shift")
            }
            _ => false,
        }
    }

    pub(super) fn extract_signature_params(&self, sig: Node<'a>) -> Vec<ParamInfo> {
        let mut params = Vec::new();
        for j in 0..sig.named_child_count() {
            if let Some(param) = sig.named_child(j) {
                match param.kind() {
                    "mandatory_parameter" => {
                        if let Some(var) = self.first_var_child(param) {
                            params.push(ParamInfo { name: var, default: None, is_slurpy: false, is_invocant: false });
                        }
                    }
                    "optional_parameter" => {
                        let var = self.first_var_child(param);
                        let default = param.child_by_field_name("default")
                            .or_else(|| {
                                let nc = param.named_child_count();
                                if nc >= 2 { param.named_child(nc - 1) } else { None }
                            })
                            .and_then(|d| d.utf8_text(self.source).ok())
                            .map(|s| s.to_string());
                        if let Some(name) = var {
                            params.push(ParamInfo { name, default, is_slurpy: false, is_invocant: false });
                        }
                    }
                    "slurpy_parameter" => {
                        if let Some(var) = self.first_var_child(param) {
                            params.push(ParamInfo { name: var, default: None, is_slurpy: true, is_invocant: false });
                        }
                    }
                    "scalar" | "array" | "hash" => {
                        if let Ok(text) = param.utf8_text(self.source) {
                            let is_slurpy = matches!(param.kind(), "array" | "hash");
                            params.push(ParamInfo { name: text.to_string(), default: None, is_slurpy, is_invocant: false });
                        }
                    }
                    _ => {}
                }
            }
        }
        params
    }

    pub(super) fn record_signature_params(&mut self, sub_node: Node<'a>, params: &[ParamInfo]) {
        // For signature syntax, params come from the signature node
        for i in 0..sub_node.child_count() {
            if let Some(sig) = sub_node.child(i) {
                if sig.kind() == "signature" {
                    let mut param_idx = 0;
                    for j in 0..sig.named_child_count() {
                        if let Some(param_node) = sig.named_child(j) {
                            if param_idx < params.len() {
                                let p = &params[param_idx];
                                let sigil = p.name.chars().next().unwrap_or('$');
                                let decl_kind = DeclKind::Param;
                                self.add_symbol(
                                    p.name.clone(),
                                    SymKind::Variable,
                                    node_to_span(param_node),
                                    node_to_span(param_node),
                                    SymbolDetail::Variable { sigil, decl_kind },
                                );
                                param_idx += 1;
                            }
                        }
                    }
                    return;
                }
            }
        }
        // Legacy params: they'll be picked up as normal variable_declaration nodes
    }

    /// Apply plugin `param_types()` rules to a sub declaration: for every rule
    /// whose `method` matches (or is `None` = any method) and whose named param
    /// is present, emit a `ReceiverGated` typed TC gated on the enclosing
    /// package's `isa` the rule's `in_role`. The gate is NOT checked here — the
    /// builder is index-free (rule #1), so a class whose `in_role` ancestor is
    /// reachable only cross-file can't be confirmed at parse time. Resolution
    /// is deferred to query time (`FileAnalysis::gated_param_type_for`), where
    /// the module index walks the ancestry cross-file. Called inside the sub
    /// scope (like `detect_first_param_type`).
    pub(super) fn apply_param_type_manifest(&mut self, method: &str, params: &[ParamInfo], node: Node<'a>) {
        // No rules → skip the alloc every sub declaration would otherwise pay.
        if self.param_type_manifest.is_empty() && self.param_type_wildcards.is_empty() {
            return;
        }
        // Action attributes (`:Local`, `:Chained`, `:Args`, …) are the only
        // honest signal that a controller sub is a dispatch *action* — the slot
        // that actually receives `$c`. `requires_action_attr` rules gate on it.
        // Skip the CST walk + Vec alloc entirely when no loaded rule uses it.
        let has_action_attr = self.any_requires_action_attr
            && node
                .child_by_field_name("attributes")
                .map_or(false, |a| a.named_child_count() > 0);

        // Collect (variable, gate-class, type-class, from_loader) before
        // mutating self — can't hold the manifest borrow while pushing
        // into `gated_param_types`.
        let mut to_gate: Vec<(String, String, String, bool)> = Vec::new();

        // Named rules: only those keyed to exactly this method name.
        if let Some(rules) = self.param_type_manifest.get(method) {
            Self::collect_param_type_matches(rules, params, has_action_attr, method, &mut to_gate);
        }

        // Wildcard rules: method is None — apply to every sub in the class.
        let wildcards = std::mem::take(&mut self.param_type_wildcards);
        Self::collect_param_type_matches(&wildcards, params, has_action_attr, method, &mut to_gate);
        self.param_type_wildcards = wildcards;

        let scope = self.current_scope();
        let span = node_to_span(node);
        for (variable, in_role, class, from_loader) in to_gate {
            if from_loader {
                // Callee-side marker: the real type arrives at
                // enrichment from caller PluginLoad facts. The static
                // `type_class` still rides the gated path below as the
                // no-caller fallback — structure-dominates-rep picks
                // the gathered shape when both land.
                self.loader_config_params.push(crate::model::file_analysis::LoaderConfigParam {
                    variable: variable.clone(),
                    scope,
                    in_role: in_role.clone(),
                });
            }
            self.gated_param_types.push(crate::model::file_analysis::ReceiverGated::new(
                in_role,
                TypeConstraint {
                    variable,
                    scope,
                    constraint_span: span,
                    inferred_type: InferredType::ClassName(class),
                },
            ));
        }
    }

    /// Inner fold for `apply_param_type_manifest`: collect matching
    /// (variable_name, in_role, type_class) triples into `out`. Pure — no self
    /// borrow, so it can be called while `param_type_wildcards` is temporarily
    /// moved out. The `in_role` gate rides each triple; ancestry is checked at
    /// query time, not here.
    pub(super) fn collect_param_type_matches(
        rules: &[plugin::ParamType],
        params: &[ParamInfo],
        has_action_attr: bool,
        sub_name: &str,
        out: &mut Vec<(String, String, String, bool)>,
    ) {
        for r in rules {
            // Attribute-gated rule: fires on attributed actions plus the
            // rule's own name-dispatched exemptions (`implicit_action_names`
            // — plugin-declared, so core carries no framework vocabulary).
            if r.requires_action_attr
                && !has_action_attr
                && !r.implicit_action_names.iter().any(|n| n == sub_name)
            {
                continue;
            }
            if let Some(p) = params.get(r.param) {
                if p.name.starts_with('$') {
                    out.push((
                        p.name.clone(),
                        r.in_role.clone(),
                        r.type_class.clone(),
                        r.from_loader_config,
                    ));
                }
            }
        }
    }

    pub(super) fn detect_first_param_type(&mut self, params: &[ParamInfo], node: Node<'a>) {
        // Find the first param with `is_invocant = true` — normally params[0] for
        // regular methods, but params[1] for `around` modifiers (params[0] is $orig).
        // The caller that sets up the param list (visit_sub for named subs,
        // visit_anonymous_sub for modifier bodies) is responsible for marking the
        // correct param as the invocant.
        let invocant = params
            .iter()
            .find(|p| p.is_invocant && p.name.starts_with('$'));
        let invocant = match invocant {
            Some(p) => p,
            None => return,
        };

        if let Some(pkg) = self.current_package.clone() {
            let (scope, span) = (self.current_scope(), node_to_span(node));
            let head = InferredType::FirstParam { package: pkg };
            self.push_type_constraint(TypeConstraint {
                variable: invocant.name.clone(),
                scope,
                constraint_span: span,
                inferred_type: head.clone(),
            });
            // `@_`'s argument window for this scope, headed by the same
            // invocant (`seed_arg_windows` covers the scopes without one).
            self.push_type_constraint(TypeConstraint {
                variable: "@_".into(),
                scope,
                constraint_span: span,
                inferred_type: InferredType::Sequence(vec![head]),
            });
        }
    }

    pub(super) fn visit_variable_decl(&mut self, node: Node<'a>) {
        let keyword = self.get_decl_keyword(node, );
        let decl_kind = match keyword.as_deref() {
            Some("my") => DeclKind::My,
            Some("our") => DeclKind::Our,
            Some("state") => DeclKind::State,
            Some("field") => DeclKind::Field,
            _ => DeclKind::My,
        };

        // Collect all declared variables
        let vars = self.collect_vars_from_decl(node);
        for (name, var_span) in &vars {
            let sigil = name.chars().next().unwrap_or('$');
            let sym_kind = if decl_kind == DeclKind::Field { SymKind::Field } else { SymKind::Variable };
            let detail = if decl_kind == DeclKind::Field {
                let attributes = self.collect_attributes(node);
                SymbolDetail::Field { sigil, attributes }
            } else {
                SymbolDetail::Variable { sigil, decl_kind }
            };
            self.add_symbol(
                name.clone(),
                sym_kind,
                node_to_span(node),
                *var_span,
                detail,
            );
            self.add_ref(
                RefKind::Variable,
                *var_span,
                name.clone(),
                AccessKind::Declaration,
            );

            // Connect a lexical hash/hashref's literal keys to its accesses so a
            // key rename rewrites the def too (not just the `$h{k}`/`$h->{k}`
            // reads, else the container keeps the old key and the renamed reads
            // miss). The valid RHS is sigil-keyed: `%h = (LIST)` — a hashref
            // `%h = {…}` is an uneven-list bug, never valid — and `$h = {HASHREF}`
            // — a `$h = (…)` list is scalar-of-last, not a hashref. A `bless {…}`
            // / `func()` RHS is a CALL, not a bare literal, so it's excluded here
            // (those keys are owned elsewhere — bless `InternalKey`, return `Sub`).
            let rhs_kinds: &[&str] = match sigil {
                '%' => &["list_expression", "parenthesized_expression"],
                '$' => &["anonymous_hash_expression"],
                _ => &[],
            };
            if !rhs_kinds.is_empty() {
                if let Some(rhs) = node
                    .parent()
                    .filter(|p| p.kind() == "assignment_expression")
                    .and_then(|p| p.child_by_field_name("right"))
                    .filter(|r| rhs_kinds.contains(&r.kind()))
                {
                    self.emit_lexical_hash_literal_keys(name, rhs);
                }
            }

            // Synthesize accessor methods for `field $x :reader` / `:writer`
            if decl_kind == DeclKind::Field {
                let bare_name = &name[1..]; // strip sigil
                // Re-read attrs from the symbol we just stored (avoid re-collecting)
                let has_reader;
                let has_writer;
                let has_param;
                if let Some(last_sym) = self.symbols.last() {
                    if let SymbolDetail::Field { ref attributes, .. } = last_sym.detail {
                        has_reader = attributes.iter().any(|a| a == "reader");
                        has_writer = attributes.iter().any(|a| a == "writer");
                        has_param = attributes.iter().any(|a| a == "param");
                    } else {
                        has_reader = false;
                        has_writer = false;
                        has_param = false;
                    }
                } else {
                    has_reader = false;
                    has_writer = false;
                    has_param = false;
                }
                // Bare-name sub-span of the `$x` token: synthesized
                // projections (ctor key, reader) select THIS, not the
                // sigiled var span — a rename writing a bare replacement
                // over a sigiled span would eat the `$`.
                let bare_span = Span {
                    start: Point {
                        row: var_span.start.row,
                        column: var_span.start.column + 1,
                    },
                    end: var_span.end,
                };
                // `:param` → constructor key: `Point->new(x => …)` connects
                // to the field, mirroring Moo `has` / Class::Tiny synthesis.
                // Selection span = the field decl's bare name, so goto-def
                // from a constructor arg key lands on `field $x :param`
                // (rule #9).
                if has_param {
                    if let Some(ref pkg) = self.current_package {
                        self.attr_projections.push(crate::model::file_analysis::AttrProjection {
                            class: pkg.clone(),
                            attr: bare_name.to_string(),
                            kind: crate::model::file_analysis::AttrProjectionKind::CtorKey,
                        });
                    }
                    self.add_symbol(
                        bare_name.to_string(),
                        SymKind::HashKeyDef,
                        node_to_span(node),
                        bare_span,
                        SymbolDetail::HashKeyDef {
                            owner: HashKeyOwner::Sub {
                                package: self.current_package.clone(),
                                name: "new".to_string(),
                            },
                            is_dynamic: false,
                        },
                    );
                }
                if has_reader {
                    self.add_symbol(
                        bare_name.to_string(),
                        SymKind::Method,
                        node_to_span(node),
                        bare_span,
                        SymbolDetail::Sub { params: vec![], is_method: true, doc: None, opaque_return: false, is_constant: false, lexical: false },
                    );
                }
                if has_writer {
                    let writer_name = format!("set_{}", bare_name);
                    self.add_symbol(
                        writer_name,
                        SymKind::Method,
                        node_to_span(node),
                        bare_span,
                        SymbolDetail::Sub {
                            params: vec![ParamInfo {
                                name: format!("${}", bare_name),
                                default: None,
                                is_slurpy: false,
                    is_invocant: false,
                            }],
                            is_method: true,
                            doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                        },
                    );
                }
            }
        }

        // Don't recurse into children — we've already extracted what we need
        // But DO check for assignment RHS (for type inference)
        // The parent assignment_expression handles that.
    }

    /// Collect attribute names from a node's `attributes` field (e.g. `:param :reader`).
    pub(super) fn collect_attributes(&self, node: Node<'a>) -> Vec<String> {
        let mut attrs = Vec::new();
        if let Some(attrlist) = node.child_by_field_name("attributes") {
            for i in 0..attrlist.named_child_count() {
                if let Some(attr) = attrlist.named_child(i) {
                    if attr.kind() == "attribute" {
                        if let Some(name_node) = attr.child_by_field_name("name") {
                            if let Ok(name) = name_node.utf8_text(self.source) {
                                attrs.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        attrs
    }

    pub(super) fn visit_for(&mut self, node: Node<'a>) {
        // Check for loop variable: `for my $x (...) { ... }`
        let loop_var = node.child_by_field_name("variable")
            .or_else(|| {
                // Some grammars: iterator > variable_declaration
                node.child_by_field_name("iterator")
                    .and_then(|it| it.child_by_field_name("variable"))
            });

        if let Some(var_node) = loop_var {
            if let Ok(var_text) = var_node.utf8_text(self.source) {
                let var_name = var_text.to_string();

                // Find the body block
                let body_span = node.child_by_field_name("body")
                    .map(|b| node_to_span(b))
                    .unwrap_or(node_to_span(node));

                self.push_scope(
                    ScopeKind::ForLoop { var: var_name.clone() },
                    body_span,
                    None,
                );

                let sigil = var_name.chars().next().unwrap_or('$');
                self.add_symbol(
                    var_name.clone(),
                    SymKind::Variable,
                    node_to_span(var_node),
                    node_to_span(var_node),
                    SymbolDetail::Variable { sigil, decl_kind: DeclKind::ForVar },
                );
                self.add_ref(
                    RefKind::Variable,
                    node_to_span(var_node),
                    var_name.clone(),
                    AccessKind::Declaration,
                );

                // Accumulate loop variable values for constant folding
                // for my $x (qw(a b c)) → $x => ["a", "b", "c"]
                if let Some(list_node) = node.child_by_field_name("list") {
                    let mut values = self.extract_string_names(list_node);
                    // CG-3a: `for my $tag (_all_html_tags()) { *$tag = sub {...} }`
                    // (CGI.pm). When the loop source is a call to a same-file sub
                    // whose body is a literal qw/list return, fold that sub's
                    // literal return into the loop var so the glob-install names
                    // resolve. Non-literal local subs (or a cross-file callee)
                    // yield nothing → loop var stays dynamic and glob synthesis
                    // skips (no fabrication).
                    if values.is_empty() {
                        values = self.fold_local_sub_literal_return(list_node);
                    }
                    // Form 2 (loop-push re-export): a body that does
                    // `push @EXPORT, @{"${var}::EXPORT"}` re-exports each module
                    // the loop iterates over. Mint an edge per statically
                    // resolvable list element. Dynamic lists fold to `values`
                    // empty → no edge (honest). Done before the move of
                    // `values` into `constant_strings`.
                    if !values.is_empty() {
                        if let Some(body) = node
                            .child_by_field_name("block")
                            .or_else(|| node.child_by_field_name("body"))
                        {
                            if self.body_has_symbolic_export_push(body, &var_name) {
                                for module in &values {
                                    self.record_reexport_edge(module);
                                }
                            }
                        }
                    }
                    if !values.is_empty() {
                        self.constant_strings.insert(var_name, values);
                    }
                }

                self.queue_children_then(node, |b| { b.pop_scope(); });
                return;
            }
        }

        // No loop variable — just visit children normally
        self.queue_children(node);
    }

    /// Statement-modifier loop: `EXPR for LIST` / `EXPR foreach LIST`.
    ///
    /// mk_classdata-in-LOOP (Catalyst.pm/Controller.pm, ~205 FPs):
    /// `__PACKAGE__->mk_classdata($_) for qw/a b c/` and
    /// `mk_classdata($_) for (LIST)` install one class-data accessor per list
    /// element. The implicit loop var (`$_`) is the call arg; the literal qw/
    /// list is the authoritative name source — same producer as the direct
    /// `mk_classdata('name')` form, just driven by the loop list. Non-literal
    /// lists (an array var, computed) fold to nothing → no synthesis.
    pub(super) fn visit_postfix_for(&mut self, node: Node<'a>) {
        // The `list` field can point at the `(` paren for the
        // `... for (LIST)` form (the child_by_field_name paren gotcha), so the
        // list payload is read off the second named child instead — `[call,
        // list]`. `extract_string_list` folds qw / paren-list / constants.
        let mut topic_values: Vec<String> = Vec::new();
        if let (Some(call), Some(list_node)) = (node.named_child(0), node.named_child(1)) {
            let names = self.extract_string_list(list_node);
            if !names.is_empty() && self.is_class_accessor_loop_body(call) {
                self.emit_class_accessor_symbols(call, &names);
            }
            topic_values = names.into_iter().map(|(n, _)| n).collect();
        }
        // The statement-modifier topic: `EXPR for qw(...)` runs EXPR once
        // per element with `$_` bound — fold `$_` over the literal list
        // for the body visit so registration loops expand
        // (`$app->helper($_ => …) for qw(a b c)` is N registrations).
        // Scoped: saved and restored around the children walk.
        let saved = self.constant_strings.get("$_").cloned();
        if !topic_values.is_empty() {
            self.constant_strings.insert("$_".to_string(), topic_values);
        }
        self.queue_children_then(node, move |b| match saved {
            Some(v) => {
                b.constant_strings.insert("$_".to_string(), v);
            }
            None => {
                b.constant_strings.remove("$_");
            }
        });
    }

    /// Is `node` a `mk_classdata`/`mk_classaccessor` call whose single arg is the
    /// loop var (`$_` or a named scalar)? The shape that, under a statement-
    /// modifier loop, installs one accessor per list element. Both the bare
    /// `mk_classdata($_)` and `__PACKAGE__->mk_classdata($_)` forms qualify;
    /// the callee name is the signal (rule #10), not the invocant.
    pub(super) fn is_class_accessor_loop_body(&self, node: Node<'a>) -> bool {
        let (callee, args) = match node.kind() {
            "function_call_expression" | "ambiguous_function_call_expression" => {
                (node.child_by_field_name("function"), node.child_by_field_name("arguments"))
            }
            "method_call_expression" => {
                (node.child_by_field_name("method"), node.child_by_field_name("arguments"))
            }
            _ => return false,
        };
        let Some(callee) = callee else { return false };
        if !matches!(
            callee.utf8_text(self.source).ok(),
            Some("mk_classdata") | Some("mk_classaccessor")
        ) {
            return false;
        }
        // Single scalar arg — the loop var the list feeds. Anything else
        // (a literal name, multiple args) is the non-loop form handled by
        // `visit_group_accessors` directly, or not a per-element install.
        matches!(args.map(|a| a.kind()), Some("scalar"))
    }

    pub(super) fn add_fold_range(&mut self, node: Node<'a>) {
        let start = node.start_position().row;
        let end = node.end_position().row;
        if end > start {
            self.fold_ranges.push(FoldRange {
                start_line: start,
                end_line: end,
                kind: FoldKind::Region,
            });
        }
    }
}
