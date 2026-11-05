//! CLI coordinate dialects and cursor-input parsing: `CoordFmt`,
//! `--at` specs, and the `[pos]` self-documenting annotation.

use super::*;

/// Which coordinate dialect the CLI renders location output in. Threaded from
/// input parsing into every location a query emits so **output speaks the same
/// dialect as the input** — the fix for the 0-based-vs-1-based foot-gun. The
/// tool's own `path:line:col` output then round-trips straight back into the
/// next query's `--at`.
///
/// - `ZeroBasedByte` — tree-sitter native: 0-based line, byte column. The
///   dialect of the positional `<file> <line> <col>` form (and the batch/gold
///   protocol's JSONL input).
/// - `EditorOneBasedChar` — editor convention: 1-based line, character column.
///   The dialect of the `--at file:line:col` form, and what the `--batch`
///   path renders (gold fixtures encode these values — do NOT change it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CoordFmt {
    ZeroBasedByte,
    EditorOneBasedChar,
}

impl CoordFmt {
    /// Render a tree-sitter `(row, byte_col)` in this dialect. `line_src` is the
    /// full text of `row` (when available) — needed only to convert byte→char
    /// for the editor dialect; rows past EOF fall back to the byte column.
    fn render(self, row: usize, byte_col: usize, line_src: Option<&str>) -> (usize, usize) {
        match self {
            CoordFmt::ZeroBasedByte => (row, byte_col),
            CoordFmt::EditorOneBasedChar => {
                let char_col = line_src
                    .map(|line| line.get(..byte_col.min(line.len())).unwrap_or(line).chars().count())
                    .unwrap_or(byte_col);
                (row + 1, char_col + 1)
            }
        }
    }

    /// Render an LSP `Position` (already 0-based **character**-counted) in this
    /// dialect — no byte→char step. Used for the handful of sites that hand back
    /// an lsp `Location`/`Range` (cpp `#include` goto-def) instead of a raw span.
    pub(super) fn render_pos(self, row0: usize, char0: usize) -> (usize, usize) {
        match self {
            CoordFmt::ZeroBasedByte => (row0, char0),
            CoordFmt::EditorOneBasedChar => (row0 + 1, char0 + 1),
        }
    }
}

/// Encode one rename edit's span as JSON in the caller's coordinate dialect.
/// `sources` supplies per-file text for the byte→char step (editor dialect).
pub(super) fn span_to_json(
    sources: &mut SourceCache,
    path: &str,
    span: file_analysis::Span,
    text: String,
) -> serde_json::Value {
    let (line, col) = sources.display(path, span.start.row, span.start.column);
    let (end_line, end_col) = sources.display(path, span.end.row, span.end.column);
    serde_json::json!({
        "line": line, "col": col,
        "end_line": end_line, "end_col": end_col,
        "new_text": text
    })
}

/// Per-file source cache for coordinate rendering — references can fan out
/// across many files; read each at most once. Carries the `CoordFmt` so every
/// `display` call renders in one dialect. Misses (unreadable file) degrade to
/// the raw byte column via `CoordFmt::render`'s fallback.
pub(super) struct SourceCache {
    fmt: CoordFmt,
    files: std::collections::HashMap<String, Option<String>>,
}

impl SourceCache {
    pub(super) fn new(fmt: CoordFmt) -> Self {
        SourceCache { fmt, files: std::collections::HashMap::new() }
    }

    pub(super) fn display(&mut self, path: &str, row: usize, byte_col: usize) -> (usize, usize) {
        let src = self
            .files
            .entry(path.to_string())
            .or_insert_with(|| crate::util::text::read_source(path).ok());
        let line_src = src.as_deref().and_then(|s| s.lines().nth(row));
        self.fmt.render(row, byte_col, line_src)
    }
}

// ---- Cursor-input parsing (positional vs `--at`) ----

/// A parsed cursor target for a single-mode CLI query: the file, the internal
/// tree-sitter point (always 0-based / byte column), the `CoordFmt` matching
/// the input dialect (so output round-trips), and the raw spelling the user
/// typed (for the `[pos]` self-documenting annotation).
pub(super) struct CursorTarget {
    pub(super) file: String,
    pub(super) point: tree_sitter::Point,
    pub(super) fmt: CoordFmt,
    raw: String,
}

/// Split a `--at` spec `file:line:col` into its parts. The two rightmost
/// `:`-fields are line and col; everything before is the file (paths rarely
/// contain `:`, and taking the last two fields tolerates the ones that do,
/// e.g. a Windows drive prefix). Returns `(file, line_1based, col_1based)`.
fn split_at_spec(spec: &str) -> Option<(String, usize, usize)> {
    let mut it = spec.rsplitn(3, ':');
    let col: usize = it.next()?.parse().ok()?;
    let line: usize = it.next()?.parse().ok()?;
    let file = it.next()?.to_string();
    if file.is_empty() {
        return None;
    }
    Some((file, line, col))
}

/// Convert an editor `(line_1based, char_col_1based)` to an internal 0-based
/// tree-sitter point with a **byte** column — the exact inverse of
/// `CoordFmt::EditorOneBasedChar` rendering. `source` is the target file's text
/// (for the char→byte step); without it, the char column is used as the byte
/// column (best effort, correct for ASCII).
fn editor_to_internal_point(source: Option<&str>, line1: usize, col1: usize) -> tree_sitter::Point {
    let row = line1.saturating_sub(1);
    let char_col = col1.saturating_sub(1);
    let byte_col = source
        .and_then(|s| s.lines().nth(row))
        .map(|line| {
            line.char_indices()
                .nth(char_col)
                .map(|(b, _)| b)
                .unwrap_or(line.len())
        })
        .unwrap_or(char_col);
    tree_sitter::Point::new(row, byte_col)
}

/// Resolve a cursor-verb file argument to a path that exists on disk.
/// Tries the argument as-is first (CWD-relative or absolute), then falls
/// back to `<root>`-relative — so a root-relative path works when invoked
/// from outside the project root. When neither exists, the original is
/// returned unchanged (downstream reports the honest "file not found").
fn resolve_cursor_file(file: &str, root: &str) -> String {
    if std::path::Path::new(file).exists() {
        return file.to_string();
    }
    let joined = std::path::Path::new(root).join(file);
    if joined.exists() {
        return joined.to_string_lossy().into_owned();
    }
    file.to_string()
}

/// Parse the cursor arguments that follow `<root>` for a single-mode query.
/// Two forms, disambiguated by the leading `--at`:
///   positional:  `<file> <line> <col>`      → 0-based, byte column (engine)
///   editor:      `--at <file>:<line>:<col>` → 1-based, char column (editor)
/// The chosen `CoordFmt` rides along so the query's output renders in the same
/// dialect the input used. The file argument is resolved CWD-first then
/// `<root>`-relative via `resolve_cursor_file`.
pub(super) fn parse_cursor_target(rest: &[String], root: &str) -> Option<CursorTarget> {
    match rest {
        [flag, spec] if flag == "--at" => {
            let (file, line1, col1) = split_at_spec(spec)?;
            let file = resolve_cursor_file(&file, root);
            let source = crate::util::text::read_source(&file).ok();
            let point = editor_to_internal_point(source.as_deref(), line1, col1);
            Some(CursorTarget { file, point, fmt: CoordFmt::EditorOneBasedChar, raw: spec.clone() })
        }
        [file, line, col] => {
            let row: usize = line.parse().ok()?;
            let column: usize = col.parse().ok()?;
            Some(CursorTarget {
                file: resolve_cursor_file(file, root),
                point: tree_sitter::Point::new(row, column),
                fmt: CoordFmt::ZeroBasedByte,
                raw: format!("{} {} {}", file, line, col),
            })
        }
        _ => None,
    }
}

/// The maximal identifier token containing (or ending just before) `byte_col`
/// on `line`. Word = alphanumeric or `_`; a cursor sitting one past a token's
/// end (a common editor placement) still reports that token. `None` means the
/// cursor landed on whitespace / punctuation — the loud-hint case.
fn token_at_byte(line: &str, byte_col: usize) -> Option<String> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let word_span = |anchor: usize| -> Option<(usize, usize)> {
        let idx = chars
            .iter()
            .position(|&(b, c)| b <= anchor && anchor < b + c.len_utf8())?;
        if !is_word(chars[idx].1) {
            return None;
        }
        let mut lo = idx;
        while lo > 0 && is_word(chars[lo - 1].1) {
            lo -= 1;
        }
        let mut hi = idx;
        while hi + 1 < chars.len() && is_word(chars[hi + 1].1) {
            hi += 1;
        }
        Some((chars[lo].0, chars[hi].0 + chars[hi].1.len_utf8()))
    };
    let (s, e) = word_span(byte_col)
        .or_else(|| byte_col.checked_sub(1).and_then(word_span))?;
    Some(line[s..e].to_string())
}

/// Self-documenting `[pos]` annotation (stderr — stdout stays the stable
/// machine format). Prints exactly how the cursor input was interpreted, the
/// internal 0-based point, the landed token, and the source line — so the
/// 0-based/1-based trap announces itself instead of silently mislanding.
pub(super) fn emit_pos_annotation(target: &CursorTarget) {
    let (label, dialect) = match target.fmt {
        CoordFmt::EditorOneBasedChar => ("EDITOR", "1-based, char col"),
        CoordFmt::ZeroBasedByte => ("POSITIONAL", "0-based, byte col"),
    };
    let row = target.point.row;
    let bc = target.point.column;
    eprintln!(
        "[pos] input {}  read as {} ({})  ->  internal {}:{}",
        target.raw, label, dialect, row, bc
    );
    // Distinguish "couldn't open the file" from "line past EOF": the old
    // code collapsed both into a "past the end" message, which lied about
    // files it never read (unresolved path, permissions).
    match crate::util::text::read_source(&target.file) {
        Err(e) => eprintln!("      (could not read {}: {})", target.file, e),
        Ok(text) => match text.lines().nth(row) {
            Some(line) => {
                match token_at_byte(line, bc) {
                    Some(tok) => eprintln!("      landed on token: {:?}", tok),
                    None => {
                        // Whitespace / no token — name the likely fix in the OTHER base.
                        let hint = match target.fmt {
                            CoordFmt::EditorOneBasedChar => format!(
                                "if these are 0-based engine coords, drop --at and pass: {} {} {}",
                                target.file, row, bc
                            ),
                            CoordFmt::ZeroBasedByte => format!(
                                "if these are 1-based editor coords, use: --at {}:{}:{}",
                                target.file, row + 1, bc + 1
                            ),
                        };
                        eprintln!("      landed on whitespace / no token — {}", hint);
                    }
                }
                eprintln!("      line {}: {}", row + 1, line);
            }
            None => eprintln!("      (line {} is past the end of {})", row + 1, target.file),
        },
    }
}

#[cfg(test)]
#[path = "positions_tests.rs"]
mod coord_tests;
