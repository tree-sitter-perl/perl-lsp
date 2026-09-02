//! Cursor-anchored queries: definition/references/highlights,
//! rename and field-group projection machinery.

use super::*;

impl FileAnalysis {
    // ---- Cursor lookup methods ----

    /// Find the ref at a given point (cursor position).
    pub fn ref_at(&self, point: Point) -> Option<&Ref> {
        self.refs.iter()
            .filter(|r| contains_point(&r.span, point))
            .min_by_key(|r| span_size(&r.span))
    }

    /// Find the symbol whose selection_span contains the point.
    pub fn symbol_at(&self, point: Point) -> Option<&Symbol> {
        self.symbols.iter().find(|s| contains_point(&s.selection_span, point))
    }

    /// The innermost callable (Sub/Method) whose body span contains the
    /// point — the "who is calling from here" question the call-hierarchy
    /// incoming projection groups reference sites by. `None` for top-level
    /// code (import-time calls, scripts): there is no callable to report.
    pub fn enclosing_callable_at(&self, point: Point) -> Option<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| {
                matches!(s.kind, SymKind::Sub | SymKind::Method) && contains_point(&s.span, point)
            })
            .min_by_key(|s| span_size(&s.span))
    }

    /// The build-time-resolved owner of the hash key under the cursor —
    /// from the access ref (`$cfg->{key}`) or the key's def symbol (Moo
    /// `has`, return-shape keys). `None` = the key is lexical/unowned and
    /// cross-file queries have nothing to pin on.
    pub fn hash_key_owner_at(&self, point: Point) -> Option<HashKeyOwner> {
        if let Some(o) = self.ref_at(point).and_then(|r| r.hash_key_owner()) {
            return Some(o.clone());
        }
        match self.symbol_at(point).map(|s| &s.detail) {
            Some(SymbolDetail::HashKeyDef { owner, .. }) => Some(owner.clone()),
            _ => None,
        }
    }

    // ---- High-level queries ----

    /// Go-to-definition: resolve the symbol at cursor to its definition span.
    pub fn find_definition(&self, point: Point, _module_index: Option<&dyn CrossFileLookup>) -> Option<Span> {
        // 1. Check if cursor is on a ref
        if let Some(r) = self.ref_at(point) {
            match &r.kind {
                RefKind::Variable => {
                    if let Some(sym_id) = r.resolved_symbol() {
                        return Some(self.symbol(sym_id).selection_span);
                    }
                }
                RefKind::FunctionCall => {
                    // Package-scoped: pick the sub whose package
                    // matches the ref's resolved_package. When the
                    // call pinned a specific package (import or
                    // local-package match), we MUST NOT jump to a
                    // same-named sub in a different package — that's
                    // the cross-class collision we're specifically
                    // guarding against. Qualified calls (`Foo::baz()`)
                    // carry the full path in `target_name` but symbols are
                    // keyed by bare name — match on the unqualified tail and
                    // pin via `resolved_package` (the qualifier).
                    if let Some(sid) = self
                        .package_scoped_callable(r.unqualified_target_name(), r.resolved_package())
                    {
                        return Some(self.symbol(sid).selection_span);
                    }
                    // Nothing local; leave cross-file resolution to
                    // the LSP adapter (symbols::find_definition).
                }
                RefKind::MethodCall { .. } => {

                    // Method dispatch is the frozen edge, full stop.
                    // `Local` lands on the local symbol; `CrossFile`
                    // returns None so the LSP adapter resolves via the
                    // ModuleIndex; a `None` edge (invocant didn't infer —
                    // genuinely untyped receiver, e.g. `my $x = external();
                    // $x->m`) returns None: honest miss. There is NO
                    // same-name fallback — a typed OR chained-method-return
                    // receiver now carries a real edge
                    // (`method_call_invocant_class` resolves chain
                    // receivers via `expr_type_at_span`), so jumping to an
                    // arbitrary same-named sub when the class can't infer is
                    // never right (the `->new` / `'Users#create'` flood, the
                    // libwww untyped-receiver case).
                    match r.method_target() {
                        Some(MethodTarget::Local { sym_id, .. }) => {
                            return Some(self.symbol(*sym_id).selection_span);
                        }
                        Some(MethodTarget::CrossFile { .. }) | None => {
                            return None;
                        }
                    }
                }
                RefKind::PackageRef => {
                    // Type space first; on a miss, value space. A pack
                    // grammar's TYPE guess in a type/value-ambiguous slot
                    // (a template argument `MakeError<StatusCode::kNotFound>`,
                    // `Buffer<MAX>`) mints a PackageRef for a VALUE token —
                    // the structural gates are pack-only shapes, so Perl
                    // package refs never take the fallback.
                    let row_ns = self.import_row_namespace(&r.span);
                    return self.find_package_or_class_in(&r.target_name, row_ns.as_deref()).or_else(|| {
                        self.symbols_named(&r.target_name)
                            .iter()
                            .map(|&sid| self.symbol(sid))
                            .find(|s| {
                                self.symbol_is_class_content(s)
                                    || self.symbol_is_file_scope_value(s)
                            })
                            .map(|s| s.selection_span)
                    });
                }
                RefKind::HashKeyAccess { .. } => {
                    // Try the pre-resolved owner first
                    if let Some(owner) = r.hash_key_owner() {
                        for def in self.hash_key_defs_for_owner(owner) {
                            if def.name == r.target_name {
                                return Some(def.selection_span);
                            }
                        }
                    }
                }
                RefKind::ContainerAccess => {
                    return self.resolve_variable(&r.target_name, point)
                        .map(|sym| sym.selection_span);
                }
                RefKind::DispatchCall { .. } if r.handler_owner().is_some() => {
                    let owner = r.handler_owner().unwrap();
                    // Go-to-def on a dispatch call site lands at the
                    // first stacked Handler for this (owner, name).
                    // Features that want all registrations walk
                    // `refs_to_symbol` or use `refs_to` with
                    // `TargetKind::Handler`.
                    for sym in &self.symbols {
                        if sym.name != r.target_name { continue; }
                        if let SymbolDetail::Handler { owner: o, .. } = &sym.detail {
                            if o == owner {
                                return Some(sym.selection_span);
                            }
                        }
                    }
                    // No LOCAL Handler — the registration is cross-file.
                    // Return None (terminal) so the LSP adapter's cross-file
                    // DispatchCall resolver runs. Falling through to the
                    // `symbol_at` fallback below would wrongly grab whatever
                    // symbol overlaps the call-arg string (e.g. a synthesized
                    // hash-key def at the same span).
                    return None;
                }
                RefKind::DispatchCall { .. } => {}
            }
        }

        // 2. Check if cursor is on a symbol declaration
        if let Some(sym) = self.symbol_at(point) {
            return Some(sym.selection_span);
        }

        None
    }

    /// Find all references to the symbol at cursor (span projection of
    /// `find_occurrences`).
    pub fn find_references(
        &self,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<Span> {
        self.find_occurrences(point, module_index)
            .into_iter()
            .map(|(span, _)| span)
            .collect()
    }

    /// THE in-file occurrence union: every same-identity site in this file,
    /// with its access classification. The single spelling behind the whole
    /// in-file family — `find_references` projects the spans, `rename_at`
    /// turns them into edits, and the CandidateSet's `references()`/
    /// `highlights()` Local arm reads it directly, so the verbs cannot
    /// drift on what "the occurrences of this cursor" means.
    pub fn find_occurrences(
        &self,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(Span, AccessKind)> {
        // A field group's spellings reference each other — from any of
        // them, surface all of them (the same union rename rewrites).
        // Group spans are bare-name tokens with no recorded access shape.
        if let Some(g) = self.field_group_at(point) {
            return self
                .field_group_spans(&g)
                .into_iter()
                .map(|s| (s, AccessKind::Read))
                .collect();
        }
        // A lexical hash key (`my %opts = (k => …); $opts{k}`): every same-key
        // access on the same `my %h` — keyed by the variable's `def_scope`, so a
        // shadowing `%h` in an inner block stays its own set — is one renameable
        // unit, single-file (no owner reaches another file).
        if let Some(pairs) = self.lexical_hash_key_refs(point) {
            return pairs;
        }
        if let Some((target_id, include_decl)) = self.resolve_target_at(point, module_index) {
            let mut results = self.collect_refs_for_target(target_id, include_decl, module_index);
            results.sort_by_key(|(s, _)| (s.start.row, s.start.column));
            results.dedup_by(|a, b| a.0.start == b.0.start && a.0.end == b.0.end);
            results
        } else {
            Vec::new()
        }
    }

    /// Occurrences of a lexical hash key under the cursor — every `$h{key}`
    /// access (with its read/write access) on the same `my %h`, plus the
    /// literal key in the declaration.
    /// `None` when the cursor isn't on a `Variable`-owned hash key.
    /// Matched on the owner's `def_scope` (the `%h` declaration) so an unrelated
    /// or shadowing same-named hash never bleeds in. Single-file by nature — a
    /// `my` lexical is unreachable from another file.
    fn lexical_hash_key_refs(&self, point: Point) -> Option<Vec<(Span, AccessKind)>> {
        let r = self.ref_at(point)?;
        if !matches!(r.kind, RefKind::HashKeyAccess { .. }) {
            return None;
        }
        let HashKeyOwner::Variable { name: var, def_scope } = self.hash_key_owner_at(point)? else {
            return None;
        };
        // The owner identifies the variable by (bare name, def_scope): the
        // sigil-stripped name distinguishes two hashes declared in the SAME
        // scope (`my %a` vs `my %b`), and `def_scope` distinguishes a shadowing
        // `%h` in an inner block. Key + that pair pins exactly this hash's key.
        let bare = |n: &str| n.trim_start_matches(['$', '@', '%']).to_string();
        let want = bare(&var);
        let key = r.target_name.as_str();
        let mut pairs: Vec<(Span, AccessKind)> = self
            .refs
            .iter()
            .filter(|o| {
                o.target_name == key
                    && matches!(o.kind, RefKind::HashKeyAccess { .. })
                    && matches!(
                        o.hash_key_owner(),
                        Some(HashKeyOwner::Variable { name: on, def_scope: ds })
                            if *ds == def_scope && bare(on) == want
                    )
            })
            .map(|o| (o.span, o.access))
            .collect();
        pairs.sort_by_key(|(s, _)| (s.start.row, s.start.column));
        pairs.dedup_by(|a, b| a.0 == b.0);
        Some(pairs)
    }

    /// Shared implementation behind `find_occurrences`.
    fn collect_refs_for_target(
        &self,
        target_id: SymbolId,
        include_decl: bool,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(Span, AccessKind)> {
        let sym = self.symbol(target_id);
        let mut results: Vec<(Span, AccessKind)> = Vec::new();

        // Include the declaration itself
        if include_decl {
            results.push((sym.selection_span, AccessKind::Declaration));
        }

        // O(1) lookup for every ref resolved to this symbol.
        // This covers variables, HashKeyAccess refs whose owner was resolved at
        // build time, and any future kinds that bind a resolved symbol.
        for &idx in self.refs_to_symbol(target_id) {
            let r = &self.refs[idx];
            results.push((r.span, r.access));
        }

        // For subs/methods/packages/classes, also find refs by name.
        // Scope-filter callable refs (Sub / Method) by the target
        // symbol's package — matches `resolve::refs_to` so
        // documentHighlight and references agree on "same callable".
        // Without this, cursor on one `create` highlights every other
        // same-named method/sub in the file regardless of class
        // (mojo-helper leaf on `_Helper::users` cross-highlighting
        // the route's `Users::create` ref, etc.).
        if matches!(sym.kind, SymKind::Sub | SymKind::Method | SymKind::Package | SymKind::Class | SymKind::Module) {
            let sym_package = sym.package.clone();
            for r in self.refs() {
                if r.resolved_symbol().is_some() { continue; }
                match (&r.kind, &sym.kind) {
                        (RefKind::FunctionCall, SymKind::Sub) => {
                            // Match the bare callable name so qualified call
                            // sites (`Foo::baz()`, target_name "Foo::baz")
                            // pair with `sub baz`; `resolved_package` (the
                            // qualifier) still isolates same-named subs
                            // across packages.
                            if r.unqualified_target_name() == sym.name
                                && r.resolved_package() == sym_package.as_deref() {
                                results.push((r.span, r.access));
                            }
                        }
                        (RefKind::MethodCall { method_name_span, .. },
                         SymKind::Sub | SymKind::Method) if r.unqualified_target_name() == sym.name => {
                            // Same-class match only; unresolved or
                            // different-class invocants are excluded.
                            // Method-call ref.span covers the whole
                            // `$obj->foo(...)` expression so we use
                            // `method_name_span` to highlight just
                            // the identifier.
                            match (self.method_call_invocant_class(r, module_index), &sym_package) {
                                (Some(cn), Some(pkg)) if cn == *pkg => {
                                    results.push((*method_name_span, r.access));
                                }
                                _ => {}
                            }
                        }
                        (RefKind::PackageRef, SymKind::Package | SymKind::Class | SymKind::Module)
                            if r.target_name == sym.name =>
                            results.push((r.span, r.access)),
                        _ => {}
                }
            }
        }

        // Member-access refs (cpp `o->field` / `o->method()`) dispatch through
        // the frozen `Method` binding, not a resolved-symbol binding. Any
        // MethodCall whose edge landed on this symbol — via the SAME
        // ancestor walk goto-def uses — references it, whether the member is a
        // method or a data field (`Variable`/`Field`). Inheritance-aware for
        // free: every `->op_type` across every struct pasting a role macro
        // froze onto the one `BASEOP::op_type` member, so they splat together.
        for r in self.refs() {
            if let (
                RefKind::MethodCall { method_name_span, .. },
                Some(MethodTarget::Local { sym_id, .. }),
            ) = (&r.kind, r.method_target())
            {
                if *sym_id == target_id {
                    results.push((*method_name_span, r.access));
                }
            }
        }

        // For hash key definitions, find all accesses with same owner + key name
        if let SymbolDetail::HashKeyDef { ref owner, .. } = sym.detail {
            for r in self.refs() {
                if matches!(r.kind, RefKind::HashKeyAccess { .. }) {
                    if r.target_name != sym.name {
                        continue;
                    }
                    let matches = match r.hash_key_owner() {
                        Some(ro) => owner.found_by(ro),
                        None => false,
                    };
                    if matches {
                        results.push((r.span, r.access));
                    }
                }
            }
        }

        results
    }

    /// Rename: return all spans + new text for renaming the symbol at cursor.
    pub fn rename_at(&self, point: Point, new_name: &str) -> Option<Vec<(Span, String)>> {
        // A Corinna field and its projections (`:param` constructor key,
        // `:reader` calls) are ONE renameable entity — rename from any
        // spelling rewrites all of them.
        if let Some(group) = self.field_group_at(point) {
            return Some(self.rename_field_group(&group, new_name));
        }
        let refs = self.find_references(point, None);
        if refs.is_empty() {
            return None;
        }

        // Determine if this is a variable (sigil handling needed)
        let is_variable = self.ref_at(point)
            .map(|r| matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess))
            .or_else(|| self.symbol_at(point).map(|s| matches!(s.kind, SymKind::Variable | SymKind::Field)))
            .unwrap_or(false);

        let edits: Vec<(Span, String)> = if is_variable {
            // Strip sigil from new_name if present
            let bare_name = if new_name.starts_with('$') || new_name.starts_with('@') || new_name.starts_with('%') {
                &new_name[1..]
            } else {
                new_name
            };
            refs.into_iter().map(|span| {
                // Each ref span includes the sigil; replace only the name part (after sigil)
                let name_span = Span {
                    start: Point::new(span.start.row, span.start.column + 1),
                    end: span.end,
                };
                (name_span, bare_name.to_string())
            }).collect()
        } else {
            refs.into_iter().map(|span| (span, new_name.to_string())).collect()
        };

        Some(edits)
    }

    /// The renameable entity behind a Corinna field: `field $x :param
    /// :reader` is ONE name spelled three ways — the field variable, the
    /// constructor key (`Point->new(x => …)`), and the reader method
    /// (`$p->x`). The field is the source; the rest are projections
    /// (rule #9), so rename rewrites them together.
    fn field_group_at(&self, point: Point) -> Option<FieldGroup> {
        // Cursor on a `has`-decl token: any of the stacked synthesized
        // symbols (accessor Method / ctor HashKeyDef) selects the pair.
        if let Some(s) = self.symbol_at(point) {
            if matches!(s.kind, SymKind::Method | SymKind::HashKeyDef) {
                if let Some(pkg) = s.package.as_deref() {
                    if let Some(g) = self.attr_pair_group(&s.name, pkg) {
                        // Only when the cursor is ON the decl token itself.
                        if contains_point(&g.decl_span.unwrap(), point) {
                            return Some(g);
                        }
                    }
                }
            }
        }
        // Cursor on the field decl, or on a field-variable use in a body.
        let field_sym = self
            .symbol_at(point)
            .filter(|s| matches!(s.kind, SymKind::Field))
            .or_else(|| {
                self.ref_at(point).and_then(|r| {
                    if !matches!(r.kind, RefKind::Variable) {
                        return None;
                    }
                    r.resolved_symbol()
                        .map(|id| self.symbol(id))
                        .filter(|s| matches!(s.kind, SymKind::Field))
                })
            });
        if let Some(sym) = field_sym {
            return self.field_group_of(sym);
        }
        // Cursor on a reader call: `$p->x` where the dispatch class has
        // `field $x :reader` in THIS file.
        if let Some(r) = self.ref_at(point) {
            if matches!(r.kind, RefKind::MethodCall { .. }) {
                let bare = r.unqualified_target_name();
                let cls = r
                    .method_target()
                    .map(|t| t.invocant_class().to_string())
                    .or_else(|| self.method_call_invocant_class(r, None));
                if let Some(class) = cls {
                    let field_name = format!("${}", bare);
                    if let Some(sym) = self.symbols.iter().find(|s| {
                        matches!(s.kind, SymKind::Field)
                            && s.name == field_name
                            && s.package.as_deref() == Some(class.as_str())
                    }) {
                        if let Some(g) = self.field_group_of(sym) {
                            if g.has_reader {
                                return Some(g);
                            }
                        }
                    }
                    // Moo accessor call → its attr pair (only when the
                    // pair really has an accessor — a key-only pair must
                    // not claim a method-call cursor).
                    if let Some(g) = self.attr_pair_group(bare, &class) {
                        if g.has_reader {
                            return Some(g);
                        }
                    }
                    // Name-mapped accessor call (`$w->has_size`, `clear_size`)
                    // → the attr it projects from. Symmetric with renaming the
                    // attr, which re-derives these names.
                    if let Some(attr) = self.attr_for_mapped_accessor(bare, &class) {
                        if let Some(g) = self.attr_group_for(&attr, &class) {
                            return Some(g);
                        }
                    }
                }
                return None;
            }
        }
        // Cursor on a constructor key (access at a call site, or the
        // synthesized def) owned by `Sub { class, new }`.
        let (key, owner) = match self
            .ref_at(point)
            .map(|r| (r, r.hash_key_owner()))
        {
            Some((r, Some(o))) if matches!(r.kind, RefKind::HashKeyAccess { .. }) => {
                (r.target_name.clone(), o.clone())
            }
            _ => match self.symbol_at(point) {
                Some(s) if matches!(s.kind, SymKind::HashKeyDef) => match &s.detail {
                    SymbolDetail::HashKeyDef { owner, .. } => (s.name.clone(), owner.clone()),
                    _ => return None,
                },
                _ => return None,
            },
        };
        // The owner names the class: a constructor key (`Sub{class,new}` —
        // `Foo->new(size => …)`), a bridged condition-arg (`Bridged` — DBIC
        // `search({col})`), or an internal slot (`Class` — `$self->{size}`).
        // A `Class`-owner deref reaches the attr ONLY if it's a real internal
        // slot (Moo/bless `InternalKey`); a bridged column isn't a hash slot, so
        // `$row->{col}` (a `Class` lookup) resolves to nothing here.
        let class = match &owner {
            HashKeyOwner::Sub { package: Some(c), name }
                if crate::model::conventions::is_constructor_name(name) =>
            {
                c.clone()
            }
            HashKeyOwner::Bridged { class: c } => c.clone(),
            HashKeyOwner::Class(c) => {
                let has_internal = self.attr_projections.iter().any(|a| {
                    a.class == *c
                        && a.attr == key
                        && matches!(a.kind, AttrProjectionKind::InternalKey)
                });
                if !has_internal {
                    return None;
                }
                c.clone()
            }
            _ => return None,
        };
        self.attr_group_for(&key, &class)
    }

    /// The attr a name-mapped accessor projects from: `has_size` /
    /// `clear_size` → `size`. Moo's `predicate`/`clearer`/`writer`/… mint an
    /// `Accessor` projection whose `method` is the derived name; the reverse
    /// lookup lets a cursor on that method reach the one attr group (rule #9:
    /// every projection traces back to its source). The bare reader
    /// (`method == attr`) is the identity case the caller already handles.
    fn attr_for_mapped_accessor(&self, method: &str, class: &str) -> Option<String> {
        self.attr_projections.iter().find_map(|a| match &a.kind {
            // Only a name that EMBEDS the attr (`has_size` for `size`, affix
            // `Some`) reverse-maps to the attr group — renaming the attr
            // re-derives it. A custom name that does NOT embed the attr
            // (`predicate => 'is_ready'` for `size`, affix `None`) is an
            // independent method: a cursor on it must rename IT, not the attr
            // (else the click renames a different token), so it doesn't map back.
            AttrProjectionKind::Accessor { method: m, affix: Some(_) }
                if a.class == class && m == method && a.attr != method =>
            {
                Some(a.attr.clone())
            }
            _ => None,
        })
    }

    /// The attr group for `attr` on `class`, whether it's a Corinna field
    /// (variable-backed) or a Moo/`has`-style pair. One entry point so every
    /// spelling (decl, ctor key, internal slot, reader, mapped accessor)
    /// resolves to the same group.
    fn attr_group_for(&self, attr: &str, class: &str) -> Option<FieldGroup> {
        let field_name = format!("${}", attr);
        if let Some(sym) = self.symbols.iter().find(|s| {
            matches!(s.kind, SymKind::Field)
                && s.name == field_name
                && s.package.as_deref() == Some(class)
        }) {
            return self.field_group_of(sym);
        }
        self.attr_pair_group(attr, class)
    }

    /// A `has`-synthesized attr pair: accessor Method + constructor
    /// HashKeyDef with the same name, package, and selection span (they
    /// were minted from the one `has 'name'` token — span equality is
    /// what distinguishes the pair from a real `sub name` that happens
    /// to share a class with someone's ctor key).
    fn attr_pair_group(&self, bare: &str, class: &str) -> Option<FieldGroup> {
        // (1) Constructor-key pairing (Moo/Mojo `has`): the ctor-key HashKeyDef
        // is the anchor; the accessor (if any) shares its selection span.
        if let Some(key_def) = self.symbols.iter().find(|s| {
            matches!(s.kind, SymKind::HashKeyDef)
                && s.name == bare
                && matches!(
                    &s.detail,
                    SymbolDetail::HashKeyDef {
                        owner: HashKeyOwner::Sub { package: Some(p), name },
                        ..
                    } if p == class && crate::model::conventions::is_constructor_name(name)
                )
        }) {
            let accessor = self.symbols.iter().find(|s| {
                matches!(s.kind, SymKind::Method)
                    && s.name == bare
                    && s.package.as_deref() == Some(class)
                    && s.selection_span == key_def.selection_span
            });
            return Some(FieldGroup {
                field_sym: None,
                decl_span: Some(key_def.selection_span),
                class: class.to_string(),
                bare: bare.to_string(),
                has_param: true,
                has_reader: accessor.is_some(),
            });
        }
        // (2) Class-key pairing (DBIC `add_columns`, Class::Accessor): an
        // accessor Method and a `Class`-owned HashKeyDef of the same name
        // minted from the SAME token (span equality is the synthesized-pair
        // signal). The key side is reached via the `has_class_key` member;
        // here we just confirm the pair exists and pin the decl span.
        let accessor = self.symbols.iter().find(|s| {
            matches!(s.kind, SymKind::Method)
                && s.name == bare
                && s.package.as_deref() == Some(class)
        })?;
        let paired = self.symbols.iter().any(|s| {
            matches!(s.kind, SymKind::HashKeyDef)
                && s.name == bare
                && s.selection_span == accessor.selection_span
                && matches!(
                    &s.detail,
                    SymbolDetail::HashKeyDef { owner: HashKeyOwner::Bridged { class: c }, .. } if c == class
                )
        });
        if !paired {
            return None;
        }
        Some(FieldGroup {
            field_sym: None,
            decl_span: Some(accessor.selection_span),
            class: class.to_string(),
            bare: bare.to_string(),
            has_param: false,
            has_reader: true,
        })
    }

    fn field_group_of(&self, sym: &Symbol) -> Option<FieldGroup> {
        let SymbolDetail::Field { ref attributes, .. } = sym.detail else {
            return None;
        };
        if !sym.name.starts_with('$') {
            return None;
        }
        Some(FieldGroup {
            field_sym: Some(sym.id),
            decl_span: None,
            class: sym.package.clone()?,
            bare: sym.name[1..].to_string(),
            has_param: attributes.iter().any(|a| a == "param"),
            has_reader: attributes.iter().any(|a| a == "reader"),
        })
    }

    /// Union of edits for every spelling in a field group, all written as
    /// the bare name: variable occurrences contribute sigil-skipped spans
    /// (the `$` survives), constructor keys and reader calls their own
    /// bare-token spans.
    fn rename_field_group(&self, g: &FieldGroup, new_name: &str) -> Vec<(Span, String)> {
        let bare_new = new_name.trim_start_matches(['$', '@', '%']);
        let bare: Vec<(Span, String)> = self
            .field_group_spans_bare(g)
            .into_iter()
            .map(|s| (s, bare_new.to_string()))
            .collect();
        let mut edits = bare.clone();
        for (method, affix) in self
            .attr_projections
            .iter()
            .filter(|a| a.class == g.class && a.attr == g.bare)
            .filter_map(|a| match &a.kind {
                AttrProjectionKind::Accessor { method, affix } => {
                    Some((method.clone(), affix.clone()))
                }
                _ => None,
            })
        {
            // References-only when the name doesn't embed the attr — a
            // custom `writer => 'store_it'` can't be re-derived.
            let Some((pre, suf)) = affix else { continue };
            let new_method = format!("{}{}{}", pre, bare_new, suf);
            for span in self.mapped_member_spans(g, &method) {
                // A synthesized member's decl span IS the group decl token,
                // which the bare edit already covers — never double-edit.
                if bare.iter().any(|(b, _)| *b == span) {
                    continue;
                }
                edits.push((span, new_method.clone()));
            }
        }
        edits.sort_by_key(|(s, _)| (s.start.row, s.start.column));
        edits.dedup_by(|a, b| a.0 == b.0);
        edits
    }

    /// Every in-file spelling of a name-mapped member: its call sites
    /// (class-checked) and a user-written decl (`sub _build_size`), which
    /// unlike synthesized members does NOT sit on the group decl token.
    fn mapped_member_spans(&self, g: &FieldGroup, method: &str) -> Vec<Span> {
        let mut spans = Vec::new();
        for r in self.refs() {
            if let RefKind::MethodCall { method_name_span, .. } = &r.kind {
                if r.unqualified_target_name() != method {
                    continue;
                }
                let cls = r
                    .method_target()
                    .map(|t| t.invocant_class().to_string())
                    .or_else(|| self.method_call_invocant_class(r, None));
                if cls.as_deref() == Some(g.class.as_str()) {
                    spans.push(*method_name_span);
                }
            }
        }
        for s in &self.symbols {
            if matches!(s.kind, SymKind::Sub | SymKind::Method)
                && s.name == method
                && s.package.as_deref() == Some(g.class.as_str())
            {
                spans.push(s.selection_span);
            }
        }
        spans
    }

    /// Every in-file spelling of a field group as bare-name spans: the
    /// field variable (decl + body uses, sigil skipped so the `$`
    /// survives a bare-text rewrite), constructor keys at call sites,
    /// reader calls. Sorted, deduped — the uniform currency for both
    /// rename (write the new bare name at each) and references.
    fn field_group_spans(&self, g: &FieldGroup) -> Vec<Span> {
        let mut spans = self.field_group_spans_bare(g);
        for a in self
            .attr_projections
            .iter()
            .filter(|a| a.class == g.class && a.attr == g.bare)
        {
            if let AttrProjectionKind::Accessor { method, .. } = &a.kind {
                spans.extend(self.mapped_member_spans(g, method));
            }
        }
        spans.sort_by_key(|s| (s.start.row, s.start.column));
        spans.dedup();
        spans
    }

    /// The bare-name spellings only (variable, decl token, ctor keys,
    /// reader calls) — the set a rename writes the plain new name at.
    /// Name-mapped members re-derive their own names instead.
    fn field_group_spans_bare(&self, g: &FieldGroup) -> Vec<Span> {
        let mut spans: Vec<Span> = Vec::new();
        // The field variable: decl + every body use. Moo attrs have no
        // variable side; their decl token contributes directly.
        if let Some(field_sym) = g.field_sym {
            for (span, _access) in self.collect_refs_for_target(field_sym, true, None) {
                spans.push(Span {
                    start: Point::new(span.start.row, span.start.column + 1),
                    end: span.end,
                });
            }
        }
        if let Some(decl) = g.decl_span {
            spans.push(decl);
        }
        // Constructor keys at call sites.
        if g.has_param {
            let owner = HashKeyOwner::Sub {
                package: Some(g.class.clone()),
                name: "new".to_string(),
            };
            for r in self.refs() {
                if let Some(o) = r.hash_key_owner() {
                    if r.target_name == g.bare && o.found_by(&owner) {
                        spans.push(r.span);
                    }
                }
            }
        }
        // Reader calls (`$p->x`) dispatching to this class.
        if g.has_reader {
            for r in self.refs() {
                if let RefKind::MethodCall { method_name_span, .. } = &r.kind {
                    if r.unqualified_target_name() != g.bare {
                        continue;
                    }
                    let cls = r
                        .method_target()
                        .map(|t| t.invocant_class().to_string())
                        .or_else(|| self.method_call_invocant_class(r, None));
                    if cls.as_deref() == Some(g.class.as_str()) {
                        spans.push(*method_name_span);
                    }
                }
            }
        }
        // Internal hash slots — present iff the synthesis minted the
        // InternalKey projection (hash-backed repr; Corinna never does).
        // STRICT Class-owner equality: `found_by` broadening would leak
        // other subs' same-named arg keys into the group.
        if self
            .attr_projections
            .iter()
            .any(|a| {
                a.class == g.class
                    && a.attr == g.bare
                    && matches!(a.kind, AttrProjectionKind::InternalKey)
            })
        {
            for r in self.refs() {
                if let Some(HashKeyOwner::Class(c)) = r.hash_key_owner() {
                    if c == &g.class && r.target_name == g.bare {
                        spans.push(r.span);
                    }
                }
            }
        }
        spans.sort_by_key(|s| (s.start.row, s.start.column));
        spans.dedup();
        spans
    }

    /// Query-time owner for a deferred (`owner: None`) hash-key access:
    /// find the enclosing call ref and derive `Sub { invocant class,
    /// method }` with the index in hand. The build-time gate deferred
    /// exactly because the class wasn't visible locally — this is the
    /// other half of that bargain (the receiver-gated discipline: the
    /// type is the gate, resolved at query time).
    pub fn deferred_hash_key_owner(
        &self,
        key_ref: &Ref,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<HashKeyOwner> {
        // Enclosing method call whose span covers this key (an arg key inside a
        // call, or a `->{key}` deref of the call's return).
        let enclosing_call = self
            .refs
            .iter()
            .filter(|c| {
                matches!(c.kind, RefKind::MethodCall { .. })
                    && contains_point(&c.span, key_ref.span.start)
                    && contains_point(&c.span, key_ref.span.end)
            })
            .min_by_key(|c| span_size(&c.span));

        // Column-keyed call arg (`$rs->search({ name => … })`, columns
        // cross-file): the key is a COLUMN of the invocant class, not a key of
        // the call's return — mint the column owner so it joins the column group.
        // Checked before the return-deref case below, which would otherwise
        // claim the same span as `Sub{class, verb}`. Gated on the key actually
        // being a column of the class (so `order_by` etc. fall through).
        if let (Some(enclosing), Some(idx)) = (enclosing_call, module_index) {
            let verb = enclosing.unqualified_target_name();
            if self.is_column_keyed_verb(verb) {
                if let Some(class) = enclosing
                    .method_target()
                    .map(|t| t.invocant_class().to_string())
                    .or_else(|| self.method_call_invocant_class(enclosing, module_index))
                {
                    // The class's files as a SET: a shadow in ANY candidate
                    // overrides DBIC's verb (same gate as the builder's
                    // `user_shadows_verb`); the column may be minted by any.
                    let cands = idx.visible_def_candidates(&class);
                    let shadows = cands.iter().any(|c| {
                        idx.whole_present(c).symbols.iter().any(|s| {
                            matches!(s.kind, SymKind::Sub | SymKind::Method)
                                && s.name == verb
                                && s.package.as_deref() == Some(class.as_str())
                        })
                    });
                    let is_column = !shadows
                        && cands.iter().any(|c| {
                            idx.whole_present(c)
                                .field_projections_named(&key_ref.target_name, &class)
                                // ONLY a real `Class`-owned column (DBIC /
                                // Class::Accessor). A Moo/Corinna attr is also a
                                // field projection but its ctor key is
                                // `Sub{class,new}`-owned — leave it to the
                                // return-deref case below, which mints that.
                                .is_some_and(|p| p.has_class_key)
                        });
                    if is_column {
                        return Some(HashKeyOwner::Bridged { class });
                    }
                }
            }
        }

        // Inline method-chain receiver: `$obj->method->{key}` — the key
        // belongs to `method`'s return value, owned `Sub{invocant_class, method}`.
        if let Some(enclosing) = enclosing_call {
            if let Some(class) = enclosing
                .method_target()
                .map(|t| t.invocant_class().to_string())
                .or_else(|| self.method_call_invocant_class(enclosing, module_index))
            {
                return Some(HashKeyOwner::Sub {
                    package: Some(class),
                    name: enclosing.unqualified_target_name().to_string(),
                });
            }
        }
        // Variable bound to an imported function call: `$c = get_config();
        // $c->{key}`. The build-time walk only pins the owner when the sub's
        // return keys were known locally; for an imported sub the producer's
        // package lives across the index. Enrichment stamps this eagerly on
        // OPEN docs — recover it here for the unenriched workspace file (the
        // producer-origin rename's consumer), so the owner edge is symmetric
        // regardless of which side is open. Same lazy seam as above.
        if let (RefKind::HashKeyAccess { var_text, .. }, Some(idx)) =
            (&key_ref.kind, module_index)
        {
            if let Some(binding) = self.call_bindings.iter().find(|b| &b.variable == var_text) {
                let func = split_qualified(&binding.func_name).1;
                if let Some((pkg, keys)) = self.imported_sub_keys(func, idx) {
                    if keys.iter().any(|k| k == &key_ref.target_name) {
                        return Some(HashKeyOwner::Sub {
                            package: pkg,
                            name: func.to_string(),
                        });
                    }
                }
            }
        }
        None
    }

    /// Resolve the package a walk-time-unresolved bare call
    /// (no `Function` binding) actually targets, by asking which `use`d
    /// module binds the name. A bare `use Bank;` auto-imports `Bank`'s
    /// `@EXPORT` — a fact only the index knows, so the single-file walk left
    /// the call unpinned. The lazy re-resolution seam method dispatch
    /// (an unstamped `Method` binding) and deferred hash-key owners already
    /// use: where the index is in hand at query time, recover what the
    /// single-file builder couldn't. Returns the module the call resolves to,
    /// or `None` when no imported module binds the name (genuinely local /
    /// builtin / unresolved).
    pub fn deferred_call_package(
        &self,
        call_ref: &Ref,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        if !matches!(call_ref.kind, RefKind::FunctionCall) || call_ref.binding.is_some() {
            return None;
        }
        // Qualified calls (`Foo::bar`) pin at build time; only truly-bare
        // unresolved calls reach the index.
        if split_qualified(&call_ref.target_name).0.is_some() {
            return None;
        }
        let idx = module_index?;
        let name = call_ref.unqualified_target_name();
        // Later `use` wins, mirroring `resolve_call_package`'s import scan.
        // A split exporter's surface lives across its candidate files.
        for import in self.imports.iter().rev() {
            for cached in idx.visible_def_candidates(&import.module_name) {
                let surface = cached.analysis.export_surface_with_index(idx);
                if imported_names(import, &surface)
                    .iter()
                    .any(|(local, _)| local.as_str() == name)
                {
                    return Some(import.module_name.clone());
                }
            }
        }
        None
    }

    /// The Field twin of a promoted-constructor-property PARAM token: php's
    /// `__construct(public readonly Level $level)` declares BOTH the ctor
    /// param (a `$level` Variable, body uses) and the class Field (`level`,
    /// member accesses) with ONE source token. A cursor there lands on the
    /// Variable (emitted first); resolution wants the member identity, so
    /// re-target structurally: a Field one sigil-column to the right on the
    /// same token. Perl analyses never exhibit the shape (fields there are
    /// sigil-less symbols on their own tokens).
    pub fn promoted_field_twin(&self, sym: &Symbol) -> Option<&Symbol> {
        if !matches!(sym.kind, SymKind::Variable) || !sym.name.starts_with('$') {
            return None;
        }
        let bare = &sym.name[1..];
        self.symbols_named(bare)
            .iter()
            .map(|&sid| self.symbol(sid))
            .find(|f| {
                matches!(f.kind, SymKind::Field)
                    && f.selection_span.start.row == sym.selection_span.start.row
                    && f.selection_span.start.column == sym.selection_span.start.column + 1
                    && f.selection_span.end == sym.selection_span.end
            })
    }

    /// The promoted param's (field decl span, variable USE spans) —
    /// sigil-narrowed to the bare name — `Some` only when `member` on
    /// `class` is a promoted constructor property here. The member's
    /// identity group folds the uses in so rename rewrites every spelling
    /// of the one name: the decl token, the member accesses (the walked
    /// member target), AND the `$level` body uses that would otherwise be
    /// left referencing a parameter that no longer exists.
    pub fn promoted_param_use_spans(&self, member: &str, class: &str) -> Option<(Span, Vec<Span>)> {
        let field = self
            .symbols_named(member)
            .iter()
            .map(|&sid| self.symbol(sid))
            .find(|f| {
                matches!(f.kind, SymKind::Field)
                    && f.package.as_deref() == Some(class)
                    && self.symbol_is_class_content(f)
            })?;
        let sigiled = format!("${member}");
        let var_id = self
            .symbols_named(&sigiled)
            .iter()
            .copied()
            .find(|&sid| {
                let v = self.symbol(sid);
                matches!(v.kind, SymKind::Variable)
                    && v.selection_span.start.row == field.selection_span.start.row
                    && v.selection_span.start.column + 1 == field.selection_span.start.column
                    && v.selection_span.end == field.selection_span.end
            })?;
        let uses = self
            .collect_refs_for_target(var_id, false, None)
            .into_iter()
            .map(|(span, _)| Span {
                start: Point::new(span.start.row, span.start.column + 1),
                end: span.end,
            })
            .collect();
        Some((field.selection_span, uses))
    }

    /// The cross-file-facing view of the field group at `point`: the
    /// class + bare name + which projections exist, plus the origin-file
    /// variable spellings (fields are lexical to the class block, so the
    /// variable side can ONLY live here — keys and reader calls in other
    /// files are walked by `refs_to` against the projection targets the
    /// caller mints from these facts).
    pub fn field_projections_at(&self, point: Point) -> Option<FieldProjections> {
        let g = self.field_group_at(point)?;
        Some(self.projections_of(g))
    }

    /// Mint the group by name instead of cursor — the consumer-side
    /// entry: a deferred ctor key / accessor call in another file chases
    /// its owner edge to this class's analysis and asks for the group
    /// the cursor file can't see.
    pub fn field_projections_named(&self, bare: &str, class: &str) -> Option<FieldProjections> {
        // `bare` may name the attr directly (field / ctor key / internal slot /
        // reader) OR a name-mapped accessor (`has_size` → `size`); both resolve
        // to the one group, so a consumer-side cursor on any spelling chases to
        // the same source.
        let g = self
            .attr_group_for(bare, class)
            .or_else(|| {
                self.attr_for_mapped_accessor(bare, class)
                    .and_then(|attr| self.attr_group_for(&attr, class))
            })?;
        Some(self.projections_of(g))
    }

    fn projections_of(&self, g: FieldGroup) -> FieldProjections {
        let mut variable_spans: Vec<Span> = g
            .field_sym
            .map(|fs| {
                self.collect_refs_for_target(fs, true, None)
                    .into_iter()
                    .map(|(span, _)| Span {
                        start: Point::new(span.start.row, span.start.column + 1),
                        end: span.end,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A `has`/column group's decl token IS its only spelling here; a
        // field-backed group's decl is the field symbol's own token (the
        // rest of `variable_spans` are body uses), sigil-skipped like them.
        let mut decl_spans: Vec<Span> = g
            .field_sym
            .map(|fs| {
                let sel = self.symbol(fs).selection_span;
                vec![Span {
                    start: Point::new(sel.start.row, sel.start.column + 1),
                    end: sel.end,
                }]
            })
            .unwrap_or_default();
        if let Some(decl) = g.decl_span {
            variable_spans.push(decl);
            decl_spans.push(decl);
        }
        let mapped = self
            .attr_projections
            .iter()
            .filter(|a| a.class == g.class && a.attr == g.bare)
            .filter_map(|a| match &a.kind {
                AttrProjectionKind::Accessor { method, affix } => Some(MappedMember {
                    method: method.clone(),
                    affix: affix.clone(),
                }),
                _ => None,
            })
            .collect();
        let has_internal = self.attr_projections.iter().any(|a| {
            a.class == g.class
                && a.attr == g.bare
                && matches!(a.kind, AttrProjectionKind::InternalKey)
        });
        // A `Column`-owned HashKeyDef for this attr (DBIC column / Class::Accessor)
        // — its key uses are the condition args (`search({attr=>…})`), reached via
        // the `Column` owner; the group needs a `HashKeyOfClass` member for them.
        // NOT `$row->{attr}` derefs — a column isn't a hash slot (that's why it's
        // `Column`, not `Class`), so a deref never joins.
        let has_class_key = self.symbols.iter().any(|s| {
            matches!(s.kind, SymKind::HashKeyDef)
                && s.name == g.bare
                && matches!(
                    &s.detail,
                    SymbolDetail::HashKeyDef { owner: HashKeyOwner::Bridged { class: c }, .. } if *c == g.class
                )
        });
        FieldProjections {
            class: g.class,
            bare: g.bare,
            has_param: g.has_param,
            has_reader: g.has_reader,
            has_internal,
            has_class_key,
            field_backed: g.field_sym.is_some(),
            variable_spans,
            decl_spans,
            mapped,
        }
    }

    /// Determine what kind of rename is appropriate for the cursor position.
    ///
    /// For `RenameKind::Method`, the class is mandatory (so cross-file
    /// walks don't cross-link unrelated classes that share a method
    /// name). When the invocant can't be resolved to a class, rename
    /// falls through to symbol-at resolution; if that also has no
    /// class context (orphan Sub), returns `None` — the cursor isn't
    /// on something we can safely rename.
    pub fn rename_kind_at(&self, point: Point, module_index: Option<&dyn CrossFileLookup>) -> Option<RenameKind> {
        if let Some(r) = self.ref_at(point) {
            match &r.kind {
                RefKind::Variable | RefKind::ContainerAccess => return Some(RenameKind::Variable),
                RefKind::FunctionCall => {
                    // A CORE-bound ref is a builtin call keyword (`shift`,
                    // `keys`, ...). It has an identity — the in-file
                    // occurrence union serves highlights/references — but no
                    // rename: the language owns the name, and a Function
                    // target here would fan a rewrite over every builtin
                    // call site.
                    if r.resolved_package() == Some("CORE") {
                        return None;
                    }
                    // A qualified call (`Foo::baz()`) carries the whole path
                    // in `target_name`; the renamable identifier is the bare
                    // tail, scoped by the `Function` binding (the qualifier). When
                    // the walk left the call unresolved (a bare imported call
                    // — `use Bank;` auto-imports `@EXPORT`, invisible single-
                    // file), recover the exporting module via the index so the
                    // target scopes to the source package, not `None`.
                    let package = r
                        .resolved_package()
                        .map(str::to_string)
                        .or_else(|| self.deferred_call_package(r, module_index));
                    return Some(RenameKind::Function {
                        name: r.unqualified_target_name().to_string(),
                        package,
                    });
                }
                RefKind::MethodCall { method_name_span, .. } => {
                    // `ref_at` can return a MethodCall ref for a cursor anywhere
                    // in the call — its span covers the args. But only the
                    // method-name token renames the method: a hash key in the
                    // args (`$rs->search({ order_by => 1 })`) that didn't resolve
                    // to a column must NOT hijack `search`. Gate on the name span
                    // — widened to the QUALIFIED token: `method_name_span`
                    // covers the bare tail only, so a cursor on the qualifier
                    // segment (`SUPER::|new`, `Foo::Bar::|m`) minted no identity
                    // — goto-def (span-generous) answered while references /
                    // rename / highlights came back empty. The renamable
                    // identifier stays the bare tail; only cursor ACCEPTANCE
                    // covers the token the user sees as one word.
                    let token_span = {
                        let mut s = *method_name_span;
                        let qual =
                            r.target_name.len().saturating_sub(r.unqualified_target_name().len());
                        s.start.column = s.start.column.saturating_sub(qual);
                        s
                    };
                    if contains_point(&token_span, point) {
                        if let Some(class) = self.method_call_invocant_class(r, module_index) {
                            // FQ `$o->Foo::Bar::m` renames the bare `m` tail; the
                            // qualifier scopes the class (same as Function above).
                            return Some(RenameKind::Method {
                                name: r.unqualified_target_name().to_string(),
                                class,
                            });
                        }
                    }
                    // Invocant unresolvable (or cursor in args) — try symbol-at
                    // fallback below; if that also has no class, bail rather
                    // than return a class-less Method rename.
                }
                RefKind::PackageRef => return Some(RenameKind::Package(r.target_name.clone())),
                RefKind::HashKeyAccess { .. } => return Some(RenameKind::HashKey(r.target_name.clone())),
                RefKind::DispatchCall { .. } if r.handler_owner().is_some() => {
                    let owner = r.handler_owner().unwrap();
                    // A dispatch name spelled via a variable (`my $e = 'x';
                    // $obj->on($e)`) has no literal token to rewrite — the
                    // cursor sits on the variable, so rename the variable, not
                    // the event. A literal site carries no co-located Variable
                    // ref, so this only diverts the folded case.
                    if self.refs.iter().any(|o| {
                        matches!(o.kind, RefKind::Variable | RefKind::ContainerAccess)
                            && contains_point(&o.span, point)
                    }) {
                        return Some(RenameKind::Variable);
                    }
                    return Some(RenameKind::Handler {
                        owner: owner.clone(),
                        name: r.target_name.clone(),
                    });
                }
                // Unresolved DispatchCall — owner couldn't be determined
                // at build time, so rename can't safely scope.
                RefKind::DispatchCall { .. } => return None,
            }
        }
        if let Some(sym) = self.symbol_at(point) {
            return match sym.kind {
                SymKind::Variable | SymKind::Field => Some(RenameKind::Variable),
                SymKind::Sub => Some(RenameKind::Function {
                    name: sym.name.clone(),
                    // Pack analyses attribute namespaces partially (macro-
                    // guarded opens desync the sticky context) — recover the
                    // def's namespace positionally so `detail::f` and
                    // `fmt::f` mint distinct targets. Perl attribution is
                    // total (closure empty → untouched).
                    package: sym.package.clone().or_else(|| {
                        (!self.pack.include_closure.is_empty())
                            .then(|| self.enclosing_package_of(&sym.span))
                            .flatten()
                    }),
                }),
                SymKind::Method => {
                    let class = sym.package.clone()?;
                    Some(RenameKind::Method {
                        name: sym.name.clone(),
                        class,
                    })
                }
                SymKind::Package | SymKind::Class => Some(RenameKind::Package(sym.name.clone())),
                SymKind::Handler => {
                    if let SymbolDetail::Handler { owner, .. } = &sym.detail {
                        Some(RenameKind::Handler { owner: owner.clone(), name: sym.name.clone() })
                    } else { None }
                }
                // A hash-key def with no field group (a return-hash key, the
                // `return { host => 1 }` shape) — `field_group_at` already
                // claimed the constructor/`has` cases ahead of us. The owner
                // recovers from `hash_key_owner_at`, so resolve_symbol's
                // `HashKey` arm mints the same `HashKeyOfSub`/`HashKeyOfClass`
                // target a `$result->{key}` access would, making def→accesses
                // symmetric with access→def.
                SymKind::HashKeyDef => Some(RenameKind::HashKey(sym.name.clone())),
                _ => None,
            };
        }
        None
    }

    /// If the cursor sits on an `our` (package-global) variable — its decl, an
    /// unqualified read that resolves to it, or a qualified `$Pkg::var` access
    /// — return `(package, sigil-name)` (e.g. `("Cfg", "$debug")`). `None` for
    /// lexical `my` vars (single-file) and non-variables. Drives the cross-file
    /// `PackageVar` rename target: a package global is reachable everywhere as
    /// `$Pkg::var`, so renaming it is a cross-file refactor, where a lexical
    /// `my` stays the single-file `Local` path.
    pub fn package_var_at(&self, point: Point) -> Option<(String, String)> {
        // `$::x` / `$main::x` both name the `main` global; the `our` decl's
        // package is "main", so normalize the empty (leading-`::`) spelling.
        let norm = |p: &str| if p.is_empty() { "main".to_string() } else { p.to_string() };
        if let Some(r) = self.ref_at(point) {
            if matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess) {
                // Qualified `$Pkg::var` — the package is explicit in the token.
                if let Some((pkg, name)) = r.qualified_var_target() {
                    return Some((norm(pkg), name));
                }
                // Unqualified — a package var only if it resolves to an `our`.
                let sym = r
                    .resolved_symbol()
                    .map(|id| self.symbol(id))
                    .or_else(|| self.resolve_variable(&r.target_name, point));
                if let Some(s) = sym {
                    if let SymbolDetail::Variable { decl_kind: DeclKind::Our, .. } = s.detail {
                        return Some((norm(s.package.as_deref()?), s.name.clone()));
                    }
                }
            }
        }
        if let Some(s) = self.symbol_at(point) {
            if let SymbolDetail::Variable { decl_kind: DeclKind::Our, .. } = s.detail {
                return Some((norm(s.package.as_deref()?), s.name.clone()));
            }
        }
        None
    }

    /// Scope-aware rename for a sub/method: matches decls + both
    /// call shapes that resolve to this (scope, name) pair.
    ///
    ///   * decl walk — `Sub`/`Method` symbols whose `package == scope`
    ///   * FunctionCall refs whose `resolved_package == scope`
    ///   * MethodCall refs whose `invocant_class == scope`
    ///
    /// The two call shapes both resolve to the same underlying sub:
    /// `package Foo; sub run {}` is callable as `run()` OR
    /// `$self->run()` OR `Foo::run()`. A rename must rewrite every
    /// shape, and must NOT rewrite same-named subs/methods in other
    /// packages. When `scope == None`, matches top-level/script subs
    /// with no package; FunctionCall refs whose resolver didn't pin
    /// a package match those. MethodCall refs always need a class —
    /// `None` scope never matches them.
    ///
    /// Single-file rename primitive: exact-match on `scope`, no
    /// inheritance fan-out. Cross-file callers go through `refs_to`
    /// (which calls `method_rename_chain` for MethodCall fan-out) and
    /// convert `RefLocation`s to edits directly.
    #[allow(dead_code)]
    fn rename_callable_in_scope(
        &self,
        old_name: &str,
        scope: &Option<String>,
        new_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(Span, String)> {
        let mut edits = Vec::new();
        for sym in &self.symbols {
            if sym.name != old_name { continue; }
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
            if sym.package != *scope { continue; }
            edits.push((sym.selection_span, new_name.to_string()));
        }
        for r in self.refs() {
            if r.target_name != old_name { continue; }
            match &r.kind {
                RefKind::FunctionCall => {
                    if r.resolved_package() == scope.as_deref() {
                        edits.push((r.span, new_name.to_string()));
                    }
                }
                RefKind::MethodCall { method_name_span, .. } => {
                    // MethodCall refs target a class — `None` scope
                    // doesn't reach methods.
                    if let (Some(cls), Some(wanted)) =
                        (self.method_call_invocant_class(r, module_index), scope.as_ref())
                    {
                        if &cls == wanted {
                            edits.push((*method_name_span, new_name.to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
        edits
    }

    /// Package-scoped sub rename. Single-file; callers that need
    /// cross-file fan-out (including inheritance) go through `refs_to`
    /// with `RoleMask::EDITABLE`. Tested by single-file rename pins.
    #[allow(dead_code)]
    pub fn rename_sub_in_package(
        &self,
        old_name: &str,
        package: &Option<String>,
        new_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(Span, String)> {
        self.rename_callable_in_scope(old_name, package, new_name, module_index)
    }

    /// Class-scoped method rename. Single-file; callers that need
    /// cross-file fan-out (including inheritance) go through `refs_to`
    /// with `RoleMask::EDITABLE`. Tested by single-file rename pins.
    #[allow(dead_code)]
    pub fn rename_method_in_class(
        &self,
        old_name: &str,
        class: &str,
        new_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(Span, String)> {
        self.rename_callable_in_scope(old_name, &Some(class.to_string()), new_name, module_index)
    }

    /// Find all occurrences of a package name (def + refs + use statements) for cross-file rename.
    #[allow(dead_code)]
    pub fn rename_package(&self, old_name: &str, new_name: &str) -> Vec<(Span, String)> {
        let mut edits = Vec::new();
        for sym in &self.symbols {
            if sym.name == old_name && matches!(sym.kind, SymKind::Package | SymKind::Class | SymKind::Module) {
                edits.push((sym.selection_span, new_name.to_string()));
            }
        }
        for r in self.refs() {
            if r.target_name == old_name && matches!(r.kind, RefKind::PackageRef) {
                edits.push((r.span, new_name.to_string()));
            }
        }
        edits
    }

}
