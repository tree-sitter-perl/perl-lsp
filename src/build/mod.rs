//! Layer 2 — the builder. The only tree-sitter CST consumers live
//! here: `builder` produces `FileAnalysis`, everything else is a
//! sanctioned extraction seam (plugins, pod, query packs, reparse).

pub mod builder;
#[cfg_attr(not(feature = "php"), allow(dead_code))]
pub mod composer;
pub mod cpanfile;
// config-variant macro model: guard trail + reachability + join
pub mod cpp_macro_model;
// Compiled unconditionally (symbols.rs consumes the macro-model surface in
// every build); the driver registration is feature-gated, so a perl-only
// build leaves most of the module unreferenced — silence dead-code there
// while keeping the all-langs build strict.
#[cfg_attr(not(feature = "cpp"), allow(dead_code))]
pub mod cpp_reparse;
// zero-config toolchain probe: shell out to cc for stdlib
// include roots + predefined macros + resource dir (spike)
pub mod cpp_toolchain;
// sentinel re-parse for member-access cursor context
pub mod cursor_sentinel;
// multi-language serving seam (LanguageDriver keystone)
pub mod language_driver;
pub mod plugin;
pub mod pod;
pub mod query_cache;
#[cfg_attr(
    not(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake", feature = "php")),
    allow(dead_code)
)]
pub mod query_extract;
// Kept-as-spike: the Perl prototype reparenthesizer that proved the
// pre-extraction reparse seam (whose production form is cpp_reparse).
#[allow(dead_code)]
pub mod reparse;
