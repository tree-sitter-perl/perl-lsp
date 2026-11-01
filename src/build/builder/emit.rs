//! Expression witness emission: literal typing, `expr_payload`,
//! `emit_expr_witness`, return/branch-arm witnesses, operator typing.

use super::*;

impl<'a> Builder<'a> {
    /// Compute the right `WitnessPayload` shape for an expression node.
    /// Single dispatch — every rvalue node, whether it appears as a
    /// return body, ternary arm, RHS of assignment, or anywhere else,
    /// runs through here. Two categories:
    ///
    /// - **Closed under syntax** (literals, constructors, arithmetic,
    ///   regexp): walker bakes `InferredType(t)` from the node kind
    ///   alone. There's nothing to resolve — `42` is `Numeric` no
    ///   matter what.
    /// - **Name-dependent / compound** (variables, calls, ternaries):
    ///   walker emits `Edge(...)` to the attachment that carries the
    ///   resolved type. Registry materialization chases at query
    ///   time. Ternaries Edge to their own `Expr(span)` since the
    ///   per-arm witnesses live there.
    /// Type a hash literal structurally: literal keys carry their
    /// values' types (`{ host => 'x' }` →
    /// `HashWithKeys{[("host", String)], closed}`). A spread element
    /// (`%$other`, `%other`) or a dynamic key flips `open` — the key
    /// set is no longer exhaustive, so consumers can't treat unknown
    /// keys as misses. No literal keys at all → plain `HashRef`
    /// (back-compat: `{}` and fully-dynamic hashes keep today's type).
    /// `(e1, e2, …)` in ARRAY context → `Sequence([t1, t2, …])`, each element
    /// typed via its own Expr witness, so `my ($x,$y) = @arr` can `element_at`.
    /// `None` if ANY element is unresolved (a holey Sequence mis-projects).
    pub(super) fn list_literal_type(&mut self, node: Node<'a>) -> Option<InferredType> {
        let mut flat: Vec<Node<'a>> = Vec::new();
        crate::cst::flatten_list(node, &mut flat);
        let named: Vec<Node<'a>> = flat.into_iter().filter(|n| n.is_named()).collect();
        let mut elems = Vec::with_capacity(named.len());
        for child in named {
            self.emit_expr_witness(child);
            elems.push(self.bag_query_expr_span(node_to_span(child))?);
        }
        (!elems.is_empty()).then(|| InferredType::Sequence(elems))
    }

    pub(super) fn hash_literal_type(&mut self, node: Node<'a>) -> InferredType {
        // A spread occupies ONE list slot but flattens to an even count
        // at runtime, so pairing must skip it as a unit — `pair_nodes`'
        // strict k/v alternation would mispair everything after it.
        let mut flat: Vec<Node<'a>> = Vec::new();
        crate::cst::flatten_list(crate::cst::literal_body(node), &mut flat);
        let named: Vec<Node<'a>> = flat.into_iter().filter(|n| n.is_named()).collect();

        let mut keys: Vec<(String, Option<Box<InferredType>>)> = Vec::new();
        let mut open = false;
        let mut i = 0;
        while i < named.len() {
            let elem = named[i];
            if crate::cst::is_flattening(elem) {
                // Spread (`%other` / `%$ref` / `@_` / `@rest`) — the
                // key set is no longer exhaustive. Arrays included:
                // `my %h = (default => 1, @_)` is the canonical
                // args-with-defaults idiom.
                open = true;
                i += 1;
                continue;
            }
            let Some(v_node) = named.get(i + 1).copied() else {
                open = true;
                break;
            };
            i += 2;
            let Some((key, is_dynamic)) = self.extract_key_text(elem) else {
                open = true;
                continue;
            };
            if is_dynamic {
                open = true;
                continue;
            }
            self.emit_expr_witness(v_node);
            let vt = self.bag_query_expr_span(node_to_span(v_node));
            keys.push((key, vt.map(Box::new)));
        }
        InferredType::HashWithKeys { keys: crate::model::file_analysis::SharedKeys::new(keys), open }
    }

    /// Type an array literal positionally: `[1, 'x']` →
    /// `Sequence([Numeric, String])`, so `->[N]` projects per index
    /// (tuple semantics — the homogeneous case is just every slot
    /// agreeing). Degrades to plain `ArrayRef` when any element's type
    /// is unknown or the literal is huge (`Sequence` is a type-per-slot
    /// tuple, not a summary).
    pub(super) fn array_literal_type(&mut self, node: Node<'a>) -> InferredType {
        const MAX_TUPLE: usize = 64;
        let mut flat: Vec<Node<'a>> = Vec::new();
        crate::cst::flatten_list(crate::cst::literal_body(node), &mut flat);
        let elems: Vec<Node<'a>> = flat.into_iter().filter(|n| n.is_named()).collect();
        if elems.is_empty() || elems.len() > MAX_TUPLE {
            return InferredType::ArrayRef;
        }
        let mut types = Vec::with_capacity(elems.len());
        for e in &elems {
            self.emit_expr_witness(*e);
            match self.bag_query_expr_span(node_to_span(*e)) {
                Some(t) => types.push(t),
                None => return InferredType::ArrayRef,
            }
        }
        InferredType::Sequence(types)
    }

    /// Is this constraint-constructor name actually imported here?
    ///
    /// The name gate itself is global: a plugin declares `Int` a constraint
    /// constructor and core has no idea which packages pulled it in. Without
    /// this, a user's own `sub Str` would type as a constraint anywhere —
    /// and the gate fires BEFORE local symbol lookup, so the user's sub does
    /// not even get to compete. Hand-curating a list of "names unlikely to
    /// collide" is the partial enumeration rule #10 warns about: it is
    /// always incomplete, and the failure is silent.
    ///
    /// Requiring the import costs nothing, because Type::Tiny constants MUST
    /// be imported to compile. `-all` / `:all` / bare `use` expand to the
    /// full vocabulary through the plugin's `on_use`, and house type
    /// libraries reach the same place via `SyntheticUse`.
    fn constraint_name_imported(&self, name: &str) -> bool {
        self.imports
            .iter()
            .any(|i| i.imported_symbols.iter().any(|sym| sym.local_name == name))
    }

    /// What a call to `name` reports about a reference: the `ref` builtin,
    /// or a plugin-declared `ref_tag_functions()` entry this file imported
    /// (by its local name) or names fully qualified.
    pub(super) fn ref_tag_fn(&self, name: &str) -> Option<crate::model::file_analysis::RefTagFn> {
        use crate::model::conventions::{is_ref_builtin, split_qualified, PERL_SPELLINGS};
        use crate::model::file_analysis::RefTagFn;
        if is_ref_builtin(name) {
            return Some(RefTagFn::Ref);
        }
        let (module, leaf) = split_qualified(name, &PERL_SPELLINGS);
        self.ref_tag_functions.iter().find_map(|f| {
            let hit = match module {
                Some(m) => m == f.module && leaf == f.name,
                None => self.imports.iter().any(|i| {
                    i.module_name == f.module
                        && i.imported_symbols.iter().any(|sym| {
                            sym.local_name == name
                                && sym.remote_name.as_deref().unwrap_or(&sym.local_name) == f.name
                        })
                }),
            };
            hit.then_some(f.tag)
        })
    }

    /// [`crate::cst::ref_tag_call`] with this file's ref-tag functions.
    pub(super) fn ref_tag_call(
        &self,
        node: Node<'a>,
    ) -> Option<(crate::model::file_analysis::RefTagFn, Node<'a>)> {
        crate::cst::ref_tag_call(node, self.source, |n| self.ref_tag_fn(n))
    }

    pub(super) fn expr_payload(&mut self, node: Node<'a>) -> Option<crate::model::witnesses::WitnessPayload> {
        use crate::model::witnesses::{RefIdx, WitnessAttachment, WitnessPayload};
        // What a reference is, named for the variable it was asked of: a
        // comparison of it to a literal guards that variable.
        if let Some((via, subject)) = self.ref_tag_call(node)
            .and_then(|(via, operand)| Some((via, crate::cst::canonical_var_name(operand, self.source)?)))
        {
            return Some(WitnessPayload::InferredType(InferredType::RefTagOf {
                subject,
                site: node_to_span(node),
                via,
            }));
        }
        match node.kind() {
            // Closed-under-syntax — bake.
            "string_literal" | "interpolated_string_literal" => {
                Some(WitnessPayload::InferredType(InferredType::String))
            }
            "number" => Some(WitnessPayload::InferredType(InferredType::Numeric)),
            "anonymous_hash_expression" => {
                Some(WitnessPayload::InferredType(self.hash_literal_type(node)))
            }
            // Drill expressions edge to their base with a projection step,
            // so `$config->{db}` resolves at QUERY time through whatever the
            // base materializes to — including an imported literal's
            // structural type the build-time chain pass can't see.
            "hash_element_expression" => {
                let key_node = node.child_by_field_name("key")?;
                let (key, is_dynamic) = self.extract_key_text(key_node)?;
                if is_dynamic {
                    return None;
                }
                // Container form `$h{k}` (grammar field `hash:`) — the
                // subject is `%h`, not scalar `$h`; project off the
                // hash variable's own attachment (the registry scope-
                // walks Variable bases the same as Edge(Variable)).
                if let Some(container) = node.child_by_field_name("hash") {
                    let name =
                        crate::cst::canonical_container_name(container, self.source)?;
                    let scope = self.scope_stack.last().copied()
                        .unwrap_or_else(|| self.scope_at_point(node.start_position()));
                    return Some(WitnessPayload::Projected {
                        base: WitnessAttachment::Variable { name, scope },
                        step: crate::model::witnesses::ProjectionStep::HashKey(key),
                    });
                }
                let base = node.named_child(0)?;
                self.emit_expr_witness(base);
                Some(WitnessPayload::Projected {
                    base: WitnessAttachment::Expr(node_to_span(base)),
                    step: crate::model::witnesses::ProjectionStep::HashKey(key),
                })
            }
            // Arrow-deref element only (`$x->[0]`); the container form
            // `$arr[N]` keeps its variable-keyed handling.
            "array_element_expression" if node.child_by_field_name("array").is_none() => {
                let base = node.named_child(0)?;
                let idx_node = node.child_by_field_name("index")?;
                let idx: i32 = idx_node.utf8_text(self.source).ok()?.parse().ok()?;
                self.emit_expr_witness(base);
                Some(WitnessPayload::Projected {
                    base: WitnessAttachment::Expr(node_to_span(base)),
                    step: crate::model::witnesses::ProjectionStep::ArrayIndex(idx),
                })
            }
            "anonymous_array_expression" => {
                Some(WitnessPayload::InferredType(self.array_literal_type(node)))
            }
            // A paren list in value position (`return ($a, $b)`) is a
            // positional tuple — the list-assignment binder projects
            // `element_at(n)` off it (docs/adr/destructuring.md). Explicit
            // parens are their own node, so the same tuple arrives either
            // spelling; a group around ONE non-list value is transparent
            // and carries that value's type through an edge.
            "list_expression" => self.list_literal_type(node).map(WitnessPayload::InferredType),
            "parenthesized_expression" => {
                let named: Vec<Node<'a>> = crate::cst::NodeExt::named(&node).collect();
                match named.as_slice() {
                    [only] if only.kind() != "list_expression" => {
                        self.emit_expr_witness(*only);
                        Some(WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(*only))))
                    }
                    _ => self.list_literal_type(node).map(WitnessPayload::InferredType),
                }
            }
            "quoted_regexp" => Some(WitnessPayload::InferredType(InferredType::Regexp)),
            // `\%h` / `\@a` take a container's reference; only `\&name` is
            // code. A `\$x` or `\(...)` is a scalar ref nothing models.
            "refgen_expression" => match crate::cst::refgen_operand(node) {
                Some(crate::cst::RefgenOperand::Code) => {
                    let return_edge = self.coderef_return_edge_for(node);
                    Some(WitnessPayload::InferredType(InferredType::CodeRef { return_edge }))
                }
                Some(crate::cst::RefgenOperand::Hash) => Some(WitnessPayload::InferredType(InferredType::HashRef)),
                Some(crate::cst::RefgenOperand::Array) => Some(WitnessPayload::InferredType(InferredType::ArrayRef)),
                _ => None,
            },
            "anonymous_subroutine_expression" => {
                // Pre-create the (anon) Symbol so
                // `coderef_return_edge_for`'s anon-sub arm resolves
                // to `Symbol(sym_id)` instead of falling back to
                // the body-span edge. The walker's
                // `visit_anonymous_sub` runs LATER (descends into
                // the body after the assignment finishes pushing
                // its payload), so without this pre-step the TC for
                // the bound variable carries the stale Expr-span
                // edge and `ReturnExprReducer` never sees the call
                // site through Symbol-attached witnesses.
                let params = self.extract_params(node);
                self.ensure_anon_sub_symbol(node, &params);
                let return_edge = self.coderef_return_edge_for(node);
                Some(WitnessPayload::InferredType(InferredType::CodeRef { return_edge }))
            }
            // A `shift` of `@_` is the head of the argument window at its
            // point; the walk's `consume_arg_head` advances the window past it.
            _ if self.shifts_arg_window(node) => Some(WitnessPayload::Projected {
                base: WitnessAttachment::Variable {
                    name: "@_".into(),
                    scope: self.scope_at_point(node.start_position()),
                },
                step: crate::model::witnesses::ProjectionStep::ArrayIndex(0),
            }),

            "binary_expression"
            | "equality_expression"
            | "relational_expression"
            | "logical_not_expression"
            | "unary_expression"
            | "postinc_expression"
            | "preinc_expression"
            | "func0op_call_expression"
            | "func1op_call_expression" => self
                .infer_expression_result_type(node)
                .map(WitnessPayload::InferredType),

            // Variable — Edge to its Variable attachment. Registry
            // materialization routes through `query_variable_type`
            // (scope-walking + framework-aware fold). Arrays/hashes edge the
            // same way (sigil kept), so `my ($x,$y) = @arr` can chase `@arr`
            // to its `Sequence` and `element_at`.
            "scalar" | "array" | "hash" => {
                let name = node.utf8_text(self.source).ok()?;
                Some(WitnessPayload::Edge(WitnessAttachment::Variable {
                    name: name.to_string(),
                    // `expr_payload` re-runs post-walk (forward-ref recovery,
                    // nested-constraint param typing) when the live scope stack
                    // is gone; recover the scope from the node's position.
                    scope: self.scope_stack.last().copied()
                        .unwrap_or_else(|| self.scope_at_point(node.start_position())),
                }))
            }

            // Method call — constructor pattern is closed under
            // syntax (`Foo->new` → `ClassName("Foo")`), bake. Otherwise
            // Edge to the call's `Expression(refidx)` attachment;
            // `emit_method_call_return_edges` re-emits
            // `Edge(PackageSymbol{package, method})` there once
            // `invocant_class` is filled, so the chase resolves through.
            "method_call_expression" => {
                if let Some(class) = self.extract_constructor_class(node) {
                    return Some(WitnessPayload::InferredType(InferredType::ClassName(class)));
                }
                // A named method, or a `$obj->$m` whose `$m` folded to one
                // name: the call's ref carries its `Invoke`.
                if let Some(&[idx]) = self.method_call_refs.get(&node.id()).map(Vec::as_slice) {
                    return Some(WitnessPayload::Edge(WitnessAttachment::Expression(RefIdx(
                        idx as u32,
                    ))));
                }
                // `$obj->$cb(…)`: call whatever `$cb` holds, on `$obj`.
                if let Some(cb) = crate::cst::dynamic_method_scalar(node) {
                    self.emit_expr_witness(cb);
                    let receiver = node.child_by_field_name("invocant").map(|inv| {
                        self.emit_invocant_witness(inv);
                        WitnessAttachment::Expr(node_to_span(inv))
                    });
                    let args = self.extract_call_args(node);
                    return Some(WitnessPayload::Invoke {
                        callee: crate::model::witnesses::Callee::Value(WitnessAttachment::Expr(
                            node_to_span(cb),
                        )),
                        receiver,
                        args: Some(self.call_operands(&args)),
                    });
                }
                let span = node_to_span(node);
                let idx = self.refs.iter().position(|r| {
                    matches!(r.kind, RefKind::MethodCall { .. }) && r.span == span
                })?;
                Some(WitnessPayload::Edge(WitnessAttachment::Expression(RefIdx(
                    idx as u32,
                ))))
            }

            // Function / bareword / scoped-identifier call — an `Invoke`
            // of the local sub the name resolves to (plugin-synthesized
            // subs included: they push into `self.symbols` before
            // walk-time use) on the call's arguments. Failures (target not
            // yet in the symbol table) are recovered post-walk by
            // `resolve_forward_expr_witnesses`.
            // `bless $ref, $class` is a value of type `ClassName($class)`
            // — closed under syntax when the class resolves. Covers the
            // anonymous-ref forms (`return bless {}, $class`) that have no
            // variable to promote; the variable form is additionally
            // promoted via `visit_bless_call`'s TC. Honest-miss (fall
            // through) when the class isn't determinable.
            // `new Foo(...)` read as `Foo->new(...)`: a constructor bakes
            // like `Foo->new`; any other method is the call's `Invoke`.
            "ambiguous_function_call_expression" if self.indirect_method_call(node).is_some() => {
                let io = self.indirect_method_call(node)?;
                let method = io.method.utf8_text(self.source).ok()?;
                if crate::model::conventions::is_constructor_name(method) {
                    let class = io.class.utf8_text(self.source).ok()?;
                    return Some(WitnessPayload::InferredType(InferredType::ClassName(class.to_string())));
                }
                let &[idx] = self.method_call_refs.get(&node.id())?.as_slice() else { return None };
                Some(WitnessPayload::Edge(WitnessAttachment::Expression(RefIdx(idx as u32))))
            }

            "function_call_expression" | "ambiguous_function_call_expression"
                if self.is_bless_call(node) =>
            {
                let args = self.extract_call_args(node);
                match args.get(1) {
                    // Receiver-polymorphic: `bless X, $class` / `bless X, ref
                    // $self || $self` returns the class it was CALLED ON, so
                    // inherited ctors and `SUPER::new` chains type to the actual
                    // subclass. Fall back to the enclosing class for bare
                    // (no-call-site) queries.
                    Some(c) if self.bless_class_is_receiver(*c) => {
                        let re = match self.current_package.clone() {
                            Some(pkg) => crate::model::witnesses::ReturnExpr::ReceiverOr(
                                InferredType::ClassName(pkg),
                            ),
                            None => crate::model::witnesses::ReturnExpr::Receiver,
                        };
                        Some(WitnessPayload::ReturnExpr(re))
                    }
                    // Fixed class: string / bareword / __PACKAGE__ / `ref` of a
                    // non-invocant.
                    Some(c) => self
                        .bless_class_of(*c)
                        .map(|c| WitnessPayload::InferredType(InferredType::ClassName(c))),
                    // One-arg `bless {}` blesses into the CURRENT package (not the
                    // receiver — Perl's one-arg bless uses __PACKAGE__).
                    None => self
                        .current_package
                        .clone()
                        .map(|c| WitnessPayload::InferredType(InferredType::ClassName(c))),
                }
            }

            "function_call_expression"
            | "ambiguous_function_call_expression"
            | "bareword"
            | "scoped_identifier" => {
                let called = self.forward_callee_name(node)?;
                // The constraint-name table keys on the bare name; the symbol
                // lookup below gets the qualified spelling, which is the half
                // that decides whether a local sub is even a candidate.
                let bare = bare_name(&called).to_string();
                // Type::Tiny constraint constructor (`InstanceOf['Foo']`): the
                // call is a *value* of type `TypeConstraintOf(inner)` — you
                // call `->check` on it, not Foo's methods. The plugin folds the
                // params (core extracts them, rule #1); we wrap.
                if self.type_constraint_names.contains(&bare)
                    && self.constraint_name_imported(&bare)
                {
                    let params = self.extract_constraint_params(node);
                    // The plugin returns the whole constraint value, not the
                    // inner — it is the half that knows whether a name it
                    // owns has an expressible inner at all. Core wrapping it
                    // again would nest a constraint inside a constraint.
                    if let Some(c) = self.plugins.type_constraint_inner(&bare, &params) {
                        return Some(WitnessPayload::InferredType(c));
                    }
                }
                // A callee this file doesn't define yet answers None here, so
                // the node queues for `resolve_forward_expr_witnesses`: a sub
                // defined further down resolves locally there, and only what
                // is still unresolved is looked up across files.
                let sid = self.find_callee_symbol(&called)?;
                Some(self.call_invoke(node, called, Some(sid)))
            }

            // `$arr[N]` projects off `@arr` at the read's own point, so
            // `$_[N]` reads `@_`'s argument window as the shifts before it
            // left it. `$_[0]`'s call-site receiver rides a second witness
            // (`emit_expr_witness_inner`).
            "array_element_expression" => {
                let container = node.child_by_field_name("array")?;
                let idx: i32 =
                    node.child_by_field_name("index")?.utf8_text(self.source).ok()?.parse().ok()?;
                let name = crate::cst::canonical_container_name(container, self.source)?;
                Some(WitnessPayload::Projected {
                    base: WitnessAttachment::Variable {
                        name,
                        scope: self.scope_at_point(node.start_position()),
                    },
                    step: crate::model::witnesses::ProjectionStep::ArrayIndex(idx),
                })
            }


            // An assignment's value is the target's new value.
            // `||=`/`//=` never reach here — `emit_expr_witness` types them
            // as a short-circuit. `=` / `&&=` produce the RHS, unless
            // the target is an aggregate: a list assignment in scalar context
            // is a count, and the list type is `@flow`'s business.
            // TODO(falsy-part): `&&=` is the false old value ⊔ the RHS; it
            // types as the RHS until a value's false part has a type.
            "assignment_expression" => match crate::cst::assign_op(node)? {
                crate::cst::AssignOp::Plain | crate::cst::AssignOp::AndThen => {
                    let left = node.child_by_field_name("left")?;
                    if self.lhs_list_targets(left).is_some()
                        || self
                            .get_var_text_from_lhs(left)
                            .is_some_and(|v| v.starts_with(['@', '%']))
                    {
                        return None;
                    }
                    let rhs = node.child_by_field_name("right")?;
                    self.emit_expr_witness(rhs);
                    Some(WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(rhs))))
                }
                crate::cst::AssignOp::Append | crate::cst::AssignOp::Repeat => {
                    Some(WitnessPayload::InferredType(InferredType::String))
                }
                crate::cst::AssignOp::Numeric => {
                    Some(WitnessPayload::InferredType(InferredType::Numeric))
                }
                // Two strings make a string Perl builds per character, which
                // no caller wants typed; anything else is the number.
                // TODO(bitwise-feature): under `use feature 'bitwise'` (the
                // v5.28 bundle) these are always numeric and `|.=` & co. are
                // the string ops. The feature is known before the code it
                // governs (`use` precedes it, bundle plugins run `on_use`
                // during the walk), so this can read a per-scope feature set
                // once the builder records one.
                crate::cst::AssignOp::Bitwise => {
                    let is_string = |b: &mut Self, n: Node<'a>| {
                        b.emit_expr_witness(n);
                        b.bag_query_expr_span(node_to_span(n)).is_some_and(|t| t.is_string())
                    };
                    let left = node.child_by_field_name("left")?;
                    let right = node.child_by_field_name("right")?;
                    if is_string(self, left) && is_string(self, right) {
                        None
                    } else {
                        Some(WitnessPayload::InferredType(InferredType::Numeric))
                    }
                }
                crate::cst::AssignOp::Fallback => None,
            },

            // `$cb->(…)`: call whatever the operand holds.
            "coderef_call_expression" => {
                let operand = node.named_child(0)?;
                self.emit_expr_witness(operand);
                let args = self.extract_call_args(node);
                Some(WitnessPayload::Invoke {
                    callee: crate::model::witnesses::Callee::Value(WitnessAttachment::Expr(
                        node_to_span(operand),
                    )),
                    receiver: None,
                    args: Some(self.call_operands(&args)),
                })
            }

            // Ternary — Edge to its own Expr(span). The per-arm
            // `branch_arm`-source witnesses live on that attachment;
            // the registry's edge chase + BranchArmFold agree them.
            "conditional_expression" => Some(WitnessPayload::Edge(WitnessAttachment::Expr(
                node_to_span(node),
            ))),

            _ => None,
        }
    }

    /// A `shift` call or `$_[N]` positional read — a PARAMETER pull. The
    /// `||`/`//` fold treats these LHSes as the param-default idiom (the
    /// value is the unknown parameter, not the fallback literal's type).
    pub(super) fn is_param_pull(&self, node: Node<'a>) -> bool {
        if self.is_shift_call(node) {
            return true;
        }
        if node.kind() == "array_element_expression" {
            if let Some(array) = node.child_by_field_name("array") {
                return array.kind() == "container_variable"
                    && array.named_child(0).and_then(|v| v.utf8_text(self.source).ok())
                        == Some("_");
            }
        }
        false
    }

    /// The receiver operand of a method call: `Expr(invocant)` typed as the
    /// invocant SLOT reads it. A bareword names a class unless a sub of that
    /// name answers (`app->routes`), so the class lands first and the sub's
    /// edge after it, winning only when it resolves. A scalar const-folded
    /// to one class string reads as that class whatever the variable holds,
    /// so its class lands last.
    pub(super) fn emit_invocant_witness(&mut self, inv: Node<'a>) {
        let span = node_to_span(inv);
        let text = inv.utf8_text(self.source).unwrap_or("");
        match inv.kind() {
            "bareword" | "package" => {
                self.push_invocant_class(span, text.to_string());
                self.emit_expr_witness(inv);
            }
            "scalar" => {
                self.emit_expr_witness(inv);
                let key = crate::cst::canonical_var_name(inv, self.source);
                if let Some([class]) = self.resolve_constant_strings(key.as_deref().unwrap_or(text), inv).as_deref() {
                    self.push_invocant_class(span, class.clone());
                }
            }
            _ => self.emit_expr_witness(inv),
        }
    }

    /// The receiver at `span` is the class `class` itself.
    pub(super) fn push_invocant_class(&mut self, span: Span, class: String) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let w = Witness {
            attachment: WitnessAttachment::Expr(span),
            source: WitnessSource::Builder("invocant_class".into()),
            payload: WitnessPayload::InferredType(InferredType::ClassName(class)),
            span,
        };
        if !self.bag.for_attachment(&w.attachment).iter().any(|o| o.payload == w.payload && o.source == w.source) {
            self.bag.push(w);
        }
    }

    /// The argument operands of a call, each an `Expr` the walk types now.
    /// Grouping parens are peeled; an aggregate flattens into any number of
    /// values, so it is a `Spread`.
    pub(super) fn call_operands(&mut self, args: &[Node<'a>]) -> Vec<crate::model::witnesses::CallArg> {
        use crate::model::witnesses::{CallArg, WitnessAttachment};
        let flat = self.flat_call_args(args.to_vec());
        flat.into_iter()
            .map(|a| {
                self.emit_expr_witness(a);
                let att = WitnessAttachment::Expr(node_to_span(a));
                if crate::cst::is_flattening(a) {
                    CallArg::Spread(att)
                } else {
                    CallArg::One(att)
                }
            })
            .collect()
    }

    /// A `shift` whose array is `@_` — bare, or spelled `shift @_`.
    pub(super) fn shifts_arg_window(&self, node: Node<'a>) -> bool {
        self.is_shift_call(node)
            && match self.extract_call_args(node).as_slice() {
                [] => true,
                [arg] => arg.utf8_text(self.source).ok() == Some("@_"),
                _ => false,
            }
    }

    /// `$_[0]` — the positional-receiver pseudo-invocant of a method
    /// body. Shared by `invocant_type_at_node`'s array-element arm and
    /// `expr_payload`'s `Receiver` emission so both agree on the shape.
    pub(super) fn is_positional_receiver(&self, node: Node<'a>) -> bool {
        let Some(array) = node.child_by_field_name("array") else { return false };
        let Some(index) = node.child_by_field_name("index") else { return false };
        array.kind() == "container_variable"
            && array.named_child(0).and_then(|v| v.utf8_text(self.source).ok()) == Some("_")
            && index.utf8_text(self.source).ok() == Some("0")
    }

    /// Emit the `Expr(span)` witnesses for `node` and (recursively)
    /// its sub-arms. For a non-ternary node: one witness on
    /// `Expr(span)` with the node's `expr_payload` (literal type, or
    /// Edge to Variable/Symbol/Expression). For a ternary: one
    /// `Edge(Expr(arm_span))` per arm on `BranchArm(span)`, a single
    /// `Edge(BranchArm(span))` on `Expr(span)` (so the expression
    /// resolves to `BranchArmFold`'s agreed answer), plus a recursive
    /// call per arm so each arm's own `Expr(span)` is populated.
    /// Idempotent on span — multiple callers (return arm + chain typing
    /// + RHS-of-assignment) firing against the same node produce
    /// duplicate witnesses but don't change query answers, since the
    /// latest-wins reducers fold them all the same way.
    /// How deep expression-shape typing will nest before it degrades.
    ///
    /// Typing a compound expression is inherently post-order — `[[1]]` cannot
    /// name its `Sequence` until the inner literal has one — so this recursion
    /// is not queueable the way the CST walk is. It is bounded instead. Past
    /// the bound `expr_payload` declines, and the enclosing shape takes the
    /// path it already takes for an element it cannot type: `Sequence`
    /// degrades to plain `ArrayRef`/`HashRef`.
    ///
    /// The file still gets a REAL analysis — symbols, refs and scopes are the
    /// walk's output and are unaffected; only the type attached to one
    /// pathologically-nested literal coarsens. 64 is far above any nesting a
    /// reader could follow (the deepest CST in 138,806 CPAN files is 247
    /// levels *total*), and a `Sequence` nested deeper than this describes
    /// nothing a consumer can use.
    pub(super) const MAX_EXPR_TYPE_DEPTH: usize = 64;

    /// Publish one UNDEF arm on the sub's return-arm attachment.
    ///
    /// Carries no rvalue type — `undef`, a bare `return;`, and the empty list
    /// `()` (which coerces to undef in scalar context) have nothing to point
    /// an edge at — so it is a Fact. Both ways out of a sub, the explicit
    /// `return` and the fallthrough tail, spell it through here.
    pub(super) fn push_return_undef_arm(
        &mut self,
        sym_id: SymbolId,
        span: Span,
        spelling: crate::model::witnesses::tags::UndefArm,
    ) {
        use crate::model::witnesses::{
            tags, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };
        self.bag.push(Witness {
            attachment: WitnessAttachment::SymbolReturnArm(sym_id),
            source: WitnessSource::Builder(tags::FACT_UNDEF_ARM.into()),
            payload: WitnessPayload::Fact {
                family: tags::FACT_UNDEF_ARM.into(),
                key: String::new(),
                value: spelling.as_fact_value(),
            },
            span,
        });
        self.link_return_arms(sym_id, span);
    }

    /// Publish one VALUE arm on the sub's return-arm attachment: the edge to
    /// its type, and the census Fact that records the arm existed.
    ///
    /// Both, always, from one place. The edge is what types the arm; the Fact
    /// is what proves it was there, because the registry resolves edges into
    /// materialized witnesses BEFORE a reducer sees the list, so an arm whose
    /// expression never types is indistinguishable from an arm that never
    /// existed. The all-undef verdict turns on exactly that distinction.
    pub(super) fn push_return_value_arm(&mut self, sym_id: SymbolId, span: Span) {
        use crate::model::witnesses::{
            tags, FactValue, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };
        self.bag.push(Witness {
            attachment: WitnessAttachment::SymbolReturnArm(sym_id),
            source: WitnessSource::Builder("return_arm".into()),
            payload: WitnessPayload::Edge(WitnessAttachment::Expr(span)),
            span,
        });
        self.bag.push(Witness {
            attachment: WitnessAttachment::SymbolReturnArm(sym_id),
            source: WitnessSource::Builder(tags::FACT_VALUE_ARM.into()),
            payload: WitnessPayload::Fact {
                family: tags::FACT_VALUE_ARM.into(),
                key: String::new(),
                value: FactValue::Bool(true),
            },
            span,
        });
        self.link_return_arms(sym_id, span);
    }

    /// Make the sub's arm-join a candidate answer on `Symbol(sub)`: one
    /// `Edge(SymbolReturnArm(sub))` per sub, anchored at its first arm.
    ///
    /// The link is a per-sub fact, not a per-arm one — every copy would point
    /// at the same arm set and materialize the same join, and each extra copy
    /// re-enters that set on every query of the sub's return. Only the arm
    /// pushers above call this, so every arm, explicit or fallthrough, is
    /// linked by construction.
    fn link_return_arms(&mut self, sym_id: SymbolId, span: Span) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let arms = WitnessAttachment::SymbolReturnArm(sym_id);
        let symbol = WitnessAttachment::Symbol(sym_id);
        if self
            .bag
            .iter_attachment(&symbol)
            .any(|w| matches!(&w.payload, WitnessPayload::Edge(t) if *t == arms))
        {
            return;
        }
        self.bag.push(Witness {
            attachment: symbol,
            source: WitnessSource::Builder("return_arm_chain".into()),
            payload: WitnessPayload::Edge(arms),
            span,
        });
    }

    pub(super) fn emit_expr_witness(&mut self, node: Node<'a>) {
        // A written element and the levels on a write's path are the write,
        // not drills: their keys extend the shape
        // (`emit_mutation_extension_witnesses`) instead of being read out of
        // it — unless the write reads the element first (`ElementWrite`).
        if matches!(node.kind(), "hash_element_expression" | "array_element_expression")
            && (self.on_write_path(node)
                || self.element_write_kind(node).is_some_and(|w| !w.reads_element()))
        {
            return;
        }
        if self.expr_type_depth >= Self::MAX_EXPR_TYPE_DEPTH {
            return;
        }
        self.expr_type_depth += 1;
        let out = self.emit_expr_witness_inner(node);
        self.expr_type_depth -= 1;
        out
    }

    fn emit_expr_witness_inner(&mut self, node: Node<'a>) {
        use crate::model::witnesses::{tags, Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let span = node_to_span(node);
        if node.kind() == "conditional_expression" {
            let arm_att = WitnessAttachment::BranchArm(span);
            let arms = [
                node.child_by_field_name("consequent"),
                node.child_by_field_name("alternative"),
            ];
            for arm in arms.into_iter().flatten() {
                // `undef` and the empty list `()` (a `stub_expression`,
                // which coerces to undef in scalar context) make the ternary
                // optional; mark either like a `return undef` arm so
                // `BranchArmFold` lifts `{T, undef}` to `Optional<T>`.
                let arm_undef = match arm.kind() {
                    "undef_expression" => Some(tags::UndefArm::Scalar),
                    "stub_expression" => Some(tags::UndefArm::EmptyList),
                    _ => None,
                };
                if let Some(spelling) = arm_undef {
                    self.bag.push(Witness {
                        attachment: arm_att.clone(),
                        source: WitnessSource::Builder(tags::FACT_UNDEF_ARM.into()),
                        payload: WitnessPayload::Fact {
                            family: tags::FACT_UNDEF_ARM.into(),
                            key: String::new(),
                            value: spelling.as_fact_value(),
                        },
                        span: node_to_span(arm),
                    });
                    continue;
                }
                // Make sure the arm's own Expr(span) carries its
                // payload, then collect it on the BranchArm attachment.
                self.emit_expr_witness(arm);
                self.bag.push(Witness {
                    attachment: arm_att.clone(),
                    source: WitnessSource::Builder("branch_arm".into()),
                    payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(arm))),
                    span: node_to_span(arm),
                });
            }
            // The ternary's value IS the agreed arm type: point its
            // Expr(span) at the BranchArm fold.
            self.bag.push(Witness {
                attachment: WitnessAttachment::Expr(span),
                source: WitnessSource::Builder("branch_arm".into()),
                payload: WitnessPayload::Edge(arm_att),
                span,
            });
            return;
        }
        // `LHS || RHS` / `LHS // RHS` — a short-circuit whose value is the
        // LHS when truthy/defined, else the RHS.
        //
        // EXCEPT a `shift`/`$_[N]`-LHS: `my $x = shift // 'd'` is the param-
        // DEFAULT idiom — the value IS the parameter (unknown type); the
        // literal is a definedness fallback, not a type claim. Folding it to
        // the literal poisons downstream narrowed uses (`return $x if
        // $x->isa(...)`). Leave it untyped so the param stays open.
        let fallback_operands = if node.kind() == "binary_expression"
            && matches!(self.get_operator_text(node).as_deref(), Some("||") | Some("//"))
        {
            node.child_by_field_name("left")
                .filter(|lhs| !self.is_param_pull(*lhs))
                .zip(node.child_by_field_name("right"))
        } else if crate::cst::assign_op(node) == Some(crate::cst::AssignOp::Fallback) {
            // `LHS ||= RHS` is `LHS = LHS || RHS`: the same two arms.
            node.child_by_field_name("left").zip(node.child_by_field_name("right"))
        } else {
            None
        };
        if let Some((lhs, rhs)) = fallback_operands {
            self.push_fallback_arms(span, lhs, rhs);
            return;
        }
        // Idempotent per span: the walk reaches many expressions twice
        // (child visit first, then the enclosing assignment/invocant
        // emitter). One "expression"-source witness per Expr(span) is
        // the contract; identical duplicates only bloat the bag.
        let already = self
            .bag
            .for_attachment(&WitnessAttachment::Expr(span))
            .iter()
            .any(|w| matches!(&w.source, WitnessSource::Builder(t) if t == "expression"));
        if already {
            return;
        }
        if let Some(payload) = self.expr_payload(node) {
            self.bag.push(Witness {
                attachment: WitnessAttachment::Expr(span),
                source: WitnessSource::Builder("expression".into()),
                payload,
                span,
            });
            // `$_[0]` is the call's receiver wherever a call site supplies
            // one (`sub me { $_[0] }` types `$o->me->me` to `$o`'s class);
            // `ReturnExprReducer` claims it ahead of the window projection
            // above, which answers when no receiver is in hand.
            if self.is_positional_receiver(node) {
                self.bag.push(Witness {
                    attachment: WitnessAttachment::Expr(span),
                    source: WitnessSource::Builder("expression".into()),
                    payload: WitnessPayload::ReturnExpr(
                        crate::model::witnesses::ReturnExpr::Receiver,
                    ),
                    span,
                });
            }
        } else {
            // `expr_payload` returned None — either the callee
            // sym isn't in the table yet (forward-defined sub),
            // or our parent visitor fired before this node's own
            // walker emitted its Ref (push-before-children
            // ordering). Both cases share one recovery: queue the
            // node and re-call `expr_payload` post-walk against
            // the now-final symbol table + refs.
            //
            // Don't queue node kinds `expr_payload` doesn't claim
            // — `expr_payload`'s match is the single source of
            // truth for "can this resolve later." Anything that
            // returns None for a structural reason (unsupported
            // syntax) will return None again post-walk, and the
            // retry is a no-op.
            self.unresolved_expr_nodes.push(node);
        }
    }

    /// Type `Expr(span)` as a short-circuit of `lhs` and `rhs`. The RHS is
    /// the guaranteed FLOOR (`$ENV{X} || 10` returns the literal default
    /// whenever the env var is unset), so it rides a distinct `fallback_arm`
    /// source; `BranchArmFold` prefers it when the arms disagree or the LHS
    /// can't be typed. Reuses the ternary's BranchArm machinery — one speller
    /// for "this expression's value is one of these arms"; `BranchArmFold`
    /// joins them.
    fn push_fallback_arms(&mut self, span: Span, lhs: Node<'a>, rhs: Node<'a>) {
        use crate::model::witnesses::{tags, Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let arm_att = WitnessAttachment::BranchArm(span);
        // Once per span, like every expression witness: a second copy of the
        // LHS arm would read to `BranchArmFold` as two agreeing arms.
        if !self.bag.for_attachment(&arm_att).is_empty() {
            return;
        }
        self.emit_expr_witness(lhs);
        self.bag.push(Witness {
            attachment: arm_att.clone(),
            source: WitnessSource::Builder("branch_arm".into()),
            payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(lhs))),
            span: node_to_span(lhs),
        });
        let exits = super::narrowing::is_exit_expression(rhs, self.source);
        self.bag.push(Witness {
            attachment: arm_att.clone(),
            source: WitnessSource::Builder(tags::FACT_SHORT_CIRCUIT.into()),
            payload: WitnessPayload::Fact {
                family: tags::FACT_SHORT_CIRCUIT.into(),
                key: String::new(),
                value: crate::model::witnesses::FactValue::Bool(exits),
            },
            span: node_to_span(rhs),
        });
        if !exits {
            self.emit_expr_witness(rhs);
            self.bag.push(Witness {
                attachment: arm_att.clone(),
                source: WitnessSource::Builder("fallback_arm".into()),
                payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(rhs))),
                span: node_to_span(rhs),
            });
        }
        self.bag.push(Witness {
            attachment: WitnessAttachment::Expr(span),
            source: WitnessSource::Builder("branch_arm".into()),
            payload: WitnessPayload::Edge(arm_att),
            span,
        });
    }

    /// A free call's `Invoke`: the callee as written, the local sub it
    /// resolved to (`None` = look it up across files), and its arguments.
    fn call_invoke(
        &mut self,
        node: Node<'a>,
        name: String,
        local: Option<SymbolId>,
    ) -> crate::model::witnesses::WitnessPayload {
        let args = self.extract_call_args(node);
        crate::model::witnesses::WitnessPayload::Invoke {
            callee: crate::model::witnesses::Callee::Function { name, local, member_of: None, constructs: None },
            receiver: None,
            args: Some(self.call_operands(&args)),
        }
    }

    /// A call this file never defines the callee of, once the symbol table is
    /// final: an `Invoke` the registry resolves through the callee's
    /// exporters at query time. A qualified call (`Foo::bar()`) names its
    /// package, which no exporter lookup asks, and a builtin is not a sub.
    fn imported_call_payload(&mut self, node: Node<'a>) -> Option<crate::model::witnesses::WitnessPayload> {
        if !matches!(node.kind(), "function_call_expression" | "ambiguous_function_call_expression")
            || self.is_bless_call(node)
            || self.indirect_method_call(node).is_some()
        {
            return None;
        }
        let called = self.forward_callee_name(node)?;
        let (qualifier, _) =
            crate::model::file_analysis::split_qualified(&called, &crate::model::conventions::PERL_SPELLINGS);
        if qualifier.is_some() || crate::model::builtins::is_builtin(&called) {
            return None;
        }
        Some(self.call_invoke(node, called, None))
    }

    /// Callee name for an expression node whose `expr_payload` arm mints a
    /// free call's `Invoke`, **as written** — a qualifier is kept, because
    /// `Foo::bar()` and `bar()` do not name the same sub and only the
    /// qualifier says which. `None` for any other kind. Sole source of truth
    /// for "what sub does this node reference" — `expr_payload`'s call arm
    /// calls this for the live lookup, and `find_callee_symbol` is what
    /// decides whether the qualifier admits a local sub.
    pub(super) fn forward_callee_name(&self, node: Node<'a>) -> Option<String> {
        let raw = match node.kind() {
            "function_call_expression" | "ambiguous_function_call_expression" => {
                node.child_by_field_name("function")?.utf8_text(self.source).ok()?
            }
            "bareword" | "scoped_identifier" => node.utf8_text(self.source).ok()?,
            _ => return None,
        };
        Some(raw.to_string())
    }

    /// Post-walk: re-call `expr_payload` on every queued node. By
    /// this point the symbol table is final (forward sub refs
    /// resolve) and every node's Ref has been emitted (parent
    /// visitors that fired before child walks now have refs to
    /// chase). Same `expression` source tag and span as the
    /// walk-time path, so reducers can't tell the two emission
    /// sites apart. Nodes that still return None (cross-file
    /// imports without a local sym, syntax `expr_payload` doesn't
    /// claim) are silently dropped — the bag is monotone, no
    /// negative witnesses.
    pub(super) fn resolve_forward_expr_witnesses(&mut self) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessSource};
        let queued = std::mem::take(&mut self.unresolved_expr_nodes);
        for node in queued {
            let Some(payload) = self.expr_payload(node).or_else(|| self.imported_call_payload(node))
            else {
                continue;
            };
            let span = node_to_span(node);
            self.bag.push(Witness {
                attachment: WitnessAttachment::Expr(span),
                source: WitnessSource::Builder("expression".into()),
                payload,
                span,
            });
        }
    }

    /// Publish witnesses for one explicit `return EXPR`:
    /// - `Expr(body_span)` is populated by `emit_expr_witness` —
    ///   directly with the body's payload for non-ternary, or
    ///   recursively with per-arm `branch_arm` Edges for ternary.
    /// - `SymbolReturnArm(sub_id)` gets one `Edge(Expr(body_span))`
    ///   witness per arm. `SymbolReturnArmFold` claims this
    ///   attachment and joins the arms (`join_return_arms`).
    /// - `Symbol(sub_id)` carries one `Edge(SymbolReturnArm(sub_id))`
    ///   per sub (`link_return_arms`) so consumers querying the
    ///   symbol's return walk through to the arm-fold.
    pub(super) fn publish_return_arm_witnesses(&mut self, return_node: Node<'a>, scope: ScopeId) {
        use crate::model::witnesses::tags;
        let body = crate::cst::nth_element(return_node, 0);
        let arm_span = body.map(node_to_span).unwrap_or_else(|| node_to_span(return_node));

        let Some(sub_name) = self.enclosing_sub_name() else { return };
        let Some(sym_id) = self.find_sub_symbol_for(&sub_name, scope) else { return };

        // A bare `return;`, `return undef`, and `return ()` are undef arms —
        // the sub's value is optional. `()` is a `stub_expression` and
        // coerces to undef in scalar context, which is the ternary side's
        // rule too (`emit_expr_witness`); the two spellings of "this arm is
        // undef" must not disagree about which syntax counts.
        //
        // None of the three has an rvalue type to ride an `Expr` edge, so
        // mark the arm with a Fact the fold counts; the join lifts
        // `{T, undef}` to `Optional<T>`, and all-undef to `Undef`.
        // Which undef spelling — see `tags::UndefArm`. A bare `return;` is
        // the EMPTY list in list context, so it groups with `()`, not with
        // `return undef`.
        let undef_arm = match body {
            None => Some(tags::UndefArm::EmptyList),
            Some(b) => match b.kind() {
                "undef_expression" => Some(tags::UndefArm::Scalar),
                "stub_expression" => Some(tags::UndefArm::EmptyList),
                _ => None,
            },
        };
        if let Some(spelling) = undef_arm {
            self.push_return_undef_arm(sym_id, arm_span, spelling);
        } else if let Some(body) = body {
            self.emit_expr_witness(body);
            self.push_return_value_arm(sym_id, arm_span);
        }
    }

    /// RHS-ternary convenience: `my $x = $c ? A : B` → one
    /// `chain_assignment`-source `Edge(Expr(ternary_span))` on
    /// Variable($x). `emit_expr_witness(cond_expr)` recurses to
    /// populate the ternary's per-arm `branch_arm` Edges on
    /// `Expr(ternary_span)`; BranchArmFold reduces them during edge
    /// chase, and the materialized Variable witness goes through
    /// FrameworkAwareTypeFold (which excludes `branch_arm` sources).
    /// Source must NOT be `branch_arm` here — there's only one Edge
    /// from the variable, BranchArmFold's ≥2-arm rule would reject it.
    pub(super) fn emit_branch_arm_witnesses_for_ternary(
        &mut self,
        lhs_var: &str,
        cond_expr: Node<'a>,
        at: Point,
    ) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let scope = self.current_scope();
        self.emit_expr_witness(cond_expr);
        // Zero-span at the write point: the synthetic InferredType
        // witness produced by edge materialization inherits this span,
        // and `FrameworkAwareTypeFold`'s point-contains filter only
        // skips *non-zero* spans that miss the query point. Using the
        // same convention as TC-mirror Variable witnesses.
        self.bag.push(Witness {
            attachment: WitnessAttachment::Variable { name: lhs_var.to_string(), scope },
            source: WitnessSource::Builder("chain_assignment".into()),
            payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(cond_expr))),
            span: Span { start: at, end: at },
        });
    }


    /// What does this anonymous-sub return, by inspecting the body's
    /// last expression? Emits the body's last-expression witness
    /// onto the bag, then queries the resolved type at that
    /// attachment. Used by Mojo `has 'x' => sub { [] }` to type the
    /// getter's return. Recursion handles `sub { sub { [] } }`:
    /// the outer's `bag_query` yields `CodeRef { return_edge }`,
    /// but the caller wants the inner sub's *return*, so we recurse
    /// into the nested anon-sub's body if the bag's CodeRef shape
    /// doesn't unwrap on its own.
    pub(super) fn infer_anonymous_sub_return_type(&mut self, node: Node<'a>) -> Option<InferredType> {
        if node.kind() != "anonymous_subroutine_expression" {
            return None;
        }
        let body = node.child_by_field_name("body")?;
        let last_stmt = crate::cst::last_element(body)?;
        let expr = match last_stmt.kind() {
            "expression_statement" | "return_expression" => crate::cst::nth_element(last_stmt, 0)?,
            _ => last_stmt,
        };
        self.emit_expr_witness(expr);
        self.bag_query_expr_span(node_to_span(expr))
            .or_else(|| self.infer_anonymous_sub_return_type(expr))
    }

    /// Push a type constraint on a variable node if it's a scalar.
    pub(super) fn push_var_type_constraint(&mut self, var_node: Node<'a>, context_node: Node<'a>, inferred_type: InferredType) {
        if var_node.kind() == "scalar" {
            if let Ok(text) = var_node.utf8_text(self.source) {
                self.push_type_constraint(TypeConstraint {
                    variable: text.to_string(),
                    scope: self.current_scope(),
                    constraint_span: node_to_span(context_node),
                    inferred_type,
                });
            }
        }
    }

    /// Infer the result type of an expression (not its operands — those are handled elsewhere).
    /// e.g. `$a + $b` produces Numeric, `$a . $b` produces String.
    pub(super) fn infer_expression_result_type(&self, node: Node<'a>) -> Option<InferredType> {
        match node.kind() {
            "binary_expression" => {
                let op = self.get_operator_text(node);
                match op.as_deref() {
                    Some("+" | "-" | "*" | "/" | "%" | "**") => Some(InferredType::Numeric),
                    Some("." | "x") => Some(InferredType::String),
                    _ => None,
                }
            }
            // A comparison yields a boolean, EXCEPT the ordering operators
            // `<=>` / `cmp` (which the grammar files under equality) — those
            // return -1/0/1, a number. `eq`/`ne` sort under equality too;
            // `lt`/`gt`/… under relational.
            "equality_expression" | "relational_expression" => {
                match self.get_operator_text(node).as_deref() {
                    Some("<=>" | "cmp") => Some(InferredType::Numeric),
                    Some(_) => Some(InferredType::Bool),
                    None => None,
                }
            }
            // `!$x` / `!!$x` is the boolify idiom; `not $x` its low-prec
            // spelling. `-$x` / `+$x` / `\$x` are also `unary_expression`,
            // so gate on the operator.
            "logical_not_expression" => Some(InferredType::Bool),
            "unary_expression" => match self.get_operator_text(node).as_deref() {
                Some("!") => Some(InferredType::Bool),
                _ => None,
            },
            "postinc_expression" | "preinc_expression" => Some(InferredType::Numeric),
            "func1op_call_expression" | "func0op_call_expression" => {
                let name = node.child(0)?.utf8_text(self.source).ok()?;
                if crate::model::conventions::is_current_package_token(name) {
                    // `package_at_pos`, not the scope walk: top-level code
                    // under `package Foo;` sits in the file scope, whose
                    // package is `main`.
                    return self.package_at_pos(node.start_position()).map(|p| InferredType::ClassName(p.to_string()));
                }
                crate::model::builtins::builtin_return_type(name)
            }
            _ => None,
        }
    }

    /// Infer a type on the first named child (the operand) of a dereference expression.
    /// An arrow deref (`$x->{k}`, `$x->[0]`, `$x->()`, `$x->@*`) observes
    /// its operand's representation.
    pub(super) fn infer_deref_type(&mut self, node: Node<'a>, rep: TypeObservation) {
        if let Some(operand) = node.named_child(0) {
            self.observe(operand, node, rep);
        }
    }

    /// Record a `$x->[i]` / `$x->()` deref whose receiver is a plain
    /// variable, for the deref diagnostics (the array/code analog of the
    /// method-call / hash-deref refs). A receiver that is a part of a
    /// variable is recorded by the place survey (`place_disturbances`),
    /// only when a narrowing names its root; a value (`f()->[0]`) is never
    /// narrowed, so it is skipped.
    pub(super) fn record_arrow_deref(
        &mut self,
        node: Node<'a>,
        receiver: Node<'a>,
        form: crate::model::file_analysis::DerefForm,
    ) {
        if !crate::cst::is_plain_scalar(receiver) {
            return;
        }
        let Some(name) = crate::cst::canonical_var_name(receiver, self.source) else { return };
        let dies_on_undef = self.deref_dies_on_undef(node, &form);
        self.arrow_deref_sites.push(crate::model::file_analysis::ArrowDerefSite {
            receiver: name,
            place: None,
            span: node_to_span(node),
            form,
            dies_on_undef,
        });
    }

    /// Whether the deref at `node` dies on an undef receiver: a call
    /// always does; an element access does where the `autovivification`
    /// hints in force forbid vivifying its receiver, and vivifies it
    /// otherwise.
    pub(super) fn deref_dies_on_undef(&self, node: Node<'a>, form: &crate::model::file_analysis::DerefForm) -> bool {
        use crate::model::file_analysis::{hint_in, vivify_dies_in, LexicalHint, VivifyAccess};
        if form.dies_on_undef() {
            return true;
        }
        let at = node.start_position();
        let scope = self.scope_at_point(at);
        // Most files never say `no autovivification`; asking the scopes
        // first spares every element access its climb to its context.
        let any_off = [VivifyAccess::Fetch, VivifyAccess::Store, VivifyAccess::Exists, VivifyAccess::Delete]
            .into_iter()
            .any(|a| hint_in(&self.scopes, scope, at, LexicalHint::NoVivify(a)));
        any_off && vivify_dies_in(&self.scopes, scope, at, crate::cst::vivify_context(node))
    }

    /// Infer types from binary operator expressions.
    pub(super) fn infer_binary_op_type(&mut self, node: Node<'a>) {
        let op = self.get_operator_text(node);
        match op.as_deref() {
            // Numeric operators: both operands are Numeric
            Some("+" | "-" | "*" | "/" | "%" | "**") => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        self.observe(child, node, TypeObservation::NumericUse);
                    }
                }
            }
            // String operators: both operands are String
            Some("." | "x") => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        self.observe(child, node, TypeObservation::StringUse);
                    }
                }
            }
            // Regex match: LHS is String, RHS is Regexp
            Some("=~" | "!~") => {
                if let Some(lhs) = crate::cst::nth_element(node, 0) {
                    self.observe(lhs, node, TypeObservation::StringUse);
                }
                if let Some(rhs) = crate::cst::nth_element(node, 1) {
                    self.observe(rhs, node, TypeObservation::RegexpUse);
                }
            }
            _ => {}
        }
    }

    /// Infer types from comparison operators (equality_expression, relational_expression).
    pub(super) fn infer_comparison_type(&mut self, node: Node<'a>) {
        let op = self.get_operator_text(node);
        let observed = match op.as_deref() {
            Some("==" | "!=" | "<=>" | "<" | ">" | "<=" | ">=") => Some(TypeObservation::NumericUse),
            Some("eq" | "ne" | "lt" | "gt" | "le" | "ge" | "cmp") => Some(TypeObservation::StringUse),
            _ => None,
        };
        if let Some(obs) = observed {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    self.observe(child, node, obs.clone());
                }
            }
        }
    }

    /// Get the operator text from a binary/comparison/equality expression.
    /// The operator is the first unnamed child between the two named children.
    pub(super) fn get_operator_text(&self, node: Node<'a>) -> Option<String> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if !child.is_named() {
                    let text = child.utf8_text(self.source).ok()?;
                    // Skip parens, brackets, etc.
                    if !matches!(text, "(" | ")" | "[" | "]" | "{" | "}" | "," | ";") {
                        return Some(text.to_string());
                    }
                }
            }
        }
        None
    }
}
