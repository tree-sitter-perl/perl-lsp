//! Invocant & target resolution: target-at-cursor, the invocant
//! ladder, the dispatch-class ladder, role contracts, class-content
//! predicates.

use super::*;

impl FileAnalysis {
    // ---- Internal resolution helpers ----

    /// The LOCAL-arm target resolver behind `find_occurrences`: cursor →
    /// this file's own symbol, for cursors the CandidateSet answered
    /// `Local`/`None` (lexicals, and targets only this file can anchor).
    /// Cross-file identity is `index::resolve::resolve_symbol_scoped` —
    /// never widen this to a second cross-file minting.
    /// Returns (SymbolId, include_decl_in_refs).
    pub(super) fn resolve_target_at(&self, point: Point, module_index: Option<&dyn CrossFileLookup>) -> Option<(SymbolId, bool)> {
        // Check refs first
        if let Some(r) = self.ref_at(point) {
            match &r.kind {
                RefKind::Variable | RefKind::ContainerAccess => {
                    if let Some(sym_id) = r.resolved_symbol() {
                        return Some((sym_id, true));
                    }
                    // Try resolving manually
                    if let Some(sym) = self.resolve_variable(&r.target_name, point) {
                        return Some((sym.id, true));
                    }
                }
                RefKind::FunctionCall => {
                    // Qualified calls carry the full path in `target_name`;
                    // symbols are keyed by bare name + the `Function` binding.
                    if let Some(sid) = self
                        .package_scoped_callable(r.unqualified_target_name(), r.resolved_package())
                    {
                        return Some((sid, true));
                    }
                }
                RefKind::MethodCall { .. } => {
                    let class_name = self.method_call_invocant_class(r, module_index);
                    // Bare method name (FQ `$o->Foo::Bar::m` resolves `m`).
                    let method = r.unqualified_target_name();
                    // Try inheritance-aware resolution first
                    if let Some(ref cn) = class_name {
                        match self.resolve_method_in_ancestors(cn, method, module_index) {
                            Some(MethodResolution::Local { sym_id, .. }) => {
                                return Some((sym_id, true));
                            }
                            Some(MethodResolution::CrossFile { .. }) => {
                                // Cross-file: no local SymbolId. Match
                                // a local symbol only when its package
                                // equals the resolved class — no
                                // same-name cross-class latching.
                                for &sid in self.symbols_named(method) {
                                    let sym = self.symbol(sid);
                                    if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                                    if sym.package.as_deref() == Some(cn.as_str()) {
                                        return Some((sid, true));
                                    }
                                }
                            }
                            None => {}
                        }
                        // No local match, and the class is known but
                        // has no local decl — return a synthetic
                        // target by name filtered to the class, so
                        // collect_refs_for_target still walks refs
                        // correctly. If there's NO matching symbol on
                        // the class locally, produce no target —
                        // better than cross-linking a same-named
                        // method on a different class.
                        for &sid in self.symbols_named(method) {
                            let sym = self.symbol(sid);
                            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                            if sym.package.as_deref() == Some(cn.as_str()) {
                                return Some((sid, true));
                            }
                        }
                        // Class known but not defined locally — no
                        // local symbol to anchor on. Caller may still
                        // collect refs via `refs_to` at the LSP layer,
                        // but highlight/definition within this file
                        // have nothing to return.
                        return None;
                    }
                    // Invocant class couldn't be pinned at all —
                    // last-resort name match. Only reaches here for
                    // refs the builder AND the runtime resolver both
                    // failed on (rare: ERROR-recovered invocants).
                    for &sid in self.symbols_named(method) {
                        if matches!(self.symbol(sid).kind, SymKind::Sub | SymKind::Method) {
                            return Some((sid, true));
                        }
                    }
                }
                RefKind::PackageRef => {
                    for &sid in self.symbols_named(&r.target_name) {
                        if matches!(self.symbol(sid).kind, SymKind::Package | SymKind::Class | SymKind::Module) {
                            return Some((sid, true));
                        }
                    }
                }
                RefKind::HashKeyAccess { .. } => {
                    if let Some(owner) = r.hash_key_owner() {
                        for def in self.hash_key_defs_for_owner(owner) {
                            if def.name == r.target_name {
                                return Some((def.id, true));
                            }
                        }
                    }
                }
                RefKind::DispatchCall { .. } if r.handler_owner().is_some() => {
                    let owner = r.handler_owner().unwrap();
                    // Folded name (`$obj->on($evt)`): the cursor is on the
                    // variable, so resolve the variable — not the event. Mirrors
                    // the `rename_kind_at` guard so references/highlight/rename
                    // agree (the event's literal sites still resolve from there).
                    if let Some(var) = self.refs.iter().find(|o| {
                        matches!(o.kind, RefKind::Variable | RefKind::ContainerAccess)
                            && contains_point(&o.span, point)
                    }) {
                        if let Some(sym_id) = var
                            .resolved_symbol()
                            .or_else(|| self.resolve_variable(&var.target_name, point).map(|s| s.id))
                        {
                            return Some((sym_id, true));
                        }
                    }
                    for sym in &self.symbols {
                        if sym.name != r.target_name { continue; }
                        if let SymbolDetail::Handler { owner: o, .. } = &sym.detail {
                            if o == owner {
                                return Some((sym.id, true));
                            }
                        }
                    }
                }
                RefKind::DispatchCall { .. } => {}
            }
        }

        // Check if cursor is directly on a symbol declaration. The decl is
        // part of the occurrence union — the same convention the
        // CandidateSet's cross-file matcher applies (every Target walk mints
        // a Declaration row), so def-side and access-side cursors answer the
        // same set and highlights/rename never drop the token under the
        // cursor itself.
        if let Some(sym) = self.symbol_at(point) {
            return Some((sym.id, true));
        }

        None
    }

    /// The class a value's MEMBER ACCESS dispatches against, index-aware.
    /// `class_name_lenient()` plus one refinement the pure projection
    /// can't make: a template `Instance` whose EXACT canonical spelling
    /// names an existing class (a per-spec Class — `template<> struct
    /// formatter<int>`) dispatches there; otherwise the base primary.
    /// Exact-spelling-or-primary only — no partial-pattern specificity
    /// ladder (that selection tier is deferred; fork #4 in
    /// `docs/adr/cpp-templates.md`). Non-Instance types are unchanged
    /// by construction (`exact_spelling()` answers `None`).
    pub fn dispatch_class_of(
        &self,
        t: &InferredType,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        self.dispatch_of(t, module_index).map(|(class, _)| class)
    }

    /// `dispatch_class_of` plus the EFFECTIVE RECEIVER member queries on
    /// that class should carry: for the exact/primary rungs the value
    /// itself; for a partial-spec match, the value REBOUND into the spec's
    /// own param space (`formatter<vector<int>>` dispatching to
    /// `formatter<vector<T>>` hands members `Instance { args: [int] }`,
    /// so `ParamOf(0)` / field substitution read the PATTERN's bindings,
    /// not the primary's positional args).
    pub fn dispatch_of(
        &self,
        t: &InferredType,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<(String, InferredType)> {
        self.dispatch_ladder_of(t, module_index).into_iter().next()
    }

    /// The full specificity ladder for a value's member dispatch, ranked:
    /// exact-spelling spec > partial-pattern specs (most literal structure
    /// first) > base primary. `dispatch_of` takes the head; the goto-def
    /// family projection walks the whole ladder so a use resolving against
    /// a spec still OFFERS the primary's def (ranked, never pruned).
    /// Non-Instance types get their single `class_name()` rung.
    pub fn dispatch_ladder_of(
        &self,
        t: &InferredType,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(String, InferredType)> {
        let mut t = t;
        while let Some(inner) = t.optional_inner() {
            t = inner;
        }
        let mut out: Vec<(String, InferredType)> = Vec::new();
        if let Some(p) = t.as_parametric() {
            if let Some(spelling) = p.exact_spelling() {
                if self.class_exists(&spelling, module_index) {
                    out.push((spelling, t.clone()));
                }
                // Partial-pattern rung: every spec of the base whose
                // pattern matches, most-specific first. The rebound
                // receiver carries the pattern's bindings.
                for (spec, bindings, _score) in self.matching_partial_specs(p, module_index) {
                    if out.iter().any(|(c, _)| *c == spec) {
                        continue;
                    }
                    let recv = InferredType::Parametric(ParametricType::Instance {
                        base: spec.clone(),
                        args: bindings,
                    });
                    out.push((spec, recv));
                }
            }
        }
        if let Some(cn) = t.class_name() {
            if !out.iter().any(|(c, _)| c == cn) {
                out.push((cn.to_string(), t.clone()));
            }
        }
        out
    }

    /// Partial specializations of `concrete`'s base whose pattern matches
    /// it, with bindings in spec-param order — sorted most-specific first
    /// (score desc, then spelling asc for determinism). Candidates come
    /// from the local `specializes` edges ∪ the index's spec map; each
    /// spec's param names ride `template_params` in its defining file.
    fn matching_partial_specs(
        &self,
        concrete: &ParametricType,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<(String, Vec<InferredType>, u32)> {
        let Some(base) = concrete.class_name() else { return Vec::new() };
        // (spec spelling, params) candidates, local first.
        let mut cands: Vec<(String, Vec<String>)> = Vec::new();
        let push_cand = |spec: &str, params: Option<&Vec<String>>, cands: &mut Vec<(String, Vec<String>)>| {
            let Some(params) = params else { return };
            if params.is_empty() || cands.iter().any(|(s, _)| s == spec) {
                return; // a full spec is the exact rung's business
            }
            cands.push((spec.to_string(), params.clone()));
        };
        for (spec, primary) in &self.pack.specializes {
            if primary == base {
                push_cand(spec, self.pack.template_params.get(spec), &mut cands);
            }
        }
        if let Some(idx) = module_index {
            for (spec, module) in idx.direct_specializations_of(base) {
                // The spec's params live in whichever candidate file
                // declares it (pack lane — never evicted).
                let params = idx
                    .visible_def_candidates(&module)
                    .iter()
                    .find_map(|c| c.analysis.pack.template_params.get(&spec).cloned());
                push_cand(&spec, params.as_ref(), &mut cands);
            }
        }
        let mut out: Vec<(String, Vec<InferredType>, u32)> = Vec::new();
        for (spec, params) in cands {
            let Some(pattern) = ParametricType::instance_from_spelling(&spec) else { continue };
            if let Some((bindings, score)) = match_template_pattern(&pattern, &params, concrete) {
                out.push((spec, bindings, score));
            }
        }
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// The declaration-order template parameter names of `class` — local
    /// `template_params`, else the class's own cached file. Empty for
    /// non-template classes and full specs.
    fn class_template_params(
        &self,
        class: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<String> {
        if let Some(p) = self.pack.template_params.get(class) {
            return p.clone();
        }
        if let Some(idx) = module_index {
            // Any candidate file declaring `class` may carry its template
            // params (pack lane — never evicted).
            if let Some(p) = idx
                .visible_def_candidates(class)
                .iter()
                .find_map(|c| c.analysis.pack.template_params.get(class).cloned())
            {
                return p;
            }
        }
        Vec::new()
    }

    /// A member's VALUE on a receiver — the receiver-typed entry every
    /// tree-free member consumer routes through (the pack chain arm of
    /// `expr_type_at_span`, the sentinel's receiver typing, member hover).
    /// Dispatch runs the specificity ladder (`dispatch_of`), method
    /// returns thread the (rebound) receiver into the `PackageSymbol`
    /// query so `ReturnExpr::ParamOf` substitutes; a data field falls
    /// back to its declared type with the class's params substituted
    /// against the receiver's instance args.
    pub fn member_value_type(
        &self,
        receiver: &InferredType,
        member: &str,
        module_index: Option<&dyn CrossFileLookup>,
        arg_count: Option<usize>,
    ) -> Option<InferredType> {
        // The whole ladder, most-specific first: a member the winning spec
        // doesn't define falls through to the next rung (ultimately the
        // primary) — same never-pruned order goto-def presents.
        for (class, recv) in self.dispatch_ladder_of(receiver, module_index) {
            if let Some(t) =
                self.method_return_type_on(&class, &recv, member, module_index, arg_count)
            {
                return Some(t);
            }
            let Some(raw) = self.field_type_on_class(&class, member, module_index) else {
                continue;
            };
            let params = self.class_template_params(&class, module_index);
            if params.is_empty() {
                return Some(raw);
            }
            let InferredType::Parametric(ParametricType::Instance { args, .. }) = &recv else {
                return Some(raw);
            };
            return Some(substitute_type_params(&raw, &params, args));
        }
        None
    }

    /// `dispatch_class_of`'s type-to-type twin for consumers that hand a
    /// receiver TYPE onward (the sentinel completion context, whose
    /// downstream projects `class_name()` without an index): an
    /// `Instance` with an existing exact-spelling class collapses to
    /// `ClassName(spelling)`; everything else passes through untouched.
    pub fn refine_instance_dispatch(
        &self,
        t: InferredType,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> InferredType {
        if t.as_parametric().is_some() {
            if let Some((class, _)) = self.dispatch_of(&t, module_index) {
                // Collapse ONLY when the ladder picked a spec class (exact
                // or partial-pattern) — a primary dispatch already projects
                // the base via `class_name()`, and keeping the Instance
                // preserves its args for downstream typing.
                if t.class_name() != Some(class.as_str()) {
                    return InferredType::ClassName(class);
                }
            }
        }
        t
    }

    /// Does a class by this exact name exist — locally (a Class/Package
    /// symbol) or as a cached module? The existence gate behind the
    /// exact-spelling dispatch refinement.
    fn class_exists(&self, name: &str, module_index: Option<&dyn CrossFileLookup>) -> bool {
        self.symbols
            .iter()
            .any(|s| matches!(s.kind, SymKind::Class | SymKind::Package) && s.name == name)
            || module_index.is_some_and(|mi| mi.get_cached(name).is_some())
    }

    /// The class a `MethodCall` ref's method lookup STARTS at — the
    /// dispatch projection over `method_call_invocant_type` (THE invocant
    /// ladder). A qualified method token (`Foo::m` / `::m` / `SUPER::m`)
    /// overrides where lookup starts without changing what the receiver
    /// IS, so the token arm lives here in the projection, not in the
    /// value ladder (Parametric arg-claiming on `$rs->SUPER::search`
    /// still sees the resultset). Every other shape projects the
    /// ladder's type through `dispatch_class_of` plus the DBIC
    /// source-moniker resolve.
    pub fn method_call_invocant_class(
        &self,
        r: &Ref,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        let RefKind::MethodCall { invocant, .. } = &r.kind else {
            return None;
        };
        let cn = 'cn: {
            // A qualified method token names its dispatch class explicitly
            // — Perl ignores the invocant's class for the lookup, so the
            // token wins ahead of invocant resolution. (A plugin-bridged
            // token is never method-qualified; the ladder's bridged arm
            // owns it.)
            if matches!(invocant, crate::model::conventions::Invocant::Name(_)) {
                use crate::model::conventions::MethodToken;
                match MethodToken::parse(&r.target_name) {
                    MethodToken::Super(name) => {
                        // SUPER searches the enclosing package's parents'
                        // MRO; the dispatch class is whichever ancestor
                        // actually defines it (multi-parent safe). `None`
                        // when the index isn't available yet (build-time
                        // stamp of a dependency file, cross-file parent) —
                        // every query-time consumer re-resolves with the
                        // index: open docs via the enrichment re-stamp,
                        // goto-def via the cross-file path,
                        // references/rename via `refs_to`'s SUPER arm.
                        let encl = self.enclosing_class_for_scope(r.scope)?;
                        break 'cn self
                            .resolve_super_method(&encl, name, module_index)
                            .map(|res| res.class().to_string())?;
                    }
                    token => {
                        if let Some(pkg) = token.literal_package() {
                            break 'cn pkg.to_string();
                        }
                    }
                }
            }
            self.method_call_invocant_type(r, module_index)
                .and_then(|t| self.dispatch_class_of(&t, module_index))?
        };
        // A DBIC resultset row projects to `ClassName(<source moniker>)` —
        // the short registration name (`Artist`), not the FQ result class
        // (`DBICTest::Schema::Artist`) where methods/columns live. Resolve
        // it here (query time, index in hand) so goto-def / references on
        // `$row->method` start the ancestor walk from a real class. Only a
        // single-segment name that names no real class is a moniker
        // candidate, so ordinary class receivers are untouched.
        Some(self.resolve_dbic_source_moniker(cn, None, module_index))
    }

    /// Resolve a DBIC source moniker (`Artist`) to the FQ result class
    /// (`DBICTest::Schema::Artist`). DBIC's `$schema->resultset('Artist')`
    /// names a row by its registered SOURCE moniker — by convention the
    /// basename of a result class registered under a schema
    /// (`load_classes`/`load_namespaces`), or its `source_name` override.
    /// The row projection (`->find`/`->first`/`->create`) types the value
    /// as `ClassName(moniker)`, which is not a real class; this maps it
    /// back so downstream method/column resolution works.
    ///
    /// The convention (moniker = last `::` segment / source_name) is DBIC
    /// knowledge, resolved GENERICALLY here via the cross-file index: a
    /// candidate is any indexed class that (a) is a DBIC result class
    /// (transitively isa `DBIx::Class`) and (b) whose basename or declared
    /// `source_name` equals the moniker. When several match, `schema_hint`
    /// (the receiver's concrete schema class, when known) scopes to sources
    /// under it; otherwise the largest source family wins (the workspace's
    /// primary schema), lexicographic tie-break. The residual — picking a
    /// source when the `$schema` value is untyped — is the value-provenance
    /// fork logged in `docs/open-forks.md`.
    ///
    /// `cn` passes through unchanged unless it is a single-segment name
    /// that names no known class (the only moniker shape), so ordinary
    /// receivers pay only a `class_exists` check.
    pub(crate) fn resolve_dbic_source_moniker(
        &self,
        cn: String,
        schema_hint: Option<&str>,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> String {
        if cn.contains("::") {
            return cn;
        }
        let Some(mi) = module_index else { return cn };
        if self.class_exists(&cn, module_index) {
            return cn;
        }
        // Gather candidate result classes by basename / source_name match.
        let mut candidates: Vec<String> = Vec::new();
        mi.for_each_cached(&mut |name, cached| {
            let basename = name.rsplit("::").next().unwrap_or(name);
            let hits = basename == cn
                || cached.analysis.dbic_source_name.as_deref() == Some(cn.as_str());
            if hits && Self::class_is_dbic_result(name, mi) {
                candidates.push(name.to_string());
            }
        });
        if candidates.is_empty() {
            return cn;
        }
        // Scope to the receiver's schema when it is a concrete schema whose
        // namespace actually contains matches.
        if let Some(sch) = schema_hint {
            let scoped: Vec<String> = candidates
                .iter()
                .filter(|c| c.starts_with(&format!("{sch}::")))
                .cloned()
                .collect();
            if !scoped.is_empty() {
                candidates = scoped;
            }
        }
        if candidates.len() == 1 {
            return candidates.into_iter().next().unwrap();
        }
        // Ambiguous: prefer the candidate in the largest source family
        // (the parent namespace shared by the most indexed classes — a
        // proxy for the workspace's primary schema), lexicographic tie.
        // Family sizes precomputed in ONE index sweep before the sort —
        // the comparator otherwise called `for_each_cached` per comparison.
        let prefixes: Vec<(String, String)> = candidates
            .iter()
            .map(|c| {
                let parent = c.rsplit_once("::").map(|(p, _)| p).unwrap_or(c);
                (c.clone(), format!("{parent}::"))
            })
            .collect();
        let mut fam_size: HashMap<String, usize> =
            candidates.iter().map(|c| (c.clone(), 0usize)).collect();
        mi.for_each_cached(&mut |name, _| {
            for (cand, prefix) in &prefixes {
                if name.starts_with(prefix) {
                    *fam_size.get_mut(cand).unwrap() += 1;
                }
            }
        });
        candidates.sort_by(|a, b| fam_size[b].cmp(&fam_size[a]).then_with(|| a.cmp(b)));
        candidates.into_iter().next().unwrap()
    }

    /// Is `class` a DBIC result class — transitively `isa DBIx::Class`
    /// (through the cross-file parent graph) but NOT itself a schema or
    /// resultset base? Depth-capped like the MRO walk. Used to gate
    /// source-moniker resolution so a stray same-basename non-DBIC class
    /// can't be mistaken for a row source.
    fn class_is_dbic_result(class: &str, mi: &dyn CrossFileLookup) -> bool {
        // A result class descends from DBIx::Class::Core / ::Row (the
        // row-behavior roots), never from ::Schema or ::ResultSet. The
        // Core/Row `Hit` is captured (not short-circuited) because a later
        // ::Schema/::ResultSet ancestor must still be able to disqualify;
        // `Reject` short-circuits that negative, and `rejected` distinguishes
        // it from plain exhaustion. Deliberately cross-file-only (no local
        // `package_parents` seam), depth-capped tighter than the isa walkers.
        let mut isa_dbic = false;
        let mut rejected = false;
        walk_ancestry(
            class,
            40,
            |c| mi.parents_cached(c),
            |c| {
                if c == "DBIx::Class::Core" || c == "DBIx::Class::Row" {
                    isa_dbic = true;
                    WalkVerdict::Miss
                } else if c == "DBIx::Class::Schema" || c == "DBIx::Class::ResultSet" {
                    rejected = true;
                    WalkVerdict::Reject
                } else {
                    WalkVerdict::Miss
                }
            },
        );
        isa_dbic && !rejected
    }

    /// Resolve a plugin-bridged invocant *class key* to the workspace class
    /// that owns `action`. The key is already in class form — the emitting
    /// plugin applied its own naming convention (e.g. camelized a Mojo
    /// controller token) — so resolution here is GENERIC, never
    /// framework-specific:
    ///   * `Exact` — the key is the class name.
    ///   * `Tail`  — the key is a `::`-tail (the plugin dropped the
    ///     namespace); match any class whose tail equals it.
    /// Candidates come from the origin file's own packages and the index;
    /// ownership ("does this class actually resolve `action`?") is the gate.
    /// When several own it, pick deterministically by name. When NONE do —
    /// an incomplete action mid-completion, or a route to a not-yet-written
    /// method — fall back to the matching candidates so the class still
    /// resolves.
    fn resolve_bridged_class(
        &self,
        key: &str,
        match_mode: crate::model::conventions::BridgedMatch,
        action: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        use crate::model::conventions::BridgedMatch;
        let matches = |class: &str| match match_mode {
            BridgedMatch::Exact => class == key,
            BridgedMatch::Tail => {
                class == key || class.strip_suffix(key).is_some_and(|p| p.ends_with("::"))
            }
        };
        let mut candidates: Vec<String> = self
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Package | SymKind::Class) && matches(&s.name))
            .map(|s| s.name.clone())
            .collect();
        if let Some(idx) = module_index {
            for m in idx.modules_with_symbol(action) {
                if matches(&m) {
                    candidates.push(m);
                }
            }
            idx.for_each_cached(&mut |name, _| {
                if matches(name) {
                    candidates.push(name.to_string());
                }
            });
        }
        candidates.sort();
        candidates.dedup();
        if candidates.is_empty() {
            return None;
        }
        let owners: Vec<String> = candidates
            .iter()
            .filter(|cls| {
                self.resolve_method_in_ancestors(cls, action, module_index)
                    .is_some()
            })
            .cloned()
            .collect();
        if owners.is_empty() { candidates } else { owners }
            .into_iter()
            .next()
    }

    /// The `InferredType` of a `MethodCall` ref's invocant — **the**
    /// invocant ladder, resolved via the witness bag. No tree, no text
    /// fallback, no per-reader parallel paths: every reader routes
    /// through here, and `method_call_invocant_class` is its dispatch
    /// projection. Token-blind by design: a `SUPER::`/`Foo::` method
    /// qualifier overrides where lookup starts, never what the receiver
    /// IS, so the token arm lives in the projection while Parametric
    /// consumers (hash-key arg-claiming on `$rs->SUPER::search`) still
    /// see the receiver's value here.
    ///
    /// A rung answers only when its type carries a dispatch class
    /// (`dispatch_class_of`); a classless answer (bare `HashRef`, `Str`)
    /// falls through so a deeper rung — ultimately the cross-file chain
    /// fallback or the bareword rule — can still resolve. The one
    /// exception is the variable rung, which returns its bag type
    /// unprojected (a classless variable receiver has no deeper rung to
    /// reach, and the flavor is still informative).
    ///
    /// Dispatch by invocant shape, in rung order:
    ///   * plugin-bridged token → generic class-key resolution (the
    ///     emitting plugin already applied its naming convention; no
    ///     plugin consult here, and no richer flavor than the class).
    ///   * positional receiver (`shift`/`$_[0]`) / `__PACKAGE__` →
    ///     enclosing class (not real variables; the bag has no witness).
    ///   * element place (`$self->{x}`, `$h{k}`) refined by a guard →
    ///     the narrowing witness at the use-site point, ahead of the
    ///     functional chase (docs/adr/flow-narrowing.md); `is_element_
    ///     place` tells a real place from a scalar deref.
    ///   * function-call receiver → the zero-arg return of the named sub
    ///     (its ref spans only the name, invisible to the exact-span read).
    ///   * any recorded expression → `expr_type_at_span` (scalar/array/
    ///     hash reads, baked literals, and chain receivers — its
    ///     exact-span call-ref arm keeps Parametric flavors intact, so
    ///     DBIC row-class narrowing survives the hop).
    ///   * cross-file chain receiver → re-resolve the receiver's class
    ///     fresh with the index and chase `PackageSymbol` through
    ///     `find_method_return_type` (the one structure-from-refs step
    ///     the build-time bag can't pre-record).
    ///   * `$var` / `@var` / `%var` → `inferred_type_via_bag_ctx` (so
    ///     cross-file enrichment's variable types compose), with the
    ///     conventional-invocant enclosing-class fallback.
    ///   * bareword → a zero-arg ClassName-returning sub's class, else
    ///     the bareword itself.
    ///
    /// Query-only: build-time chain typing already landed its product in
    /// the bag; this never re-derives. `module_index` lets chain
    /// receivers whose return type lives in another package resolve —
    /// pass `None` only for CLI debug / isolated tests.
    pub fn method_call_invocant_type(
        &self,
        r: &Ref,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        let RefKind::MethodCall { invocant, invocant_span, .. } = &r.kind else {
            return None;
        };
        let invocant = match invocant {
            crate::model::conventions::Invocant::Bridged { token, match_mode, .. } => {
                return self
                    .resolve_bridged_class(
                        token,
                        *match_mode,
                        r.unqualified_target_name(),
                        module_index,
                    )
                    .map(InferredType::ClassName);
            }
            crate::model::conventions::Invocant::Name(n) => n,
        };
        if invocant.is_empty() {
            return None;
        }

        use crate::model::conventions::InvocantText;
        if matches!(
            invocant.classify(),
            InvocantText::PositionalReceiver | InvocantText::CurrentPackage
        ) {
            return self.enclosing_class_for_scope(r.scope).map(InferredType::ClassName);
        }

        // Flow-narrowing place invocant.
        if let Some(span) = invocant_span {
            if InvocantText::parse(invocant).is_element_place() {
                if let Some(t) = self.inferred_type_via_bag_ctx(invocant, span.start, module_index)
                {
                    if self.dispatch_class_of(&t, module_index).is_some() {
                        return Some(t);
                    }
                }
            }
        }

        // Function-call receiver (`create_user(5)->name`): the ref spans
        // only the function NAME, so the exact-span read below can't see
        // it — chase it here via `RefTable::call_at_start` (start-anchored,
        // contained in the invocant). A method-call receiver is left to
        // the exact-span read: its ref spans the whole receiver
        // expression, and the start-anchored index deliberately holds the
        // INNERMOST call at a point, which for a multi-hop chain is a
        // strict prefix of the receiver — the wrong hop.
        if let Some(span) = invocant_span {
            if let Some(recv_idx) = self.refs.call_at_start(&span.start) {
                let recv_span = self.refs[recv_idx].span;
                let contained = recv_span.start == span.start
                    && (recv_span.end.row, recv_span.end.column)
                        <= (span.end.row, span.end.column);
                let is_self = std::ptr::eq(&self.refs[recv_idx], r);
                if contained && !is_self {
                    if let RefKind::FunctionCall { .. } = &self.refs[recv_idx].kind {
                        if let Some(t) = self.sub_return_type_at_arity(
                            &self.refs[recv_idx].target_name,
                            Some(0),
                        ) {
                            if self.dispatch_class_of(&t, module_index).is_some() {
                                return Some(t);
                            }
                        }
                    }
                }
            }
        }

        // The invocant's type, resolved tree-free from the bag at its
        // exact span. Covers every recorded shape: scalar/array/hash
        // reads, baked literals, and chain receivers — the exact-span
        // call-ref arm inside `expr_type_at_span` reads the receiver
        // call's bag return with Parametric flavors intact, so DBIC
        // row-class narrowing survives the hop.
        if let Some(span) = invocant_span {
            if let Some(t) = self.expr_type_at_span(*span, module_index) {
                if self.dispatch_class_of(&t, module_index).is_some() {
                    return Some(t);
                }
            }
        }

        // Cross-file chain-receiver fallback. When the inner receiver's
        // class is only knowable once other modules load (`$c->minion->
        // enqueue` at enrichment), the build-time `Expr(span)` witness
        // is absent and `method_call_return_type_via_bag` has no edge to
        // chase. Re-resolve the receiver's own invocant class fresh with
        // the index, then chase `PackageSymbol{package, method}` through
        // `find_method_return_type` (ancestors + cross-file bridges via
        // the registry). This is the one structure-from-refs step the
        // bag can't pre-record, so it lives here, not in the builder.
        if let Some(span) = invocant_span {
            if let Some(recv_idx) = self.refs.call_at_start(&span.start) {
                let recv_span = self.refs[recv_idx].span;
                let contained = recv_span.start == span.start
                    && (recv_span.end.row, recv_span.end.column)
                        <= (span.end.row, span.end.column);
                let is_self = std::ptr::eq(&self.refs[recv_idx], r);
                if contained && !is_self {
                    if let RefKind::MethodCall { .. } = &self.refs[recv_idx].kind {
                        let recv = &self.refs[recv_idx];
                        if let Some(recv_class) =
                            self.method_call_invocant_class(recv, module_index)
                        {
                            let recv_method = recv.unqualified_target_name();
                            if crate::model::conventions::is_constructor_name(recv_method) {
                                return Some(InferredType::ClassName(recv_class));
                            }
                            if let Some(t) = self.find_method_return_type(
                                &recv_class,
                                recv_method,
                                module_index,
                                None,
                            ) {
                                return Some(t);
                            }
                        }
                        // A chain receiver's invocant text is the whole
                        // receiver expression, never a variable or
                        // bareword — the trailing rungs cannot answer it.
                        return None;
                    }
                }
            }
        }

        // Variable invocant. `expr_type_at_span` above only answers when
        // the builder pre-recorded an `Expr(span)` — which it can't for a
        // variable whose type flows from a cross-file source resolved
        // only at enrichment (`my $x = $c->helper`, `$$x` re-typed once
        // other modules load). Re-derive from the bag by the variable's
        // name + position, threading the index so the chase follows the
        // cross-file Variable edge. Same single bag query everything else
        // uses; only the var name (which lives on the ref, not the span)
        // brings us here instead of `expr_type_at_span`.
        let point = invocant_span.map(|s| s.start).unwrap_or(r.span.start);
        let first = invocant.as_bytes()[0];
        if first == b'$' || first == b'@' || first == b'%' {
            if let Some(t) = self.inferred_type_via_bag_ctx(invocant, point, module_index) {
                // A conventional invocant (`$self`/`$class`/...) whose bag
                // type carries no dispatch class still dispatches on the
                // enclosing class — identity outranks a classless value
                // type (`$self->{count}++` observations must not demote
                // `$self` to a plain hashref).
                if self.dispatch_class_of(&t, module_index).is_none()
                    && crate::model::conventions::is_conventional_invocant_name(invocant)
                {
                    return self.enclosing_class_for_scope(r.scope).map(InferredType::ClassName);
                }
                return Some(t);
            }
            // Enclosing-class fallback for an untyped conventional
            // invocant. Other untyped variable invocants stay None —
            // better than poisoning them with the surrounding package.
            if crate::model::conventions::is_conventional_invocant_name(invocant) {
                return self.enclosing_class_for_scope(r.scope).map(InferredType::ClassName);
            }
            return None;
        }

        // Bareword invocant. Could be a zero-arg sub returning ClassName
        // (`app->routes` where `app` is plugin-emitted); promote that.
        // Otherwise the bareword text *is* the class (`Foo->method`).
        let bare = split_qualified(invocant).1;
        if let Some(InferredType::ClassName(c)) = self.sub_return_type_at_arity(bare, Some(0)) {
            return Some(InferredType::ClassName(c));
        }
        Some(InferredType::ClassName(invocant.to_string()))
    }

    /// Walk the scope chain to find the enclosing class or package.
    pub(crate) fn enclosing_class_for_scope(&self, scope: ScopeId) -> Option<String> {
        for sid in self.scope_chain(scope).iter() {
            let s = self.scope(*sid);
            if let ScopeKind::Class { ref name } = s.kind {
                return Some(name.clone());
            }
            if let Some(ref pkg) = s.package {
                return Some(pkg.clone());
            }
        }
        None
    }

    /// The class of the method enclosing `point` — the implicit-`this` class
    /// for a bare member access in a method body. Read off the innermost
    /// containing Sub/Method SYMBOL's package (not the body scope): an
    /// out-of-line body (`Status DBImpl::Recover(...) { ... }`) is lexically at
    /// file scope, so its body scope carries no package, but the peeled method
    /// symbol does — reading it off the symbol covers in-class AND out-of-line
    /// with one rule (the same seam `emit_return_fuel`'s sibling-call pin uses).
    pub(crate) fn implicit_receiver_class_at(&self, point: Point) -> Option<String> {
        self.symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Method | SymKind::Sub))
            .filter(|s| contains_point(&s.span, point))
            .min_by_key(|s| span_size(&s.span))
            .and_then(|s| s.package.clone())
    }

    /// Is `name` a genuine LOCAL variable declaration (param or `Type x = …`)
    /// visible at `point` — as opposed to a bare implicit-`this` member? A
    /// member write (`prog_ = f()`, no declarator) mints flow witnesses but no
    /// Variable SYMBOL, so the presence of a scope-visible Variable symbol is
    /// the discriminator: a member never has one. Used to route receiver typing
    /// — a local trusts its (flow-narrowed) value; a member resolves on the
    /// enclosing class, dodging the phantom-local flow witnesses a member's
    /// reassignment leaves behind.
    pub(crate) fn has_local_variable_at(&self, name: &str, point: Point) -> bool {
        let Some(scope) = self.scope_at(point) else { return false };
        let chain = self.scope_chain(scope);
        self.symbols.iter().any(|s| {
            matches!(s.kind, SymKind::Variable) && s.name == name && chain.contains(&s.scope)
        })
    }

    /// Test-only wrapper over the private `resolve_invocant_class`.
    #[cfg(test)]
    pub(crate) fn resolve_invocant_class_test(
        &self,
        invocant: &str,
        scope: ScopeId,
        point: Point,
    ) -> Option<String> {
        self.resolve_invocant_class(invocant, scope, point)
    }

    /// Resolve an invocant string to a class name. Internal helper
    /// used by string-based completion / hover-context paths that
    /// don't have a `Ref` in hand. The ref-aware
    /// `method_call_invocant_class` is preferred everywhere a
    /// `&Ref` is available.
    pub(super) fn resolve_invocant_class(&self, invocant: &str, scope: ScopeId, point: Point) -> Option<String> {
        use crate::model::conventions::InvocantText;
        let enclosing = || {
            for scope_id in &self.scope_chain(scope) {
                let s = self.scope(*scope_id);
                if let ScopeKind::Class { ref name } = s.kind {
                    return Some(name.clone());
                }
                if let Some(ref pkg) = s.package {
                    return Some(pkg.clone());
                }
            }
            None
        };
        match InvocantText::parse(invocant) {
            InvocantText::CurrentPackage | InvocantText::PositionalReceiver => enclosing(),
            InvocantText::NonScalar(_) => None,
            InvocantText::Scalar(_) => {
                // Scalar invocant → infer type via the witness bag so
                // framework/branch/arity rules refine the answer.
                self.inferred_type_via_bag(invocant, point)
                    .and_then(|t| t.class_name().map(|s| s.to_string()))
                    .or_else(|| {
                        // Enclosing-class fallback only applies to
                        // conventional invocants — other variable invocants
                        // whose type we don't know stay None, not poisoned
                        // with the surrounding package. Otherwise `$r->to(...)`
                        // with `$r` un-typed would pretend `to` is a method on
                        // the enclosing package (MyApp), and goto-def on the
                        // method name lands on `package MyApp;`.
                        if crate::model::conventions::is_conventional_invocant_name(invocant) {
                            enclosing()
                        } else {
                            None
                        }
                    })
            }
            InvocantText::Bareword(_) => {
                // Bareword invocant: ambiguous between class-name and
                // zero-arg function call. If a sub by this name resolves
                // (locally or via a cross-file import) to a ClassName
                // return type when called with zero args, treat the
                // bareword as the call and use that class. Mirrors the
                // same rule in `invocant_type_at_node` and
                // `resolve_invocant_class_tree`.
                let bare = split_qualified(invocant).1;
                if let Some(InferredType::ClassName(c)) =
                    self.sub_return_type_at_arity(bare, Some(0))
                {
                    return Some(c);
                }
                Some(invocant.to_string())
            }
        }
    }

    /// Find a method definition within a class/package.
    #[cfg(test)]
    pub(crate) fn find_method_in_class(&self, class_name: &str, method_name: &str) -> Option<Span> {
        self.find_method_in_class_with_index(class_name, method_name, None)
    }

    /// Find a method definition, walking the inheritance chain if needed.
    #[cfg(test)]
    fn find_method_in_class_with_index(
        &self,
        class_name: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<Span> {
        match self.resolve_method_in_ancestors(class_name, method_name, module_index) {
            Some(MethodResolution::Local { sym_id, .. }) => {
                Some(self.symbol(sym_id).selection_span)
            }
            Some(MethodResolution::CrossFile { .. }) => {
                // Cross-file method found but no local span to return
                // Caller should handle cross-file resolution via ModuleIndex
                None
            }
            None => None,
        }
    }

    /// The plugin module whose BRIDGE is what resolves `class`->`name`
    /// — the helper's provider. `None` when a real def resolves first
    /// (local sub, inherited method, typeglob install) or when nothing
    /// resolves at all. The entrypoint-scan lint asks this to find
    /// helpers whose providing plugin no entrypoint loads.
    pub fn bridged_helper_provider(
        &self,
        class: &str,
        name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        let res = self.resolve_method_in_ancestors(class, name, module_index)?;
        let MethodResolution::CrossFile { class: on_class, def_module: Some(module) } = res
        else {
            return None;
        };
        // CrossFile{def_module: Some} covers typeglob installs too —
        // bridge-classify by the providing module's own declaration. The
        // provider may be a losing candidate of its own name slot — pick
        // the candidate that defines the sub.
        let idx = module_index?;
        let cached = idx
            .candidate_defining_sub_in_package(&module, &on_class, &name)
            .or_else(|| idx.get_cached(&module))?;
        let whole = idx.whole_present(&cached);
        let is_bridge = whole.plugin.namespaces.iter().any(|ns| {
            ns.bridges
                .iter()
                .any(|b| matches!(b, Bridge::Class(c) if c == &on_class))
                && ns.entities.iter().any(|sid| {
                    whole.symbols.get(sid.0 as usize).is_some_and(|s| {
                        matches!(s.kind, SymKind::Sub | SymKind::Method) && s.name == name
                    })
                })
        });
        is_bridge.then_some(module)
    }

    /// Is `pkg` a role? Single source of the property — consumers ask
    /// here, never re-derive from use lists. The verdict is baked at
    /// build time from an OPEN maker set (builder `ROLE_MAKERS` base
    /// engines ∪ plugin `role_makers()` manifests), so house role
    /// engines join via plugin declaration with no core change.
    pub fn is_role_package(&self, pkg: &str) -> bool {
        self.packages.get(pkg).is_some_and(|f| f.is_role)
    }

    /// Does `verb` take a column-keyed first hashref arg (DBIC `search`/`create`
    /// /…)? Plugin-declared (`column_keyed_verbs()`), baked at build time — the
    /// gate that links call-arg keys to the receiver class's columns.
    pub fn is_column_keyed_verb(&self, verb: &str) -> bool {
        self.column_keyed_verbs.contains(verb)
    }

    /// The composer-mismatch check (docs/adr/role-contracts.md): for
    /// each local package with role parents, every name in each
    /// transitively-composed role's `role_requires` must be PROVIDED —
    /// a real def anywhere in the composer's MRO (local sub, inherited
    /// method, `has` accessor, or a sibling role's def; a `requires`
    /// marker is a contract re-declaration, never a provision).
    ///
    /// Stays honest-silent when it can't know: roles defer their
    /// obligations to their eventual class composers, an unresolved
    /// ancestor may provide anything, and an AUTOLOAD anywhere in the
    /// MRO can satisfy any contract at runtime.
    pub fn unfulfilled_role_requires(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<UnfulfilledRequire> {
        // Requires list for a class that may live in this file or in
        // the index. `None` = not a role — the requires walk PRUNES
        // there (a base CLASS's composed roles were checked at its own
        // composition site), preserving the role-only edge semantics of
        // docs/adr/role-contracts.md.
        let role_requires_of = |c: &str| -> Option<Vec<String>> {
            let is_local = self
                .symbols
                .iter()
                .any(|s| matches!(s.kind, SymKind::Package | SymKind::Class) && s.name == c);
            if is_local {
                if !self.is_role_package(c) {
                    return None;
                }
                return Some(self.role_requires(c).to_vec());
            }
            // Role-ness and requires live in the packages lane (never
            // evicted) of whichever candidate file declares the role.
            module_index?
                .visible_def_candidates(c)
                .iter()
                .find(|cached| cached.analysis.is_role_package(c))
                .map(|cached| cached.analysis.role_requires(c).to_vec())
        };

        let mut out: Vec<UnfulfilledRequire> = Vec::new();
        let mut composers: Vec<&String> = self.packages.keys().collect();
        composers.sort();
        for pkg in composers {
            if self.is_role_package(pkg) {
                continue;
            }
            if self.class_has_unresolved_ancestor(pkg, module_index) {
                continue;
            }
            if self
                .resolve_method_in_ancestors(pkg, "AUTOLOAD", module_index)
                .is_some()
            {
                continue;
            }

            // Gather (name, declaring role, direct parent) over the
            // role-only reachable set from each direct parent: the
            // INHERITS walk (app-surface edge excluded — the synthetic
            // parent is never a role), pruned at every non-role node.
            // `walk` excludes its origin, so the direct parent's own
            // verdict is taken here.
            let graph = crate::model::graph::GraphView::new(self, module_index);
            let mut required: Vec<(String, String, String)> = Vec::new();
            for direct in self.declared_parents(pkg) {
                let Some(requires) = role_requires_of(direct) else { continue };
                for n in requires {
                    required.push((n, direct.clone(), direct.clone()));
                }
                graph.walk(
                    crate::model::graph::Node::Class(direct.clone()),
                    crate::model::graph::EdgeKindMask::INHERITS,
                    &mut |n| {
                        let crate::model::graph::Node::Class(c) = n else {
                            return crate::model::graph::WalkControl::PruneChildren;
                        };
                        match role_requires_of(c) {
                            Some(requires) => {
                                for name in requires {
                                    required.push((name, c.clone(), direct.clone()));
                                }
                                crate::model::graph::WalkControl::Continue
                            }
                            None => crate::model::graph::WalkControl::PruneChildren,
                        }
                    },
                );
            }

            let mut checked: HashSet<String> = HashSet::new();
            for (name, role, via_parent) in required {
                if !checked.insert(name.clone()) {
                    continue;
                }
                let mut provided = false;
                self.for_each_ancestor_class(pkg, module_index, |a| {
                    let here = self.class_provides_method(a, &name)
                        || module_index.is_some_and(|idx| {
                            idx.visible_def_candidates(a).iter().any(|c| {
                                idx.whole_present(c).provides_method_anywhere(&name)
                            })
                        })
                        || module_index.is_some_and(|idx| {
                            // Cross-package typeglob installs + plugin
                            // bridges (mirrors `method_resolution_on_class`
                            // arm c). The typeglob lookup rides the
                            // names index, which the markers feed too —
                            // re-check the home module package-attributed
                            // and contract-excluded, or every requires
                            // satisfies itself.
                            idx.module_declaring_method_in_package(&name, a)
                                .is_some_and(|home| {
                                    // The definer may be a losing candidate
                                    // of its own name slot.
                                    idx.visible_def_candidates(&home).iter().any(|c| {
                                        idx.whole_present(c).provides_method_in_package(&name, a)
                                    })
                                }) || {
                                let mut hit = false;
                                idx.for_each_entity_bridged_to(a, &mut |_m, _c, sym| {
                                    if !hit
                                        && matches!(sym.kind, SymKind::Sub | SymKind::Method)
                                        && sym.name == name
                                    {
                                        hit = true;
                                    }
                                });
                                hit
                            }
                        });
                    if here {
                        provided = true;
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                });
                if !provided {
                    out.push(UnfulfilledRequire {
                        package: pkg.clone(),
                        role,
                        name,
                        via_parent,
                    });
                }
            }
        }
        out
    }

    /// Does this file give class `cls` a REAL def of `name` — a local
    /// Sub/Method symbol (incl. `has` accessors and plugin-namespace
    /// entities bridged to `cls`) that is not a `requires` contract
    /// marker? The provision predicate for the composer-mismatch check;
    /// `method_resolution_on_class` stays marker-inclusive on purpose
    /// (in-role `$self->name` dispatch lands on the contract).
    fn class_provides_method(&self, cls: &str, name: &str) -> bool {
        for &sid in self.symbols_named(name) {
            let sym = self.symbol(sid);
            if matches!(sym.kind, SymKind::Sub | SymKind::Method)
                && self.symbol_in_class(sid, cls)
                && !self.contract_symbols.contains(&sid)
            {
                return true;
            }
        }
        self.plugin.namespaces.iter().any(|ns| {
            ns.bridges.iter().any(|b| matches!(b, Bridge::Class(c) if c == cls))
                && ns.entities.iter().any(|sid| {
                    self.symbols.get(sid.0 as usize).is_some_and(|s| {
                        matches!(s.kind, SymKind::Sub | SymKind::Method) && s.name == name
                    })
                })
        })
    }

    /// Any non-contract Sub/Method named `name` in this file,
    /// regardless of package attribution — the name-only flavor of
    /// `class_provides_method`, excluding `requires` markers. The
    /// distinction is load-bearing for the default-implementation
    /// pattern: a role that both requires AND defines a name must
    /// count as providing it.
    pub fn provides_method_anywhere(&self, name: &str) -> bool {
        self.symbols.iter().enumerate().any(|(i, s)| {
            s.name == name
                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                && !self.contract_symbols.contains(&SymbolId(i as u32))
        })
    }

    /// `provides_method_anywhere` narrowed to symbols attributed to
    /// `package` — the contract-excluded sibling of
    /// `has_sub_in_package`, for the typeglob-install provision arm.
    pub fn provides_method_in_package(&self, name: &str, package: &str) -> bool {
        self.symbols.iter().enumerate().any(|(i, s)| {
            s.name == name
                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                && s.package.as_deref() == Some(package)
                && !self.contract_symbols.contains(&SymbolId(i as u32))
        })
    }

    /// Collect every Handler visible from `class_name` whose `dispatchers`
    /// list contains `dispatcher`. Walks the inheritance chain and at
    /// each level pulls Handlers from (1) this file's symbols with
    /// matching owner, (2) this file's `plugin_namespaces` bridged to
    /// the current class, and (3) cross-file via
    /// `module_index.for_each_entity_bridged_to` — rule #8.
    ///
    /// Shares `for_each_ancestor_class` with `resolve_method_in_ancestors`
    /// so the two dispatch paths can't drift on MRO rules. Both
    /// `dispatch_target_completions` and `class_has_dispatch_handlers`
    /// in `symbols.rs` funnel through here.
    ///
    /// `visit(symbol, provenance)` — provenance is `"this file"` for
    /// local hits and the cached module's filename for cross-file
    /// hits (matches the existing detail-string contract).
    pub fn for_each_dispatch_handler_on_class(
        &self,
        class_name: &str,
        dispatcher: &str,
        module_index: Option<&dyn CrossFileLookup>,
        mut visit: impl FnMut(&Symbol, &str),
    ) {
        let disp_matches = |dd: &[String]| dd.iter().any(|d| d == dispatcher);
        self.for_each_ancestor_class(class_name, module_index, |cls| {
            // (1) Local Handler symbols owned by this class.
            for sym in &self.symbols {
                if let SymbolDetail::Handler { owner, dispatchers, .. } = &sym.detail {
                    let HandlerOwner::Class(n) = owner;
                    if n == cls && disp_matches(dispatchers) {
                        visit(sym, "this file");
                    }
                }
            }
            // (2) Local plugin-namespace bridge to `cls`.
            for ns in &self.plugin.namespaces {
                if !ns.bridges.iter().any(|b| matches!(b, Bridge::Class(c) if c == cls)) { continue; }
                for sym_id in &ns.entities {
                    let Some(sym) = self.symbols.get(sym_id.0 as usize) else { continue };
                    if let SymbolDetail::Handler { dispatchers, .. } = &sym.detail {
                        if disp_matches(dispatchers) {
                            visit(sym, "this file");
                        }
                    }
                }
            }
            // (3) Cross-file plugin-namespace bridge.
            if let Some(idx) = module_index {
                idx.for_each_entity_bridged_to(cls, &mut |_mod, cached, sym| {
                    if let SymbolDetail::Handler { dispatchers, .. } = &sym.detail {
                        if disp_matches(dispatchers) {
                            let prov = cached.path.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("cross-file");
                            visit(sym, prov);
                        }
                    }
                });
            }
            std::ops::ControlFlow::Continue(())
        });
    }

    /// Check if a symbol is defined within a class/package.
    pub(crate) fn symbol_in_class(&self, sym_id: SymbolId, class_name: &str) -> bool {
        let sym = self.symbol(sym_id);
        // Fast path: check the package captured at symbol creation time.
        // This is authoritative for multi-package files where the scope's
        // mutable `package` field gets overwritten by later package statements.
        if let Some(ref pkg) = sym.package {
            return pkg == class_name;
        }
        // Fallback: walk the scope chain for symbols without a package field.
        let start_scope = self.scope_at(sym.span.start).unwrap_or(sym.scope);
        let chain = self.scope_chain(start_scope);
        for scope_id in &chain {
            let s = self.scope(*scope_id);
            if let ScopeKind::Class { ref name } = s.kind {
                return name == class_name;
            }
            if let Some(ref pkg) = s.package {
                return pkg == class_name;
            }
        }
        false
    }

    /// Is `sym` a class's OWN content — a member declared in the class body
    /// (struct field, member-block-macro role member) or an enum constant
    /// (which leaks into the scope that contains the enum) — as opposed to a
    /// lexical local that merely inherited the class as sticky context? A
    /// pack-language local declared inside an inline method carries the class
    /// in `package` too, so the package tag alone cannot distinguish them;
    /// the structure of the declaring scope relative to the class span can:
    ///
    /// * enum constant — its scope CONTAINS the whole class (enum) span;
    /// * direct member — its scope sits inside the class span but its parent
    ///   does not (the class body / a role-macro member region, whose scope
    ///   has no parent at all);
    /// * lexical local — its scope's parent is still inside the class span
    ///   (a function body), so it fails both tests.
    ///
    /// This is the backward (def→uses) gate mirroring how a USE reaches the
    /// def forward: members via the ancestor walk, enum constants via the
    /// bare-name cross-file lookup. Perl symbols never qualify (variables
    /// and Corinna fields carry sigils; callables aren't Variable/Field).
    pub(crate) fn symbol_is_class_content(&self, sym: &Symbol) -> bool {
        if !matches!(sym.kind, SymKind::Variable | SymKind::Field | SymKind::Enumerator) {
            return false;
        }
        if sym.name.starts_with(['$', '@', '%']) {
            return false;
        }
        let Some(pkg) = sym.package.as_deref() else { return false };
        let sc = self.scope(sym.scope);
        // Role-macro member region: `inject_member_blocks` mints a synthetic
        // PARENTLESS non-file scope per member-block macro, tagged with the
        // role as its package — the walk itself never produces a parentless
        // block (everything nests under the File root). The role's Class
        // span can sit on an ALIAS `#define` far from the member tokens
        // (perl5's `#define BASEOP BASEOP_DEFINITION`), so span containment
        // cannot be required here.
        if sc.parent.is_none()
            && !matches!(sc.kind, ScopeKind::File)
            && sc.package.as_deref() == Some(pkg)
        {
            return true;
        }
        let contains = |o: &Span, i: &Span| {
            (o.start.row, o.start.column) <= (i.start.row, i.start.column)
                && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
        };
        // The owner container is a Class (struct/union/enum) OR a namespace
        // (`SymKind::Package`): a namespace-scope global (`ns::kBits`) is its
        // namespace's content the same way a field is its class's — the Sub-
        // scope walk below still excludes a sub-body local that merely carries
        // the enclosing namespace as sticky package.
        let Some(class_span) = self
            .symbols_named(pkg)
            .iter()
            .map(|&sid| self.symbol(sid))
            .filter(|c| {
                matches!(c.kind, SymKind::Class | SymKind::Package)
                    && contains(&c.span, &sym.span)
            })
            .map(|c| c.span)
            .next()
        else {
            return false;
        };
        // Enum-constant leak: declared in the scope that contains the enum.
        if contains(&sc.span, &class_span) {
            return true;
        }
        // Member vs method-body local: walk the chain outward. Exiting the
        // class span first = class content (the class body, or a nested
        // container body — an inline union's members are still the class's);
        // crossing a Sub/Method scope first = a local inside a method (the
        // sticky class package tags those too, so the package alone would
        // over-claim). A chain that ends inside (parentless synthetic
        // scopes) was already handled by the role-member check above.
        let mut cur = Some(sym.scope);
        while let Some(id) = cur {
            let s = self.scope(id);
            if !contains(&class_span, &s.span) {
                return true;
            }
            if matches!(s.kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. }) {
                return false;
            }
            cur = s.parent;
        }
        true
    }

    /// Among class-content symbols, is `sym` an ENUM-CONSTANT shape — a name
    /// C injects into the ENCLOSING scope, so a bare unresolved read
    /// (`case OP_SCOPE:`) is a legitimate use? Struct fields, methods, and
    /// member-block role members are receiver-reached (`o->op_type`); a bare
    /// same-named token elsewhere is NOT a use of them (the `format`
    /// 1621-hit sweep). Structurally: the constant's declaring scope
    /// CONTAINS its enum's Class span (the hoist); a member's scope sits
    /// inside the class span.
    pub(crate) fn class_content_is_bare_constant(&self, sym: &Symbol) -> bool {
        if !self.symbol_is_class_content(sym) {
            return false;
        }
        let sc = self.scope(sym.scope);
        // Role-macro member region (parentless synthetic scope) — a struct
        // body, never bare-reachable.
        if sc.parent.is_none() && !matches!(sc.kind, ScopeKind::File) {
            return false;
        }
        let Some(pkg) = sym.package.as_deref() else { return false };
        let contains = |o: &Span, i: &Span| {
            (o.start.row, o.start.column) <= (i.start.row, i.start.column)
                && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
        };
        self.symbols_named(pkg)
            .iter()
            .map(|&sid| self.symbol(sid))
            .any(|c| {
                matches!(c.kind, SymKind::Class)
                    && contains(&c.span, &sym.span)
                    && contains(&sc.span, &c.span)
            })
    }

    /// The innermost namespace (`Package` symbol) whose span contains `span`
    /// — the def-side namespace fact recovered POSITIONALLY, so it stays
    /// correct where the walk's sticky context desynced (macro-guarded
    /// namespace opens, mid-file scope desync). Callers gate on pack-shaped
    /// analyses; Perl attribution is total at build time.
    ///
    /// **Not usable on Perl.** It needs a `Package` SYMBOL whose span
    /// CONTAINS the point, which is a brace-delimited pack namespace. Perl's
    /// `package Foo;` spans one statement and contains nothing, so this
    /// answers `None` for essentially every point in a Perl file (10,435 of
    /// 10,436 imports on the substrate). `package_at` is the Perl accessor —
    /// it reads `package_ranges`, which models "in effect from here".
    pub(crate) fn enclosing_package_of(&self, span: &Span) -> Option<String> {
        let contains = |o: &Span, i: &Span| {
            (o.start.row, o.start.column) <= (i.start.row, i.start.column)
                && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
                && !(o.start == i.start && o.end == i.end)
        };
        self.symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Package) && contains(&s.span, span))
            .max_by_key(|s| (s.span.start.row, s.span.start.column))
            .map(|s| s.name.clone())
    }

    /// Is `sym` a file-scope value a bare name reaches from anywhere — a C
    /// global, an object-like `#define`'s symbol, an anonymous-enum constant?
    /// Package-less + sigil-less + declared in the root scope. The backward
    /// mirror of the generic by-name cross-file goto-def tail. Perl never
    /// qualifies: its variables carry sigils, its callables aren't Variable.
    pub(crate) fn symbol_is_file_scope_value(&self, sym: &Symbol) -> bool {
        // `ScopeKind::File` is the SAME gate `register_symbols` keys the
        // by-name cross-file index on — forward and backward share the key.
        // `Enumerator` covers an anonymous-enum constant (no enclosing
        // named scope of its own; it leaks straight to file scope).
        matches!(sym.kind, SymKind::Variable | SymKind::Enumerator)
            && sym.package.is_none()
            && !sym.name.starts_with(['$', '@', '%'])
            && matches!(self.scope(sym.scope).kind, ScopeKind::File)
    }

    /// Does `name` name a `#define` in this file? (`selection_span` match
    /// optional: `at` narrows to "this exact def site".)
    pub(crate) fn names_macro_def(&self, name: &str, at: Option<Span>) -> bool {
        self.pack.macro_defs
            .iter()
            .any(|m| m.name == name && at.is_none_or(|s| m.selection_span == s))
    }

    /// Find the definition span of a package or class by name.
    pub(super) fn find_package_or_class(&self, name: &str) -> Option<Span> {
        for &sid in self.symbols_named(name) {
            let sym = self.symbol(sid);
            if matches!(sym.kind, SymKind::Package | SymKind::Class) {
                return Some(sym.selection_span);
            }
        }
        None
    }

}
