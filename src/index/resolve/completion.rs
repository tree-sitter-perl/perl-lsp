//! The completion projections: candidate SOURCES only (`complete`,
//! `complete_modules`, qualified-path and pack-qualified gathering) —
//! cursor-context slot detection stays in the LSP layer.
use super::*;

impl<'a> CandidateSet<'a> {
    /// The def location of a named type (a Class symbol — enum/struct/
    /// typedef — or a namespace/module), local first, then cross-file by
    /// name. Used by the spec-ladder type gd and the bare-word fallback.
    pub(super) fn type_def_location(&self, type_name: &str, idx: &dyn CrossFileLookup) -> Option<RefLocation> {
        let wanted =
            |k: &SymKind| matches!(k, SymKind::Class | SymKind::Package | SymKind::Module);
        if let Some(sym) = self
            .origin
            .symbols()
            .iter()
            .find(|s| s.name == type_name && wanted(&s.kind))
        {
            return Some(self.origin_decl(sym.selection_span));
        }
        // Whichever candidate file declares the type symbol — not the
        // name-slot winner.
        idx.visible_def_candidates(type_name).iter().find_map(|cached| {
            let whole = idx.whole_present(cached);
            let sym = whole
                .symbols()
                .iter()
                .find(|s| s.name == type_name && wanted(&s.kind))?;
            Some(RefLocation {
                key: FileKey::Path(cached.path.clone()),
                span: sym.selection_span,
                access: AccessKind::Declaration,
                rewritable: true,
                label: None
            })
        })
    }

    /// Completion visibility: unlike the navigation projections there is no
    /// resolved target to run `references_mask_for` on (the cursor sits on a
    /// prefix, not a name), so the default is the full VISIBLE universe; the
    /// construction-time override still narrows it — the same one knob that
    /// narrows references/rename.
    pub(super) fn completion_visibility(&self) -> RoleMask {
        self.visibility_override.unwrap_or(RoleMask::VISIBLE)
    }

    /// Completion candidate gathering: the prefix-enumeration of the same
    /// visible universe the navigation projections resolve against. This is
    /// the SOURCE of identifier candidates only — cursor-context gating
    /// (which slot the cursor is in) and item presentation stay in the LSP
    /// adapter. Sources by tier:
    ///
    /// - OPEN — the origin file's in-scope names (variables, subs,
    ///   packages: the origin is the document being edited, i.e. the open
    ///   tier by definition of the completion verb) and the names its `use`
    ///   statements explicitly import (origin-file facts; the dep cache only
    ///   enriches their detail).
    /// - DEPENDENCY — names supplied by other modules' export surfaces:
    ///   the rest of an imported module's `@EXPORT`/`@EXPORT_OK`, and every
    ///   cached exporter's surface as auto-import candidates.
    ///
    /// `import_slot` is the slot's import affordance: whether accepting an
    /// import-sourced name here has somewhere to land its `use` edit.
    /// `false` means the slot offers no import-sourced names at all (today:
    /// every slot except the general identifier slot) — an import candidate
    /// without a place for its edit would complete to broken code. The
    /// candidates carry the importable-from FACT (`ImportFact`); the
    /// adapter composes fact + affordance into the edit.
    ///
    /// The general slot passes `""` (clients filter by prefix); a non-empty
    /// prefix narrows server-side for callers that want it.
    pub fn complete(
        &self,
        prefix: &str,
        import_slot: bool,
    ) -> Vec<CompletionCandidate> {
        let mask = self.completion_visibility();
        // Pack routing: the identifier universe is the origin's #include
        // closure — C's import surface ("C = Perl, everything exported": the
        // closure IS the import list, so enum constants, free functions,
        // typedefs and globals from included headers are candidates exactly
        // like imported subs are for Perl). Same projection, same mask knob;
        // the sources differ per routing because the languages' name-supply
        // models differ, not the seam.
        if self.pack {
            let mut out = Vec::new();
            if mask.contains(RoleMask::DEPENDENCY) && !self.origin.pack.include_closure.is_empty() {
                if let Some(idx) = self.module_index {
                    let visible: std::collections::HashSet<String> =
                        self.origin.pack.include_closure.iter_strs().map(|a| a.as_ref().to_owned()).collect();
                    // Many candidate names come from the same header —
                    // resolve each FILE's whole view once per request, not
                    // once per name (the LRU absorbs misses, but even hits
                    // pay a map probe + recency write).
                    let mut whole_memo: std::collections::HashMap<
                        PathBuf,
                        std::sync::Arc<crate::model::file_analysis::FileAnalysis>,
                    > = std::collections::HashMap::new();
                    for (name, cached) in idx.visible_defs_with_prefix(prefix, &visible) {
                        // Only linkage-visible defs (a TU-static never
                        // completes elsewhere). Symbol detail (kind, parent
                        // enum) reads the whole view — the resident copy may
                        // be symbol-evicted.
                        let whole = whole_memo
                            .entry(cached.path.clone())
                            .or_insert_with(|| idx.whole_present(&cached))
                            .clone();
                        let Some(sym) = whole
                            .symbols_named(&name)
                            .iter()
                            .map(|id| whole.symbol(*id))
                            .find(|s| whole.is_linkage_visible(s))
                        else {
                            continue;
                        };
                        let header = cached
                            .path
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        // An enum constant carries its parent enum as
                        // `package` — "opcode — opnames.h" reads the domain
                        // at a glance.
                        let detail = match sym.package.as_deref() {
                            Some(p) if !p.is_empty() => format!("{} — {}", p, header),
                            _ => header,
                        };
                        out.push(CompletionCandidate {
                            label: name.clone(),
                            is_static: false,
                            kind: sym.kind.clone(),
                            detail: Some(detail),
                            insert_text: None,
                            sort_priority: crate::model::file_analysis::PRIORITY_CLOSURE,
                            additional_edits: vec![],
                            import_fact: None,
                            display_override: None,
                        });
                    }
                }
            }
            return out;
        }
        let mut out = Vec::new();
        if mask.contains(RoleMask::OPEN) {
            out.extend(self.origin.complete_general(self.point));
        }
        // BUILTIN tier: the Perl builtin surface (`model::builtins`) is the
        // tier's name source — the same authority diagnostics suppression
        // and builtin hover ask, so a name offered here is never flagged
        // unresolved. Perl-only by construction (the pack arm returned
        // above); callable builtins only, keywords stay suppression-side.
        if mask.contains(RoleMask::BUILTIN) {
            out.extend(crate::model::builtins::builtin_functions().map(|name| {
                CompletionCandidate {
                    label: name.to_string(),
                    is_static: false,
                    kind: SymKind::Sub,
                    detail: Some("perl builtin".to_string()),
                    insert_text: None,
                    sort_priority: crate::model::file_analysis::PRIORITY_BUILTIN,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                }
            }));
        }
        if let (true, Some(idx)) = (import_slot, self.module_index) {
            import_candidates(self.origin, idx, mask, &mut out);
            if mask.contains(RoleMask::DEPENDENCY) {
                unimported_export_candidates(self.origin, idx, &mut out);
            }
        }
        if !prefix.is_empty() {
            out.retain(|c| c.label.starts_with(prefix));
        }
        out
    }

    /// The loadable-module half of the completion universe: names a `use`
    /// statement (or a `Foo::` path drill) can reach, as
    /// (name, is_resolved). Dependency-tier by construction — both the
    /// resolved module cache and the @INC availability scan live behind the
    /// index. Workspace-package names are a documented gap: the store holds
    /// their analyses but no gathering source enumerates them yet, here or
    /// pre-seam (see the ADR's honest-boundary list). In-file package names
    /// ride `complete()`'s OPEN tier instead.
    pub fn complete_modules(&self, prefix: &str) -> Vec<(String, bool)> {
        let mask = self.completion_visibility();
        let mut out = Vec::new();
        if mask.contains(RoleMask::DEPENDENCY) {
            if let Some(idx) = self.module_index {
                out.extend(idx.complete_module_names(prefix));
            }
        }
        out
    }

    /// `complete_modules` shaped into candidates: indexed modules rank
    /// above merely-available ones. Presentation (the MODULE kind, the
    /// availability detail) rides the candidate so the one adapter
    /// projection reproduces the `use`-line / path-drill module half.
    pub fn complete_module_candidates(&self, prefix: &str) -> Vec<CompletionCandidate> {
        self.complete_modules(prefix)
            .into_iter()
            .map(|(name, is_resolved)| {
                let (detail, sort_priority) = if is_resolved {
                    (Some("indexed".to_string()), 10u8)
                } else {
                    (Some("available".to_string()), 50u8)
                };
                CompletionCandidate {
                    label: name,
                    is_static: false,
                    kind: SymKind::Module,
                    detail,
                    insert_text: None,
                    sort_priority,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                }
            })
            .collect()
    }

    /// Candidates for a `Package::<cursor>` drill: the subs declared in (or
    /// inherited by) `package` — bare-name inserts so the typed prefix stays
    /// put (tier 10) — plus the sub-packages nested under it, both the
    /// loadable modules the set's module universe knows and the in-file
    /// `package Package::Other` names its OPEN tier holds (tier 20, labelled
    /// by the suffix so the client's `Package::<typed>` filter matches).
    pub fn complete_qualified_path(
        &self,
        module_index: &dyn CrossFileLookup,
        package: &str,
    ) -> Vec<CompletionCandidate> {
        // Pack routing: the qualifier names a namespace/class owner; the
        // candidates are its members, gathered through the SAME
        // owner-membership predicate owner-anchored goto-def resolves with.
        // Same projection, per-routing sources — like `complete()`.
        if self.pack {
            return self.complete_pack_qualified(module_index, package);
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<CompletionCandidate> = Vec::new();

        for c in self.origin.complete_methods_for_class(package, Some(module_index)) {
            if !seen.insert(c.label.clone()) {
                continue;
            }
            out.push(CompletionCandidate {
                label: c.label.clone(),
                is_static: false,
                kind: SymKind::Sub,
                detail: c.detail.or_else(|| Some(format!("from {}", package))),
                insert_text: Some(c.label),
                sort_priority: 10,
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }

        let prefix = format!("{}::", package);
        let mut subpaths: Vec<(String, &'static str)> = Vec::new();
        for (name, is_resolved) in self.complete_modules(&prefix) {
            subpaths.push((name, if is_resolved { "indexed" } else { "available" }));
        }
        for c in self.complete(&prefix, false) {
            if !matches!(c.kind, SymKind::Package | SymKind::Class) {
                continue;
            }
            subpaths.push((c.label, "in-file"));
        }
        for (name, hint) in subpaths {
            let suffix = match name.strip_prefix(&prefix) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            if !seen.insert(suffix.clone()) {
                continue;
            }
            out.push(CompletionCandidate {
                label: suffix.clone(),
                is_static: false,
                kind: SymKind::Module,
                detail: Some(hint.to_string()),
                insert_text: Some(suffix),
                sort_priority: 20,
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }
        out
    }

    /// Pack half of the qualified-path drill (`fmtx::<cursor>`): the members
    /// of the owner the qualifier names — never the global pool. Per file,
    /// membership is `pack_member_of` over the inline-expanded owner set
    /// (inline namespaces are transparent), plus the nested containers
    /// (sub-namespaces, types) filed directly under the owner. Sources by
    /// tier: OPEN = the origin's own symbols; DEPENDENCY = every cached file
    /// closure-connected to the origin — the same connectivity the
    /// owner-anchored goto-def scan walks, so completion offers exactly what
    /// gd can resolve. Empty when the qualifier resolves nothing (e.g. a
    /// macro-guarded namespace open left members unattributed) — the caller
    /// falls through to the bare-identifier universe, mirroring gd.
    pub(super) fn complete_pack_qualified(
        &self,
        module_index: &dyn CrossFileLookup,
        owner: &str,
    ) -> Vec<CompletionCandidate> {
        let mask = self.completion_visibility();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<CompletionCandidate> = Vec::new();
        let gather = |fa: &FileAnalysis,
                      header: Option<&str>,
                      seen: &mut std::collections::HashSet<String>,
                      out: &mut Vec<CompletionCandidate>| {
            let owners = pack_inline_owner_set(fa, owner);
            for s in fa.symbols() {
                let nested_container = matches!(s.kind, SymKind::Package | SymKind::Class)
                    && s.package.as_deref().is_some_and(|p| owners.iter().any(|o| o == p));
                if !nested_container && !pack_member_of(fa, s, &owners) {
                    continue;
                }
                // a default-named symbol is structure, not an addressable name
                if s.attributes.iter().any(|a| a == "anonymous") {
                    continue;
                }
                if !seen.insert(s.name.clone()) {
                    continue;
                }
                let detail = match (s.package.as_deref(), header) {
                    (Some(p), Some(h)) if !p.is_empty() => Some(format!("{} — {}", p, h)),
                    (_, Some(h)) => Some(h.to_string()),
                    (Some(p), None) if !p.is_empty() => Some(p.to_string()),
                    _ => None,
                };
                out.push(CompletionCandidate {
                    label: s.name.clone(),
                    is_static: false,
                    kind: s.kind.clone(),
                    detail,
                    insert_text: None,
                    sort_priority: if nested_container { 20 } else { 10 },
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                });
            }
        };
        if mask.contains(RoleMask::OPEN) {
            gather(self.origin, None, &mut seen, &mut out);
        }
        if mask.contains(RoleMask::DEPENDENCY) {
            if let Some((self_path, visible)) = module_index.visibility_scope() {
                let self_str = self_path.to_string_lossy().into_owned();
                module_index.for_each_cached_file(&mut |cached| {
                    let p = cached.path.to_string_lossy();
                    let connected = visible.contains(p.as_ref())
                        || cached.analysis.pack.include_closure.contains(&self_str);
                    if !connected {
                        return;
                    }
                    let header =
                        cached.path.file_name().map(|f| f.to_string_lossy().into_owned());
                    // The gather reads symbols — closure-connected copies may
                    // be symbol-evicted; the LRU bounds the rehydration.
                    let whole = module_index.whole_present(cached);
                    gather(&whole, header.as_deref(), &mut seen, &mut out);
                });
            }
        }
        out
    }
}
