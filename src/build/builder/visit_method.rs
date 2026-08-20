//! Method-call visitor and friends: func1op/refgen, group accessors,
//! hash-element and anonymous-hash visitors.

use super::*;

impl<'a> Builder<'a> {
    /// Handle func1op_call_expression: abs($x), length($s), int($n), etc.
    /// The function name is the first child (keyword), the arg is a named child.
    pub(super) fn visit_func1op(&mut self, node: Node<'a>) {
        let name = node.child(0)
            .and_then(|c| c.utf8_text(self.source).ok())
            .unwrap_or("");
        // Push type constraint on the argument
        if let Some(arg_type) = crate::model::builtins::builtin_first_arg_type(name) {
            if let Some(arg) = node.named_child(0) {
                self.push_var_type_constraint(arg, node, arg_type);
            }
        }
        self.queue_children(node);
    }

    /// Handle `\&name` and `\&Pkg::name` refgen expressions.
    /// Emits a FunctionCall ref at the sub-name span so goto-def and references
    /// resolve to the sub definition. The `expr_payload` / `coderef_return_edge_for`
    /// paths own the CodeRef type witness; this visitor owns the navigation ref.
    pub(super) fn visit_refgen(&mut self, node: Node<'a>) {
        // Extract name(s) — `extract_names_from_refgen` handles plain names,
        // qualified names, and const-foldable `\&$var` cases.
        let names = self.extract_names_from_refgen(node);
        if !names.is_empty() {
            // The `function` child wraps the `&name` form; use the varname
            // node's span so the ref lands on the identifier, not the `&`.
            let name_span = node.named_child(0).and_then(|func_node| {
                // func_node is aliased as "function"; its named child is "varname".
                func_node.named_child(0).map(node_to_span)
            }).unwrap_or_else(|| node_to_span(node));

            for name in names {
                let pkg = self.resolve_call_package(&name);
                self.add_bound_ref(
                    RefKind::FunctionCall,
                    name_span,
                    name,
                    AccessKind::Read,
                    pkg.map(|package| crate::model::file_analysis::RefBinding::Function { package }),
                );
            }
        }
        // Still descend: `\@array`, `\%hash`, `\$scalar` children may carry
        // variable refs that the walk needs to see.
        self.queue_children(node);
    }

    /// Extract the first argument node from a function call.
    pub(super) fn first_call_arg(&self, call_node: Node<'a>) -> Option<Node<'a>> {
        let args = call_node.child_by_field_name("arguments")?;
        match args.kind() {
            "list_expression" | "parenthesized_expression" => args.named_child(0),
            _ => Some(args), // single arg (ambiguous_function_call_expression)
        }
    }

    pub(super) fn visit_method_call(&mut self, node: Node<'a>) {
        use crate::cst::NodeExt;
        let call = crate::cst::MethodCall::cast(node)
            .expect("visit_node dispatches method_call_expression here");
        let method_node = call.method();
        let method_name = method_node
            .and_then(|n| n.text(self.source))
            .map(|s| s.to_string());
        // A fully-qualified call (`$o->Foo::Bar::m`) keeps the full path in
        // `target_name` but narrows the renamable span to the `m` tail (rule
        // #7), mirroring FunctionCall — so rename rewrites only the method.
        let method_name_span = match (method_node, method_name.as_deref()) {
            (Some(n), Some(name)) => crate::cst::fq_tail_span(n, name),
            _ => node.span(),
        };
        let invocant_node = call.invocant();
        // Canonical sigiled name for variable invocants (`${sner}` records
        // as `$sner`) so downstream bag lookups hit the variable's key; raw
        // text for everything else.
        let invocant_text = invocant_node.and_then(|n| {
            crate::cst::canonical_var_name(n, self.source)
                .or_else(|| n.text(self.source).map(|s| s.to_string()))
        });
        let invocant = invocant_text.as_ref().map(|s| {
            // Resolve __PACKAGE__ to enclosing package name. This is THE
            // canonical producer for ref invocants — varname spelling
            // normalized above, package token resolved here.
            if crate::model::conventions::is_current_package_token(s) {
                crate::model::conventions::Invocant::assume_canonical(
                    self.current_package.clone().unwrap_or_else(|| s.to_string()),
                )
            } else {
                crate::model::conventions::Invocant::assume_canonical(s)
            }
        });
        // Stored even when walk-time can't resolve the class — PostFold's
        // `apply_chain_typing_invocants` needs the span to find the node
        // and fill `invocant_class`, else class-scoped `refs_to` matches
        // too broadly.
        let invocant_span = invocant_node.map(|n| n.span());

        // Walk-time invocant_class: closed-under-syntax cases only
        // (constructor chain `Sner->new->hi`, `__PACKAGE__->m`, a scalar
        // holding a const-folded class string). Everything
        // inference-dependent stays None for PostFold to fill from the bag.
        let invocant_class = invocant_node.and_then(|n| match n.kind() {
            "method_call_expression" => self.extract_constructor_class(n),
            "bareword" | "package"
                if n.utf8_text(self.source).ok().is_some_and(crate::model::conventions::is_current_package_token) =>
            {
                self.current_package.clone()
            }
            _ => None,
        });

        let args = self.extract_call_args(node);

        if let Some(ref name) = method_name {
            // Dynamic method dispatch: $self->$method() — resolve $method if known
            if name.starts_with('$') {
                // Record the dynamic-dispatch site regardless of whether
                // const-folding resolves it: folding is best-effort, so the
                // dispatched method may still be invisible to the static graph.
                // The heatmap's dead-code pass reads this as a soundness gate.
                self.dynamic_dispatch_sites = self.dynamic_dispatch_sites.saturating_add(1);
                if let Some(resolved) = self.resolve_constant_strings(name, 0) {
                    // The call token is the `$m` read, not a name literal — its
                    // rewrite target is the source string the fold came from. Only
                    // a single-literal binding has one unambiguous source span.
                    let folded_src = (resolved.len() == 1)
                        .then(|| self.constant_string_source.get(name).copied())
                        .flatten();
                    for rname in resolved {
                        let idx = self.refs.len();
                        self.add_ref(
                            RefKind::MethodCall {
                                invocant: invocant.clone().unwrap_or_default(),
                                invocant_span,
                                method_name_span,
                                member_op: None,
                            },
                            node_to_span(node),
                            rname,
                            AccessKind::Read,
                        );
                        self.refs[idx].folded_from = folded_src;
                        if let Some(c) = invocant_class.clone() {
                            self.method_call_invocant.insert(idx, c);
                        }
                        self.method_call_arity
                            .insert(idx, args.len() as u32);
                    }
                }
            } else {
                let idx = self.refs.len();
                self.add_ref(
                    RefKind::MethodCall {
                        invocant: invocant.clone().unwrap_or_default(),
                        invocant_span,
                        method_name_span,
                        member_op: None,
                    },
                    node_to_span(node),
                    name.clone(),
                    AccessKind::Read,
                );
                if let Some(c) = invocant_class.clone() {
                    self.method_call_invocant.insert(idx, c);
                }
                self.method_call_arity
                    .insert(idx, args.len() as u32);

                // Runtime-exporter setup in method-call form:
                // `Moose::Exporter->setup_import_methods(...)`,
                // `__PACKAGE__->add_type({ name => 'X' })`. The invocant
                // package isn't load-bearing (Type::Library subclasses inherit
                // `add_type`), so gate on the enclosing package having `use`d
                // the relevant exporter — otherwise an unrelated
                // `$x->add_type(...)` would pollute `export_ok`.
                if matches!(name.as_str(), "setup_import_methods" | "add_type")
                    && self.package_uses_moose_exporter_or_type_library()
                {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        self.detect_exporter_setup_call(name, args);
                    }
                }

                // `__PACKAGE__->source_name('X')` overrides this result
                // class's DBIC source moniker (default = class basename).
                // Class-level call only; the string arg is the moniker.
                if name == "source_name" {
                    let pkg_receiver = node
                        .child_by_field_name("invocant")
                        .and_then(|inv| inv.utf8_text(self.source).ok())
                        .is_some_and(|t| {
                            crate::model::conventions::is_current_package_token(t)
                                || self.current_package.as_deref() == Some(t)
                        });
                    if pkg_receiver {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(sn) = self.first_string_literal_arg(args) {
                                self.dbic_source_name = Some(sn);
                            }
                        }
                    }
                }

                // `recv->resultset('Foo')` is closed under syntax: push the
                // `Parametric` type at the call's `Expression(refidx)`, and
                // mark the ref so `emit_method_call_return_edges` skips its
                // standard `Edge(PackageSymbol)` — that edge would resolve to
                // plain `resultset` and mask the row-class arg.
                if let Some(ty) = self.extract_resultset_parametric(node) {
                    self.parametric_emitted_refs.insert(idx);
                    let r_span = self.refs[idx].span;
                    self.bag.push(crate::model::witnesses::Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Expression(
                            crate::model::witnesses::RefIdx(idx as u32),
                        ),
                        source: crate::model::witnesses::WitnessSource::Builder(
                            "parametric_resultset".into(),
                        ),
                        payload: crate::model::witnesses::WitnessPayload::InferredType(ty.clone()),
                        span: r_span,
                    });

                    // Symbol-declarative ReturnExpr declarations on
                    // `PackageSymbol{base, method}` are the single
                    // source of truth for the per-flavor projection
                    // table (find → RowOf, etc.). The chain typer's
                    // method-call arm threads the call's invocant as
                    // `q.receiver`, so direct (`$rs->find(...)`),
                    // coderef (`\&MyRS::find; $cb->($rs, ...)`), and
                    // dynamic-method (`$rs->$cb(...)`) routes all
                    // resolve through the same `Operator(RowOf(
                    // Receiver))` substitution.
                    self.emit_parametric_return_expr_decls(&ty);
                } else if self.is_fluent_verb_call(node) {
                    // A fluent verb (`$rs->search`) returns its invocant's type
                    // UNCHANGED. Edge the call's type to the invocant's rather
                    // than minting one — any invocant type flows through, no
                    // re-mint (edges-not-values). Skip the standard PackageSymbol
                    // edge: `search` has no return-type def to resolve through.
                    if let RefKind::MethodCall { invocant_span: Some(inv_span), .. } =
                        self.refs[idx].kind
                    {
                        self.parametric_emitted_refs.insert(idx);
                        let r_span = self.refs[idx].span;
                        self.bag.push(crate::model::witnesses::Witness {
                            attachment: crate::model::witnesses::WitnessAttachment::Expression(
                                crate::model::witnesses::RefIdx(idx as u32),
                            ),
                            source: crate::model::witnesses::WitnessSource::Builder(
                                "fluent_passthrough".into(),
                            ),
                            payload: crate::model::witnesses::WitnessPayload::Edge(
                                crate::model::witnesses::WitnessAttachment::Expr(inv_span),
                            ),
                            span: r_span,
                        });
                    }
                }
            }
        }

        // Bareword invocant that's really a function call (`app->routes`
        // where `app` is a plugin-emitted Sub returning Mojolicious).
        // The wider MethodCall ref covers the whole `app->routes` span,
        // so cursor-on-`app` currently lands on a ref that describes
        // `routes` — no semantic token on the bareword, no hover/gd
        // targeting the sub itself. Emitting a narrower FunctionCall
        // ref at the bareword span makes ref_at prefer it (innermost
        // wins), which lets the existing FunctionCall paths (semantic
        // tokens, hover, gd) light up the bareword as the call it is.
        if let Some(inv_node) = invocant_node {
            if matches!(inv_node.kind(), "bareword" | "package") {
                if let Ok(bw_text) = inv_node.utf8_text(self.source) {
                    // Find the matching sub and capture its package so
                    // the FunctionCall ref's `resolved_package` points
                    // at the real definer — otherwise find_definition
                    // and hover_info's package-scoped match miss it.
                    let matched_pkg = self.symbols.iter().find_map(|s| {
                        if s.name != bw_text { return None; }
                        if !matches!(s.kind, SymKind::Sub | SymKind::Method) { return None; }
                        Some(s.package.clone())
                    });
                    if let Some(pkg) = matched_pkg {
                        self.add_bound_ref(
                            RefKind::FunctionCall,
                            node_to_span(inv_node),
                            bw_text.to_string(),
                            AccessKind::Read,
                            pkg.map(|package| {
                                crate::model::file_analysis::RefBinding::Function { package }
                            }),
                        );
                    } else if !crate::model::conventions::is_current_package_token(bw_text) {
                        // Class-name invocant (`Foo->bar`): the bareword is a
                        // package, not a local sub. Emit a narrower PackageRef
                        // at the invocant span so cursor-on-`Foo` resolves to
                        // the `package Foo` decl (local via find_package_or_class,
                        // cross-file via the module index) exactly like `use Foo`,
                        // instead of falling through to the wider MethodCall ref
                        // that describes `bar`. ref_at prefers the narrower span,
                        // so the `bar` method-token goto-def (NAV-A) is untouched.
                        // `__PACKAGE__` is excluded — it's a keyword, not a name.
                        self.add_ref(
                            RefKind::PackageRef,
                            node_to_span(inv_node),
                            bw_text.to_string(),
                            AccessKind::Read,
                        );
                    }
                }
            }
        }

        // DBIC accessor synthesis: __PACKAGE__->add_columns(...), ->has_many(...), etc.
        let is_pkg_call = invocant_text.as_deref().is_some_and(crate::model::conventions::is_current_package_token)
            || (invocant_node.map(|n| n.kind()) == Some("package")
                && invocant_text.as_ref() == self.current_package.as_ref());
        if is_pkg_call {
            if let Some(ref name) = method_name {
                // load_components / load_own_components — register components
                // as parents for method resolution. Works for any class (DBIC,
                // Catalyst, etc.) — components are mixins. `load_own_components`
                // resolves bare names against the CURRENT package's namespace;
                // `load_components` against `DBIx::Class`.
                if name == "load_components" {
                    self.visit_load_components(node, "DBIx::Class");
                } else if name == "load_own_components" {
                    if let Some(pkg) = self.current_package.clone() {
                        self.visit_load_components(node, &pkg);
                    }
                }
                // DBIC column/relationship synthesis lives in the `dbic`
                // plugin (`ClassIsa("DBIx::Class")`), fed `ctx.arg_names`.
                // Class::Accessor::Grouped accessor synthesis. Not DBIC-gated:
                // any package can `use parent 'Class::Accessor::Grouped'`. The
                // call shape is the signal — `mk_*_accessors('group', @names)`
                // (first arg is the group, the rest are accessor names) and
                // `mk_classdata('name')` (single name).
                self.visit_group_accessors(node, name);
            }
        }

        // Even-position stringy args become `HashKeyAccess` refs
        // owned by `Sub{invocant_class, method_name}` — pairs with
        // the HashKeyDef symbols that `has` / `bless { … }`
        // synthesize on the callee side. Emission is deferred to
        // post-walk (`emit_method_call_arg_keys`) so the owner's
        // class can be read off the canonical `invocant_class`
        // (filled by `apply_chain_typing_invocants` against the
        // bag) instead of the partially-resolved walk-time value.
        // The chain-typing index records the args node by call
        // span; the post-walk pass joins refs to args via that
        // span.

        // Trigger-independent manifest recording (dispatch verbs,
        // module loads). Runs for every method call, gated cheaply by
        // the per-verb manifest probe inside each recorder — no arg
        // extraction happens for non-manifest verbs.
        if let Some(ref name) = method_name {
            self.record_provisional_dispatch(name, node, invocant_node);
            self.record_plugin_loads(name, node, invocant_node);
        }

        self.queue_children(node);
    }

    /// Class::Accessor::Grouped accessor synthesis. The `mk_group_*_accessors`
    /// family takes a leading group name and then a list of accessor names
    /// (strings / barewords / qw lists); `mk_classdata` is just a name list.
    /// We synthesize a stub `Method` symbol per accessor name — same as DBIC
    /// `add_columns`. The accessor-name list is the authoritative source.
    pub(super) fn visit_group_accessors(&mut self, node: Node<'a>, method_name: &str) {
        let skip_first = match method_name {
            "mk_group_accessors"
            | "mk_group_ro_accessors"
            | "mk_group_rw_accessors"
            | "mk_group_wo_accessors" => true,
            "mk_classdata" | "mk_classaccessor" => false,
            _ => return,
        };
        let Some(args) = node.child_by_field_name("arguments") else { return };
        let arg_nodes: Vec<Node> = match args.kind() {
            "list_expression" | "parenthesized_expression" => {
                (0..args.named_child_count())
                    .filter_map(|i| args.named_child(i))
                    .collect()
            }
            // Single bare arg, e.g. `mk_classdata('config')`.
            _ => vec![args],
        };

        let mut names: Vec<(String, Span)> = Vec::new();
        for child in arg_nodes.iter().skip(skip_first as usize) {
            match child.kind() {
                "string_literal" | "interpolated_string_literal" => {
                    if let Some(text) = self.extract_string_content(*child) {
                        names.push((text, self.string_content_span(*child)));
                    }
                }
                "bareword" | "autoquoted_bareword" => {
                    if let Ok(text) = child.utf8_text(self.source) {
                        names.push((text.to_string(), node_to_span(*child)));
                    }
                }
                "quoted_word_list" | "anonymous_array_expression" => {
                    self.extract_array_attr_names(*child, &mut names);
                }
                _ => {}
            }
        }

        self.emit_class_accessor_symbols(node, &names);
    }

    /// Synthesize a stub class-data `Method` symbol per `(name, selection-span)`.
    /// Shared by `visit_group_accessors` (direct `mk_*` call args) and the
    /// statement-modifier loop path (`mk_classdata($_) for qw/.../`); `def_node`
    /// anchors the definition span (the call), each name carries its own select
    /// span so rename/goto land on the source token.
    pub(super) fn emit_class_accessor_symbols(&mut self, def_node: Node<'a>, names: &[(String, Span)]) {
        for (name, sel_span) in names {
            self.add_symbol(
                name.clone(),
                SymKind::Method,
                node_to_span(def_node),
                *sel_span,
                SymbolDetail::Sub {
                    params: vec![ParamInfo {
                        name: "$val".into(),
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

    pub(super) fn visit_hash_element(&mut self, node: Node<'a>) {
        // Infer HashRef on the operand variable (e.g. $x in $x->{key})
        self.infer_deref_type(node, InferredType::HashRef);

        // Record the hash variable access. Container form (`$h{k}`,
        // grammar field `hash:`) reads `%h`, not scalar `$h` — use the
        // canonical name so the key ref, the shape witness, and the
        // KeyWrite all key the same variable.
        let var_text = match node.child_by_field_name("hash") {
            Some(c) => crate::cst::canonical_container_name(c, self.source)
                .or_else(|| self.get_hash_var_from_element(node)),
            None => self.get_hash_var_from_element(node),
        };

        // Distinguish read vs write by asking determine_access on the
        // element node itself — `$self->{k} = ...` has this element as
        // the LHS of an assignment, so the grandparent check returns
        // Write. Needed for invocant mutations.
        let element_access = self.determine_access(node);

        // READ drills get their Projected witness here, not only when
        // an enclosing assignment/invocant emitter happens to reach
        // them — a bare-statement `cfg()->{k};` is still a drill the
        // expression-base diagnostic must see. Writes stay
        // witness-less: a write extends the producer's shape, it isn't
        // a typo to hint on.
        if element_access != AccessKind::Write {
            self.emit_expr_witness(node);
        }

        // A method-call container (`$obj->get_config->{host}`) has no
        // variable identity to anchor the key on — its owner is the
        // chain receiver's *return type*, knowable only post-fold. The
        // `emit_chained_hash_key_refs` pass owns that shape: it emits a
        // ref only when the type resolves (and stays silent otherwise,
        // so no orphan owner-`None` ref is left behind). Emit here only
        // for the variable-container shape.
        let container_is_method_call = node
            .named_child(0)
            .map(|c| c.kind() == "method_call_expression")
            .unwrap_or(false);

        // Record the key access
        if !container_is_method_call {
            if let Some(key_node) = node.child_by_field_name("key") {
                if let Some((key_text, is_dynamic)) = self.extract_key_text(key_node) {
                    if !is_dynamic {
                        // Owner resolved in post-pass (`resolve_hash_key_owners`).
                        self.add_ref(
                            RefKind::HashKeyAccess {
                                var_text: var_text.clone().unwrap_or_default(),
                            },
                            node_to_span(key_node),
                            key_text,
                            element_access,
                        );
                    }
                }
            }
        }

        // Visit children for the container variable ref
        self.queue_children(node);
    }

    pub(super) fn visit_anon_hash(&mut self, node: Node<'a>) {
        // Detect bless context for hash key ownership
        let owner = self.detect_anon_hash_owner(node);

        // Collect hash-literal keys as HashKeyDef symbols
        if let Some(ref owner) = owner {
            let keys = self.collect_pair_keys(node, owner);
            // A blessed hash's keys are instance slots of the class — the same
            // `$self->{key}` slots a Moo `has` mints an `InternalKey` for
            // (rule #9: provenance — bless key → internal hash key). Emit the
            // projection so rename/references from the constructor key reach
            // the `Class(C)`-owned `$self->{key}` accesses via the group's
            // `InternalHashKey` member. A `return { ... }` hash is NOT instance
            // slots (its keys are the sub's return shape, consumed `Sub`-owned),
            // so only the bless context mints this.
            if let HashKeyOwner::Sub { package: Some(class), .. } = owner {
                if self.anon_hash_is_blessed(node) {
                    for key in keys {
                        self.attr_projections.push(crate::model::file_analysis::AttrProjection {
                            class: class.clone(),
                            attr: key,
                            kind: crate::model::file_analysis::AttrProjectionKind::InternalKey,
                        });
                    }
                }
            }
        }

        self.queue_children(node);
    }

    /// Is this anon-hash node the operand of a `bless` call (within 5
    /// ancestors)? Mirrors `detect_anon_hash_owner`'s bless branch — bless
    /// keys are instance slots; `return { ... }` keys are not.
    pub(super) fn anon_hash_is_blessed(&self, anon_hash: Node<'a>) -> bool {
        let mut ancestor = anon_hash.parent();
        for _ in 0..5 {
            let Some(a) = ancestor else { return false };
            if self.is_bless_call(a) {
                return true;
            }
            ancestor = a.parent();
        }
        false
    }

    /// `recv->resultset('Foo')` → `Parametric(ResultSet { base,
    /// row })`. `base` is discovered via the
    /// `<NS>::Result::<X>` ↔ `<NS>::ResultSet::<X>` convention if
    /// that class exists in this file; otherwise falls back to
    /// `DBIx::Class::ResultSet` (the universal DBIC default that
    /// runtime DBIC creates dynamically when no custom resultset
    /// class is defined). The fallback hardcode is core-resident
    /// for now — the DBIC plugin (queued, see
    /// `docs/prompt-dbic-as-plugin.md`) will own it once the port
    /// lands.
    ///
    /// First arg must be a string literal — a non-literal
    /// (variable, computed) means the row class is dynamic and
    /// we don't claim.
    pub(super) fn extract_resultset_parametric(&self, node: Node<'a>) -> Option<InferredType> {
        use crate::model::file_analysis::ParametricType;
        if node.kind() != "method_call_expression" {
            return None;
        }
        let method = node.child_by_field_name("method")?;
        let mtext = method.utf8_text(self.source).ok()?;
        // `recv->resultset('Foo')` — row class from the string arg.
        if mtext == "resultset" {
            let args = node.child_by_field_name("arguments")?;
            let row_class = self.first_string_or_constfold_arg(args)?;
            let base = self
                .discover_resultset_class(&row_class)
                .unwrap_or_else(|| "DBIx::Class::ResultSet".to_string());
            return Some(InferredType::Parametric(ParametricType::ResultSet { base, row: row_class }));
        }
        None
    }

    /// Is this call a plugin-declared FLUENT verb (`$rs->search`)? A fluent verb
    /// returns its invocant's type UNCHANGED — whatever it is — so the call site
    /// edges the call's type to the invocant's rather than minting one (DBIC's
    /// `search`/`search_rs` keep a resultset a resultset; this stays generic).
    /// The verb list is the plugin's (#10/#8).
    pub(super) fn is_fluent_verb_call(&self, node: Node<'a>) -> bool {
        node.kind() == "method_call_expression"
            && node
                .child_by_field_name("method")
                .and_then(|m| m.utf8_text(self.source).ok())
                .is_some_and(|m| self.plugins.fluent_verbs().any(|v| v == m))
    }

    /// Push `ReturnExpr` declarations on `PackageSymbol{base, m}`
    /// for every projection method the flavor declares. Called
    /// after each `extract_resultset_parametric` hit so the chain
    /// typer's coderef-edge / dynamic-method / inheritance routes
    /// all see the same answer through the bag's standard
    /// `PackageSymbol` chase. Latest-wins among duplicates: same
    /// `(base, method)` pair seen twice (two `resultset(...)` calls
    /// in one file) re-publishes the same ReturnExpr — the
    /// reducer's content equality on Operator(RowOf(Receiver))
    /// makes that a no-op.
    ///
    /// Idempotent across re-runs: `EXTRACT_VERSION` bumps when
    /// `WitnessPayload::ReturnExpr` lands, so cached blobs from
    /// before this phase don't carry stale declarations. Within
    /// one build, the worklist driver doesn't call this — emission
    /// happens once during the live walk.
    pub(super) fn emit_parametric_return_expr_decls(&mut self, ty: &InferredType) {
        let Some(p) = ty.as_parametric() else { return };
        // The flavor's own dispatch class pins the slot — no per-variant
        // match, so a flavor with no declarations (empty vec) is a no-op.
        let Some(base_class) = p.class_name().map(|s| s.to_string()) else { return };
        let zero = Span {
            start: Point { row: 0, column: 0 },
            end: Point { row: 0, column: 0 },
        };
        for (method_name, return_expr) in p.return_method_declarations() {
            self.bag.push(crate::model::witnesses::Witness {
                attachment: crate::model::witnesses::WitnessAttachment::PackageSymbol {
                    package: base_class.clone(),
                    name: method_name.to_string(),
                },
                source: crate::model::witnesses::WitnessSource::Builder(
                    "parametric_return_expr".into(),
                ),
                payload: crate::model::witnesses::WitnessPayload::ReturnExpr(return_expr),
                span: zero,
            });
        }
    }

    /// Like `first_string_literal_arg` but additionally const-folds
    /// a scalar arg (`$sner` where `my $sner = 'Foo'` exists in
    /// scope). Used by `extract_resultset_parametric` so
    /// `$schema->resultset($sner)` types the same as
    /// `$schema->resultset('Foo')` when `$sner` is a known
    /// compile-time constant. Multi-value const-folding (a scalar
    /// that resolves to multiple strings) bails — Parametric's
    /// `row` is a single class, not a sum type yet.
    pub(super) fn first_string_or_constfold_arg(&self, args: Node<'a>) -> Option<String> {
        if let Some(s) = self.first_string_literal_arg(args) {
            return Some(s);
        }
        // Try the first arg as a `scalar` node with const-foldable
        // value. Same one-level unwrap rule as the literal helper.
        let arg_node = match args.kind() {
            "scalar" => Some(args),
            "parenthesized_expression" | "list_expression" => {
                let mut found: Option<Node<'a>> = None;
                for i in 0..args.named_child_count() {
                    if let Some(c) = args.named_child(i) {
                        found = Some(c);
                        break;
                    }
                }
                found
            }
            _ => None,
        }?;
        if arg_node.kind() != "scalar" {
            return None;
        }
        let var_text = arg_node.utf8_text(self.source).ok()?;
        let folded = self.resolve_constant_strings(var_text, 0)?;
        // Single-value fold only — multi-value (loop variable that
        // takes several strings) doesn't have a single row class to
        // emit. Future sum-types work could lift this.
        if folded.len() == 1 {
            folded.into_iter().next()
        } else {
            None
        }
    }

    /// Discover the resultset class for a given row class via the
    /// `<NS>::Result::<X>` ↔ `<NS>::ResultSet::<X>` convention.
    /// Returns `Some(class)` only when the discovered class is
    /// declared in the file's symbols (`SymKind::Package` or
    /// `SymKind::Class`). Cross-file discovery (looking up
    /// `<NS>::ResultSet::<X>` in `module_index`) is queued with
    /// the DBIC plugin port — most projects keep the row class
    /// and resultset class in sibling files of the same dist,
    /// so this in-file discovery covers the common case until
    /// the plugin lands.
    pub(super) fn discover_resultset_class(&self, row_class: &str) -> Option<String> {
        if !row_class.contains("::Result::") {
            return None;
        }
        let candidate = row_class.replacen("::Result::", "::ResultSet::", 1);
        let exists = self.symbols.iter().any(|s| {
            s.name == candidate
                && matches!(s.kind, SymKind::Package | SymKind::Class)
        });
        if exists { Some(candidate) } else { None }
    }

    /// First named child of a call-arg node that's a string literal,
    /// returning its content. Returns None for non-literal first
    /// args (variables, computed expressions). Shared between the
    /// resultset emission and any future parametric-by-literal
    /// emission rule.
    pub(super) fn first_string_literal_arg(&self, args: Node<'a>) -> Option<String> {
        // Single-arg method calls land here with `args` itself a
        // `string_literal` node (no surrounding paren / list
        // expression). Multi-arg calls wrap in
        // `list_expression`/`parenthesized_expression`. Handle the
        // bare-string case first, then recurse into wrappers.
        match args.kind() {
            "string_literal" | "interpolated_string_literal" => {
                return self.extract_string_content(args);
            }
            "parenthesized_expression" | "list_expression" => {
                for i in 0..args.named_child_count() {
                    let child = args.named_child(i)?;
                    return match child.kind() {
                        "string_literal" | "interpolated_string_literal" => {
                            self.extract_string_content(child)
                        }
                        "parenthesized_expression" | "list_expression" => {
                            self.first_string_literal_arg(child)
                        }
                        _ => None,
                    };
                }
            }
            _ => {}
        }
        None
    }
}
