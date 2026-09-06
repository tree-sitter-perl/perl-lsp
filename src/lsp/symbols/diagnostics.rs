//! Diagnostics: unresolved names, the narrowing family, `DiagnosticOptions`.

use super::*;

/// Every diagnostic code this adapter mints, spelled ONCE. Metrics key on
/// these strings (per-file yield counts in the ghost lane), so a literal at a
/// mint site is a typo away from a silently separate metric bucket — the
/// wide-table drift failure in string form.
pub mod codes {
    pub const UNRESOLVED_FUNCTION: &str = "unresolved-function";
    pub const UNRESOLVED_METHOD: &str = "unresolved-method";
    pub const UNDEF_DEREF: &str = "undef-deref";
    pub const OPTIONAL_DEREF: &str = "optional-deref";
    pub const DEREF_SHAPE_MISMATCH: &str = "deref-shape-mismatch";
    pub const ROLE_REQUIRES_UNFULFILLED: &str = "role-requires-unfulfilled";
    pub const HELPER_NOT_LOADED: &str = "helper-not-loaded";
    pub const UNRESOLVED_DISPATCH: &str = "unresolved-dispatch";
    pub const UNKNOWN_HASH_KEY: &str = "unknown-hash-key";
}

// ---- Diagnostics ----

/// Opt-in diagnostic toggles. Defaults are all-off for the QA/plugin-author
/// channels (noise for end users); the always-on hints (`unresolved-function`
/// / `unresolved-method`) ignore this.
///
/// **The struct is the schema.** `rename_all = "camelCase"` makes each field
/// its own LSP key under `initializationOptions.diagnostics`, so `backend.rs`
/// parses the whole block with one `serde_json::from_value` — no hand-mapped
/// key strings to drift. `default` fills any absent key with `false`. The CLI
/// surface (`DiagnosticOptions::from_cli_args`) is the one spelling serde
/// can't derive; `cli_flags_match_diagnostic_option_fields` guards it against
/// drift. A `Config` god-struct, a generated editor schema, and richer
/// per-code config are a design note in `docs/prompt-config-schema.md`. See
/// `docs/adr/receiver-gated-dispatch.md`, `docs/adr/narrowing-diagnostics.md`.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiagnosticOptions {
    /// Fire `unresolved-dispatch` when a known dispatch verb's receiver can't
    /// be typed (`GateResult::ReceiverUntyped`) — never on a settled
    /// `DoesNotApply`. Off by default.
    pub unresolved_dispatch: bool,
    /// Fire `use-after-move` on the decidable subset (straight-line, in-function,
    /// local-only moved-then-used). Pack-language (C++) channel, off by default —
    /// it is a heuristic-adjacent lane whose honest subset is narrow. See
    /// `use_after_move_reads` / `docs/adr/use-after-move.md`.
    pub use_after_move: bool,
    /// Extend `unresolved-method` past locally-defined classes to any
    /// cross-file-resolvable class (D8). The local case is always-on; this
    /// opt-in lifts the `is_local_class` gate so a narrowed or otherwise
    /// cross-file-typed receiver (`$x->isa('Some::Dep'); $x->bogus`) is
    /// checked too, gated by the same complete-ancestry honest-silent valve.
    /// Off by default: cross-file classes carry more codegen/XS methods the
    /// static walker can't see (the diag-09/10 Log4perl-accessor class), so
    /// it earns trust before promotion. See docs/adr/narrowing-diagnostics.md.
    pub unresolved_method_cross_file: bool,
    /// Fire `optional-deref` (D2) when a receiver is `Optional<T>` at an
    /// unguarded use point (a possible undef deref — the strictNullChecks
    /// analog). Narrowing strips the `Optional` under a dominating
    /// `defined`/`blessed` guard, so a surviving `Optional` is unguarded by
    /// construction. "May be undef", not "is" — opt-in, INFORMATION severity,
    /// with a guard-insertion quick-fix. Off by default.
    pub optional_deref: bool,
    /// Fire `redundant-guard` (D3) / `contradictory-guard` (D4): a guard whose
    /// outcome is constant given the subject's prior type (`if (defined $x)`
    /// where `$x` is already a confident value; `$x->isa('Foo')` where `$x` is
    /// already `Foo` or an unrelated class). Off by default — needs confident
    /// prior types and MRO relatedness, so it earns trust before promotion.
    pub redundant_guard: bool,
    /// Fire `deref-shape-mismatch` (D6): a deref whose form demands one
    /// container rep while a `ref…eq` guard proved another (`$x->{k}` on
    /// array/code, `$x->[i]` on hash/code, `$x->()` on hash/array) — a
    /// guaranteed runtime die. Guard-narrowed reps only; objects are never a
    /// mismatch. Off by default.
    pub deref_shape: bool,
}

impl DiagnosticOptions {
    /// Parse the opt-in flags from CLI args (`--optional-deref`, …). The kebab
    /// flag for each field mirrors its serde camelCase key; the mapping is
    /// explicit here (serde doesn't parse argv) and pinned by
    /// `cli_flags_match_diagnostic_option_fields`.
    pub fn from_cli_args(args: &[String]) -> Self {
        let has = |flag: &str| args.iter().any(|a| a == flag);
        DiagnosticOptions {
            unresolved_dispatch: has("--unresolved-dispatch"),
            use_after_move: has("--use-after-move"),
            unresolved_method_cross_file: has("--unresolved-method-cross-file"),
            optional_deref: has("--optional-deref"),
            redundant_guard: has("--redundant-guard"),
            deref_shape: has("--deref-shape"),
        }
    }
}

pub fn collect_diagnostics(
    analysis: &FileAnalysis,
    module_index: &ModuleIndex,
    options: DiagnosticOptions,
) -> Vec<Diagnostic> {
    crate::util::ghost_stats::count("collect_diagnostics");
    let mut diagnostics = Vec::new();

    // Plugin-emitted diagnostics (pattern lints) — already decided at
    // build time; here they only render. Severity vocabulary is the
    // plugin's; unknown strings degrade to HINT rather than shouting.
    for pd in &analysis.plugin.diagnostics {
        diagnostics.push(Diagnostic {
            range: span_to_range(pd.span),
            severity: Some(match pd.severity.as_str() {
                "error" => DiagnosticSeverity::ERROR,
                "warning" => DiagnosticSeverity::WARNING,
                "info" => DiagnosticSeverity::INFORMATION,
                _ => DiagnosticSeverity::HINT,
            }),
            code: Some(NumberOrString::String(pd.code.clone())),
            source: Some(format!("perl-lsp/{}", pd.plugin_id)),
            message: pd.message.clone(),
            ..Default::default()
        });
    }

    // Snapshot each `use` once: its bound set (local→remote) and, when the
    // producer is cached, the names on its (transitive) export surface. The
    // resolvability verdict for a given call name is then a map lookup against
    // this snapshot — the same logic as `classify_import`, but the surface walk
    // and `imported_names` allocation happen once per import instead of once per
    // (unresolved-ref × import) on every diagnostics publish (every keystroke).
    // Diagnostics need only the import + verdict, not the producer path or the
    // remote name `classify_import` also returns — so neither is computed here.
    struct ImportBinding<'a> {
        import: &'a crate::model::file_analysis::Import,
        /// local → remote for everything this `use` brings into scope.
        bound: HashMap<String, String>,
        /// Names on the producer's export surface; `None` when not yet cached.
        exported: Option<std::collections::HashSet<String>>,
    }
    // The tail is ~87% unattributed after decode and hit-path overhead. These
    // four regions are the whole body of `collect_diagnostics` below the plugin
    // render, so their sum bounds the per-file cost from the inside rather than
    // by subtraction — which is the step that produced two wrong per-item costs
    // today.
    let _g_imports = crate::util::ghost_stats::ScopedNs::start("diag.1_import_bindings");
    let import_bindings: Vec<ImportBinding> = analysis
        .imports
        .iter()
        .map(|import| {
            // Union the export surface across EVERY candidate file of the
            // producer — a split exporter's surface (and thus a false
            // "not exported" verdict) must not hinge on the name-slot winner.
            let cands = module_index.visible_def_candidates(&import.module_name);
            let (bound, exported) = if !cands.is_empty() {
                let mut bound: HashMap<String, String> = HashMap::new();
                let mut all: std::collections::HashSet<String> = Default::default();
                for c in &cands {
                    let surface = c.analysis.export_surface_with_index(module_index);
                    bound.extend(crate::model::file_analysis::imported_names(import, &surface));
                    all.extend(surface.all_names());
                }
                (bound, Some(all))
            } else {
                // Producer not cached yet: only an explicitly-named import can be
                // judged `Brought` (tags / bare-use defaults need the surface).
                let bound = import
                    .imported_symbols
                    .iter()
                    .map(|s| (s.local_name.clone(), s.remote().to_string()))
                    .collect();
                (bound, None)
            };
            ImportBinding { import, bound, exported }
        })
        .collect();

    // Best resolution of a call name across all imports: `Brought` dominates
    // `ExportedNotBrought`. Mirrors `resolve_imported_function_classified` over
    // the precomputed snapshot.
    drop(_g_imports);
    let resolve_name = |name: &str| -> Option<(&crate::model::file_analysis::Import, ImportResolution)> {
        let mut best: Option<(&crate::model::file_analysis::Import, ImportResolution)> = None;
        for b in &import_bindings {
            let res = if b.bound.contains_key(name) {
                ImportResolution::Brought
            } else if b.exported.as_ref().is_some_and(|e| e.contains(name)) {
                ImportResolution::ExportedNotBrought
            } else {
                continue;
            };
            if matches!(best, Some((_, ImportResolution::Brought))) {
                continue;
            }
            best = Some((b.import, res));
        }
        best
    };

    let _g_fn = crate::util::ghost_stats::ScopedNs::start("diag.2_unresolved_fn_loop");
    for r in analysis.refs() {
        if !matches!(r.kind, RefKind::FunctionCall { .. }) {
            continue;
        }
        let name = &r.target_name;

        // Skip package-qualified calls like Foo::bar()
        if crate::model::file_analysis::split_qualified(name).0.is_some() {
            continue;
        }

        // Skip code deref calls like &{$var}()
        if name.starts_with('&') {
            continue;
        }

        // Names the Perl language owns never resolve to user code: the
        // model's builtin surface (the same authority the BUILTIN
        // resolution tier and builtin hover read) plus the indirect-object
        // constructor convention (`new Foo(...)` parses as a call named
        // `new`).
        if crate::model::builtins::is_builtin(name)
            || crate::model::conventions::is_constructor_name(name)
        {
            continue;
        }

        // Skip locally defined subs
        if !analysis.symbols_named(name).is_empty() {
            continue;
        }

        // Skip functions implicitly imported by OOP frameworks (has, extends, etc.)
        if analysis.framework_imports.contains(name.as_str()) {
            continue;
        }

        // Single resolvability verdict — the same query goto-def reads, so a
        // name goto-def can jump to is never flagged as unresolved here (NAV
        // § (c)). `Brought` = the name is in scope (named in qw, pulled in by a
        // `:tag` selector against the producer surface, or auto-imported by a
        // bare `use`); `ExportedNotBrought` = importable but not yet in the qw
        // list → actionable hint.
        //
        // Bare-use auto-import deliberately treats `export_ok` as brought:
        // runtime exporters (Moose::Exporter->setup_import_methods etc.) record
        // their names in `export_ok` because the builder can't tell "runtime
        // default" from "explicit opt-in" at parse time, so flagging them
        // produced ~684 FPs (Moose::Util::TypeConstraints &c.). Traditional
        // opt-in `@EXPORT_OK` on a bare use is suppressed too — accepted.
        let range = span_to_range(r.span);
        let resolution = resolve_name(name);
        match resolution {
            Some((_, ImportResolution::Brought)) => continue,
            Some((import, ImportResolution::ExportedNotBrought)) => {
                diagnostics.push(Diagnostic {
                    range,
                    severity: Some(DiagnosticSeverity::HINT),
                    code: Some(NumberOrString::String(codes::UNRESOLVED_FUNCTION.into())),
                    source: Some("perl-lsp".into()),
                    message: format!(
                        "'{}' is exported by {} but not imported",
                        name, import.module_name,
                    ),
                    data: Some(serde_json::json!({
                        "module": import.module_name,
                        "function": name,
                    })),
                    ..Default::default()
                });
            }
            None => {
                // Search ALL cached modules for this function.
                let exporters = module_index.find_exporters(name);
                if !exporters.is_empty() {
                    let msg = if exporters.len() == 1 {
                        format!(
                            "'{}' is exported by {} (not yet imported)",
                            name, exporters[0],
                        )
                    } else {
                        format!(
                            "'{}' is exported by {} and {} other module(s)",
                            name,
                            exporters[0],
                            exporters.len() - 1,
                        )
                    };
                    diagnostics.push(Diagnostic {
                        range,
                        severity: Some(DiagnosticSeverity::HINT),
                        code: Some(NumberOrString::String(codes::UNRESOLVED_FUNCTION.into())),
                        source: Some("perl-lsp".into()),
                        message: msg,
                        data: Some(serde_json::json!({
                            "modules": exporters,
                            "function": name,
                        })),
                        ..Default::default()
                    });
                } else {
                    // HINT (not INFORMATION): an unresolved bareword call is
                    // often a genuinely-dynamic sub (AUTOLOAD, runtime glob
                    // install, a not-installed dep) the static walker can't see.
                    // Keep it the quietest visible severity so a Moose/AUTOLOAD-
                    // heavy codebase doesn't light up the Problems panel.
                    diagnostics.push(Diagnostic {
                        range,
                        severity: Some(DiagnosticSeverity::HINT),
                        code: Some(NumberOrString::String(codes::UNRESOLVED_FUNCTION.into())),
                        source: Some("perl-lsp".into()),
                        message: format!("'{}' is not defined in this file", name),
                        ..Default::default()
                    });
                }
            }
        }
    }

    drop(_g_fn);
    // 5e: Unresolved method diagnostics for locally-defined classes.
    //
    // Core owns ONLY the true `UNIVERSAL::` surface — the methods every Perl
    // class answers regardless of framework. Everything framework-shaped
    // (DBIC's `DBIx::Class::Core` inheritance, Moose's meta) is declared by
    // the plugin that knows it, via the `meta_methods()` manifest, and
    // arrives baked on the analysis. A per-framework name list here would be
    // a partial enumeration that silently misses tomorrow's framework
    // (rule #10).
    //
    // Built once as a set, not scanned per ref: the loop below runs per
    // method-call ref (it carries its own ghost-stats region for that
    // reason), and the plugin half is an OPEN set — a third-party plugin can
    // declare as many meta-methods as it likes, so a linear scan here
    // degrades with the plugin roster rather than staying bounded.
    let implicitly_answered: std::collections::HashSet<&str> =
        ["new", "AUTOLOAD", "DESTROY", "can", "isa", "DOES", "VERSION"]
            .into_iter()
            .chain(analysis.plugin.meta_methods.iter().map(|s| s.as_str()))
            .collect();
    let _g_meth = crate::util::ghost_stats::ScopedNs::start("diag.3_unresolved_method_loop");
    for r in analysis.refs() {
        let (invocant, _invocant_span) = match &r.kind {
            // A plugin-bridged token is plugin-resolved, not a receiver we
            // can flag as an unresolved method — skip it.
            RefKind::MethodCall { invocant, invocant_span, .. } => match invocant.as_name() {
                Some(n) => (n, invocant_span),
                None => continue,
            },
            _ => continue,
        };
        let method_name = &r.target_name;

        // Skip methods every class of its kind answers without declaring.
        if implicitly_answered.contains(method_name.as_str()) {
            continue;
        }

        // Skip SUPER::-qualified and other package-qualified method names.
        // `$self->SUPER::foo()` stores `target_name = "SUPER::foo"`; trying
        // to find a method literally named "SUPER::foo" in the MRO always
        // fails. Caller-side package dispatch (`Class::method`) is intentional
        // and not our job to validate here.
        use crate::model::conventions::{InvocantText, MethodToken};
        if !matches!(MethodToken::parse(method_name), MethodToken::Bare(_)) {
            continue;
        }

        // Resolve invocant to class name. Diagnostics stays bag-only for
        // scalars — no enclosing-class fallback, which would manufacture
        // warnings on untyped invocants — and skips everything else.
        let class_name = match invocant.classify() {
            InvocantText::Bareword(b) => Some(b.to_string()),
            InvocantText::Scalar(_) => analysis.inferred_type_via_bag(invocant, r.span.start)
                .and_then(|ty| ty.class_name().map(|s| s.to_string())),
            _ => None,
        };
        let class_name = match class_name {
            Some(cn) => cn,
            None => continue,
        };

        // Fire for classes we can fully see. Always-on: classes defined in
        // THIS file (high precision — you wrote it, the walker sees its
        // methods). Opt-in (D8): also cross-file-resolvable classes, so a
        // narrowed or cross-file-typed receiver is checked. A class that is
        // neither local nor cached is external/uninstalled — stay silent, we
        // can't enumerate its methods. The complete-ancestry valve below is
        // the shared honest-silent guard for both.
        let is_local_class = analysis.symbols().iter().any(|s| {
            matches!(s.kind, FaSymKind::Class | FaSymKind::Package) && s.name == class_name
        });
        let is_cached_class =
            options.unresolved_method_cross_file && module_index.get_cached(&class_name).is_some();
        if !is_local_class && !is_cached_class {
            continue;
        }

        // A local class must define ≥1 method we can see (else it's likely a
        // forward decl / external alias re-opened here). A cached cross-file
        // class is already a real module — its methods live in its analysis,
        // which `resolve_method_in_ancestors` consults below.
        let has_methods = is_cached_class
            || analysis.symbols().iter().any(|s| {
                matches!(s.kind, FaSymKind::Sub | FaSymKind::Method)
                    && analysis.symbol_in_class(s.id, &class_name)
            });
        if !has_methods {
            continue;
        }

        // Check if the method exists in the class (walks inheritance chain)
        if analysis.resolve_method_in_ancestors(&class_name, method_name, Some(module_index)).is_some() {
            continue;
        }

        // A class with `AUTOLOAD` anywhere in its MRO answers ANY method name at
        // runtime, so the static `sub` set isn't its real surface — stay silent
        // (the role-contracts diagnostic uses the same skip, file_analysis.rs).
        if analysis.resolve_method_in_ancestors(&class_name, "AUTOLOAD", Some(module_index)).is_some() {
            continue;
        }

        // Honest-silent on an incomplete ISA chain: if `class_name` (or any
        // resolvable ancestor) names a parent we can't resolve in the
        // workspace or @INC, the method might be inherited from there. One
        // predicate gates EVERY invocant-typing path (`$self`/FirstParam and
        // direct `Pkg->m` alike), so they can't drift (rule #10).
        if analysis.class_has_unresolved_ancestor(&class_name, Some(module_index)) {
            continue;
        }

        diagnostics.push(Diagnostic {
            range: span_to_range(r.span),
            severity: Some(DiagnosticSeverity::HINT),
            code: Some(NumberOrString::String(codes::UNRESOLVED_METHOD.into())),
            source: Some("perl-lsp".into()),
            message: format!(
                "'{}' is not defined in {}",
                method_name, class_name,
            ),
            ..Default::default()
        });
    }

    // 5g: undef-deref (D1) — a method call or hash deref on a receiver the
    // lattice proves is `Undef` at that point (the `else` of `if defined`,
    // the fall-through after `return if defined`, an `unless defined` body).
    // Runtime is a hard die. Maximal confidence — the type *is* undef, not
    // *may be* — so this is always-on `WARNING`, the one narrowing diagnostic
    // that doesn't wait behind an opt-in flag (rule #10: it reads the type
    // at the use point, never the syntax). See docs/adr/narrowing-diagnostics.md.
    // D2 (`optional-deref`) shares this same lattice read: a receiver typed
    // `Optional<T>` at an UNGUARDED use point — narrowing already strips the
    // `Optional` wherever a `defined`/`blessed` guard dominates, so a
    // surviving `Optional` here is unguarded by construction. "May be undef",
    // not "is" → opt-in, INFORMATION, with a guard-insertion quick-fix.
    for site in analysis.deref_receiver_sites(Some(module_index)) {
        match &site.receiver_ty {
            InferredType::Undef => {
                diagnostics.push(Diagnostic {
                    range: span_to_range(site.span),
                    severity: Some(DiagnosticSeverity::WARNING),
                    code: Some(NumberOrString::String(codes::UNDEF_DEREF.into())),
                    source: Some("perl-lsp".into()),
                    message: format!(
                        "'{}' is undef here; {} on it dies at runtime",
                        site.receiver,
                        site.form.access_phrase(),
                    ),
                    ..Default::default()
                });
            }
            InferredType::Optional(_) if options.optional_deref => {
                diagnostics.push(Diagnostic {
                    range: span_to_range(site.span),
                    severity: Some(DiagnosticSeverity::INFORMATION),
                    code: Some(NumberOrString::String(codes::OPTIONAL_DEREF.into())),
                    source: Some("perl-lsp".into()),
                    // The quick-fix reads the receiver back to synthesize
                    // `return unless defined $r;`.
                    data: Some(serde_json::json!({ "receiver": site.receiver })),
                    message: format!(
                        "'{}' may be undef here; {} on it could die — guard with `defined`",
                        site.receiver,
                        site.form.access_phrase(),
                    ),
                    ..Default::default()
                });
            }
            _ => {}
        }

        // D6 — a deref whose form demands one container rep while a `ref…eq`
        // guard proved the receiver is another (a guaranteed runtime die).
        // Read the GUARD-narrowed rep specifically: a deref self-infers its
        // own demanded rep as a zero-extent witness at the use point, masking
        // any conflict under the merged query, so only a guard surfaces here.
        // `RepKind::of` answers `None` for objects (overloadable) — never a
        // mismatch.
        if options.deref_shape {
            if let Some(demanded) = site.form.demands_rep() {
                if let Some(rep) = analysis
                    .guard_narrowed_rep(&site.receiver, site.span.start)
                    .and_then(|t| crate::model::file_analysis::RepKind::of(&t))
                {
                    if rep != demanded {
                        diagnostics.push(Diagnostic {
                            range: span_to_range(site.span),
                            severity: Some(DiagnosticSeverity::WARNING),
                            code: Some(NumberOrString::String(codes::DEREF_SHAPE_MISMATCH.into())),
                            source: Some("perl-lsp".into()),
                            message: format!(
                                "'{}' is {} here; {} dies at runtime",
                                site.receiver,
                                rep.noun(),
                                site.form.access_phrase(),
                            ),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }

    // D3/D4 — a guard whose outcome the lattice already fixes: redundant
    // (always true → the `else` is dead) or contradictory (always false →
    // the `then` is dead). Opt-in; gated hard on confident prior types in
    // `guard_redundancies` (rule #10 — the type answers, never the syntax).
    if options.redundant_guard {
        for g in analysis.guard_redundancies(Some(module_index)) {
            let code = match g.verdict {
                GuardVerdict::AlwaysTrue => "redundant-guard",
                GuardVerdict::AlwaysFalse => "contradictory-guard",
            };
            let message = render_guard_message(&g);
            diagnostics.push(Diagnostic {
                range: span_to_range(g.span),
                severity: Some(DiagnosticSeverity::INFORMATION),
                code: Some(NumberOrString::String(code.into())),
                source: Some("perl-lsp".into()),
                message,
                ..Default::default()
            });
        }
    }

    // 5f: role-requires-unfulfilled — the composer-mismatch contract
    // check (docs/adr/role-contracts.md). WARNING, not HINT: Perl
    // dies at composition time for this. Anchored to the `with 'Role'`
    // PackageRef inside the composing package; the package decl is the
    // fallback (e.g. the parent edge came from a raw `@ISA` push).
    for u in analysis.unfulfilled_role_requires(Some(module_index)) {
        let span = analysis
            .refs()
            .iter()
            .find(|r| {
                matches!(r.kind, RefKind::PackageRef)
                    && r.target_name == u.via_parent
                    && analysis.package_at(r.span.start) == Some(u.package.as_str())
            })
            .map(|r| r.span)
            .or_else(|| {
                analysis
                    .symbols()
                    .iter()
                    .find(|s| {
                        matches!(s.kind, FaSymKind::Package | FaSymKind::Class)
                            && s.name == u.package
                    })
                    .map(|s| s.selection_span)
            });
        let Some(span) = span else { continue };
        diagnostics.push(Diagnostic {
            range: span_to_range(span),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String(codes::ROLE_REQUIRES_UNFULFILLED.into())),
            source: Some("perl-lsp".into()),
            message: format!(
                "role {} requires '{}'; {} does not provide it",
                u.role, u.name, u.package,
            ),
            ..Default::default()
        });
    }

    drop(_g_meth);

    // 5h: helper-not-loaded — the entrypoint-scan lint
    // (docs/prompt-helper-consumption.md phase 2). A method call whose
    // ONLY resolution is a plugin bridge from a WORKSPACE module that
    // no workspace file loads (imports literally or via the SyntheticUse
    // a `plugin 'X'` line emits). Installed CPAN plugins are exempt —
    // the "downloaded = intended" policy keeps resolution generous and
    // makes precision this lint's job. HINT severity.
    {
        use crate::model::conventions::{InvocantText, MethodToken};
        let mut seen: std::collections::HashSet<(String, String)> = Default::default();
        let _g_narrow = crate::util::ghost_stats::ScopedNs::start("diag.4_helper_not_loaded");
        for r in analysis.refs() {
            let RefKind::MethodCall { invocant, .. } = &r.kind else { continue };
            // Plugin-bridged tokens are resolved by their owning plugin,
            // not a missing-plugin hint candidate.
            let Some(invocant) = invocant.as_name() else { continue };
            let method_name = &r.target_name;
            if !matches!(MethodToken::parse(method_name), MethodToken::Bare(_)) {
                continue;
            }
            let class_name = match invocant.classify() {
                InvocantText::Bareword(b) => Some(b.to_string()),
                InvocantText::Scalar(_) => analysis
                    .inferred_type_via_bag(invocant, r.span.start)
                    .and_then(|ty| ty.class_name().map(|s| s.to_string())),
                _ => None,
            };
            let Some(class_name) = class_name else { continue };
            if !seen.insert((class_name.clone(), method_name.clone())) {
                // one hint per (class, helper) per file — the fix is
                // one `plugin` line, not one per call site
                continue;
            }
            let Some(provider) =
                analysis.bridged_helper_provider(&class_name, method_name, Some(module_index))
            else {
                continue;
            };
            if !module_index.is_workspace_module(&provider) {
                continue;
            }
            if analysis.imports.iter().any(|i| i.module_name == provider)
                || module_index.is_module_loaded(&provider)
            {
                continue;
            }
            diagnostics.push(Diagnostic {
                range: span_to_range(r.span),
                severity: Some(DiagnosticSeverity::HINT),
                code: Some(NumberOrString::String(codes::HELPER_NOT_LOADED.into())),
                source: Some("perl-lsp".into()),
                message: format!(
                    "'{}' is provided by {}, which no workspace entrypoint loads",
                    method_name, provider,
                ),
                ..Default::default()
            });
        }
    }

    // Opt-in `unresolved-dispatch`: a known dispatch verb whose receiver
    // couldn't be typed, so we can't tell if the dispatch applies. Fires ONLY
    // on `ReceiverUntyped` (a real typing gap), never on `DoesNotApply` — the
    // 3-way `GateResult` keeps the two apart so the diagnostic can't spew on
    // every unrelated receiver. QA/plugin-author tool, hence default-off.
    if options.unresolved_dispatch {
        let _g_dispatch =
            crate::util::ghost_stats::ScopedNs::start("diag.5_unresolved_dispatch");
        for untyped in analysis.untyped_dispatches(Some(module_index)) {
            diagnostics.push(Diagnostic {
                range: span_to_range(untyped.call_span),
                severity: Some(DiagnosticSeverity::INFORMATION),
                code: Some(NumberOrString::String(codes::UNRESOLVED_DISPATCH.into())),
                source: Some("perl-lsp".into()),
                message: format!(
                    "dispatch verb '{}' fired on an untyped receiver; can't confirm it dispatches into {}",
                    untyped.dispatcher, untyped.gate,
                ),
                ..Default::default()
            });
        }
    }

    // Unknown-hash-key: reads of keys a CLOSED structural shape doesn't
    // define, in both spellings — variable base (`$config->{typo}`) and
    // expression base (`cfg()->{typo}`). Detection and the trust gates live
    // on the seams (`closed_shape_key_typos` / `projected_key_typos`);
    // here the site renders. HINT severity, per the quiet-by-design
    // diagnostics convention; long key lists elide past five.
    let _g_hashkey = crate::util::ghost_stats::ScopedNs::start("diag.6_unknown_hash_key");
    for site in analysis
        .closed_shape_key_typos(Some(module_index))
        .into_iter()
        .chain(analysis.projected_key_typos(Some(module_index)))
    {
        let mut known: Vec<&str> =
            site.known_keys.iter().map(String::as_str).take(5).collect();
        if site.known_keys.len() > 5 {
            known.push("...");
        }
        let message = match &site.spelling {
            Some(base) => format!(
                "key '{}' is not in {}'s literal shape (keys: {})",
                site.key,
                base,
                known.join(", "),
            ),
            None => format!(
                "key '{}' is not in this expression's literal shape (keys: {})",
                site.key,
                known.join(", "),
            ),
        };
        diagnostics.push(Diagnostic {
            range: span_to_range(site.span),
            severity: Some(DiagnosticSeverity::HINT),
            code: Some(NumberOrString::String(codes::UNKNOWN_HASH_KEY.into())),
            message,
            ..Default::default()
        });
    }

    diagnostics
}

/// Render a D3/D4 verdict into its user-facing message. The phrasing lives
/// here in the adapter, not on the neutral `FileAnalysis` IR — a per-language
/// concern in the multi-language design (`language_driver.rs`).
fn render_guard_message(g: &crate::model::file_analysis::GuardRedundancy) -> String {
    use crate::model::file_analysis::GuardPredicate;
    let subject = &g.subject;
    match (&g.verdict, &g.predicate) {
        (GuardVerdict::AlwaysTrue, GuardPredicate::Defined) => {
            format!("'{subject}' is always defined here; this guard is redundant")
        }
        (GuardVerdict::AlwaysFalse, GuardPredicate::Defined) => {
            format!("'{subject}' is undef here; this guard can never pass")
        }
        (GuardVerdict::AlwaysTrue, GuardPredicate::IsType(t)) => {
            format!("'{subject}' is already {}; this guard is redundant", format_inferred_type(t))
        }
        (GuardVerdict::AlwaysFalse, GuardPredicate::IsType(t)) => {
            format!("'{subject}' is not {} here; this guard can never pass", format_inferred_type(t))
        }
    }
}
