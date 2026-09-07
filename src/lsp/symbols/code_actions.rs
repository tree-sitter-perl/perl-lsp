//! Code actions (auto-import) and pack member-operator diagnostics.

use super::*;

// ---- Code actions ----

/// Find the position to insert a new `use` statement, scoped to the package at `point`.
/// Uses line-range approach: finds which package range the cursor is in,
/// then inserts after the last `use` in that range.
/// `stable_packages` provides fallback package lines from the stable outline
/// when the current parse lost packages due to error recovery.
pub(super) fn find_use_insertion_position(
    analysis: &FileAnalysis,
    point: Point,
    stable_packages: Option<&[(String, usize)]>,
) -> Position {
    // Collect package declaration lines from current parse
    let mut pkg_lines: Vec<usize> = analysis.symbols().iter()
        .filter(|s| matches!(s.kind, FaSymKind::Package | FaSymKind::Class))
        .map(|s| s.selection_span.start.row)
        .collect();

    // If the stable outline has MORE packages than the current parse,
    // merge them in — the parse lost some due to error recovery.
    if let Some(stable) = stable_packages {
        if stable.len() > pkg_lines.len() {
            for (_, line) in stable {
                if !pkg_lines.contains(line) {
                    pkg_lines.push(*line);
                }
            }
        }
    }
    pkg_lines.sort();

    // Find the package range containing `point`
    let pkg_start = pkg_lines.iter().rev()
        .find(|&&line| line <= point.row)
        .copied()
        .unwrap_or(0);
    let pkg_end = pkg_lines.iter()
        .find(|&&line| line > point.row)
        .copied()
        .unwrap_or(usize::MAX);

    // Find the last import within this package's line range
    let last_import = analysis.imports.iter().rev().find(|imp| {
        imp.span.start.row >= pkg_start && imp.span.start.row < pkg_end
    });

    if let Some(imp) = last_import {
        Position {
            line: imp.span.end.row as u32 + 1,
            character: 0,
        }
    } else {
        // No imports in this package range — insert after the package statement
        Position {
            line: pkg_start as u32 + 1,
            character: 0,
        }
    }
}

/// Diagnostic code for a member-access whose operator disagrees with the
/// receiver's pointer depth (`p.member` on a `Box* p`). The fix is a
/// single-token swap; `code_actions` reads `data.operator` for the
/// replacement text and `range` for where to write it.
const MEMBER_OP_CODE: &str = "member-access-operator";

/// Mode B: the operator-mismatch diagnostics. One WARNING per
/// `member_op_mismatches()` entry, each self-describing (range = the operator
/// token, `data.operator` = the correct token) so the quick-fix needs no
/// re-analysis. Language-agnostic: Perl's `MethodCall` refs carry no
/// `member_op` (one operator), so the query is empty by construction — no gate.
pub fn pack_member_op_diagnostics(analysis: &FileAnalysis) -> Vec<Diagnostic> {
    analysis
        .member_op_mismatches()
        .into_iter()
        .map(|m| Diagnostic {
            range: span_to_range(m.op_span),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String(MEMBER_OP_CODE.into())),
            source: Some("perl-lsp".into()),
            message: format!(
                "use `{}` here — the receiver's type requires it (you wrote `{}`)",
                m.expected.as_str(),
                m.typed.as_str(),
            ),
            data: Some(serde_json::json!({ "operator": m.expected.as_str() })),
            ..Default::default()
        })
        .collect()
}

/// Diagnostic code for a member-access whose receiver is too deeply indirected
/// for a single `.`/`->` — the fix is an expression wrap (`(*pp)->m`), not a
/// swap, so this carries NO `data.operator` and offers no quick-fix (show-only,
/// mirroring Mode A's stance for the ambiguous case).
const MEMBER_OP_PEEL_CODE: &str = "member-access-peel";

/// Mode B (peel half): the DEEP-receiver hints. One WARNING per
/// `member_op_deep_accesses()` entry — the case a token swap can't express
/// (`OP** op_p; op_p->m` needs `(*op_p)->m`). Range = the written operator; the
/// message names the peeled receiver spelling. No auto-fix.
pub fn pack_member_op_peel_diagnostics(analysis: &FileAnalysis) -> Vec<Diagnostic> {
    analysis
        .member_op_deep_accesses()
        .into_iter()
        .map(|p| Diagnostic {
            range: span_to_range(p.op_span),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String(MEMBER_OP_PEEL_CODE.into())),
            source: Some("perl-lsp".into()),
            message: format!(
                "receiver is {}-level indirect — a single `.`/`->` can't reach its members; dereference first: `{}->`",
                p.depth, p.wrap,
            ),
            ..Default::default()
        })
        .collect()
}

/// Mode C: use-after-move. One WARNING per `use_after_move_reads()` read —
/// a variable read after a `std::move` of it, before any reassignment. The
/// region + cutoff + honesty gates live on `FileAnalysis` (the edge-driven
/// moved-from window, gates B/C/E); this is the thin LSP projection. Opt-in
/// via `DiagnosticOptions.use_after_move`.
pub fn pack_use_after_move_diagnostics(analysis: &FileAnalysis) -> Vec<Diagnostic> {
    analysis
        .use_after_move_reads()
        .into_iter()
        .map(|(name, span)| Diagnostic {
            range: span_to_range(span),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("use-after-move".into())),
            source: Some("perl-lsp".into()),
            message: format!("use of `{name}` after `std::move` (moved-from state)"),
            ..Default::default()
        })
        .collect()
}

/// Every pack-language (non-Perl) diagnostic for an analysis, concatenated.
/// One seam so a backend dispatch never enumerates the individual checks.
pub fn pack_diagnostics(
    analysis: &FileAnalysis,
    lookup: Option<&dyn crate::model::file_analysis::CrossFileLookup>,
    index_settled: bool,
    options: DiagnosticOptions,
) -> Vec<Diagnostic> {
    let mut diags = pack_member_op_diagnostics(analysis);
    diags.extend(pack_member_op_peel_diagnostics(analysis));
    diags.extend(super::diagnostics::pack_symbol_diagnostics(analysis, lookup, index_settled));
    // use-after-move is OPT-IN (`DiagnosticOptions.use_after_move`): the wired
    // check is the decidable subset only — gates B/C/E on `use_after_move_reads`
    // keep it to straight-line, in-function, local moves, verified to emit ZERO
    // false positives over the spdlog/fmt/onednn headers. The path-sensitive
    // residuals (cross-branch use, loop-carried move, by-ref reset, subobject
    // move) stay OUT by design, not flagged. `docs/adr/use-after-move.md`.
    if options.use_after_move {
        diags.extend(pack_use_after_move_diagnostics(analysis));
    }
    diags
}

pub fn code_actions(
    diagnostics: &[Diagnostic],
    analysis: &FileAnalysis,
    text: &str,
    uri: &Url,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();

    for diag in diagnostics {
        // A missing return type: the inferred spelling after the parameter list.
        if matches!(&diag.code, Some(NumberOrString::String(s)) if s == "missing-return-type") {
            if let Some(action) = make_return_type_action(analysis, text, uri, diag) {
                actions.push(action);
            }
            continue;
        }
        // Unimplemented contracts: one edit declaring every missing method.
        if matches!(&diag.code, Some(NumberOrString::String(s)) if s == "unimplemented-method") {
            if let Some(action) = make_implement_contracts_action(analysis, text, uri, diag) {
                actions.push(action);
            }
            continue;
        }
        // Member-access operator swap: replace the operator token (the
        // diagnostic's range) with the correct one (`data.operator`).
        if matches!(&diag.code, Some(NumberOrString::String(s)) if s == MEMBER_OP_CODE) {
            if let Some(op) = diag.data.as_ref().and_then(|d| d.get("operator")).and_then(|v| v.as_str()) {
                let mut changes = HashMap::new();
                changes.insert(uri.clone(), vec![TextEdit { range: diag.range, new_text: op.to_string() }]);
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Change to `{}`", op),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diag.clone()]),
                    edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
                    is_preferred: Some(true),
                    ..Default::default()
                }));
            }
            continue;
        }
        // D2 guard-insertion quick-fix.
        if matches!(&diag.code, Some(NumberOrString::String(s)) if s == "optional-deref") {
            if let Some(action) = make_optional_guard_action(uri, diag) {
                actions.push(action);
            }
            continue;
        }

        // An undefined type the workspace declares elsewhere: one import per
        // declaring namespace, in the pack's own import syntax.
        if matches!(&diag.code, Some(NumberOrString::String(s)) if s == "undefined-type") {
            actions.extend(make_import_type_actions(analysis, uri, diag));
            continue;
        }
        // An unused import whose row binds only that name: delete the row.
        if matches!(&diag.code, Some(NumberOrString::String(s)) if s == "unused-import") {
            if let Some(action) = make_remove_import_action(uri, diag) {
                actions.push(action);
            }
            continue;
        }
        let code_matches = matches!(
            &diag.code,
            Some(NumberOrString::String(s)) if s == "unresolved-function"
        );
        if !code_matches {
            continue;
        }
        let data = match &diag.data {
            Some(d) => d,
            None => continue,
        };
        let func_name = match data.get("function").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => continue,
        };

        // Case 1: Already-imported module — add function to existing qw() list
        if let Some(module_name) = data.get("module").and_then(|v| v.as_str()) {
            if let Some(action) =
                make_add_to_qw_action(analysis, uri, diag, module_name, func_name)
            {
                actions.push(action);
            }
            continue;
        }

        // Case 2: New import — add `use Module qw(func);` statement
        if let Some(modules) = data.get("modules").and_then(|v| v.as_array()) {
            let diag_point = position_to_point(diag.range.start);
            let mut insert_pos = find_use_insertion_position(analysis, diag_point, None);
            // If position is after the diagnostic, fall back to nearest import/package above
            if insert_pos.line > diag.range.start.line {
                let last_import_above = analysis.imports.iter().rev()
                    .find(|imp| imp.span.start.row < diag_point.row);
                if let Some(imp) = last_import_above {
                    insert_pos = Position { line: imp.span.end.row as u32 + 1, character: 0 };
                } else {
                    let last_pkg_above = analysis.symbols().iter().rev()
                        .find(|s| matches!(s.kind, FaSymKind::Package | FaSymKind::Class) && s.selection_span.start.row < diag_point.row);
                    if let Some(pkg) = last_pkg_above {
                        insert_pos = Position { line: pkg.selection_span.start.row as u32 + 1, character: 0 };
                    }
                }
            }
            for (i, module_val) in modules.iter().enumerate() {
                if let Some(module_name) = module_val.as_str() {
                    let new_text = format!("use {} qw({});\n", module_name, func_name);
                    let edit = TextEdit {
                        range: Range {
                            start: insert_pos,
                            end: insert_pos,
                        },
                        new_text,
                    };
                    let mut changes = HashMap::new();
                    changes.insert(uri.clone(), vec![edit]);

                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Add 'use {} qw({})'", module_name, func_name),
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diag.clone()]),
                        edit: Some(WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        }),
                        is_preferred: Some(i == 0 && modules.len() == 1),
                        ..Default::default()
                    }));
                }
            }
        }
    }

    actions
}

/// Delete the whole import row the diagnostic names (`data.row` = its
/// first and last line), newline included.
fn make_remove_import_action(uri: &Url, diag: &Diagnostic) -> Option<CodeActionOrCommand> {
    let row = diag.data.as_ref()?.get("row")?.as_array()?;
    let (first, last) = (row.first()?.as_u64()? as u32, row.get(1)?.as_u64()? as u32);
    let edit = TextEdit {
        range: Range {
            start: Position { line: first, character: 0 },
            end: Position { line: last + 1, character: 0 },
        },
        new_text: String::new(),
    };
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![edit]);
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Remove unused import".to_string(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
        is_preferred: Some(true),
        ..Default::default()
    }))
}

/// `Add 'use Ns\Leaf;'` for each candidate the diagnostic carries, inserted
/// after the last import row above the site, else after the namespace
/// declaration above it (a blank line between), else after the first line.
fn make_import_type_actions(analysis: &FileAnalysis, uri: &Url, diag: &Diagnostic) -> Vec<CodeActionOrCommand> {
    let Some(candidates) = diag.data.as_ref().and_then(|d| d.get("candidates")).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let point = position_to_point(diag.range.start);
    candidates
        .iter()
        .filter_map(|c| c.as_str())
        .filter_map(|fq| analysis.import_edit_for(fq, point.row).map(|e| (fq, e)))
        .enumerate()
        .map(|(i, (_fq, (at, text)))| {
            let pos = point_to_position(at);
            let mut changes = HashMap::new();
            changes.insert(uri.clone(), vec![TextEdit { range: Range { start: pos, end: pos }, new_text: text.clone() }]);
            CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Add '{}'", text.trim()),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
                is_preferred: Some(i == 0 && candidates.len() == 1),
                ..Default::default()
            })
        })
        .collect()
}

/// D2 quick-fix: insert `return unless defined $r;` on its own line just
/// before the flagged dereference. Indented to the receiver's column (the
/// diagnostic range start), which is exact for a statement-leading deref and
/// harmless otherwise. Produces precisely the guard the narrower then
/// consumes to strip the `Optional`.
fn make_optional_guard_action(uri: &Url, diag: &Diagnostic) -> Option<CodeActionOrCommand> {
    let receiver = diag.data.as_ref()?.get("receiver")?.as_str()?;
    let indent = " ".repeat(diag.range.start.character as usize);
    let insert_pos = Position { line: diag.range.start.line, character: 0 };
    let edit = TextEdit {
        range: Range { start: insert_pos, end: insert_pos },
        new_text: format!("{}return unless defined {};\n", indent, receiver),
    };
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![edit]);
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Guard: return unless defined {}", receiver),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        is_preferred: Some(true),
        ..Default::default()
    }))
}

/// Generate a code action that adds a function to an existing `qw()` import list.
fn make_add_to_qw_action(
    analysis: &FileAnalysis,
    uri: &Url,
    diag: &Diagnostic,
    module_name: &str,
    func_name: &str,
) -> Option<CodeActionOrCommand> {
    let import = analysis
        .imports
        .iter()
        .find(|imp| imp.module_name == module_name)?;
    let close_pos = import.qw_close_paren?;
    let insert_pos = point_to_position(close_pos);
    let edit = TextEdit {
        range: Range {
            start: insert_pos,
            end: insert_pos,
        },
        new_text: format!(" {}", func_name),
    };
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![edit]);
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Import '{}' from {}", func_name, module_name),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        is_preferred: Some(true),
        ..Default::default()
    }))
}

/// "Implement missing methods": one stub per unfulfilled contract, the
/// declarator copied from the contract's own declaration (its types kept —
/// a return type must stay covariant, so it is never dropped) under the
/// pack's `contract_stub` template, inserted before the class body's
/// closing brace.
fn make_implement_contracts_action(
    analysis: &FileAnalysis,
    text: &str,
    uri: &Url,
    diag: &Diagnostic,
) -> Option<CodeActionOrCommand> {
    let template = analysis.pack.contract_stub.as_str();
    if template.is_empty() {
        return None;
    }
    let data = diag.data.as_ref()?;
    let class = data.get("class")?.as_str()?;
    let contracts = data.get("contracts")?.as_array()?;
    // The class symbol's span ends just past the body's closing brace.
    let end = analysis
        .symbols()
        .iter()
        .find(|s| s.kind == FaSymKind::Class && s.name == class)?
        .span
        .end;
    let brace = Position { line: end.row as u32, character: end.column.saturating_sub(1) as u32 };
    let mut stubs = Vec::new();
    for c in contracts {
        let role = c.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let name = c.get("name").and_then(|v| v.as_str())?;
        let sig = match c.get("sig").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                let sym = analysis.symbols().iter().find(|s| {
                    matches!(s.kind, FaSymKind::Sub | FaSymKind::Method)
                        && s.name == name
                        && s.package.as_deref() == Some(role)
                })?;
                declarator_text(text, sym)?
            }
        };
        let body = template.replace("{}", &sig);
        stubs.push(
            body.lines()
                .map(|l| if l.is_empty() { String::new() } else { format!("    {l}") })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    let lead = if brace.character > 0 { "\n" } else { "" };
    let new_text = format!("{lead}{}\n", stubs.join("\n\n"));
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![TextEdit { range: Range { start: brace, end: brace }, new_text }]);
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: if contracts.len() == 1 {
            "Implement missing method".to_string()
        } else {
            format!("Implement {} missing methods", contracts.len())
        },
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
        is_preferred: Some(true),
        ..Default::default()
    }))
}

/// A callable's declarator as written — from its name token to the end of
/// its declaration, minus the terminator, whitespace collapsed.
pub fn declarator_text(src: &str, sym: &crate::model::file_analysis::Symbol) -> Option<String> {
    let start = crate::build::cursor_sentinel::point_to_byte(src, sym.selection_span.start);
    let end = crate::build::cursor_sentinel::point_to_byte(src, sym.span.end);
    let raw = src.get(start..end)?;
    let t = raw.trim_end().trim_end_matches(';').trim_end();
    if t.is_empty() {
        return None;
    }
    Some(t.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// "Add return type": the pack's return-annotation template with the
/// diagnostic's spelling, inserted right after the parameter list's closing
/// parenthesis (found from the name token with the quote-aware scan a
/// signature render uses — a `)` inside a default value is not the close).
fn make_return_type_action(
    analysis: &FileAnalysis,
    text: &str,
    uri: &Url,
    diag: &Diagnostic,
) -> Option<CodeActionOrCommand> {
    let template = analysis.pack.return_annotation_template.as_str();
    if template.is_empty() {
        return None;
    }
    let spelling = diag.data.as_ref()?.get("spelling")?.as_str()?;
    let name_end = crate::build::cursor_sentinel::point_to_byte(text, position_to_point(diag.range.end));
    let close = params_close_byte(text, name_end)?;
    let at = byte_to_position(text, close + 1);
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), vec![TextEdit { range: Range { start: at, end: at }, new_text: template.replace("{}", spelling) }]);
    Some(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Add return type `{}`", template.replace("{}", spelling).trim()),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
        is_preferred: Some(true),
        ..Default::default()
    }))
}

/// Byte offset of the `)` closing the first parenthesized group at or after
/// `from`; quotes hide a `)` inside a default value.
fn params_close_byte(text: &str, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut opened = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (off, ch) in text.get(from..)?.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => {
                depth += 1;
                opened = true;
            }
            ')' => {
                depth -= 1;
                if opened && depth == 0 {
                    return Some(from + off);
                }
            }
            '{' | ';' if !opened => return None,
            _ => {}
        }
    }
    None
}

fn byte_to_position(text: &str, byte: usize) -> Position {
    let (mut line, mut col) = (0u32, 0u32);
    for (i, ch) in text.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
    }
    Position { line, character: col }
}
