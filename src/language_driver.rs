//! Multi-language serving seam — the `LanguageDriver` keystone
//! (`docs/prompt-multi-language.md`). One trait the server routes
//! through; Perl is the always-present reference driver (and the
//! gold-corpus regression net), pack languages are opt-in features.
//!
//! Distribution identity is a feature flag, not a repo split: a
//! `cpp-lsp` is this binary built `--features cpp`; the default Perl
//! build never links a C++ grammar. The crate stays single + lockstep
//! (the layering test is the seam) until a second driver makes a cargo
//! *workspace* earn its keep — see `docs/gold-roadmap.md`.

use crate::file_analysis::FileAnalysis;
use std::path::Path;

/// Everything the server needs to host one language: parse + analyze a
/// file to a `FileAnalysis`, claim its extensions, and resolve a
/// module name to candidate paths (cross-file).
pub trait LanguageDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn extensions(&self) -> &[&'static str];
    /// Source → `FileAnalysis`. The driver owns its parser (and any
    /// pre-parse transform, e.g. C++ macro expansion).
    fn analyze(&self, source: &str) -> FileAnalysis;
    /// Module name → workspace-relative candidate paths.
    fn module_paths(&self, module: &str) -> Vec<String>;
}

/// Perl — the reference driver. Wraps the production builder; behaviour
/// is exactly the current single-file analysis path.
pub struct PerlDriver;

impl LanguageDriver for PerlDriver {
    fn id(&self) -> &'static str {
        "perl"
    }
    fn extensions(&self) -> &[&'static str] {
        &["pm", "pl", "t"]
    }
    fn analyze(&self, source: &str) -> FileAnalysis {
        let mut parser = crate::builder::create_parser();
        match parser.parse(source, None) {
            Some(tree) => crate::builder::build(&tree, source.as_bytes()),
            None => FileAnalysis::new(Default::default()),
        }
    }
    fn module_paths(&self, module: &str) -> Vec<String> {
        vec![format!("{}.pm", module.replace("::", "/"))]
    }
}

/// C++ — the first pack driver. Full pipeline: macro reparse (validated
/// expansion) → query-pack extraction → real `FileAnalysis`. Gated by
/// the `cpp` feature so the default build never links the grammar.
#[cfg(feature = "cpp")]
pub struct CppDriver;

#[cfg(feature = "cpp")]
impl CppDriver {
    fn parser() -> tree_sitter::Parser {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&tree_sitter_cpp::LANGUAGE.into()).expect("cpp grammar");
        p
    }
}

#[cfg(feature = "cpp")]
impl LanguageDriver for CppDriver {
    fn id(&self) -> &'static str {
        "cpp"
    }
    fn extensions(&self) -> &[&'static str] {
        &["cpp", "cc", "cxx", "hpp", "hh", "h"]
    }
    fn analyze(&self, source: &str) -> FileAnalysis {
        let mut parser = Self::parser();
        // reparse past the preprocessor (declarator-position macros),
        // then extract through the language-agnostic driver.
        let (expanded, _anchors) = crate::cpp_reparse::preprocess_validated(&mut parser, source);
        let Some(tree) = parser.parse(&expanded, None) else { return FileAnalysis::new(Default::default()) };
        match crate::query_extract::extract(&tree, expanded.as_bytes(), &crate::query_extract::cpp_pack()) {
            Ok(skel) => skel.into_file_analysis(),
            Err(_) => FileAnalysis::new(Default::default()),
        }
    }
    fn module_paths(&self, module: &str) -> Vec<String> {
        let p = module.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
        vec![p.to_string()]
    }
}

/// The drivers this binary was compiled to serve. Perl always; pack
/// languages per feature.
pub struct LanguageRegistry {
    drivers: Vec<Box<dyn LanguageDriver>>,
}

impl LanguageRegistry {
    pub fn with_enabled() -> Self {
        let mut drivers: Vec<Box<dyn LanguageDriver>> = vec![Box::new(PerlDriver)];
        #[cfg(feature = "cpp")]
        drivers.push(Box::new(CppDriver));
        LanguageRegistry { drivers }
    }

    pub fn for_path(&self, path: &Path) -> Option<&dyn LanguageDriver> {
        let ext = path.extension()?.to_str()?;
        self.drivers.iter().find(|d| d.extensions().contains(&ext)).map(|d| d.as_ref())
    }

    pub fn for_id(&self, id: &str) -> Option<&dyn LanguageDriver> {
        self.drivers.iter().find(|d| d.id() == id).map(|d| d.as_ref())
    }

    /// Configured language ids — what this distribution serves.
    pub fn languages(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|d| d.id()).collect()
    }
}

#[cfg(test)]
#[path = "language_driver_tests.rs"]
mod tests;
