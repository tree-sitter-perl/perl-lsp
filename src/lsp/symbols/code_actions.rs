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
    uri: &Url,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();

    for diag in diagnostics {
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
