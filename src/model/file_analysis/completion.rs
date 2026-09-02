//! Completion + signature help: priority constants, candidate types, the
//! completion query methods and their helpers.

use super::*;

// ---- Completion priority constants ----
//
// Lower numbers sort first. Used by both file_analysis (local completions)
// and symbols.rs (cross-file completions).

/// Variables, local subs, methods — direct scope match.
pub const PRIORITY_LOCAL: u8 = 0;
/// General subs, hash keys, keyval args — file-wide match.
pub const PRIORITY_FILE_WIDE: u8 = 10;
/// Explicitly imported via `use Foo qw(bar)`.
pub const PRIORITY_EXPLICIT_IMPORT: u8 = 12;
/// Bare `use Foo;` @EXPORT symbol (no qw list to edit).
pub const PRIORITY_BARE_IMPORT: u8 = 15;
/// Auto-add to existing `qw()` list.
pub const PRIORITY_AUTO_ADD_QW: u8 = 18;
/// Sub already used as first param (less relevant).
pub const PRIORITY_LESS_RELEVANT: u8 = 20;
/// Unimported module — inserts full `use` statement.
pub const PRIORITY_UNIMPORTED: u8 = 25;
/// Perl builtin functions (the BUILTIN resolution tier's source) — always
/// valid, but user code outranks the language's own vocabulary.
pub const PRIORITY_BUILTIN: u8 = 30;
/// Dynamic hash keys (may not exist).
pub const PRIORITY_DYNAMIC: u8 = 50;
/// Pack closure-universe names (headers' file-scope symbols) — sort after
/// every in-scope identifier (the adapter renders this tier past `z`).
pub const PRIORITY_CLOSURE: u8 = 90;

// ---- Method resolution types ----

/// One unmet role contract: `package` composes (transitively) `role`,
/// which `requires 'name'`, and nothing in `package`'s MRO provides
/// it. `via_parent` is the direct parent edge the role was reached
/// through — the `with 'X'` ref the diagnostic anchors to.
#[derive(Debug, Clone)]
pub struct UnfulfilledRequire {
    pub package: String,
    pub role: String,
    pub name: String,
    pub via_parent: String,
}

/// Result of resolving a method through the inheritance chain.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum MethodResolution {
    /// Found in a local class within this file.
    Local { class: String, sym_id: SymbolId },
    /// Found in a cross-file module. `def_module` names the module the
    /// definition actually lives in: `Some(m)` when the hit came through a
    /// plugin BRIDGE (the synthesized symbol lives in bridging module `m`, not
    /// in `class`'s own module); `None` for a real method in `class`'s module.
    /// Every consumer resolves location/signature the same way —
    /// `whole_present(get_cached(def_module.unwrap_or(class))).sub_info_view(method)` — so bridged
    /// helpers and real inherited methods share one code path.
    CrossFile { class: String, def_module: Option<String> },
}

impl MethodResolution {
    /// The class the method resolved on (both variants carry it).
    pub fn class(&self) -> &str {
        match self {
            MethodResolution::Local { class, .. } | MethodResolution::CrossFile { class, .. } => class,
        }
    }
}

/// Result of resolving a sub/method call — local symbol or cross-file metadata.
pub enum ResolvedSub<'a> {
    /// Found locally in this file's symbols.
    Local(&'a Symbol),
    /// Found in a cross-file module via ModuleIndex.
    CrossFile {
        params: Vec<ParamInfo>,
        /// Inferred type per param (parallel to `params`); `None` if unknown.
        param_types: Vec<Option<InferredType>>,
        is_method: bool,
        hash_keys: Vec<String>,
    },
}

// ---- Completion types ----

/// The model-level import fact on a completion candidate: this name is
/// importable from a module. The candidate carries the FACT; the LSP
/// adapter composes it with the slot's import affordance (where an edit
/// may land) into the actual text edit — edit shaping never happens in
/// the model.
#[derive(Debug, Clone)]
pub enum ImportFact {
    /// The name can join an existing `use module qw(...)` list whose
    /// closing paren sits at `qw_close` (an import-statement fact of the
    /// origin file).
    AddToQw { name: String, qw_close: Point },
    /// No importing `use` exists yet — accepting the candidate needs a new
    /// `use module qw(name);` statement (placement is the adapter's).
    NewUse { module: String, name: String },
}

/// A completion candidate from FileAnalysis resolution (pure table lookup).
#[derive(Debug, Clone)]
pub struct CompletionCandidate {
    pub label: String,
    pub kind: SymKind,
    pub detail: Option<String>,
    pub insert_text: Option<String>,
    pub sort_priority: u8,
    /// Additional text edits applied when this candidate is accepted.
    /// Composed by the ADAPTER (from `import_fact` + the slot's
    /// affordance, or a slot-local fix like the `.`→`->` operator swap) —
    /// model-side gathering leaves this empty.
    pub additional_edits: Vec<(Span, String)>,
    /// See `ImportFact` — the importable-from fact, when this candidate is
    /// import-sourced.
    pub import_fact: Option<ImportFact>,
    /// Plugin-provided display override. When `Some`, the LSP adapter renders
    /// the candidate with this kind instead of `kind`'s default mapping. Lets
    /// helpers/routes/DSL verbs carry their plugin-chosen icon all the way
    /// through completion without leaking plugin specifics into the core.
    pub display_override: Option<HandlerDisplay>,
}

/// Signature info for a sub/method, resolved from the symbol table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SignatureInfo {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub is_method: bool,
    /// End of the sub body — used to query inferred types for params.
    pub body_end: Point,
    /// Pre-resolved param types (for cross-file subs where body_end is meaningless).
    pub param_types: Option<Vec<Option<String>>>,
}

// ---- Completion query methods ----

impl FileAnalysis {
    /// Complete variables at a point with cross-sigil forms.
    pub fn complete_variables(&self, point: Point, sigil: char) -> Vec<CompletionCandidate> {
        let visible = self.visible_symbols(point);
        let mut seen = HashSet::<(String, char)>::new();
        let mut candidates = Vec::new();

        // Sort by scope size (innermost first) — stable priority ordering
        let mut vars: Vec<(&Symbol, usize)> = visible
            .into_iter()
            .filter(|s| matches!(s.kind, SymKind::Variable | SymKind::Field))
            .filter_map(|s| {
                if let SymbolDetail::Variable { .. } = &s.detail {
                    let scope = &self.scopes[s.scope.0 as usize];
                    let scope_size = span_size(&scope.span);
                    Some((s, scope_size))
                } else if let SymbolDetail::Field { .. } = &s.detail {
                    let scope = &self.scopes[s.scope.0 as usize];
                    let scope_size = span_size(&scope.span);
                    Some((s, scope_size))
                } else {
                    None
                }
            })
            .collect();
        vars.sort_by_key(|(_, sz)| *sz);

        for (sym, _scope_size) in vars {
            let (bare_name, decl_sigil) = match &sym.detail {
                SymbolDetail::Variable { sigil: ds, .. } => {
                    (sym.name[1..].to_string(), *ds)
                }
                SymbolDetail::Field { sigil: ds, .. } => {
                    (sym.name[1..].to_string(), *ds)
                }
                _ => continue,
            };
            let key = (bare_name.clone(), decl_sigil);
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);

            // Tier, not magnitude: an in-scope lexical must outrank every
            // cross-file tier (imports at 12/15, auto-import at 18/25,
            // builtins at 30) or the rank-then-cut cap deletes the asker's
            // own variables in favor of the workspace firehose. The old
            // `min(scope_size, 255)` spelling saturated to 255 for any
            // multi-line scope (span_size counts rows*10000), which parked
            // every real lexical BELOW all of those tiers — invisible while
            // completion was uncapped, candidate loss the day it wasn't.
            // Innermost-shadow selection still rides the scope-size sort
            // above; within a tier the client tie-breaks by label.
            let priority = match &self.scopes[sym.scope.0 as usize].kind {
                ScopeKind::File => PRIORITY_FILE_WIDE,
                _ => PRIORITY_LOCAL,
            };
            let detail = match &sym.detail {
                SymbolDetail::Variable { decl_kind, .. } => {
                    Some(match decl_kind {
                        DeclKind::My => "my".to_string(),
                        DeclKind::Our => "our".to_string(),
                        DeclKind::State => "state".to_string(),
                        DeclKind::Field => "field".to_string(),
                        DeclKind::Param => "param".to_string(),
                        DeclKind::ForVar => "for".to_string(),
                    })
                }
                SymbolDetail::Field { .. } => Some("field".to_string()),
                _ => None,
            };

            generate_cross_sigil_candidates(
                &bare_name,
                decl_sigil,
                sigil,
                detail,
                priority,
                &mut candidates,
            );
        }

        candidates
    }

    /// Complete methods for an invocant (variable or class name) at a point.
    pub fn complete_methods(
        &self,
        invocant: &str,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        let class_name = self.resolve_invocant_class(
            invocant,
            self.scope_at(point).unwrap_or(ScopeId(0)),
            point,
        );

        if let Some(ref cn) = class_name {
            // Pass `module_index` so the ancestor walk reaches CROSS-FILE
            // parents. Without it, an untyped `$self` (e.g. assigned via
            // `$class->SUPER::new`, which the bag can't yet type) resolves to
            // the enclosing class but offers only its OWN methods — inherited
            // methods from a `use parent`/`-base` ancestor vanish.
            let candidates = self.complete_methods_for_class(cn, module_index);
            if !candidates.is_empty() {
                return candidates;
            }
        }

        // Fallback: native subs/methods in file, deduped by label.
        //
        // Plugin-synthesized entries are skipped on purpose. A plugin
        // emits Methods on specific classes (Mojo helpers on
        // Controller, DBIC accessors on the schema, etc.); without a
        // known receiver type we can't say which ones apply here.
        // Surfacing them blindly dumps framework noise onto every
        // untyped `$x->` call site. Native entries stay — those are
        // always valid candidates regardless of receiver.
        let mut seen = HashSet::<String>::new();
        self.symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
            // An anonymous sub (name `(anon)`) has no callable name — never a
            // method candidate. Gate on callability, not the `(anon)` spelling.
            .filter(|s| crate::model::conventions::is_callable_sub_name(&s.name))
            // Lexicals never complete bare on a receiver; the `&name` lane
            // (`complete_lexical_methods_at`) is their one member source.
            .filter(|s| !matches!(&s.detail, SymbolDetail::Sub { lexical: true, .. }))
            .filter(|s| !s.namespace.is_framework())
            .filter(|s| seen.insert(s.name.clone()))
            .map(|s| CompletionCandidate {
                label: s.name.clone(),
                kind: s.kind,
                detail: Some(
                    if matches!(s.kind, SymKind::Method) {
                        "method"
                    } else {
                        "sub"
                    }
                    .to_string(),
                ),
                insert_text: None,
                sort_priority: PRIORITY_FILE_WIDE,
                additional_edits: vec![],
                import_fact: None,
                display_override: s.presentation.display,
            })
            .collect()
    }

    /// Complete hash keys for a resolved owner.
    fn complete_hash_keys_for_owner(
        &self,
        owner: &HashKeyOwner,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        let defs = self.hash_key_defs_for_owner(owner);
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();

        for def in defs {
            if !seen.insert(def.name.clone()) {
                continue;
            }

            let is_dynamic = matches!(
                &def.detail,
                SymbolDetail::HashKeyDef { is_dynamic: true, .. }
            );

            let detail = match owner {
                HashKeyOwner::Class(name) => format!("{}->{{{}}}", name, def.name),
                // A column is reached via its accessor / condition args, not a
                // hash deref — show the accessor form.
                HashKeyOwner::Bridged { class } => format!("{}->{}", class, def.name),
                HashKeyOwner::Variable { name, .. } => format!("{}{{{}}}", name, def.name),
                HashKeyOwner::Sub { name, .. } => format!("{}()->{{{}}}", name, def.name),
            };

            candidates.push(CompletionCandidate {
                label: def.name.clone(),
                kind: SymKind::Variable,
                detail: Some(detail),
                insert_text: None,
                sort_priority: if is_dynamic { PRIORITY_DYNAMIC } else { PRIORITY_FILE_WIDE },
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }

        // Imported return-hash keys live on the producer's real HashKeyDef, not
        // a local stub — reach them cross-file (the same scan enrichment does
        // for the owner fixup, run at query time so completion has one source).
        if let (HashKeyOwner::Sub { name, .. }, Some(idx)) = (owner, module_index) {
            if let Some((_pkg, keys)) = self.imported_sub_keys(name, idx) {
                for key in keys {
                    if seen.insert(key.clone()) {
                        candidates.push(CompletionCandidate {
                            label: key.clone(),
                            kind: SymKind::Variable,
                            detail: Some(format!("{}()->{{{}}}", name, key)),
                            insert_text: None,
                            sort_priority: PRIORITY_FILE_WIDE,
                            additional_edits: vec![],
                            import_fact: None,
                            display_override: None,
                        });
                    }
                }
            }
        }

        candidates
    }

    /// `(producer package, return-hash keys)` of an imported sub, resolved
    /// cross-file through the index. The producer's real `HashKeyDef`s are the
    /// single source: completion reads keys here and the deferred owner reads
    /// the package, exactly as rename/references/goto-def reach them via the
    /// owner edge — no consumer-side stub is materialized.
    pub(super) fn imported_sub_keys(
        &self,
        sub_name: &str,
        module_index: &dyn CrossFileLookup,
    ) -> Option<(Option<String>, Vec<String>)> {
        for import in &self.imports {
            // The exporting package may be split — the sub (and its keys)
            // live in whichever candidate defines it.
            let Some(cached) = module_index.candidate_defining_sub(&import.module_name, sub_name)
            else { continue };
            let whole = module_index.whole_present(&cached);
            for sym in &whole.symbols {
                if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                if sym.name != sub_name { continue; }
                if !whole.exports_name(&sym.name) { continue; }
                if let Some(sub_info) = whole.sub_info_view(&sym.name) {
                    let hk = sub_info.hash_keys();
                    if !hk.is_empty() {
                        return Some((sym.package.clone(), hk.to_vec()));
                    }
                }
            }
        }
        None
    }

    /// Complete hash keys for a variable at a point.
    pub fn complete_hash_keys(
        &self,
        var_text: &str,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        match self.resolve_hash_key_owner(var_text, point) {
            Some(owner) => self.complete_hash_keys_for_owner(&owner, module_index),
            None => Vec::new(),
        }
    }

    /// Complete hash keys for a known class name (from expression type resolution).
    pub fn complete_hash_keys_for_class(
        &self,
        class_name: &str,
        _point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        self.complete_hash_keys_for_owner(&HashKeyOwner::Class(class_name.to_string()), module_index)
    }

    /// Complete hash keys for a sub's return value (from expression type resolution).
    ///
    /// Tries the caller's enclosing package first (local subs), then an
    /// unpackaged owner, then the cross-file producer (imported subs' return
    /// keys live on the producer's real HashKeyDef, reached via the index).
    pub fn complete_hash_keys_for_sub(
        &self,
        sub_name: &str,
        _point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        // Try each candidate owner variant until one has defs.
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let push_unique = |cands: Vec<CompletionCandidate>, out: &mut Vec<CompletionCandidate>, seen: &mut HashSet<String>| {
            for c in cands {
                if seen.insert(c.label.clone()) {
                    out.push(c);
                }
            }
        };
        for sym in &self.symbols {
            if (sym.kind == SymKind::Sub || sym.kind == SymKind::Method) && sym.name == sub_name {
                let owner = HashKeyOwner::Sub { package: sym.package.clone(), name: sub_name.to_string() };
                push_unique(self.complete_hash_keys_for_owner(&owner, module_index), &mut out, &mut seen);
            }
        }
        let imported_owner = HashKeyOwner::Sub { package: None, name: sub_name.to_string() };
        push_unique(self.complete_hash_keys_for_owner(&imported_owner, module_index), &mut out, &mut seen);

        // Final sweep: match any HashKeyDef whose owner is Sub{name: sub_name}
        // regardless of package. Covers plugin-synthesized options (e.g.
        // `Sub { package: Minion, name: enqueue }` from the minion plugin)
        // where the sub itself isn't in the local symbol table.
        let pseudo_syms: Vec<_> = self.symbols.iter()
            .filter(|s| {
                if !matches!(s.kind, SymKind::HashKeyDef) { return false; }
                matches!(&s.detail, SymbolDetail::HashKeyDef {
                    owner: HashKeyOwner::Sub { name, .. }, ..
                } if name == sub_name)
            })
            .collect();
        for def in pseudo_syms {
            if seen.insert(def.name.clone()) {
                let detail = format!("{}() option", sub_name);
                out.push(CompletionCandidate {
                    label: def.name.clone(),
                    kind: SymKind::Variable,
                    detail: Some(detail),
                    insert_text: None,
                    sort_priority: PRIORITY_FILE_WIDE,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                });
            }
        }

        // Body-derived keys: if the sub has a final hashref-ish param
        // (`sub foo { my ($x, $opts) = @_; $opts->{priority} }`), the
        // key accesses in the body reveal the expected option names.
        // Mirror of `complete_keyval_args` but for the nested-hash-literal
        // call shape (`foo($x, { | })`) routed through HashKey context.
        for sym in &self.symbols {
            if sym.name != sub_name { continue; }
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
            let params = match &sym.detail {
                SymbolDetail::Sub { params, .. } => params,
                _ => continue,
            };
            let hashish = params
                .iter()
                .find(|p| p.is_slurpy && p.name.starts_with('%'))
                .or_else(|| params
                    .last()
                    .filter(|p| !p.is_invocant && p.name.starts_with('$')));
            let bare_name = match hashish {
                Some(p) if p.name.len() > 1 => &p.name[1..],
                _ => continue,
            };
            let Some(body_scope) = self.find_body_scope(sym) else { continue };
            for k in self.hash_keys_in_scope(bare_name, body_scope) {
                if seen.insert(k.clone()) {
                    out.push(CompletionCandidate {
                        label: k,
                        kind: SymKind::Variable,
                        detail: Some(format!("{}() option", sub_name)),
                        insert_text: None,
                        sort_priority: PRIORITY_FILE_WIDE,
                        additional_edits: vec![],
                        import_fact: None,
                        display_override: None,
                    });
                }
            }
        }

        out
    }

    /// Lexical methods (`my method name`) callable at `point`, offered with
    /// the `&` call-syntax prefix — `$invocant->&name(...)` is the only
    /// spelling that dispatches one, so the inserted text must carry it.
    /// Scope rule matches the bare lexical-sub gate: visible from the
    /// declaration down, within the declaring block only. The class-keyed
    /// MRO walk excludes these symbols entirely (they don't dispatch by
    /// name and are invisible cross-file); this lane is their one source.
    pub fn complete_lexical_methods_at(&self, point: Point) -> Vec<CompletionCandidate> {
        let mut out = Vec::new();
        for sym in &self.symbols {
            if !matches!(sym.kind, SymKind::Method) {
                continue;
            }
            if !matches!(&sym.detail, SymbolDetail::Sub { lexical: true, .. }) {
                continue;
            }
            if !crate::model::conventions::is_callable_sub_name(&sym.name) {
                continue;
            }
            let enclosing = &self.scope(sym.scope).span;
            let visible = (point.row, point.column)
                >= (sym.span.start.row, sym.span.start.column)
                && (point.row, point.column) <= (enclosing.end.row, enclosing.end.column);
            if !visible {
                continue;
            }
            out.push(CompletionCandidate {
                label: format!("&{}", sym.name),
                kind: SymKind::Method,
                detail: Some("my method".to_string()),
                insert_text: Some(format!("&{}", sym.name)),
                sort_priority: PRIORITY_LOCAL,
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }
        out
    }

    /// General completion: all variables (all sigils) + subs + packages.
    pub fn complete_general(&self, point: Point) -> Vec<CompletionCandidate> {
        let mut candidates = Vec::new();

        // Variables (all sigils). The sigil-lane contract is "the client's
        // buffer already holds the typed sigil", so every variable
        // insert_text omits it (the client word-replaces the part after the
        // sigil). This is the BARE-cursor lane — no sigil exists in the
        // buffer, so inserting that text verbatim writes sigil-less code
        // (`emit('connect', self)`). Restore the requested sigil onto the
        // insert; the label already carries it.
        for sigil in ['$', '@', '%'] {
            candidates.extend(self.complete_variables(point, sigil).into_iter().map(|mut c| {
                c.insert_text = Some(match c.insert_text.take() {
                    Some(t) => format!("{sigil}{t}"),
                    None => c.label.clone(),
                });
                c
            }));
        }

        // Subs
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Sub | SymKind::Method)
                && crate::model::conventions::is_callable_sub_name(&sym.name)
            {
                // A lexical sub (`my sub helper`) is callable only inside
                // its declaring block, from its declaration down — offering
                // it file-wide completes a name that would not compile.
                if let SymbolDetail::Sub { lexical: true, .. } = &sym.detail {
                    // A lexical METHOD has no bare-call spelling at all — it
                    // dispatches only as `$invocant->&name`; the member lane
                    // (`complete_lexical_methods_at`) owns it.
                    if matches!(sym.kind, SymKind::Method) {
                        continue;
                    }
                    let enclosing = &self.scope(sym.scope).span;
                    let visible = (point.row, point.column)
                        >= (sym.span.start.row, sym.span.start.column)
                        && (point.row, point.column) <= (enclosing.end.row, enclosing.end.column);
                    if !visible {
                        continue;
                    }
                }
                candidates.push(CompletionCandidate {
                    label: sym.name.clone(),
                    kind: sym.kind,
                    detail: Some(
                        if matches!(sym.kind, SymKind::Method) {
                            "method"
                        } else {
                            "sub"
                        }
                        .to_string(),
                    ),
                    insert_text: None,
                    sort_priority: PRIORITY_FILE_WIDE,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
        }

        // Packages/classes
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Package | SymKind::Class) {
                candidates.push(CompletionCandidate {
                    label: sym.name.clone(),
                    kind: sym.kind,
                    detail: Some(
                        if matches!(sym.kind, SymKind::Class) {
                            "class"
                        } else {
                            "package"
                        }
                        .to_string(),
                    ),
                    insert_text: None,
                    sort_priority: PRIORITY_LESS_RELEVANT,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
        }

        candidates
    }

    /// Complete keyval args at a call site.
    /// Returns `key =>` completions for unused keys.
    pub fn complete_keyval_args(
        &self,
        call_name: &str,
        is_method: bool,
        invocant: Option<&str>,
        point: Point,
        used_keys: &HashSet<String>,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<CompletionCandidate> {
        // For constructor calls on a class, check for :param fields
        if crate::model::conventions::is_constructor_name(call_name) {
            if let Some(inv) = invocant {
                let class_name = self.resolve_invocant_class(
                    inv,
                    self.scope_at(point).unwrap_or(ScopeId(0)),
                    point,
                );
                if let Some(ref cn) = class_name {
                    let param_candidates = self.class_param_completions(cn, used_keys);
                    if !param_candidates.is_empty() {
                        return param_candidates;
                    }
                }
            }
        }

        // Find the sub definition (local or cross-file)
        let resolved = match self.find_sub_for_call(call_name, is_method, invocant, point, module_index) {
            Some(r) => r,
            None => return Vec::new(),
        };

        match resolved {
            ResolvedSub::Local(sub_sym) => {
                let params = match &sub_sym.detail {
                    SymbolDetail::Sub { params, .. } => params,
                    _ => return Vec::new(),
                };

                // Pick the param that carries key=>value pairs:
                //   * slurpy `%opts` is the classic shape
                //   * final `$opts` scalar deref'd as a hashref in the
                //     body (`$opts->{…}`) is the same pattern — we
                //     collect the same way, just strip the `$` sigil
                //     when scanning the body. Previously only slurpy
                //     worked; every "options-hashref" sub missed out.
                let hashish = params
                    .iter()
                    .find(|p| p.is_slurpy && p.name.starts_with('%'))
                    .or_else(|| params
                        .last()
                        .filter(|p| !p.is_invocant && p.name.starts_with('$')));
                let slurpy_name = match hashish {
                    Some(p) => {
                        if p.name.starts_with('%') || p.name.starts_with('$') || p.name.starts_with('@') {
                            &p.name[1..]
                        } else {
                            &p.name
                        }
                    }
                    None => return Vec::new(),
                };

                // Find hash key accesses for this param name within the sub's body scope
                let body_scope = self.find_body_scope(sub_sym);
                let keys = match body_scope {
                    Some(scope_id) => self.hash_keys_in_scope(slurpy_name, scope_id),
                    None => Vec::new(),
                };
                // Bail silently when the chosen scalar doesn't actually
                // get deref'd as a hash — the last-param heuristic is
                // loose; no accesses means "not an options param".
                if keys.is_empty() { return Vec::new(); }

                keys.into_iter()
                    .filter(|k| !used_keys.contains(k))
                    .map(|k| CompletionCandidate {
                        label: format!("{} =>", k),
                        kind: SymKind::Variable,
                        detail: Some(format!("{}(%{})", call_name, slurpy_name)),
                        insert_text: Some(format!("{} => ", k)),
                        sort_priority: PRIORITY_LOCAL,
                        additional_edits: vec![],
                import_fact: None,
                display_override: None,
                    })
                    .collect()
            }
            ResolvedSub::CrossFile { hash_keys, params, .. } => {
                // Check if any param is slurpy %hash
                let has_slurpy = params.iter().any(|p| p.is_slurpy && p.name.starts_with('%'));
                if !has_slurpy || hash_keys.is_empty() {
                    return Vec::new();
                }

                hash_keys.into_iter()
                    .filter(|k| !used_keys.contains(k))
                    .map(|k| CompletionCandidate {
                        label: format!("{} =>", k),
                        kind: SymKind::Variable,
                        detail: Some(format!("{}()", call_name)),
                        insert_text: Some(format!("{} => ", k)),
                        sort_priority: PRIORITY_LOCAL,
                        additional_edits: vec![],
                import_fact: None,
                display_override: None,
                    })
                    .collect()
            }
        }
    }

    /// Resolve signature info for a call (sub/method name).
    pub fn signature_for_call(
        &self,
        name: &str,
        is_method: bool,
        invocant: Option<&str>,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<SignatureInfo> {
        let resolved = self.find_sub_for_call(name, is_method, invocant, point, module_index)?;

        match resolved {
            ResolvedSub::Local(sub_sym) => {
                let (params, sym_is_method) = match &sub_sym.detail {
                    SymbolDetail::Sub { params, is_method, .. } => (params.clone(), *is_method),
                    _ => return None,
                };

                let mut params = params;
                let is_method = is_method
                    || sym_is_method
                    || params.first().map_or(false, |p| {
                        crate::model::conventions::is_conventional_invocant_name(&p.name)
                    });

                // Strip the implicit invocant from the display list.
                // `is_invocant` covers both Perl-native `$self`/`$class`
                // (flagged by the builder at extract time) and plugin-
                // marked framework invocants (`$c` for helpers, etc.).
                if !params.is_empty() && params[0].is_invocant {
                    params.remove(0);
                }

                Some(SignatureInfo {
                    name: name.to_string(),
                    params,
                    is_method,
                    body_end: sub_sym.span.end,
                    param_types: None, // local — use inferred_type() with body_end
                })
            }
            ResolvedSub::CrossFile {
                params: cross_params,
                param_types: cross_param_types,
                is_method: cf_is_method,
                ..
            } => {
                let mut params: Vec<ParamInfo> = cross_params;
                let mut param_types: Vec<Option<String>> = cross_param_types
                    .into_iter()
                    .map(|t| t.as_ref().map(inferred_type_to_tag))
                    .collect();

                let is_method = is_method
                    || cf_is_method
                    || params.first().map_or(false, |p| {
                        crate::model::conventions::is_conventional_invocant_name(&p.name)
                    });

                // Same invocant-strip as the local branch — by flag,
                // not by name. Cross-file ParamInfo carries the flag
                // through the cache (set by the plugin or builder at
                // build time), so `$c` on a helper is dropped the
                // same way `$self` on a Perl method is.
                if !params.is_empty() && params[0].is_invocant {
                    params.remove(0);
                    if !param_types.is_empty() {
                        param_types.remove(0);
                    }
                }

                Some(SignatureInfo {
                    name: name.to_string(),
                    params,
                    is_method,
                    body_end: Point::new(0, 0),
                    param_types: Some(param_types),
                })
            }
        }
    }

    // ---- Internal completion helpers ----

    /// Find a sub/method by name, optionally scoped to a class.
    /// Returns `ResolvedSub::Local` for same-file symbols, or `ResolvedSub::CrossFile`
    /// for inherited methods and imported functions found via `ModuleIndex`.
    fn find_sub_for_call<'s>(
        &'s self,
        name: &str,
        is_method: bool,
        invocant: Option<&str>,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<ResolvedSub<'s>> {
        let scope = self.scope_at(point).unwrap_or(ScopeId(0));
        // A fully-qualified / SUPER call token (`$o->Foo::Bar::m`, `SUPER::m`)
        // names its dispatch class explicitly — split it so lookups use the
        // bare tail scoped to the qualifier, overriding the invocant-derived
        // class (Perl ignores the invocant's class for the lookup). SUPER
        // resolves over the enclosing package's parent MRO.
        let token = crate::model::conventions::MethodToken::parse(name);
        let name = token.name();
        let fq_class = match token {
            crate::model::conventions::MethodToken::Super(tail) => self
                .enclosing_class_for_scope(scope)
                .and_then(|e| self.resolve_super_method(&e, tail, module_index))
                .map(|r| r.class().to_string()),
            t => t.literal_package().map(str::to_string),
        };
        // Resolve class name for scoped lookup
        let class_name = if fq_class.is_some() {
            fq_class
        } else if is_method {
            invocant.and_then(|inv| self.resolve_invocant_class(inv, scope, point))
        } else {
            None
        };

        // Try inheritance-aware class-scoped lookup first
        if let Some(ref cn) = class_name {
            match self.resolve_method_in_ancestors(cn, name, module_index) {
                Some(MethodResolution::Local { sym_id, .. }) => {
                    return Some(ResolvedSub::Local(self.symbol(sym_id)));
                }
                Some(MethodResolution::CrossFile { ref class, .. }) => {
                    if let Some(idx) = module_index {
                        // Symbol-disambiguated: the candidate defining `name`.
                        if let Some(cached) = idx.candidate_defining_sub(class, name) {
                            let whole = idx.bag_present(&cached);
                            if let Some(sub_info) = whole.sub_info_view(name) {
                                return Some(cross_file_resolved(&sub_info));
                            }
                        }
                    }
                }
                None => {}
            }
        }

        // Fallback: any local sub/method with that name
        for &sid in self.symbols_named(name) {
            let sym = self.symbol(sid);
            if matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                return Some(ResolvedSub::Local(sym));
            }
        }

        // Fallback: imported function via ModuleIndex. Resolution routes
        // through `imported_names` — the SAME bound-set evaluator the
        // unresolved-function diagnostic reads (symbols.rs) — so goto-def and
        // the diagnostic can never disagree on whether a name is brought in by
        // this `use`. A bare `use M;` binds `@EXPORT`; `:tag` binds the tag's
        // members; `-as` binds local→origin; `use M ();` binds nothing.
        if !is_method {
            if let Some(idx) = module_index {
                for import in &self.imports {
                    // A split exporter's surface lives across its candidates.
                    let bound_remote = idx
                        .visible_def_candidates(&import.module_name)
                        .iter()
                        .find_map(|cached| {
                            let surface = cached.analysis.export_surface_with_index(idx);
                            imported_names(import, &surface)
                                .iter()
                                .find(|(local, _)| local == name)
                                .map(|(_local, remote)| remote.clone())
                        });
                    let Some(remote) = bound_remote else { continue };
                    // The name may be defined in the directly-`use`d module
                    // or in a module it re-exports. `defining_module_cached`
                    // chases the same re-export edges (seen-set bounded);
                    // the candidate pick covers a split module's own subs.
                    if let Some(cached) = idx
                        .defining_module_cached(&import.module_name, &remote)
                        .or_else(|| idx.candidate_defining_sub(&import.module_name, &remote))
                    {
                        let whole = idx.bag_present(&cached);
                        if let Some(sub_info) = whole.sub_info_view(&remote) {
                            return Some(cross_file_resolved(&sub_info));
                        }
                    }
                }
            }
        }

        None
    }

    /// Resolve a variable text to a HashKeyOwner for hash key completion.
    fn resolve_hash_key_owner(&self, var_text: &str, point: Point) -> Option<HashKeyOwner> {
        let bare_name = if var_text.starts_with('$') || var_text.starts_with('@') || var_text.starts_with('%') {
            &var_text[1..]
        } else {
            var_text
        };

        // Try type inference → class owner (bag-routed). Hash-
        // key context: read `hash_key_class()` so Parametric
        // values narrow to their row-class arg (DBIC `$row->{name}`
        // after `find` etc.). For non-Parametric this is
        // equivalent to `class_name()`. CLAUDE.md invariant #10.
        if let Some(it) = self.inferred_type_via_bag(var_text, point) {
            if let Some(cn) = it.hash_key_class() {
                return Some(HashKeyOwner::Class(cn.to_string()));
            }
        }

        // Check call bindings → follow to sub's return hash keys
        for cb in &self.call_bindings {
            if cb.variable == var_text
                && cb.span.start <= point
                && contains_point(&self.scopes[cb.scope.0 as usize].span, point)
            {
                let package = self.sub_defining_package(&cb.func_name);
                return Some(HashKeyOwner::Sub { package, name: cb.func_name.clone() });
            }
        }

        // Check method call bindings → follow to method's return hash keys.
        // Ownership keys on {invocant class, method}: the invocant's class
        // walks the MRO to the DEFINING symbol, so a same-named method on an
        // unrelated class can't claim the keys. An invocant that doesn't
        // type (or a definer outside the local ancestry) keeps the
        // name-only fallback for recall.
        for mcb in &self.method_call_bindings {
            if mcb.variable == var_text
                && mcb.span.start <= point
                && contains_point(&self.scopes[mcb.scope.0 as usize].span, point)
            {
                let package = self
                    .resolve_invocant_class(&mcb.invocant_var, mcb.scope, mcb.span.start)
                    .and_then(|cn| {
                        match self.resolve_method_in_ancestors(&cn, &mcb.method_name, None) {
                            Some(MethodResolution::Local { sym_id, .. }) => {
                                self.symbol(sym_id).package.clone()
                            }
                            _ => None,
                        }
                    })
                    .or_else(|| self.sub_defining_package(&mcb.method_name));
                return Some(HashKeyOwner::Sub { package, name: mcb.method_name.clone() });
            }
        }

        // Try resolving the variable declaration → Variable owner
        // For $hash{}, try %hash first
        let try_names: Vec<String> = if var_text.starts_with('$') {
            vec![format!("%{}", bare_name), var_text.to_string()]
        } else {
            vec![var_text.to_string()]
        };

        for name in &try_names {
            if let Some(sym) = self.resolve_variable(name, point) {
                return Some(HashKeyOwner::Variable {
                    name: name.clone(),
                    def_scope: sym.scope,
                });
            }
        }

        // Check if any existing hash key refs/defs use this bare_name
        for sym in &self.symbols {
            if let SymbolDetail::HashKeyDef { ref owner, .. } = sym.detail {
                match owner {
                    HashKeyOwner::Variable { name, .. } => {
                        let owner_bare = if name.starts_with('$') || name.starts_with('@') || name.starts_with('%') {
                            &name[1..]
                        } else {
                            name
                        };
                        if owner_bare == bare_name {
                            return Some(owner.clone());
                        }
                    }
                    HashKeyOwner::Class(_)
                    | HashKeyOwner::Bridged { .. }
                    | HashKeyOwner::Sub { .. } => {}
                }
            }
        }

        None
    }

    /// Look up the defining package of a sub/method by name. Returns None when
    /// the sub is not found locally (imported, or absent). Used to package-
    /// qualify `HashKeyOwner::Sub` so distinct same-name subs in different
    /// packages don't collide at query time.
    fn sub_defining_package(&self, name: &str) -> Option<String> {
        for sym in &self.symbols {
            if (sym.kind == SymKind::Sub || sym.kind == SymKind::Method) && sym.name == name {
                return sym.package.clone();
            }
        }
        None
    }

    /// Collect :param field names from a core class as keyval completions.
    fn class_param_completions(
        &self,
        class_name: &str,
        used_keys: &HashSet<String>,
    ) -> Vec<CompletionCandidate> {
        let mut candidates = Vec::new();
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Field) {
                if let SymbolDetail::Field { ref attributes, .. } = sym.detail {
                    if attributes.contains(&"param".to_string()) {
                        // Check this field belongs to the class
                        if self.symbol_in_class(sym.id, class_name) {
                            let key = sym.bare_name().to_string();
                            if !used_keys.contains(&key) {
                                candidates.push(CompletionCandidate {
                                    label: format!("{} =>", key),
                                    kind: SymKind::Variable,
                                    detail: Some(format!("{}->new(:param)", class_name)),
                                    insert_text: Some(format!("{} => ", key)),
                                    sort_priority: PRIORITY_LOCAL,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                                });
                            }
                        }
                    }
                }
            }
        }
        candidates
    }

    /// Find hash key names accessed via a variable in a specific scope.
    fn hash_keys_in_scope(&self, var_bare_name: &str, scope_id: ScopeId) -> Vec<String> {
        let scope_span = &self.scopes[scope_id.0 as usize].span;
        let mut keys = Vec::new();
        let mut seen = HashSet::new();

        for r in self.refs() {
            if let RefKind::HashKeyAccess { ref var_text, .. } = r.kind {
                // Check the var_text's bare name matches
                let ref_bare = if var_text.starts_with('$')
                    || var_text.starts_with('@')
                    || var_text.starts_with('%')
                {
                    &var_text[1..]
                } else {
                    var_text.as_str()
                };
                if ref_bare == var_bare_name && contains_point(scope_span, r.span.start) {
                    if !seen.contains(&r.target_name) {
                        seen.insert(r.target_name.clone());
                        keys.push(r.target_name.clone());
                    }
                }
            }
        }

        keys
    }
}

/// Generate cross-sigil completion candidates for a variable.
fn generate_cross_sigil_candidates(
    bare_name: &str,
    decl_sigil: char,
    requested_sigil: char,
    detail: Option<String>,
    priority: u8,
    out: &mut Vec<CompletionCandidate>,
) {
    match requested_sigil {
        '$' => {
            if decl_sigil == '$' {
                out.push(CompletionCandidate {
                    label: format!("${}", bare_name),
                    kind: SymKind::Variable,
                    detail: detail.clone(),
                    insert_text: Some(bare_name.to_string()),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
            if decl_sigil == '@' {
                out.push(CompletionCandidate {
                    label: format!("${}[]", bare_name),
                    kind: SymKind::Variable,
                    detail: detail.clone().or(Some(format!("@{}", bare_name))),
                    insert_text: Some(format!("{}[", bare_name)),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
                out.push(CompletionCandidate {
                    label: format!("$#{}", bare_name),
                    kind: SymKind::Variable,
                    detail: detail
                        .clone()
                        .or(Some(format!("last index of @{}", bare_name))),
                    insert_text: Some(format!("#{}", bare_name)),
                    sort_priority: priority.saturating_add(1),
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
            if decl_sigil == '%' {
                out.push(CompletionCandidate {
                    label: format!("${}{{}}", bare_name),
                    kind: SymKind::Variable,
                    detail: detail.clone().or(Some(format!("%{}", bare_name))),
                    insert_text: Some(format!("{}{{", bare_name)),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
        }
        '@' => {
            if decl_sigil == '@' {
                out.push(CompletionCandidate {
                    label: format!("@{}", bare_name),
                    kind: SymKind::Variable,
                    detail: detail.clone(),
                    insert_text: Some(bare_name.to_string()),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
                out.push(CompletionCandidate {
                    label: format!("@{}[]", bare_name),
                    kind: SymKind::Variable,
                    detail: Some("array slice".to_string()),
                    insert_text: Some(format!("{}[", bare_name)),
                    sort_priority: priority.saturating_add(1),
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
            if decl_sigil == '%' {
                out.push(CompletionCandidate {
                    label: format!("@{}{{}}", bare_name),
                    kind: SymKind::Variable,
                    detail: detail.clone().or(Some("hash slice".to_string())),
                    insert_text: Some(format!("{}{{", bare_name)),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
        }
        '%' => {
            if decl_sigil == '%' {
                out.push(CompletionCandidate {
                    label: format!("%{}", bare_name),
                    kind: SymKind::Variable,
                    detail: detail.clone(),
                    insert_text: Some(bare_name.to_string()),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
                out.push(CompletionCandidate {
                    label: format!("%{}{{}}", bare_name),
                    kind: SymKind::Variable,
                    detail: Some("hash kv slice".to_string()),
                    insert_text: Some(format!("{}{{", bare_name)),
                    sort_priority: priority.saturating_add(1),
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
            if decl_sigil == '@' {
                out.push(CompletionCandidate {
                    label: format!("%{}[]", bare_name),
                    kind: SymKind::Variable,
                    detail: Some("array kv slice".to_string()),
                    insert_text: Some(format!("{}[", bare_name)),
                    sort_priority: priority,
                    additional_edits: vec![],
                import_fact: None,
                display_override: None,
                });
            }
        }
        _ => {}
    }
}

// ---- Helpers ----

pub(crate) fn contains_point(span: &Span, point: Point) -> bool {
    (span.start.row < point.row || (span.start.row == point.row && span.start.column <= point.column))
        && (point.row < span.end.row || (point.row == span.end.row && point.column <= span.end.column))
}

pub(super) fn span_size(span: &Span) -> usize {
    // Use row difference as primary size metric; column as tiebreaker
    let rows = span.end.row.saturating_sub(span.start.row);
    let cols = if rows == 0 {
        span.end.column.saturating_sub(span.start.column)
    } else {
        0
    };
    rows * 10000 + cols
}

/// Strip the implicit invocant param from a handler signature so hover
/// and sig help don't include the `$self` the user never types.
pub(super) fn display_handler_params(params: &[ParamInfo]) -> Vec<String> {
    params
        .iter()
        .filter(|p| !p.is_invocant)
        .map(|p| p.name.clone())
        .collect()
}

pub(super) fn source_line_at(source: &str, row: usize) -> &str {
    source.lines().nth(row).unwrap_or("")
}

/// Serialize an InferredType to a simple string tag.
/// Used by signature help's `param_types` field, which piggy-backs on the
/// pre-unification string representation for backwards-compatible JSON output.
pub fn inferred_type_to_tag(ty: &InferredType) -> String {
    match ty {
        InferredType::ClassName(name) => format!("Object:{}", name),
        InferredType::FirstParam { package } => format!("Object:{}", package),
        InferredType::HashRef => "HashRef".to_string(),
        // Structurally-typed hashes read as plain HashRef on the wire —
        // the per-key detail drives narrowing, not display (yet).
        InferredType::HashWithKeys { .. } => "HashRef".to_string(),
        InferredType::ArrayRef => "ArrayRef".to_string(),
        InferredType::CodeRef { .. } => "CodeRef".to_string(),
        InferredType::Regexp => "Regexp".to_string(),
        InferredType::Numeric => "Numeric".to_string(),
        InferredType::String => "String".to_string(),
        // Method dispatch on a Parametric uses its `class_name()`
        // (= the flavor's dispatch class), so the tag follows.
        // Type-arg detail lives in the richer
        // `format_inferred_type` rendering.
        InferredType::Parametric(p) => match p.class_name() {
            Some(c) => format!("Object:{}", c),
            None => "Parametric".to_string(),
        },
        InferredType::Sequence(_) => "Sequence".to_string(),
        // A constraint is a Type::Tiny object; method dispatch (deferred)
        // routes there, so tag it as such rather than as its inner type.
        InferredType::TypeConstraintOf(_) => "Object:Type::Tiny".to_string(),
        // Method dispatch is against the base; tag like any object.
        InferredType::BrandedRoute { base, .. } => format!("Object:{}", base),
        // Optional dispatches nowhere until narrowed; tag the inner so the
        // wire format stays backward-compatible, prefixed Maybe.
        InferredType::Optional(inner) => format!("Maybe:{}", inferred_type_to_tag(inner)),
        InferredType::Undef => "Undef".to_string(),
        InferredType::Bool => "Bool".to_string(),
        // Never reaches a renderer: scrubbed to `None` at the registry
        // boundary. Named so the match stays exhaustive.
        InferredType::Unknown => "Unknown".to_string(),
    }
}

/// Format a cross-file method signature from a SubInfo view.
pub(super) fn format_cross_file_signature(method_name: &str, sub_info: &SubInfo<'_>) -> String {
    let params = sub_info.params();
    if params.is_empty() {
        format!("sub {}()", method_name)
    } else {
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        format!("sub {}({})", method_name, names.join(", "))
    }
}

/// Build a `ResolvedSub::CrossFile` from a SubInfo view, snapshotting owned data.
fn cross_file_resolved(sub_info: &SubInfo<'_>) -> ResolvedSub<'static> {
    let params: Vec<ParamInfo> = sub_info.params().to_vec();
    let param_types: Vec<Option<InferredType>> = params
        .iter()
        .map(|p| sub_info.param_inferred_type(&p.name))
        .collect();
    ResolvedSub::CrossFile {
        params,
        param_types,
        is_method: sub_info.is_method(),
        hash_keys: sub_info.hash_keys().to_vec(),
    }
}

pub(crate) fn format_inferred_type(ty: &InferredType) -> String {
    match ty {
        InferredType::ClassName(name) => name.clone(),
        InferredType::FirstParam { package } => package.clone(),
        InferredType::HashRef => "HashRef".to_string(),
        // Structurally-typed hashes read as plain HashRef on the wire —
        // the per-key detail drives narrowing, not display (yet).
        InferredType::HashWithKeys { .. } => "HashRef".to_string(),
        InferredType::ArrayRef => "ArrayRef".to_string(),
        InferredType::CodeRef { .. } => "CodeRef".to_string(),
        InferredType::Regexp => "Regexp".to_string(),
        InferredType::Numeric => "Numeric".to_string(),
        InferredType::String => "String".to_string(),
        InferredType::Parametric(p) => format_parametric_type(p),
        InferredType::Sequence(elems) => {
            // Angle brackets, not `[...]` — markdown renderers treat
            // bracketed text as link syntax and either swallow it
            // or render it as a broken link. Matches the
            // `Parametric<T1, T2>` style.
            // Elide long tuples — a 64-slot literal's hover shouldn't be
            // a wall of element types.
            let mut parts: Vec<String> =
                elems.iter().take(4).map(format_inferred_type).collect();
            if elems.len() > 4 {
                parts.push("…".to_string());
            }
            format!("Sequence<{}>", parts.join(", "))
        }
        InferredType::TypeConstraintOf(inner) => {
            format!("TypeConstraint<{}>", format_inferred_type(inner))
        }
        InferredType::BrandedRoute { base, controller, .. } => match controller {
            Some(c) => format!("{}<controller={}>", base, c),
            None => base.clone(),
        },
        InferredType::Optional(inner) => format!("Maybe<{}>", format_inferred_type(inner)),
        InferredType::Undef => "Undef".to_string(),
        InferredType::Bool => "Bool".to_string(),
        // Never reaches a renderer: scrubbed to `None` at the registry
        // boundary. Named so the match stays exhaustive.
        InferredType::Unknown => "Unknown".to_string(),
    }
}

pub(super) fn format_parametric_type(p: &ParametricType) -> String {
    match p {
        ParametricType::ResultSet { base, row } => {
            format!("{}<{}>", base, row)
        }
        // Presentation keeps the args (`b: Box<Widget>`) even though
        // dispatch projects the base.
        ParametricType::Instance { base, .. } => {
            p.exact_spelling().unwrap_or_else(|| base.clone())
        }
    }
}
