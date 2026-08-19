//! FileAnalysis lifecycle: construction, eviction, index building,
//! post-walk finalization, and the heap-estimate probe.

use super::*;

/// How much of a resident index copy survives the registration strip.
///
/// The evictable axes are a **ladder, not a set of independent switches**:
/// every tier that drops the row axes drops the witness bag too. That was
/// previously carried as `evict_axes(strip_bag, strip_rows)`, whose fourth
/// combination — rows dropped, bag kept — no tier wants and nothing
/// prevented. It is not a crash if someone writes it: `bag_present` hands
/// back a copy whose symbols were evicted, and every consumer that reads
/// both (cross-file import enrichment walks `symbols` off a bag view) reads
/// absence-by-eviction as absence-in-fact. A silently smaller answer.
///
/// The ladder is not invented here — the callers already computed it by
/// hand, as `let strip_rows = strip_bag && rows_ok`. This lifts that
/// conjunction out of an expression and into the type, so the illegal
/// pairing has no spelling.
///
/// Ordered widest-to-narrowest; a new axis extends the narrow end (and
/// `is_fully_resident`), never adds a flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Residency {
    /// Nothing evicted — open docs, degraded files, `PERL_LSP_NO_EVICT`.
    Whole,
    /// Witness bag dropped; refs and symbols kept. The @INC tier, whose
    /// copies the MRO existence walks hammer for symbols.
    RowsOnly,
    /// Bag AND both row axes dropped. The workspace/pack tier once the blob
    /// and its rows are persisted and can rehydrate.
    Skeleton,
}

impl Residency {
    /// The strip a persisting tier wants: nothing when eviction is off,
    /// otherwise bag-only until the rows are safely on disk.
    ///
    /// This is the one place the "rows only once the blob can rehydrate
    /// them" rule is written. `rows_ok` false with eviction on is exactly
    /// `RowsOnly` — never the rows-without-bag combination, which the type
    /// cannot express.
    pub fn for_strip(eviction_enabled: bool, rows_ok: bool) -> Self {
        match (eviction_enabled, rows_ok) {
            (false, _) => Residency::Whole,
            (true, false) => Residency::RowsOnly,
            (true, true) => Residency::Skeleton,
        }
    }
}

impl FileAnalysis {
    /// Create a new FileAnalysis with indices built from the raw tables.
    /// `finalize_post_walk` runs on the builder path to seal baseline
    /// counts and resolve text-based MCB; hand-crafted test FAs skip it
    /// and push witnesses directly.
    pub fn new(parts: FileAnalysisParts) -> Self {
        let FileAnalysisParts {
            scopes,
            symbols,
            refs,
            fold_ranges,
            imports,
            call_bindings,
            packages,
            pack,
            plugin,
            method_call_bindings,
            framework_imports,
            export,
            export_ok,
            export_tags,
            reexport_modules,
            lib_roots,
            type_provenance,
            package_ranges,
            mut witnesses,
            provisional_dispatches,
            guard_sites,
            arrow_deref_sites,
            gated_param_types,
            attr_projections,
            reassigned_scalars,
            key_writes,
            contract_symbols,
            dbic_source_name,
            column_keyed_verbs,
            dynamic_dispatch_sites,
            loader_config_params,
            flow_edges,
        } = parts;
        witnesses.rebuild_index();
        let mut fa = FileAnalysis {
            pack,
            plugin,
            scopes,
            symbols: SymbolTable::from_vec(symbols),
            refs: RefTable::from_vec(refs),
            fold_ranges,
            imports,
            call_bindings,
            method_call_bindings,
            package_ranges,
            packages,
            framework_imports,
            export,
            export_ok,
            export_tags,
            reexport_modules,
            lib_roots,
            type_provenance,
            witnesses,
            bag_evicted: false,
            base_witness_count: 0,
            provisional_dispatches,
            guard_sites,
            arrow_deref_sites,
            gated_param_types,
            attr_projections,
            reassigned_scalars,
            key_writes,
            contract_symbols,
            dbic_source_name,
            column_keyed_verbs,
            dynamic_dispatch_sites,
            loader_config_params,
            flow_edges,
            degraded: false,
            // Pack drivers re-stamp their id post-construction.
            language: super::default_language(),
            scope_starts: Vec::new(),
            export_lookup: HashSet::new(),
        };
        fa.build_indices();
        fa
    }

    /// Run the local-only method-call-binding resolution and seal
    /// baseline counts. Called by `builder::build` after the witness
    /// bag has been moved in.
    ///
    /// `Symbol(sym_id)` and `MethodOnClass{class, name}` return-type
    /// witnesses for every local Sub/Method are already in the bag —
    /// published by `Builder::write_back_sub_return_types` at the
    /// end of the worklist (single emission point for "this sub's
    /// return type is known"). Cross-file imports do not get a local
    /// mirror; they resolve lazily through `query_sub_return_type`.
    /// Drop the witness bag (the build-time type-inference scaffold) from this
    /// resident analysis. The full bag rides the on-disk blob, so this is
    /// lossless — a type query needing it rehydrates the exact persisted bag on
    /// demand (`docs/adr/memory-slice-2-lru.md`). Clears both the
    /// `Vec<Witness>` and its rebuilt index; refs, symbols and ref bindings all
    /// survive. Idempotent.
    ///
    /// What does NOT survive, and is easy to assume otherwise: a sub's RETURN
    /// TYPE. The fold's conclusion is published by
    /// `write_back_sub_return_types` as a `MethodOnClass{..} -> Edge(Symbol(id))`
    /// witness — in the bag, by the "edges, not values" invariant, since
    /// materialising it into a field would be the parallel store the worklist
    /// rules forbid. There is no `return_types` field to fall back on. So
    /// evicting here means every cross-file "what does this sub return" costs a
    /// rehydrate, which is the whole reason enrichment's provider chase is
    /// dominated by `bag_present` (measured: ~400 witnesses moved per return-type
    /// query answered).
    pub fn evict_witness_bag(&mut self) {
        self.witnesses = crate::model::witnesses::WitnessBag::default();
        self.bag_evicted = true;
    }

    /// True when `evict_witness_bag` stripped this copy's bag: an empty bag
    /// here means "on disk, not resident", not "no type facts".
    pub fn bag_is_evicted(&self) -> bool {
        self.bag_evicted
    }

    /// Strip the resident refs axis — the refs twin of `evict_witness_bag`.
    /// `RefTable::evict` drops the vec and every index over it in one place,
    /// so no index can survive its refs. Touches no other pinned field.
    /// Idempotent.
    pub fn evict_refs(&mut self) {
        self.refs.evict();
    }

    /// True when `evict_refs` stripped this copy's refs: empty means "on
    /// disk, not resident", never "no references". Gates `refs_present`'s
    /// resident fast path (with `symbols_are_evicted` — the matcher reads
    /// both row axes); the rehydrate arm is mandatory, or eviction reads
    /// as absence.
    pub fn refs_are_evicted(&self) -> bool {
        self.refs.is_evicted()
    }

    /// Strip the resident symbols axis from an index copy whose blob +
    /// `syms` rows are persisted — the symbols twin of `evict_refs`.
    /// `SymbolTable::evict` drops the vec and every index over it in one
    /// place, so no index can survive its symbols. Lossless: the on-disk
    /// analysis keeps the full vec; enumeration (workspace/symbol) reads
    /// rows, detail reads rehydrate. Registration feeds were extracted
    /// BEFORE this runs. Touches no other pinned field
    /// (`export`/`export_ok`/`export_lookup` derive from export lists, not
    /// symbols, and stay). Idempotent.
    pub fn evict_symbols(&mut self) {
        self.symbols.evict();
    }

    /// True when `evict_symbols` stripped this copy's symbols: empty means
    /// "on disk, not resident", never "no symbols".
    pub fn symbols_are_evicted(&self) -> bool {
        self.symbols.is_evicted()
    }

    /// Whole on EVERY evictable axis — the property `whole_present` gates
    /// on. New eviction axes extend THIS conjunction (and their `evict_*`
    /// setter), so multi-axis consumers stay whole-covered by construction
    /// instead of each spelling its own flag list.
    pub fn is_fully_resident(&self) -> bool {
        !self.bag_evicted && !self.refs.is_evicted() && !self.symbols.is_evicted()
    }

    /// The ONE speller of the registration strip. Every registration path
    /// routes here so a new eviction axis is added in exactly one place; a
    /// site spelling `evict_*` calls directly is re-stating this by
    /// convention.
    ///
    /// Takes a `Residency` rather than a pair of flags because the axes are
    /// a LADDER, not independent switches — see that type. The pair could
    /// spell "rows stripped, bag kept", which no tier wants and which would
    /// turn every `bag_present` consumer that also reads symbols into
    /// absence-by-eviction: a silently smaller answer, not a crash.
    pub fn evict_to(&mut self, level: Residency) {
        match level {
            Residency::Whole => {}
            Residency::RowsOnly => self.evict_witness_bag(),
            Residency::Skeleton => {
                self.evict_witness_bag();
                self.evict_refs();
                self.evict_symbols();
            }
        }
    }

    pub(crate) fn finalize_post_walk(&mut self) {
        self.emit_method_call_binding_edges();
        // Fill HashKeyAccess owners that are resolvable in-file
        // via the invocant ladder (`method_call_invocant_type`).
        // Cross-file gaps stay None until
        // `enrich_imported_types_with_keys` re-runs the same
        // routine with `module_index`.
        self.fix_chain_receiver_hash_key_owners(None);
        // Stamp the build-time-resolved dispatch target on MethodCall
        // refs (local-only here; enrichment re-stamps with the index
        // for OPEN docs). Mutates existing refs in place, so it must run
        // before the ref table seals its baseline — the seal counts the
        // refs, the stamp only sets a field on them.
        self.stamp_method_call_targets(None);
        self.base_witness_count = self.witnesses.len();
        self.symbols.seal_baseline();
        self.refs.seal_baseline();
    }

    /// Stamp the `Method` binding on every `MethodCall` ref — the NAV
    /// unification edge (build pipeline phase 6 `PostFold`, then re-stamped
    /// at enrichment). The invocant class is resolved ONCE here (via the
    /// bag-routed `method_call_invocant_class`) and frozen on the ref;
    /// `refs_to` / `find_definition` / hover read the frozen edge instead of
    /// re-deriving the class at query time, so they can never diverge.
    ///
    /// Contract: if the invocant class does not infer, store `None` (honest
    /// miss). No name-only fallback — that re-introduces the `->new` flood.
    pub(crate) fn stamp_method_call_targets(&mut self, module_index: Option<&dyn CrossFileLookup>) {
        // Collect resolutions first; `method_call_invocant_class` /
        // `resolve_method_in_ancestors` borrow `&self`, so we can't hold a
        // `&mut self.refs[i]` while calling them.
        let mut stamped: Vec<(usize, Option<MethodTarget>)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            // A plugin-bridged invocant must NEVER freeze as a class:
            // its resolution needs the index + the owning plugin, absent
            // at build time. Leaving the edge `None` makes `refs_to` /
            // goto-def re-consult the plugin at query time (with the
            // index in hand) instead of trusting a guessed token.
            if !matches!(r.kind, RefKind::MethodCall { .. })
                || matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.is_bridged())
            {
                continue;
            }
            let target = self
                .method_call_invocant_class(r, module_index)
                .map(|cn| {
                    match self.resolve_method_in_ancestors(&cn, r.unqualified_target_name(), module_index) {
                        Some(MethodResolution::Local { sym_id, .. }) => MethodTarget::Local {
                            sym_id,
                            invocant_class: cn,
                        },
                        // Method found cross-file, OR the invocant class is
                        // known but the method isn't found on it locally and
                        // the class has cross-file parents / a cross-file
                        // body the index may carry. Either way the invocant
                        // froze, so keep the edge (CrossFile); the rename
                        // chain still gates which targets it matches. A class
                        // with no method and no parents still resolved its
                        // invocant — the edge records that fact; find_def's
                        // method-not-found arm returns None honestly.
                        _ => MethodTarget::CrossFile { invocant_class: cn },
                    }
                });
            stamped.push((i, target));
        }
        for (i, target) in stamped {
            // Monotone: a re-stamp that can't re-derive the invocant class must
            // not ERASE an authoritative freeze. Witnesses only accrue (finalize
            // → enrichment adds the index), so a class never legitimately
            // retracts; the only Some→None here is a synthesized member ref
            // whose class was frozen from the field decl (a macro-body
            // `->field` whose receiver is an untypeable macro parameter). Keep it.
            if let Some(target) = target {
                self.refs[i].bind_method(target);
            }
        }
    }

    /// Set the owner binding on owner-less `HashKeyAccess` refs
    /// whose enclosing `MethodCall`'s receiver types as a
    /// `Parametric` flavor that claims this method's args (DBIC's
    /// `search`/`find`/`update`/...). Build emits these refs
    /// eagerly with `owner: None` for chain receivers it can't
    /// resolve at walk time; this routine fills them once the
    /// receiver's type is resolvable.
    ///
    /// `module_index = None` resolves only in-file chains. The
    /// same routine runs from enrichment with `module_index =
    /// Some(_)` to fill cross-file gaps. Idempotent — only None-
    /// owner refs are touched, so a second run leaves them alone.
    pub(super) fn fix_chain_receiver_hash_key_owners(&mut self, module_index: Option<&dyn CrossFileLookup>) {
        let mut owner_fixes: Vec<(usize, HashKeyOwner)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if !matches!(r.kind, RefKind::HashKeyAccess { .. }) || r.hash_key_owner().is_some() {
                continue;
            }
            // Find the enclosing MethodCall ref by span
            // containment — smallest-span containing MethodCall
            // wins (innermost call's args).
            let mut enclosing: Option<&Ref> = None;
            let mut enclosing_area: u64 = u64::MAX;
            for other in self.refs() {
                if !matches!(other.kind, RefKind::MethodCall { .. }) {
                    continue;
                }
                if !contains_point(&other.span, r.span.start) {
                    continue;
                }
                let area = (other.span.end.row.saturating_sub(other.span.start.row)) as u64
                    * 10_000
                    + other.span.end.column as u64;
                if area < enclosing_area {
                    enclosing = Some(other);
                    enclosing_area = area;
                }
            }
            let Some(call) = enclosing else { continue };
            let Some(ty) = self.method_call_invocant_type(call, module_index) else {
                continue;
            };
            let Some(p) = ty.as_parametric() else { continue };
            // Bare method name: a qualified spelling (`SUPER::search`,
            // `Foo::search`) claims args exactly like the bare one — the
            // flavor's vocabulary is unqualified.
            let Some(o) = p.method_arg_owner(call.unqualified_target_name()) else { continue };
            owner_fixes.push((i, o));
        }
        for (i, o) in owner_fixes {
            self.refs[i].bind_hash_key_owner(o);
        }
    }


    /// Rebuild all derived indices after deserialization.
    /// Idempotent: safe to call on a freshly deserialized `FileAnalysis` whose
    /// index fields were zeroed by `#[serde(skip, default)]`.
    pub fn after_deserialize(&mut self) {
        // Clear first in case this is called on a populated FileAnalysis.
        self.scope_starts.clear();
        self.export_lookup.clear();
        self.build_indices();
    }

    fn build_indices(&mut self) {
        // Scope starts — sorted for binary search
        self.scope_starts = self.scopes.iter()
            .map(|s| (s.span.start, s.id))
            .collect();
        self.scope_starts.sort_by_key(|(p, _)| (p.row, p.column));

        self.symbols.rebuild_indices();

        // Link HashKeyAccess refs to their HashKeyDef symbols whenever the
        // owner is already resolved (the builder's pre-pass handled type
        // constraints + variable identity + call-binding fixups). With this
        // link, `refs_to_symbol(def_id)` returns all accesses in O(1), which
        // is what references, rename, and highlights consume.
        let hashkey_defs: HashMap<(&str, &HashKeyOwner), SymbolId> = self.symbols.iter()
            .filter_map(|sym| {
                if let SymbolDetail::HashKeyDef { owner, .. } = &sym.detail {
                    Some(((sym.name.as_str(), owner), sym.id))
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
                if let Some(&sid) = hashkey_defs.get(&(r.target_name.as_str(), owner)) {
                    hashkey_resolutions.push((i, sid));
                }
            }
        }
        for (idx, sid) in hashkey_resolutions {
            self.refs[idx].link_owned_symbol(sid);
        }

        // Link DispatchCall refs → Handler symbols by (owner, name). A
        // DispatchCall whose owner couldn't be resolved at build time (e.g.
        // `$obj->emit('x')` where `$obj` type isn't known yet) stays
        // unlinked here and may be re-resolved by enrichment when the
        // cross-file receiver type becomes known.
        //
        // Unlike hash keys, multiple Handlers with the same (owner, name)
        // legitimately coexist (stacked registrations) — we link the ref
        // to the *first* def found so the linked symbol is a single target,
        // and rely on `refs_to_symbol` walking all stacked defs separately
        // for features like references/rename.
        let handler_defs: HashMap<(&str, &HandlerOwner), SymbolId> = self.symbols.iter()
            .filter_map(|sym| {
                if let SymbolDetail::Handler { owner, .. } = &sym.detail {
                    Some(((sym.name.as_str(), owner), sym.id))
                } else {
                    None
                }
            })
            .collect();
        let mut handler_resolutions: Vec<(usize, SymbolId)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if r.resolved_symbol().is_some() { continue; }
            if let Some(owner) = r.handler_owner() {
                if let Some(&sid) = handler_defs.get(&(r.target_name.as_str(), owner)) {
                    handler_resolutions.push((i, sid));
                }
            }
        }
        for (idx, sid) in handler_resolutions {
            self.refs[idx].link_owned_symbol(sid);
        }

        self.refs.rebuild_indices();

        // Export membership set — union of export + export_ok for O(1) lookup.
        self.export_lookup = self.export.iter()
            .chain(self.export_ok.iter())
            .cloned()
            .collect();
    }

    /// True if `name` appears in `@EXPORT` or `@EXPORT_OK` for this module.
    /// O(1) via `export_lookup` (built by `build_indices`).
    pub fn exports_name(&self, name: &str) -> bool {
        self.export_lookup.contains(name)
    }

    /// A producer module's export surface — the names a consumer's `use` can
    /// bring into scope, split into the default set (`@EXPORT`, auto-imported by
    /// a bare `use M;`), the optional set (`@EXPORT_OK`, opt-in only), and tags
    /// (`%EXPORT_TAGS`, with `:DEFAULT` synthesized as `@EXPORT`). This is the
    /// single structure `imported_names` evaluates a consumer's import spec
    /// against, so diagnostics and nav share one notion of "what does this
    /// module export, and what does this `use` bind."
    pub fn export_surface(&self) -> ExportSurface<'_> {
        ExportSurface {
            analysis: self,
            default_set: None,
            optional_set: None,
            tags: None,
            all_names: None,
        }
    }

    /// Like `export_surface`, but resolves `reexport_modules` transitively
    /// through `module_index`: the materialized surface includes every
    /// re-exported module's surface (default ∪ optional ∪ tags), walked
    /// cross-file via `ModuleIndex::for_each_reexport_module` (seen-set for
    /// cycles, fan-out cap). When this module has no re-export edges the
    /// result is identical to `export_surface` (own-only, zero extra storage).
    /// This is the one transitive-closure site — the consumer evaluator
    /// (`imported_names`) is untouched; it binds whatever the surface reports.
    pub fn export_surface_with_index(
        &self,
        module_index: &dyn CrossFileLookup,
    ) -> ExportSurface<'_> {
        if self.reexport_modules.is_empty() {
            return self.export_surface();
        }

        let mut default_set: Vec<String> = self.export.clone();
        let mut optional_set: Vec<String> = self.export_ok.clone();
        let mut tags: HashMap<String, Vec<String>> = self.export_tags.clone();

        // Merge every re-exported module's surface, walking the edges through the
        // one shared traversal (cycle-bounded + fan-out-capped). Own surface is
        // already seeded above, so we seed the queue with `reexport_modules`.
        module_index.for_each_reexport_module(
            self.reexport_modules.to_vec(),
            &mut |cached| {
                let a = &cached.analysis;
                for n in &a.export {
                    if !default_set.contains(n) {
                        default_set.push(n.clone());
                    }
                }
                for n in &a.export_ok {
                    if !optional_set.contains(n) {
                        optional_set.push(n.clone());
                    }
                }
                for (tag, members) in &a.export_tags {
                    let bucket = tags.entry(tag.clone()).or_default();
                    for m in members {
                        if !bucket.contains(m) {
                            bucket.push(m.clone());
                        }
                    }
                }
                std::ops::ControlFlow::Continue(())
            },
        );

        let mut all_names: HashSet<String> = HashSet::new();
        all_names.extend(default_set.iter().cloned());
        all_names.extend(optional_set.iter().cloned());
        for members in tags.values() {
            all_names.extend(members.iter().cloned());
        }

        ExportSurface {
            analysis: self,
            default_set: Some(default_set),
            optional_set: Some(optional_set),
            tags: Some(tags),
            all_names: Some(all_names),
        }
    }

}

/// Per-bucket resident-heap estimate for one or many `FileAnalysis`es, summed
/// by `add`. Measurement support for the bounded-memory work
/// (`docs/adr/memory-slice-2-lru.md`); NOT on any query path, wired only behind
/// the `PERL_LSP_HEAP_DUMP` env gate at the end of pack indexing.
///
/// Methodology: flat `size_of` of each collection's element footprint times its
/// `capacity` (so `Vec`/`HashMap` backing slack is counted), plus the deep
/// `String` capacities of the dominant string-bearing buckets (ref target
/// names, symbol names, the include closure, the reverse-index keys). Deep
/// strings inside the long-tail structs are NOT drilled — a deliberate,
/// documented undercount that keeps the probe cheap; the dominant buckets it
/// drills are what the eviction design turns on.
#[derive(Default, Clone, Debug)]
pub struct HeapBreakdown {
    pub files: usize,
    /// `refs` vec + every ref's `target_name`.
    pub refs: usize,
    /// `symbols` vec + names/packages/attributes.
    pub symbols: usize,
    /// Witness-bag `witnesses` vec.
    pub witness_vec: usize,
    /// Witness-bag rebuilt attachment index (serde-skip, rebuilt on load).
    pub witness_index: usize,
    /// `include_closure` + `include_directives` strings — the abseil
    /// header-path duplication.
    pub include: usize,
    /// `scopes` vec + package names.
    pub scopes: usize,
    /// The serde-skip reverse indices rebuilt on load (the ref table's
    /// name/target/call lookups, the symbol table's name/scope lookups, … ).
    pub rebuilt_indices: usize,
    /// `imports` + `call_bindings` + `method_call_bindings` + `fold_ranges`.
    pub bindings: usize,
    /// The pack/cpp flat fact vectors (domain sites, flow edges, macro defs,
    /// guard/deref sites, projections, moved-from, regions, …).
    pub cpp_extras: usize,
    /// The per-package small maps/sets (parents, uses, frameworks, exports,
    /// role/dynamic sets, provenance, template params, …).
    pub misc: usize,
    /// `size_of::<FileAnalysis>()` — the inline struct shell, once per file.
    pub shell: usize,
}

impl HeapBreakdown {
    pub fn add(&mut self, o: &HeapBreakdown) {
        self.files += o.files;
        self.refs += o.refs;
        self.symbols += o.symbols;
        self.witness_vec += o.witness_vec;
        self.witness_index += o.witness_index;
        self.include += o.include;
        self.scopes += o.scopes;
        self.rebuilt_indices += o.rebuilt_indices;
        self.bindings += o.bindings;
        self.cpp_extras += o.cpp_extras;
        self.misc += o.misc;
        self.shell += o.shell;
    }

    pub fn total(&self) -> usize {
        self.refs
            + self.symbols
            + self.witness_vec
            + self.witness_index
            + self.include
            + self.scopes
            + self.rebuilt_indices
            + self.bindings
            + self.cpp_extras
            + self.misc
            + self.shell
    }
}

impl std::fmt::Display for HeapBreakdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |b: usize| b as f64 / 1_048_576.0;
        let t = self.total().max(1);
        let row = |f: &mut std::fmt::Formatter<'_>, name: &str, b: usize| {
            writeln!(
                f,
                "  {name:<20} {:>9.1} MB  ({:>4.1}%)",
                mb(b),
                b as f64 / t as f64 * 100.0
            )
        };
        writeln!(
            f,
            "FileAnalysis heap composition ({} files, ~{:.1} MB estimated payload):",
            self.files,
            mb(self.total())
        )?;
        row(f, "refs", self.refs)?;
        row(f, "rebuilt_indices", self.rebuilt_indices)?;
        row(f, "witness_vec", self.witness_vec)?;
        row(f, "witness_index", self.witness_index)?;
        row(f, "symbols", self.symbols)?;
        row(f, "include_closure", self.include)?;
        row(f, "scopes", self.scopes)?;
        row(f, "bindings", self.bindings)?;
        row(f, "cpp_extras", self.cpp_extras)?;
        row(f, "misc_maps", self.misc)?;
        row(f, "struct_shell", self.shell)?;
        write!(f, "  {:-<20} {:>9.1} MB", "TOTAL ", mb(self.total()))
    }
}

/// Flat vec footprint: element size times capacity (backing slack counted).
#[allow(clippy::ptr_arg)]
pub(super) fn vcap<T>(v: &Vec<T>) -> usize {
    v.capacity() * std::mem::size_of::<T>()
}

/// Flat map footprint — hashbrown: ~1 control byte per slot on top of the
/// (K,V) pair.
pub(super) fn mcap<K, V>(m: &HashMap<K, V>) -> usize {
    m.capacity() * (std::mem::size_of::<(K, V)>() + 1)
}

pub(super) fn scap<T>(s: &HashSet<T>) -> usize {
    s.capacity() * (std::mem::size_of::<T>() + 1)
}

/// `HashMap<String, Vec<V>>`: flat table + deep key strings + value vecs.
pub(super) fn map_str_vec<V>(m: &HashMap<String, Vec<V>>) -> usize {
    let mut b = mcap(m);
    for (k, v) in m {
        b += k.capacity() + v.capacity() * std::mem::size_of::<V>();
    }
    b
}

impl FileAnalysis {
    /// Estimate this analysis's resident heap by bucket. See `HeapBreakdown`.
    pub fn heap_estimate(&self) -> HeapBreakdown {
        // The per-package table: flat entries + key strings + the name
        // vecs each entry owns. The `bool`/`Option` lanes ride the entry
        // struct, already counted by `mcap`.
        fn pkg_facts(m: &HashMap<String, PackageFacts>) -> usize {
            let mut b = mcap(m);
            for (k, f) in m {
                b += k.capacity()
                    + (f.parents.capacity() + f.uses.capacity() + f.requires.capacity())
                        * std::mem::size_of::<String>();
            }
            b
        }

        let mut h = HeapBreakdown {
            files: 1,
            shell: std::mem::size_of::<FileAnalysis>(),
            ..Default::default()
        };

        // refs + their indices — the dominant bucket for a big-fan-in TU.
        self.refs.heap_add(&mut h);

        // symbols + their deep strings + their name/scope indices.
        self.symbols.heap_add(&mut h);

        // witness bag.
        let (wv, wi) = self.witnesses.heap_bytes_estimate();
        h.witness_vec = wv;
        h.witness_index = wi;

        // the pack lane (include graph, macros, template params, regions)
        // and the plugin lane (namespaces, loads, emissions).
        self.pack.heap_add(&mut h);
        self.plugin.heap_add(&mut h);

        // scopes.
        h.scopes = vcap(&self.scopes)
            + self
                .scopes
                .iter()
                .map(|s| s.package.as_ref().map_or(0, |p| p.capacity()))
                .sum::<usize>();

        // The serde-skip reverse indices (rebuilt on load, resident-only).
        // The ref- and symbol-keyed shares of this bucket are added by the
        // tables' own `heap_add`.
        h.rebuilt_indices += self.scope_starts.capacity()
            * std::mem::size_of::<(Point, ScopeId)>()
            + self.export_lookup.capacity() * (std::mem::size_of::<String>() + 1)
            + self.export_lookup.iter().map(|s| s.capacity()).sum::<usize>();

        // bindings / imports.
        h.bindings = vcap(&self.imports)
            + vcap(&self.call_bindings)
            + vcap(&self.method_call_bindings)
            + vcap(&self.fold_ranges);

        // flat fact vectors. The pack and plugin lanes add their own.
        h.cpp_extras += vcap(&self.provisional_dispatches)
            + vcap(&self.guard_sites)
            + vcap(&self.arrow_deref_sites)
            + vcap(&self.gated_param_types)
            + vcap(&self.attr_projections)
            + vcap(&self.key_writes)
            + vcap(&self.flow_edges)
            + vcap(&self.loader_config_params)
            + vcap(&self.package_ranges);

        // per-package small maps/sets + export lists.
        h.misc += pkg_facts(&self.packages)
            + map_str_vec(&self.export_tags)
            + mcap(&self.type_provenance)
            + scap(&self.framework_imports)
            + scap(&self.reassigned_scalars)
            + scap(&self.column_keyed_verbs)
            + vcap(&self.export)
            + vcap(&self.export_ok)
            + vcap(&self.reexport_modules);

        h
    }
}
