//! documentLink: the NON-SYMBOL clickable ranges goto-def cannot reach by
//! construction — POD `L<...>` links, bare URLs in comments/POD, and
//! string-path loads (`require "some/path.pl"`, `use lib 'lib'`). Module
//! names in `use`/`require`/`use parent`/`with` are deliberately EXCLUDED:
//! goto-def already resolves those identifiers (verified empirically; a
//! link there would duplicate an existing verb as an underline).
//!
//! Client-polled verb (editors request links on every open/change), so the
//! pass is a single text scan plus registered-only lookups: module targets
//! resolve through `module_path_cached`/`visible_def_candidates` (map
//! reads) and file targets through existence checks — it never kicks an
//! @INC resolve. An unresolvable target yields NO link (a link to the
//! wrong file is worse than no link, because the user clicks it).

use super::*;
use std::path::{Path, PathBuf};

/// One resolved link: the clickable span and its target.
pub struct DocLink {
    pub span: crate::model::file_analysis::Span,
    pub target: LinkTarget,
}

pub enum LinkTarget {
    /// An absolute URL (POD `L<https://...>`, a comment URL).
    Web(String),
    /// A local file that EXISTS (or a registered module's file).
    File(PathBuf),
}

impl LinkTarget {
    pub fn to_url(&self) -> Option<Url> {
        match self {
            LinkTarget::Web(s) => Url::parse(s).ok(),
            LinkTarget::File(p) => Url::from_file_path(p).ok(),
        }
    }
    pub fn display(&self) -> String {
        match self {
            LinkTarget::Web(s) => s.clone(),
            LinkTarget::File(p) => p.display().to_string(),
        }
    }
}

/// Scan a document's text for link ranges. `self_dir` (the file's own
/// directory) and `root` anchor relative path existence checks; `idx`
/// resolves POD module links against the already-registered universe only.
pub fn document_links(
    text: &str,
    self_dir: Option<&Path>,
    root: Option<&Path>,
    idx: Option<&dyn crate::model::file_analysis::CrossFileLookup>,
) -> Vec<DocLink> {
    let mut out = Vec::new();
    let mut in_pod = false;
    for (row, line) in text.lines().enumerate() {
        // POD block tracking (perlpod: a command paragraph starts with `=`
        // at column 0; `=cut` returns to code).
        if let Some(rest) = line.strip_prefix('=') {
            if rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
                in_pod = !rest.starts_with("cut");
            }
        }
        if in_pod {
            let claimed_from = out.len();
            pod_links(line, row, idx, &mut out);
            // Bare URLs in prose — minus any range an `L<...>` above already
            // claims (an `L<url>` would otherwise link twice).
            let mut urls = Vec::new();
            url_links(line, 0, row, &mut urls);
            let claimed = out[claimed_from..]
                .iter()
                .map(|l| (l.span.start.column, l.span.end.column))
                .collect::<Vec<_>>();
            urls.retain(|u| {
                !claimed
                    .iter()
                    .any(|(s, e)| u.span.start.column < *e && *s < u.span.end.column)
            });
            out.extend(urls);
        } else {
            // Comments: URLs after a `#`. Code before the `#` is scanned
            // only for the string-path loads below — never for URLs (a URL
            // inside a code string is the author's data, not navigation).
            if let Some(hash) = line.find('#') {
                url_links(&line[hash..], hash, row, &mut out);
            }
            path_load_links(line, row, self_dir, root, &mut out);
        }
    }
    out
}

/// `L<...>` occurrences in one POD line. Handles `L<text|target>`,
/// `L<Module>`, `L<Module/"sec">`, and `L<url>`; `L<< ... >>` and
/// section-only `L</sec>` links are skipped (no file target to offer).
fn pod_links(
    line: &str,
    row: usize,
    idx: Option<&dyn crate::model::file_analysis::CrossFileLookup>,
    out: &mut Vec<DocLink>,
) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(pos) = line[i..].find("L<") {
        let start = i + pos;
        // `L<<` (padded form) — rare, and matching its ` >>` terminator is
        // its own grammar; skip rather than mis-span.
        if bytes.get(start + 2) == Some(&b'<') {
            i = start + 3;
            continue;
        }
        let Some(close_rel) = line[start + 2..].find('>') else { break };
        let inner = &line[start + 2..start + 2 + close_rel];
        i = start + 2 + close_rel + 1;
        // `L<text|target>` — the target half is what resolves.
        let target = inner.rsplit('|').next().unwrap_or(inner);
        // Strip a `/section` tail; a section-only link has no file target.
        let module = target.split('/').next().unwrap_or(target).trim();
        let span = crate::model::file_analysis::Span {
            start: tree_sitter::Point::new(row, start),
            end: tree_sitter::Point::new(row, i),
        };
        if target.starts_with("http://") || target.starts_with("https://") {
            out.push(DocLink { span, target: LinkTarget::Web(target.to_string()) });
        } else if !module.is_empty()
            && module
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        {
            if let Some(path) = registered_module_path(module, idx) {
                out.push(DocLink { span, target: LinkTarget::File(path) });
            }
            // Unknown module: no link (honest miss; it appears once the
            // module is registered — never a guessed target).
        }
    }
}

/// The registered-only module→file lookup: cached name-keyed path first,
/// then any def candidate declaring the package. Pure map reads.
fn registered_module_path(
    module: &str,
    idx: Option<&dyn crate::model::file_analysis::CrossFileLookup>,
) -> Option<PathBuf> {
    let idx = idx?;
    if let Some(p) = idx.module_path_cached(module) {
        return Some(p.into_path_buf());
    }
    idx.visible_def_candidates(module)
        .into_iter()
        .next()
        .map(|c| c.path.to_path_buf())
}

/// Bare `http(s)://` URLs in `segment` (which starts at byte `col_base` of
/// the line). Trailing prose punctuation is trimmed from the span.
fn url_links(segment: &str, col_base: usize, row: usize, out: &mut Vec<DocLink>) {
    let mut i = 0;
    while let Some(pos) = segment[i..].find("http") {
        let start = i + pos;
        let rest = &segment[start..];
        let scheme_len = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            i = start + 4;
            continue;
        };
        let tail = &rest[scheme_len..];
        let mut end = rest.len();
        if let Some(stop) = tail.find(|c: char| c.is_whitespace() || c == '>' || c == '"' || c == '\'') {
            end = scheme_len + stop;
        }
        let url = rest[..end].trim_end_matches(['.', ',', ';', ')', ']']);
        if url.len() > scheme_len {
            out.push(DocLink {
                span: crate::model::file_analysis::Span {
                    start: tree_sitter::Point::new(row, col_base + start),
                    end: tree_sitter::Point::new(row, col_base + start + url.len()),
                },
                target: LinkTarget::Web(url.to_string()),
            });
        }
        i = start + end.max(scheme_len);
    }
}

/// String-path loads: `require "some/path.pl";` and `use lib 'lib';`
/// arguments. Interpolated strings (`"$FindBin::Bin/../lib"`) are skipped —
/// their value isn't knowable statically, and a guessed link is worse than
/// none. A candidate links only when it EXISTS relative to the file's own
/// directory or the workspace root (require's true resolution is @INC at
/// runtime; existence against these two anchors is the honest static
/// approximation — miss, don't guess).
fn path_load_links(
    line: &str,
    row: usize,
    self_dir: Option<&Path>,
    root: Option<&Path>,
    out: &mut Vec<DocLink>,
) {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let args: &str = if let Some(rest) = trimmed.strip_prefix("require") {
        if !rest.starts_with([' ', '\t', '"', '\'']) {
            return;
        }
        rest
    } else if let Some(rest) = trimmed.strip_prefix("use") {
        let rest2 = rest.trim_start();
        let Some(libargs) = rest2.strip_prefix("lib") else { return };
        if !libargs.starts_with([' ', '\t', '"', '\'', '(']) {
            return;
        }
        libargs
    } else {
        return;
    };
    let args_base = indent + (trimmed.len() - args.len());
    // Every quoted string on the rest of the line is a candidate.
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let q = bytes[i];
        if q == b'"' || q == b'\'' {
            if let Some(close) = args[i + 1..].find(q as char) {
                let inner = &args[i + 1..i + 1 + close];
                let interpolated = q == b'"' && inner.contains(['$', '@']);
                if !inner.is_empty() && !interpolated {
                    if let Some(path) = existing_relative(inner, self_dir, root) {
                        out.push(DocLink {
                            span: crate::model::file_analysis::Span {
                                start: tree_sitter::Point::new(row, args_base + i + 1),
                                end: tree_sitter::Point::new(row, args_base + i + 1 + close),
                            },
                            target: LinkTarget::File(path),
                        });
                    }
                }
                i += close + 2;
                continue;
            }
        }
        i += 1;
    }
}

/// The first anchor under which `rel` exists — file dir, then root.
fn existing_relative(rel: &str, self_dir: Option<&Path>, root: Option<&Path>) -> Option<PathBuf> {
    for base in [self_dir, root].into_iter().flatten() {
        let joined = base.join(rel);
        if joined.exists() {
            return joined.canonicalize().ok().or(Some(joined));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<(usize, usize, String)> {
        document_links(text, None, None, None)
            .into_iter()
            .map(|l| (l.span.start.row, l.span.start.column, l.target.display()))
            .collect()
    }

    #[test]
    fn comment_url_links_code_string_url_does_not() {
        let text = "my $u = \"https://example.com/in-code\";\n# docs: https://example.com/doc.\n";
        let got = links(text);
        assert_eq!(got.len(), 1);
        // Trailing prose period trimmed.
        assert_eq!(got[0], (1, 8, "https://example.com/doc".to_string()));
    }

    #[test]
    fn pod_l_url_links_once_not_twice() {
        let text = "=head1 X\n\nsee L<https://example.com/spec> now\n\n=cut\n";
        let got = links(text);
        assert_eq!(got.len(), 1, "L<url> must not double-link via the bare-URL scan: {got:?}");
        assert_eq!(got[0].2, "https://example.com/spec");
    }

    #[test]
    fn pod_l_unknown_module_yields_no_link() {
        // No index → no registered universe → honest miss, never a guess.
        let text = "=head1 X\n\nL<No::Such::Module>\n\n=cut\n";
        assert!(links(text).is_empty());
    }

    #[test]
    fn interpolated_and_missing_paths_yield_no_link() {
        let text = "use lib \"$FindBin::Bin/../lib\";\nrequire \"definitely/missing.pl\";\n";
        assert!(links(text).is_empty());
    }

    #[test]
    fn require_path_links_when_it_exists() {
        let dir = std::env::temp_dir().join("perl_lsp_links_test");
        let _ = std::fs::create_dir_all(dir.join("helpers"));
        std::fs::write(dir.join("helpers/util.pl"), "1;\n").unwrap();
        let text = "require \"helpers/util.pl\";\n";
        let got = document_links(text, None, Some(&dir), None);
        assert_eq!(got.len(), 1);
        assert!(got[0].target.display().ends_with("util.pl"));
        // Span covers exactly the quoted path text.
        assert_eq!(got[0].span.start.column, 9);
        assert_eq!(got[0].span.end.column, 9 + "helpers/util.pl".len());
    }
}
