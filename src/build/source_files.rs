//! A language's source files on disk: the directory walk every indexer and
//! dependency map shares, and the size-capped read-and-analyze every
//! indexing and dependency parse of a file on disk goes through. An open
//! editor buffer is analyzed straight from memory, uncapped.

use crate::build::language_driver::LanguageDriver;
use crate::model::file_analysis::FileAnalysis;
use std::path::{Path, PathBuf};

/// The size at or over which a pack-language file is left unindexed.
pub const PACK_FILE_CAP: u64 = 2_000_000;

/// What one walk takes.
pub struct Walk<'a> {
    /// The extensions that make a file the language's.
    pub extensions: &'a [&'a str],
    /// Files at or over this many bytes are left out, and listed.
    pub cap: u64,
    /// Directories the walk never enters: another tier owns them.
    pub skip: &'a [PathBuf],
    /// Honor `.gitignore` and the other standard filters. A workspace walk
    /// does; a walk inside a dependency directory must not, since projects
    /// routinely ignore the directory their dependencies install into.
    pub ignores: bool,
    /// Descend through symlinked directories. Search roots install through
    /// them (a versioned `@INC` directory linked under its short name); the
    /// workspace walk keeps a link from pulling a second tree into the project.
    pub follow_links: bool,
}

/// A walk's answer.
#[derive(Default)]
pub struct Walked {
    pub files: Vec<PathBuf>,
    /// The files `Walk::cap` left out, with their sizes.
    pub oversize: Vec<(PathBuf, u64)>,
}

/// Every file of the language under `root`, in walk order. A `root` that is
/// itself a file is the one file.
pub fn walk(root: &Path, w: &Walk) -> Walked {
    use ignore::types::TypesBuilder;
    use ignore::WalkBuilder;
    let mut out = Walked::default();
    if w.extensions.is_empty() {
        return out;
    }
    let mut tb = TypesBuilder::new();
    for ext in w.extensions {
        let _ = tb.add("lang", &format!("*.{ext}"));
    }
    let _ = tb.select("lang");
    let Ok(types) = tb.build() else { return out };
    let skip: Vec<PathBuf> = w.skip.to_vec();
    let entries = WalkBuilder::new(root)
        .standard_filters(w.ignores)
        .follow_links(w.follow_links)
        .types(types)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(move |e| !skip.iter().any(|r| e.path().starts_with(r)))
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()));
    for e in entries {
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        if len >= w.cap {
            out.oversize.push((e.into_path(), len));
        } else {
            out.files.push(e.into_path());
        }
    }
    out
}

/// Say what a walk's size cap left out, once per walk: a skipped file gets
/// no diagnostics from `--check` or the workspace index, and silence there
/// reads as "no problems found". The open-doc path has no cap — the editor
/// still analyzes these on didOpen.
pub fn report_oversize(oversize: &[(PathBuf, u64)], cap: u64) {
    if oversize.is_empty() {
        return;
    }
    let mut oversize = oversize.to_vec();
    oversize.sort();
    // eprintln, not log — the CLI runs without a logger.
    eprintln!(
        "perl-lsp: {} file(s) over the {} MB index cap were skipped \
         (no --check/workspace diagnostics for them; opening in an editor still works):",
        oversize.len(),
        cap as f64 / 1e6
    );
    for (p, len) in &oversize {
        eprintln!("perl-lsp:   {:.1} MB  {}", *len as f64 / 1e6, p.display());
    }
}

/// `path` read and analyzed in `driver`'s language. `None` when it cannot
/// be read, is at or over `cap` bytes, or its analysis panics; a panic is
/// reported with the file it hit, since one that doesn't name its file
/// costs a bisection.
pub fn analyze_file(driver: &dyn LanguageDriver, path: &Path, cap: u64) -> Option<FileAnalysis> {
    // Stat before reading: a generated gigabyte under `vendor/` is refused
    // without being read whole first.
    if std::fs::metadata(path).ok()?.len() >= cap {
        return None;
    }
    let source = std::fs::read_to_string(path).ok()?;
    if source.len() as u64 >= cap {
        return None;
    }
    let analyzed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| driver.analyze_with_path(&source, Some(path))));
    if analyzed.is_err() {
        eprintln!("perl-lsp: panic while analyzing {}; file skipped", path.display());
    }
    analyzed.ok()
}

#[cfg(test)]
#[path = "source_files_tests.rs"]
mod tests;
