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
        let list = node
            .named_child(0)
            .filter(|c| c.kind() == "list_expression")
            .unwrap_or(node);
        let mut flat: Vec<Node<'a>> = Vec::new();
        crate::cst::flatten_list(list, &mut flat);
        let named: Vec<Node<'a>> = flat.into_iter().filter(|n| n.is_named()).collect();

        let mut keys: Vec<(String, Option<Box<InferredType>>)> = Vec::new();
        let mut open = false;
        let mut i = 0;
        while i < named.len() {
            let elem = named[i];
            if matches!(
                elem.kind(),
                "hash" | "hash_deref_expression" | "container_variable"
                    | "array" | "array_deref_expression"
            ) {
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
        if keys.is_empty() && !open {
            return InferredType::HashRef;
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
        let list = node
            .named_child(0)
            .filter(|c| c.kind() == "list_expression")
            .unwrap_or(node);
        let mut flat: Vec<Node<'a>> = Vec::new();
        crate::cst::flatten_list(list, &mut flat);
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

    pub(super) fn expr_payload(&mut self, node: Node<'a>) -> Option<crate::model::witnesses::WitnessPayload> {
        use crate::model::witnesses::{RefIdx, WitnessAttachment, WitnessPayload};
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
            // `element_at(n)` off it (docs/adr/destructuring.md).
            "list_expression" => self.list_literal_type(node).map(WitnessPayload::InferredType),
            "quoted_regexp" => Some(WitnessPayload::InferredType(InferredType::Regexp)),
            "anonymous_subroutine_expression" | "refgen_expression" => {
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
                if node.kind() == "anonymous_subroutine_expression" {
                    let params = self.extract_params(node);
                    self.ensure_anon_sub_symbol(node, &params);
                }
                let return_edge = self.coderef_return_edge_for(node);
                Some(WitnessPayload::InferredType(InferredType::CodeRef { return_edge }))
            }
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
                let span = node_to_span(node);
                let idx = self.refs.iter().position(|r| {
                    matches!(r.kind, RefKind::MethodCall { .. }) && r.span == span
                })?;
                Some(WitnessPayload::Edge(WitnessAttachment::Expression(RefIdx(
                    idx as u32,
                ))))
            }

            // Function call — Edge to the matching local
            // `Symbol(sid)`. For names that resolve to a local sub
            // (or to a plugin-synthesized sub, since plugin synths
            // also push into `self.symbols` before walk-time use),
            // the chase resolves through. Cross-file imports without
            // a local sym don't pin the Expr(span); chain typing's
            // own resolvers cover the chain-receiver case.
            // Function / bareword / scoped-identifier call — extract the
            // callee's bare name and Edge to its `Symbol(sid)`. Failures
            // (target not yet in symbol table) are recovered post-walk by
            // `resolve_forward_expr_witnesses`, which re-calls this same
            // `expr_payload` against the final symbol table.
            // `bless $ref, $class` is a value of type `ClassName($class)`
            // — closed under syntax when the class resolves. Covers the
            // anonymous-ref forms (`return bless {}, $class`) that have no
            // variable to promote; the variable form is additionally
            // promoted via `visit_bless_call`'s TC. Honest-miss (fall
            // through) when the class isn't determinable.
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
                if self.type_constraint_names.contains(&bare) {
                    let params = self.extract_constraint_params(node);
                    if let Some(inner) = self.plugins.type_constraint_inner(&bare, &params) {
                        return Some(WitnessPayload::InferredType(
                            InferredType::TypeConstraintOf(Box::new(inner)),
                        ));
                    }
                }
                let sid = self.find_callee_symbol(&called)?;
                Some(WitnessPayload::Edge(WitnessAttachment::Symbol(sid)))
            }

            // `$_[0]` in value position is the positional-receiver
            // pseudo-invocant (`sub me { return $_[0] }` — the
            // self-returning idiom). Its return value IS the call's
            // receiver, so emit the deferred `Receiver` placeholder:
            // `ReturnExprReducer` substitutes `q.receiver` at the call
            // site, letting `Symbol(me)` / `PackageSymbol{C, me}` type
            // a *chained* `$obj->me->me->...` to the receiver class at
            // arbitrary depth. A general `$arr[N]` read carries no
            // receiver semantics — leave it to the chain typer's
            // element-projection arm (no Expr payload).
            "array_element_expression" if self.is_positional_receiver(node) => {
                Some(WitnessPayload::ReturnExpr(crate::model::witnesses::ReturnExpr::Receiver))
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

    pub(super) fn emit_expr_witness(&mut self, node: Node<'a>) {
        
        if self.expr_type_depth >= Self::MAX_EXPR_TYPE_DEPTH {
            return;
        }
        self.expr_type_depth += 1;
        let out = self.emit_expr_witness_inner(node);
        self.expr_type_depth -= 1;
        out
    }

    fn emit_expr_witness_inner(&mut self, node: Node<'a>) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
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
                if matches!(arm.kind(), "undef_expression" | "stub_expression") {
                    self.bag.push(Witness {
                        attachment: arm_att.clone(),
                        source: WitnessSource::Builder("undef_arm".into()),
                        payload: WitnessPayload::Fact {
                            family: "undef_arm".into(),
                            key: String::new(),
                            value: crate::model::witnesses::FactValue::Bool(true),
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
        // LHS when truthy/defined, else the RHS. The RHS is the guaranteed
        // FLOOR (`$ENV{X} || 10` returns the literal default whenever the
        // env var is unset), so it rides a distinct `fallback_arm` source;
        // `BranchArmFold` prefers it when the arms disagree or the LHS can't
        // be typed. Reuses the ternary's BranchArm machinery — one speller
        // for "this expression's value is one of these arms."
        //
        // EXCEPT a `shift`/`$_[N]`-LHS: `my $x = shift // 'd'` is the param-
        // DEFAULT idiom — the value IS the parameter (unknown type); the
        // literal is a definedness fallback, not a type claim. Folding it to
        // the literal poisons downstream narrowed uses (`return $x if
        // $x->isa(...)`). Leave it untyped so the param stays open.
        if node.kind() == "binary_expression"
            && matches!(self.get_operator_text(node).as_deref(), Some("||") | Some("//"))
            && node
                .child_by_field_name("left")
                .is_some_and(|lhs| !self.is_param_pull(lhs))
        {
            if let (Some(lhs), Some(rhs)) =
                (node.child_by_field_name("left"), node.child_by_field_name("right"))
            {
                let arm_att = WitnessAttachment::BranchArm(span);
                self.emit_expr_witness(lhs);
                self.bag.push(Witness {
                    attachment: arm_att.clone(),
                    source: WitnessSource::Builder("branch_arm".into()),
                    payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(lhs))),
                    span: node_to_span(lhs),
                });
                self.emit_expr_witness(rhs);
                self.bag.push(Witness {
                    attachment: arm_att.clone(),
                    source: WitnessSource::Builder("fallback_arm".into()),
                    payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(rhs))),
                    span: node_to_span(rhs),
                });
                self.bag.push(Witness {
                    attachment: WitnessAttachment::Expr(span),
                    source: WitnessSource::Builder("branch_arm".into()),
                    payload: WitnessPayload::Edge(arm_att),
                    span,
                });
                return;
            }
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

    /// Callee name for an expression node whose `expr_payload` arm resolves
    /// to `Edge(Symbol(sid))`, **as written** — a qualifier is kept, because
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
            let Some(payload) = self.expr_payload(node) else { continue };
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
    ///   attachment and folds arms via `resolve_return_type`
    ///   (agreement / disagreement / single-arm).
    /// - `Symbol(sub_id)` gets one `Edge(SymbolReturnArm(sub_id))`
    ///   chain witness so consumers querying the symbol's return
    ///   walk through to the arm-fold. Pushed per arm — duplicates
    ///   are idempotent (same target, same materialized result).
    pub(super) fn publish_return_arm_witnesses(&mut self, return_node: Node<'a>, scope: ScopeId) {
        use crate::model::witnesses::{
            FactValue, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };
        let body = return_node.named_child(0);
        let arm_span = body.map(node_to_span).unwrap_or_else(|| node_to_span(return_node));

        let Some(sub_name) = self.enclosing_sub_name() else { return };
        let Some(sym_id) = self.find_sub_symbol_for(&sub_name, scope) else { return };

        // A bare `return;` and `return undef` are undef arms — the sub's
        // value is optional. Neither has an rvalue type to ride an `Expr`
        // edge, so mark the arm with a Fact the fold counts; the arm join
        // lifts `{T, undef}` to `Optional<T>`.
        let is_undef_arm = match body {
            None => true,
            Some(b) => b.kind() == "undef_expression",
        };
        if is_undef_arm {
            self.bag.push(Witness {
                attachment: WitnessAttachment::SymbolReturnArm(sym_id),
                source: WitnessSource::Builder("undef_arm".into()),
                payload: WitnessPayload::Fact {
                    family: "undef_arm".into(),
                    key: String::new(),
                    value: FactValue::Bool(true),
                },
                span: arm_span,
            });
        } else if let Some(body) = body {
            self.emit_expr_witness(body);
            self.bag.push(Witness {
                attachment: WitnessAttachment::SymbolReturnArm(sym_id),
                source: WitnessSource::Builder("return_arm".into()),
                payload: WitnessPayload::Edge(WitnessAttachment::Expr(arm_span)),
                span: arm_span,
            });
        }
        self.bag.push(Witness {
            attachment: WitnessAttachment::Symbol(sym_id),
            source: WitnessSource::Builder("return_arm_chain".into()),
            payload: WitnessPayload::Edge(WitnessAttachment::SymbolReturnArm(sym_id)),
            span: arm_span,
        });
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
        context: Node<'a>,
    ) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let scope = self.current_scope();
        let context_span = node_to_span(context);
        self.emit_expr_witness(cond_expr);
        // Zero-span at the assignment start: the synthetic InferredType
        // witness produced by edge materialization inherits this span,
        // and `FrameworkAwareTypeFold`'s point-contains filter only
        // skips *non-zero* spans that miss the query point. Using the
        // same convention as TC-mirror Variable witnesses.
        self.bag.push(Witness {
            attachment: WitnessAttachment::Variable { name: lhs_var.to_string(), scope },
            source: WitnessSource::Builder("chain_assignment".into()),
            payload: WitnessPayload::Edge(WitnessAttachment::Expr(node_to_span(cond_expr))),
            span: Span { start: context_span.start, end: context_span.start },
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
        let last_stmt = body.named_child(body.named_child_count().checked_sub(1)?)?;
        let expr = match last_stmt.kind() {
            "expression_statement" | "return_expression" => last_stmt.named_child(0)?,
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
                crate::model::builtins::builtin_return_type(name)
            }
            _ => None,
        }
    }

    /// Infer a type on the first named child (the operand) of a dereference expression.
    pub(super) fn infer_deref_type(&mut self, node: Node<'a>, narrowing: InferredType) {
        if let Some(operand) = node.named_child(0) {
            // The narrowing is observational — `$cb->()` says
            // "$cb is a coderef", `${$x}` says "$x is a hashref",
            // etc. — and doesn't reveal payload (no body span
            // from the deref site). If the operand is ALREADY
            // typed with a witness at least as informative as
            // this narrowing, the TC would only ever clobber
            // richer payload under latest-wins reduction (the
            // motivating regression: a `my $cb = sub { ... }`
            // literal's `CodeRef { return_edge: Some(_) }` losing
            // its edge to the subsequent `$cb->()`'s
            // `CodeRef { return_edge: None }`). Skip in that case.
            if let Some(existing) = self.invocant_type_at_node(operand) {
                if existing.subsumes_narrowing(&narrowing) {
                    return;
                }
            }
            self.push_var_type_constraint(operand, node, narrowing);
        }
    }

    /// Record a `$x->[i]` / `$x->()` arrow deref whose receiver is a plain
    /// scalar, for the deref diagnostics (the array/code analog of the
    /// method-call / hash-deref refs). Only a scalar operand can be undef /
    /// Optional / rep-narrowed; a chain receiver (`f()->[0]`) is skipped.
    pub(super) fn record_arrow_deref(&mut self, node: Node<'a>, form: crate::model::file_analysis::DerefForm) {
        let Some(operand) = node.named_child(0) else { return };
        if operand.kind() != "scalar" {
            return;
        }
        let Ok(text) = operand.utf8_text(self.source) else { return };
        if !text.starts_with('$') {
            return;
        }
        self.arrow_deref_sites.push(crate::model::file_analysis::ArrowDerefSite {
            receiver: text.to_string(),
            span: node_to_span(node),
            form,
        });
    }

    /// Infer types from binary operator expressions.
    pub(super) fn infer_binary_op_type(&mut self, node: Node<'a>) {
        let op = self.get_operator_text(node);
        match op.as_deref() {
            // Numeric operators: both operands are Numeric
            Some("+" | "-" | "*" | "/" | "%" | "**") => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        self.push_var_type_constraint(child, node, InferredType::Numeric);
                    }
                }
            }
            // String operators: both operands are String
            Some("." | "x") => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        self.push_var_type_constraint(child, node, InferredType::String);
                    }
                }
            }
            // Regex match: LHS is String, RHS is Regexp
            Some("=~" | "!~") => {
                if let Some(lhs) = node.named_child(0) {
                    self.push_var_type_constraint(lhs, node, InferredType::String);
                }
                if let Some(rhs) = node.named_child(1) {
                    self.push_var_type_constraint(rhs, node, InferredType::Regexp);
                }
            }
            _ => {}
        }
    }

    /// Infer types from comparison operators (equality_expression, relational_expression).
    pub(super) fn infer_comparison_type(&mut self, node: Node<'a>) {
        let op = self.get_operator_text(node);
        let inferred = match op.as_deref() {
            // Numeric comparisons
            Some("==" | "!=" | "<=>" | "<" | ">" | "<=" | ">=") => Some(InferredType::Numeric),
            // String comparisons
            Some("eq" | "ne" | "lt" | "gt" | "le" | "ge" | "cmp") => Some(InferredType::String),
            _ => None,
        };
        if let Some(it) = inferred {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    self.push_var_type_constraint(child, node, it.clone());
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
