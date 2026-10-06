//! Build-time symbolic execution: `resolve_invocant_class_tree` /
//! `invocant_type_at_node` (the single chain typer).

use super::*;

impl<'a> Builder<'a> {
    /// Type any expression node in invocant position. Single
    /// kind-dispatch shared between `resolve_invocant_class_tree`
    /// (which projects to a class name string) and the manifest
    /// recorders / `type` projection (which use the InferredType
    /// directly). Routes everything through the bag
    /// where the bag has the answer; falls back to walk-time
    /// canonical sources (TCs, current_package) only where the bag
    /// can't (variable invocants whose TC mirror happens post-walk).
    ///
    /// Resolves:
    ///   - `Foo->new(...)` (constructor)                       → ClassName(Foo)
    ///   - `X->method(...)` chain                              → bag chase via Expression(refidx)
    ///   - `$self`                                             → ClassName(current_package)
    ///   - `__PACKAGE__`                                       → ClassName(current_package)
    ///   - `$var`                                              → TC for `$var`, then bag
    ///   - bareword `Foo` (sub with ClassName return)          → that class
    ///   - bareword `Foo` (no such sub)                        → ClassName(Foo)
    ///   - `shift` / `$_[0]` in method body                    → ClassName(current_package)
    ///   - `get_foo()->bar()` outer's invocant                 → bag query Symbol(get_foo, arity)
    /// `@`/`%` containers and unrecognized kinds return `None`.
    pub(super) fn invocant_type_at_node(&self, node: Node<'a>) -> Option<InferredType> {
        match node.kind() {
            "method_call_expression" => {
                // Constructor pattern is closed under syntax — bake
                // before consulting the bag. Same shape the walker's
                // `expr_payload` uses for every rvalue expression.
                if let Some(class) = self.extract_constructor_class(node) {
                    return Some(InferredType::ClassName(class));
                }
                // Dynamic-method dispatch: `$obj->$cb(args)`. The parser
                // shapes this as `method_call_expression` with a
                // `method` field whose first named child is a `scalar`,
                // not a bareword. Semantically it's `$cb->($obj,
                // args)` — the coderef in `$cb` is invoked with `$obj`
                // as the receiver. Route through the same edge chase
                // as `coderef_call_expression`'s arm: chase the
                // scalar's `CodeRef.return_edge` with `$obj`'s type
                // as `q.receiver` and `arity = args.len()` (no
                // first-arg-is-receiver subtraction here — the
                // method-call syntax already accounts for the
                // receiver via the `invocant` field).
                if let Some(scalar) = crate::cst::dynamic_method_scalar(node) {
                    let cb_ty = self.invocant_type_at_node(scalar)?;
                    let target = cb_ty.callable_return_edge()?.clone();
                    let invocant_ty = node
                        .child_by_field_name("invocant")
                        .and_then(|inv| self.invocant_type_at_node(inv));
                    let arity = self.extract_call_args(node).len() as u32;
                    return self.bag_query_attachment_with(
                        &target,
                        Some(arity),
                        invocant_ty,
                    );
                }
                let span = node_to_span(node);
                let idx = self.refs.iter().position(|r| {
                    matches!(r.kind, RefKind::MethodCall { .. }) && r.span == span
                })?;
                // Arity from the method-call node disambiguates fluent
                // accessors (Mojo::Base `has 'title' => 'default'`
                // synthesizes a 0-arg getter returning String AND a
                // 1-arg writer returning $self). Without it the
                // PackageSymbol chase's `UnionOnArgs` reducer would
                // hit the writer's `Any`/`AtLeast(1)` arm regardless
                // of how many args the call has.
                //
                // Receiver = the invocant's resolved type. Threads
                // through `PackageSymbol` chases so
                // `ReturnExpr::Operator(RowOf(Receiver))` (DBIC find)
                // and `UnionOnArgs::Receiver` (Mojo writer) substitute
                // to the right value-side answer.
                let arity = self.extract_call_args(node).len() as u32;
                let invocant_ty = node
                    .child_by_field_name("invocant")
                    .and_then(|inv| self.invocant_type_at_node(inv));
                let call_ty = self.bag_query_expression(
                    crate::model::witnesses::RefIdx(idx as u32),
                    Some(arity),
                    invocant_ty.clone(),
                );
                if call_ty.is_some() {
                    return call_ty;
                }
                // The `Expression(refidx)` chase came up empty. Two
                // receiver-relative fallbacks let a chain hop resolve
                // DURING the fold — before `emit_method_call_return_edges`
                // / `emit_invocant_expr_witnesses` (both post-fold) publish
                // this call's edge. Without them, the variable a fluent /
                // projecting chain is bound to (`my $art = $rs->search(
                // ...)->first`) never gets a build-time TC, so hover /
                // goto-def on it (which read the TC) stay dark.
                //
                // (a) Fluent verb (`$rs->search`) — returns its invocant's
                //     type unchanged, so the receiver IS the answer.
                if self.is_fluent_verb_call(node) {
                    if invocant_ty.is_some() {
                        return invocant_ty;
                    }
                }
                // (b) Receiver-relative projection (`->first`/`->create` →
                //     RowOf) declared on `PackageSymbol{recv.class, method}`
                //     by `emit_parametric_return_expr_decls` (published in
                //     the live walk, so it IS in the bag now). Query it with
                //     the receiver threaded so `RowOf(Receiver)` projects to
                //     the row class.
                if let Some(recv) = &invocant_ty {
                    if let Some(cls) = recv.class_name() {
                        if let Some(mtext) = node
                            .child_by_field_name("method")
                            .and_then(|m| m.utf8_text(self.source).ok())
                        {
                            let method = crate::model::conventions::MethodToken::parse(mtext)
                                .name()
                                .to_string();
                            let moc = crate::model::witnesses::WitnessAttachment::PackageSymbol {
                                package: cls.to_string(),
                                name: method,
                            };
                            if let Some(t) = self.bag_query_attachment_with(
                                &moc,
                                Some(arity),
                                invocant_ty.clone(),
                            ) {
                                return Some(t);
                            }
                        }
                    }
                }
                None
            }
            "coderef_call_expression" => {
                // `$cb->(args)` — value-type IS whatever the operand's
                // `CodeRef.return_edge` resolves to. Resolve the
                // operand recursively (via this same dispatch),
                // pull its callable return target, and chase it
                // through the bag.
                //
                // Arity / receiver semantics depend on the target
                // attachment shape:
                //   - `PackageSymbol{...}` → the first arg IS the
                //     method's receiver (Perl's `\&Class::method`
                //     semantics: invoking via coderef requires
                //     passing the invocant as arg[0]). Drop it from
                //     the arity count and surface its type as
                //     `q.receiver` so `ReturnExpr::Receiver`
                //     substitutes correctly. Also covers the
                //     `\&foo` case (bare named sub) — today's
                //     `coderef_return_edge_for` emits PackageSymbol
                //     even for non-method subs in the current
                //     package; that's a known caveat (would be
                //     wrong for a plain sub treated as a method),
                //     but it matches the existing arity dispatch
                //     contract: the synth's UnionOnArgs branches
                //     are written in receiver-relative arity.
                //   - `Symbol(_)` (anon subs, in-file named subs)
                //     → no receiver convention,
                //     `q.receiver = None`, arity = args.len().
                //     UnionOnArgs branches that mention `Receiver`
                //     get `None`-substituted; Concrete arms work
                //     unchanged.
                //   - `Expr(_)` fallback (parse-error recovery for
                //     anon subs whose Symbol stash didn't fire) →
                //     same as Symbol: opaque body span, no
                //     receiver, arity = args.len().
                let operand = node.named_child(0)?;
                let target = self
                    .invocant_type_at_node(operand)?
                    .callable_return_edge()?
                    .clone();
                let args = self.extract_call_args(node);
                let (arity, receiver) = match &target {
                    crate::model::witnesses::WitnessAttachment::PackageSymbol { .. } => {
                        let recv_ty = args
                            .first()
                            .and_then(|n| self.invocant_type_at_node(*n));
                        let arity = (args.len() as u32).saturating_sub(1);
                        (Some(arity), recv_ty)
                    }
                    _ => (Some(args.len() as u32), None),
                };
                self.bag_query_attachment_with(&target, arity, receiver)
            }
            "scalar" => {
                let text = node.utf8_text(self.source).ok()?;
                // Const-folded class string: `my $c = 'Counter'; $c->bump`.
                // In invocant position a known constant string IS the
                // dispatch class — the same fold dynamic method names use
                // on the other slot of the arrow. Single candidate only (a
                // multi-valued fold can't pin one class); checked before
                // the bag, whose honest answer for `$c` is the degenerate
                // `String`.
                let canonical = crate::cst::canonical_var_name(node, self.source);
                let fold_key = canonical.as_deref().unwrap_or(text);
                if let Some([class]) = self.resolve_constant_strings(fold_key, 0).as_deref() {
                    return Some(InferredType::ClassName(class.clone()));
                }
                // The bag is canonical at every phase, walk-time
                // included: `push_type_constraint` mirrors every TC
                // into a Variable witness live during the walk, so
                // `bag_query_variable` always sees whatever was just
                // seeded by an earlier visit (plus the framework-aware
                // `FirstParam → ClassName` projection).
                let scope = self.scope_at_point(node.start_position());
                self.bag_query_variable(text, scope, node.start_position())
            }
            "func0op_call_expression" => self.infer_expression_result_type(node),
            "bareword" | "package" => {
                let text = node.utf8_text(self.source).ok()?;
                // Bareword invocant is ambiguous: class-name reference
                // OR zero-arg function call whose return type seeds
                // the chain (`app->routes` where `sub app :: Mojolicious`).
                // Prefer the function-call interpretation; fall back
                // to treating the text as a class. A qualified bareword the
                // file declares no package for is a class name, never a local
                // sub — `Foo::Bar` must not bind to some `sub Bar` here.
                if let Some(bare) = self.local_callee_name(text) {
                    if let Some(t) = self.bag_query_named_sub(bare, Some(0)) {
                        return Some(t);
                    }
                }
                Some(InferredType::ClassName(text.to_string()))
            }
            // A bare `shift` READS the head of `@_`'s window at its point;
            // the consume is the walk's `consume_arg_head`, once per node.
            // This arm re-runs every fold iteration, so it must stay a read.
            "func1op_call_expression" if self.is_shift_call(node) => {
                self.arg_window_at(node)?.element_at(0).cloned()
            }
            "array_element_expression" => {
                // Arrow deref on an expression (`$x->[0]`,
                // `$obj->{users}->[0]`) has no `array` field — the base is
                // the first named child; its Sequence projects the element.
                let Some(array) = node.child_by_field_name("array") else {
                    let base = node.named_child(0)?;
                    let idx_node = node.child_by_field_name("index")?;
                    let idx: i32 = idx_node.utf8_text(self.source).ok()?.parse().ok()?;
                    return self.invocant_type_at_node(base)?.element_at(idx).cloned();
                };
                let varname = array.named_child(0)?;
                let index = node.child_by_field_name("index")?;
                // General `$arr[N]` — `$_[N]` included, `@_` being the
                // argument window: read the Sequence shape from
                // the bag and project the index. Mirror of
                // `FileAnalysis::resolve_expression_type`'s array arm.
                let name = varname.utf8_text(self.source).ok()?;
                let idx: i32 = index.utf8_text(self.source).ok()?.parse().ok()?;
                let arr_var = format!("@{}", name);
                let scope = self.scope_at_point(node.start_position());
                self.bag_query_variable(&arr_var, scope, node.start_position())
                    .and_then(|t| t.element_at(idx).cloned())
            }
            "hash_element_expression" => {
                // A hash element's VALUE type is independent of the
                // container's class — never `$self`'s class.
                let base = node.named_child(0)?;
                let base_ty = self.invocant_type_at_node(base)?;
                let key_node = node.child_by_field_name("key")?;
                let (key, is_dynamic) = self.extract_key_text(key_node)?;
                if is_dynamic {
                    return None;
                }
                // Structurally-typed hash: the literal told us the per-key
                // type — `$config->{db}` narrows to it, and nesting recurses
                // for free (the inner literal's HashWithKeys rides in the
                // value slot).
                if let Some(v) = base_ty.key_value_type(&key) {
                    return v.cloned();
                }
                // Class-typed base: a typed write to this slot
                // (`SlotType{class,key}`, seeded at the write site, agreed
                // by `SlotTypeFold`) is the honest answer; otherwise
                // untyped.
                let class = base_ty.class_name()?.to_string();
                self.bag_query_attachment(&crate::model::witnesses::WitnessAttachment::SlotType {
                    class,
                    key,
                })
            }
            "function_call_expression" | "ambiguous_function_call_expression" => {
                if self.is_shift_call(node) {
                    return self.arg_window_at(node)?.element_at(0).cloned();
                }
                let func = node.child_by_field_name("function")?;
                let name = func.utf8_text(self.source).ok()?;
                // The cross-file arm below pairs this tail with an EXPLICITLY
                // resolved package, so it is entitled to the bare name; only
                // the local lookup has to clear the qualifier gate.
                let bare = bare_name(name);
                let arg_count = self.extract_call_args(node).len() as u32;
                if let Some(local) = self.local_callee_name(name) {
                    if let Some(t) = self.bag_query_named_sub(local, Some(arg_count)) {
                        return Some(t);
                    }
                }
                // Parity with the method form: an imported function resolves
                // its return like the remote class method it aliases, so a
                // Mojo::Lite `under('/x')` (which calls
                // `Mojolicious::Routes::Route::under`) types as Route just like
                // `$r->under('/x')` — else the route value never brands and a
                // partial `->to('#action')` downstream loses its inherited
                // controller. Import map pins the class; the override lives on
                // `PackageSymbol{package, verb}`. Only reached on a local/cross-
                // file miss, so it strictly adds answers (None → maybe Some).
                let class = self.resolve_call_package(name)?;
                self.bag_query_attachment_with(
                    &crate::model::witnesses::WitnessAttachment::PackageSymbol {
                        package: class,
                        name: bare.to_string(),
                    },
                    Some(arg_count),
                    None,
                )
            }
            _ => None,
        }
    }

    /// Read a string-ish arg node's literal value (string content or
    /// bareword/autoquoted key text). `None` for non-literal args.
    pub(super) fn literal_arg_string(&self, arg: Node<'a>) -> Option<String> {
        match arg.kind() {
            "string_literal" | "interpolated_string_literal" => {
                self.extract_string_content(arg)
            }
            "autoquoted_bareword" | "bareword" => {
                arg.utf8_text(self.source).ok().map(|s| s.to_string())
            }
            _ => None,
        }
    }

    /// Resolve a method-call invocant NODE to a class name. Thin
    /// wrapper over `invocant_type_at_node` that projects the
    /// resulting `InferredType` to a class string. Same caller
    /// contract: callable both walk-time (variable arms read TCs,
    /// the bag has plugin/framework synthesis answers) and
    /// post-walk (the bag is canonical for everything).
    pub(super) fn resolve_invocant_class_tree(&self, node: Node<'a>) -> Option<String> {
        let t = self.invocant_type_at_node(node)?;
        if let Some(c) = t.class_name() {
            return Some(c.to_string());
        }
        // `FirstParam` projection: a TC carrying `FirstParam{package}`
        // (the `my $self = shift;` idiom) pins the package even
        // when `class_name()` doesn't recognize the variant.
        if let InferredType::FirstParam { package } = t {
            return Some(package);
        }
        // Last fallback for bareword nodes: if the bag returned
        // something non-class, treat the syntactic text as the class.
        // Pre-bag-routing this was the bareword arm's `Some(text.to_string())`
        // tail; preserved here so unrelated InferredType variants
        // (Numeric / String / etc.) on a bareword still degrade to
        // the class-name interpretation rather than vanishing.
        if matches!(node.kind(), "bareword" | "package") {
            return node.utf8_text(self.source).ok().map(|s| s.to_string());
        }
        None
    }
    /// The active topic-route DSL, when a plugin declared one whose
    /// gating module is in scope. Core knows no names — the plugin
    /// manifest carries the module, the verbs, and the scope function.
    pub(super) fn active_topic_dsl(&self) -> Option<&plugin::TopicRouteDsl> {
        self.topic_dsls.iter().find(|d| {
            self.package_uses
                .values()
                .any(|ms| ms.iter().any(|m| *m == d.module))
        })
    }

    /// The called function's name when `inv` is a call — the generic
    /// syntax fact the `call_name` projection carries to plugins.
    pub(super) fn invocant_call_name(&self, inv: Node<'a>) -> Option<String> {
        if !matches!(
            inv.kind(),
            "function_call_expression" | "ambiguous_function_call_expression"
        ) {
            return None;
        }
        inv.child_by_field_name("function")?
            .utf8_text(self.source)
            .ok()
            .map(str::to_string)
    }

    /// Transitive parent walk within the current file. Depth-limited like
    /// `resolve_method_in_ancestors`. Returns parents in BFS order. Used
    /// for plugin trigger matching so a class that transitively extends
    /// `Mojo::EventEmitter` (via an intermediate base) still fires its
    /// plugins — matches Perl's own MRO behavior.
    pub(super) fn transitive_parents(&self, pkg: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stack: Vec<String> = self.package_parents.get(pkg).cloned().unwrap_or_default();
        let mut depth = 0;
        while let Some(p) = stack.pop() {
            if depth > 20 { break; }
            if !seen.insert(p.clone()) { continue; }
            out.push(p.clone());
            if let Some(grandparents) = self.package_parents.get(&p) {
                for gp in grandparents {
                    stack.push(gp.clone());
                }
            }
            depth += 1;
        }
        out
    }
}
