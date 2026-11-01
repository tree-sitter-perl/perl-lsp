//! Cross-file enrichment and dispatch application: imported-type propagation,
//! gated emissions, dispatch/guard resolution, enrichment indices.

use super::*;

impl FileAnalysis {
    /// Resolve call bindings for imported functions: return-type TCs plus
    /// hash-key owner stamps on their accesses (the producer's real
    /// HashKeyDef is the single source — no consumer-side stub). Walks
    /// `self.imports` against `module_index` for exactly the names this
    /// file's `call_bindings` reference, reaching cross-file `Symbol(_)`
    /// witnesses through `BagContext.module_index` directly. Call after
    /// building, when the module index is available.
    /// The enrichment half of loader-config param typing
    /// (`prompt-long-distance.md`): for each `from_loader_config`
    /// marker, gather the config-arg shapes from every caller's
    /// `PluginLoad` facts and push the agreed type as a TC. Honesty
    /// gate: callers here are ENUMERABLE BY CONSTRUCTION (the loader
    /// facts name this module); shapes that disagree fold to an OPEN
    /// key union rather than a guess; zero matching callers pushes
    /// nothing (the static `type_class` fallback already rode the
    /// gated path at build).
    fn apply_loader_config_params(&mut self, module_index: Option<&dyn CrossFileLookup>) {
        if self.loader_config_params.is_empty() {
            return;
        }
        let Some(idx) = module_index else { return };
        // every package name this file declares, for FQ + tail matching
        let my_packages: Vec<String> = self
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Package | SymKind::Class))
            .map(|s| s.name.clone())
            .collect();
        let matches_me = |load_name: &str| -> bool {
            my_packages.iter().any(|p| {
                p == load_name || p.rsplit("::").next() == Some(load_name)
            })
        };

        let markers = self.loader_config_params.clone();
        for m in &markers {
            // re-gate: the marker's package must still isa the
            // declaring role/class (same condition the static gated
            // path checks at query time)
            let pkg = self.scopes.get(m.scope.0 as usize).and_then(|sc| sc.package.clone());
            let Some(pkg) = pkg else { continue };
            if !self.class_isa(&pkg, &m.in_role, module_index) {
                continue;
            }
            let mut shapes: Vec<InferredType> = Vec::new();
            idx.for_each_loader_shape(&mut |load_name, t| {
                if matches_me(load_name) {
                    shapes.push(t.clone());
                }
            });
            if shapes.is_empty() {
                continue;
            }
            // Every caller's shape, joined: keyed hashes keep every key any
            // caller passes. Callers that pass no one shape type nothing.
            let agreed = match crate::model::file_analysis::join_all(&shapes, &self.type_order(module_index)) {
                Some(InferredType::Union(_) | InferredType::Unknown) | None => continue,
                Some(t) => t,
            };
            // Anchored at the parameter's binding token (a write marker at
            // its declaration retires what lies strictly before it), else
            // the scope's start.
            let at = self
                .binding_site_of(&m.variable, m.scope)
                .or_else(|| self.scopes.get(m.scope.0 as usize).map(|sc| sc.span.start))
                .unwrap_or(Point { row: 0, column: 0 });
            let span = Span { start: at, end: at };
            self.push_type_constraint(TypeConstraint {
                variable: m.variable.clone(),
                scope: m.scope,
                constraint_span: span,
                inferred_type: agreed,
            });
        }
    }

    /// Which of this file's [`GatedEmission`]s fire under `module_index`,
    /// one verdict per emission: each asks its [`Question`], so a file read
    /// late converges to one built with its ancestry known. Emissions
    /// asking the same question (one DBIC class's `add_columns` +
    /// relationships) ask it once.
    pub fn gate_verdicts(&self, module_index: Option<&dyn CrossFileLookup>) -> Vec<bool> {
        let facts = InFile { fa: self, index: module_index };
        let mut asked: Vec<(Question, bool)> = Vec::new();
        self.plugin
            .gated_emissions
            .iter()
            .map(|em| {
                let question = em.question();
                if let Some((_, fires)) = asked.iter().find(|(q, _)| *q == question) {
                    return *fires;
                }
                let fires = question.decide(&facts) == Verdict::Yes;
                asked.push((question, fires));
                fires
            })
            .collect()
    }

    /// Every gated emission's content as if each gate fired. The producer
    /// feeds (ref/sym rows, the reverse index, bridges) index these as
    /// CANDIDATES; whether a gate holds is asked where a copy is handed to a
    /// reader, so a candidate the gate refuses costs a wasted decode, never
    /// a wrong answer.
    pub fn gated_candidates(&self) -> GatedMint {
        self.mint_gated(|_| true)
    }

    /// The symbols a listing adds to this file under `verdicts`, with no
    /// view built. A symbol-evicted copy cannot dedup them against its own
    /// symbols; a listing dedups its results.
    pub fn shown_gated_symbols(&self, verdicts: &[bool]) -> Vec<Symbol> {
        self.mint_gated(|i| verdicts.get(i).copied().unwrap_or(false)).symbols
    }

    /// Every name a gated emission could add to this file, as the rows key
    /// them: its own presence rows (`held`), so a retrieval that must not
    /// miss a view finds the file without the base rows claiming the names.
    pub fn held_row_names(&self) -> Vec<String> {
        if self.plugin.gated_emissions.is_empty() {
            return Vec::new();
        }
        let names = &self.pack.names;
        let mint = self.gated_candidates();
        let mut out: Vec<String> = mint.refs.iter().map(|r| r.match_key(names)).collect();
        for s in &mint.symbols {
            out.push(s.name.clone());
            out.push(crate::model::conventions::name_match_key(&s.name, names));
        }
        out.sort();
        out.dedup();
        out
    }

    /// Everything the relational rows store of this file.
    pub fn row_seeds(&self) -> RowSeeds {
        RowSeeds { refs: self.ref_row_seeds(), syms: self.sym_row_seeds(), held: self.held_row_names() }
    }

    /// A copy with the emissions `verdicts` fires applied: the view of a
    /// gated file. The applied content sits above the enrichment baselines,
    /// so a later enrichment of the copy truncates and re-applies it.
    pub fn with_gated_emissions(&self, verdicts: &[bool]) -> FileAnalysis {
        let mut copy = self.clone();
        copy.apply_gated_emissions(verdicts);
        copy.rebuild_enrichment_indices();
        copy
    }

    fn apply_gated_emissions(&mut self, verdicts: &[bool]) {
        let mint = self.mint_gated(|i| verdicts.get(i).copied().unwrap_or(false));
        for sym in mint.symbols {
            self.symbols.push(sym);
        }
        for r in mint.refs {
            self.refs.push(r);
        }
        for w in mint.witnesses {
            self.witnesses.push(w);
        }
    }

    /// The symbols, refs and witnesses the emissions `fires` selects add to
    /// this file, minted as the builder would have: positional `SymbolId`s
    /// after the current symbols, and never a second copy of a symbol
    /// already present (the builder's `apply_emit_action` dedup).
    fn mint_gated(&self, fires: impl Fn(usize) -> bool) -> GatedMint {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let mut mint = GatedMint::default();
        for (i, em) in self.plugin.gated_emissions.iter().enumerate() {
            if !fires(i) {
                continue;
            }
            let scope = self.scope_at(em.scope_point).unwrap_or(ScopeId(0));
            let ns = Namespace::framework(em.plugin_id.clone());
            for gs in &em.symbols {
                let pkg = gs.on_class.clone().or_else(|| Some(em.package.clone()));
                let same = |s: &Symbol| {
                    s.name == gs.name && s.kind == gs.kind && s.package == pkg && s.namespace == ns
                };
                if self.symbols.iter().any(same) || mint.symbols.iter().any(same) {
                    continue;
                }
                let id = SymbolId((self.symbols.len() + mint.symbols.len()) as u32);
                mint.symbols.push(Symbol {
                    id,
                    name: gs.name.clone(),
                    kind: gs.kind,
                    span: gs.span,
                    selection_span: gs.selection_span,
                    scope,
                    package: pkg.clone(),
                    detail: gs.detail.clone(),
                    namespace: ns.clone(),
                    presentation: gs.presentation.clone(),
                    attributes: Vec::new(),
                    flags: Default::default(),
                    declared_with: None,
                    deref_stack: Vec::new(),
                    arity: None,
                });
                if let Some(rt) = &gs.return_type {
                    // Plugin-priority `Symbol(sid) → InferredType`, matching
                    // the builder's Method emit, plus the
                    // `PackageSymbol{package,name}` mirror the fold writeback
                    // would have pushed so cross-file return-type queries
                    // resolve the relationship shape.
                    mint.witnesses.push(Witness {
                        attachment: WitnessAttachment::Symbol(id),
                        source: WitnessSource::Plugin(em.plugin_id.clone()),
                        payload: WitnessPayload::InferredType(rt.clone()),
                        span: gs.span,
                    });
                    if matches!(gs.kind, SymKind::Method | SymKind::Sub) {
                        if let Some(class) = &pkg {
                            mint.witnesses.push(Witness {
                                attachment: WitnessAttachment::PackageSymbol {
                                    package: class.clone(),
                                    name: gs.name.clone(),
                                },
                                source: WitnessSource::Plugin(em.plugin_id.clone()),
                                payload: WitnessPayload::Edge(WitnessAttachment::Symbol(id)),
                                span: gs.span,
                            });
                        }
                    }
                }
            }
            for gr in &em.refs {
                mint.refs.push(Ref {
                    kind: gr.kind.clone(),
                    span: gr.span,
                    scope,
                    target_name: gr.target_name.clone(),
                    access: gr.access,
                    binding: gr.binding.clone(),
                    folded_from: None,
                    arg_count: None,
                });
            }
        }
        mint
    }

}

/// Score every re-stamp the gate skipped, by running it anyway and comparing.
///
/// The gate's soundness is inherited, not proved: it is exactly as sound as
/// the freshness edges that feed `dirty_consumers`, and a provider whose change
/// never reaches them marks nobody. A wrong skip is silent — the frozen
/// `MethodTarget` simply stays as it was, and goto-def keeps answering it — so
/// the assumption ships with the switch that checks it, the same discipline as
/// `PERL_LSP_CONCL_EQUIV` and `PERL_LSP_FLUSH_EQUIV`.
///
/// Costs strictly more than not gating at all, by design. It is a measurement
/// mode, not a safety net for production.
fn restamp_gate_equiv() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("PERL_LSP_RESTAMP_EQUIV").is_ok())
}

impl FileAnalysis {
    /// Run the skipped re-stamp and report whether it would have changed an
    /// answer. `PERL_LSP_RESTAMP_EQUIV` only.
    fn score_restamp_gate_skip(
        &mut self,
        module_index: Option<&dyn CrossFileLookup>,
        path: Option<&std::path::Path>,
    ) {
        let arms = |r: &Ref| r.frozen_receiver().map(|f| f.targets().to_vec());
        let before: Vec<Option<Vec<MethodTarget>>> = self.refs.iter().map(arms).collect();
        self.stamp_method_call_targets(module_index);
        let diverged = self
            .refs
            .iter()
            .map(arms)
            .zip(before.iter())
            .filter(|(now, was)| now != *was)
            .count();
        if diverged > 0 {
            crate::util::ghost_stats::count_by("restampequiv.break", diverged as u64);
            log::warn!(
                "restamp equiv: the gate skipped {} target(s) that WOULD have \
                 changed in {:?} — a provider moved without marking this file, \
                 so the freshness edge that should have covered it does not",
                diverged,
                path
            );
        } else {
            crate::util::ghost_stats::count("restampequiv.agreed");
        }
    }

    /// Enrich without saying which file this is.
    ///
    /// The re-stamp gate needs a path — a `FileAnalysis` does not know its own
    /// — so this spelling always fails the gate open and re-stamps, which is
    /// the behavior every caller had before the gate existed. Production
    /// enrichment writers, which do hold the path, call
    /// `enrich_imported_types_with_keys_for`.
    pub fn enrich_imported_types_with_keys(
        &mut self,
        module_index: Option<&dyn CrossFileLookup>,
    ) {
        self.enrich_imported_types_with_keys_for(module_index, None)
    }

    /// Enrich as `path`, so the re-stamp gate can ask whether any provider of
    /// this file has moved since it last stamped.
    pub fn enrich_imported_types_with_keys_for(
        &mut self,
        module_index: Option<&dyn CrossFileLookup>,
        path: Option<&std::path::Path>,
    ) {
        crate::util::ghost_stats::count("enrich_imported_types_with_keys");
        // Enrichment is a walk (`docs/adr/resolution-session.md`): every
        // caller gets the candidate memo and the budget, nested callers ride
        // the outer session.
        let _session = crate::model::witnesses::ResolutionSession::enter(module_index);
        // Truncate back to baseline so repeated enrichment doesn't
        // accumulate duplicates. Enrichment pushes Variable witnesses
        // via `push_type_constraint` and synthetic symbols + witnesses
        // for imported-hash-key completion.
        self.symbols.truncate_to_baseline();
        self.witnesses.truncate(self.base_witness_count);
        self.refs.truncate_to_baseline();

        // Dispatch promotion is NOT done here: gated candidates resolve at
        // query time (`applicable_dispatches`), so a `$minion->enqueue('T')`
        // surfaces by the receiver's type whether or not its file is open.
        // See `docs/adr/receiver-gated-dispatch.md`.

        // Re-fire plugin pattern emissions whose `ClassIsa` trigger the
        // build couldn't confirm against LOCAL ancestry (DBIC result classes
        // reaching `DBIx::Class` through a cross-file intermediate base).
        // Runs first so the synthesized symbols exist for the rest of the
        // pass and the final `rebuild_enrichment_indices`. See `GatedEmission`.
        let verdicts = self.gate_verdicts(module_index);
        self.apply_gated_emissions(&verdicts);

        // Loader-config param typing: join my `loader_config_params`
        // markers with caller-side `PluginLoad` facts across the index.
        self.apply_loader_config_params(module_index);

        // Build the import → exported-name map inline from
        // `self.imports` + `module_index`. `imported_hash_keys` is
        // the only piece still needed by enrichment; imported return
        // types are reached lazily by `query_sub_return_type` walking
        // `module_index.find_exporters(name)`.
        // func name → (producer package, keys). The producer package is the
        // load-bearing piece: a consumer's `$cfg->{host}` access must carry the
        // SAME owner the producer's `host` HashKeyDef does
        // (`Sub{Some("Cfg"), get_config}`), or cross-file rename/references
        // never link the two. `None` here is a lossy projection of a package
        // the index already knows.
        let mut imported_hash_keys: HashMap<String, (Option<String>, Vec<String>)> = HashMap::new();
        let mut imported_returns: HashMap<String, InferredType> = HashMap::new();
        // NEED-driven: both maps are consumed only through `call_bindings`
        // lookups (the return-TC push and the hash-key owner fixup below), so
        // only names this file actually binds are worth querying. Scanning
        // every exported symbol instead runs a cross-file registry chase per
        // export — and each raw-bag miss force-builds the exporter's enriched
        // overlay, whose own enrichment recurses into ITS imports: one warm
        // didOpen cascaded into deep-copying + enriching the entire dep
        // closure (~340 overlay builds, ~146k overlay consults on crm) for
        // answers nothing consumed.
        let needed_names: std::collections::HashSet<&str> = self
            .call_bindings
            .iter()
            .flat_map(|b| {
                [b.func_name.as_str(), split_qualified(&b.func_name, self.names()).1]
            })
            .collect();
        // A file with no call bindings needs no provider walk at all.
        // Does an IMPORT ever collide with an ancestor's method of the
        // same name? That is the one case where folding imports into the
        // package-symbol chase would be actively WRONG rather than merely
        // generous: Perl resolves a name in the package's OWN stash — which
        // is where `import` aliased it — before consulting @ISA, for plain
        // calls and method calls alike. A chase that reaches the ancestor
        // first returns the wrong sub for code that runs correctly.
        if crate::util::ghost_stats::probe("shadow") {
            crate::util::ghost_stats::count("shadow.file");
            crate::util::ghost_stats::add_n(
                "shadow.imports_len", self.imports.len() as u64);
            for import in &self.imports {
                crate::util::ghost_stats::count("shadow.import_seen");
                // `package_at`, not `enclosing_package_of`: the latter
                // wants a Package SYMBOL whose span contains the point, which
                // is a brace-delimited pack namespace. Perl's `package Foo;`
                // symbol spans one statement and contains nothing, so it
                // attributes 10,435 of 10,436 imports to nobody.
                let Some(pkg) = self.package_at(import.span.start).map(str::to_string) else {
                    crate::util::ghost_stats::count("shadow.no_enclosing_package");
                    continue;
                };
                if import.imported_symbols.is_empty() {
                    // Bare `use M;` — the names live in M's @EXPORT, so the
                    // collision set is not enumerable from this file alone.
                    crate::util::ghost_stats::count("shadow.bare_use_unknowable");
                    continue;
                }
                for sym in &import.imported_symbols {
                    crate::util::ghost_stats::count("shadow.imported_name");
                    match self.resolve_method_in_ancestors(
                        &pkg, &sym.local_name, module_index)
                    {
                        Some(MethodResolution::Local { class, .. })
                        | Some(MethodResolution::CrossFile { class, .. })
                            if class != pkg =>
                        {
                            crate::util::ghost_stats::count("shadow.COLLIDES");
                            crate::util::ghost_stats::count_distinct(
                                "shadow.collision_shapes",
                                &format!("{pkg}|{}|{class}", sym.local_name),
                            );
                        }
                        _ => crate::util::ghost_stats::count("shadow.clear"),
                    }
                }
            }
        }
        if let Some(idx) = module_index.filter(|_| !needed_names.is_empty()) {
            let _chase = crate::util::ghost_stats::ScopedNs::start("chase.total");
            let _attrib = crate::util::ghost_stats::Attribute::start("chase");
            crate::util::ghost_stats::count("chase.file");
            for import in &self.imports {
                crate::util::ghost_stats::count("chase.import");
                crate::util::ghost_stats::count_distinct(
                    "chase.import", &import.module_name);
                // A split exporter's subs live across its candidate files.
                for cached in idx.visible_def_candidates(&import.module_name) {
                    crate::util::ghost_stats::count("chase.candidate");
                    crate::util::ghost_stats::count_distinct(
                        "chase.candidate", &cached.path.display().to_string());
                // Return-shape reads go through the bag — the resident index
                // copy may be bag-evicted (workspace tier included), so take
                // the bag-present view for the whole scan.
                // The scan below only does work for a symbol that is BOTH
                // needed here and exported there, so a candidate exporting
                // none of `needed_names` contributes nothing — and the
                // expensive part of finding that out is fetching a view.
                //
                // `any_package_exports` (@EXPORT u @EXPORT_OK) is not an evictable
                // axis and is rebuilt by `build_indices` on every copy, so this
                // reads the RESIDENT analysis: no rehydrate, no decode, no LRU
                // traffic. Measured on the substrate: it drops the per-candidate
                // bag fetch from 7,829 to 332 (95.8% of them were for providers
                // that matched nothing). Do not "simplify" this to a
                // symbols-axis probe — `symbols_present` rehydrates too, and
                // doing that made the chase SLOWER, not faster.
                if !needed_names.iter().any(|n| cached.analysis.any_package_exports(n)) {
                    crate::util::ghost_stats::count("chase.candidate_skipped");
                    continue;
                }
                crate::util::ghost_stats::count_distinct(
                    "chase.fetched", &cached.path.display().to_string());
                let whole = crate::util::ghost_stats::timed(
                    "chase.bag_present", || idx.bag_present(&cached));
                crate::util::ghost_stats::count("chase.fetched");
                crate::util::ghost_stats::add_n(
                    "chase.witnesses_rehydrated", whole.witnesses.len() as u64);
                crate::util::ghost_stats::add_n(
                    "chase.symbols_rehydrated", whole.symbols.len() as u64);
                // Fetched on the first return-type MISS (fallback-on-miss): the
                // exporter's own return may chain through ITS imports (A→B→C),
                // materialized only after the exporter is itself enriched — the
                // transitive-enrichment case. `enriched_present` is ENRICHING-
                // guarded, so a mutual A↔B import declines to the raw bag rather
                // than looping, and the tainted copy is never cached.
                let mut enriched: Option<std::sync::Arc<FileAnalysis>> = None;
                for sym in &whole.symbols {
                    if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                        continue;
                    }
                    if !needed_names.contains(sym.name.as_str()) {
                        continue;
                    }
                    if !whole.symbol_is_exported(sym) {
                        continue;
                    }
                    crate::util::ghost_stats::count("chase.sym_matched");
                    if matches!(sym.detail, SymbolDetail::Sub { .. }) {
                        crate::util::ghost_stats::count("chase.return_query");
                        let mut ty = crate::util::ghost_stats::timed(
                            "chase.symbol_return_type_via_bag",
                            || whole.symbol_return_type_via_bag(sym.id, None),
                        );
                        if ty.is_none() {
                            crate::util::ghost_stats::count("chase.return_miss");
                            if idx.serves_enriched() {
                                let en = enriched.get_or_insert_with(|| {
                                    crate::util::ghost_stats::count("chase.enriched_present");
                                    crate::util::ghost_stats::timed(
                                        "chase.enriched_present",
                                        || idx.enriched_present(&cached),
                                    )
                                });
                                if !std::sync::Arc::ptr_eq(en, &whole) {
                                    ty = en.symbol_return_type_via_bag(sym.id, None);
                                }
                            }
                        }
                        if let Some(ty) = ty {
                            imported_returns.insert(sym.name.clone(), ty);
                        }
                    }
                    if let Some(sub_info) = whole.sub_info_view(&sym.name) {
                        let hk = sub_info.hash_keys();
                        if !hk.is_empty() {
                            imported_hash_keys
                                .insert(sym.name.clone(), (sym.package.clone(), hk.to_vec()));
                        }
                    }
                }
                }
            }
        }

        // Push call-binding TCs for imports whose return type the
        // cross-file scan resolved. Same shape the local-sub path
        // produces (`propagate_call_bindings_to_constraints`).
        let mut to_push: Vec<TypeConstraint> = Vec::new();
        for binding in &self.call_bindings {
            if self.sub_return_type_local(&binding.func_name).is_some()
                || crate::model::builtins::builtin_return_type(&binding.func_name).is_some()
            {
                continue;
            }
            if let Some(rt) = imported_returns.get(&binding.func_name) {
                to_push.push(TypeConstraint {
                    variable: binding.variable.clone(),
                    scope: binding.scope,
                    constraint_span: binding.span,
                    inferred_type: rt.clone(),
                });
            }
        }
        for tc in to_push {
            self.push_type_constraint(tc);
        }

        // No synthetic HashKeyDef is materialized for imported keys: the
        // producer's real def is the single source. Completion reaches it
        // cross-file (`complete_hash_keys_for_owner` walks the index for a
        // `Sub` owner); rename/references/goto-def reach it via the owner edge
        // the fixup below stamps. A location-less stub would only re-appear as
        // a phantom `0:0` decl in those queries.

        // HashKeyAccess owner fixup for imports: the builder's pass
        // only ran for bindings where the func's return type was
        // known at build time. Cross-file return types come in here,
        // so the consumer-side `$cfg = Lib::get_config(); $cfg->{host}`
        // access gets its owner set to Sub{pkg, "get_config"} — matching
        // the PRODUCER's own HashKeyDef, which is the single source (see
        // the note above; no consumer-side stub is injected).
        let imported_keyed_subs: std::collections::HashSet<String> = imported_hash_keys
            .keys()
            .cloned()
            .collect();
        let binding_by_var: std::collections::HashMap<String, String> = self.call_bindings.iter()
            .filter_map(|b| {
                let bare = split_qualified(&b.func_name, self.names()).1.to_string();
                if imported_keyed_subs.contains(&bare) {
                    Some((b.variable.clone(), bare))
                } else {
                    None
                }
            })
            .collect();
        // MEASUREMENT (Q6): how many HashKeyAccess refs owe their BAKED owner
        // to this cross-file fixup, versus already having one from the build?
        for r in self.refs.iter() {
            if matches!(r.kind, RefKind::HashKeyAccess { .. }) {
                crate::util::ghost_stats::count("hka.total");
                match r.hash_key_owner() {
                    Some(o) if !matches!(o, HashKeyOwner::Variable { .. }) => {
                        crate::util::ghost_stats::count("hka.baked_before_fixup")
                    }
                    Some(_) => crate::util::ghost_stats::count("hka.variable_owned"),
                    None => crate::util::ghost_stats::count("hka.unowned"),
                }
            }
        }
        if !binding_by_var.is_empty() {
            for r in &mut self.refs {
                if let RefKind::HashKeyAccess { ref var_text } = r.kind {
                    if r.hash_key_owner()
                        .is_some_and(|o| !matches!(o, HashKeyOwner::Variable { .. }))
                    {
                        continue;
                    }
                    if let Some(func_name) = binding_by_var.get(var_text.as_str()) {
                        if let Some((pkg, keys)) = imported_hash_keys.get(func_name) {
                            if keys.iter().any(|k| k == &r.target_name) {
                                // Re-stamping the owner drops the stale
                                // HashKeyDef link; the enrichment re-index
                                // re-links against the new owner.
                                crate::util::ghost_stats::count("hka.baked_by_fixup");
                                r.bind_hash_key_owner(HashKeyOwner::Sub {
                                    package: pkg.clone(),
                                    name: func_name.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Deferred HashKeyAccess owner fix for chain-receiver
        // method calls — see `fix_chain_receiver_hash_key_owners`.
        // Enrichment runs it with module_index so cross-file
        // receiver types resolve; the same routine runs from
        // `finalize_post_walk` with module_index=None for the
        // in-file-resolvable case (chain recursion through
        // `RefTable::call_spanning` doesn't need module_index).
        self.fix_chain_receiver_hash_key_owners(module_index);

        // Cross-file inheritance edges. Local writeback emits
        // `PackageSymbol(child, m) → Edge(PackageSymbol(parent, m))`
        // for every method `m` declared on a *local* parent. When
        // the parent class lives in another file (or its methods
        // do, via further parent chaining), we read the cached
        // analysis here and project the same edge shape into the
        // local bag. The registry's edge-chase then follows
        // `PackageSymbol(child, m) → PackageSymbol(parent_cross, m)`
        // and re-enters the cached parent's bag via the existing
        // cross-file primary lookup in `query_rec`.
        if let Some(idx) = module_index {
            use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
            let zero = Span {
                start: Point { row: 0, column: 0 },
                end: Point { row: 0, column: 0 },
            };
            // Snapshot to avoid double-mutable-borrow when pushing.
            let parents_snapshot: Vec<(String, Vec<String>)> = self
                .package_parent_edges()
                .map(|(c, ps)| (c.clone(), ps.to_vec()))
                .collect();
            for (child, parents) in &parents_snapshot {
                // First-parent-wins per method, mirroring Perl's
                // default DFS-MRO. Aligned with the local-parent
                // edge emission in `write_back_sub_return_types`.
                // Same override seed as the local writeback — see
                // `own_method_names`. The two must agree, or a child gets one
                // answer at build and another after enrichment.
                let mut emitted_for_child: std::collections::HashSet<String> =
                    super::own_method_names(self.symbols(), child).collect();
                for parent in parents {
                    if parent == child {
                        continue;
                    }
                    // The parent's methods may span its candidate files.
                    for cached in idx.visible_def_candidates(parent) {
                    let whole = idx.whole_present(&cached);
                    for sym in &whole.symbols {
                        if sym.package.as_deref() != Some(parent.as_str()) {
                            continue;
                        }
                        if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                            continue;
                        }
                        if !emitted_for_child.insert(sym.name.clone()) {
                            continue;
                        }
                        self.witnesses.push(Witness {
                            attachment: WitnessAttachment::PackageSymbol {
                                package: child.clone(),
                                name: sym.name.clone(),
                            },
                            source: WitnessSource::Enrichment("inheritance_cross".to_string()),
                            payload: WitnessPayload::Edge(WitnessAttachment::PackageSymbol {
                                package: parent.clone(),
                                name: sym.name.clone(),
                            }),
                            span: zero,
                        });
                    }
                    }
                }
            }
        }

        // Re-run the mutation-extension pass: imported shapes (a var
        // typed by an imported sub's `HashWithKeys` return) only land
        // with the TCs pushed above, so build-time extension found no
        // shape to extend for them. Append-only (`clear = false`) —
        // post-finalize removal would shift `base_witness_count`;
        // duplicates are idempotent and truncated by the next cycle.
        {
            let key_writes = std::mem::take(&mut self.key_writes);
            let ctx = crate::model::witnesses::BagContext {
                scopes: &self.scopes,
                package_framework: &self.packages,
                module_index,
                package_parents: &self.packages,
                app_surface_consumers: &self.plugin.app_surface_consumers,
                lattice: crate::model::conventions::lattice_for(&self.language),
            };
            crate::model::witnesses::emit_mutation_extension_witnesses(
                &mut self.witnesses,
                &ctx,
                &key_writes,
                false,
            );
            self.key_writes = key_writes;
        }

        // Re-stamp the receivers now that the bag carries enriched
        // cross-file invocant types: every receiver consumer reads this
        // freeze (`receiver_classes`), never re-derives at query time.
        //
        // The GATE is the freshness question: a re-stamp re-derives what the
        // build already froze unless some provider of this file has moved,
        // and the flush is what knows that — see
        // `CrossFileLookup::restamp_owed`. Every unknown fails open.
        let owed = match (path, module_index) {
            (Some(p), Some(idx)) => idx.restamp_owed(p, self.stamped_at),
            _ => true,
        };
        if owed {
            self.stamp_method_call_targets(module_index);
            if let Some(idx) = module_index {
                // Recorded only for a stamp that ran WITH the index: a
                // build-time stamp resolved nothing cross-file, so
                // treating it as a stamp would license skipping the very
                // first enrichment re-stamp — the one with the most to
                // add. The clock is read AFTER the stamp, so a wave that
                // lands mid-stamp is not credited to it.
                self.stamped_at = Some(idx.flush_epoch());
            }
        } else if restamp_gate_equiv() {
            self.score_restamp_gate_skip(module_index, path);
        }
        self.rebuild_enrichment_indices();
    }

    /// Push a `TypeConstraint` shape into the witness bag — a Variable
    /// `InferredType` witness plus a class-assertion observation when
    /// the type is a class identity. The bag is the single store; this
    /// helper exists so callers can keep the legible "I'm seeding a
    /// type constraint on $X" call shape rather than open-coding the
    /// witness construction. Builder has a parallel helper that does
    /// the same thing during the walk.
    pub(crate) fn push_type_constraint(&mut self, tc: TypeConstraint) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let TypeConstraint { variable, scope, constraint_span: span, inferred_type: ty } = tc;
        self.witnesses.push(Witness {
            attachment: WitnessAttachment::Variable { name: variable, scope },
            source: WitnessSource::Builder("type_constraint".into()),
            payload: WitnessPayload::InferredType(ty),
            span: Span { start: span.start, end: span.start },
        });
    }

    /// Resolve one gated dispatch candidate against its receiver, AT QUERY
    /// TIME. Receiver resolution is two-tier: the build-time `receiver_class`
    /// hint (a locally-constructed `My::Minion->new`, a typed `has`-attribute)
    /// when present, else cross-file resolution of the call's invocant via
    /// `resolve_receiver` with the module index — which lights up
    /// helper-/attribute-returned receivers (`$c->minion->enqueue`,
    /// `$self->_minion->enqueue`) that only type once other modules are in
    /// scope. The gate (`isa target_class`) is applied by `resolve_for`; the
    /// caller never reads the inner candidate without it.
    fn resolve_dispatch_candidate<'a>(
        &'a self,
        gated: &'a ProvisionalDispatch,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> GateResult<&'a DispatchCandidate> {
        // A union receiver applies the handler when any arm passes the gate.
        let mut out = GateResult::ReceiverUntyped;
        for recv in self.dispatch_receiver_classes(gated, module_index) {
            match gated.resolve_for(Some(&recv), &self.packages, module_index) {
                applies @ GateResult::Applies(_) => return applies,
                other => out = other,
            }
        }
        out
    }

    /// Resolve the receiver classes for a gated dispatch candidate: the
    /// build-time hint, else the call's invocant via the MethodCall ref at
    /// `call_span` (cross-file aware through the bag). Uses only the gate-input
    /// accessors, never the gated handler payload.
    fn dispatch_receiver_classes(
        &self,
        gated: &ProvisionalDispatch,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> ReceiverClasses {
        if let Some(hint) = gated.receiver_hint() {
            return [hint.clone()].into_iter().collect();
        }
        let call_span = gated.call_span();
        let dispatcher = gated.dispatcher();
        self.refs
            .iter()
            .find(|r| {
                r.span == call_span
                    && r.target_name == dispatcher
                    && matches!(r.kind, RefKind::MethodCall { .. })
            })
            .map(|r| self.receiver_classes(r, module_index, ReceiverPolicy::Navigate))
            .unwrap_or_default()
    }

    /// Query-time handler call-sites in THIS file: every gated dispatch
    /// candidate whose receiver isa-resolves the gate, projected to the data
    /// `refs_to` and goto-def need. The single seam for both — `resolve.rs`
    /// (handler references) and dispatch goto-def call it, so they can't
    /// drift. Candidates ride the cache; resolution is lazy, so non-open
    /// workspace/dependency files surface exactly like open ones
    /// (`docs/adr/receiver-gated-dispatch.md`).
    pub fn applicable_dispatches(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<AppliedDispatch> {
        // Avoid double-counting a site the emit-hook path already materialized
        // as a real `DispatchCall` ref (files whose triggers fired).
        let materialized: HashSet<(Point, Point, String)> = self
            .refs
            .iter()
            .filter_map(|r| match &r.kind {
                RefKind::DispatchCall { dispatcher, .. } => {
                    Some((r.span.start, r.span.end, dispatcher.clone()))
                }
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for gated in &self.provisional_dispatches {
            if let GateResult::Applies(c) = self.resolve_dispatch_candidate(gated, module_index) {
                if materialized.contains(&(c.span.start, c.span.end, c.dispatcher.clone())) {
                    continue;
                }
                out.push(AppliedDispatch {
                    name: c.name.clone(),
                    span: c.span,
                    owner: HandlerOwner::Class(c.owner_class.clone()),
                });
            }
        }
        out
    }

    /// The applicable dispatch at a cursor point, if the cursor sits on a
    /// gated dispatch verb call whose receiver isa-resolves. Drives
    /// query-time dispatch goto-def — the same gate as `applicable_dispatches`,
    /// so an open file with a cross-file receiver resolves the handler without
    /// any eagerly-materialized `DispatchCall` ref. Matches on either the
    /// name-arg span or the whole-call span so the cursor anywhere on the
    /// `verb('name')` call lands.
    pub fn dispatch_at(
        &self,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<AppliedDispatch> {
        for gated in &self.provisional_dispatches {
            // `call_span` (a gate-input accessor) spans the whole `verb('name')`
            // call, so it covers the name-arg too — cheap cursor pre-filter
            // before resolving the gate.
            if !contains_point(&gated.call_span(), point) {
                continue;
            }
            if let GateResult::Applies(c) = self.resolve_dispatch_candidate(gated, module_index) {
                return Some(AppliedDispatch {
                    name: c.name.clone(),
                    span: c.span,
                    owner: HandlerOwner::Class(c.owner_class.clone()),
                });
            }
        }
        None
    }

    /// Every scalar-receiver dereference in this file, paired with the
    /// receiver's **narrowed** type at the use point. The one lattice read
    /// the undef/Optional/shape diagnostics (D1/D2/D6,
    /// `docs/adr/narrowing-diagnostics.md`) consume — each is a filter
    /// over this stream that asks the type, never the syntax (rule #10).
    ///
    /// A site is included only when the receiver's type resolves; an
    /// unresolvable receiver is omitted (honest silence — the diagnostics
    /// built on top miss it rather than guess). All four arrow forms are
    /// covered: method (`$x->m`) and hash (`$x->{k}`) carry a receiver-typed
    /// ref; array (`$x->[i]`) and code (`$x->()`) carry none, so the builder
    /// records them as `arrow_deref_sites` and this query merges the two
    /// sources. The residual is the receiver *shape*, not the arrow form: only
    /// a plain scalar operand is recorded, so a chain receiver (`f()->[0]`,
    /// `$x->{k}->()`) is skipped — only a plain scalar can be provably
    /// `Undef`/`Optional`.
    pub fn deref_receiver_sites(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<DerefSite> {
        use crate::model::conventions::InvocantText;
        let mut out = Vec::new();
        for r in self.refs() {
            let (receiver, form) = match &r.kind {
                RefKind::MethodCall { invocant, .. } | RefKind::FieldAccess { invocant, .. } => {
                    // Only a scalar invocant can be undef/Optional; a
                    // bareword/`__PACKAGE__`/chain/bridged receiver never
                    // narrows here.
                    let Some(name) = invocant.as_name() else { continue };
                    if !matches!(name.classify(), InvocantText::Scalar(_)) {
                        continue;
                    }
                    (invocant.text().to_string(), DerefForm::Method(r.target_name.clone()))
                }
                // A scalar `var_text` is the arrow form `$x->{k}`; the direct
                // `$h{k}` form's base is the named hash, which can't be an
                // undef *receiver* (and won't type as Undef/Optional anyway).
                RefKind::HashKeyAccess { var_text, .. } if var_text.starts_with('$') => {
                    (var_text.clone(), DerefForm::HashKey)
                }
                _ => continue,
            };
            if let Some(ty) =
                self.inferred_type_via_bag_ctx(&receiver, r.span.start, module_index)
            {
                out.push(DerefSite {
                    span: r.span,
                    receiver,
                    receiver_ty: ty,
                    form,
                });
            }
        }
        // Array (`$x->[i]`) / code (`$x->()`) derefs have no typed ref; the
        // builder records them so they join the same stream.
        for s in &self.arrow_deref_sites {
            if let Some(ty) =
                self.inferred_type_via_bag_ctx(&s.receiver, s.span.start, module_index)
            {
                out.push(DerefSite {
                    span: s.span,
                    receiver: s.receiver.clone(),
                    receiver_ty: ty,
                    form: s.form.clone(),
                });
            }
        }
        out
    }

    /// True if `ancestor` is on `descendant`'s inheritance chain (or they are
    /// the same class) — the relatedness test D4 uses to avoid flagging a
    /// legitimate downcast as a contradiction.
    fn is_ancestor_of(
        &self,
        ancestor: &str,
        descendant: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> bool {
        let mut found = false;
        self.for_each_ancestor_class(descendant, module_index, |c| {
            if c == ancestor {
                found = true;
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        });
        found
    }

    /// D3/D4: each recorded guard whose outcome is constant given its
    /// subject's prior (pre-guard) type — redundant (always-true) or
    /// contradictory (always-false). Only fires on a *confident* prior type
    /// (a concrete class / rep / `Undef`); an absent or merely-Optional prior
    /// leaves the guard meaningful and is skipped (rule #10 — ask the type).
    /// Scoped to `isa`/`DOES` (class) and `defined`/`blessed` guards; rep
    /// `ref…eq` guards are the documented residual.
    pub fn guard_redundancies(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<GuardRedundancy> {
        let mut out = Vec::new();
        for g in &self.guard_sites {
            let Some(prior) = self
                .inferred_type_via_bag_ctx(&g.subject, g.before_point, module_index)
                .filter(InferredType::is_known)
            else {
                continue;
            };
            // (definitely satisfies the raw predicate, definitely contradicts it)
            let (satisfied, contradicted) = match &g.predicate {
                GuardPredicate::Defined => match &prior {
                    InferredType::Undef => (false, true),
                    // The genuine "maybe undef" — the guard is doing its job.
                    InferredType::Optional(_) => continue,
                    // Any other concrete type is definitely defined.
                    _ => (true, false),
                },
                GuardPredicate::IsType(InferredType::ClassName(target)) => {
                    let Some(prior_cls) = prior.class_name() else { continue };
                    if prior_cls == target
                        || self.is_ancestor_of(target, prior_cls, module_index)
                    {
                        (true, false) // prior is-a target
                    } else if self.is_ancestor_of(prior_cls, target, module_index) {
                        continue; // target is a subclass of prior — a legit downcast
                    } else {
                        (false, true) // unrelated concrete classes
                    }
                }
                // Rep `ref…eq` guards (HASH/ARRAY/CODE) — residual.
                GuardPredicate::IsType(_) => continue,
            };
            let always_true =
                (satisfied && g.asserts_when_true) || (contradicted && !g.asserts_when_true);
            let always_false =
                (satisfied && !g.asserts_when_true) || (contradicted && g.asserts_when_true);
            let verdict = if always_true {
                GuardVerdict::AlwaysTrue
            } else if always_false {
                GuardVerdict::AlwaysFalse
            } else {
                continue;
            };
            out.push(GuardRedundancy {
                span: g.span,
                verdict,
                subject: g.subject.clone(),
                predicate: g.predicate.clone(),
                satisfied,
            });
        }
        out
    }

    /// The rep a `ref…eq` / `isa` GUARD established for `subject` at `point`,
    /// read from **narrowing-sourced witnesses only**. Distinct from
    /// `inferred_type_via_bag`: a `$x->{k}` deref pushes a zero-extent
    /// `HashRef` belief sitting exactly at the use point, which would mask the
    /// guard's rep under the merged query — so D6 reads the guard's assertion
    /// directly. Innermost (narrowest) containing region wins. `None` when no
    /// guard narrows the subject here (so D6 fires only on guard-narrowed
    /// reps, per the plan).
    pub fn guard_narrowed_rep(&self, subject: &str, point: Point) -> Option<InferredType> {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload, WitnessSource};
        let mut best: Option<&crate::model::witnesses::Witness> = None;
        for w in self.witnesses.filter(|w| {
            matches!(&w.source, WitnessSource::Builder(s) if s == "narrowing" || s == "defined_narrowing")
                && matches!(&w.attachment, WitnessAttachment::Variable { name, .. } if name == subject)
        }) {
            if !contains_point(&w.span, point) {
                continue;
            }
            if !matches!(&w.payload, WitnessPayload::InferredType(_)) {
                continue;
            }
            let innermost = match best {
                None => true,
                Some(b) => (w.span.start.row, w.span.start.column) >= (b.span.start.row, b.span.start.column),
            };
            if innermost {
                best = Some(w);
            }
        }
        match best.map(|w| &w.payload) {
            Some(WitnessPayload::InferredType(t)) => Some(t.clone()),
            _ => None,
        }
    }

    /// Gated dispatch candidates in THIS file whose receiver couldn't be
    /// typed (`ReceiverUntyped`) — the genuine typing gaps the opt-in
    /// `unresolved-dispatch` diagnostic surfaces. `DoesNotApply` is a settled
    /// negative and never appears here.
    pub fn untyped_dispatches(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<UntypedDispatch> {
        let mut out = Vec::new();
        for gated in &self.provisional_dispatches {
            if let GateResult::ReceiverUntyped =
                self.resolve_dispatch_candidate(gated, module_index)
            {
                out.push(UntypedDispatch {
                    call_span: gated.call_span(),
                    dispatcher: gated.dispatcher().to_string(),
                    gate: gated.gate().to_string(),
                });
            }
        }
        out
    }


    /// Rebuild the indices affected by enrichment (symbols, the ref
    /// name/target lookups, HashKeyAccess linkage).
    ///
    /// Enrichment re-owns HashKeyAccess refs (dropping their stale
    /// HashKeyDef links) and gated emissions can mint symbols. This
    /// method re-runs the same `(target_name, owner)` linker that
    /// `build_indices` uses, so the ref→target index stays accurate after a
    /// cross-file hash-key binding resolves.
    fn rebuild_enrichment_indices(&mut self) {
        self.symbols.rebuild_indices();

        // Re-link HashKeyAccess refs to (possibly newly-injected) HashKeyDef
        // symbols, mirroring build_indices's logic.
        let hashkey_defs: HashMap<(String, HashKeyOwner), SymbolId> = self.symbols.iter()
            .filter_map(|sym| {
                if let SymbolDetail::HashKeyDef { owner, .. } = &sym.detail {
                    Some(((sym.name.clone(), owner.clone()), sym.id))
                } else {
                    None
                }
            })
            .collect();
        let mut hashkey_resolutions: Vec<(usize, SymbolId)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if r.resolved_symbol().is_some() {
                continue;
            }
            if let Some(owner) = r.hash_key_owner() {
                if let Some(&sid) = hashkey_defs.get(&(r.target_name.clone(), owner.clone())) {
                    hashkey_resolutions.push((i, sid));
                }
            }
        }
        for (idx, sid) in hashkey_resolutions {
            self.refs[idx].link_owned_symbol(sid);
        }

        self.refs.refresh_name_target_indices(&self.pack.names);
    }


}

#[cfg(test)]
mod restamp_gate_tests {
    use crate::model::file_analysis::CrossFileLookup;
    use std::path::{Path, PathBuf};

    /// A lookup that answers only the gate, so the gate's rules can be tested
    /// without an index. Everything else takes the trait's defaults.
    struct Marks {
        epoch: u64,
        marks: Vec<(PathBuf, u64)>,
    }
    /// The required-method boilerplate every `CrossFileLookup` double pays.
    /// None of it participates in the gate — the gate's inputs are the path
    /// and the file's own `stamped_at`, by design.
    macro_rules! inert_lookup {
        () => {
            fn get_cached(
                &self,
                _m: &str,
            ) -> Option<std::sync::Arc<crate::model::file_analysis::CachedModule>> {
                None
            }
            fn modules_with_symbol(&self, _n: &str) -> Vec<String> {
                Vec::new()
            }
            fn modules_providing_package(&self, _p: &str) -> Vec<String> {
                Vec::new()
            }
            fn find_exporters(&self, _n: &str) -> Vec<String> {
                Vec::new()
            }
            fn defining_module_cached(
                &self,
                _e: &str,
                _n: &str,
            ) -> Option<std::sync::Arc<crate::model::file_analysis::CachedModule>> {
                None
            }
            fn module_declaring_method_in_package(
                &self,
                _p: &str,
                _m: &str,
            ) -> Option<String> {
                None
            }
            fn for_each_cached(
                &self,
                _f: &mut dyn FnMut(&str, &std::sync::Arc<crate::model::file_analysis::CachedModule>),
            ) {
            }
            fn for_each_reexport_module(
                &self,
                _s: Vec<String>,
                _v: &mut dyn FnMut(
                    &str,
                    &std::sync::Arc<crate::model::file_analysis::CachedModule>,
                ) -> std::ops::ControlFlow<()>,
            ) {
            }
            fn for_each_entity_bridged_to(
                &self,
                _c: &str,
                _f: &mut dyn FnMut(
                    &str,
                    &std::sync::Arc<crate::model::file_analysis::CachedModule>,
                    &crate::model::file_analysis::Symbol,
                ) -> std::ops::ControlFlow<()>,
            ) {
            }
            fn direct_children_of(&self, _p: &str) -> Vec<(String, String)> {
                Vec::new()
            }
            fn for_each_loader_shape(
                &self,
                _f: &mut dyn FnMut(&str, &crate::model::file_analysis::InferredType),
            ) {
            }
        };
    }

    impl CrossFileLookup for Marks {
        inert_lookup!();
        fn flush_epoch(&self) -> u64 {
            self.epoch
        }
        fn restamp_owed(&self, path: &Path, stamped_at: Option<u64>) -> bool {
            let Some(stamped_at) = stamped_at else { return true };
            match self.marks.iter().find(|(p, _)| p == path) {
                Some((_, m)) => stamped_at < *m,
                None => true,
            }
        }
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// Every unknown fails OPEN — to a re-stamp, never away from one.
    ///
    /// This is the property that lets the gate land before the flush is the
    /// standing path: with no marks written, it says "owed" everywhere and
    /// behaviour is what it was before the gate existed. A gate whose default
    /// were "skip" would silently freeze stale dispatch targets across a whole
    /// session and look like a speedup.
    #[test]
    fn every_unknown_fails_open_to_a_re_stamp() {
        let none = Marks { epoch: 7, marks: Vec::new() };
        assert!(
            none.restamp_owed(&p("/A.pm"), Some(5)),
            "no mark means no wave has spoken about this file — which is also \
             what a lost mark and an uncovered freshness edge look like"
        );
        assert!(
            none.restamp_owed(&p("/A.pm"), None),
            "never stamped is owed unconditionally: a rehydrated copy always \
             reads None, because `stamped_at` is serde(skip)"
        );
        let marked = Marks { epoch: 7, marks: vec![(p("/A.pm"), 6)] };
        assert!(
            marked.restamp_owed(&p("/A.pm"), None),
            "never stamped beats any mark — the very first enrichment re-stamp \
             is the one with the most to add"
        );
        assert!(
            marked.restamp_owed(&p("/B.pm"), Some(5)),
            "a mark on A says nothing about B"
        );
    }

    /// The gate skips only on positive evidence: this file stamped at or after
    /// the last epoch a provider of it moved.
    #[test]
    fn a_stamp_at_or_after_the_mark_skips() {
        let m = Marks { epoch: 9, marks: vec![(p("/A.pm"), 4)] };
        assert!(!m.restamp_owed(&p("/A.pm"), Some(4)), "stamped AT the mark: covered");
        assert!(!m.restamp_owed(&p("/A.pm"), Some(9)), "stamped after it: covered");
        assert!(
            m.restamp_owed(&p("/A.pm"), Some(3)),
            "stamped before the provider moved: the frozen targets predate the \
             change and must be re-derived"
        );
    }

    /// Clock reading 0 is a REAL stamp, not "never".
    ///
    /// Every stamp taken before the first flush records 0. Collapsing that
    /// into the never-stamped sentinel makes it compare equal to the first
    /// wave's mark — which is 1 — only if the sentinel is bumped to 1 to stay
    /// distinguishable, and then the pre-flush stamp silently satisfies a mark
    /// that postdates it. `Option` is what keeps the two apart; this is the
    /// case that fails without it.
    #[test]
    fn a_stamp_taken_before_any_flush_is_still_owed_after_the_first_one() {
        let m = Marks { epoch: 1, marks: vec![(p("/A.pm"), 1)] };
        assert!(
            m.restamp_owed(&p("/A.pm"), Some(0)),
            "the stamp read the clock as 0, the wave marked at 1: the stamp \
             predates the provider move and the re-stamp is owed"
        );
    }

    /// The trait's own default is fail-open.
    ///
    /// Every `CrossFileLookup` that does not implement the gate — the test
    /// doubles, the scoped wrappers, anything added later — must re-stamp. An
    /// implementor that silently inherited "skip" would disable the re-stamp
    /// for a whole class of lookups and nothing would fail.
    #[test]
    fn the_trait_default_re_stamps() {
        struct Bare;
        impl CrossFileLookup for Bare {
            inert_lookup!();
        }
        assert!(Bare.restamp_owed(&p("/A.pm"), Some(42)));
        assert!(Bare.restamp_owed(&p("/A.pm"), None));
        assert_eq!(Bare.flush_epoch(), 0);
    }
}
