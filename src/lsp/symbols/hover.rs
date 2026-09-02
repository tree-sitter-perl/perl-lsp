//! Hover rendering for Perl and pack languages.

use super::*;

/// Hover for pack languages: a presentation of the CandidateSet's hover
/// projection (`docs/adr/resolution-candidate-set.md` — hover presents the
/// top-ranked candidate goto-def would jump to, so the two verbs answer one
/// resolution and can't disagree). Presentation stays here: the member
/// drill-downs (domain headline, storage leaf, template substitution) run
/// first over the same invocant resolution the set's member goto-def lane
/// uses; everything else renders the projection's candidate.
pub fn pack_hover_markdown(
    cs: &crate::index::resolve::CandidateSet,
    language: &str,
) -> Option<String> {
    let analysis = cs.origin_analysis();
    let source = cs.origin_source()?;
    let point = cs.cursor();
    let module_index = cs.scoped_index();
    // Member access (`obj->field` / `obj->method()`): resolve the EXACT member
    // via the invocant class + ancestor walk — the SAME resolution the set's
    // member goto-def lane uses — so a same-file field def (or a same-named
    // symbol on another class) can't hijack it with the wrong scope.
    // A data field shows `field: type` (member_hover, keyed on the field's own
    // scope); a method shows its signature.
    if let Some(r) = analysis.ref_at(point).filter(|r| matches!(r.kind, RefKind::MethodCall { .. })) {
        if let Some(midx) = module_index {
            if let Some(cn) = analysis.method_call_invocant_class(r, Some(midx)) {
                let field = r.unqualified_target_name();
                // The receiver's full VALUE (not just its dispatch class):
                // a template instance's args refine a param-shaped member
                // type (`T get()` on a `Box<int>` receiver → `int`) — shown
                // only when the substitution actually changed the answer,
                // so non-template hovers stay byte-identical.
                let recv_ty = match &r.kind {
                    RefKind::MethodCall { invocant_span: Some(sp), .. } => {
                        analysis.expr_type_at_span(*sp, Some(midx))
                    }
                    _ => None,
                };
                let substituted = |raw: Option<InferredType>| -> Option<InferredType> {
                    let sub = recv_ty
                        .as_ref()
                        .and_then(|t| analysis.member_value_type(t, field, Some(midx), None))?;
                    (raw.as_ref() != Some(&sub)).then_some(sub)
                };
                use crate::model::file_analysis::MethodResolution;
                let returns_line = |text: &mut String| {
                    if let Some(rt) = substituted(
                        analysis.find_method_return_type(&cn, field, Some(midx), None),
                    ) {
                        text.push_str(&format!("\n\n*returns: {}*", analysis.render_type(&rt)));
                    }
                };
                let shape = match &r.kind {
                    RefKind::MethodCall { shape, .. } => *shape,
                    _ => Default::default(),
                };
                match analysis.resolve_member_in_ancestors(&cn, field, shape, Some(midx)) {
                    Some(MethodResolution::Local { sym_id, .. }) => {
                        let sym = analysis.symbol(sym_id);
                        if matches!(sym.kind, FaSymKind::Method | FaSymKind::Sub) {
                            let mut text = render_symbol_hover(
                                sym, source, language, analysis, sym.span.start, Some(midx),
                            );
                            returns_line(&mut text);
                            return Some(text);
                        }
                    }
                    // A cross-file method renders exactly like a local one —
                    // its signature line read from the DEFINING file (a cached
                    // analysis carries spans, not source), labeled by kind. The
                    // kind-agnostic `member: type` fallback below is for data
                    // members; a method routed there lost its signature and
                    // read as a property.
                    Some(MethodResolution::CrossFile { class, def_module }) => {
                        let module = def_module.as_deref().unwrap_or(class.as_str());
                        let cached = midx
                            .candidate_defining_sub_in_package(module, &class, field)
                            .or_else(|| midx.get_cached(module));
                        if let Some(cached) = cached {
                            let whole = midx.whole_present(&cached);
                            let sym = whole.symbols().iter().find(|s| {
                                matches!(s.kind, FaSymKind::Method | FaSymKind::Sub)
                                    && s.name == field
                                    && s.package.as_deref() == Some(class.as_str())
                            });
                            if let (Some(sym), Ok(text)) =
                                (sym, std::fs::read_to_string(&cached.path))
                            {
                                let mut out = render_symbol_hover(
                                    sym, &text, language, &whole, sym.span.start, Some(midx),
                                );
                                returns_line(&mut out);
                                return Some(out);
                            }
                        }
                    }
                    None => {}
                }
                // A param-typed member substitutes the same way (`T v_;` on
                // `Box<int>` reads `v_: int`; a cross-file method's return
                // lands here too, so the label stays kind-agnostic).
                if let Some(sub) =
                    substituted(analysis.field_type_on_class(&cn, field, Some(midx)))
                {
                    return Some(format!(
                        "```{}\n{}: {}\n```\n\n*member*",
                        language,
                        field,
                        analysis.render_type(&sub)
                    ));
                }
                // The member's declared type may be a config-variant macro whose
                // flow type is the join abstraction (`Numeric`); display the
                // concrete leaf from the config-active variant's alias chain.
                let storage_leaf = analysis
                    .member_type_spelling(&cn, field, Some(midx))
                    .and_then(|sp| config_variant_leaf_display(analysis, &sp, midx));
                // Domain typing: the slot's storage type (`uint16_t`) discards
                // its DOMAIN (`opcode`), recoverable from usage. When the
                // usage-fold recovers one, it headlines with the storage leaf
                // as a drill-down: `op_type: opcode (stored as uint16_t)`. The
                // domain never overrides storage for correctness — a human
                // surface only.
                if let Some(dom) = analysis.field_domain(&cn, field, Some(midx)) {
                    let stored = storage_leaf
                        .clone()
                        .map(|s| format!(" *(stored as `{}`)*", s))
                        .unwrap_or_default();
                    return Some(format!(
                        "```{}\n{}: {}\n```\n\n*field*{}",
                        language, field, dom.domain, stored
                    ));
                }
                if let Some(leaf) = storage_leaf {
                    return Some(format!("```{}\n{}: {}\n```\n\n*field*", language, field, leaf));
                }
                if let Some(h) = analysis.member_hover(&cn, field, Some(midx)) {
                    return Some(format!("```{}\n{}\n```\n\n*field*", language, h));
                }
            }
        }
    }
    // The current-object receiver (`$this` — the pack's declared receiver
    // names) has no declaration to land on; its value IS the enclosing
    // class, which is what a reader hovering it wants to know.
    if let Some(tok) = sigiled_token_at(source, point) {
        if analysis.pack.receiver_names.iter().any(|n| n == &tok) {
            if let Some(cls) = analysis
                .scope_at(point)
                .and_then(|sc| analysis.enclosing_class_for_scope(sc))
            {
                return Some(format!("```{}\n{}: {}\n```\n\n*variable*", language, tok, cls));
            }
        }
    }
    // The projection's answer: present the top-ranked definition candidate —
    // what goto-def would jump to — wherever it lives (macro variants,
    // template/spec ladders, locals, cross-file functions all arrive here).
    if let Some(loc) = cs.hover_candidate() {
        if let Some(text) = render_candidate_hover(cs, &loc, language) {
            return Some(text);
        }
    }
    // Cursor on a decl the forward walk didn't self-resolve: render the
    // symbol under the cursor directly (its own type point + scope).
    if let Some(sym) = analysis.symbol_at(point) {
        return Some(render_symbol_hover(
            sym, source, language, analysis, point, module_index,
        ));
    }
    None
}

/// The identifier token under `point` INCLUDING a leading sigil (`$this`),
/// the spelling the pack's receiver names use.
fn sigiled_token_at(source: &str, point: Point) -> Option<String> {
    let line = source.lines().nth(point.row)?;
    let b = line.as_bytes();
    let is_tok = |c: u8| c == b'_' || c == b'$' || c.is_ascii_alphanumeric();
    let mut s = point.column.min(b.len());
    if s == b.len() || !is_tok(b[s]) {
        s = s.checked_sub(1)?;
    }
    if !is_tok(b[s]) {
        return None;
    }
    while s > 0 && is_tok(b[s - 1]) {
        s -= 1;
    }
    let mut e = s;
    while e < b.len() && is_tok(b[e]) {
        e += 1;
    }
    Some(line[s..e].to_string())
}

/// Render the hover projection's candidate: the symbol declared at the
/// location — in the origin (fresh text in hand) or a cached pack module
/// (read from disk, suffixed with the defining file's name) — through the
/// same renderer decl-site hovers use. A location no Symbol sits at (a
/// macro def whose Symbol was claimed under another lane, a top-of-file
/// landing) renders its source line, which for a `#define` IS the def.
fn render_candidate_hover(
    cs: &crate::index::resolve::CandidateSet,
    loc: &crate::index::resolve::RefLocation,
    language: &str,
) -> Option<String> {
    let module_index = cs.scoped_index();
    let sym_at = |a: &FileAnalysis| -> Option<usize> {
        a.symbols()
            .iter()
            .position(|s| s.selection_span.start == loc.span.start)
            .or_else(|| {
                a.symbols()
                    .iter()
                    .position(|s| s.selection_span.start.row == loc.span.start.row
                        && crate::model::file_analysis::contains_point(&s.selection_span, loc.span.start))
            })
    };
    if crate::index::resolve::file_key_eq(&loc.key, cs.origin_file_key()) {
        let analysis = cs.origin_analysis();
        let source = cs.origin_source()?;
        if let Some(i) = sym_at(analysis) {
            let sym = &analysis.symbols()[i];
            return Some(render_symbol_hover(
                sym, source, language, analysis, cs.cursor(), module_index,
            ));
        }
        let line = source.lines().nth(loc.span.start.row)?.trim();
        return (!line.is_empty()).then(|| format!("```{}\n{}\n```", language, line));
    }
    let path = crate::index::resolve::key_for_sort(&loc.key);
    let text = std::fs::read_to_string(&path).ok()?;
    let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    // The candidate's own analysis: the scoped index caches every pack file
    // a projection can answer from.
    let mut found: Option<std::sync::Arc<crate::model::file_analysis::CachedModule>> = None;
    if let Some(midx) = module_index {
        midx.for_each_cached_file(&mut |cached| {
            if found.is_none() && cached.path == path {
                found = Some(std::sync::Arc::clone(cached));
            }
        });
    }
    if let Some(cached) = &found {
        let whole = module_index
            .map(|midx| midx.whole_present(cached))
            .unwrap_or_else(|| cached.analysis.clone());
        if let Some(i) = sym_at(&whole) {
            let sym = &whole.symbols()[i];
            let mut out = render_symbol_hover(
                sym, &text, language, &whole, sym.span.start,
                module_index,
            );
            out.push_str(&format!("\n\n— `{}`", fname));
            return Some(out);
        }
    }
    let line = text.lines().nth(loc.span.start.row)?.trim();
    (!line.is_empty()).then(|| format!("```{}\n{}\n```\n\n— `{}`", language, line, fname))
}

/// Render a symbol's hover. Variables/fields show `name: type` (the inferred
/// type — exact class for objects, generic for primitives) rather than the
/// raw decl line, which for a PARAM is the whole function signature. Other
/// kinds show their declaration line + kind (+ class attribute signals).
/// The hover/label word for `sym` — one mapping shared by every render path
/// below (the typed-variable early return AND the declaration-line
/// fallback), so a kind never gets a different label depending on which
/// branch happened to serve it. A `#define`-backed callable is a real
/// `SymKind::Sub` everywhere else (dispatch/completion/goto-def), but its
/// `"macro"` attribute (stamped at extraction) overrides the label here —
/// the attribute is the value-borne "this Sub is macro-shaped" fact,
/// checked before the kind match rather than re-deriving it from the name.
fn hover_kind_label(sym: &crate::model::file_analysis::Symbol) -> &'static str {
    if sym.attributes.iter().any(|a| a == "macro") {
        return "macro";
    }
    match sym.kind {
        FaSymKind::Sub => "function",
        FaSymKind::Method => "method",
        FaSymKind::Class => "class",
        FaSymKind::Package => "namespace",
        FaSymKind::Variable => "variable",
        FaSymKind::Field => "field",
        FaSymKind::Enumerator => "enumerator",
        _ => "symbol",
    }
}

fn render_symbol_hover(
    sym: &crate::model::file_analysis::Symbol,
    source: &str,
    language: &str,
    analysis: &FileAnalysis,
    type_point: Point,
    module_index: Option<&dyn crate::model::file_analysis::CrossFileLookup>,
) -> String {
    if matches!(sym.kind, FaSymKind::Variable | FaSymKind::Field | FaSymKind::Enumerator) {
        if let Some(ty) = analysis.inferred_type_via_bag_ctx(&sym.name, type_point, module_index) {
            // Config-variant macro type → display the concrete leaf recovered
            // from the config-active variant's alias chain, not the join
            // abstraction the type flows as.
            let display = module_index
                .and_then(|midx| {
                    analysis
                        .type_name_edge_of(&sym.name, sym.scope)
                        .and_then(|sp| config_variant_leaf_display(analysis, &sp, midx))
                })
                .unwrap_or_else(|| analysis.display_type_of(sym, &ty));
            // A union member's def-site hover carries the storage overlay,
            // same as the member-access path (`FileAnalysis::member_hover`).
            let overlay = match analysis.union_overlay(sym) {
                Some(sibs) if !sibs.is_empty() => {
                    format!(" — union member (overlays {})", sibs.join(", "))
                }
                _ => String::new(),
            };
            let mut out = format!(
                "```{}\n{}: {}{}\n```\n\n*{}*",
                language, sym.name, display, overlay, hover_kind_label(sym)
            );
            if let Some(doc) = sym.presentation.doc.as_deref() {
                out.push_str("\n\n");
                out.push_str(doc);
            }
            return out;
        }
    }
    // The signature line is the line carrying the NAME token, not the def
    // span's first row — an attributed def (`#[Test]` above a php method,
    // `template<...>` above a cpp fn) starts rows earlier, and rendering
    // that row showed the annotation as the signature.
    // A variable hovered at a REBIND (php's function-scoped locals: one
    // def at the first assignment, every later `$x = …` a rebind) must not
    // show the first assignment's line as if it were this site's — that
    // attributes another branch's code to the cursor. Untyped there, the
    // honest answer is the name alone.
    if matches!(sym.kind, FaSymKind::Variable) && type_point.row != sym.selection_span.start.row {
        return format!("```{}\n{}\n```\n\n*{}*", language, sym.name, hover_kind_label(sym));
    }
    let line = source.lines().nth(sym.selection_span.start.row).unwrap_or("").trim();
    let sig = line.trim_end_matches([' ', '{', ';']).trim();
    let mut out = format!("```{}\n{}\n```\n\n*{}*", language, sig, hover_kind_label(sym));
    if matches!(sym.kind, FaSymKind::Class) {
        for attr in &sym.attributes {
            out.push_str(&format!("\n\n*{}*", attr));
        }
    }
    if let Some(doc) = sym.presentation.doc.as_deref() {
        out.push_str("\n\n");
        out.push_str(doc);
    }
    out
}

pub fn pack_hover(cs: &crate::index::resolve::CandidateSet, language: &str) -> Option<Hover> {
    let value = pack_hover_markdown(cs, language)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

/// Perl hover: a presenter over one resolution. Local identity — symbols,
/// method dispatch, hash keys — renders through the model's
/// `FileAnalysis::hover_info`; the cross-file call lanes present what the
/// CandidateSet resolved: builtin membership is the model's builtin table
/// (doc VALUE from `module_index.builtin_doc`, hydrated from SQLite —
/// parsed from `perlfunc.pod` only on cold-cache miss), and the
/// import / FQ-package binding comes from `cs.function_binding()` — the
/// same lanes `definitions()` jumps through — so hover presents exactly
/// what goto-def would reach.
pub fn perl_hover(
    cs: &crate::index::resolve::CandidateSet,
    module_index: &ModuleIndex,
) -> Option<Hover> {
    let value = perl_hover_markdown(cs, module_index)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

pub fn perl_hover_markdown(
    cs: &crate::index::resolve::CandidateSet,
    module_index: &ModuleIndex,
) -> Option<String> {
    if let Some(markdown) = perl_hover_named(cs, module_index) {
        return Some(markdown);
    }
    // Nothing the name-keyed ladder above knows how to render. Present the
    // hover projection's candidate — the definition goto-def would jump to —
    // exactly as `pack_hover_markdown` does, so the two verbs cannot disagree
    // about whether a token resolves (`docs/adr/resolution-candidate-set.md`).
    // A module-name token was the standing case: `Koha::Database->new` is a
    // `PackageRef` whose package lives in another file, which the ladder's
    // local `symbols_named` sweep can never see.
    let loc = cs.hover_candidate()?;
    render_candidate_hover(cs, &loc, "perl")
}

/// The name-keyed Perl ladder: whatever the model can render from the ref or
/// symbol under the cursor, plus the import/qualified-call signature lookups
/// that read a cross-file `SubInfo` by NAME. Everything here is richer than a
/// rendered declaration line, so it runs before the projection fallback.
fn perl_hover_named(
    cs: &crate::index::resolve::CandidateSet,
    module_index: &ModuleIndex,
) -> Option<String> {
    let analysis = cs.origin_analysis();
    let source = cs.origin_source()?;
    let point = cs.cursor();

    // Local hover first — the model renderer.
    if let Some(markdown) = analysis.hover_info(point, source, Some(module_index)) {
        return Some(markdown);
    }

    let r = analysis.ref_at(point)?;
    if !matches!(r.kind, RefKind::FunctionCall { .. }) {
        return None;
    }
    if crate::model::builtins::is_builtin(&r.target_name) {
        if let Some(markdown) = module_index.builtin_doc(&r.target_name) {
            return Some(markdown);
        }
    }
    match cs.function_binding()? {
        crate::index::resolve::FunctionBinding::Imported { import, remote: remote_name, .. } => {
            let mut parts = Vec::new();

            // Show signature if available. Cross-file lookup uses
            // the REMOTE name — for a renaming import (`del` →
            // `delete`), cursor is on `del` but sub_info lives
            // under `delete` in the cached module.
            if let Some(cached) = module_index
                .defining_module_cached(&import.module_name, &remote_name)
                // A split exporter's sub may live in a losing candidate —
                // pick by the queried symbol, keeping the plain winner as
                // the last resort (hover still names the module's file).
                .or_else(|| module_index.candidate_defining_sub(&import.module_name, &remote_name))
                .or_else(|| module_index.get_cached(&import.module_name))
            {
                let whole = module_index.bag_present(&cached);
                if let Some(sub_info) = whole.sub_info_view(&remote_name) {
                    // Present the sig under the LOCAL name — that's
                    // what the user typed and what hover should lead
                    // with; the remote name is just how we fetched it.
                    let sig = format_imported_signature(&r.target_name, &sub_info);
                    parts.push(format!("```perl\n{}\n```", sig));
                    if let Some(doc) = sub_info.doc() {
                        parts.push(doc.to_string());
                    }
                }
            }

            if remote_name != r.target_name {
                parts.push(format!(
                    "*imported from `{}` (as `{}`)*",
                    import.module_name, remote_name
                ));
            } else {
                parts.push(format!("*imported from `{}`*", import.module_name));
            }
            Some(parts.join("\n\n"))
        }
        crate::index::resolve::FunctionBinding::Qualified { package: pkg } => {
            let bare = r.unqualified_target_name();
            // Symbol-disambiguated: the file defining `bare`, not the
            // name-slot winner (same rule as the FQ goto-def lane).
            let cached = module_index.candidate_defining_sub(pkg, bare)?;
            let whole = module_index.bag_present(&cached);
            let sub_info = whole.sub_info_view(bare)?;
            let sig = format_imported_signature(bare, &sub_info);
            let mut parts = vec![format!("```perl\n{}\n```", sig)];
            if let Some(doc) = sub_info.doc() {
                parts.push(doc.to_string());
            }
            parts.push(format!("*from `{}`*", pkg));
            Some(parts.join("\n\n"))
        }
    }
}
