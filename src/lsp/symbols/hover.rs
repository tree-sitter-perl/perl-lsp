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
    if let Some((r, want)) = analysis
        .ref_at(point)
        .and_then(|r| Some((r, crate::model::file_analysis::MemberKind::of_ref(&r.kind)?)))
    {
        if let Some(midx) = module_index {
            // The receiver's full VALUE (not just its dispatch class): a
            // template instance's args refine a param-shaped member type
            // (`T get()` on a `Box<int>` receiver → `int`) — shown only when
            // the substitution actually changed the answer, so non-template
            // hovers stay byte-identical. Each class reads the arm it came
            // from; one section per class the receiver may be.
            let recv_ty = r
                .member_site()
                .and_then(|m| m.invocant_span)
                .and_then(|sp| analysis.expr_type_at_span(sp, Some(midx)));
            let arm_for = |cn: &str| {
                let arms = recv_ty.as_ref().map_or(&[][..], InferredType::receiver_arms);
                match arms {
                    [one] => Some(one),
                    arms => arms
                        .iter()
                        .find(|a| analysis.dispatch_class_of(a, Some(midx)).as_deref() == Some(cn)),
                }
            };
            // A cross-file method's declaration is the projection's
            // candidate, rendered from its own file.
            let declaration = |via: &CallView<'_>, defining: &str| {
                cs.hover_candidate().and_then(|loc| render_candidate_hover(cs, &loc, language, Some((via, defining))))
            };
            let sections: Vec<String> = analysis
                .receiver_classes(r, Some(midx), ReceiverPolicy::Navigate)
                .iter()
                .filter_map(|cn| {
                    // A construction names each class's constructor.
                    let field = analysis.called_member(r, cn, Some(midx))?;
                    member_hover_on(analysis, cn, &field, want, arm_for(cn), source, language, midx, &declaration)
                })
                .collect();
            if !sections.is_empty() {
                return Some(sections.join("\n\n---\n\n"));
            }
        }
    }
    let token = pack_token_hover(cs, language)?;
    // A construction's class token (`new Foo(…)`) is the class — goto-def's
    // landing, shown first — and the call of its constructor, shown under
    // it as the call a member access to it would show.
    let construction = analysis.construction_at(point).zip(module_index).and_then(|(r, midx)| {
        let sections: Vec<String> = analysis
            .receiver_classes(r, Some(midx), ReceiverPolicy::Navigate)
            .iter()
            .filter_map(|cn| {
                let ctor = analysis.called_member(r, cn, Some(midx))?;
                // No projection candidate names the constructor: a
                // cross-file one renders from the file declaring it.
                let declaration =
                    |via: &CallView<'_>, defining: &str| cross_file_member_hover(&ctor, defining, via, language, midx);
                let callable = crate::model::file_analysis::MemberKind::CALLABLE;
                member_hover_on(analysis, cn, &ctor, callable, None, source, language, midx, &declaration)
            })
            .collect();
        (!sections.is_empty()).then(|| sections.join("\n\n---\n\n"))
    });
    Some(match construction {
        Some(ctor) => format!("{token}\n\n---\n\n{ctor}"),
        None => token,
    })
}

/// The hover of the token under the cursor when no member access answers
/// it: the projection's candidate, else the declaration at the cursor.
fn pack_token_hover(cs: &crate::index::resolve::CandidateSet, language: &str) -> Option<String> {
    let analysis = cs.origin_analysis();
    let source = cs.origin_source()?;
    let point = cs.cursor();
    let module_index = cs.scoped_index();
    // The projection's answer: present the top-ranked definition candidate —
    // what goto-def would jump to — wherever it lives (macro variants,
    // template/spec ladders, locals, cross-file functions all arrive here).
    if let Some(loc) = cs.hover_candidate() {
        if let Some(text) = render_candidate_hover(cs, &loc, language, None) {
            return Some(text);
        }
    }
    // Cursor on a decl the forward walk didn't self-resolve: render the
    // symbol under the cursor directly (its own type point + scope).
    if let Some(sym) = analysis.symbol_at(point) {
        return Some(render_symbol_hover(
            sym, source, &sym.span.start, language, analysis, value_point(analysis, point), module_index, None,
        ));
    }
    None
}

/// Where the value a token at `point` denotes is read: a written token
/// denotes what its write leaves, which lands after the token (an
/// assignment lands once its right side is read); any other token, what
/// holds at the token.
fn value_point(analysis: &FileAnalysis, point: Point) -> Point {
    analysis.write_lands_at(point).unwrap_or(point)
}

/// What a call knows about the callable it reaches that the declaration
/// alone does not: the class the call is made on and the return the call
/// gives there (a receiver-relative or template-substituted one).
struct CallView<'a> {
    class: &'a str,
    returns: Option<InferredType>,
}

/// The callable `member` of class `defining`, declared in another file,
/// rendered from that file for the call `via`.
fn cross_file_member_hover(
    member: &str,
    defining: &str,
    via: &CallView<'_>,
    language: &str,
    midx: &dyn crate::model::file_analysis::CrossFileLookup,
) -> Option<String> {
    let cached = midx.candidate_defining_sub_in_package(defining, defining, member)?;
    let whole = midx.whole_present(&cached);
    let sym = whole.symbols().iter().find(|s| {
        s.name == member && matches!(s.kind, FaSymKind::Sub | FaSymKind::Method) && s.package.as_deref() == Some(defining)
    })?;
    let text = crate::util::text::read_source(&cached.path).ok()?;
    let fname = cached.path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let mut out = render_symbol_hover(sym, &text, &sym.span.start, language, &whole, sym.span.start, Some(midx), Some(via));
    out.push_str(&format!("\n\n— `{}`", fname));
    Some(out)
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
    via: Option<(&CallView<'_>, &str)>,
) -> Option<String> {
    // Rendered for a call, the candidate must be the declaration the call
    // resolved to (`defining`'s member), not another class's.
    let call = |sym: &crate::model::file_analysis::Symbol| match via {
        Some((v, defining)) => (sym.package.as_deref() == Some(defining)).then_some(Some(v)),
        None => Some(None),
    };
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
                sym, source, &sym.span.start, language, analysis, value_point(analysis, cs.cursor()), module_index,
                call(sym)?,
            ));
        }
        if via.is_some() {
            return None;
        }
        let line = source.lines().nth(loc.span.start.row)?.trim();
        return (!line.is_empty()).then(|| format!("```{}\n{}\n```", language, line));
    }
    let path = crate::index::resolve::key_for_sort(&loc.key);
    let text = crate::util::text::read_source(&path).ok()?;
    let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    // The candidate's own analysis: the scoped index caches every pack file
    // a projection can answer from.
    let found = module_index.zip(loc.key.canonical_path()).and_then(|(midx, p)| midx.cached_by_path(p));
    if let Some(cached) = &found {
        let whole = module_index
            .map(|midx| midx.whole_present(cached))
            .unwrap_or_else(|| cached.analysis.clone());
        if let Some(i) = sym_at(&whole) {
            let sym = &whole.symbols()[i];
            let mut out = render_symbol_hover(
                sym, &text, &sym.span.start, language, &whole, sym.span.start,
                module_index, call(sym)?,
            );
            out.push_str(&format!("\n\n— `{}`", fname));
            return Some(out);
        }
    }
    if via.is_some() {
        return None;
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
    if sym.flags.contains(crate::model::file_analysis::SymbolFlags::MACRO) {
        return "macro";
    }
    if sym.flags.contains(crate::model::file_analysis::SymbolFlags::CONSTANT) {
        return "constant";
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

/// `via` is the call the hover is made at, when it is made at one: its
/// class and its return stand for the declaration's own.
#[allow(clippy::too_many_arguments)]
fn render_symbol_hover(
    sym: &crate::model::file_analysis::Symbol,
    source: &str,
    line_at: &Point,
    language: &str,
    analysis: &FileAnalysis,
    type_point: Point,
    module_index: Option<&dyn crate::model::file_analysis::CrossFileLookup>,
    via: Option<&CallView<'_>>,
) -> String {
    let doc = |out: &mut String| {
        if let Some(d) = sym.doc() {
            out.push_str("\n\n");
            out.push_str(d);
        }
    };
    if matches!(sym.kind, FaSymKind::Variable | FaSymKind::Field | FaSymKind::Enumerator) {
        if let Some(ty) = analysis
            .declared_value_type(sym.id, type_point, module_index)
            .filter(InferredType::is_known)
        {
            // Config-variant macro type → display the concrete leaf recovered
            // from the config-active variant's alias chain, not the join
            // abstraction the type flows as.
            let display = module_index
                .and_then(|midx| {
                    analysis
                        .type_name_edge_of(sym.id)
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
                language, sym.listed_name(), display, overlay, hover_kind_label(sym)
            );
            doc(&mut out);
            return out;
        }
    }
    let line = source.lines().nth(line_at.row).unwrap_or("").trim();
    let sig = line.trim_end_matches([' ', '{', ';']).trim();
    let mut out = format!("```{}\n{}\n```\n\n*{}*", language, sig, hover_kind_label(sym));
    if matches!(sym.kind, FaSymKind::Class) {
        for attr in &sym.attributes {
            out.push_str(&format!("\n\n*{}*", attr));
        }
    }
    if !matches!(sym.kind, FaSymKind::Sub | FaSymKind::Method) {
        doc(&mut out);
        return out;
    }
    // Where it lives: the class a call is made on (and the one declaring
    // the member, where that differs), else the declaration's own owner —
    // a method's class, a function's namespace.
    let defining = sym.package.as_deref();
    let owner = match (via, defining) {
        (Some(v), Some(d)) if v.class != d => Some(format!("{} (from {})", v.class, d)),
        (Some(v), _) => Some(v.class.to_string()),
        (None, d) => d.map(str::to_string),
    };
    let word = if via.is_some() || sym.kind == FaSymKind::Method { "class" } else { "namespace" };
    let returns = match via {
        Some(v) => v.returns.clone(),
        None => analysis.symbol_return_type_via_bag_ctx(sym.id, None, module_index),
    }
    .filter(InferredType::is_known)
    .map(|rt| analysis.render_type(&rt));
    out.push_str(&crate::model::file_analysis::callable_hover_tail(
        owner.as_deref().map(|o| (word, o)),
        returns.as_deref(),
        sym.doc(),
    ));
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
    render_candidate_hover(cs, &loc, "perl", None)
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
    if !matches!(r.kind, RefKind::FunctionCall | RefKind::ImportedName) {
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
                    let sig = format_imported_signature(analysis, &r.target_name, &sub_info);
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
            let bare = r.unqualified_target_name(analysis.names());
            // Symbol-disambiguated: the file defining `bare`, not the
            // name-slot winner (same rule as the FQ goto-def lane).
            let cached = module_index.candidate_defining_sub(pkg, bare)?;
            let whole = module_index.bag_present(&cached);
            let sub_info = whole.sub_info_view(bare)?;
            let sig = format_imported_signature(analysis, bare, &sub_info);
            let mut parts = vec![format!("```perl\n{}\n```", sig)];
            if let Some(doc) = sub_info.doc() {
                parts.push(doc.to_string());
            }
            parts.push(format!("*from `{}`*", pkg));
            Some(parts.join("\n\n"))
        }
    }
}

/// One class's member hover: `field` on `cn`, `recv` the receiver value
/// that dispatches there (its template args refine a param-shaped member).
/// A method answers with its signature and the return the call gives on
/// `cn` (`declaration` renders a cross-file one, given the call and the
/// class declaring the member); only a field answers with its type.
#[allow(clippy::too_many_arguments)]
fn member_hover_on(
    analysis: &crate::model::file_analysis::FileAnalysis,
    cn: &str,
    field: &str,
    want: crate::model::file_analysis::MemberKind,
    recv: Option<&InferredType>,
    source: &str,
    language: &str,
    midx: &dyn crate::model::file_analysis::CrossFileLookup,
    declaration: &dyn Fn(&CallView<'_>, &str) -> Option<String>,
) -> Option<String> {
    // The ref's family picks the rung: a value read never answers
    // through a method's return.
    let substituted = |raw: Option<InferredType>| -> Option<InferredType> {
        let sub = recv.and_then(|t| match want.mode {
            crate::model::file_analysis::MemberMode::Value => {
                analysis.field_value_type(t, field, Some(midx), want)
            }
            crate::model::file_analysis::MemberMode::Callable => {
                analysis.member_value_type(t, field, Some(midx), None)
            }
        })?;
        (raw.as_ref() != Some(&sub)).then_some(sub)
    };
    // The return the call gives on `cn`: asked of `cn`, so a
    // receiver-relative return (php's `static`) names it, and refined by
    // the receiver's template arguments where they change it.
    let call_returns = || {
        let raw = analysis.find_method_return_type(cn, field, Some(midx), None);
        substituted(raw.clone()).or(raw)
    };
    let resolution = analysis.resolve_member_in_ancestors(cn, field, Some(midx), want);
    if let Some(crate::model::file_analysis::MethodResolution::Local { sym_id, .. }) = resolution {
        let sym = analysis.symbol(sym_id);
        if matches!(sym.kind, FaSymKind::Method | FaSymKind::Sub) {
            let via = CallView { class: cn, returns: call_returns() };
            return Some(render_symbol_hover(
                sym, source, &sym.span.start, language, analysis, sym.span.start, Some(midx), Some(&via),
            ));
        }
    }
    if want.mode == crate::model::file_analysis::MemberMode::Callable {
        // A cross-file method shows its declaration, rendered from its own
        // file for this call.
        let crate::model::file_analysis::MethodResolution::CrossFile { class, .. } = resolution? else {
            return None;
        };
        return declaration(&CallView { class: cn, returns: call_returns() }, &class);
    }
    // A param-typed field substitutes the same way (`T v_;` on `Box<int>`
    // reads `v_: int`).
    if let Some(sub) =
        substituted(analysis.field_type_on_class(cn, field, Some(midx), want))
    {
        return Some(format!(
            "```{}\n{}: {}\n```\n\n*field*",
            language,
            field,
            analysis.render_type(&sub)
        ));
    }
    // The member's declared type may be a config-variant macro whose
    // flow type is the join abstraction (`Numeric`); display the
    // concrete leaf from the config-active variant's alias chain.
    let storage_leaf = analysis
        .member_type_spelling(cn, field, Some(midx))
        .and_then(|sp| config_variant_leaf_display(analysis, &sp, midx));
    // Domain typing: the slot's storage type (`uint16_t`) discards
    // its DOMAIN (`opcode`), recoverable from usage. When the
    // usage-fold recovers one, it headlines with the storage leaf
    // as a drill-down: `op_type: opcode (stored as uint16_t)`. The
    // domain never overrides storage for correctness — a human
    // surface only.
    if let Some(dom) = analysis.field_domain(cn, field, Some(midx)) {
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
    if let Some(h) = analysis.member_hover(cn, field, Some(midx)) {
        return Some(format!("```{}\n{}\n```\n\n*field*", language, h));
    }
    None
}
