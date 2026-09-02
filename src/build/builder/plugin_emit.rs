//! Plugin dispatch at walk time: `on_use` fan-out, provisional dispatch
//! recording, and `apply_emit_action` — the one sink for plugin emissions.

use super::*;

impl<'a> Builder<'a> {
    /// Run every applicable plugin's `on_use` hook. Used for
    /// `use` statements that autoimport a fixed verb set (Mojolicious::Lite,
    /// Dancer2, etc.) — plugins emit `FrameworkImport` actions per
    /// imported verb so our unresolved-function diagnostic skips them.
    ///
    /// `on_use` bypasses the normal trigger filter: every plugin sees
    /// every use. The plugin checks the module name itself. This is
    /// because `use` statements are the *place where* triggers become
    /// true for a package — the UsesModule("X") trigger wouldn't match
    /// at the exact statement that introduces X.
    pub(super) fn dispatch_use_plugins(&mut self, ctx: plugin::UseContext) {
        if self.plugins.is_empty() { return; }
        let actions: Vec<(String, plugin::EmitAction)> = self.plugins
            .all()
            .flat_map(|p| {
                let id = p.id().to_string();
                p.on_use(&ctx).into_iter().map(move |a| (id.clone(), a))
            })
            .collect();
        for (plugin_id, action) in actions {
            self.apply_emit_action(plugin_id, action);
        }
    }

    /// Record a provisional dispatch when a method call matches a plugin
    /// `dispatch_verbs()` declaration. The receiver isa check happens later,
    /// at enrichment, against the cross-file-resolved receiver class — here
    /// we just capture the candidate (name + span + the build-time receiver
    /// class hint, if any). Trigger-independent: keyed off the global verb
    /// manifest, not the file's `use`s, so a `$minion->enqueue('T')` in a
    /// plain class is captured the same as one in a Mojo app.
    pub(super) fn record_provisional_dispatch(
        &mut self,
        method: &str,
        node: Node<'a>,
        invocant_node: Option<Node<'a>>,
    ) {
        let Some(spec) = self.dispatch_manifest.get(method).cloned() else { return };
        let args_flat = self.flat_call_args(self.extract_call_args(node));
        let Some(arg_node) = args_flat.get(spec.name_arg_index).copied() else { return };
        let info = self.arg_info_for(arg_node);
        let Some(name) = info.string_value else { return };
        if name.is_empty() {
            return;
        }
        let span = info.span;
        let receiver_class = match invocant_node.and_then(|n| self.invocant_type_at_node(n)) {
            Some(InferredType::ClassName(c)) => Some(c),
            _ => None,
        };
        let call_span = node_to_span(node);
        // Gate the candidate on the verb's `target_class`: the inner
        // payload is unreadable until a receiver isa-resolves at query time.
        self.provisional_dispatches.push(crate::model::file_analysis::ReceiverGated::new(
            spec.target_class.clone(),
            crate::model::file_analysis::DispatchCandidate {
                name,
                span,
                dispatcher: method.to_string(),
                owner_class: spec.owner_class.clone(),
                receiver_class,
                call_span,
            },
        ));
    }

    /// Record `PluginLoadFact`s for a method-form load verb
    /// (`$app->plugin(...)`), TRIGGER-INDEPENDENTLY — the nested-plugin
    /// cascade runs inside `Mojolicious::Plugin` files that no Mojo
    /// trigger matches, so gating on the file class loses every load
    /// past the entrypoint (the receiver-gated dispatch lesson,
    /// `docs/adr/receiver-gated-dispatch.md`).
    ///
    /// Multi-value by construction: `ctx.args[i].string_values` carries
    /// the full constant-fold — a `qw(...)` loop topic, a `$_` postfix
    /// fold, OR a folded scalar constant — so `$app->plugin($_) for
    /// qw/A B C/` records A, B, AND C, and `$app->plugin(SOME_CONST)`
    /// records the folded name. The load fact is a SUPPRESSION signal
    /// for the entrypoint lint, so an untyped receiver records (honest-
    /// silent); the verb-name specificity + workspace-only + tail-match
    /// bound any false suppression. config_span (arg i+1) rides through
    /// for loader-config `$conf` typing.
    pub(super) fn record_plugin_loads(
        &mut self,
        method: &str,
        node: Node<'a>,
        invocant_node: Option<Node<'a>>,
    ) {
        let Some(spec) = self.load_manifest.get(method).cloned() else { return };
        let args_flat = self.flat_call_args(self.extract_call_args(node));
        let Some(arg_node) = args_flat.get(spec.name_arg_index).copied() else { return };
        let names = self.arg_info_for(arg_node).string_values;
        if names.is_empty() {
            return;
        }
        // Skip a load on a receiver KNOWN to be a different concrete
        // class — the only confident negative we have at walk time
        // (the app receiver is param-typed, hence None here).
        if let Some(InferredType::ClassName(c)) =
            invocant_node.and_then(|n| self.invocant_type_at_node(n))
        {
            if c != spec.receiver_class
                && !self.package_isa_local(&c, &spec.receiver_class)
            {
                // an untyped/None receiver still records; a confirmed
                // foreign class does not. cross-file isa isn't available
                // at build, so this only fires on a locally-known class.
                if self.package_parents.contains_key(&c) {
                    return;
                }
            }
        }
        // Emit the config value's Expr witness so a cross-file
        // `expr_type_at_span(config_span)` (the loader-config `$conf`
        // join in `record_loader_shapes`) resolves the value's shape.
        let config_span = match args_flat.get(spec.name_arg_index + 1) {
            Some(&cfg) => {
                self.emit_expr_witness(cfg);
                Some(node_to_span(cfg))
            }
            None => None,
        };
        for n in names {
            if n.is_empty() {
                continue;
            }
            self.plugin_loads.push(crate::model::file_analysis::PluginLoadFact {
                name: n,
                config_span,
            });
        }
    }

    /// Local-only isa: does `child` reach `ancestor` through this
    /// file's `package_parents`? THE isa seam's build-time face:
    /// `class_isa` over the builder's still-accumulating parent map (a
    /// `LocalParents` impl), no index at build. Same walker, same
    /// budget as every other isa question — this was the one isa walk
    /// with no bound at all (seen-set only), and "no cycle" is a weaker
    /// guarantee than "terminates in bounded time".
    pub(super) fn package_isa_local(&self, child: &str, ancestor: &str) -> bool {
        crate::model::file_analysis::class_isa(child, ancestor, &self.package_parents, None)
    }

    /// Convert a plugin-produced `EmitAction` into real builder state. All
    /// emitted symbols carry a `Namespace::Framework { id }` tag so downstream
    /// queries can distinguish plugin-synthesized entities from native ones.
    pub(super) fn apply_emit_action(&mut self, plugin_id: String, action: plugin::EmitAction) {
        let ns = Namespace::framework(plugin_id.clone());
        match action {
            // SetRouteBase is meaningful only in the fold-phase pattern
            // dispatch, where the driver intercepts it before
            // `apply_emit_action` to update the replayed topic base; a
            // walk-phase SetRouteBase has no live stack to write and is
            // ignored.
            plugin::EmitAction::SetRouteBase { .. } => {}
            plugin::EmitAction::Diagnostic {
                message,
                span,
                severity,
                code,
            } => {
                self.plugin_diagnostics
                    .push(crate::model::file_analysis::PluginDiagnostic {
                        message,
                        span,
                        severity,
                        code,
                        plugin_id: plugin_id.clone(),
                    });
            }
            plugin::EmitAction::Method {
                name,
                span,
                selection_span,
                params,
                is_method,
                return_type,
                doc,
                on_class,
                display,
                hide_in_outline,
                opaque_return,
                outline_label,
                attr,
                return_via_edge,
            } => {
                let return_type_for_bag = return_type.clone();
                let detail = SymbolDetail::Sub {
                    params: params.into_iter().map(Into::into).collect(),
                    is_method,
                    doc,
                    opaque_return,
                    is_constant: false,
                    lexical: false,
                };
                let target_pkg = on_class.clone().or_else(|| self.current_package.clone());
                // Projection-group enrollment: the plugin declared which
                // attr this accessor projects. Recorded on the analysis so
                // the group machinery (references/rename union) can find
                // name-mapped members (`has_size` for attr `size`).
                if let (Some(attr_name), Some(cls)) = (attr.clone(), target_pkg.clone()) {
                    self.attr_projections.push(
                        crate::model::file_analysis::AttrProjection::accessor(
                            cls,
                            attr_name,
                            name.clone(),
                        ),
                    );
                }

                let already_emitted = self.symbols.iter().any(|s| {
                    s.name == name
                        && s.kind == SymKind::Method
                        && s.package == target_pkg
                        && s.namespace == ns
                });
                if already_emitted { return; }

                let sid = if let Some(pkg) = on_class {
                    let saved = self.current_package.take();
                    self.current_package = Some(pkg);
                    let id = self.add_symbol_ns(name, SymKind::Method, span, selection_span, detail, ns);
                    self.current_package = saved;
                    id
                } else {
                    self.add_symbol_ns(name, SymKind::Method, span, selection_span, detail, ns)
                };
                // Stamped post-mint; kept out of `add_symbol_ns` so the
                // core constructor stays narrow (defaults for the
                // builder-native paths, plugin policy only here).
                *self.presentation_mut(sid) = crate::model::file_analysis::Presentation {
                    hide_in_outline,
                    doc: None,
                    deprecation: None,
                    display,
                    label: outline_label,
                };
                // Mirror the return type into the bag so walk-time and
                // post-walk consumers see the plugin-synthesized sub
                // through the same bag-query path locals + imports
                // already use. Symbol(sid) is pushed unconditionally —
                // class-scoped and free-fn synth both publish here.
                // Writeback's `PackageSymbol{package, name}` mirror reads
                // back through the registry's Edge(Symbol(sid)) chase,
                // so the per-class slot is populated for class-scoped
                // synth too. Bridges remain the dispatch mechanism for
                // plugin namespaces (CLAUDE.md rule #8).
                if let Some(rt) = return_type_for_bag {
                    use crate::model::witnesses::{
                        Witness, WitnessAttachment, WitnessPayload, WitnessSource,
                    };
                    self.bag.push(Witness {
                        attachment: WitnessAttachment::Symbol(sid),
                        source: WitnessSource::Plugin(plugin_id.clone()),
                        payload: WitnessPayload::InferredType(rt),
                        span,
                    });
                } else if let Some(target) = return_via_edge {
                    // Lazy return type: emit `Symbol(sid) →
                    // Edge(target)`. `target` is whichever
                    // attachment the source callable carries
                    // (`Expr(span)` for anon-sub bodies, or
                    // `PackageSymbol{package, name}` for `\&foo` /
                    // `\&Foo::bar` references — the bag's existing
                    // edge-chase covers both, including the
                    // cross-file `module_index` recursion for
                    // PackageSymbol). Writeback projects this
                    // single Symbol-attached emission to
                    // `PackageSymbol{package, name}` for class-
                    // scoped methods at the post-walk pass.
                    use crate::model::witnesses::{
                        Witness, WitnessAttachment, WitnessPayload, WitnessSource,
                    };
                    self.bag.push(Witness {
                        attachment: WitnessAttachment::Symbol(sid),
                        source: WitnessSource::Plugin(plugin_id),
                        payload: WitnessPayload::Edge(target),
                        span,
                    });
                }
            }
            plugin::EmitAction::HashKeyDef { name, owner, span, selection_span } => {
                let detail = SymbolDetail::HashKeyDef { owner, is_dynamic: false };
                self.add_symbol_ns(name, SymKind::HashKeyDef, span, selection_span, detail, ns);
            }
            plugin::EmitAction::HashKeyAccess { name, owner, var_text, span, access } => {
                // Owner-carrying binding so the linkage pass (which looks
                // for HashKeyAccess → HashKeyDef by name+owner) pairs these
                // refs to both in-file and cross-file defs automatically.
                self.refs.push(Ref {
                    kind: RefKind::HashKeyAccess { var_text },
                    span,
                    scope: self.current_scope(),
                    target_name: name,
                    access,
                    binding: Some(crate::model::file_analysis::RefBinding::HashKey {
                        owner,
                        sym: None,
                    }),
                    folded_from: None,
                    arg_count: None,
                });
            }
            plugin::EmitAction::Handler {
                name, owner, dispatchers, params, span, selection_span, display,
                hide_in_outline, outline_label,
            } => {
                // Dedup: the partial-route re-dispatch re-runs `->to`
                // plugins post-fold, so the same Handler (same name +
                // span) can be produced twice. Keep one.
                let already = self.symbols.iter().any(|s| {
                    s.name == name
                        && s.kind == SymKind::Handler
                        && s.span == span
                        && s.namespace == ns
                });
                if already {
                    return;
                }
                let detail = SymbolDetail::Handler {
                    owner,
                    dispatchers,
                    params: params.into_iter().map(Into::into).collect(),
                };
                let sid = self.add_symbol_ns(name, SymKind::Handler, span, selection_span, detail, ns);
                *self.presentation_mut(sid) = crate::model::file_analysis::Presentation {
                    hide_in_outline,
                    doc: None,
                    deprecation: None,
                    display: Some(display),
                    label: outline_label,
                };
            }
            plugin::EmitAction::PluginLoad { name, config_span } => {
                self.plugin_loads.push(crate::model::file_analysis::PluginLoadFact {
                    name,
                    config_span,
                });
            }
            plugin::EmitAction::MethodCallRef { method_name, invocant, span, invocant_span, bridged } => {
                // Standard MethodCall ref — gd/gr/hover/rename route to
                // the usual resolution path (inheritance walk + module
                // index + type inference). The plugin's job is just
                // "there's a call to method X on invocant Y here".
                //
                // `bridged`: when Some(mode), the invocant is a class key the
                // emitting plugin already transformed (a camelized Mojo
                // controller name), not a Perl receiver. It becomes
                // `Invocant::Bridged` so the freeze pass never pins it and
                // core resolves it generically by the declared match mode.
                // A non-bridged invocant is the intended receiver class /
                // canonical variable, treated as the resolved class unless
                // it's a sigil-shape.
                let invocant_class = if bridged.is_some()
                    || invocant.is_empty()
                    || invocant.starts_with('$')
                    || invocant.starts_with('@')
                    || invocant.starts_with('%')
                {
                    None
                } else {
                    Some(invocant.clone())
                };
                if !self.method_call_ref_dedup.insert((
                    span.start,
                    span.end,
                    method_name.clone(),
                )) {
                    return;
                }
                let invocant = match bridged {
                    Some(match_mode) => {
                        crate::model::conventions::Invocant::bridged(plugin_id.clone(), invocant, match_mode)
                    }
                    None => crate::model::conventions::Invocant::assume_canonical(invocant),
                };
                let ref_idx = self.refs.len();
                self.refs.push(Ref {
                    kind: RefKind::MethodCall {
                        invocant,
                        invocant_span,
                        method_name_span: span,
                        member_op: None,
                        shape: crate::model::file_analysis::MemberShape::Unknown,
                    },
                    span,
                    scope: self.current_scope(),
                    target_name: method_name,
                    access: AccessKind::Read,
                    binding: None,
                    folded_from: None,
                    arg_count: None,
                });
                if let Some(c) = invocant_class {
                    self.method_call_invocant.insert(ref_idx, c);
                }
            }
            plugin::EmitAction::DispatchCall { name, dispatcher, owner, span, var_text } => {
                // Same pattern as HashKeyAccess: record the owner so
                // `build_indices` can link the ref to its Handler def in
                // O(1) and `resolve::refs_to` matches cross-file by
                // (owner, name). The var_text lives on the kind for
                // features that want to show the receiver in hover.
                let _ = var_text; // reserved for future hover enrichment
                if self.dispatch_dedup.insert((
                    span.start,
                    span.end,
                    dispatcher.clone(),
                    name.clone(),
                )) {
                    self.refs.push(Ref {
                        kind: RefKind::DispatchCall { dispatcher },
                        span,
                        scope: self.current_scope(),
                        target_name: name,
                        access: AccessKind::Read,
                        binding: Some(crate::model::file_analysis::RefBinding::Handler {
                            owner,
                            sym: None,
                        }),
                        folded_from: None,
                        arg_count: None,
                    });
                }
            }
            plugin::EmitAction::Symbol {
                name, kind, span, selection_span, detail, return_type,
                display, hide_in_outline,
            } => {
                // The per-symbol return type rides at the action
                // level, not on `SymbolDetail`. The Symbol(sid) push
                // is the canonical record — chain typing's bag-routed
                // queries see plugin-synthesized callables uniformly
                // with locals + imports. Writeback iterates symbols
                // and pushes `PackageSymbol{package, name} → Edge(Symbol(sid))`
                // for the primary slot when the sym carries a class.
                let sid = self.add_symbol_ns(name, kind, span, selection_span, detail, ns);
                *self.presentation_mut(sid) = crate::model::file_analysis::Presentation {
                    hide_in_outline,
                    doc: None,
                    deprecation: None,
                    display,
                    label: None,
                };
                if let Some(rt) = return_type {
                    use crate::model::witnesses::{
                        Witness, WitnessAttachment, WitnessPayload, WitnessSource,
                    };
                    self.bag.push(Witness {
                        attachment: WitnessAttachment::Symbol(sid),
                        source: WitnessSource::Plugin(plugin_id),
                        payload: WitnessPayload::InferredType(rt),
                        span,
                    });
                }
            }
            plugin::EmitAction::ImportRef { name, package, span } => {
                // The plugin-facing equivalent of core's qw/`-as` import-token
                // refs: a BYO exporter plugin makes its custom rename-spec
                // tokens navigable by emitting a `FunctionCall` ref pinned to a
                // package — the exporting module for a remote name (joins the
                // source sub's rename), or the consuming package for a local
                // alias (a self-contained local group).
                self.add_bound_ref(
                    RefKind::FunctionCall,
                    span,
                    name,
                    AccessKind::Read,
                    package.map(|package| {
                        crate::model::file_analysis::RefBinding::Function { package }
                    }),
                );
            }
            plugin::EmitAction::PackageParent { package, parent } => {
                self.package_parents.entry(package).or_default().push(parent);
            }
            plugin::EmitAction::FrameworkImport { keyword } => {
                self.framework_imports.insert(keyword);
            }
            plugin::EmitAction::Import { module_name, imported_symbols, span } => {
                // Plugin-synthetic `use` — indistinguishable from a
                // hand-written `use Module qw(name1 name2)` downstream.
                // The whole imported-function machinery (hover, gd,
                // sig-help, unresolved-function diagnostic, completion
                // detail) just works. `qw_close_paren` stays None —
                // there's no qw list to insert into for auto-import.
                self.imports.push(Import {
                    module_name,
                    imported_symbols,
                    span,
                    qw_close_paren: None,
                    empty_import: false,
                });
            }
            plugin::EmitAction::VarType { variable, at, inferred_type } => {
                // Scope resolution is deferred to the end of the build —
                // plugin dispatch runs BEFORE we recurse into call
                // arguments, so the callback body's scope doesn't exist
                // yet. Queue the request and apply it once every scope
                // has been pushed.
                self.deferred_var_types.push(DeferredVarType {
                    variable,
                    at,
                    inferred_type,
                    plugin_id: plugin_id.clone(),
                });
            }
            plugin::EmitAction::NamedSubParamType { sub_name, param_index, inferred_type } => {
                // `->helper(_ => \&sub)` — type the named sub's positional.
                // A qualifier (`Foo::bar`) pins the enclosing package; a bare
                // name defaults to the package the registration sits in (the
                // sub is local to it). Resolution is deferred so a forward-
                // declared sub still resolves.
                let (package, sub_name) = match crate::model::file_analysis::split_qualified(&sub_name) {
                    (Some(pkg), n) => (Some(pkg.to_string()), n.to_string()),
                    (None, _) => (self.current_package.clone(), sub_name),
                };
                self.deferred_named_sub_param_types.push(DeferredNamedSubParamType {
                    sub_name,
                    package,
                    param_index,
                    inferred_type,
                    plugin_id: plugin_id.clone(),
                });
            }
            plugin::EmitAction::SyntheticUse { module, args, imports, span } => {
                // Route through the same worker `visit_use` uses. Plugin
                // dispatch re-fires inside; `use_dedup` breaks cycles.
                // `node: None` skips source-positioned ref emission
                // (parent PackageRefs, qw-import FunctionCalls) — there's
                // no source span to attach those to. The Module symbol,
                // framework mode flip, package_uses, package_parents, the
                // `Import` entry, and the recursive `on_use` dispatch all
                // run identically to a literal `use`. `plugin_id` rides
                // through as `synthesized_by` so the Module symbol carries
                // the emitting plugin's `Namespace::Framework { id }` tag
                // — `--dump-package` / outline filters can distinguish
                // synthesized Module symbols from user-written ones.
                // Anything DOWNSTREAM of the synthetic (re-entered
                // `on_use` hooks, has-synthesizers, etc.) gets its own
                // emitter's id through the regular `apply_emit_action`
                // path; the namespace chain works out naturally.
                self.process_use(module, args, imports, span, span, None, Some(plugin_id));
            }
            plugin::EmitAction::PluginNamespace {
                id,
                kind,
                bridges,
                entity_names,
                decl_span,
            } => {
                // Find-or-create the namespace. Bridges union across
                // repeated emissions so dotted helpers emitted one at
                // a time aggregate into a single namespace; entity_names
                // is resolved now against symbols already emitted by
                // this plugin in THIS dispatch (and any earlier one).
                let plugin_id_for_ns = plugin_id.clone();
                // O(symbols) with O(1) name lookup — the previous
                // `entity_names.iter().any(...)` inside the filter was
                // O(symbols × entity_names). Helpers register dozens
                // of names per app; the quadratic scan compounds.
                let entity_name_set: std::collections::HashSet<&str> =
                    entity_names.iter().map(|s| s.as_str()).collect();
                let entities: Vec<_> = self.symbols.iter()
                    .filter(|s| matches!(
                        &s.namespace,
                        crate::model::file_analysis::Namespace::Framework { id } if id == &plugin_id_for_ns
                    ))
                    .filter(|s| entity_name_set.contains(s.name.as_str()))
                    .map(|s| s.id)
                    .collect();

                // Namespace identity is (plugin_id, id) — not just `id`.
                // Two plugins that both pick "app" as an id belong to
                // different namespaces; matching only on `id` would
                // silently merge entities and bridges across plugins.
                let existing = self.plugin_namespaces.iter_mut()
                    .find(|n| n.id == id && n.plugin_id == plugin_id_for_ns);
                if let Some(existing) = existing {
                    for b in bridges {
                        if !existing.bridges.contains(&b) {
                            existing.bridges.push(b);
                        }
                    }
                    for e in entities {
                        if !existing.entities.contains(&e) {
                            existing.entities.push(e);
                        }
                    }
                } else {
                    self.plugin_namespaces.push(crate::model::file_analysis::PluginNamespace {
                        id,
                        plugin_id: plugin_id_for_ns,
                        kind,
                        entities,
                        bridges,
                        decl_span,
                    });
                }
            }
        }
    }

    /// Build an `ArgInfo` for a plugin. Constant-folds literals, barewords,
    /// and `$var` references that accumulate in `constant_strings`. When the
    /// arg is an anonymous sub, also extracts its param list so plugins
    /// registering handlers (`->on('ready', sub ($s, $m) {})`) can preserve
    /// the handler signature for later sig-help lookup.
    ///
    /// `&mut self` because the inferred-type derivation emits the arg's
    /// `Expr(span)` witness onto the bag before querying it — the order
    /// matters: emit first, then query. Reversing yields `None` from the
    /// query (no witness on the attachment yet) and the caller would
    /// silently skip the `callable_return_edge` projection.
    pub(super) fn arg_info_for(&mut self, arg: Node<'a>) -> plugin::ArgInfo {
        let text = arg.utf8_text(self.source).unwrap_or("").to_string();
        let mut content_span: Option<Span> = None;
        let string_value = match arg.kind() {
            "string_literal" | "interpolated_string_literal" => {
                // Read the string_content child — quote-flavor-agnostic
                // (handles q{}, qq!!, heredocs, etc.). An empty literal
                // has no content child, so default to "".
                // Also capture the content span so plugins can address
                // positions inside the string without hardcoding
                // quote-length offsets into the outer node's span.
                for i in 0..arg.named_child_count() {
                    if let Some(c) = arg.named_child(i) {
                        if c.kind() == "string_content" {
                            content_span = Some(node_to_span(c));
                            break;
                        }
                    }
                }
                Some(self.extract_string_content(arg).unwrap_or_default())
            }
            // `autoquoted_bareword` is a fat-comma key (`key => value`)
            // — its text IS the value, never const-folded (a key that
            // happens to match a constant name is still that key).
            "autoquoted_bareword" => Some(text.clone()),
            // A positional `bareword` arg may be a constant — fold it
            // through the constant table (`$app->plugin(EXTRA)` where
            // `use constant EXTRA => 'Gizmos'`). Falls back to the raw
            // token when it names no constant.
            "bareword" => self
                .resolve_constant_strings(&text, 0)
                .and_then(|f| f.into_iter().next())
                .or_else(|| Some(text.clone())),
            "scalar" | "array" | "hash" => {
                self.resolve_constant_strings(&text, 0).and_then(|f| f.into_iter().next())
            }
            _ => None,
        };
        // `string_values` is the multi-value channel: a loop registration
        // (`$app->helper("get_$name" => …) for my $name (qw(a b))`) folds to
        // every candidate. The general enumeration owns literal / interpolated
        // / constant-ref / concat folding; an undecidable arg yields empty and
        // falls back to the single `string_value` (a fat-comma bareword key,
        // an unfolded interpolation the plugin then skips).
        let mut string_values = self.enumerate_string_values(arg);
        if string_values.is_empty() {
            string_values.extend(string_value.clone());
        }
        self.emit_expr_witness(arg);
        let inferred_type = self.bag_query_expr_span(node_to_span(arg));
        let sub_params = if arg.kind() == "anonymous_subroutine_expression" {
            self.extract_anonymous_sub_params(arg)
        } else {
            Vec::new()
        };
        // `callable_return_edge` flows from whichever
        // `InferredType::CodeRef { return_edge }` is reachable for
        // this arg. Three reachability paths covered uniformly:
        //
        //   helper(name => sub { … })             (anon literal)
        //   my $sub = sub { … }; helper(_, $sub)   (rebound anon)
        //   helper(name => \&Foo::bar)             (named ref)
        //
        // The literal paths (anon-sub + refgen) flow through
        // `emit_expr_witness`'s closed-syntax arms in `expr_payload`;
        // the rebind path goes through `invocant_type_at_node`'s
        // `scalar` arm, which `bag_query_variable`-resolves the
        // variable's TC. Either yields the right `CodeRef` shape;
        // the projection extracts the attachment whatever its target
        // shape (`Expr(span)` for anon, `PackageSymbol{...}` for refgen).
        let callable_return_edge = inferred_type
            .as_ref()
            .and_then(InferredType::callable_return_edge)
            .cloned()
            .or_else(|| {
                self.invocant_type_at_node(arg)
                    .as_ref()
                    .and_then(InferredType::callable_return_edge)
                    .cloned()
            });
        // `\&name` refgen — the named sub a registration plugin may want to
        // type the first param of. Same name extraction the return-edge path
        // uses; bare names stay bare so the deferred resolver scopes them to
        // the current package.
        let ref_sub_name = if arg.kind() == "refgen_expression" {
            self.extract_names_from_refgen(arg).into_iter().next()
        } else {
            None
        };
        let value_shape = self.classify_value_shape(arg);
        plugin::ArgInfo {
            text,
            string_value,
            string_values,
            span: node_to_span(arg),
            content_span,
            inferred_type,
            value_shape,
            sub_params,
            callable_return_edge,
            ref_sub_name,
        }
    }

    /// Extract params from an anonymous sub. Delegates to the builder's
    /// shared named-sub extractor (signature syntax + `my (...) = @_` +
    /// `shift`/`$_[N]` unpacks, all via tree walking) so the two codepaths
    /// can't diverge.
    pub(super) fn extract_anonymous_sub_params(&self, sub_node: Node<'a>) -> Vec<plugin::EmittedParam> {
        self.extract_params(sub_node)
            .into_iter()
            .map(|p| plugin::EmittedParam {
                name: p.name,
                default: p.default,
                is_slurpy: p.is_slurpy,
                is_invocant: false,
            })
            .collect()
    }
}
