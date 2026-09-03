//! Semantic tokens and inlay hints.

use super::*;

// ---- Semantic tokens ----

// Token type/modifier indices are defined in file_analysis.rs (TOK_*, MOD_*).

pub fn semantic_token_types() -> Vec<SemanticTokenType> {
    vec![
        SemanticTokenType::VARIABLE,       // 0: variables
        SemanticTokenType::PARAMETER,      // 1: sub parameters
        SemanticTokenType::FUNCTION,       // 2: function calls
        SemanticTokenType::METHOD,         // 3: method calls
        SemanticTokenType::MACRO,          // 4: framework DSL keywords
        SemanticTokenType::PROPERTY,       // 5: hash keys
        SemanticTokenType::NAMESPACE,      // 6: package/class names
        // No REGEXP: the TextMate `string.regexp` scope (with escape-sequence
        // highlighting) is left to shine through — see #63.
        SemanticTokenType::ENUM_MEMBER,    // 7: constants
        SemanticTokenType::KEYWORD,        // 8: $self/$class
    ]
}

pub fn semantic_token_modifiers() -> Vec<SemanticTokenModifier> {
    vec![
        SemanticTokenModifier::DECLARATION,      // 0
        SemanticTokenModifier::READONLY,         // 1
        SemanticTokenModifier::MODIFICATION,     // 2
        SemanticTokenModifier::DEFAULT_LIBRARY,  // 3
        SemanticTokenModifier::DEPRECATED,       // 4
        SemanticTokenModifier::STATIC,           // 5
        SemanticTokenModifier::new("scalar"),    // 6
        SemanticTokenModifier::new("array"),     // 7
        SemanticTokenModifier::new("hash"),      // 8
    ]
}

/// Returns inlay hints for the given range.
///
/// Shows type annotations for variable declarations with non-obvious inferred types,
/// and return type annotations for sub/method declarations.
pub fn inlay_hints(analysis: &FileAnalysis, range: Range) -> Vec<InlayHint> {
    let start = position_to_point(range.start);
    let end = position_to_point(range.end);
    let mut hints = Vec::new();

    for sym in analysis.symbols() {
        let decl_point = sym.selection_span.end;
        // Skip symbols outside the requested range
        if decl_point.row < start.row || decl_point.row > end.row {
            continue;
        }

        match sym.kind {
            FaSymKind::Variable => {
                // Skip conventional invocants — always the enclosing class.
                if crate::model::conventions::is_conventional_invocant_name(&sym.name) {
                    continue;
                }
                // Skip variables whose type is written EXPLICITLY (`int c`,
                // `Box b`) — the hint just echoes the source. Languages with
                // explicit types mark the declaration with a `skeleton-annot`
                // witness; inferred ones (`auto`, Perl) have none, so they
                // still get the hint.
                if analysis.witnesses.has_builder_source(
                    &crate::model::witnesses::WitnessAttachment::Variable {
                        name: sym.name.clone(),
                        scope: sym.scope,
                    },
                    crate::model::witnesses::ANNOT_SOURCE,
                ) {
                    continue;
                }
                if let Some(ty) = analysis.inferred_type_via_bag(&sym.name, sym.span.start) {
                    // Only show Object/HashRef/ArrayRef/CodeRef/Regexp — not Numeric/String
                    if matches!(ty, InferredType::Numeric | InferredType::String) {
                        continue;
                    }
                    hints.push(InlayHint {
                        position: point_to_position(decl_point),
                        label: InlayHintLabel::String(format!(": {}", analysis.display_type_of(sym, &ty))),
                        kind: Some(InlayHintKind::TYPE),
                        text_edits: None,
                        tooltip: None,
                        padding_left: Some(true),
                        padding_right: None,
                        data: None,
                    });
                }
            }
            FaSymKind::Sub | FaSymKind::Method => {
                // Plugin-synthesized subs/methods often have
                // return_type set to internal proxy classes (Mojo
                // helpers' `_Helper::users` chain, DBIC ResultSet
                // wrappers, etc.). The kind icon + hover already
                // carry the useful info; the inlay hint just
                // repeats a long dotted class name at every
                // declaration. Suppress it for framework symbols.
                if sym.namespace.is_framework() {
                    continue;
                }
                if matches!(sym.detail, SymbolDetail::Sub { .. }) {
                    if let Some(rt) = analysis.symbol_return_type_via_bag(sym.id, None) {
                        // Only show non-trivial return types
                        if matches!(rt, InferredType::Numeric | InferredType::String) {
                            continue;
                        }
                        hints.push(InlayHint {
                            position: point_to_position(decl_point),
                            label: InlayHintLabel::String(format!(
                                "→ {}",
                                analysis.render_type(&rt)
                            )),
                            kind: Some(InlayHintKind::TYPE),
                            text_edits: None,
                            tooltip: None,
                            padding_left: Some(true),
                            padding_right: None,
                            data: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    hints
}

pub fn semantic_tokens(analysis: &FileAnalysis) -> Vec<SemanticToken> {
    let tokens = analysis.semantic_tokens();

    let mut result = Vec::new();
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;

    for t in &tokens {
        let line = t.span.start.row as u32;
        let start = t.span.start.column as u32;
        let length = if t.span.start.row == t.span.end.row {
            (t.span.end.column as u32).saturating_sub(start).max(1)
        } else {
            1
        };

        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            start.saturating_sub(prev_start)
        } else {
            start
        };

        result.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: t.token_type,
            token_modifiers_bitset: t.modifiers,
        });

        prev_line = line;
        prev_start = start;
    }

    result
}

/// Inlay hints for a pack document: the type hints above plus a
/// `name:` before every positional argument of a call in `range` whose
/// callee resolves — the pack's own call shapes find the sites, the
/// signature-help ladder names the parameters. Positional matching stops
/// at a named argument or a spread; a variadic parameter covers the rest;
/// an argument that IS the same-named variable already reads as the name.
pub fn pack_inlay_hints(
    analysis: &FileAnalysis,
    tree: &Tree,
    text: &str,
    range: Range,
    language: &str,
    module_index: &dyn CrossFileLookup,
) -> Vec<InlayHint> {
    let mut hints = inlay_hints(analysis, range);
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
    let Some(pack) = reg.for_id(language).and_then(|d| d.lang_pack()) else {
        return hints;
    };
    let rows = range.start.line as usize..=range.end.line as usize;
    for call in crate::build::cursor_sentinel::calls_in_rows(tree, &pack, text, rows) {
        if call.args.is_empty() {
            continue;
        }
        let Some((_, params, _, _)) =
            pack_callee_signature(analysis, text, call.callee.start, module_index)
        else {
            continue;
        };
        let names: Vec<(String, bool)> = params.iter().filter_map(|p| param_name(p)).collect();
        if names.len() != params.len() {
            continue; // a parameter the renderer could not name: no guessing
        }
        for (i, arg) in call.args.iter().enumerate() {
            if arg.named || arg.spread {
                break;
            }
            let Some((name, _)) = names.get(i).or_else(|| names.last().filter(|(_, v)| *v)) else {
                break;
            };
            let shown = arg.text.trim();
            if shown == format!("${name}") || shown == name {
                continue;
            }
            hints.push(InlayHint {
                position: point_to_position(arg.span.start),
                label: InlayHintLabel::String(format!("{name}:")),
                kind: Some(InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
    }
    hints
}

/// `(name, variadic)` of one rendered parameter (`string $to`,
/// `int ...$rest`, `?Foo $x = null`, `const T& v`): the declarator token —
/// sigil-led where the language has one, else the last token of a
/// type-then-name pair. A lone type (`int`) names nothing.
fn param_name(p: &str) -> Option<(String, bool)> {
    let head = p.split('=').next().unwrap_or(p).trim();
    let toks: Vec<&str> = head.split_whitespace().collect();
    let tok = toks
        .iter()
        .rev()
        .find(|t| t.contains('$'))
        .copied()
        .or_else(|| if toks.len() >= 2 { toks.last().copied() } else { None })?;
    let variadic = tok.starts_with("...") || tok.contains("...");
    let name = tok
        .trim_start_matches("...")
        .trim_start_matches(['&', '*', '$'])
        .trim_end_matches("[]");
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((name.to_string(), variadic))
}
