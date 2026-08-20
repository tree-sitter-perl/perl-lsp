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
            let agreed = if shapes.windows(2).all(|w| w[0] == w[1]) {
                shapes.pop().unwrap()
            } else {
                // disagree → widen: union of keys when every shape is
                // keyed, OPEN (a key any caller passes may arrive);
                // anything else declines.
                let mut keys: Vec<(String, Option<Box<InferredType>>)> = Vec::new();
                let mut all_keyed = true;
                for sh in &shapes {
                    match sh {
                        InferredType::HashWithKeys { keys: ks, .. } => {
                            for (k, v) in ks {
                                match keys.iter_mut().find(|(ek, _)| ek == k) {
                                    None => keys.push((k.clone(), v.clone())),
                                    Some((_, ev)) => {
                                        if *ev != *v {
                                            *ev = None;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            all_keyed = false;
                            break;
                        }
                    }
                }
                if !all_keyed {
                    continue;
                }
                InferredType::HashWithKeys { keys, open: true }
            };
            let span = self
                .scopes
                .get(m.scope.0 as usize)
                .map(|sc| Span { start: sc.span.start, end: sc.span.start })
                .unwrap_or(Span {
                    start: Point { row: 0, column: 0 },
                    end: Point { row: 0, column: 0 },
                });
            self.push_type_constraint(TypeConstraint {
                variable: m.variable.clone(),
                scope: m.scope,
                constraint_span: span,
                inferred_type: agreed,
            });
        }
    }

    /// Materialize deferred plugin pattern emissions ([`GatedEmission`])
    /// whose `ClassIsa` gate is now satisfied CROSS-FILE. Called from
    /// `enrich_imported_types_with_keys` after the symbol/ref/witness
    /// truncation, so the re-fired content sits above the baselines and is
    /// re-derived every enrichment cycle (idempotent). Deterministic: gate
    /// resolution goes through `class_isa_prefix` (the single MRO seam), and
    /// symbols are minted with positional `SymbolId`s exactly as the builder
    /// would have — a file enriched late converges to one built with the
    /// ancestry known. Rule #10: the "should this synthesis apply?" question
    /// is answered by asking the ancestry graph, never by a shape branch.
    fn apply_gated_emissions(&mut self, module_index: Option<&dyn CrossFileLookup>) {
        if self.plugin.gated_emissions.is_empty() {
            return;
        }
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        // Snapshot: the borrow of `self.plugin.gated_emissions` can't overlap the
        // `&mut self` symbol/ref/witness pushes below.
        let emissions = std::mem::take(&mut self.plugin.gated_emissions);
        for em in &emissions {
            let fires = em.gate_prefixes.iter().any(|prefix| {
                class_isa_prefix(&em.package, prefix, &self.packages, module_index)
            });
            if !fires {
                continue;
            }
            let scope = self.scope_at(em.scope_point).unwrap_or(ScopeId(0));
            let ns = Namespace::framework(em.plugin_id.clone());
            for gs in &em.symbols {
                let pkg = gs.on_class.clone().or_else(|| Some(em.package.clone()));
                // Same dedup the builder's `apply_emit_action` runs: never
                // stack a second identical synthesized symbol.
                let dup = self.symbols.iter().any(|s| {
                    s.name == gs.name
                        && s.kind == gs.kind
                        && s.package == pkg
                        && s.namespace == ns
                });
                if dup {
                    continue;
                }
                let id = SymbolId(self.symbols.len() as u32);
                self.symbols.push(Symbol {
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
                    deref_stack: Vec::new(),
                    arity: None,
                });
                if let Some(rt) = &gs.return_type {
                    // Plugin-priority `Symbol(sid) → InferredType`, matching
                    // the builder's Method emit. Class-scoped methods also get
                    // the `PackageSymbol{package,name} → Edge(Symbol(sid))`
                    // mirror the fold writeback would have pushed, so cross-
                    // file return-type queries resolve the relationship shape.
                    self.witnesses.push(Witness {
                        attachment: WitnessAttachment::Symbol(id),
                        source: WitnessSource::Plugin(em.plugin_id.clone()),
                        payload: WitnessPayload::InferredType(rt.clone()),
                        span: gs.span,
                    });
                    if matches!(gs.kind, SymKind::Method | SymKind::Sub) {
                        if let Some(class) = &pkg {
                            self.witnesses.push(Witness {
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
                self.refs.push(Ref {
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
        self.plugin.gated_emissions = emissions;
    }

    /// Materialize deferred gated emissions into a WORKSPACE-resident cached
    /// copy, standalone (not inside the full enrichment pass). Used by the
    /// index-completion pass so `whole_present` — the view every cross-file
    /// goto-def / references reader consults — sees a DBIC result class's
    /// synthesized accessors WITHOUT paying the per-query enriched overlay.
    /// Idempotent: `apply_gated_emissions` dedups against existing symbols, so
    /// a second call is a no-op; the emissions sit above the symbol table's
    /// enrichment baseline and a later full enrichment re-derives them the
    /// same way. Rebuilds the name/scope indices so `symbols_named` /
    /// `sub_info_view` find them.
    pub fn materialize_gated_emissions(&mut self, module_index: &dyn CrossFileLookup) {
        if self.plugin.gated_emissions.is_empty() {
            return;
        }
        self.apply_gated_emissions(Some(module_index));
        self.rebuild_enrichment_indices();
    }

    pub fn enrich_imported_types_with_keys(
        &mut self,
        module_index: Option<&dyn CrossFileLookup>,
    ) {
        crate::util::ghost_stats::count("enrich_imported_types_with_keys");
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
        self.apply_gated_emissions(module_index);

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
                [b.func_name.as_str(), split_qualified(&b.func_name).1]
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
                // `package_ranges`, not `enclosing_package_of`: the latter
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
                // `export_lookup` (@EXPORT u @EXPORT_OK) is not an evictable
                // axis and is rebuilt by `build_indices` on every copy, so this
                // reads the RESIDENT analysis: no rehydrate, no decode, no LRU
                // traffic. Measured on the substrate: it drops the per-candidate
                // bag fetch from 7,829 to 332 (95.8% of them were for providers
                // that matched nothing). Do not "simplify" this to a
                // symbols-axis probe — `symbols_present` rehydrates too, and
                // doing that made the chase SLOWER, not faster.
                if !needed_names.iter().any(|n| cached.analysis.exports_name(n)) {
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
                    if !whole.exports_name(&sym.name) {
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
                let bare = split_qualified(&b.func_name).1.to_string();
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
        // in-file-resolvable case (chain recursion via
        // `RefTable::call_at_start` doesn't need module_index).
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
                let mut emitted_for_child: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
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
            };
            crate::model::witnesses::emit_mutation_extension_witnesses(
                &mut self.witnesses,
                &ctx,
                &key_writes,
                false,
            );
            self.key_writes = key_writes;
        }

        self.emit_method_call_binding_edges();
        // Re-stamp the MethodCall dispatch-target edges now that the bag
        // carries enriched cross-file invocant types. Enrichment truncated
        // refs back to their baseline, wiping the build-time (local-only)
        // edge; this re-derives it with the index so cross-file-typed
        // invocants resolve. Single-sourced: refs_to / find_def / hover
        // read this frozen edge, never re-derive at query time.
        self.stamp_method_call_targets(module_index);
        self.rebuild_enrichment_indices();
    }

    /// The MCB→bag bridge: each recorded `$var = $invocant->method()`
    /// binding becomes a `Variable → Edge(PackageSymbol{package, method})`
    /// witness (tag `mcb`), so the registry chases the method's return
    /// lazily — with whatever index the QUERY holds — instead of a value
    /// materialized here (edges, not values). The invocant class is
    /// resolved per run: the finalize run sees only walk-seeded types;
    /// the enrichment re-run resolves invocants that only type once
    /// imported TCs land. Append-only (post-finalize removal would shift
    /// the sealed `base_witness_count`, same rule as
    /// `emit_mutation_extension_witnesses`): duplicates are idempotent
    /// under the fold and truncated by the next enrichment cycle.
    pub(super) fn emit_method_call_binding_edges(&mut self) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let bindings = self.method_call_bindings.clone();
        for binding in &bindings {
            // Resolve invocant to class name
            let class_name = self.resolve_invocant_class(
                &binding.invocant_var,
                binding.scope,
                binding.span.start,
            );

            if let Some(cn) = class_name {
                self.witnesses.push(Witness {
                    attachment: WitnessAttachment::Variable {
                        name: binding.variable.clone(),
                        scope: binding.scope,
                    },
                    source: WitnessSource::Builder("mcb".into()),
                    payload: WitnessPayload::Edge(WitnessAttachment::PackageSymbol {
                        package: cn,
                        name: binding.method_name.clone(),
                    }),
                    // Zero-width at the assignment, the TC temporal
                    // contract: invisible to reads before the binding.
                    span: Span {
                        start: binding.span.start,
                        end: binding.span.start,
                    },
                });
            }
        }
    }

    /// Push a `TypeConstraint` shape into the witness bag — a Variable
    /// `InferredType` witness plus a class-assertion observation when
    /// the type is a class identity. The bag is the single store; this
    /// helper exists so callers can keep the legible "I'm seeding a
    /// type constraint on $X" call shape rather than open-coding the
    /// witness construction. Builder has a parallel helper that does
    /// the same thing during the walk.
    pub(crate) fn push_type_constraint(&mut self, tc: TypeConstraint) {
        use crate::model::witnesses::{
            TypeObservation, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };
        let TypeConstraint { variable, scope, constraint_span: span, inferred_type: ty } = tc;
        self.witnesses.push(Witness {
            attachment: WitnessAttachment::Variable { name: variable.clone(), scope },
            source: WitnessSource::Builder("type_constraint".into()),
            payload: WitnessPayload::InferredType(ty.clone()),
            span: Span { start: span.start, end: span.start },
        });
        match ty {
            InferredType::ClassName(n) => {
                self.witnesses.push(Witness {
                    attachment: WitnessAttachment::Variable { name: variable, scope },
                    source: WitnessSource::Builder("type_constraint".into()),
                    payload: WitnessPayload::Observation(TypeObservation::ClassAssertion(n)),
                    span,
                });
            }
            InferredType::FirstParam { package } => {
                self.witnesses.push(Witness {
                    attachment: WitnessAttachment::Variable { name: variable, scope },
                    source: WitnessSource::Builder("type_constraint".into()),
                    payload: WitnessPayload::Observation(TypeObservation::FirstParamInMethod {
                        package,
                    }),
                    span,
                });
            }
            _ => {}
        }
    }

    /// Resolve one gated dispatch candidate against its receiver, AT QUERY
    /// TIME. Receiver resolution is two-tier: the build-time `receiver_class`
    /// hint (a locally-constructed `My::Minion->new`, a typed `has`-attribute)
    /// when present, else cross-file resolution of the call's invocant via
    /// `method_call_invocant_class` with the module index — which lights up
    /// helper-/attribute-returned receivers (`$c->minion->enqueue`,
    /// `$self->_minion->enqueue`) that only type once other modules are in
    /// scope. The gate (`isa target_class`) is applied by `resolve_for`; the
    /// caller never reads the inner candidate without it.
    fn resolve_dispatch_candidate<'a>(
        &'a self,
        gated: &'a ProvisionalDispatch,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> GateResult<&'a DispatchCandidate> {
        let recv = self.dispatch_receiver_class(gated, module_index);
        gated.resolve_for(recv.as_deref(), &self.packages, module_index)
    }

    /// Resolve the receiver class for a gated dispatch candidate: the
    /// build-time hint, else the call's invocant via the MethodCall ref at
    /// `call_span` (cross-file aware through the bag). Uses only the gate-input
    /// accessors, never the gated handler payload.
    fn dispatch_receiver_class(
        &self,
        gated: &ProvisionalDispatch,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        gated.receiver_hint().cloned().or_else(|| {
            let call_span = gated.call_span();
            let dispatcher = gated.dispatcher();
            self.refs
                .iter()
                .find(|r| {
                    r.span == call_span
                        && r.target_name == dispatcher
                        && matches!(r.kind, RefKind::MethodCall { .. })
                })
                .and_then(|r| self.method_call_invocant_class(r, module_index))
        })
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
                RefKind::MethodCall { invocant, .. } => {
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
            let Some(prior) =
                self.inferred_type_via_bag_ctx(&g.subject, g.before_point, module_index)
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

        self.refs.refresh_name_target_indices();
    }


}
