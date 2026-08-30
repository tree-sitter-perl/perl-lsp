//! Hover rendering: every markdown hover surface — cursor hover,
//! member hover, handler hover, and the symbol-hover primitives —
//! over the resolution and type queries.

use super::*;

impl FileAnalysis {
    /// `format_inferred_type` through this file's language vocabulary
    /// (`PackFacts.type_display`): a mapped tag renders as the language's
    /// own spelling (php `array`, not `HashRef`); unmapped output (class
    /// names, parametrics) and every Perl analysis (empty map) pass
    /// through. THE type-label projection for human surfaces — hover,
    /// inlay hints, signatures, completion detail all route here so a
    /// language's vocabulary can't leak on one surface and not another.
    pub fn render_type(&self, ty: &InferredType) -> String {
        let raw = format_inferred_type(ty);
        self.translate_type_label(raw)
    }

    /// `Symbol::display_type` (class/deref-aware) through the same
    /// vocabulary.
    pub fn display_type_of(&self, sym: &Symbol, ty: &InferredType) -> String {
        self.translate_type_label(sym.display_type(ty))
    }

    fn translate_type_label(&self, raw: String) -> String {
        self.pack
            .type_display
            .iter()
            .find(|(k, _)| *k == raw)
            .map(|(_, v)| v.clone())
            .unwrap_or(raw)
    }

    /// Hover info: return display text for the symbol at cursor.
    pub fn hover_info(&self, point: Point, source: &str, module_index: Option<&dyn CrossFileLookup>) -> Option<String> {
        // Check refs first
        if let Some(r) = self.ref_at(point) {
            match &r.kind {
                RefKind::Variable | RefKind::ContainerAccess => {
                    // Check if this variable is also a dynamic method call target
                    // (e.g. $self->$method() where $method is a known constant).
                    // Gate on the METHOD-NAME token span, not the whole call span:
                    // a multi-line chain's MethodCall ref spans the entire
                    // expression, so `mr.span` contains the head invocant too —
                    // hovering `$schema` at the head of a chain would wrongly
                    // return the tail method's POD. The dynamic-dispatch method
                    // token IS this variable, so the point lands in
                    // `method_name_span` only for the genuine case.
                    let method_hover = self.refs.iter()
                        .find(|mr| matches!(&mr.kind, RefKind::MethodCall { method_name_span, .. }
                                if contains_point(method_name_span, point))
                            && mr.target_name != r.target_name);
                    if let Some(mr) = method_hover {
                        if matches!(mr.kind, RefKind::MethodCall { .. }) {
                            let class_name = self.method_call_invocant_class(mr, module_index);
                            let mname = mr.unqualified_target_name();
                            if let Some(ref cn) = class_name {
                                match self.resolve_method_in_ancestors(cn, mname, module_index) {
                                    Some(MethodResolution::Local { sym_id, class: ref defining_class, .. }) => {
                                        let sym = self.symbol(sym_id);
                                        let line = source_line_at(source, sym.selection_span.start.row);
                                        let class_label = if defining_class != cn {
                                            format!("{} (from {})", cn, defining_class)
                                        } else {
                                            cn.to_string()
                                        };
                                        let mut text = format!("```perl\n{}\n```\n\n*class {} — resolved from `{}`*", line.trim(), class_label, r.target_name);
                                        if let Some(ref rt) = self.find_method_return_type(cn, mname, module_index, None) {
                                            text.push_str(&format!("\n\n*returns: {}*", self.render_type(&rt)));
                                        }
                                        if let SymbolDetail::Sub { ref doc, .. } = sym.detail {
                                            if let Some(ref d) = doc {
                                                text.push_str(&format!("\n\n{}", d));
                                            }
                                        }
                                        return Some(text);
                                    }
                                    Some(MethodResolution::CrossFile { ref class, ref def_module }) => {
                                        if let Some(idx) = module_index {
                                            // Bridged helper lives in `def_module`; real
                                            // inherited method in `class`'s own module.
                                            // Either name maps to a SET of files — the
                                            // definer may be a losing candidate.
                                            let module = def_module.as_deref().unwrap_or(class.as_str());
                                            if let Some(cached) =
                                                idx.candidate_defining_sub_in_package(module, class, mname)
                                            {
                                                let whole = idx.bag_present(&cached);
                                                if let Some(sub_info) = whole.sub_info_view(mname) {
                                                    let sig = format_cross_file_signature(mname, &sub_info);
                                                    let mut text = format!("```perl\n{}\n```\n\n*class {} — resolved from `{}`*", sig, class, r.target_name);
                                                    if let Some(rt) = sub_info.return_type(Some(idx)) {
                                                        text.push_str(&format!("\n\n*returns: {}*", self.render_type(&rt)));
                                                    }
                                                    if let Some(doc) = sub_info.doc() {
                                                        text.push_str(&format!("\n\n{}", doc));
                                                    }
                                                    return Some(text);
                                                }
                                            }
                                        }
                                    }
                                    None => {}
                                }
                            }
                        }
                    }
                    if let Some(sym_id) = r.resolved_symbol() {
                        let sym = self.symbol(sym_id);
                        return Some(self.format_symbol_hover_at(sym, source, point, module_index));
                    }
                    // Unresolved variable — try resolve ourselves
                    if let Some(sym) = self.resolve_variable(&r.target_name, point) {
                        return Some(self.format_symbol_hover_at(sym, source, point, module_index));
                    }
                }
                RefKind::FunctionCall => {
                    // Package-scoped: hover shows the sub whose
                    // package matches what the ref resolved to. Qualified
                    // calls match on the bare tail (symbols are keyed by
                    // bare name); the `Function` binding pins the package.
                    if let Some(sid) = self
                        .package_scoped_callable(r.unqualified_target_name(), r.resolved_package())
                    {
                        return Some(self.format_symbol_hover(self.symbol(sid), source, module_index));
                    }
                    // Fall-through: the name might be a function imported
                    // from another module (either hand-written `use` or a
                    // plugin-synthesized Import like Mojolicious::Lite's
                    // route verbs). Cross-file lookup pulls real POD,
                    // real signature, real return type from the source
                    // module's cached analysis.
                    if let Some(idx) = module_index {
                        for import in &self.imports {
                            let matched = import.imported_symbols.iter()
                                .find(|s| s.local_name == r.target_name);
                            let Some(is) = matched else { continue };
                            // The exporting package may be split — pick the
                            // candidate file that defines the remote sub.
                            let Some(cached) =
                                idx.candidate_defining_sub(&import.module_name, is.remote())
                            else { continue };
                            let whole = idx.whole_present(&cached);
                            let Some(sub_info) = whole.sub_info_view(is.remote()) else { continue };

                            let sig_params = sub_info.params().iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            let mut sig = format!("sub {}({})", r.target_name, sig_params);
                            if let Some(rt) = sub_info.return_type(Some(idx)) {
                                sig.push_str(&format!(" → {}", self.render_type(&rt)));
                            }
                            let mut text = format!("```perl\n{}\n```", sig);
                            if let Some(doc) = sub_info.doc() {
                                text.push_str(&format!("\n\n{}", doc));
                            }
                            if is.remote() != r.target_name {
                                text.push_str(&format!(
                                    "\n\n*imported from `{}` (as `{}`)*",
                                    import.module_name, is.remote()
                                ));
                            } else {
                                text.push_str(&format!(
                                    "\n\n*imported from `{}`*",
                                    import.module_name
                                ));
                            }
                            return Some(text);
                        }
                    }
                }
                RefKind::MethodCall { .. } => {
                    // Single-source the invocant class off the frozen
                    // dispatch edge (NAV unification) — same edge find_def /
                    // refs_to read, so hover never diverges.
                    let class_name = r.method_target().map(|t| t.invocant_class().to_string());
                    // The bare method name (FQ `$o->Foo::Bar::m` resolves `m`).
                    let method = r.unqualified_target_name();
                    if let Some(ref cn) = class_name {
                        match self.resolve_method_in_ancestors(cn, method, module_index) {
                            Some(MethodResolution::Local { sym_id, class: ref defining_class, .. }) => {
                                let sym = self.symbol(sym_id);
                                let line = source_line_at(source, sym.selection_span.start.row);
                                let class_label = if defining_class != cn {
                                    format!("{} (from {})", cn, defining_class)
                                } else {
                                    cn.to_string()
                                };
                                let mut text = format!("```perl\n{}\n```\n\n*class {}*", line.trim(), class_label);
                                if let Some(ref rt) = self.find_method_return_type(cn, method, module_index, None) {
                                    text.push_str(&format!("\n\n*returns: {}*", self.render_type(&rt)));
                                }
                                return Some(text);
                            }
                            Some(MethodResolution::CrossFile { ref class, ref def_module }) => {
                                if let Some(idx) = module_index {
                                    // Bridged helper lives in `def_module`; real
                                    // inherited method in `class`'s own module.
                                    // Either name maps to a SET of files — the
                                    // definer may be a losing candidate.
                                    let module = def_module.as_deref().unwrap_or(class.as_str());
                                    if let Some(cached) =
                                        idx.candidate_defining_sub_in_package(module, class, method)
                                    {
                                        let whole = idx.bag_present(&cached);
                                        if let Some(sub_info) = whole.sub_info_view(method) {
                                            let class_label = if class != cn {
                                                format!("{} (from {})", cn, class)
                                            } else {
                                                cn.to_string()
                                            };
                                            let sig = format_cross_file_signature(method, &sub_info);
                                            let mut text = format!("```perl\n{}\n```\n\n*class {}*", sig, class_label);
                                            if let Some(rt) = sub_info.return_type(Some(idx)) {
                                                text.push_str(&format!("\n\n*returns: {}*", self.render_type(&rt)));
                                            }
                                            if let Some(doc) = sub_info.doc() {
                                                text.push_str(&format!("\n\n{}", doc));
                                            }
                                            return Some(text);
                                        }
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                    // Fallback
                    for &sid in self.symbols_named(method) {
                        let sym = self.symbol(sid);
                        if matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                            return Some(self.format_symbol_hover(sym, source, module_index));
                        }
                    }
                }
                RefKind::PackageRef => {
                    for &sid in self.symbols_named(&r.target_name) {
                        let sym = self.symbol(sid);
                        if matches!(sym.kind, SymKind::Package | SymKind::Class) {
                            return Some(self.format_symbol_hover(sym, source, module_index));
                        }
                    }
                }
                RefKind::HashKeyAccess { .. } => {
                    if let Some(owner) = r.hash_key_owner() {
                        let defs = self.hash_key_defs_for_owner(owner);
                        let matching: Vec<_> = defs.iter()
                            .filter(|d| d.name == r.target_name)
                            .collect();
                        if !matching.is_empty() {
                            let lines: Vec<String> = matching.iter()
                                .map(|d| {
                                    let line = source_line_at(source, d.span.start.row);
                                    format!("- `{}`", line.trim())
                                })
                                .collect();
                            return Some(format!("**Hash key `{}`**\n\n{}", r.target_name, lines.join("\n")));
                        }
                    }
                }
                RefKind::DispatchCall { dispatcher } => {
                    if let Some(owner) = r.handler_owner() {
                        return Some(self.format_handler_hover(
                            &r.target_name,
                            owner,
                            Some(dispatcher),
                            module_index,
                        ));
                    }
                }
            }
        }

        // Check symbols
        if let Some(sym) = self.symbol_at(point) {
            // Handler symbols get a specialized multi-registration hover
            // (stacked defs, dispatcher list, param shapes).
            if let SymbolDetail::Handler { owner, .. } = &sym.detail {
                return Some(self.format_handler_hover(&sym.name, owner, None, module_index));
            }
            return Some(self.format_symbol_hover(sym, source, module_index));
        }

        None
    }

    /// The DEFINITION site of data member `field` on `class` (or an
    /// ancestor): the field symbol's file + selection span. `None` path = the
    /// field lives in THIS analysis (current file); `Some(path)` = a
    /// cross-file class. Drives goto-def on `obj->field`. Same cross-file
    /// ancestor walk as member completion.
    /// `field: type` for hover on `obj->field`, resolved through the SAME
    /// `resolve_method_in_ancestors` walk goto-def uses — no parallel walk.
    /// Type read from the field's OWNING analysis; rendered via the one
    /// `display_type` projection.
    pub fn member_hover(
        &self,
        class: &str,
        field: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        let render = |analysis: &FileAnalysis, sym: &Symbol| {
            let base = match analysis.inferred_type_via_bag_ctx(field, sym.span.end, module_index)
            {
                Some(ty) => format!("{}: {}", field, self.display_type_of(sym, &ty)),
                None => field.to_string(),
            };
            // A union member shares storage with its siblings — surface the
            // overlay so the reader sees what else lives in those bytes.
            match analysis.union_overlay(sym) {
                Some(sibs) if !sibs.is_empty() => {
                    format!("{} — union member (overlays {})", base, sibs.join(", "))
                }
                _ => base,
            }
        };
        match self.resolve_method_in_ancestors(class, field, module_index)? {
            MethodResolution::Local { sym_id, .. } => Some(render(self, self.symbol(sym_id))),
            MethodResolution::CrossFile { class, .. } => {
                let idx = module_index?;
                // The declaring file is whichever of `class`'s candidates
                // holds the field, not the name-slot winner. `render` reads
                // the field's flow type from its OWNING bag — the symbol
                // scan needs symbols too; take the whole view.
                idx.visible_def_candidates(&class).iter().find_map(|cached| {
                    let full = idx.whole_present(cached);
                    let sym = full.symbols.iter().find(|s| {
                        matches!(s.kind, SymKind::Variable | SymKind::Field)
                            && s.name == field
                            && s.package.as_deref() == Some(class.as_str())
                            && full.symbol_is_class_content(s)
                    })?;
                    Some(render(&full, sym))
                })
            }
        }
    }

    /// Hover text for a `Handler` symbol or `DispatchCall` ref. Shows
    /// every stacked registration with its param shape, lists the
    /// dispatcher methods that route to it, and names the owning class.
    /// Walks the module index too so consumer-file hovers cross-file
    /// back to the producer's registrations — critical for the common
    /// case where events are defined in a lib and emitted from scripts.
    pub(super) fn format_handler_hover(
        &self,
        name: &str,
        owner: &HandlerOwner,
        active_dispatcher: Option<&str>,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> String {
        let class = match owner {
            HandlerOwner::Class(n) => n.as_str(),
        };

        // Gather stacked registrations from this file first, then any
        // additional ones in the workspace/dependency cache. Each entry
        // is `(line_number, display_params)` rather than a `&Symbol`
        // reference so cross-file handlers (owned by other
        // FileAnalyses) flow through the same formatting path.
        let mut registrations: Vec<(usize, Vec<String>)> = self.symbols.iter()
            .filter(|s| s.name == name)
            .filter_map(|s| match &s.detail {
                SymbolDetail::Handler { owner: o, params, .. } if o == owner => {
                    Some((s.selection_span.start.row + 1, display_handler_params(params)))
                }
                _ => None,
            })
            .collect();

        // Cross-file walk — now O(matches) instead of O(workspace)
        // via the name-based reverse index on ModuleIndex. Only modules
        // that have a symbol with this name are visited; most of the
        // workspace is skipped without any per-module inspection.
        if let Some(idx) = module_index {
            for module_name in idx.modules_with_symbol(name) {
                // Every file registered under the name — a registration
                // gathering must not stop at the name-slot winner.
                for cached in idx.visible_def_candidates(&module_name) {
                    let whole = idx.whole_present(&cached);
                    for sym in &whole.symbols {
                        if sym.name != name { continue; }
                        if let SymbolDetail::Handler { owner: o, params, .. } = &sym.detail {
                            if o == owner {
                                registrations.push((
                                    sym.selection_span.start.row + 1,
                                    display_handler_params(params),
                                ));
                            }
                        }
                    }
                }
            }
        }
        registrations.sort();
        registrations.dedup();

        // Dispatchers: union across stacked registrations, current-file only
        // (plugins declare them consistently, no need to walk deps).
        let registrations_ref: Vec<&Symbol> = self.symbols.iter()
            .filter(|s| s.name == name)
            .filter(|s| matches!(
                &s.detail,
                SymbolDetail::Handler { owner: o, .. } if o == owner
            ))
            .collect();

        // Union dispatcher lists across the current-file registrations.
        let mut dispatchers: Vec<String> = registrations_ref.iter()
            .filter_map(|s| match &s.detail {
                SymbolDetail::Handler { dispatchers, .. } => Some(dispatchers.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        // If we have only cross-file registrations, pull dispatchers from
        // the module index cache too so the hover still shows the
        // dispatcher list to the consumer.
        if dispatchers.is_empty() {
            if let Some(idx) = module_index {
                for module_name in idx.modules_with_symbol(name) {
                    for cached in idx.visible_def_candidates(&module_name) {
                        let whole = idx.whole_present(&cached);
                        for sym in &whole.symbols {
                            if sym.name != name { continue; }
                            if let SymbolDetail::Handler { owner: o, dispatchers: ds, .. } = &sym.detail {
                                if o == owner { dispatchers.extend(ds.clone()); }
                            }
                        }
                    }
                }
            }
        }
        dispatchers.sort();
        dispatchers.dedup();

        let mut text = String::new();
        text.push_str(&format!("**handler `{}`** on `{}`\n\n", name, class));

        if registrations.is_empty() {
            text.push_str("*no handler registered in this workspace — dispatch will be a no-op*");
            return text;
        }

        let plural = if registrations.len() == 1 { "" } else { "s" };
        text.push_str(&format!(
            "*{} registration{} stack{}:*\n\n",
            registrations.len(),
            plural,
            if registrations.len() == 1 { "s" } else { "" },
        ));

        for (line, display) in &registrations {
            text.push_str(&format!(
                "- **line {}:** `({})`\n",
                line,
                display.join(", "),
            ));
        }

        if !dispatchers.is_empty() {
            text.push_str(&format!(
                "\n*Dispatch via:* `{}`",
                dispatchers.iter()
                    .map(|d| {
                        if Some(d.as_str()) == active_dispatcher {
                            format!("**->{}(...)**", d)
                        } else {
                            format!("->{}(...)", d)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        text
    }

    pub(super) fn format_symbol_hover(
        &self,
        sym: &Symbol,
        source: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> String {
        self.format_symbol_hover_at(sym, source, sym.selection_span.end, module_index)
    }

    pub(super) fn format_symbol_hover_at(
        &self,
        sym: &Symbol,
        source: &str,
        at: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> String {
        let line = source_line_at(source, sym.span.start.row);
        let mut text = format!("```perl\n{}\n```", line.trim());

        // Append inferred type for variables/fields (bag-routed so
        // framework / branch / arity rules refine the answer). The index lets
        // a role-contract param type (`$c` in a Catalyst action) resolve
        // through cross-file ancestry via the `ReceiverGated` gate.
        if matches!(sym.kind, SymKind::Variable | SymKind::Field) {
            if let Some(it) = self.inferred_type_via_bag_ctx(&sym.name, at, module_index) {
                text.push_str(&format!("\n\n*type: {}*", self.render_type(&it)));
            }
        }

        // Append return type + preceding/POD doc for subs/methods.
        if matches!(sym.kind, SymKind::Sub | SymKind::Method) {
            // Disambiguate `Foo::run` vs `Bar::run` by showing the
            // owning package. Without this, two same-named subs in
            // different packages both render identical hover text
            // and the user can't tell which one the LSP resolved to.
            if let Some(pkg) = sym.package.as_deref() {
                text.push_str(&format!("\n\n*package {}*", pkg));
            }
            if let SymbolDetail::Sub { ref doc, .. } = sym.detail {
                if let Some(rt) = self.symbol_return_type_via_bag(sym.id, None) {
                    text.push_str(&format!("\n\n*returns: {}*", self.render_type(&rt)));
                }
                if let Some(d) = doc {
                    text.push_str(&format!("\n\n{}", d));
                }
            }
        }

        text
    }

}
