//! Class/member query methods: method return types, member completion
//! surfaces, field/enum domains, package context.

use super::*;

impl FileAnalysis {
    /// Bag-routed query: "what does `Symbol(sym_id)` return at this
    /// arity?" Runs through the full reducer registry — Plugin
    /// overrides dominate, then arity dispatch, then `SubReturnReducer`
    /// (which claims plain writeback-pushed `InferredType` witnesses).
    /// Returns `None` when nothing in the bag answers.
    pub(crate) fn symbol_return_type_via_bag(
        &self,
        sym_id: SymbolId,
        arg_count: Option<usize>,
    ) -> Option<InferredType> {
        self.symbol_return_type_via_bag_ctx(sym_id, arg_count, None)
    }

    /// As `symbol_return_type_via_bag`, but with a `ModuleIndex` so the
    /// reducer chase can cross module boundaries — the sub's body may return
    /// a value typed by a cross-file method chain (`my $m = Foo->new->bar; …;
    /// return $m`). Without the index that chain dies at the boundary and the
    /// return type comes back `None`. Pass the index whenever the query has
    /// one (hover/completion against a cached module); the bare wrapper above
    /// keeps `None` for the many call sites that don't.
    pub(crate) fn symbol_return_type_via_bag_ctx(
        &self,
        sym_id: SymbolId,
        arg_count: Option<usize>,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry,
            WitnessAttachment,
        };
        let att = WitnessAttachment::Symbol(sym_id);
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context(module_index);
        // Default the arity hint from the sym's own param count when
        // the caller didn't supply one — the sym's params count IS its
        // native arity. Mojo writer (params=1) answers its
        // `(AtLeast(1), Receiver)` arm, getter (params=0) its
        // `(Empty, Concrete(_))` arm. Without this a writer's
        // UnionOnArgs would be unmatched at a None hint (AtLeast(1)
        // doesn't match None) and the query would silently return None.
        //
        // Default receiver = `ClassName(class)` so the writer's
        // `Receiver` placeholder evaluates to the natural fluent
        // answer at sym-introspection time.
        let resolved_arity = arg_count.map(|n| n as u32).or_else(|| {
            self.symbols
                .get(sym_id.0 as usize)
                .and_then(|s| match &s.detail {
                    SymbolDetail::Sub { params, .. } => Some(params.len() as u32),
                    _ => None,
                })
        });
        let receiver = self
            .symbols
            .get(sym_id.0 as usize)
            .and_then(|s| s.package.clone())
            .map(InferredType::ClassName);
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: resolved_arity,
            receiver,
            args: Vec::new(),
            context: Some(&ctx),
        };
        match reg.query(&self.witnesses, &q) {
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        }
    }

    /// Find a method's return type within a class/package, walking
    /// inheritance. Thin wrapper that queries the bag's class-keyed
    /// `PackageSymbol{package, method}` attachment with the caller's
    /// `arg_count` as arity hint. Inheritance composes through
    /// `package_parents` (carried in `BagContext`); cross-file
    /// classes resolve via `module_index`. No procedural ancestor
    /// walk; no procedural overload picking — the registry's
    /// `ReturnExprReducer` claims `PackageSymbol + ReturnExpr` and
    /// the structural-walk code in `query_rec` handles MRO.
    pub(crate) fn find_method_return_type(
        &self,
        class_name: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        arg_count: Option<usize>,
    ) -> Option<InferredType> {
        // Default receiver = `ClassName(class_name)` so that
        // `ReturnExpr::Receiver` evaluates correctly for class-keyed
        // method-return queries that don't have a specific
        // call-site invocant — Mojo `has 'title'` writer's
        // Receiver evaluates to ClassName(Bar), DBIC `find`'s
        // RowOf(Receiver) wraps the Parametric (when one is
        // supplied via the `arg_count` Some path elsewhere). Same
        // policy as `query_sub_return_type`'s class-fallback rule.
        self.method_return_type_on(
            class_name,
            &InferredType::ClassName(class_name.to_string()),
            method_name,
            module_index,
            arg_count,
        )
    }

    /// `find_method_return_type` with the receiver's FULL value threaded
    /// into the `PackageSymbol` query — the receiver-relative shapes
    /// (`ReturnExpr::Receiver`, `Operator(RowOf/ParamOf)`) substitute the
    /// rich value (`Parametric(Instance { args })`) rather than a bare
    /// class projection. Callers with a value in hand pass it here (via
    /// `member_value_type` / `dispatch_of`); the string-keyed wrapper
    /// above defaults the receiver to the class identity.
    pub(crate) fn method_return_type_on(
        &self,
        class_name: &str,
        receiver: &InferredType,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        arg_count: Option<usize>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry,
            WitnessAttachment,
        };
        let framework = self.package_framework(class_name).unwrap_or(FrameworkFact::Plain);
        let att = WitnessAttachment::PackageSymbol {
            package: class_name.to_string(),
            name: method_name.to_string(),
        };
        let ctx = self.bag_context(module_index);
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework,
            arity_hint: arg_count.map(|n| n as u32),
            receiver: Some(receiver.clone()),
            args: Vec::new(),
            context: Some(&ctx),
        };
        let reg = ReducerRegistry::with_defaults();
        match reg.query(&self.witnesses, &q) {
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        }
    }

    /// Format a method completion detail string, appending return type if known.
    pub(super) fn method_detail(
        &self,
        class_name: &str,
        method_name: &str,
        defining_class: Option<&str>,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> String {
        let base = if let Some(dc) = defining_class {
            if dc != class_name {
                format!("{} (from {})", class_name, dc)
            } else {
                class_name.to_string()
            }
        } else {
            class_name.to_string()
        };
        if let Some(ref rt) = self.find_method_return_type(class_name, method_name, module_index, None) {
            // `opaque_return` lets the declaring plugin say "this chain
            // link is internal plumbing — don't render the class name OR
            // the return type". The chain still resolves; the user just
            // doesn't see the proxy-class path at every completion detail.
            //
            // Check both the context class AND the defining class: the
            // plugin declares opacity on the symbol where the method
            // LIVES, which is the defining class during a cross-class
            // walk (e.g. Users inheriting the helper from
            // Mojolicious::Controller).
            let opaque = self.method_opaque_return_cross_file(class_name, method_name, module_index)
                || defining_class.is_some_and(|dc| {
                    self.method_opaque_return_cross_file(dc, method_name, module_index)
                });
            if opaque {
                return String::new();
            }
            format!("{} → {}", base, self.render_type(&rt))
        } else {
            base
        }
    }

    /// Does the Method/Sub `method_name` on `class_name` opt out of
    /// rendering its return type at call sites? Plugin-declared via
    /// `opaque_return` on the symbol's detail. Walks both local
    /// symbols and any cross-file modules that emit content on the
    /// class (plugin helpers land in the file where the registration
    /// runs, not in the target class's own module).
    fn method_opaque_return(&self, class_name: &str, method_name: &str) -> bool {
        let check = |sym: &Symbol| -> bool {
            if sym.name != method_name { return false; }
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { return false; }
            if sym.package.as_deref() != Some(class_name) { return false; }
            matches!(&sym.detail, SymbolDetail::Sub { opaque_return: true, .. })
        };
        for sym in &self.symbols {
            if check(sym) { return true; }
        }
        false
    }

    /// Cross-file-aware variant: used by `method_detail` during
    /// completion to decide whether to suppress the proxy chain in
    /// the detail string, even when the declaring plugin emitted the
    /// method from another file. Same contract as
    /// `method_opaque_return` otherwise.
    fn method_opaque_return_cross_file(
        &self,
        class_name: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> bool {
        if self.method_opaque_return(class_name, method_name) {
            return true;
        }
        let Some(idx) = module_index else { return false };
        let mut found = false;
        idx.for_each_entity_bridged_to_named(class_name, method_name, &mut |_mod, _cached, sym| {
            use std::ops::ControlFlow;
            if sym.name != method_name {
                return ControlFlow::Continue(());
            }
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                return ControlFlow::Continue(());
            }
            if matches!(&sym.detail, SymbolDetail::Sub { opaque_return: true, .. }) {
                found = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
        found
    }

    /// Complete methods for a known class name, walking the inheritance chain.
    pub fn complete_methods_for_class(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        let mut candidates = Vec::new();
        let mut seen_names: HashSet<String> = HashSet::new();

        // Check for class definition → implicit new
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Class) && sym.name == class_name {
                candidates.push(CompletionCandidate {
                    label: "new".to_string(),
                    kind: SymKind::Method,
                    detail: Some(self.method_detail(class_name, "new", None, module_index)),
                    insert_text: None,
                    sort_priority: PRIORITY_LOCAL,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
                seen_names.insert("new".to_string());
                break;
            }
        }

        // Collect methods from this class and all ancestors. Perl has no
        // access-specifier concept, so no symbol here ever carries
        // "non_public" — `requesting_class: None` is a no-op gate.
        self.collect_ancestor_methods(
            class_name, class_name, module_index, &mut candidates, &mut seen_names, 0, None,
        );

        candidates
    }

    /// Push a class's DATA fields (class-body `Variable`/`Field` members,
    /// not method locals — anchored on the method-declaration scope) into
    /// `candidates`, deduped by `seen`. Called per-class in the ancestor
    /// walk: on `self` for local classes, on a cached module's analysis
    /// for cross-file ones.
    fn collect_class_fields(
        &self,
        cls: &str,
        candidates: &mut Vec<CompletionCandidate>,
        seen: &mut HashSet<String>,
        requesting_class: Option<&str>,
    ) {
        let class_body = self
            .symbols
            .iter()
            .find(|s| {
                matches!(s.kind, SymKind::Sub | SymKind::Method) && self.symbol_in_class(s.id, cls)
            })
            .map(|m| m.scope);
        // A bodiless method DECLARATION (`void f(int arg1, int arg2);`) or a
        // function-pointer typedef (`using F = void(*)(void* arg1)`) carries no
        // `@scope.sub` body, so its parameters land directly on the class body
        // scope with the sticky class package — indistinguishable from a data
        // member by scope/kind alone (they're `Variable`, like inline-union
        // members we DO want). A parameter's selection span sits inside a
        // recorded parameter-list region; that's the value-borne discriminator
        // (same idiom the use-after-move param check uses).
        let contains = |outer: &Span, inner: &Span| {
            (outer.start.row, outer.start.column) <= (inner.start.row, inner.start.column)
                && (inner.end.row, inner.end.column) <= (outer.end.row, outer.end.column)
        };
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Variable | SymKind::Field)
                && self.symbol_in_class(sym.id, cls)
                // A Field is class content BY KIND — extraction only mints it
                // for real data members, and constructor promotion (php
                // `__construct(public string $name)`) legitimately places one
                // inside a sub body and a param region, so the locals gates
                // below apply to Variables only.
                && (matches!(sym.kind, SymKind::Field)
                    || (!self
                        .pack
                        .param_regions
                        .iter()
                        .any(|pr| contains(pr, &sym.selection_span))
                        // the class body itself, or a nested container body
                        // inside it (an inline union's members complete flat
                        // on the struct) — but never a method body (its
                        // locals carry the sticky class package too; the Sub
                        // boundary is what marks them locals).
                        && class_body.is_none_or(|cb| {
                            self.scope_chain(sym.scope).contains(&cb)
                                && !self.scope_within_sub_body(sym.scope)
                        })))
                && !self.pack.receiver_names.contains(&sym.name)
                // an anonymous container (`(union)`) is structure, not an
                // addressable member
                && !sym.attributes.iter().any(|a| a == "anonymous")
                // access-specifier gate: a non-public member
                // completes only from inside its OWN class's lexical body —
                // two-state (friend/protected-via-inheritance not modeled).
                && (requesting_class == Some(cls)
                    || !sym.attributes.iter().any(|a| a == "non_public"))
                && seen.insert(sym.name.clone())
            {
                candidates.push(CompletionCandidate {
                    label: sym.name.clone(),
                    kind: sym.kind,
                    detail: None,
                    insert_text: None,
                    sort_priority: PRIORITY_LOCAL,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                });
            }
        }
    }

    /// Members (methods + data fields) of a class for pack-language member
    /// completion (`obj.` / `obj->`). Unlike `complete_methods_for_class`
    /// (Perl-shaped: methods + a synthesized `new`), this includes data
    /// fields and mints no constructor — C++/Python member access lists
    /// the real members. Methods (and inherited ones) come from the shared
    /// ancestor walk; fields are this class's `Variable`/`Field` symbols.
    /// `requesting_class` is the class the completion CURSOR is lexically
    /// inside (`None` from free-standing code) — the access-specifier gate:
    /// a non-public member offers only when the cursor is
    /// inside that SAME class's own body. A caller with no cursor context
    /// passes `None` and safely under-offers to "public only" — it never
    /// leaks a private member.
    pub fn complete_members_for_class(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        requesting_class: Option<&str>,
    ) -> Vec<CompletionCandidate> {
        let mut candidates = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        self.collect_ancestor_methods(
            class_name, class_name, module_index, &mut candidates, &mut seen, 0, requesting_class,
        );
        // Data members from this class AND its ancestors. A field lives in
        // its class body scope (or a nested container body — inline unions);
        // a Variable inside a Sub-kind scope is a local/param. Anchored on
        // where each class's methods are declared — that IS the body scope,
        // whichever node carries it.
        self.for_each_ancestor_class(class_name, module_index, |cls| {
            self.collect_class_fields(cls, &mut candidates, &mut seen, requesting_class);
            // a class defined in ANOTHER file — pull its fields from the
            // cached module so cross-file member completion is complete
            // (methods already cross via collect_ancestor_methods).
            if let Some(mi) = module_index {
                // Every file declaring `cls` — fields of a reopened class
                // live across the set (`seen` dedups).
                for cached in mi.visible_def_candidates(cls) {
                    mi.whole_present(&cached).collect_class_fields(
                        cls, &mut candidates, &mut seen, requesting_class,
                    );
                }
            }
            std::ops::ControlFlow::Continue(())
        });
        candidates
    }

    /// The union container symbol whose body scope declares `sym`, if any —
    /// the symbol carrying the "union" attribute (a named union type, a named
    /// field-union, or a synthetic `(union)` container) whose span holds the
    /// member's declaring scope. Value-borne identification: the attribute is
    /// stamped at extraction; no name/shape test here.
    pub fn union_container_of(&self, sym: &Symbol) -> Option<&Symbol> {
        let sc = self.scope(sym.scope);
        let contains = |o: &Span, i: &Span| {
            (o.start.row, o.start.column) <= (i.start.row, i.start.column)
                && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
        };
        self.symbols.iter().find(|c| {
            c.id != sym.id
                && c.attributes.iter().any(|a| a == "union")
                && Some(c.scope) == sc.parent
                && contains(&c.span, &sc.span)
        })
    }

    /// The other members overlaying `sym`'s storage — Variables sharing its
    /// union body scope, rendered `name: type` (bare name when untyped).
    /// `None` when `sym` isn't a union member.
    pub fn union_overlay(&self, sym: &Symbol) -> Option<Vec<String>> {
        self.union_container_of(sym)?;
        Some(
            self.symbols
                .iter()
                .filter(|s| {
                    s.id != sym.id
                        && s.scope == sym.scope
                        && matches!(s.kind, SymKind::Variable | SymKind::Field)
                })
                .map(|s| {
                    match self.inferred_type_via_bag(&s.name, s.span.end) {
                        Some(ty) => format!("{}: {}", s.name, s.display_type(&ty)),
                        None => s.name.clone(),
                    }
                })
                .collect(),
        )
    }

    /// The declared type of data field `field` on `class` (or an ancestor) —
    /// a member's type for pack-language member-access COMPLETION chains
    /// (`a.b.`, cursor-time, where no ref exists yet). Resolves through the
    /// shared `resolve_method_in_ancestors` walk and reads the type from the
    /// field's OWNING analysis — so a cross-file field's type resolves, not
    /// just a local one.
    pub fn field_type_on_class(
        &self,
        class: &str,
        field: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        match self.resolve_method_in_ancestors(class, field, module_index)? {
            MethodResolution::Local { sym_id, .. } => {
                self.inferred_type_via_bag(field, self.symbol(sym_id).span.end)
            }
            MethodResolution::CrossFile { class, .. } => {
                let idx = module_index?;
                // Whichever of `class`'s candidate files holds the field.
                // The field's type lives in the OWNING file's bag; the symbol
                // scan needs symbols too; take the whole view.
                idx.visible_def_candidates(&class).iter().find_map(|cached| {
                    let full = idx.whole_present(cached);
                    let sym = full.symbols.iter().find(|s| {
                        matches!(s.kind, SymKind::Variable | SymKind::Field)
                            && s.name == field
                            && s.package.as_deref() == Some(class.as_str())
                            && full.symbol_is_class_content(s)
                    })?;
                    full.inferred_type_via_bag(field, sym.span.end)
                })
            }
        }
    }

    /// The type SPELLING a field's declared type edges to — the `TypeName(n)`
    /// target of the field's `Variable → Edge(TypeName(n))` witness (`op_type`
    /// declared `PERL_BITFIELD16` → `Some("PERL_BITFIELD16")`). This is the
    /// alias/macro name whose provenance chain hover walks for the concrete
    /// leaf; the flow type (`inferred_type_via_bag`) is the join abstraction the
    /// same edge resolves to. `None` when the field's declared type is a
    /// primitive/committed value rather than an alias edge. Reads the field's
    /// OWNING analysis (cross-file fields resolve, like `field_type_on_class`).
    pub fn member_type_spelling(
        &self,
        class: &str,
        field: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        match self.resolve_method_in_ancestors(class, field, module_index)? {
            MethodResolution::Local { sym_id, .. } => {
                self.type_name_edge_of(&self.symbol(sym_id).name, self.symbol(sym_id).scope)
            }
            MethodResolution::CrossFile { class, .. } => {
                let idx = module_index?;
                // `type_name_edge_of` reads the field's `Edge(TypeName(_))`
                // witness — plus the symbol scan; take the whole view of
                // whichever candidate file holds the field.
                idx.visible_def_candidates(&class).iter().find_map(|cached| {
                    let full = idx.whole_present(cached);
                    let sym = full.symbols.iter().find(|s| {
                        matches!(s.kind, SymKind::Variable | SymKind::Field)
                            && s.name == field
                            && s.package.as_deref() == Some(class.as_str())
                    })?;
                    full.type_name_edge_of(&sym.name, sym.scope)
                })
            }
        }
    }

    /// Resolve a value token (an enumerator USE) to its enum. An enumerator
    /// carries its enum as its symbol `package` (the enum-container work):
    /// `OP_SCOPE` → `opcode`. Local first, then cross-file by name
    /// (`get_cached(value)` finds the header that declares it — `op_type ==
    /// OP_SCOPE` in op.c resolves through opnames.h). `None` when `value` is
    /// not a packaged file-scope symbol (a plain int / local), so a
    /// non-enum comparison drops out of the domain fold.
    pub(crate) fn resolve_enumerator_enum(
        &self,
        value: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        // "" is the extraction sentinel for a non-identifier operand — it
        // can't name an enumerator, so skip the symbol sweep + cache probe.
        if value.is_empty() {
            return None;
        }
        let packaged = |a: &FileAnalysis| {
            a.symbols
                .iter()
                .find(|s| {
                    s.name == value
                        && matches!(s.kind, SymKind::Variable | SymKind::Field | SymKind::Enumerator)
                        && s.package.is_some()
                })
                .and_then(|s| s.package.clone())
        };
        if let Some(p) = packaged(self) {
            return Some(p);
        }
        let idx = module_index?;
        // Symbols-axis read only (name/kind/package scan) — first candidate
        // file that attributes the value to a package.
        idx.visible_def_candidates(value)
            .iter()
            .find_map(|cached| packaged(&idx.symbols_present(cached)))
    }

    /// The canonical, language-generic `Field{owner, name}` subject for a
    /// field access `(class, name)` — the SAME bag primitive a C struct
    /// member folds onto (`witnesses::WitnessAttachment::Field`). This is the
    /// ONE place a field's project-wide identity is minted; every Perl field
    /// flavor (Moo/Moose `has`, Corinna `field`, classic `$self->{k}` hash
    /// slots, DBIC/Mojo columns) and the C struct member all route here, so
    /// downstream (domain fold, cross-class analysis) is source-agnostic —
    /// the consumer asks the subject, never the flavor (rule #10).
    ///
    /// `owner` is the DECLARING class: the `resolve_method_in_ancestors` walk
    /// (the one hover/goto-def use) climbs to the class whose accessor backs
    /// the slot, so an inherited/role field accessed on any receiver converges
    /// on one subject (a child's `$self->{name}` and the parent's `has 'name'`
    /// are the same `Field`). A classic hash slot with no accessor doesn't
    /// resolve through method lookup — it isn't inherited that way — so the
    /// owner falls back to the access class (its own per-class storage).
    pub fn field_subject(
        &self,
        class: &str,
        name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> crate::model::witnesses::WitnessAttachment {
        let owner = match self.resolve_method_in_ancestors(class, name, module_index) {
            Some(MethodResolution::Local { class, .. })
            | Some(MethodResolution::CrossFile { class, .. }) => class,
            None => class.to_string(),
        };
        crate::model::witnesses::WitnessAttachment::Field { owner, name: name.to_string() }
    }

    /// Route a field ACCESS ref — in any of Perl's varied forms — onto its
    /// shared `Field{owner, name}` subject. The dispatch is on the access
    /// SHAPE (accessor call / hash-slot deref / Corinna field variable) to
    /// recover the owning class + slot name; identity minting is delegated to
    /// `field_subject`, so the field's FLAVOR never enters here. This is the
    /// projection every source-agnostic field consumer calls to fold a use
    /// onto the one subject (the Perl analog of the C domain-site routing).
    /// Parked: Perl domain typing isn't surfaced yet (go-live map "Deferred");
    /// the tests keep the routing proven until it is.
    #[allow(dead_code)]
    pub fn field_subject_of_ref(
        &self,
        r: &Ref,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<crate::model::witnesses::WitnessAttachment> {
        let (class, name) = match &r.kind {
            RefKind::MethodCall { .. } => {
                let class = self.method_call_invocant_class(r, module_index)?;
                (class, r.unqualified_target_name().to_string())
            }
            RefKind::HashKeyAccess { .. } => {
                let class = match r.hash_key_owner() {
                    Some(HashKeyOwner::Class(c)) | Some(HashKeyOwner::Bridged { class: c }) => {
                        c.clone()
                    }
                    _ => match self.deferred_hash_key_owner(r, module_index)? {
                        HashKeyOwner::Class(c) | HashKeyOwner::Bridged { class: c } => c,
                        HashKeyOwner::Sub { package: Some(c), name }
                            if crate::model::conventions::is_constructor_name(&name) =>
                        {
                            c
                        }
                        _ => return None,
                    },
                };
                (class, r.target_name.clone())
            }
            RefKind::Variable => {
                // Corinna `field $x` use: the ref resolves to a Field symbol
                // whose package IS the declaring class (fields are per-class).
                let sym = self.symbol(r.resolved_symbol()?);
                if !matches!(sym.kind, SymKind::Field) {
                    return None;
                }
                let bare = sym.name.trim_start_matches(['$', '@', '%']);
                (sym.package.clone()?, bare.to_string())
            }
            _ => return None,
        };
        Some(self.field_subject(&class, &name, module_index))
    }

    /// The DOMAIN of a data field on `class` — the enum it is *used as*,
    /// recovered from usage (`slot == OP_CONST`, `slot = OP_FREED`, …).
    /// Keyed by the canonical `field_subject` (declaring-class owner), so
    /// every access — whatever the receiver's concrete struct — folds onto
    /// ONE subject.
    pub fn field_domain(
        &self,
        class: &str,
        field: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<NominalDomain> {
        let crate::model::witnesses::WitnessAttachment::Field { owner, .. } =
            self.field_subject(class, field, module_index)
        else {
            return None;
        };
        self.field_domain_for_owner(&owner, field, module_index)
    }

    /// Fold this file's domain sites for `Field{owner, field}` and query
    /// `DomainCoherenceFold`. The witnesses are built into a scratch bag at
    /// query time because a site's enum resolves cross-file (the module
    /// index is only in hand here); the fold + majority rule live in the
    /// reducer (`witnesses::domain_coherence`).
    ///
    /// Two gates keep the vote honest:
    /// - **owner**: a site votes only when its own receiver resolves to the
    ///   SAME declaring owner as the queried subject (`domain_site_owner`) —
    ///   name-keyed pooling let `struct basket { int kind; }` contaminate
    ///   `struct crate { int kind; }`. A site whose receiver doesn't resolve
    ///   votes nowhere (we don't know whose slot it is).
    /// - **counter-evidence**: a gathered site whose value operand is not an
    ///   enumerator pushes `enum_type: None`, so the denominator is the
    ///   slot's whole interaction story, not the enum-shaped subset.
    pub fn field_domain_for_owner(
        &self,
        owner: &str,
        field: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<NominalDomain> {
        use crate::model::witnesses::{
            domain_coherence, ReducedValue, ReducerQuery, ReducerRegistry, Witness,
            WitnessAttachment, WitnessBag, WitnessPayload, WitnessSource,
        };
        let att = WitnessAttachment::Field { owner: owner.to_string(), name: field.to_string() };
        let mut bag = WitnessBag::new();
        for site in &self.pack.domain_sites {
            if site.slot != field {
                continue;
            }
            if self.domain_site_owner(site, module_index).as_deref() != Some(owner) {
                continue;
            }
            let enum_name = self.resolve_enumerator_enum(&site.value, module_index);
            bag.push(Witness {
                attachment: att.clone(),
                source: WitnessSource::Builder("field_domain".into()),
                payload: WitnessPayload::DomainCompare { enum_type: enum_name },
                span: site.slot_span,
            });
        }
        let reg = ReducerRegistry::with_defaults();
        let ctx = self.bag_context(module_index);
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: crate::model::witnesses::FrameworkFact::Plain,
            arity_hint: None,
            receiver: None,
            args: Vec::new(),
            context: Some(&ctx),
        };
        let domain = match reg.query(&bag, &q) {
            ReducedValue::Type(InferredType::ClassName(d)) => d,
            ReducedValue::Type(_)
            | ReducedValue::FactMap(_)
            | ReducedValue::None => return None,
        };
        // Confidence = dominant share, recomputed from the same witnesses the
        // reducer folded (the coherence helper reports it deterministically).
        let ws: Vec<&Witness> = bag.all().iter().collect();
        let confidence = domain_coherence(&ws)
            .map(|(_, count, total)| count as f32 / total as f32)
            .unwrap_or(0.0);
        Some(NominalDomain { domain, confidence })
    }

    /// The canonical `Field` owner of one domain site: the member ref at the
    /// site's own span (the member pattern captures the same field token the
    /// domain pattern does), its receiver's class, then the SAME
    /// `field_subject` ancestor walk that minted the queried subject — so an
    /// access through a subtype converges on the declaring owner (perl5's
    /// BASEOP-role structs all collapse to one `op_type` subject). `None`
    /// when the receiver doesn't resolve — such a site belongs to no subject.
    fn domain_site_owner(
        &self,
        site: &DomainSite,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        let r = self
            .refs
            .iter()
            .find(|r| r.span == site.slot_span && matches!(r.kind, RefKind::MethodCall { .. }))?;
        let class = self.method_call_invocant_class(r, module_index)?;
        let crate::model::witnesses::WitnessAttachment::Field { owner, .. } =
            self.field_subject(&class, &site.slot, module_index)
        else {
            return None;
        };
        Some(owner)
    }

    /// Reverse bridge: the slot spans in THIS file whose domain value
    /// resolves to `enum_name` — what a find-references on `enum_name` (or
    /// one of its enumerators) surfaces backward. A targeted scan of the
    /// stored domain sites, not a full witness sweep.
    pub fn field_sites_for_enum(
        &self,
        enum_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<Span> {
        self.pack.domain_sites
            .iter()
            .filter(|s| {
                self.resolve_enumerator_enum(&s.value, module_index).as_deref() == Some(enum_name)
            })
            .map(|s| s.slot_span)
            .collect()
    }

    /// The enumerators of `enum_name`, in declaration order — the members
    /// a `field == |` domain slot ranks first. An enumerator carries its
    /// enum as its symbol `package` (the inverse of `resolve_enumerator_enum`).
    /// Local first; when the enum is declared cross-file (perl5's `opcode`
    /// lives in a header, not the querying `.c`) its declaring file is
    /// fetched by name so members enumerate even when none are in scope.
    pub fn enum_members(
        &self,
        enum_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<String> {
        let collect = |a: &FileAnalysis| -> Vec<String> {
            a.symbols
                .iter()
                .filter(|s| {
                    matches!(s.kind, SymKind::Enumerator) && s.package.as_deref() == Some(enum_name)
                })
                .map(|s| s.name.clone())
                .collect()
        };
        let local = collect(self);
        if !local.is_empty() {
            return local;
        }
        module_index
            .and_then(|idx| {
                // First candidate file whose analysis yields members.
                idx.visible_def_candidates(enum_name)
                    .iter()
                    .map(|cached| collect(&idx.whole_present(cached)))
                    .find(|v| !v.is_empty())
            })
            .unwrap_or_default()
    }

    /// The `TypeName(n)` a `Variable{name, scope}` declared type edges to, read
    /// from the recorded witness bag (the alias edge the skeleton emits for a
    /// class-shaped declared type). Scope-matched so two same-named fields in
    /// different structs don't cross-wire.
    pub(crate) fn type_name_edge_of(&self, name: &str, scope: ScopeId) -> Option<String> {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload};
        self.witnesses.all().iter().find_map(|w| match (&w.attachment, &w.payload) {
            (
                WitnessAttachment::Variable { name: n, scope: s },
                WitnessPayload::Edge(WitnessAttachment::TypeName(target)),
            ) if n == name && *s == scope => Some(target.clone()),
            _ => None,
        })
    }

    /// Resolve a type-alias/macro spelling to its terminal concrete type by
    /// chasing the recorded `TypeName` alias graph (`U16 → U16TYPE → unsigned
    /// short`), cross-file capable. A thin `TypeName(name)` registry query —
    /// the same chase the field's flow type already performs, exposed so a
    /// display consumer can walk a chosen macro-variant body to its leaf
    /// without re-deriving the graph. `None` when the spelling doesn't resolve
    /// past itself (an unresolved `TypeName` is terminal → `ClassName(name)`,
    /// which this returns as-is for the caller to compare against the input).
    pub fn resolve_type_name(
        &self,
        name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        use crate::model::witnesses::{
            FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry, WitnessAttachment,
        };
        let att = WitnessAttachment::TypeName(name.to_string());
        let ctx = self.bag_context(module_index);
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: None,
            receiver: None,
            args: Vec::new(),
            context: Some(&ctx),
        };
        let reg = ReducerRegistry::with_defaults();
        match reg.query(&self.witnesses, &q) {
            ReducedValue::Type(t) => Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        }
    }

    /// Get the enclosing package name at a point.
    ///
    /// Resolves via `package_ranges` (innermost — latest-starting —
    /// containing range wins). Falls back to a scope walk for older
    /// cache blobs deserialised before `package_ranges` existed.
    #[allow(dead_code)]
    pub fn package_at(&self, point: Point) -> Option<&str> {
        if !self.package_ranges.is_empty() {
            let mut best: Option<&PackageRange> = None;
            for r in &self.package_ranges {
                if !contains_point(&r.span, point) {
                    continue;
                }
                let win = match best {
                    None => true,
                    Some(prev) => {
                        // Latest-starting wins; on a tie, narrower span wins.
                        let cur_start = (r.span.start.row, r.span.start.column);
                        let prev_start = (prev.span.start.row, prev.span.start.column);
                        cur_start > prev_start
                            || (cur_start == prev_start && span_size(&r.span) < span_size(&prev.span))
                    }
                };
                if win {
                    best = Some(r);
                }
            }
            return best.map(|r| r.package.as_str());
        }
        // Fallback: legacy cache blob with no package_ranges.
        let scope = self.scope_at(point)?;
        let chain = self.scope_chain(scope);
        for scope_id in &chain {
            let s = &self.scopes[scope_id.0 as usize];
            if let Some(ref pkg) = s.package {
                return Some(pkg.as_str());
            }
        }
        None
    }

    /// Iterate Handler symbols whose owner class is `owner_class` and
    /// that dispatch through any of `dispatchers`. When `dispatchers`
    /// is empty, all handlers for that class match. Powers plugin
    /// `dispatch_targets_for` (rule #5: this extraction lives on the
    /// data model, not duplicated in symbols.rs).
    pub fn handlers_for_owner<'a>(
        &'a self,
        owner_class: &'a str,
        dispatchers: &'a [String],
    ) -> impl Iterator<Item = &'a Symbol> + 'a {
        self.symbols.iter().filter(move |sym| {
            let SymbolDetail::Handler { owner, dispatchers: dd, .. } = &sym.detail else {
                return false;
            };
            let HandlerOwner::Class(c) = owner;
            if c != owner_class { return false; }
            if !dispatchers.is_empty()
                && !dd.iter().any(|d| dispatchers.iter().any(|n| n == d))
            {
                return false;
            }
            true
        })
    }

    /// Trigger-matching view for plugin query hooks at `point`: the
    /// modules `use`d inside the enclosing package plus the transitive
    /// parent chain. Mirrors what the builder assembles at emit time so
    /// query hooks can be gated by the same `PluginRegistry::applicable`
    /// filter instead of running against every bundled plugin.
    pub fn trigger_view_at(&self, point: Point) -> (Vec<String>, Vec<String>) {
        let pkg = match self.package_at(point) {
            Some(p) => p.to_string(),
            None => return (Vec::new(), Vec::new()),
        };
        let uses = self.package_uses(&pkg).to_vec();
        // Local-only transitive parents: no index (emit-time never had
        // cross-file resolution) and the app-surface edge masked off —
        // a consumer is not a descendant of the synthetic surface for
        // trigger gating.
        let mut parents = Vec::new();
        let graph = crate::model::graph::GraphView::new(self, None);
        graph.walk(
            crate::model::graph::Node::Class(pkg),
            crate::model::graph::EdgeKindMask::INHERITS,
            &mut |n| {
                if let crate::model::graph::Node::Class(c) = n {
                    parents.push(c.clone());
                }
                crate::model::graph::WalkControl::Continue
            },
        );
        (uses, parents)
    }

    /// Resolve the class name for an invocant expression text (the token
    /// left of `->`). Handles the two Perl conventions `$self` and
    /// `__PACKAGE__` by falling back to the enclosing package; typed
    /// scalars go through `inferred_type`; barewords are treated as
    /// class names verbatim.
    ///
    /// This is Perl-semantic resolution, so it belongs on the data
    /// layer (rule #3). Callers in `symbols.rs` / `cursor_context.rs`
    /// compose this; they don't repeat the rules.
    pub fn invocant_text_to_class(&self, invocant: Option<&str>, point: Point) -> Option<String> {
        use crate::model::conventions::InvocantText;
        let text = invocant?;
        if crate::model::conventions::is_conventional_invocant_name(text) {
            return self.package_at(point).map(|s| s.to_string());
        }
        match InvocantText::parse(text) {
            InvocantText::CurrentPackage | InvocantText::PositionalReceiver => {
                self.package_at(point).map(|s| s.to_string())
            }
            // Use `InferredType::class_name()` so BOTH `ClassName` and
            // `FirstParam` resolve to their class — without this, a
            // `my ($c) = @_` invocant in a controller method (typed
            // `FirstParam { package: Users }`) falls back to None and
            // dispatch-target completion never fires for `$c->url_for(|)`.
            // Method completion on the same `$c` already uses this
            // accessor; routing through it here keeps the two paths in
            // sync. Rule #3.
            InvocantText::Scalar(_) => self
                .inferred_type_via_bag(text, point)
                .and_then(|t| t.class_name().map(str::to_string)),
            InvocantText::NonScalar(_) => None,
            InvocantText::Bareword(b) => Some(b.to_string()),
        }
    }

}
