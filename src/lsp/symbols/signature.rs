//! Signature help and string-dispatch completion (dispatch arg-0, mid-string).

use super::*;

// ---- Signature help ----

/// Does this class have ANY registered Handlers matching the method
/// name as a declared dispatcher? Used to decide whether we're in a
/// "known dispatch call" context — gates the noise-suppression logic
/// around it so the firehose of unrelated subs only gets suppressed
/// when we actually know this is a dispatcher call site.
pub(super) fn class_has_dispatch_handlers(
    analysis: &FileAnalysis,
    module_index: &ModuleIndex,
    class: &str,
    dispatcher: &str,
) -> bool {
    // Funnels through `for_each_dispatch_handler_on_class` (the
    // ancestor-aware bridge walker) so this predicate agrees with
    // `dispatch_target_completions`: if completion would offer at
    // least one handler, this returns true. `found` short-circuits
    // semantically — the walker still visits every class, but per-
    // class closure exits cheaply once the flag is set.
    let mut found = false;
    analysis.for_each_dispatch_handler_on_class(
        class,
        dispatcher,
        Some(module_index),
        |_sym, _prov| { found = true; },
    );
    found
}

/// Span of the string-literal's CONTENT (between the quotes) at `point`,
/// if the cursor is inside one. Returns `None` for non-string contexts.
///
/// Used by mid-string completions (dispatch-target handlers, method-ref
/// mid-string) to anchor a `TextEdit` range. Without this, the client's
/// word-at-cursor heuristic (nvim's `iskeyword` default excludes `/`
/// and `#`) mis-extracts the typed prefix for non-identifier labels
/// like `/users/profile` or `Users#list` and drops valid matches.
/// Setting `textEdit.range` to the content span makes the client match
/// the whole in-range text against the label.
///
/// Empty strings (`url_for('')`): no `string_content` child exists, so
/// fall back to a zero-width span at the cursor. Any prefix match
/// against "" passes, which is the right answer for "user hasn't
/// typed anything in the string yet".
pub(super) fn string_content_span_at(tree: &Tree, point: Point) -> Option<Span> {
    let mut node = tree.root_node().descendant_for_point_range(point, point)?;
    for _ in 0..4 {
        match node.kind() {
            "string_content" => {
                return Some(Span {
                    start: node.start_position(),
                    end: node.end_position(),
                });
            }
            "string_literal" | "interpolated_string_literal" => {
                // Boundary case: when the cursor sits at the END of
                // the content (just before the closing quote) or on
                // the closing quote itself, `descendant_for_point_range`
                // lands on the literal wrapper instead of `string_content`
                // — content ranges are half-open, so the end column
                // isn't "contained". Look down for a `string_content`
                // child and use its span. Otherwise we'd return a
                // zero-width range at cursor, which makes textEdit
                // APPEND the label after the user's typed text
                // (`'/fall' → '/fall/fallback'`) instead of replacing it.
                let mut walker = node.walk();
                for child in node.named_children(&mut walker) {
                    if child.kind() == "string_content" {
                        return Some(Span {
                            start: child.start_position(),
                            end: child.end_position(),
                        });
                    }
                }
                // Genuinely empty literal (`''` with cursor between the
                // quotes). Zero-width range at cursor is correct here —
                // there's nothing to replace.
                return Some(Span { start: point, end: point });
            }
            _ => {}
        }
        let Some(p) = node.parent() else { break };
        node = p;
    }
    None
}

/// Rewrite each item's replace range to `span`, materialized as a
/// `TextEdit` on `text_edit`. Used for mid-string completions where
/// the client's default word-extraction would otherwise misfilter the
/// item (see `string_content_span_at`).
///
/// `newText` is the bare `label` — NOT `insert_text`. `insert_text`
/// from other code paths can carry wrapping quotes (`'connect'` for
/// the bare-parens case), and the replace range we're setting already
/// sits INSIDE the existing string's quotes. Threading insert_text
/// through here would insert `'connect'` inside `''` → `''connect''`.
/// Using the label keeps the invariant simple: whatever span we're
/// replacing, the replacement is the identifier text, no decoration.
/// `insert_text` is also cleared — textEdit takes precedence in the
/// LSP spec, and leaving both set confuses some clients.
pub(super) fn retarget_items_to_span(items: &mut [CompletionItem], span: Span) {
    let range = span_to_range(span);
    for item in items {
        item.text_edit = Some(tower_lsp::lsp_types::CompletionTextEdit::Edit(
            tower_lsp::lsp_types::TextEdit { range, new_text: item.label.clone() },
        ));
        item.insert_text = None;
    }
}

/// True if the cursor sits somewhere that makes wrapping-quotes in the
/// insert_text wrong. Two cases:
///   * cursor in a `string_literal` / `interpolated_string_literal`
///     (the quotes are already typed)
///   * cursor at a fat-comma LHS autoquote position (the `=>` will
///     autoquote whatever bareword lands there)
fn cursor_in_string_or_autoquote(tree: &Tree, point: Point) -> bool {
    let Some(mut node) = tree.root_node().descendant_for_point_range(point, point) else {
        return false;
    };
    // Walk upward a handful of levels — tree-sitter may hand back a
    // token node first; the enclosing string_literal is typically one
    // or two parents up.
    for _ in 0..4 {
        match node.kind() {
            "string_literal" | "interpolated_string_literal"
            | "string_content" | "autoquoted_bareword" => return true,
            _ => {}
        }
        let Some(p) = node.parent() else { break };
        node = p;
    }
    false
}

/// Completions for the first arg of a string-dispatched method call.
/// Walks every Handler symbol (local + cross-file via module index),
/// filters by (owner class matches receiver, dispatcher matches method
/// name), and returns each as a CompletionItem. Stacked registrations
/// dedup on name; the completion shows the param shape in detail so
/// you know what args the handler will get.
pub(super) fn dispatch_target_completions(
    analysis: &FileAnalysis,
    module_index: &ModuleIndex,
    invocant: Option<&str>,
    method_name: &str,
    point: Point,
    tree: &Tree,
) -> Vec<CompletionCandidate> {
    let class = match analysis.invocant_text_to_class(invocant, point) {
        Some(c) => c,
        None => return Vec::new(),
    };

    // Quote-aware insert_text. Three cases:
    //   * cursor inside a string_literal (`->emit('|')`) — the quotes
    //     are already there, emit bare name.
    //   * cursor at fat-comma LHS autoquote position (`->emit(|=>...)`)
    //     — no quotes needed, Perl auto-quotes barewords on fat-comma
    //     LHS. (Dispatch shouldn't normally fire here, but defensive.)
    //   * anywhere else — emit with surrounding quotes so one accept
    //     keystroke produces `'name'` in bare parens.
    // Implemented as a closure so the branching stays adjacent to the
    // decision and every emitted candidate is consistent.
    let needs_quotes = !cursor_in_string_or_autoquote(tree, point);

    // Accumulate (handler_name → display_params + provenance) so stacked
    // registrations across files appear once in the completion list.
    // The walker funnels local + namespace-bridged + cross-file via
    // `for_each_entity_bridged_to` (rule #8) and walks ancestors so
    // `$c->url_for('|')` on a `Users` controller surfaces routes whose
    // Handlers live on `Mojolicious::Controller` (the shared base).
    let mut acc: std::collections::BTreeMap<String, (Vec<String>, String)> =
        std::collections::BTreeMap::new();
    analysis.for_each_dispatch_handler_on_class(
        &class, method_name, Some(module_index),
        |sym, provenance| {
            let SymbolDetail::Handler { params, .. } = &sym.detail else { return };
            let display: Vec<String> = params
                .iter()
                .filter(|p| !p.is_invocant)
                .map(|p| p.name.clone())
                .collect();
            acc.entry(sym.name.clone()).or_insert((display, provenance.to_string()));
        },
    );

    acc.into_iter().map(|(name, (params, provenance))| {
        let detail = if params.is_empty() {
            format!("handler on {}  ({})", class, provenance)
        } else {
            format!("handler on {} ({})  — {}", class, params.join(", "), provenance)
        };
        CompletionCandidate {
            label: name.clone(),
            is_static: false,
            // Handler kind flows to CompletionItemKind::EVENT via
            // `fa_completion_kind` — consistent with outline and hover.
            kind: FaSymKind::Handler,
            detail: Some(detail),
            // Bare inside quotes / autoquote, quoted otherwise — see
            // `needs_quotes` above. Accepting the suggestion lands
            // correct source text regardless of where the cursor was.
            insert_text: Some(if needs_quotes {
                format!("'{}'", name)
            } else {
                name.clone()
            }),
            // Top of the list: handlers are the canonical completion in
            // this position when a dispatcher is declared for them.
            sort_priority: 0,
            additional_edits: Vec::new(),
                import_fact: None,
                display_override: None,
        }
    }).collect()
}

/// Collect refs whose span contains `point` and match a predicate.
/// Small generic helper — used by mid-string completion to find the
/// (typically unique) MethodCallRef at the cursor.
pub(super) fn refs_at_point_matching<'a>(
    analysis: &'a FileAnalysis,
    point: Point,
    pred: impl Fn(&crate::model::file_analysis::Ref) -> bool,
) -> Option<Vec<&'a crate::model::file_analysis::Ref>> {
    let out: Vec<&crate::model::file_analysis::Ref> = analysis.refs().iter()
        .filter(|r| span_contains_point(&r.span, point) && pred(r))
        .collect();
    if out.is_empty() { None } else { Some(out) }
}

fn span_contains_point(span: &crate::model::file_analysis::Span, p: Point) -> bool {
    let a = (span.start.row, span.start.column);
    let b = (span.end.row, span.end.column);
    let pp = (p.row, p.column);
    a <= pp && pp <= b
}

/// Mid-string completion for a cursor inside a plugin-emitted
/// `MethodCallRef`. Offers methods on the invocant class (walking
/// inheritance + workspace), prefix-filtered by whatever the user has
/// typed since the start of the ref's span.
///
/// The core is deliberately ignorant of plugin-specific string formats
/// (no `#` splitting, no `::` splitting — that's Mojo-routes syntax
/// bleeding in). It's the plugin's job to emit a tight span that
/// covers only the method-name portion of its string; the core just
/// slices `source[ref.span.start..cursor]` and uses that as the prefix.
/// If a plugin wants fuzzier matching behavior it can widen its spans;
/// the semantics stay plugin-controlled.
pub(super) fn mid_string_methodref_completions(
    analysis: &FileAnalysis,
    module_index: &ModuleIndex,
    invocant_class: &str,
    source: &str,
    point: Point,
    ref_span: crate::model::file_analysis::Span,
) -> Vec<CompletionItem> {
    // Pull the typed prefix out of the live source text — not from the
    // parser, because during active editing the two diverge.
    let lines: Vec<&str> = source.lines().collect();
    if ref_span.start.row >= lines.len() || point.row >= lines.len() {
        return Vec::new();
    }
    let typed = if ref_span.start.row == point.row {
        let line = lines[point.row];
        let start = ref_span.start.column.min(line.len());
        let end = point.column.min(line.len());
        &line[start..end]
    } else {
        // Multi-line ref spans: conservative — only use the current line.
        let line = lines[point.row];
        &line[..point.column.min(line.len())]
    };

    let candidates = analysis.complete_methods_for_class(invocant_class, Some(module_index));
    let mut items: Vec<CompletionItem> = candidates
        .into_iter()
        .filter(|c| typed.is_empty() || c.label.starts_with(typed))
        .map(|c| {
            let mut item = candidate_to_completion_item(c);
            item.sort_text = Some(format!("000{}", item.label));
            item
        })
        .collect();
    // Anchor the replace range to the ref's span (already tight on
    // the method-name portion — plugins control its width). Without
    // this, labels containing non-identifier chars (`Ctrl#act` past
    // the `#`) get dropped by the client's word-match. Same fix as
    // dispatch-target items, same reason.
    retarget_items_to_span(&mut items, ref_span);
    items
}

/// Materialize `PluginCompletion` items for every Handler symbol
/// whose owner matches `owner_class` and whose `dispatchers` list
/// contains any of `dispatcher_names`. Walks the local analysis AND
/// every cross-file cached module. Used by plugin-delegated
/// dispatch-name completion (Minion enqueue arg-0, Mojo emit arg-0,
/// etc.).
pub(super) fn dispatch_target_items_for(
    analysis: &FileAnalysis,
    module_index: &ModuleIndex,
    owner_class: &str,
    dispatcher_names: &[String],
) -> Vec<CompletionItem> {
    use crate::model::file_analysis::SymbolDetail;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<CompletionItem> = Vec::new();
    let mut emit = |sym: &crate::model::file_analysis::Symbol| {
        let SymbolDetail::Handler { .. } = &sym.detail else { return };
        if !seen.insert(sym.name.clone()) { return; }
        let display = sym.presentation.display.unwrap_or_default();
        let detail = display.outline_word().map(|s| s.to_string());
        out.push(CompletionItem {
            label: sym.name.clone(),
            kind: Some(handler_display_to_completion_kind(&display)),
            detail,
            filter_text: Some(sym.name.clone()),
            sort_text: Some(format!(" 000{}", sym.name)),
            insert_text: Some(format!("'{}'", sym.name)),
            ..Default::default()
        });
    };
    for sym in analysis.handlers_for_owner(owner_class, dispatcher_names) {
        emit(sym);
    }
    module_index.for_each_cached(|_, cached| {
        let whole = module_index.whole_present(cached);
        for sym in whole.handlers_for_owner(owner_class, dispatcher_names) {
            emit(sym);
        }
    });
    out
}

/// Convert a plugin's minimal `PluginSignatureHelp` to the full LSP
/// `SignatureHelp` shape. Core fills in `active_signature` and the
/// per-parameter scaffolding so plugin-side Rhai stays ergonomic.
fn plugin_sig_to_lsp(p: crate::build::plugin::PluginSignatureHelp) -> SignatureHelp {
    let parameters: Vec<ParameterInformation> = p.params.iter().cloned()
        .map(|label| ParameterInformation {
            label: ParameterLabel::Simple(label),
            documentation: None,
        })
        .collect();
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label: p.label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(p.active_param as u32),
        }],
        active_signature: Some(0),
        active_parameter: Some(p.active_param as u32),
    }
}

/// Convert a plugin completion hint to LSP `CompletionItemKind`.
fn plugin_completion_kind_hint(h: &crate::build::plugin::CompletionKindHint) -> CompletionItemKind {
    use crate::build::plugin::CompletionKindHint as K;
    match h {
        K::Function | K::Task | K::Helper | K::Route => CompletionItemKind::FUNCTION,
        K::Method => CompletionItemKind::METHOD,
        K::Field => CompletionItemKind::FIELD,
        K::Property => CompletionItemKind::PROPERTY,
        K::Value => CompletionItemKind::VALUE,
        K::Event => CompletionItemKind::EVENT,
        K::Operator => CompletionItemKind::OPERATOR,
        K::Keyword => CompletionItemKind::KEYWORD,
    }
}

pub(super) fn plugin_completion_to_item(p: crate::build::plugin::PluginCompletion) -> CompletionItem {
    let filter_text = Some(p.label.clone());
    let kind = plugin_completion_kind_hint(&p.kind);
    // Map the semantic hint to an outline-style detail word so the
    // client can distinguish Task/Helper/Route from plain Function.
    let detail = p.detail.or_else(|| match p.kind {
        crate::build::plugin::CompletionKindHint::Task => Some("task".into()),
        crate::build::plugin::CompletionKindHint::Helper => Some("helper".into()),
        crate::build::plugin::CompletionKindHint::Route => Some("route".into()),
        _ => None,
    });
    CompletionItem {
        label: p.label,
        kind: Some(kind),
        detail,
        insert_text: p.insert_text,
        filter_text,
        sort_text: Some(" 000".into()), // space prefix sorts above digit-prefixed priorities
        ..Default::default()
    }
}

/// Find the enclosing method_call_expression at `point` and return any
/// `DispatchCall` ref whose span sits inside that call's argument list.
/// Returns `(handler_name, owner_class, dispatcher)` — all three are
/// already plugin-resolved, so the caller inherits const-folding and
/// receiver-type inference without re-deriving them.
fn dispatch_info_for_enclosing_call(
    analysis: &FileAnalysis,
    tree: &Tree,
    _source: &[u8],
    point: Point,
) -> Option<(String, String, String)> {
    // Walk up from the cursor until we hit the enclosing method call.
    let mut node = tree.root_node().descendant_for_point_range(point, point)?;
    let call = loop {
        if node.kind() == "method_call_expression" {
            break node;
        }
        node = node.parent()?;
    };
    let call_start = crate::model::file_analysis::Span {
        start: call.start_position(),
        end: call.end_position(),
    };

    // First DispatchCall ref whose span is contained by this call.
    for r in analysis.refs() {
        let RefKind::DispatchCall { dispatcher } = &r.kind else { continue };
        if !span_contains_span(&call_start, &r.span) { continue; }
        let Some(HandlerOwner::Class(class)) = r.handler_owner() else { continue };
        return Some((r.target_name.clone(), class.clone(), dispatcher.clone()));
    }
    None
}

fn span_contains_span(outer: &crate::model::file_analysis::Span, inner: &crate::model::file_analysis::Span) -> bool {
    let o_start = (outer.start.row, outer.start.column);
    let o_end   = (outer.end.row,   outer.end.column);
    let i_start = (inner.start.row, inner.start.column);
    let i_end   = (inner.end.row,   inner.end.column);
    o_start <= i_start && i_end <= o_end
}

/// Build sig help for a known (class, dispatcher, handler_name). Walks
/// the current file's symbols AND every cached module — otherwise a
/// consumer file that emits against a producer-defined handler gets no
/// sig help, even though hover already walks cross-file (the two must
/// agree — same abstraction, same reach).
fn string_dispatch_signature_for(
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
    class: &str,
    dispatcher: &str,
    handler_name: &str,
    active_param: usize,
) -> Option<SignatureHelp> {
    let mut signatures: Vec<SignatureInformation> = Vec::new();

    // Shared builder — used both for in-file and cross-file symbol walks
    // so a handler's sig is formatted identically regardless of where
    // it lives.
    let push_sig = |signatures: &mut Vec<SignatureInformation>,
                    sym: &crate::model::file_analysis::Symbol,
                    provenance: Option<&str>| {
        let SymbolDetail::Handler { owner, dispatchers, params, .. } = &sym.detail else { return };
        let HandlerOwner::Class(n) = owner else { return };
        if n != class { return; }
        let dispatcher_ok = dispatchers.is_empty()
            || dispatchers.iter().any(|d| d == dispatcher);
        if !dispatcher_ok || params.is_empty() { return; }

        let display: Vec<&ParamInfo> = params
            .iter()
            .filter(|p| !p.is_invocant)
            .collect();
        let labels: Vec<String> = display.iter()
            .map(|p| match &p.default {
                Some(d) => format!("{} = {}", p.name, d),
                None => p.name.clone(),
            })
            .collect();
        let parameters: Vec<ParameterInformation> = labels.iter()
            .map(|l| ParameterInformation {
                label: ParameterLabel::Simple(l.clone()),
                documentation: None,
            })
            .collect();
        let doc = match provenance {
            Some(p) => format!(
                "{} handler on `{}`, registered at {} line {}",
                handler_name, class, p, sym.selection_span.start.row + 1,
            ),
            None => format!(
                "{} handler on `{}`, registered at line {}",
                handler_name, class, sym.selection_span.start.row + 1,
            ),
        };
        signatures.push(SignatureInformation {
            label: format!("{}('{}', {})", dispatcher, handler_name, labels.join(", ")),
            documentation: Some(Documentation::String(doc)),
            parameters: Some(parameters),
            active_parameter: None,
        });
    };

    for sym in analysis.symbols() {
        if sym.name != handler_name { continue; }
        push_sig(&mut signatures, sym, None);
    }
    if let Some(idx) = module_index {
        for module_name in idx.modules_with_symbol(handler_name) {
            // Every file registered under the name — stacked registrations
            // may live in a losing candidate.
            for cached in idx.visible_def_candidates(&module_name) {
                let whole = idx.whole_present(&cached);
                for sym in whole.symbols() {
                    if sym.name != handler_name { continue; }
                    push_sig(&mut signatures, sym, Some(module_name.as_str()));
                }
            }
        }
    }

    if signatures.is_empty() { return None; }
    Some(SignatureHelp {
        signatures,
        active_signature: Some(0),
        active_parameter: Some(active_param.saturating_sub(1) as u32),
    })
}

/// Resolve the class of an invocant expression at a given cursor point.
///   * `$self` / `__PACKAGE__`  → enclosing package at that position
///   * bare `Pkg::Name`         → the literal class
///   * `$var`                   → looked up via `analysis.inferred_type`
/// Returns `None` when the expression doesn't resolve to a known class.
/// Text-driven entry point: resolve invocant → class, then delegate to
/// `string_dispatch_signature_for`. Used for mid-editing states where
/// no DispatchCall ref exists yet.
fn string_dispatch_signature(
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
    invocant: Option<&str>,
    dispatcher: &str,
    handler_name: &str,
    active_param: usize,
    point: Point,
) -> Option<SignatureHelp> {
    let class = analysis.invocant_text_to_class(invocant, point)?;
    string_dispatch_signature_for(analysis, module_index, &class, dispatcher, handler_name, active_param)
}

pub fn signature_help(
    analysis: &FileAnalysis,
    tree: &Tree,
    text: &str,
    pos: Position,
    module_index: &ModuleIndex,
) -> Option<SignatureHelp> {
    let point = position_to_point(pos);

    // Plugin query hook — runs BEFORE native sig help. Plugin can
    // show a custom sig (arrayref-wrapped handler args) OR silently
    // claim the slot to suppress native sig (cursor in an options
    // hash of a dispatcher — native would mis-show the task sig).
    let mut skip_string_dispatch = false;
    if let Some(qctx) = cursor_context::build_plugin_query_context(analysis, tree, text.as_bytes(), point) {
        let registry = crate::build::plugin::default_plugin_registry();
        let (uses, parents) = analysis.trigger_view_at(point);
        let query = crate::build::plugin::TriggerQuery {
            package_uses: &uses,
            package_parents: &parents,
        };
        for p in registry.applicable(&query) {
            match p.on_signature_help(&qctx) {
                Some(crate::build::plugin::PluginSigHelpAnswer::Show(psig)) => {
                    return Some(plugin_sig_to_lsp(psig));
                }
                Some(crate::build::plugin::PluginSigHelpAnswer::Silent) => {
                    return None;
                }
                Some(crate::build::plugin::PluginSigHelpAnswer::ShowHandler {
                    owner_class, dispatcher, handler_name, active_param,
                }) => {
                    // Core-side Handler lookup — same machinery the
                    // native DispatchCall path uses, just triggered by
                    // plugin instead of ref. `active_param` is a
                    // displayed index; `string_dispatch_signature_for`
                    // applies the +1 offset to match its internal
                    // convention (params[0] is invocant, stripped).
                    if let Some(sig) = string_dispatch_signature_for(
                        analysis, Some(module_index),
                        &owner_class, &dispatcher, &handler_name,
                        active_param + 1,
                    ) {
                        return Some(sig);
                    }
                    // Plugin claimed the slot but no Handler was found
                    // — suppress native to avoid fallthrough mis-fires.
                    return None;
                }
                Some(crate::build::plugin::PluginSigHelpAnswer::ShowCallSig) => {
                    // Plugin recognizes this call but the cursor isn't
                    // in its args slot. Skip the native string-dispatch
                    // fallback (which would key the task's sig off the
                    // OUTER call's positional count) and fall through
                    // to the method's OWN signature.
                    skip_string_dispatch = true;
                    break;
                }
                None => {}
            }
        }
    }

    // Step 1: the enclosing call's ArgPosition slot (`docs/adr/cursor-slots.md`)
    // — only the VERDICT routes through `detect_slot`'s call-slot entry;
    // sig-help's own machinery (below) is unchanged.
    let Slot::ArgPosition { callee: Some(call_ctx), .. } =
        crate::lsp::cursor_slot::detect_call_slot(tree, text.as_bytes(), point)?.slot
    else {
        return None;
    };

    // Step 1a: string-dispatch specialization. `$x->emit('ready', CURSOR)`
    // is a method call whose string arg routes to a registered handler;
    // when handler_params are on record for that (class, event_name) pair,
    // surface them instead of the emit() method's own generic signature.
    // Stacked defs (multiple `->on('ready', sub {...})` wire-ups) each
    // contribute one `SignatureInformation` so users see every handler
    // shape they might be dispatching to.
    // Arrayref-wrapped handler args (Minion's `enqueue(task, [@args])`)
    // are handled by the plugin's `on_signature_help` IoC hook earlier
    // in this function — the hook sees the Array container + active
    // slot and returns the task sig. No core-side arrayref branching.

    if !skip_string_dispatch && call_ctx.is_method && call_ctx.active_param >= 1 {
        // Primary path: find the DispatchCall ref the plugin already
        // emitted for this call site. Its `target_name`, `owner`, and
        // `dispatcher` were all computed with the builder's full
        // knowledge — including const-folding `$dynamic` back to
        // `'connect'` — so sig help inherits folding for free and
        // can't drift from what hover shows.
        if let Some((handler_name, owner_class, dispatcher)) =
            dispatch_info_for_enclosing_call(analysis, tree, text.as_bytes(), point)
        {
            if let Some(sig) = string_dispatch_signature_for(
                analysis,
                Some(module_index),
                &owner_class,
                &dispatcher,
                &handler_name,
                call_ctx.active_param,
            ) {
                return Some(sig);
            }
        }

        // Fallback: no DispatchCall ref at this call site (e.g. no plugin
        // declared the method as a dispatcher). Try the text-level
        // first-arg string; this covers mid-editing states where a ref
        // hasn't been emitted yet.
        if let Some(ref name) = call_ctx.first_arg_string {
            if let Some(sig) = string_dispatch_signature(
                analysis,
                Some(module_index),
                call_ctx.invocant.as_deref(),
                &call_ctx.name,
                name,
                call_ctx.active_param,
                point,
            ) {
                return Some(sig);
            }
        }
    }

    // Step 2: file_analysis resolves the sub signature (local + cross-file)
    if let Some(sig_info) = analysis.signature_for_call(
        &call_ctx.name,
        call_ctx.is_method,
        call_ctx.invocant.as_deref(),
        point,
        Some(module_index),
    ) {
        // Build param labels with inferred types
        let param_labels: Vec<String> = sig_info
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let base = if let Some(ref default) = p.default {
                    format!("{} = {}", p.name, default)
                } else {
                    p.name.clone()
                };
                // Skip the invocant — its type is obvious. Flag first (covers
                // plugin-marked invocants like a helper's `$c`); name
                // convention as backstop for pre-flag cache blobs.
                if p.is_invocant
                    || crate::model::conventions::is_conventional_invocant_name(&p.name)
                {
                    return base;
                }
                // Cross-file: use pre-resolved param types
                if let Some(ref types) = sig_info.param_types {
                    if let Some(Some(ref type_tag)) = types.get(i) {
                        return format!("{}: {}", base, type_tag);
                    }
                    return base;
                }
                // Local: look up inferred type at end of sub body —
                // route through the witness bag so framework + branch
                // + arity rules refine the answer.
                if let Some(ty) = analysis.inferred_type_via_bag(&p.name, sig_info.body_end) {
                    format!("{}: {}", base, format_inferred_type(&ty))
                } else {
                    base
                }
            })
            .collect();

        let params: Vec<ParameterInformation> = param_labels
            .iter()
            .map(|label| ParameterInformation {
                label: ParameterLabel::Simple(label.clone()),
                documentation: None,
            })
            .collect();

        let label = format!("{}({})", sig_info.name, param_labels.join(", "));

        return Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation: None,
                parameters: Some(params),
                active_parameter: Some(call_ctx.active_param as u32),
            }],
            active_signature: Some(0),
            active_parameter: Some(call_ctx.active_param as u32),
        });
    }

    None
}
