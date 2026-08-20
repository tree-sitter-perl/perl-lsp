//! Framework synthesis: `has` (Moo/Moose/Mojo::Base), extends/requires,
//! DBIC components, Sub::Exporter config, and isa-constraint mapping.

use super::*;

impl<'a> Builder<'a> {
    /// Synthesize accessor methods from `has` calls in Moo/Moose/Mojo::Base classes.
    /// Handle Moose/Moo `extends 'Parent::Class', ...` — register parent classes.
    /// `requires LIST` in a role — each name is a method CONTRACT the
    /// composing class must fulfill. Synthesize a Method symbol per
    /// name (span = the name's own atom, rule #7) so `$self->name`
    /// resolves inside the role: the unresolved-method hint stays
    /// quiet, goto-def lands on the contract, completion offers it.
    /// Names also land in `role_requires` — the input for the future
    /// composer-mismatch diagnostic.
    pub(super) fn visit_requires_call(&mut self, node: Node<'a>, pkg: &str) {
        let Some(args) = node.child_by_field_name("arguments") else { return };
        let names = self.extract_string_list(args);
        for (name, span) in &names {
            let sid = self.add_symbol(
                name.clone(),
                SymKind::Method,
                *span,
                *span,
                SymbolDetail::Sub {
                    params: vec![],
                    is_method: true,
                    doc: Some(format!(
                        "**Required** by role `{pkg}` — the composing class must provide it.",
                    )),
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                },
            );
            self.contract_symbols.insert(sid);
        }
        self.role_requires
            .entry(pkg.to_string())
            .or_default()
            .extend(names.into_iter().map(|(n, _)| n));
    }

    pub(super) fn visit_extends_call(&mut self, node: Node<'a>, pkg: &str) {
        let args = match node.child_by_field_name("arguments") {
            Some(a) => a,
            None => return,
        };
        let (named, residue) = crate::cst::string_list_with_residue(args, self.source, &mut |n| {
            let Ok(text) = n.utf8_text(self.source) else { return vec![] };
            let Some(values) = self.resolve_constant_strings(text, 0) else { return vec![] };
            let span = node_to_span(n);
            values.into_iter().map(|v| (v, span)).collect()
        });
        // A parent we couldn't fold (`with ReportProxy(type => ...)` —
        // a runtime-generated role) makes this package's ancestry
        // incomplete: record the fact so `class_has_unresolved_ancestor`
        // keeps inheritance-dependent consumers honest-silent.
        if residue {
            self.dynamic_parent_packages.insert(pkg.to_string());
        }
        let parents: Vec<String> = named.into_iter().map(|(s, _)| s).collect();
        if !parents.is_empty() {
            let parent_set: std::collections::HashSet<&str> = parents.iter().map(|s| s.as_str()).collect();
            self.emit_refs_for_strings(node, &parent_set, RefKind::PackageRef, None);
            self.package_parents
                .entry(pkg.to_string())
                .or_default()
                .extend(parents);
        }
    }

    /// Handle `__PACKAGE__->load_components('+Full::Name', 'Short::Name')` and
    /// `load_own_components`. Bare names are prefixed with `base_ns`
    /// (`DBIx::Class` for `load_components`, the CURRENT package for
    /// `load_own_components` — DBIC's own-namespace mixin loader:
    /// `DBIx::Class::Relationship->load_own_components('CascadeActions')` pulls
    /// in `DBIx::Class::Relationship::CascadeActions`); `+` prefix means fully
    /// qualified. Both register the component as a parent so method resolution
    /// and the implementations fan-out see the composed mixin.
    pub(super) fn visit_load_components(&mut self, node: Node<'a>, base_ns: &str) {
        let pkg = match self.current_package.clone() {
            Some(p) => p,
            None => return,
        };
        let args = match node.child_by_field_name("arguments") {
            Some(a) => a,
            None => return,
        };
        let components: Vec<String> = self.extract_string_names(args).into_iter()
            .map(|name| {
                if let Some(stripped) = name.strip_prefix('+') {
                    stripped.to_string()
                } else {
                    format!("{}::{}", base_ns, name)
                }
            })
            .collect();
        if !components.is_empty() {
            self.package_parents
                .entry(pkg)
                .or_default()
                .extend(components);
        }
    }

    /// Publish a multi-arm `UnionOnArgs` ReturnExpr on
    /// `PackageSymbol{current_package, name}` so cross-symbol arity
    /// dispatch routes through one class-keyed declaration instead of
    /// whichever sym the "primary" rule picked first. A name with
    /// multiple syms (Mojo/Moo getter `name()` + writer `name($v)`)
    /// shares `(class, name)`; the writer's `Some(1)` and getter's
    /// `Some(0)` both attach here and the reducer picks by `arity_hint`
    /// (without this, `level(1)` returns the getter's `String` instead
    /// of the writer's invocant type). Class-keyed so `Sweet::flavor` /
    /// `Sour::flavor` stay independent. The chain typer's coderef-edge,
    /// dynamic-method, and direct-method routes all hit this attachment
    /// with their call-site receiver, so substitution answers uniformly.
    ///
    /// Branch ordering is the caller's responsibility (each framework
    /// knows its arm shape): `Empty` / `Exact(N)` first, `AtLeast(N)`
    /// next, `Any` last — `UnionOnArgs` is first-match.
    ///
    /// The per-Symbol answer + provenance is published separately by
    /// `record_framework_accessor_witness`. No-op when `current_package`
    /// is unset (file-scoped accessors have no class slot).
    pub(super) fn publish_class_accessor_union(
        &mut self,
        name: &str,
        branches: Vec<(crate::model::witnesses::ArgGuard, crate::model::witnesses::ReturnExpr)>,
    ) {
        if branches.is_empty() {
            return;
        }
        let Some(class) = self.current_package.clone() else { return };
        let zero = Span {
            start: Point { row: 0, column: 0 },
            end: Point { row: 0, column: 0 },
        };
        self.bag.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::PackageSymbol {
                package: class,
                name: name.to_string(),
            },
            source: crate::model::witnesses::WitnessSource::Builder(
                "framework_accessor_returnexpr".into(),
            ),
            payload: crate::model::witnesses::WitnessPayload::ReturnExpr(
                crate::model::witnesses::ReturnExpr::UnionOnArgs { branches },
            ),
            span: zero,
        });
    }

    /// Record a framework-synthesized accessor's per-Symbol answer
    /// in the bag and stamp its provenance. `return_expr` is the
    /// arity-gated return for THIS specific symbol (a getter sym
    /// answers `(Empty, Concrete(getter_type))`; a fluent writer
    /// sym answers `(AtLeast(1), Receiver)`; a Moo rw writer
    /// answers `(AtLeast(1), Concrete(isa_type))`). When the
    /// caller has no meaningful return to declare (Mojo `has 'name'`
    /// getter with no default), pass `None` — the function still
    /// records `TypeProvenance::FrameworkSynthesis` so dump-package
    /// reports the accessor's origin, but pushes no bag witness.
    ///
    /// The per-Symbol UnionOnArgs is the canonical answer (gated
    /// to its specific arity). Cross-symbol dispatch (getter+
    /// writer pair sharing `(class, name)`) is published by
    /// `publish_class_accessor_union` as a multi-arm UnionOnArgs
    /// on `PackageSymbol{package, name}` so per-arity callers route
    /// to the right arm regardless of which sym they hit first.
    pub(super) fn record_framework_accessor_witness(
        &mut self,
        sym_id: SymbolId,
        name: &str,
        return_expr: Option<(crate::model::witnesses::ArgGuard, crate::model::witnesses::ReturnExpr)>,
        framework: &str,
        reason: String,
    ) {
        use crate::model::witnesses::{
            ReturnExpr, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };
        // Provenance always recorded — even when there's no return
        // to publish, dump-package can answer "where did this sym
        // come from".
        self.type_provenance.insert(
            sym_id,
            TypeProvenance::FrameworkSynthesis {
                framework: framework.to_string(),
                reason,
            },
        );
        let Some((guard, expr)) = return_expr else { return };
        let zero = Span {
            start: Point { row: 0, column: 0 },
            end: Point { row: 0, column: 0 },
        };
        let union = ReturnExpr::UnionOnArgs {
            branches: vec![(guard, expr)],
        };
        self.bag.push(Witness {
            attachment: WitnessAttachment::Symbol(sym_id),
            source: WitnessSource::Builder("framework_accessor".into()),
            payload: WitnessPayload::ReturnExpr(union),
            span: zero,
        });
        // Cross-class dispatch lives on the multi-arm `UnionOnArgs`
        // pushed once per Mojo attribute in `visit_has_call`'s
        // MojoBase branch, and on writeback's primary slot for Moo
        // accessors (writer returns same as getter, so the primary
        // answers either arity correctly via `PackageSymbolReducer`).
        // Don't re-emit at the class-scoped attachment here — would
        // create overlapping single-arm unions that conflict with
        // the comprehensive multi-arm one.
        let _ = name;
    }

    pub(super) fn visit_has_call(&mut self, node: Node<'a>, mode: FrameworkMode) {
        // Extract attribute names and options from the `has` call arguments.
        // CST: ambiguous_function_call_expression
        //   function: "has"
        //   arguments: list_expression
        //     [0] string_literal 'name' | anonymous_array_expression [qw(a b)]
        //     [1] list_expression (is => 'ro', isa => 'Str')   -- options (Moo/Moose)
        //          OR absent (Mojo::Base: has 'name' or has 'name' => 'default')
        let mut attr_names: Vec<(String, Span)> = Vec::new();
        let mut is_value: Option<String> = None;
        let mut isa_value: Option<String> = None;
        // The `isa` value NODE — for a parametric constraint
        // (`InstanceOf['Foo']`) `extract_node_string` drops the value
        // (it isn't a bareword/string), so the node is the only handle.
        // Read structurally, never re-parsed.
        let mut isa_value_node: Option<Node<'a>> = None;
        let mut mojo_default_node: Option<Node<'a>> = None;

        // Get the arguments node
        let args = match node.child_by_field_name("arguments") {
            Some(a) => a,
            None => return,
        };

        // The arguments node might be a list_expression or a single node
        let args_children: Vec<Node> = if args.kind() == "list_expression" || args.kind() == "parenthesized_expression" {
            (0..args.child_count()).filter_map(|i| args.child(i)).collect()
        } else {
            // Single argument (e.g., has 'name')
            vec![args]
        };

        let mut first_named_idx: Option<usize> = None;
        for (idx, child) in args_children.iter().enumerate() {
            if !child.is_named() { continue; }
            first_named_idx = Some(idx);
            match child.kind() {
                "string_literal" | "interpolated_string_literal" => {
                    if let Some(text) = self.extract_string_content(*child) {
                        if !text.starts_with('+') {
                            attr_names.push((text, self.string_content_span(*child)));
                        }
                    }
                }
                "bareword" | "autoquoted_bareword" => {
                    if let Ok(text) = child.utf8_text(self.source) {
                        attr_names.push((text.to_string(), node_to_span(*child)));
                    }
                }
                // Literal arrayref (`has ['a','b']`) or a ref to a constant
                // array (`has \@attrs` where `my @attrs = qw/.../`) flatten
                // through the constant-fold seam: `extract_array_attr_names`
                // → `string_list` resolves `\@attrs` against the constant table.
                // NOT a bare `has @attrs` — that SPLATS the array into the call
                // (`has 'a', 'b', is => …`), a different declaration entirely,
                // so the `array` node is intentionally excluded. A non-constant
                // arrayref folds to nothing and stays unclaimed.
                "anonymous_array_expression" | "refgen_expression" => {
                    self.extract_array_attr_names(*child, &mut attr_names);
                }
                _ => {}
            }
            break;
        }

        if attr_names.is_empty() { return; }

        // After first arg: options (Moo/Moose) or default value (Mojo::Base).
        // Both the nested `=> (is => ...)` and the flat `, is => ...` forms
        // route through `has_option_pair_nodes`.
        let rest: Vec<Node> = first_named_idx
            .map(|first| args_children[first + 1..].to_vec())
            .unwrap_or_default();
        if mode == FrameworkMode::MojoBase {
            mojo_default_node = rest.iter().copied().find(|c| c.is_named());
        } else {
            for (k_node, val_node) in self.has_option_pair_nodes(&rest) {
                match self.extract_node_string(k_node).as_deref() {
                    Some("is") => {
                        if is_value.is_none() {
                            is_value = self.extract_node_string(val_node);
                        }
                    }
                    Some("isa") => {
                        if isa_value.is_none() {
                            isa_value = self.extract_node_string(val_node);
                        }
                        if isa_value_node.is_none() {
                            isa_value_node = Some(val_node);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Map isa value to InferredType
        let return_type = match mode {
            FrameworkMode::Moo | FrameworkMode::Moose => {
                // String / bareword isa (`'Str'`, `'My::Class'`) → meaning-map.
                // A parametric constraint (`InstanceOf['Foo']`) isn't a string,
                // so resolve its expression type and ask the *constraint* what
                // it constrains to (rule #10 — the type answers). Same path
                // covers `isa => $t` where `$t` is a constraint-typed variable.
                isa_value
                    .as_deref()
                    .and_then(|isa| self.map_isa_to_type(isa, mode))
                    .or_else(|| {
                        let n = isa_value_node?;
                        self.emit_expr_witness(n);
                        self.bag_query_expr_span(node_to_span(n))?
                            .constrained_inner()
                            .cloned()
                    })
            }
            FrameworkMode::MojoBase => {
                // Fluent return: ClassName(current_package)
                self.current_package.as_ref().map(|pkg| InferredType::ClassName(pkg.clone()))
            }
        };

        // Determine what accessors to synthesize
        match mode {
            FrameworkMode::Moo | FrameworkMode::Moose => {
                let is = is_value.as_deref();
                match is {
                    Some("bare") => return, // no accessor
                    None => return,         // no `is` = no accessor (Moo/Moose default)
                    _ => {}
                }
                let is_rw = matches!(is, Some("rw"));
                let is_rwp = matches!(is, Some("rwp"));

                let framework = match mode {
                    FrameworkMode::Moo => "Moo",
                    FrameworkMode::Moose => "Moose",
                    FrameworkMode::MojoBase => unreachable!(),
                };
                for (name, sel_span) in &attr_names {
                    // Getter (always present for ro/rw/lazy/rwp)
                    let getter_id = self.add_symbol(
                        name.clone(),
                        SymKind::Method,
                        node_to_span(node),
                        *sel_span,
                        SymbolDetail::Sub {
                            params: vec![],
                            is_method: true,
                            doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                        },
                    );
                    // Moo/Moose getter: arity 0 → isa-derived type
                    // (when known). When `isa` doesn't pin a type
                    // we still record provenance via `None` so
                    // dump-package shows the synth origin.
                    let getter_arm = return_type.clone().map(|t| {
                        (
                            crate::model::witnesses::ArgGuard::Empty,
                            crate::model::witnesses::ReturnExpr::Concrete(t),
                        )
                    });
                    self.record_framework_accessor_witness(
                        getter_id,
                        name,
                        getter_arm,
                        framework,
                        format!("{} `has '{}'` getter (isa)", framework, name),
                    );
                    // Setter for rw — same isa type at arity ≥ 1
                    // (Moo/Moose writers return the new value, not
                    // the invocant).
                    if is_rw {
                        let writer_id = self.add_symbol(
                            name.clone(),
                            SymKind::Method,
                            node_to_span(node),
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
                        // Same name+span as the getter — hide so the outline
                        // shows one entry per `has` attribute, not a
                        // reader+writer pair.
                        self.presentation_mut(writer_id).hide_in_outline = true;
                        let writer_arm = return_type.clone().map(|t| {
                            (
                                crate::model::witnesses::ArgGuard::AtLeast(1),
                                crate::model::witnesses::ReturnExpr::Concrete(t),
                            )
                        });
                        self.record_framework_accessor_witness(
                            writer_id,
                            name,
                            writer_arm,
                            framework,
                            format!("{} `has '{}'` rw writer", framework, name),
                        );
                    }
                    // Private writer for rwp (Moo only) — same shape
                    // as rw writer.
                    if is_rwp {
                        let writer_name = format!("_set_{}", name);
                        let writer_id = self.add_symbol(
                            writer_name.clone(),
                            SymKind::Method,
                            node_to_span(node),
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
                        let writer_arm = return_type.clone().map(|t| {
                            (
                                crate::model::witnesses::ArgGuard::AtLeast(1),
                                crate::model::witnesses::ReturnExpr::Concrete(t),
                            )
                        });
                        self.record_framework_accessor_witness(
                            writer_id,
                            &writer_name,
                            writer_arm,
                            framework,
                            format!("{} `has '{}'` rwp private writer", framework, name),
                        );
                    }

                    // Cross-symbol union for `PackageSymbol{package, name}`.
                    // Moo/Moose getter and rw writer share the same
                    // isa-derived return type, so the union is a
                    // single-arm `(Any, Concrete(t))` when an isa
                    // type is known. Without isa, no class-keyed
                    // declaration — name-keyed lookups fall back
                    // through standard inheritance walks.
                    //
                    // The rwp private writer (named `_set_<attr>`)
                    // gets its own per-Symbol union but needs no
                    // class-scoped union (its name doesn't collide
                    // with the getter's).
                    if let Some(t) = return_type.clone() {
                        self.publish_class_accessor_union(
                            name,
                            vec![(
                                crate::model::witnesses::ArgGuard::Any,
                                crate::model::witnesses::ReturnExpr::Concrete(t),
                            )],
                        );
                    }
                }
            }
            FrameworkMode::MojoBase => {
                // Infer getter return type from default value if present
                let getter_type = if let Some(n) = mojo_default_node {
                    if n.kind() == "anonymous_subroutine_expression" {
                        self.infer_anonymous_sub_return_type(n)
                    } else {
                        self.emit_expr_witness(n);
                        self.bag_query_expr_span(node_to_span(n))
                    }
                } else {
                    None
                };
                let fluent_type = self
                    .current_package
                    .as_ref()
                    .map(|pkg| InferredType::ClassName(pkg.clone()));
                let framework = "Mojo::Base";

                // Mojo::Base `has` produces getter + setter (two symbols)
                for (name, sel_span) in &attr_names {
                    // Getter: no params, return type from default value (or None)
                    let getter_id = self.add_symbol(
                        name.clone(),
                        SymKind::Method,
                        node_to_span(node),
                        *sel_span,
                        SymbolDetail::Sub {
                            params: vec![],
                            is_method: true,
                            doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                        },
                    );
                    let getter_arm = getter_type.clone().map(|t| {
                        (
                            crate::model::witnesses::ArgGuard::Empty,
                            crate::model::witnesses::ReturnExpr::Concrete(t),
                        )
                    });
                    self.record_framework_accessor_witness(
                        getter_id,
                        name,
                        getter_arm,
                        framework,
                        format!("Mojo::Base `has '{}'` getter (default-value type)", name),
                    );
                    // Setter: fluent, returns $self for chaining
                    let writer_id = self.add_symbol(
                        name.clone(),
                        SymKind::Method,
                        node_to_span(node),
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
                    // Same name+span as the getter — hide so the outline
                    // shows one entry per `has` attribute, not a
                    // reader+writer pair.
                    self.presentation_mut(writer_id).hide_in_outline = true;
                    // Mojo writer: arity ≥ 1 → fluent return
                    // (the invocant). Encode as `Receiver` so the
                    // value-side substitution evaluates to the
                    // call's receiver type at consumption — matches
                    // direct `$obj->name(1)`, coderef-of-method
                    // `\&Class::name; $cb->($obj, 1)`, and
                    // dynamic-method `$obj->$cb(1)` uniformly.
                    let _ = fluent_type; // retained as documentation
                    let writer_arm = Some((
                        crate::model::witnesses::ArgGuard::AtLeast(1),
                        crate::model::witnesses::ReturnExpr::Receiver,
                    ));
                    self.record_framework_accessor_witness(
                        writer_id,
                        name,
                        writer_arm,
                        framework,
                        format!("Mojo::Base `has '{}'` fluent writer (returns invocant)", name),
                    );

                    // Symbol-declarative ReturnExpr for class-keyed
                    // dispatch. The chain typer's `PackageSymbol{package,
                    // name}` chase (direct call, coderef-of-method,
                    // dynamic-method) substitutes `q.receiver` for
                    // `Receiver`, evaluating to the call's invocant
                    // class for fluent return.
                    let getter_arm = getter_type.clone().map(|t| {
                        (
                            crate::model::witnesses::ArgGuard::Empty,
                            crate::model::witnesses::ReturnExpr::Concrete(t),
                        )
                    });
                    let writer_arm = (
                        crate::model::witnesses::ArgGuard::AtLeast(1),
                        crate::model::witnesses::ReturnExpr::Receiver,
                    );
                    self.publish_class_accessor_union(
                        name,
                        getter_arm.into_iter().chain(std::iter::once(writer_arm)).collect(),
                    );
                }
            }
        }

        // Synthesize HashKeyDef entries so Foo->new(name => ...) connects to the attribute.
        if let Some(ref pkg) = self.current_package {
            // The projection entity: `has` is hash-backed in every framework
            // this visitor serves, so the InternalKey projection is minted
            // HERE — the repr gate is encoded at the source, not re-derived
            // by consumers (Corinna's field visitor mints no InternalKey).
            for (name, _sel_span) in &attr_names {
                for kind in [
                    crate::model::file_analysis::AttrProjectionKind::CtorKey,
                    crate::model::file_analysis::AttrProjectionKind::InternalKey,
                ] {
                    self.attr_projections.push(crate::model::file_analysis::AttrProjection {
                        class: pkg.clone(),
                        attr: name.clone(),
                        kind,
                    });
                }
            }
            let owner = HashKeyOwner::Sub {
                package: self.current_package.clone(),
                name: "new".to_string(),
            };
            for (name, sel_span) in &attr_names {
                self.add_symbol(
                    name.clone(),
                    SymKind::HashKeyDef,
                    node_to_span(node),
                    *sel_span,
                    SymbolDetail::HashKeyDef {
                        owner: owner.clone(),
                        is_dynamic: false,

                    },
                );
            }
        }
    }

    /// The `isa` option's resolved type in a `has`-style option tail
    /// (the `isa` projection): the string-vocabulary + constraint-fold
    /// resolution over the option pairs, taking the ARGUMENTS node
    /// directly so the pattern dispatcher can project it per matched
    /// capture. Falls back to `Moo` when the match-site
    /// package has no recorded framework mode (a `Dancer2::Plugin` /
    /// `MooX::Options` package whose `has` is still Moo-backed).
    pub(crate) fn isa_type_in_option_tail(&mut self, args_node: Node<'a>) -> Option<InferredType> {
        let mode = self
            .current_package
            .as_ref()
            .and_then(|p| self.framework_modes.get(p))
            .copied()
            .unwrap_or(FrameworkMode::Moo);
        if mode == FrameworkMode::MojoBase {
            return None;
        }
        let args_children: Vec<Node<'a>> = if matches!(
            args_node.kind(),
            "list_expression" | "parenthesized_expression"
        ) {
            (0..args_node.child_count())
                .filter_map(|i| args_node.child(i))
                .collect()
        } else {
            vec![args_node]
        };
        let first = args_children.iter().position(|c| c.is_named())?;
        for (k_node, v_node) in self.has_option_pair_nodes(&args_children[first + 1..]) {
            let Some(key) = self.extract_node_string(k_node) else {
                continue;
            };
            if key != "isa" {
                continue;
            }
            return self
                .extract_node_string(v_node)
                .as_deref()
                .and_then(|s| self.map_isa_to_type(s, mode))
                .or_else(|| {
                    self.emit_expr_witness(v_node);
                    self.bag_query_expr_span(node_to_span(v_node))?
                        .constrained_inner()
                        .cloned()
                });
        }
        None
    }

    /// Collect a call's option-tail fat-comma pairs as `(key_node, value_node)`,
    /// regardless of surface form — `has 'n' => (k => v)` (one nested list) or
    /// `has 'n', k => v` (flat siblings). Owned Vec so the caller can run
    /// `&self` classifiers over each pair without a borrow tangle.
    pub(super) fn has_option_pair_nodes(&self, rest: &[Node<'a>]) -> Vec<(Node<'a>, Node<'a>)> {
        let named: Vec<Node<'a>> = rest.iter().copied().filter(|n| n.is_named()).collect();
        if let [only] = named.as_slice() {
            if matches!(only.kind(), "list_expression" | "parenthesized_expression") {
                return crate::cst::pair_nodes(*only);
            }
        }
        crate::cst::pair_nodes_in(rest)
    }

    /// Classify a fat-comma value node into the generic [`plugin::ValueShape`]
    /// — no DSL vocabulary, just the syntactic shape. The plugin maps
    /// `(keyword, shape)` to behavior.
    pub(super) fn classify_value_shape(&self, node: Node<'a>) -> plugin::ValueShape {
        match node.kind() {
            "number" => plugin::ValueShape::Num(
                node.utf8_text(self.source).unwrap_or("").to_string(),
            ),
            "string_literal" | "interpolated_string_literal" => {
                plugin::ValueShape::Str(self.extract_string_content(node).unwrap_or_default())
            }
            "bareword" | "autoquoted_bareword" => {
                plugin::ValueShape::Str(node.utf8_text(self.source).unwrap_or("").to_string())
            }
            "anonymous_hash_expression" => {
                let mut tokens: Vec<String> = Vec::new();
                self.collect_hash_tokens(node, &mut tokens);
                let mut pairs = Vec::new();
                let mut i = 0;
                while i + 1 < tokens.len() {
                    pairs.push((tokens[i].clone(), tokens[i + 1].clone()));
                    i += 2;
                }
                plugin::ValueShape::HashPairs(pairs)
            }
            "anonymous_array_expression" => {
                plugin::ValueShape::ArrayItems(
                    self.extract_string_list(node).into_iter().map(|(s, _)| s).collect(),
                )
            }
            other => plugin::ValueShape::Other(other.to_string()),
        }
    }

    /// Extract a string from a bareword or string-literal node.
    pub(super) fn extract_node_string(&self, node: Node<'a>) -> Option<String> {
        match node.kind() {
            "bareword" | "autoquoted_bareword" => node.utf8_text(self.source).ok().map(|s| s.to_string()),
            "string_literal" | "interpolated_string_literal" => self.extract_string_content(node),
            _ => None,
        }
    }

    /// Flatten a fat-comma hash node into alternating key/value strings,
    /// recursing into tree-sitter-perl's right-associative nested
    /// `list_expression` wrappers.
    pub(super) fn collect_hash_tokens(&self, node: Node<'a>, out: &mut Vec<String>) {
        match node.kind() {
            "anonymous_hash_expression" => {
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i) {
                        if matches!(child.kind(), "list_expression" | "parenthesized_expression") {
                            self.collect_hash_tokens(child, out);
                            return;
                        }
                    }
                }
                self.collect_hash_tokens_flat(node, out);
            }
            "list_expression" | "parenthesized_expression" => {
                self.collect_hash_tokens_flat(node, out);
            }
            _ => {
                if let Some(s) = self.extract_node_string(node) {
                    out.push(s);
                }
            }
        }
    }

    pub(super) fn collect_hash_tokens_flat(&self, node: Node<'a>, out: &mut Vec<String>) {
        let count = node.child_count();
        let mut i = 0;
        while i < count {
            if let Some(child) = node.child(i) {
                match child.kind() {
                    "=>" | "," => {}
                    "list_expression" | "parenthesized_expression" => {
                        self.collect_hash_tokens_flat(child, out);
                    }
                    _ if child.is_named() => {
                        if let Some(s) = self.extract_node_string(child) {
                            out.push(s);
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }

    /// Extract arguments from `use Mojo::Base ...` including barewords like -strict, -base.
    /// Put `pkg` into Mojo::Base mode (accessor synthesis via `has`) and wire
    /// `parents` into `package_parents` so the universal Mojo::Base methods
    /// (`tap`/`attr`/`new`) resolve through inheritance. Shared by the literal
    /// `use Mojo::Base ...` arm and the generic `use X -base` arm.
    pub(super) fn apply_mojo_base_mode(&mut self, pkg: String, parents: Vec<String>, node: Option<Node<'a>>) {
        self.framework_modes.insert(pkg.clone(), FrameworkMode::MojoBase);
        self.framework_imports.insert("has".to_string());
        if parents.is_empty() { return; }
        if let Some(node) = node {
            let parent_set: std::collections::HashSet<&str> =
                parents.iter().map(|s| s.as_str()).collect();
            self.emit_refs_for_strings(node, &parent_set, RefKind::PackageRef, None);
        }
        self.package_parents.entry(pkg).or_default().extend(parents);
    }

    pub(super) fn extract_mojo_base_args(&self, node: Node<'a>) -> Vec<String> {
        let mut args = Vec::new();
        let module_end = node.child_by_field_name("module")
            .map(|m| m.end_byte())
            .unwrap_or(0);
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.start_byte() <= module_end { continue; }
                if let Some(text) = self.extract_node_string(child) {
                    args.push(text);
                }
            }
        }
        if args.is_empty() {
            // Fallback to standard extraction
            let (standard, _) = self.extract_use_import_list(node);
            return standard;
        }
        args
    }

    /// Extract string content from a string_literal node (strips quotes).
    pub(super) fn extract_string_content(&self, node: Node<'a>) -> Option<String> {
        crate::cst::string_content_text(node, self.source)
    }

    pub(super) fn string_content_span(&self, node: Node<'a>) -> Span {
        crate::cst::string_content_span(node)
    }

    pub(super) fn extract_array_attr_names(&self, node: Node<'a>, names: &mut Vec<(String, Span)>) {
        // Handle bare qw() node directly (e.g. as method arg)
        if node.kind() == "quoted_word_list" {
            self.extract_qw_word_spans(node, names);
            return;
        }
        names.extend(self.extract_string_list(node));
    }

    /// Walk a constraint constructor's args (`InstanceOf['Foo']`,
    /// `Enum['a','b']`, `Maybe[InstanceOf['Foo']]`) into a flat param list
    /// for the plugin fold. The arg is the parameterizing arrayref
    /// `['Foo', ...]`; we flatten its elements via `constraint_param_for`
    /// (string → `string`, nested constructor → `ty`). Rule #1: only the
    /// builder walks these nodes — the plugin gets the structured params.
    pub(super) fn extract_constraint_params(&mut self, call_node: Node<'a>) -> Vec<plugin::ConstraintParam> {
        let mut params = Vec::new();
        let Some(args) = call_node.child_by_field_name("arguments") else {
            return params;
        };
        // The `arguments` field is the `[...]` arrayref itself (`Name[p, ...]`),
        // or a paren list for the `Name(p, ...)` form. Each named child is one
        // param — a string literal, or a nested constructor (`Maybe[InstanceOf
        // ['Foo']]`). A nested arrayref (`Tuple[[...]]`) flattens one level.
        for i in 0..args.named_child_count() {
            let Some(child) = args.named_child(i) else { continue };
            match child.kind() {
                "anonymous_array_expression" => {
                    for j in 0..child.named_child_count() {
                        if let Some(el) = child.named_child(j) {
                            params.push(self.constraint_param_for(el));
                        }
                    }
                }
                _ => params.push(self.constraint_param_for(child)),
            }
        }
        params
    }

    /// One arrayref element of a constraint constructor → a `ConstraintParam`.
    /// A string-literal param fills `string` (class names, enum values); a
    /// nested constructor (`Maybe[InstanceOf['Foo']]`, `ArrayRef[Int]`) is a
    /// *value* in its own right, so we type its expression through the bag —
    /// the same `expr_payload` path the outer call walks, hence any nesting
    /// depth resolves — and fill `ty` with the resulting `TypeConstraintOf`.
    /// The plugin's fold then projects (a `Maybe` passthrough asks the param's
    /// `ty` for its `constrained_inner`). rule #1: the builder walks the node.
    pub(super) fn constraint_param_for(&mut self, el: Node<'a>) -> plugin::ConstraintParam {
        if matches!(el.kind(), "string_literal" | "interpolated_string_literal") {
            return plugin::ConstraintParam {
                string: self.extract_string_content(el),
                ty: None,
            };
        }
        self.emit_expr_witness(el);
        plugin::ConstraintParam {
            string: None,
            ty: self.bag_query_expr_span(node_to_span(el)),
        }
    }

    // ---- Runtime exporter modeling ----
    //
    // Static analysis can't run an exporter's import(), so we model the
    // declarative *setup* shapes: the names a package registers as exports
    // map to same-named subs defined in the package. We feed the discovered
    // names into `export_ok` — the existing `@EXPORT_OK` plumbing then drives
    // goto-def (`resolve_imported_function` → same-named sub), cross-file
    // `refs_to` (the consumer's `use X 'name'` FunctionCall ref pins to X;
    // the def is a `Sub { package: X }` symbol), and diagnostic suppression
    // (`find_exporters`). Generators (`exports => { a => \&gen }`) are
    // best-effort: the name resolves to a same-named sub if one exists,
    // otherwise goto-def stops at the `use` line. Conditional/dynamic
    // exports built at runtime are unmodeled.

    /// Walk a pair list, invoking `f(key_string, value_node)` for each
    /// positional `key, value` pair. The single pair scanner — every "find the
    /// value after a key" caller routes here. The separator is irrelevant: `=>`
    /// is a comma that autoquotes a bareword LHS, so `a => 1` and `'a', 1` are
    /// the same flat sequence (elem[2k]→key, elem[2k+1]→value). Accepts a bare
    /// `list_expression` / `parenthesized_expression` or an
    /// `anonymous_hash_expression` (its inner list is unwrapped). Keys are
    /// barewords, autoquoted barewords (`-setup`), or strings; the inter-token
    /// commas / `=>` are skipped positionally. Stops early when `f` returns
    /// `false`.
    pub(super) fn for_each_pair_in_list<F>(&self, container: Node<'a>, mut f: F)
    where
        F: FnMut(&str, Node<'a>) -> bool,
    {
        for (k_node, val) in crate::cst::pair_nodes(container) {
            if let Some(key) = self.extract_node_string(k_node) {
                if !f(&key, val) {
                    return;
                }
            }
        }
    }

    /// Node-level pair walker over a flat sibling sequence — see
    /// `cst::pair_nodes_in` for the pairing rule. Stops early when `f`
    /// returns `false`.
    pub(super) fn for_each_pair_node_in_children<F>(&self, children: &[Node<'a>], mut f: F)
    where
        F: FnMut(Node<'a>, Node<'a>) -> bool,
    {
        for (k_node, val) in crate::cst::pair_nodes_in(children) {
            if !f(k_node, val) {
                return;
            }
        }
    }

    /// Find the value node following a `key` in a list-like container. Thin
    /// single-key lookup over `for_each_pair_in_list`. Pairs positionally —
    /// works for both `key => value` and the plain-comma `'key', value`.
    pub(super) fn value_node_after_key(&self, container: Node<'a>, key: &str) -> Option<Node<'a>> {
        let mut found = None;
        self.for_each_pair_in_list(container, |k, v| {
            if k == key {
                found = Some(v);
                false
            } else {
                true
            }
        });
        found
    }

    /// `use Sub::Exporter -setup => { exports => [...], groups => {...} }` —
    /// fold the `exports` and `groups` member names into the export surface
    /// and record per-member sites so the post-walk pass refs the ones that
    /// name a local sub. Also accepts a bare `exports => [...]` at the top of
    /// the use args (the common minimal form).
    pub(super) fn detect_sub_exporter_use(&mut self, use_node: Node<'a>) {
        // The args live in the use statement's list_expression child.
        let args = (0..use_node.named_child_count())
            .filter_map(|i| use_node.named_child(i))
            .find(|c| c.kind() == "list_expression");
        let Some(args) = args else { return; };
        let setup = self.value_node_after_key(args, "-setup");
        // `-setup => { exports => [...] }` or top-level `exports => [...]`.
        let config = setup.unwrap_or(args);
        self.fold_sub_exporter_config(config);
    }

    /// Fold a Sub::Exporter config (the `{ exports => ..., groups => ... }`
    /// hashref, or the bare top-level use args) into the export surface.
    /// `exports` members are the public export names; `groups` member arrays
    /// list exports that make up each named group (the group name itself is a
    /// `:tag` selector, not a sub, so it never joins the surface — only its
    /// members do). Records per-member sites for the post-walk ref pass.
    pub(super) fn fold_sub_exporter_config(&mut self, config: Node<'a>) {
        if let Some(list) = self.value_node_after_key(config, "exports") {
            let members = self.sub_exporter_member_sites(list);
            self.record_sub_exporter_members(members);
        }
        // Group definitions list the exports that compose each group; those
        // member names join the same surface as `exports`. The group key is a
        // selector, never folded — it's a fat-comma key we descend past to its
        // value (the member array), not a member itself.
        if let Some(groups) = self.value_node_after_key(config, "groups") {
            // `for_each_pair_in_list` holds a shared borrow; collect the
            // group member nodes first, then fold them.
            let mut member_nodes: Vec<Node<'a>> = Vec::new();
            self.for_each_pair_in_list(groups, |_group_name, members_node| {
                member_nodes.push(members_node);
                true
            });
            for members_node in member_nodes {
                let members = self.sub_exporter_member_sites(members_node);
                self.record_sub_exporter_members(members);
            }
        }
    }

    /// Feed Sub::Exporter member `(name, span)` pairs into the export surface
    /// (`export_ok`) and queue per-member sites for the ref pass.
    pub(super) fn record_sub_exporter_members(&mut self, members: Vec<(String, Span)>) {
        let pkg = self.current_package.clone();
        let names: Vec<String> = members.iter().map(|(n, _)| n.clone()).collect();
        self.record_runtime_exports(names);
        for (name, span) in members {
            self.export_member_sites.push((name, span, pkg.clone()));
        }
    }

    /// Collect `(name, span)` for every exported member under a Sub::Exporter
    /// `exports` / `groups`-member value. The value is either an arrayref
    /// (`[ qw(foo bar), baz => \&_gen ]`) or a hashref of name→generator pairs
    /// (`{ name => \&gen }`). The NAME is the export in every case — the
    /// generator coderef / sub body is opaque. `quoted_word_list`, string
    /// literals, and barewords are names directly (unlike `extract_string_list`
    /// we do NOT gate barewords on constant resolution — an export name written
    /// bare is a literal name, not a folded constant). Fat-comma generator
    /// values (`\&gen`, `sub {...}`, `undef`) are skipped: only their keys are
    /// exports. Recurses into nested list/array nodes.
    pub(super) fn sub_exporter_member_sites(&self, node: Node<'a>) -> Vec<(String, Span)> {
        let mut out = Vec::new();
        self.collect_sub_exporter_members(node, &mut out);
        out
    }

    pub(super) fn collect_sub_exporter_members(&self, node: Node<'a>, out: &mut Vec<(String, Span)>) {
        match node.kind() {
            "quoted_word_list" => self.extract_qw_word_spans(node, out),
            "string_literal" | "interpolated_string_literal" => {
                if let Some(text) = self.extract_string_content(node) {
                    out.push((text, self.string_content_span(node)));
                }
            }
            "bareword" | "autoquoted_bareword" => {
                if let Ok(text) = node.utf8_text(self.source) {
                    out.push((text.to_string(), node_to_span(node)));
                }
            }
            "anonymous_hash_expression" => {
                // Generator hashref: keys are export names, values opaque.
                self.collect_sub_exporter_hash_keys(node, out);
            }
            "parenthesized_expression" | "list_expression"
            | "anonymous_array_expression" => {
                self.collect_sub_exporter_list_members(node, out);
            }
            _ => {}
        }
    }

    /// Generator-hashref keys (`{ name => \&gen }`) → `(name, key-span)`. The
    /// key token carries the right span for a member ref; the value is opaque.
    pub(super) fn collect_sub_exporter_hash_keys(&self, node: Node<'a>, out: &mut Vec<(String, Span)>) {
        let list = (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "list_expression")
            .unwrap_or(node);
        let children: Vec<Node<'a>> = (0..list.child_count())
            .filter_map(|i| list.child(i))
            .collect();
        let mut i = 0;
        while i < children.len() {
            let k = children[i];
            i += 1;
            if !k.is_named() {
                continue;
            }
            if let Some((name, span)) = self.sub_exporter_name_token(k) {
                out.push((name, span));
            }
            // Skip past this key's value to the next key.
            while let Some(c) = children.get(i) {
                if c.is_named() {
                    i += 1;
                    break;
                }
                i += 1;
            }
        }
    }

    /// Walk a Sub::Exporter array/list, treating `name => <opaque>` fat-comma
    /// pairs by keeping the key and skipping the value, and bare `qw()` /
    /// string / bareword entries as standalone names.
    pub(super) fn collect_sub_exporter_list_members(&self, list: Node<'a>, out: &mut Vec<(String, Span)>) {
        let children: Vec<Node<'a>> = (0..list.child_count())
            .filter_map(|i| list.child(i))
            .collect();
        let mut i = 0;
        while i < children.len() {
            let c = children[i];
            if !c.is_named() {
                i += 1;
                continue;
            }
            if matches!(
                c.kind(),
                "parenthesized_expression" | "list_expression" | "anonymous_array_expression"
            ) {
                self.collect_sub_exporter_list_members(c, out);
                i += 1;
                continue;
            }
            if c.kind() == "quoted_word_list" {
                self.extract_qw_word_spans(c, out);
                i += 1;
                continue;
            }
            if let Some((name, span)) = self.sub_exporter_name_token(c) {
                // Look ahead for a fat-comma generator value to skip.
                let mut j = i + 1;
                let next_named = loop {
                    match children.get(j) {
                        Some(n) if n.is_named() => break Some(*n),
                        Some(_) => j += 1,
                        None => break None,
                    }
                };
                out.push((name, span));
                if let Some(nv) = next_named {
                    if matches!(
                        nv.kind(),
                        "refgen_expression"
                            | "anonymous_subroutine_expression"
                            | "undef_expression"
                            | "anonymous_hash_expression"
                            | "scalar"
                    ) {
                        i = j + 1;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }

    /// A standalone export-name token (bareword / autoquoted bareword / string
    /// literal) → its literal name + span. Barewords are NOT constant-folded:
    /// an export name written bare is a literal name.
    pub(super) fn sub_exporter_name_token(&self, node: Node<'a>) -> Option<(String, Span)> {
        match node.kind() {
            "bareword" | "autoquoted_bareword" => node
                .utf8_text(self.source)
                .ok()
                .map(|t| (t.to_string(), node_to_span(node))),
            "string_literal" | "interpolated_string_literal" => self
                .extract_string_content(node)
                .map(|t| (t, self.string_content_span(node))),
            _ => None,
        }
    }

    /// `Moose::Exporter->setup_import_methods(with_meta => [...], as_is => [...])`
    /// or `Sub::Exporter::setup_exporter({ exports => [...] })`. `args` is the
    /// call's argument node. Pull names from the export-bearing keys.
    pub(super) fn detect_exporter_setup_call(&mut self, callee: &str, args: Node<'a>) {
        match callee {
            "setup_import_methods" => {
                // Moose::Exporter: with_meta + as_is are exported names.
                let mut names = Vec::new();
                for key in ["with_meta", "as_is"] {
                    if let Some(list) = self.value_node_after_key(args, key) {
                        names.extend(self.extract_string_names(list));
                    }
                }
                self.record_runtime_exports(names);
                // Form 3: `also => [ 'Moose', ... ]` re-exports each named
                // module's surface. The value is a literal module-name list
                // (string/bareword elements); each is a re-export edge. The
                // Exporter::Tiny equivalent uses the same `also =>` key shape,
                // so recognizing by the key (not a module allowlist, rule #10)
                // covers both.
                if let Some(list) = self.value_node_after_key(args, "also") {
                    for module in self.extract_string_names(list) {
                        self.record_reexport_edge(&module);
                    }
                }
            }
            "setup_exporter" => {
                // Sub::Exporter::setup_exporter({ exports => [...], groups => {...} }).
                // The config hashref is the first positional; fold it the same
                // as the `-setup` use form so exports + groups + member refs
                // ride one path.
                let config = args
                    .named_child(0)
                    .filter(|c| c.kind() == "anonymous_hash_expression")
                    .unwrap_or(args);
                self.fold_sub_exporter_config(config);
            }
            "add_type" => {
                // Type::Library / Exporter::Tiny: __PACKAGE__->add_type({ name => 'X' })
                // registers `X` as an exported constant sub. Bare-name form
                // `add_type(Foo => ...)` / `add_type('Foo')` also seen.
                if let Some(name_node) = self.value_node_after_key(args, "name") {
                    self.record_runtime_exports(self.extract_string_names(name_node));
                } else if let Some(first) = args.named_child(0) {
                    // `add_type Foo, ...` — first positional is the name.
                    if matches!(first.kind(), "string_literal" | "interpolated_string_literal" | "bareword" | "autoquoted_bareword") {
                        self.record_runtime_exports(self.extract_string_names(first));
                    }
                }
            }
            _ => {}
        }
    }

    /// Map a Moo/Moose `isa` type constraint string to an InferredType.
    pub(super) fn map_isa_to_type(&self, isa: &str, mode: FrameworkMode) -> Option<InferredType> {
        match isa {
            "Str" => Some(InferredType::String),
            "Int" | "Num" => Some(InferredType::Numeric),
            "Bool" => Some(InferredType::Bool),
            "HashRef" => Some(InferredType::HashRef),
            "ArrayRef" => Some(InferredType::ArrayRef),
            "CodeRef" => Some(InferredType::CodeRef { return_edge: None }),
            "RegexpRef" => Some(InferredType::Regexp),
            _ => {
                // `Maybe[T]` / `Optional[T]` (Type::Tiny / Types::Standard)
                // → `Optional<inner>`, recursing on the wrapped constraint.
                // Checked before the InstanceOf/Moose-class fallbacks so
                // `Maybe[Int]` doesn't read as a class named "Maybe[Int]".
                for prefix in ["Maybe[", "Optional["] {
                    if let Some(inner) = isa.strip_prefix(prefix).and_then(|r| r.strip_suffix(']')) {
                        return self
                            .map_isa_to_type(inner.trim(), mode)
                            .map(|t| InferredType::Optional(Box::new(t)));
                    }
                }
                // InstanceOf['Foo::Bar'] (Moo style) — the isa value is
                // valid-ish Perl syntax, so re-parse it with tree-sitter
                // and pull the class name out of the tree rather than
                // hand-stripping brackets and quotes.
                if let Some(class) = parse_instance_of(isa) {
                    return Some(InferredType::ClassName(class));
                }
                // Moose allows class names as types (contains :: or starts uppercase)
                if mode == FrameworkMode::Moose && (isa.contains("::") || isa.starts_with(|c: char| c.is_uppercase())) {
                    // Avoid matching Moose type names like "Str", "Int" etc. already handled above
                    if isa.contains("::") || isa.len() > 3 {
                        return Some(InferredType::ClassName(isa.to_string()));
                    }
                }
                None
            }
        }
    }
}
