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
    // Rule-#10 debt: the framework entries below (DBIC/Moose) belong to the
    // frameworks, not core diagnostics — they move out when plugins can
    // register meta-methods (docs/prompt-dbic-as-plugin.md) or the Openness
    // rule lands (docs/prompt-graph-walking.md, Openness).
    let universal_methods = [
        "new", "AUTOLOAD", "DESTROY", "can", "isa", "DOES",
        // Moose adds lowercase `does` alongside UNIVERSAL's uppercase DOES.
        "does",
        "VERSION",
        // DBIC meta-methods (inherited from DBIx::Class::Core)
        "add_columns", "add_column", "set_primary_key", "table", "resultset_class",
        "has_many", "has_one", "belongs_to", "might_have", "many_to_many",
        "load_components", "load_own_components",
        // Moose/Moo meta-methods
        "meta",
    ];
    let _g_meth = crate::util::ghost_stats::ScopedNs::start("diag.3_unresolved_method_loop");
    for r in analysis.refs() {
        // A plugin-bridged token is plugin-resolved, not a receiver we can
        // flag as an unresolved method — skip it.
        if !matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.as_name().is_some()) {
            continue;
        }
        let method_name = &r.target_name;

        // Skip universal methods
        if universal_methods.contains(&method_name.as_str()) {
            continue;
        }

        // Skip SUPER::-qualified and other package-qualified method names.
        // `$self->SUPER::foo()` stores `target_name = "SUPER::foo"`; trying
        // to find a method literally named "SUPER::foo" in the MRO always
        // fails. Caller-side package dispatch (`Class::method`) is intentional
        // and not our job to validate here.
        use crate::model::conventions::MethodToken;
        if !matches!(MethodToken::parse(method_name), MethodToken::Bare(_)) {
            continue;
        }

        // Resolve invocant to class name. Diagnostics stays bag-only for
        // scalars — no enclosing-class fallback, which would manufacture
        // warnings on untyped invocants — and skips everything else.
        let class_name = receiver_class(analysis, r);
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


/// The class a method-call receiver names, by the evidence every verb
/// reads: the bag's type for a scalar, a pack-declared receiver name
/// (php's `$this`) for the enclosing class, the expression's own `Expr`
/// witnesses for anything else (`$this->mailer->m()`). Bag-derived or
/// language-ruled — never manufactured from an untyped scalar, which is
/// why Perl's `$self` (no declared receiver name) stays silent.
fn receiver_class(analysis: &FileAnalysis, r: &crate::model::file_analysis::Ref) -> Option<String> {
    use crate::model::conventions::InvocantText;
    let RefKind::MethodCall { invocant, .. } = &r.kind else {
        return None;
    };
    let invocant = invocant.as_name()?;
    match invocant.classify() {
        InvocantText::Bareword(b) => Some(b.to_string()),
        InvocantText::Scalar(_) => analysis
            .inferred_type_via_bag(invocant, r.span.start)
            .and_then(|ty| ty.class_name().map(|s| s.to_string())),
        _ => None,
    }
}

/// The pack-language symbol lanes — the facts only packs mint (`arg_count`
/// on calls, `ParamArity` on callables, the use-map's namespace pins,
/// `Ref::binding` on every variable read) turned into the diagnostics an
/// editor expects of a typed language. Precision first: every lane has a
/// silence rule for the case it cannot see, named at the rule.
pub fn pack_symbol_diagnostics(
    analysis: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
    index_settled: bool,
) -> Vec<Diagnostic> {
    use crate::model::file_analysis::{HandlerOwner, MemberShape, MethodResolution, ScopeKind, SymbolDetail};
    let mut out = Vec::new();
    let pack = &analysis.pack;
    // Every per-class fact is derived ONCE per class, never per ref: a
    // 10k-line class file has thousands of member calls on a handful of
    // classes, and a symbol scan per call is quadratic.
    let local_classes: std::collections::HashSet<&str> = analysis
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, FaSymKind::Class | FaSymKind::Package))
        .map(|s| s.name.as_str())
        .collect();
    let local_class = |class: &str| local_classes.contains(class);
    // A class answering any member name (php `__call`/`__get`) is silent
    // for every undefined-member lane — Perl's AUTOLOAD rule.
    let catch_all = |class: &str| {
        pack.catch_all_methods.iter().any(|m| {
            analysis.resolve_member_in_ancestors(class, m, MemberShape::Callable, idx).is_some()
        })
    };
    // Callables reading their arguments dynamically (php `func_get_args`)
    // and scopes materializing variables dynamically (`extract`): the
    // call sites, collected once, matched by containment.
    let dynamic_arg_calls = dynamic_call_spans(analysis, &["func_get_args", "func_num_args", "func_get_arg"]);
    let dynamic_var_calls =
        dynamic_call_spans(analysis, &["extract", "get_defined_vars", "eval", "parse_str", "compact"]);
    // closures, for the rebound-`$this` silence
    let closures: Vec<Span> = analysis
        .symbols()
        .iter()
        .filter(|s| s.attributes.iter().any(|a| a == "anonymous"))
        .map(|s| s.span)
        .collect();
    // php declares a property by writing it: (class, member) of every
    // member write in the file
    let written: std::collections::HashSet<(String, String)> = analysis
        .refs()
        .iter()
        .filter(|w| {
            matches!(w.kind, RefKind::MethodCall { .. })
                && matches!(w.access, crate::model::file_analysis::AccessKind::Write)
        })
        .filter_map(|w| {
            analysis
                .method_call_invocant_class(w, idx)
                .map(|c| (c, w.unqualified_target_name().to_string()))
        })
        .collect();
    // The class's DEFINING analysis plus the facts every lane asks of it,
    // memoized per class. `None` = a class the lanes stay silent on.
    struct OwnerFacts {
        owner: Option<std::sync::Arc<FileAnalysis>>,
        is_interface: bool,
        is_enum: bool,
        /// A trait's `$this` is whatever class composes it: every member
        /// it does not declare may live there.
        is_trait: bool,
        dynamic_arg_calls: Vec<Span>,
    }
    // keyed by (leaf, the namespace the CALL means): a `parent::` call's
    // parent is the parent-namespace row of the class it is written in — an
    // aliased parent carrying the child's own leaf is not the child
    let mut owner_memo: HashMap<(String, Option<String>), Option<OwnerFacts>> = HashMap::new();
    // An ancestor we cannot see — at ANY depth — may declare the member:
    // silent.
    let ancestry_complete = |class: &str| ancestry_visible(analysis, idx, class);
    let push = |out: &mut Vec<Diagnostic>, span: Span, sev: DiagnosticSeverity, code: &str, msg: String| {
        out.push(Diagnostic {
            range: span_to_range(span),
            severity: Some(sev),
            code: Some(NumberOrString::String(code.to_string())),
            source: Some("perl-lsp".to_string()),
            message: msg,
            ..Default::default()
        });
    };

    // ---- undefined member / arity, per member call ----
    // Template method: `$this->step()` in a base whose SUBCLASS declares
    // `step` dispatches on the runtime class, which is that subclass. One
    // graph walk per (class, member, shape).
    let mut below_memo: HashMap<(String, String, bool), bool> = HashMap::new();
    for r in analysis.refs() {
        let RefKind::MethodCall { shape, invocant, named_by_string, .. } = &r.kind else { continue };
        let name = r.unqualified_target_name();
        if name.is_empty() || name.starts_with('$') {
            continue; // `$obj->$dyn()` — dynamic member name
        }
        if !pack.class_literal_member.is_empty() && name == pack.class_literal_member {
            continue; // `Foo::class` is the class-name literal
        }
        // The dispatch projection every verb reads (`$this` is a typed
        // receiver here — the extractor witnesses it at the class body).
        let Some(class) = analysis.method_call_invocant_class(r, idx) else { continue };
        // What namespace the CALL means by the leaf: a `parent::` call's
        // parent is the parent-namespace row of the class it is written in
        // (an aliased parent carrying the child's own leaf is not the
        // child); any other call, the file's pin or its own namespace.
        let super_call = matches!(
            crate::model::conventions::MethodToken::parse(&r.target_name),
            crate::model::conventions::MethodToken::Super(_)
        );
        let want_ns = if super_call {
            analysis
                .scope_at(r.span.start)
                .and_then(|sc| analysis.enclosing_class_for_scope(sc))
                .and_then(|c| {
                    analysis
                        .pack
                        .parent_namespaces
                        .iter()
                        .find(|(child, parent, _)| *child == c && *parent == class)
                        .map(|(_, _, ns)| ns.clone())
                })
                .or_else(|| analysis.leaf_namespace(&class))
                .or_else(|| analysis.use_map_pins().own_namespace.clone())
        } else {
            analysis.leaf_namespace(&class).or_else(|| analysis.use_map_pins().own_namespace.clone())
        };
        let facts = owner_memo.entry((class.clone(), want_ns.clone())).or_insert_with(|| {
            // The class's DEFINING analysis: this file, or — once the
            // workspace index is settled — the candidate its namespace
            // names. An unsettled index would flag every cross-file member.
            let is_local = local_class(&class)
                && (want_ns.is_none() || analysis.declared_type_namespace(&class) == want_ns);
            let owner_arc: Option<std::sync::Arc<FileAnalysis>> = if is_local {
                None
            } else if index_settled {
                let i = idx?;
                Some(i.visible_def_candidates(&class).into_iter().find_map(|c| {
                    let a = i.symbols_present(&c);
                    let declared = a.declared_type_namespace(&class);
                    (declared.is_some() && (want_ns.is_none() || declared == want_ns)).then_some(a)
                })?)
            } else {
                return None;
            };
            let owner: &FileAnalysis = owner_arc.as_deref().unwrap_or(analysis);
            let owner_has_members = owner.symbols().iter().any(|s| {
                matches!(s.kind, FaSymKind::Sub | FaSymKind::Method | FaSymKind::Field)
                    && s.package.as_deref() == Some(class.as_str())
            });
            let owner_catch_all = pack.catch_all_methods.iter().any(|m| {
                owner.resolve_member_in_ancestors(&class, m, MemberShape::Callable, idx).is_some()
            });
            if !owner_has_members || owner_catch_all || !ancestry_visible(owner, idx, &class) {
                return None;
            }
            let class_attr = |attr: &str| {
                owner.symbols().iter().any(|s| {
                    matches!(s.kind, FaSymKind::Class) && s.name == class && s.attributes.iter().any(|a| a == attr)
                })
            };
            Some(OwnerFacts {
                // A receiver typed as an INTERFACE names any implementation.
                // `instanceof` narrowing retypes a VARIABLE receiver, but a
                // member subject (`$this->x instanceof T`), a method guard
                // (`->isT()`) or `is_a()` leave the interface type standing
                // — so the interface stays silent on undefined members
                // (resolved ones still check arity).
                is_interface: class_attr("interface"),
                is_enum: class_attr("enum"),
                is_trait: class_attr("trait"),
                dynamic_arg_calls: if owner_arc.is_some() {
                    dynamic_call_spans(owner, &["func_get_args", "func_num_args", "func_get_arg"])
                } else {
                    dynamic_arg_calls.clone()
                },
                owner: owner_arc,
            })
        });
        let Some(facts) = facts.as_ref() else { continue };
        let owner: &FileAnalysis = facts.owner.as_deref().unwrap_or(analysis);
        let want = match shape {
            MemberShape::Value => MemberShape::Value,
            _ => MemberShape::Callable,
        };
        // an enum's language-given members
        if facts.is_enum && pack.enum_members.iter().any(|m| m == name) {
            continue;
        }
        match owner.resolve_member_in_ancestors(&class, name, want, idx) {
            None if facts.is_interface || facts.is_trait => {}
            // a class with no declared constructor has the default one
            None if pack.constructor_names.iter().any(|c| c == name) => {}
            None if *named_by_string => {
                // `[$obj, 'name']` is data until dispatch proves it a
                // callable: a claim only when it resolves
            }
            None => {
                // php declares a property by writing it: a write of this
                // member on the same class anywhere in the file is its
                // declaration
                if matches!(want, MemberShape::Value) && written.contains(&(class.clone(), name.to_string())) {
                    continue;
                }
                // a read inside an existence probe (`isset($x->p)`) IS the
                // question of whether the member exists
                if matches!(want, MemberShape::Value)
                    && analysis.pack.probe_regions.iter().any(|p| p.contains(&r.span))
                {
                    continue;
                }
                // the receiver is the pack's own (`$this`): the runtime class
                // may be any descendant, and one of them declares the member
                let own_receiver = pack.receiver_names.iter().any(|n| n == invocant.text());
                if own_receiver {
                    let declared_below = *below_memo
                        .entry((class.clone(), name.to_string(), matches!(want, MemberShape::Value)))
                        .or_insert_with(|| {
                            owner
                                .dispatch_participants(&class, idx)
                                .iter()
                                .filter(|p| **p != class)
                                .any(|p| owner.resolve_member_in_ancestors(p, name, want, idx).is_some())
                        });
                    if declared_below {
                        continue;
                    }
                }
                // a same-named member of the OTHER shape is a different
                // finding (a method read as a property) — still undefined
                let (code, what) = match want {
                    MemberShape::Value => ("undefined-property", "property"),
                    _ => ("unresolved-method", "method"),
                };
                push(&mut out, r.span, DiagnosticSeverity::ERROR, code, format!("Undefined {what} '{name}'."));
            }
            Some(MethodResolution::Local { sym_id, .. }) => {
                let sym = owner.symbol(sym_id);
                if let Some(text) = deprecation_of(sym) {
                    out.push(deprecated_diag(r.span, name, &text));
                }
                // non-public member reached from outside its class — unless
                // from inside a closure, whose `$this` may be rebound to the
                // owner (`Closure::bind`, `->call($obj)`: the private-access
                // idiom tests live on)
                let in_closure = analysis
                    .scope_chain(r.scope)
                    .into_iter()
                    .any(|sc| closures.iter().any(|c| span_within(analysis.scope(sc).span, *c)));
                // a property READ that resolved only to a same-named METHOD
                // (or the reverse) is not an access violation
                let shape_agrees = match want {
                    MemberShape::Value => matches!(sym.kind, FaSymKind::Field | FaSymKind::Variable),
                    _ => matches!(sym.kind, FaSymKind::Method | FaSymKind::Sub),
                };
                if shape_agrees && !in_closure && sym.attributes.iter().any(|a| a == "non_public") {
                    let from = analysis.enclosing_class_for_scope(r.scope);
                    let owner = sym.package.clone().unwrap_or_default();
                    if from.as_deref() != Some(owner.as_str())
                        && !from.as_deref().is_some_and(|f| analysis.class_isa(f, &owner, idx))
                    {
                        push(&mut out, r.span, DiagnosticSeverity::ERROR, "non-public-access",
                            format!("Cannot access non-public member '{name}' of {owner} from {} scope.", from.as_deref().unwrap_or("global")));
                    }
                }
                // arity: the written argument count against the declared list
                if let (Some(n), Some(a)) = (r.arg_count, sym.arity) {
                    if !callee_takes_any(&facts.dynamic_arg_calls, sym) {
                        if n < a.required {
                            push(&mut out, r.span, DiagnosticSeverity::ERROR, "arity-mismatch",
                                format!("Not enough arguments. Expected {}. Found {n}.", a.required));
                        } else if !a.variadic && n > a.total {
                            push(&mut out, r.span, DiagnosticSeverity::WARNING, "arity-mismatch",
                                format!("Too many arguments. Expected {}. Found {n}.", a.total));
                        }
                    }
                }
            }
            Some(MethodResolution::CrossFile { .. }) => {}
        }
    }

    // ---- deprecated functions and classes, local or cross-file ----
    for r in analysis.refs() {
        let (leaf, want_class) = match r.kind {
            RefKind::FunctionCall => (r.unqualified_target_name(), false),
            RefKind::PackageRef => (r.unqualified_target_name(), true),
            _ => continue,
        };
        if leaf.is_empty() || analysis.pack.import_row_covering(&r.span).is_some() {
            continue;
        }
        let is_kind = |s: &crate::model::file_analysis::Symbol| {
            if want_class { matches!(s.kind, FaSymKind::Class) } else { matches!(s.kind, FaSymKind::Sub) }
        };
        let local = analysis.symbols_named(leaf).iter().map(|&sid| analysis.symbol(sid)).find(|s| is_kind(s)).and_then(deprecation_of);
        // Cross-file: only a declaration in the namespace THIS file means by
        // the leaf (its pin, else its own namespace) — a same-leaf stranger
        // elsewhere in the workspace is a different declaration.
        let found = local.or_else(|| {
            let i = idx?;
            let want_ns = analysis.leaf_namespace(leaf).or_else(|| analysis.use_map_pins().own_namespace.clone());
            i.visible_def_candidates(leaf).into_iter().find_map(|c| {
                let a = i.symbols_present(&c);
                a.symbols_named(leaf)
                    .iter()
                    .map(|&sid| a.symbol(sid))
                    .find(|s| is_kind(s) && (want_ns.is_none() || s.package == want_ns))
                    .and_then(deprecation_of)
            })
        });
        if let Some(text) = found {
            out.push(deprecated_diag(r.span, leaf, &text));
        }
    }

    // ---- arity on plain calls and constructors (local callees only) ----
    for r in analysis.refs() {
        if !matches!(r.kind, RefKind::FunctionCall) {
            continue;
        }
        let Some(n) = r.arg_count else { continue };
        let name = r.unqualified_target_name();
        let callee = analysis
            .symbols_named(name)
            .iter()
            .map(|&sid| analysis.symbol(sid))
            .find(|s| matches!(s.kind, FaSymKind::Sub))
            .or_else(|| {
                // `new Foo(...)`: the class's own constructor; a class with
                // none accepts any argument list
                let ctor = pack.constructor_names.first()?;
                if !local_class(name) || catch_all(name) || !ancestry_complete(name) {
                    return None;
                }
                match analysis.resolve_member_in_ancestors(name, ctor, MemberShape::Callable, idx)? {
                    MethodResolution::Local { sym_id, .. } => Some(analysis.symbol(sym_id)),
                    _ => None,
                }
            });
        let Some(sym) = callee else { continue };
        let Some(a) = sym.arity else { continue };
        if callee_takes_any(&dynamic_arg_calls, sym) {
            continue;
        }
        if n < a.required {
            push(&mut out, r.span, DiagnosticSeverity::ERROR, "arity-mismatch",
                format!("Not enough arguments. Expected {}. Found {n}.", a.required));
        } else if !a.variadic && n > a.total {
            push(&mut out, r.span, DiagnosticSeverity::WARNING, "arity-mismatch",
                format!("Too many arguments. Expected {}. Found {n}.", a.total));
        }
    }

    // ---- undefined variable: an unbound read inside a callable ----
    if !pack.implicit_variables.is_empty() {
        // occurrences per (callable scope, name) — a name read MORE than
        // once is presumed bound by a call the callee lane below cannot
        // resolve; the single stray read is the typo this lane names.
        let callable_of = |scope: crate::model::file_analysis::ScopeId| {
            analysis.scope_chain(scope).into_iter().find(|&sc| {
                matches!(analysis.scope(sc).kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. })
            })
        };
        let mut seen: HashMap<(u32, String), usize> = HashMap::new();
        for r in analysis.refs() {
            if matches!(r.kind, RefKind::Variable) && r.target_name.starts_with('$') {
                if let Some(sc) = callable_of(r.scope) {
                    *seen.entry((sc.0, r.target_name.clone())).or_default() += 1;
                }
            }
        }
        // A bare variable written as a call argument is bound by the call
        // when the callee declares that position by reference (`&$out`).
        // The callee resolves as signature help resolves it: the receiver
        // through the dispatch projection, a plain call by name, locally
        // then across files. An UNRESOLVABLE callee is silence, not a
        // guess: php's own functions carry no declaration here
        // (`preg_match($re, $s, $m)` binds `$m`), and a `__call` class
        // answers every name.
        let calls_by_args_start: HashMap<(usize, usize), &crate::model::file_analysis::Ref> = analysis
            .refs()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall))
            .map(|r| ((r.span.end.row, r.span.end.column), r))
            .collect();
        let callee_arity = |call: &crate::model::file_analysis::Ref| -> Option<crate::model::file_analysis::ParamArity> {
            let name = call.unqualified_target_name();
            let callable = |s: &crate::model::file_analysis::Symbol| matches!(s.kind, FaSymKind::Sub | FaSymKind::Method);
            match &call.kind {
                RefKind::MethodCall { shape, .. } => {
                    let class = analysis.method_call_invocant_class(call, idx)?;
                    match analysis.resolve_member_in_ancestors(&class, name, *shape, idx)? {
                        MethodResolution::Local { sym_id, .. } => analysis.symbol(sym_id).param_arity(),
                        MethodResolution::CrossFile { class, def_module } => {
                            let ix = idx?;
                            let module = def_module.as_deref().unwrap_or(class.as_str());
                            let cached = ix.candidate_defining_sub_in_package(module, &class, name)?;
                            let view = ix.symbols_present(&cached);
                            let syms = view.symbols();
                            syms.iter()
                                .find(|s| callable(s) && s.name == name && s.package.as_deref() == Some(class.as_str()))
                                .or_else(|| syms.iter().find(|s| callable(s) && s.name == name))
                                .and_then(|s| s.param_arity())
                        }
                    }
                }
                RefKind::FunctionCall => {
                    if let Some(sym) = analysis
                        .symbols_named(name)
                        .iter()
                        .map(|&sid| analysis.symbol(sid))
                        .find(|s| matches!(s.kind, FaSymKind::Sub))
                    {
                        return sym.param_arity();
                    }
                    let ix = idx?;
                    let cached = ix.visible_def_candidates(name).into_iter().next()?;
                    let view = ix.symbols_present(&cached);
                    view.symbols()
                        .iter()
                        .find(|s| matches!(s.kind, FaSymKind::Sub) && s.name == name)
                        .and_then(|s| s.param_arity())
                }
                _ => None,
            }
        };
        // `Some(true)` bound by the call, `Some(false)` a plain read,
        // `None` an argument of a callee this lane cannot resolve
        let argument_binding = |var: Span| -> Option<bool> {
            let Some(site) = analysis.pack.variable_arg_sites.iter().find(|s| s.var == var) else {
                return Some(false);
            };
            let call = calls_by_args_start.get(&(site.args.start.row, site.args.start.column))?;
            let arity = callee_arity(call)?;
            Some(arity.binds_arg(site.position as usize))
        };
        for r in analysis.refs() {
            // a WRITE binds (php declares a variable by assigning it)
            if !matches!(r.kind, RefKind::Variable)
                || r.binding.is_some()
                || matches!(r.access, crate::model::file_analysis::AccessKind::Write)
                || !r.target_name.starts_with('$')
            {
                continue;
            }
            if pack.implicit_variables.contains(&r.target_name) {
                continue;
            }
            let Some(sc) = callable_of(r.scope) else { continue };
            if seen.get(&(sc.0, r.target_name.clone())).copied().unwrap_or(0) != 1 {
                continue;
            }
            if argument_binding(r.span) != Some(false) {
                continue;
            }
            // `isset($x)` / `empty($x)` / `unset($x)`: the read IS the
            // existence question, the member lanes' probe silence
            if analysis.pack.probe_regions.iter().any(|p| p.contains(&r.span)) {
                continue;
            }
            // a callable that materializes variables dynamically is silent
            let body = analysis.scope(sc).span;
            if dynamic_var_calls.iter().any(|c| span_within(*c, body)) {
                continue;
            }
            push(&mut out, r.span, DiagnosticSeverity::ERROR, "undefined-variable",
                format!("Undefined variable '{}'.", r.target_name));
        }

        // ---- unused variable: a local written and never read ----
        // A read counts for the callable it sits in, every enclosing callable
        // (a closure's `use ($x)` reads the outer `$x` through its own copy)
        // and every callable nested inside it (a by-reference capture is
        // written inside the closure and read by the scope around it); a
        // same-named declaration in a nested callable is the capture itself.
        let callables_up = |scope: crate::model::file_analysis::ScopeId| -> Vec<u32> {
            analysis
                .scope_chain(scope)
                .into_iter()
                .filter(|&sc| matches!(analysis.scope(sc).kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. }))
                .map(|sc| sc.0)
                .collect()
        };
        let mut read_chains: HashMap<String, Vec<Vec<u32>>> = HashMap::new();
        for r in analysis.refs() {
            if !matches!(r.kind, RefKind::Variable)
                || matches!(r.access, crate::model::file_analysis::AccessKind::Write)
                || !r.target_name.starts_with('$')
            {
                continue;
            }
            read_chains.entry(r.target_name.clone()).or_default().push(callables_up(r.scope));
        }
        let mut decl_chains: HashMap<String, Vec<(u32, Vec<u32>)>> = HashMap::new();
        for sym in analysis.symbols() {
            if matches!(sym.kind, FaSymKind::Variable) && sym.name.starts_with('$') {
                if let Some(sc) = callable_of(sym.scope) {
                    decl_chains.entry(sym.name.clone()).or_default().push((sc.0, callables_up(sym.scope)));
                }
            }
        }
        for sym in analysis.symbols() {
            if !matches!(sym.kind, FaSymKind::Variable) || !sym.name.starts_with('$') {
                continue;
            }
            let Some(sc) = callable_of(sym.scope) else { continue };
            // an alias (`$h = &$opts['h']`) is written to reach its storage
            if pack.implicit_variables.contains(&sym.name)
                || pack.throwaway_names.contains(&sym.name)
                || pack.param_regions.iter().any(|p| p.contains(&sym.span))
                || sym.attributes.iter().any(|a| a == "alias")
            {
                continue;
            }
            let body = analysis.scope(sc).span;
            if dynamic_var_calls.iter().any(|c| span_within(*c, body)) {
                continue;
            }
            let related = |chain: &Vec<u32>| chain.contains(&sc.0) || callables_up(sym.scope).iter().any(|c| chain.first() == Some(c));
            let read = read_chains.get(&sym.name).is_some_and(|chains| chains.iter().any(related));
            let captured = decl_chains
                .get(&sym.name)
                .is_some_and(|ds| ds.iter().any(|(owner, chain)| *owner != sc.0 && chain.contains(&sc.0)));
            if read || captured {
                continue;
            }
            out.push(Diagnostic {
                range: span_to_range(sym.selection_span),
                severity: Some(DiagnosticSeverity::HINT),
                code: Some(NumberOrString::String("unused-variable".to_string())),
                source: Some("perl-lsp".to_string()),
                message: format!("'{}' is assigned but never used.", sym.name),
                tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                ..Default::default()
            });
        }
    }

    // ---- unused import: a bound name the file never spells ----
    if pack.imports_bind_names {
        let pins = analysis.use_map_pins();
        let ns_heads: std::collections::HashSet<&str> = pack
            .qualified_spellings
            .iter()
            .filter_map(|(_, prefix)| prefix.trim_start_matches('\\').split('\\').next())
            .filter(|h| !h.is_empty())
            .collect();
        for (span, raw) in &pack.include_directives {
            let leaf = raw.rsplit('\\').next().unwrap_or(raw);
            // the alias token has a row of its own; the import's row reports
            if !raw.contains('\\') && pack.use_aliases.iter().any(|(alias, _, _)| alias == leaf) {
                continue;
            }
            // the name the row binds: its alias when it has one
            let bound = pack
                .use_aliases
                .iter()
                .find(|(_, ns, real)| {
                    real == leaf && (format!("{ns}\\{real}") == *raw || (ns.is_empty() && real == raw))
                })
                .map(|(alias, _, _)| alias.as_str())
                .unwrap_or(leaf);
            // a constant import (`use const FOO`) has no spelling the
            // walker records — silent
            if bound.is_empty() || !bound.chars().any(|c| c.is_lowercase()) {
                continue;
            }
            let used = pins.spelled.contains(bound)
                || ns_heads.contains(bound)
                || pack.doc_mentions.iter().any(|m| m == bound);
            if used {
                continue;
            }
            out.push(Diagnostic {
                range: span_to_range(*span),
                severity: Some(DiagnosticSeverity::HINT),
                code: Some(NumberOrString::String("unused-import".to_string())),
                source: Some("perl-lsp".to_string()),
                message: format!("'{bound}' is imported but never used."),
                tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                data: pack
                    .import_rows
                    .iter()
                    .find(|r| span_within(*span, **r))
                    .filter(|r| pack.include_directives.iter().filter(|(s, _)| span_within(*s, **r)).count() == 1)
                    .map(|r| serde_json::json!({ "row": [r.start.row, r.end.row] })),
                ..Default::default()
            });
        }
    }

    // ---- undefined rail name: a use on a named rail (`route('home')`)
    // that no definition on that rail answers, here or in the settled
    // index. Names a framework synthesizes (`Route::resource`) have no
    // definition token, so the lane warns rather than errors.
    if index_settled {
        if let Some(idx) = idx {
            let defines = |syms: &[crate::model::file_analysis::Symbol], owner: &HandlerOwner, name: &str| {
                syms.iter().any(|s| {
                    s.name == name && matches!(&s.detail, SymbolDetail::Handler { owner: o, .. } if o == owner)
                })
            };
            for r in analysis.refs() {
                if !matches!(r.kind, RefKind::DispatchCall { .. }) {
                    continue;
                }
                let Some(owner @ HandlerOwner::Rail(rail)) = r.handler_owner() else { continue };
                let name = r.target_name.as_str();
                if defines(analysis.symbols(), owner, name) {
                    continue;
                }
                if !crate::index::resolve::handler_definitions(owner, name, idx).is_empty() {
                    continue;
                }
                push(&mut out, r.span, DiagnosticSeverity::WARNING, &format!("undefined-{rail}"),
                    format!("Undefined {rail} '{name}'."));
            }
        }
    }

    // ---- undefined type: a class name the namespace cannot supply ----
    if index_settled {
        if let Some(idx) = idx {
            let pins = analysis.use_map_pins();
            // a namespace-less file lives in the global namespace
            let own_ns = pins.own_namespace.clone().unwrap_or_default();
            {
                let own = own_ns.as_str();
                let ns_heads: std::collections::HashSet<&str> = pack
                    .qualified_spellings
                    .iter()
                    .filter_map(|(_, prefix)| prefix.trim_start_matches('\\').split('\\').next())
                    .filter(|h| !h.is_empty())
                    .collect();
                let mut reported: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
                for r in analysis.refs() {
                    // a class reference, or a constructor call (`new Foo()`
                    // carries the class name) that no function answers
                    let is_type_ref = match r.kind {
                        RefKind::PackageRef => true,
                        RefKind::FunctionCall => {
                            pack.types_are_capitalized
                                && r.target_name.rsplit('\\').next().unwrap_or("").chars().next().is_some_and(|c| c.is_uppercase())
                                && !analysis.symbols_named(r.unqualified_target_name()).iter().any(|&sid| matches!(analysis.symbol(sid).kind, FaSymKind::Sub))
                        }
                        _ => false,
                    };
                    if !is_type_ref {
                        continue;
                    }
                    let written = r.target_name.as_str();
                    // absolute names reach the global namespace (builtins we
                    // carry no stubs for) — silent
                    if written.starts_with('\\') || written.contains("::") {
                        continue;
                    }
                    let leaf = written.rsplit('\\').next().unwrap_or(written);
                    if leaf.is_empty() || matches!(leaf, "self" | "static" | "parent") {
                        continue;
                    }
                    // a segment used as a NAMESPACE prefix in this file
                    // (`Psr7\Utils`) names a namespace, not a type
                    if ns_heads.contains(leaf) {
                        continue;
                    }
                    if analysis.pack.import_row_covering(&r.span).is_some() {
                        // an import row naming a function/constant, not a type
                        if pack.types_are_capitalized && leaf.chars().next().is_some_and(|c| c.is_lowercase()) {
                            continue;
                        }
                        // a row whose leaf the file never spells bare imports
                        // a NAMESPACE (`use GuzzleHttp\Psr7;` then
                        // `Psr7\Utils`) or nothing — no type to assert
                        if !pins.spelled.contains(leaf) {
                            continue;
                        }
                    }
                    let ns = match pins.pins.get(leaf) {
                        Some(Some(ns)) => ns.clone(),
                        Some(None) => continue, // conflicting evidence
                        None => match written.rsplit_once('\\') {
                            Some((prefix, _)) => format!("{own}\\{prefix}"),
                            None => own.to_string(),
                        },
                    };
                    // the global namespace is the builtins we carry no stubs
                    // for — silent, unless the workspace declares the leaf
                    // under a namespace and nowhere global: then the type is
                    // real and missing its import
                    let declared = type_namespaces(analysis, idx, leaf);
                    if ns.is_empty() {
                        if declared.is_empty()
                            || declared.iter().any(|d| d.is_empty())
                            || crate::build::language_driver::LanguageRegistry::builtin_types(&analysis.language)
                                .contains(&leaf)
                        {
                            continue;
                        }
                    } else if declared.contains(&ns) {
                        continue;
                    }
                    if !reported.insert((r.span.start.row, r.span.start.column)) {
                        continue;
                    }
                    // every namespace that DOES declare the leaf is an import
                    // the quick-fix can offer
                    let candidates: Vec<String> =
                        declared.iter().filter(|d| !d.is_empty()).map(|d| format!("{d}\\{leaf}")).collect();
                    out.push(Diagnostic {
                        range: span_to_range(r.span),
                        severity: Some(DiagnosticSeverity::ERROR),
                        code: Some(NumberOrString::String("undefined-type".to_string())),
                        source: Some("perl-lsp".to_string()),
                        message: if ns.is_empty() {
                            format!("Undefined type '{leaf}'.")
                        } else {
                            format!("Undefined type '{ns}\\{leaf}'.")
                        },
                        data: (!candidates.is_empty()).then(|| serde_json::json!({ "candidates": candidates })),
                        ..Default::default()
                    });
                }
            }
        }
    }
    // ---- unimplemented contracts: the role-requires lane in the pack's
    // vocabulary. An interface / trait / abstract class is a role whose
    // contract callables a concrete composer must declare or inherit from
    // a concrete ancestor. Silent for an ancestor we cannot see and for a
    // composer that defers (abstract). A catch-all method (`__call`) does
    // NOT silence it: the contract is checked when the class is declared,
    // before any call could be caught.
    if let Some(idx) = idx {
        let mut by_class: std::collections::BTreeMap<
            String,
            Vec<crate::model::file_analysis::UnfulfilledRequire>,
        > = Default::default();
        for u in analysis.unfulfilled_role_requires(Some(idx)) {
            by_class.entry(u.package.clone()).or_default().push(u);
        }
        for (class, missing) in by_class {
            let Some(sym) = analysis
                .symbols()
                .iter()
                .find(|s| s.kind == FaSymKind::Class && s.name == class)
            else {
                continue;
            };
            // The contract's own declarator rides the diagnostic so the
            // quick-fix needs no resolution: a closed declaring file reads
            // from disk here; the open document's is rendered from its
            // buffer by the action (`sig` = null).
            let contracts: Vec<serde_json::Value> = missing
                .iter()
                .map(|u| {
                    let sig = contract_declarator(analysis, idx, &u.role, &u.name);
                    serde_json::json!({"role": u.role, "name": u.name, "sig": sig})
                })
                .collect();
            let list = missing
                .iter()
                .map(|u| format!("`{}::{}()`", u.role, u.name))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Diagnostic {
                range: span_to_range(sym.selection_span),
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String("unimplemented-method".to_string())),
                source: Some("perl-lsp".to_string()),
                message: format!(
                    "'{class}' does not implement {list}; declare {} or make the class abstract.",
                    if missing.len() == 1 { "it" } else { "them" }
                ),
                data: Some(serde_json::json!({"class": class, "contracts": contracts})),
                ..Default::default()
            });
        }
    }

    // ---- missing return type: a callable with a body, no native return
    // annotation, and an inferred return the pack can spell natively — in a
    // file that writes native return types already (its own convention;
    // a docblock-typed codebase is not asked to change style). Skipped:
    // constructors, contracts, and any return the spelling cannot name
    // (ambiguous numerics, unions, a leaf that means another class here).
    if !pack.return_annotation_template.is_empty() {
        // `declared_return` is the structural fact (the declaration writes
        // an annotation) — a type witness cannot carry it: `: void` names
        // no type.
        let declared = |s: &crate::model::file_analysis::Symbol| s.attributes.iter().any(|a| a == "declared_return");
        let callables: Vec<&crate::model::file_analysis::Symbol> = analysis
            .symbols()
            .iter()
            .filter(|s| matches!(s.kind, FaSymKind::Sub | FaSymKind::Method))
            .collect();
        if callables.iter().any(|s| declared(s)) {
            for s in callables {
                // no annotation to add: a constructor, a contract, a docblock
                // `@method`, a closure; and none wanted for an already-declared one
                if pack.constructor_names.iter().any(|c| c == &s.name)
                    || s.attributes.iter().any(|a| a == "contract" || a == "documented" || a == "anonymous")
                    || declared(s)
                {
                    continue;
                }
                let Some(ty) = analysis.total_inferred_return(s.id) else { continue };
                // a fluent `return $this` wants `static`, `new self()` wants
                // `self` — the fold cannot tell them apart, so the enclosing
                // class is never spelled from inference
                if matches!(&ty, crate::model::file_analysis::InferredType::ClassName(n)
                    if s.package.as_deref().is_some_and(|p| p == n || p.rsplit(['\\', ':']).next() == Some(n.as_str())))
                {
                    continue;
                }
                let Some(spelling) = analysis.native_type_spelling(&ty) else { continue };
                out.push(Diagnostic {
                    range: span_to_range(s.selection_span),
                    severity: Some(DiagnosticSeverity::HINT),
                    code: Some(NumberOrString::String("missing-return-type".to_string())),
                    source: Some("perl-lsp".to_string()),
                    message: format!("'{}' has no declared return type; it returns `{spelling}`.", s.name),
                    data: Some(serde_json::json!({"spelling": spelling})),
                    ..Default::default()
                });
            }
        }
    }

    out
}


/// The declarator text of a contract callable declared by a CLOSED file —
/// `None` when the role is declared in this document (the quick-fix reads
/// the open buffer) or nothing on disk declares it.
fn contract_declarator(
    analysis: &FileAnalysis,
    idx: &dyn CrossFileLookup,
    role: &str,
    name: &str,
) -> Option<String> {
    if analysis.symbols().iter().any(|s| s.kind == FaSymKind::Class && s.name == role) {
        return None;
    }
    for cached in idx.visible_def_candidates(role) {
        let whole = idx.whole_present(&cached);
        let Some(sym) = whole.symbols().iter().find(|s| {
            matches!(s.kind, FaSymKind::Sub | FaSymKind::Method)
                && s.name == name
                && s.package.as_deref() == Some(role)
        }) else {
            continue;
        };
        let Ok(src) = std::fs::read_to_string(&cached.path) else { continue };
        if let Some(t) = declarator_text(&src, sym) {
            return Some(t);
        }
    }
    None
}

/// A deprecated declaration's notice: `Some(text)` when the `deprecated`
/// attribute is set (the text may be absent), `None` otherwise.
fn deprecation_of(sym: &crate::model::file_analysis::Symbol) -> Option<Option<String>> {
    sym.attributes
        .iter()
        .any(|a| a == "deprecated")
        .then(|| sym.presentation.deprecation.clone())
}

/// The deprecated-tagged hint at a use site.
fn deprecated_diag(span: Span, name: &str, text: &Option<String>) -> Diagnostic {
    Diagnostic {
        range: span_to_range(span),
        severity: Some(DiagnosticSeverity::HINT),
        code: Some(NumberOrString::String("deprecated".to_string())),
        source: Some("perl-lsp".to_string()),
        message: match text {
            Some(t) => format!("'{name}' is deprecated: {t}"),
            None => format!("'{name}' is deprecated."),
        },
        tags: Some(vec![DiagnosticTag::DEPRECATED]),
        ..Default::default()
    }
}

/// Every namespace declaring a type named `leaf`: this file's own
/// declaration plus every workspace/dependency candidate — the set the
/// undefined-type lane tests membership in and the import quick-fix lists.
fn type_namespaces(analysis: &FileAnalysis, idx: &dyn CrossFileLookup, leaf: &str) -> Vec<String> {
    let mut out: Vec<String> = analysis.declared_type_namespace(leaf).into_iter().collect();
    for c in idx.def_candidates(leaf) {
        if let Some(ns) = idx.symbols_present(&c).declared_type_namespace(leaf) {
            if !out.contains(&ns) {
                out.push(ns);
            }
        }
    }
    out
}

/// Call sites of the named functions — the dynamic-behaviour markers a
/// containing scope is silenced by.
fn dynamic_call_spans(analysis: &FileAnalysis, names: &[&str]) -> Vec<Span> {
    analysis
        .refs()
        .iter()
        .filter(|c| matches!(c.kind, RefKind::FunctionCall) && names.contains(&c.unqualified_target_name()))
        .map(|c| c.span)
        .collect()
}

/// A callable that reads its arguments dynamically (php `func_get_args`)
/// accepts any count.
fn callee_takes_any(dynamic_arg_calls: &[Span], sym: &crate::model::file_analysis::Symbol) -> bool {
    dynamic_arg_calls.iter().any(|c| span_within(*c, sym.span))
}

fn span_within(inner: Span, outer: Span) -> bool {
    outer.contains(&inner)
}


/// Every ancestor of `class`, transitively, is declared somewhere we can
/// read (this file, or a candidate the lookup reaches). One unreadable
/// parent anywhere in the chain means a member may live there.
fn ancestry_visible(analysis: &FileAnalysis, idx: Option<&dyn CrossFileLookup>, class: &str) -> bool {
    fn walk(
        a: &FileAnalysis,
        idx: Option<&dyn CrossFileLookup>,
        class: &str,
        seen: &mut std::collections::HashSet<(String, String)>,
        depth: usize,
    ) -> bool {
        if depth > 20 {
            return true;
        }
        for p in a.declared_parents(class) {
            let leaf = p.rsplit(['\\', ':']).next().unwrap_or(p);
            // The parent's namespace as THIS edge wrote it (`extends
            // \Exception` is the global one, whatever leaf the child
            // carries — `class Exception extends \Exception` must not find
            // itself), else as this file sees the leaf (its `use` rows, its
            // own namespace) — a same-leaf stranger elsewhere in the
            // workspace is not this parent.
            let want_ns = a
                .pack
                .parent_namespaces
                .iter()
                .find(|(c, pl, _)| c == class && pl == leaf)
                .map(|(_, _, ns)| ns.clone())
                .or_else(|| a.leaf_namespace(leaf))
                .or_else(|| a.use_map_pins().own_namespace.clone());
            let local = a
                .symbols()
                .iter()
                .any(|s| matches!(s.kind, FaSymKind::Class | FaSymKind::Package) && s.name == leaf)
                && a.declared_type_namespace(leaf) == want_ns;
            if local {
                if !seen.insert((want_ns.clone().unwrap_or_default(), leaf.to_string())) {
                    continue;
                }
                if !walk(a, idx, leaf, seen, depth + 1) {
                    return false;
                }
                continue;
            }
            let Some(i) = idx else { return false };
            let mut any = false;
            for c in &i.visible_def_candidates(leaf) {
                let whole = i.symbols_present(c);
                let declared = whole.declared_type_namespace(leaf);
                if declared.is_none() || (want_ns.is_some() && declared != want_ns) {
                    continue;
                }
                any = true;
                if !seen.insert((declared.unwrap_or_default(), leaf.to_string())) {
                    continue;
                }
                if !walk(&whole, idx, leaf, seen, depth + 1) {
                    return false;
                }
            }
            if !any {
                return false;
            }
        }
        true
    }
    walk(analysis, idx, class, &mut std::collections::HashSet::new(), 0)
}
