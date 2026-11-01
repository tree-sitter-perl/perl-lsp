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

use crate::model::file_analysis::{FileAnalysis, NameSpellings};
use std::path::Path;

/// Per-language behavioral capabilities — the properties core layers ask
/// instead of naming languages (rule #10). Each field is one behavior a
/// serving site genuinely tests; a driver declares the ones that hold for
/// its language. `Default` is every capability OFF/empty — the honest
/// answer for an id no driver claims (which also matches how the serving
/// paths treated an unrecognized language before the capabilities existed).
///
/// This is the CLOSED set of axes on which served languages may differ in
/// core; growing it should feel expensive — the first question on a new
/// flag is whether the difference should exist at all. The exhaustiveness
/// witness in `language_driver_tests.rs` destructures every field (no
/// `..`), so adding one is a compile error until every declared answer is
/// reviewed — a capability added-and-silently-defaulted is how a caps
/// struct decays back into the branching it replaced.
#[derive(Clone, Copy)]
pub struct DriverCaps {
    /// Analyses integrate with the Perl module hub's cross-file lane:
    /// import/export enrichment (`enrich_imported_types_with_keys`),
    /// build-time Surface freshness records (`Document::baseline_surface`),
    /// @INC open-doc residency (`mark_doc_open`), and the enrichment-derived
    /// diagnostics pipeline. Off = diagnostics read the analysis as-is and
    /// freshness is the pack invalidator's disk-side gate.
    pub hub_enrichment: bool,
    /// Completion / cursor-slot context derives from the LIVE document tree
    /// (`lsp/cursor_context`). Off = the sentinel-reparse pack path
    /// (`build/cursor_sentinel`) supplies member/identifier context.
    pub cursor_context: bool,
    /// `FileAnalysis::hover_info` renders hover for this language (the
    /// analysis-native renderer: POD docs, sigil semantics). Off = hover is
    /// the CandidateSet's language-agnostic projection (`pack_hover`).
    pub hover_info: bool,
    /// The signatureHelp verb is served (the cursor-context handler).
    pub signature_help: bool,
    /// The selectionRange verb is served (the tree-shape handler).
    pub selection_range: bool,
    /// A didChange rebuild is cheap enough to run synchronously on the
    /// message loop. Off = the document swaps tree/text immediately and
    /// DEBOUNCES the analysis rebuild (macro-heavy C: ~0.7s per rebuild).
    pub synchronous_rebuild: bool,
    /// Full-quality analysis needs a cross-file context gather beyond the
    /// file's own bytes (C++: `#include`d macro tables) — so an open built
    /// cached-only is degraded until the heal lane re-gathers, and index
    /// completion re-runs open docs through the gather.
    pub context_gather: bool,
    /// Disk-change lifecycle is owned by `PackInvalidator` (re-registration
    /// + include-closure consumer eviction). Off = the Perl direct re-index
    /// path handles watcher/save events.
    pub pack_invalidation: bool,
    /// A bare identifier can name a CROSS-FILE entity with no import
    /// binding (macro / enum constant / global reached through the include
    /// closure) — enables the raw-word goto-def/hover fallback lane outside
    /// the CandidateSet.
    pub cross_file_words: bool,
    /// See `LangPack::entrypoint_symbols` — symbols the runtime enters
    /// through the ABI, alive at zero fan-in by contract.
    pub entrypoint_symbols: &'static [&'static str],
    /// Files at or over this many bytes are never analyzed — the walk
    /// lists them and the dependency tier refuses them.
    pub file_cap: u64,
}

impl Default for DriverCaps {
    fn default() -> Self {
        DriverCaps {
            hub_enrichment: false,
            cursor_context: false,
            hover_info: false,
            signature_help: false,
            selection_range: false,
            synchronous_rebuild: false,
            context_gather: false,
            pack_invalidation: false,
            cross_file_words: false,
            entrypoint_symbols: &[],
            file_cap: crate::build::source_files::PACK_FILE_CAP,
        }
    }
}

/// Everything the server needs to host one language: parse + analyze a
/// file to a `FileAnalysis`, claim its extensions, and resolve a
/// module name to candidate paths (cross-file).
pub trait LanguageDriver: Send + Sync {
    fn id(&self) -> &'static str;
    /// How finished this language is. Required, with no default, so adding
    /// a driver forces the call rather than inheriting a flattering one.
    fn maturity(&self) -> Maturity;
    fn extensions(&self) -> &[&'static str];
    /// Exact filenames the driver claims (e.g. `CMakeLists.txt`), beyond
    /// extensions. Default none.
    fn filenames(&self) -> &[&'static str] {
        &[]
    }
    /// A fresh parser for this language — for the open `Document` to hold
    /// a tree (incremental edits, position handlers). NOTE: this parses
    /// the ORIGINAL source; `analyze` may run a pre-parse transform (C++
    /// macro expansion) internally, so the two trees can differ on
    /// macro-heavy files (the span-remap follow-up reconciles them).
    fn make_parser(&self) -> tree_sitter::Parser;
    /// Source → `FileAnalysis`.
    fn analyze(&self, source: &str) -> FileAnalysis;
    /// Build an analysis directly from the CALLER's already-parsed tree —
    /// only possible when the driver runs no pre-parse transform, so its
    /// analysis speaks the same tree/coordinates the caller holds (the
    /// native Perl builder). `None` = the driver analyzes independently
    /// (`analyze_with_path` may transform + reparse internally), so callers
    /// keep their tree for positions only.
    fn analyze_from_tree(&self, _tree: &tree_sitter::Tree, _source: &str) -> Option<FileAnalysis> {
        None
    }
    /// The behavioral capabilities this driver declares — see `DriverCaps`.
    /// Default: none (every serving site's generic arm).
    fn caps(&self) -> DriverCaps {
        DriverCaps::default()
    }
    /// Source + the file's path → `FileAnalysis`. The path lets a driver
    /// resolve cross-file context (C++ gathers `#define`s from `#include`d
    /// headers so namespace/export macros expand); every implementation
    /// also names it for the duration via `timings::current_file_scope`,
    /// so server-side open-doc builds attribute like indexed ones.
    fn analyze_with_path(&self, source: &str, path: Option<&Path>) -> FileAnalysis {
        let _file = crate::util::timings::current_file_scope(path);
        self.analyze(source)
    }
    /// Completion trigger characters for this language — the LSP
    /// `completionProvider` slot, so the client auto-fires completion (e.g.
    /// on `.`/`->`).
    fn trigger_chars(&self) -> &[&'static str];
    /// The language's `LangPack` — the ONE per-language config (grammar facts
    /// the query engine AND the cursor-completion path both read). `None` for
    /// the native Perl path (it uses `cursor_context`, not the pack). Lets a
    /// caller reach the pack through the single `for_id` lookup, no parallel
    /// `lang_cfg` registry.
    fn lang_pack(&self) -> Option<crate::build::query_extract::LangPack> {
        None
    }
    /// Fingerprint of the EXTERNAL inputs this driver's analyses depend on
    /// beyond the source files themselves (C++: the probed toolchain — its
    /// system include roots decide what a gather reaches). The persist tier
    /// keys on it so an analysis built under one input generation is never
    /// served under another. 0 = no external inputs.
    fn analysis_input_fingerprint(&self) -> u64 {
        0
    }
    /// How this language spells a module as a file under a search root.
    /// `None` = it names no module by path.
    fn module_spelling(&self) -> Option<ModuleSpelling> {
        self.lang_pack().map(|p| ModuleSpelling { names: p.names, module_paths: p.module_paths, module_of: p.module_of })
    }
    /// Where this project keeps the dependencies the workspace walk does
    /// not own (php: Composer's `vendor/`), and which of their files
    /// declares a name; `root` is `None` with no project open. `None` =
    /// the walk is the whole story.
    fn dependency_locator(&'static self, _root: Option<&Path>) -> Option<std::sync::Arc<dyn DependencyLocator>> {
        None
    }
    /// The names a file registers under, each with whether it declares a
    /// class there — what the workspace front door files it by and what a
    /// dependency file lands under. Default: every linkage-visible name.
    fn registration_names(&self, analysis: &FileAnalysis) -> Vec<(String, bool)> {
        analysis.linkage_feed()
    }
    /// The file names the watcher subscribes to so a package install
    /// moves the map: every name a locator's `inputs` spell, since an
    /// input no watcher reports moves a one-shot run's fingerprint and not
    /// the server's (`every_dependency_input_is_a_watched_manifest`).
    fn dependency_manifests(&self) -> &[&'static str] {
        &[]
    }
    /// The file whose presence gives a directory meaning to this
    /// language's analyses (python's `__init__.py`). The index records
    /// where a walk saw it (`walk_markers`) and the pack invalidator notes
    /// each one created or deleted. `None` = no such file.
    fn module_marker(&self) -> Option<&'static str> {
        None
    }
    /// Whether what `analysis` read about `path` from outside its source
    /// still holds — the module a path-named file is. A warm row that
    /// fails it re-analyzes.
    fn path_inputs_current(&self, _path: &Path, _analysis: &FileAnalysis) -> bool {
        true
    }
}

/// A language's module spelling: its names (whose use-map separator, if
/// any, splits an imported name from its module) and the files under a
/// search root a module name may be, most-preferred first.
#[derive(Clone)]
pub struct ModuleSpelling {
    pub names: NameSpellings,
    pub module_paths: fn(&str) -> Vec<String>,
    pub module_of: fn(&str) -> Option<String>,
}

/// The dependency tier's map for one project: a name nothing in the
/// workspace declares → the files that may declare it. The index asks it
/// on demand and parses only what it lands; the locator only probes the
/// filesystem. Implemented per language (php: `build::composer`).
pub trait DependencyLocator: Send + Sync {
    /// Files that may declare `identity`, most-preferred first.
    fn locate(&self, identity: &str) -> Vec<Located>;
    /// What the project declares it depends on apart from what its files
    /// use, asked like a use. Each is a name `locate` places.
    fn declared_demand(&self) -> &[Declared] {
        &[]
    }
    /// Directories whose files belong to this tier, never to the walk.
    fn roots(&self) -> &[std::path::PathBuf];
    /// The manifests this map was read from: a change to any is a new map.
    fn inputs(&self) -> &[std::path::PathBuf];
    /// The directories a path-named language names a located file from
    /// (python's `site-packages`), as it names a workspace file from the
    /// walk root.
    fn search_roots(&self) -> &[std::path::PathBuf] {
        &[]
    }
    /// Every identity this map can place, named without parsing: what
    /// completion offers before anything asks for a body. A directory the
    /// workspace sits in is never walked (`build::search_path::listable`).
    fn for_each_name(&self, _workspace: Option<&std::path::Path>, _visit: &mut dyn FnMut(&str)) {}
}

/// A dependency a project declares outside its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declared {
    /// The runtime loads it whatever the project names (Composer's
    /// `files`), so every session asks for it: nothing that calls into it
    /// is a use the demand would see.
    Loaded(String),
    /// The manifest lists it (a cpanfile's `requires`). What the project
    /// runs it also uses, so only a session that prefetches asks for it.
    Listed(String),
}

/// A file a dependency map places for a name, and how it got there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub path: std::path::PathBuf,
    pub by: LocatedBy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocatedBy {
    /// The path spells the name: it is that module's file, the one a
    /// `require` of the name loads, whatever it declares inside.
    Exact,
    /// The file of an enclosing module (`acme/__init__.py` for
    /// `acme.models.Base`), which may or may not declare the name.
    Enclosing,
}

/// How a language that names modules by path names one: the marker file
/// that makes a directory a package, and the namer that walks the marked
/// directories above a file.
#[cfg(feature = "pack-langs")]
#[derive(Clone, Copy)]
pub struct FileModuleRule {
    pub marker: &'static str,
    pub name: fn(&Path, &crate::build::walk_markers::MarkedDirs<'_>) -> Option<crate::build::query_extract::FileModule>,
}

/// Perl — the reference driver. Wraps the production builder; behaviour
/// is exactly the current single-file analysis path.
/// How finished a language's support is, declared by its driver and shown
/// by `--languages`. This is a PROMISE TO THE USER, not a capability: it
/// gates nothing, it tells someone whether to trust the answers. Kept off
/// `DriverCaps` for exactly that reason — caps are behaviour the engine
/// branches on, this is editorial.
///
/// The bar, so the label means something: `Stable` = the gold corpus
/// asserts the verb surface and regressions are caught; `Beta` = broad gold
/// coverage, known gaps documented; `Alpha` = it parses and answers, with
/// little or no net watching it — expect wrong answers.
// The pack drivers that construct `Beta` and `Alpha` are feature-gated, so a
// build without them sees only `Stable`.
#[cfg_attr(not(feature = "pack-langs"), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Maturity {
    Stable,
    Beta,
    Alpha,
}

impl Maturity {
    /// The parenthetical `--languages` appends. `Stable` is unmarked: the
    /// default expectation needs no annotation.
    pub fn suffix(self) -> &'static str {
        match self {
            Maturity::Stable => "",
            Maturity::Beta => " (beta)",
            Maturity::Alpha => " (alpha)",
        }
    }
}

pub struct PerlDriver;

impl LanguageDriver for PerlDriver {
    /// The plugin set: a `.rhai` edit changes what every analysis emits.
    fn analysis_input_fingerprint(&self) -> u64 {
        crate::build::plugin::rhai_host::plugin_fingerprint()
    }
    fn maturity(&self) -> Maturity {
        Maturity::Stable
    }
    fn id(&self) -> &'static str {
        crate::model::conventions::PERL_LANGUAGE
    }
    fn extensions(&self) -> &[&'static str] {
        &["pm", "pl", "t"]
    }
    fn make_parser(&self) -> tree_sitter::Parser {
        crate::build::builder::create_parser()
    }
    fn analyze(&self, source: &str) -> FileAnalysis {
        let mut parser = crate::build::builder::create_parser();
        match parser.parse(source, None) {
            Some(tree) => crate::build::builder::build(&tree, source.as_bytes()),
            None => FileAnalysis::new(Default::default()),
        }
    }
    fn analyze_from_tree(&self, tree: &tree_sitter::Tree, source: &str) -> Option<FileAnalysis> {
        Some(crate::build::builder::build(tree, source.as_bytes()))
    }
    fn caps(&self) -> DriverCaps {
        DriverCaps {
            hub_enrichment: true,
            cursor_context: true,
            hover_info: true,
            signature_help: true,
            selection_range: true,
            synchronous_rebuild: true,
            file_cap: 1_000_000,
            ..Default::default()
        }
    }
    fn trigger_chars(&self) -> &[&'static str] {
        // Sigils open variable completion; `>`/`:`/`{` open
        // method/pkg/hash-key slots; `(`/`,` are signature-help adjacent.
        &["$", "@", "%", ">", ":", "{", "(", ","]
    }
    fn module_spelling(&self) -> Option<ModuleSpelling> {
        Some(ModuleSpelling {
            names: crate::model::conventions::PERL_SPELLINGS,
            module_paths: crate::model::conventions::perl_module_paths,
            module_of: crate::model::conventions::perl_module_of,
        })
    }
    fn dependency_locator(&'static self, root: Option<&Path>) -> Option<std::sync::Arc<dyn DependencyLocator>> {
        Some(crate::build::inc::locator(root, self) as std::sync::Arc<dyn DependencyLocator>)
    }
    /// Perl subs link through their package, never by bare name: a file
    /// registers under the packages it declares.
    fn registration_names(&self, analysis: &FileAnalysis) -> Vec<(String, bool)> {
        analysis.package_names()
    }
    fn dependency_manifests(&self) -> &[&'static str] {
        &["cpanfile.snapshot"]
    }
}

/// A pack-language driver — the generic, query-driven path. One value
/// per language: a grammar, a `LangPack` (capture predicates), and an
/// optional pre-parse `transform` (C++ uses it for macro reparse;
/// others pass through). The whole multi-language story for a language
/// whose extraction is query-shaped is a `PackDriver { ... }` literal.
#[cfg(feature = "pack-langs")]
pub struct PackDriver {
    maturity: Maturity,
    exts: &'static [&'static str],
    /// Exact filenames this driver also claims (extensionless conventions
    /// like `CMakeLists.txt`). Matched before extension.
    filenames: &'static [&'static str],
    make_parser: fn() -> tree_sitter::Parser,
    pack: fn() -> crate::build::query_extract::LangPack,
    /// (source, external macros) → (transformed source, anchor map, recovered
    /// declarator macros), run before parsing (C++ macro expansion). The map
    /// remaps extracted spans back to ORIGINAL coordinates; the recovered
    /// `(class_name, macro_token)` pairs let the analyze path stamp the
    /// attribute-macro signal onto each recovered class. `None` = pass-through
    /// (identity, no recoveries).
    transform: Option<
        fn(
            &mut tree_sitter::Parser,
            &str,
            &crate::build::cpp_reparse::PreExpandedExternal,
        ) -> (String, crate::build::cpp_reparse::SpliceMap, Vec<(String, String)>),
    >,
    /// Path-aware cross-file macro gather (C++ #include resolution). Given
    /// the file path + source, returns the pre-expanded external macro table
    /// (mutually-expanded once, cached by include-set) that seeds `transform`.
    /// `None` = no cross-file macros.
    gather_macros: Option<
        fn(
            &Path,
            &str,
            &mut tree_sitter::Parser,
        ) -> std::sync::Arc<crate::build::cpp_reparse::PreExpandedExternal>,
    >,
    /// The macro identity/navigation lane collector (C preprocessor only):
    /// original source → every `#define` as a `MacroDef` (guard trail, def
    /// span, delegation callee). `None` for packs without a preprocessor.
    collect_macro_defs: Option<fn(&mut tree_sitter::Parser, &str) -> Vec<crate::model::file_analysis::MacroDef>>,
    /// Member-block macros as roles (C preprocessor only): classify a macro
    /// pasted standalone into a struct/class body, BLANK the use (so the base
    /// parses clean), and mint the synthetic base + parent edges. `None` for
    /// packs without a preprocessor.
    member_blocks: Option<fn(&mut tree_sitter::Parser, &str) -> crate::build::cpp_reparse::MemberBlockPlan>,
    /// Transitive `#include` closure (C preprocessor only): file path + source →
    /// canonical header paths this file reaches — the cross-file VISIBILITY key
    /// (`ScopedLookup` ranks `get_cached` candidates by it). `None` for packs
    /// with no include model.
    include_closure: Option<fn(&Path, &str) -> (Vec<String>, bool)>,
    /// External analysis-input fingerprint (see
    /// `LanguageDriver::analysis_input_fingerprint`). `None` = no external
    /// inputs (fingerprint 0).
    input_fingerprint: Option<fn() -> u64>,
    /// `public:`/`private:`/`protected:` region scan (C preprocessor-free
    /// languages only; `None` when the language has no access-specifier
    /// concept). Stamps a `non_public` attribute on member symbols so
    /// completion can filter by visibility.
    access_regions: Option<fn(&mut tree_sitter::Parser, &str) -> Vec<crate::build::cpp_reparse::AccessRegion>>,
    /// The module a file is, for a language that names modules by path
    /// (python). Asks the walk's marker snapshot, else the disk, so it
    /// runs in the gather phase.
    file_module: Option<FileModuleRule>,
    /// The project's dependency map (php: Composer). `None` = the workspace
    /// walk is the whole story.
    dependencies: Option<PackDependencies>,
}

/// A pack's dependency map: how to build it for a root, and the manifest
/// file names the watcher must report.
#[cfg(feature = "pack-langs")]
#[derive(Clone, Copy)]
struct PackDependencies {
    /// Builds the map for a root; given the driver it serves, so the map
    /// parses and spells names in that driver's language.
    locator: fn(&Path, &'static dyn LanguageDriver) -> std::sync::Arc<dyn DependencyLocator>,
    manifests: &'static [&'static str],
}

/// Pre-parse external state gathered in phase 1 (`gather_pack_context`) and
/// threaded through phases 2 and 5 (`transform_and_parse`, `enrich_skeleton`)
/// — see the phase list on `analyze_with_path`.
#[cfg(feature = "pack-langs")]
struct PackContext {
    external: std::sync::Arc<crate::build::cpp_reparse::PreExpandedExternal>,
    plan: Option<crate::build::cpp_reparse::MemberBlockPlan>,
    module: Option<crate::build::query_extract::FileModule>,
}

#[cfg(feature = "pack-langs")]
impl LanguageDriver for PackDriver {
    fn maturity(&self) -> Maturity {
        self.maturity
    }
    fn id(&self) -> &'static str {
        (self.pack)().id
    }
    fn extensions(&self) -> &[&'static str] {
        self.exts
    }
    fn filenames(&self) -> &[&'static str] {
        self.filenames
    }
    fn make_parser(&self) -> tree_sitter::Parser {
        (self.make_parser)()
    }
    fn analyze(&self, source: &str) -> FileAnalysis {
        self.analyze_with_path(source, None)
    }
    /// Source + path → `FileAnalysis` for a pack-driven language (C++/Python/
    /// R/CMake). Fixed pipeline, each phase consuming state the previous
    /// produced — the phase fns live in the `impl PackDriver` block below,
    /// in the same order:
    ///
    /// 1. `gather_pack_context` — pre-parse external inputs read from the
    ///    ORIGINAL source: the cross-file macro table (`gather_macros`) and
    ///    the member-block plan (blanks standalone macro-in-struct-body uses
    ///    so the base parses clean), plus the module a path-named file is
    ///    (`file_module`, which phase 3 files its classes under). Produces
    ///    `PackContext`.
    /// 2. `transform_and_parse` — the macro-expansion reparse (`transform`)
    ///    over the plan's blanked source (or the original, when there's no
    ///    plan), then the real tree-sitter parse of the transformed text.
    ///    MUST run after (1): consumes `ctx.external` + `ctx.plan`. `None`
    ///    means the transformed text failed to parse — caller serves a
    ///    degraded empty analysis without reaching extraction at all.
    /// 3. `query_extract::extract` — the query-driven skeleton extraction
    ///    over the transformed tree (unchanged; lives in `query_extract.rs`).
    /// 4. `remap_spans` — transformed → original coordinate remap over EVERY
    ///    span-bearing skeleton field (see its own doc for the exhaustive-
    ///    destructure enforcement). MUST run before (5): every later phase
    ///    that reads or writes skeleton spans assumes original coordinates.
    /// 5. `enrich_skeleton` — post-remap, pre-assembly skeleton enrichment:
    ///    external type-alias witnesses, member-block injection (synthetic
    ///    bases + parent edges), erased-macro-read minting, and the macro
    ///    identity/typing lane (`collect_macro_defs` + `macro_return_hints`).
    ///    MUST run after (4) (injected spans are original-coordinate) and
    ///    before (6) (`into_file_analysis` builds indices over everything).
    /// 6. `skel.into_file_analysis()` — the skeleton → FileAnalysis assembly
    ///    (a method on `SkeletonAnalysis`, not a phase fn here). It also reads
    ///    the scope facts the document stated: a callable's return arms and
    ///    the implicit receiver, ahead of the enrichment seal.
    /// 7. `register_post_build` — post-assembly hooks that stamp fields only
    ///    queryable once the FileAnalysis exists: macro defs, attribute-macro
    ///    signals, access-region visibility, include closure, degraded flag.
    ///
    /// Add a phase by inserting a numbered call here plus a fn beside the
    /// others below, in order — don't inline new logic into an existing
    /// phase's body.
    fn analyze_with_path(&self, source: &str, path: Option<&Path>) -> FileAnalysis {
        let _file = crate::util::timings::current_file_scope(path);
        self.analyze_pipeline(source, path)
    }
    fn lang_pack(&self) -> Option<crate::build::query_extract::LangPack> {
        Some((self.pack)())
    }
    fn caps(&self) -> DriverCaps {
        let pack = (self.pack)();
        DriverCaps {
            // Full quality needs a gather exactly when the driver reads
            // cross-file context at all (macro tables / include closure).
            // Packs without either (python/r/cmake) analyze context-free —
            // a cached-only open is already full quality, nothing to heal.
            context_gather: self.gather_macros.is_some() || self.include_closure.is_some(),
            pack_invalidation: true,
            cross_file_words: true,
            entrypoint_symbols: pack.entrypoint_symbols,
            ..Default::default()
        }
    }
    fn trigger_chars(&self) -> &[&'static str] {
        (self.pack)().trigger_chars
    }
    fn analysis_input_fingerprint(&self) -> u64 {
        self.input_fingerprint.map(|f| f()).unwrap_or(0)
    }
    fn dependency_locator(&'static self, root: Option<&Path>) -> Option<std::sync::Arc<dyn DependencyLocator>> {
        let root = root?;
        self.dependencies.map(|d| (d.locator)(root, self))
    }
    fn dependency_manifests(&self) -> &[&'static str] {
        self.dependencies.map(|d| d.manifests).unwrap_or(&[])
    }
    fn module_marker(&self) -> Option<&'static str> {
        self.file_module.map(|r| r.marker)
    }
    fn path_inputs_current(&self, path: &Path, analysis: &FileAnalysis) -> bool {
        self.file_module.is_none() || self.module_of(path) == analysis.pack.file_module
    }
}

#[cfg(feature = "pack-langs")]
impl PackDriver {
    /// The module `path` is, by the language's rule over the marked
    /// directories the walk saw (the disk, outside any walk).
    fn module_of(&self, path: &Path) -> Option<crate::build::query_extract::FileModule> {
        let rule = self.file_module?;
        crate::build::walk_markers::with_dirs(rule.marker, path, |dirs| (rule.name)(path, dirs))
    }

    /// The phase pipeline `analyze_with_path` documents; the trait method
    /// wraps it to stamp `FileAnalysis.language` on every exit.
    fn analyze_pipeline(&self, source: &str, path: Option<&Path>) -> FileAnalysis {
        let mut parser = (self.make_parser)();
        let ctx = self.gather_pack_context(&mut parser, source, path);
        let Some((tree, src, map, recovered)) = self.transform_and_parse(&mut parser, source, &ctx)
        else {
            return self.degraded_stand_in();
        };
        let pack = (self.pack)();
        match crate::build::query_extract::extract_in(&tree, src.as_bytes(), &pack, ctx.module.as_ref()) {
            Ok(mut skel) => {
                // remap extracted spans from transformed → original coords
                // (no-op for identity / pass-through languages).
                remap_spans(&mut skel, &src, source, &map);
                // Re-anchor members orphaned by a truncated container node, on
                // the now-original coordinates (the transform's macro expansion
                // unbalances braces; only the original source is trustworthy).
                if pack.brace_scoped_members {
                    skel.reanchor_truncated_containers(source);
                }
                let macro_defs = self.enrich_skeleton(&mut skel, &mut parser, source, &src, &map, &ctx);
                let mut fa = skel.into_file_analysis();
                self.register_post_build(&mut fa, &mut parser, source, path, &ctx, &recovered, macro_defs);
                fa
            }
            Err(e) => {
                // Fail LOUD: a cached empty analysis would be re-served
                // forever (the source file never changes).
                log::warn!(
                    "query extract failed for {:?}: {e:?} — serving an empty (non-cacheable) analysis",
                    path
                );
                self.degraded_stand_in()
            }
        }
    }

    /// The empty analysis a file that never reached extraction serves —
    /// still of its language (a failed cpp parse is a cpp file, and
    /// `resolve()` routes on that), and degraded so the persist tier can't
    /// freeze it.
    fn degraded_stand_in(&self) -> FileAnalysis {
        let mut fa = crate::build::query_extract::SkeletonAnalysis::for_pack(&(self.pack)()).into_file_analysis();
        fa.degraded = true;
        fa
    }
}

/// The pack analyze pipeline's phases (1/2/5/7 in `analyze_with_path`'s doc;
/// 3/4/6 are the free fns / `SkeletonAnalysis` method called between them).
/// Order is fixed and load-bearing — see that doc for the full contract.
#[cfg(feature = "pack-langs")]
impl PackDriver {
    /// Phase 1: pre-parse external context — the cross-file macro table and
    /// the member-block plan, both read from the ORIGINAL source only, and
    /// the module the file's path makes it.
    fn gather_pack_context(&self, parser: &mut tree_sitter::Parser, source: &str, path: Option<&Path>) -> PackContext {
        // Cross-file macros from #included headers (C++), so a macro
        // #defined elsewhere (SPDLOG_NAMESPACE_BEGIN) expands here.
        let external = match (self.gather_macros, path) {
            (Some(g), Some(p)) => crate::util::timings::phase("cpp.gather", || g(p, source, parser)),
            _ => std::sync::Arc::new(crate::build::cpp_reparse::PreExpandedExternal::empty()),
        };
        // Member-block macros as roles: BLANK the standalone-in-struct-body uses
        // so `struct op { BASEOP };` parses clean, and mint the synthetic base +
        // parent edges (injected in phase 5, into the extracted skeleton). The
        // blank is length-preserving, so the transform + remap stay in original
        // coordinates; the ORIGINAL source keeps the token (goto-def-on-`BASEOP`
        // untouched). `docs/adr/macro-handling.md`.
        let plan = self.member_blocks.map(|f| crate::util::timings::phase("cpp.member_blocks", || f(parser, source)));
        let module = path.and_then(|p| self.module_of(p));
        PackContext { external, plan, module }
    }

    /// Phase 2: the macro-expansion reparse over `ctx`'s blanked/original
    /// source, then the real tree-sitter parse of the transformed text.
    /// `None` means the transformed text failed to parse.
    #[allow(clippy::type_complexity)]
    fn transform_and_parse(
        &self,
        parser: &mut tree_sitter::Parser,
        source: &str,
        ctx: &PackContext,
    ) -> Option<(tree_sitter::Tree, String, crate::build::cpp_reparse::SpliceMap, Vec<(String, String)>)> {
        let parse_input: &str = ctx.plan.as_ref().map(|p| p.blanked_source.as_str()).unwrap_or(source);
        let (src, map, recovered) = match self.transform {
            Some(t) => crate::util::timings::phase("cpp.transform", || t(parser, parse_input, &ctx.external)),
            None => (parse_input.to_string(), crate::build::cpp_reparse::SpliceMap::default(), Vec::new()),
        };
        let tree = parser.parse(&src, None)?;
        Some((tree, src, map, recovered))
    }

    /// Phase 5: post-remap, pre-assembly skeleton enrichment. Returns the
    /// macro-identity lane (`MacroDef`s) for phase 7 to stamp onto the built
    /// `FileAnalysis` — the skeleton itself has no field for it.
    fn enrich_skeleton(
        &self,
        skel: &mut crate::build::query_extract::SkeletonAnalysis,
        parser: &mut tree_sitter::Parser,
        source: &str,
        src: &str,
        map: &crate::build::cpp_reparse::SpliceMap,
        ctx: &PackContext,
    ) -> Vec<crate::model::file_analysis::MacroDef> {
        // Type-alias `#define`s gathered from the include closure ride into
        // this file's bag as `TypeName` witnesses (span-less, so post-remap is
        // fine): the cross-file chase can't index a gitignored generated
        // header (`config.h`'s `U16TYPE`), but the gather reached it — so
        // carry the alias here.
        emit_external_type_aliases(&mut skel.witnesses, &ctx.external, (self.pack)().annot_type);
        // Member-block roles: inject the synthetic bases + parent edges
        // (original coords) into the skeleton, so the ONE ancestor walk
        // resolves `o->op_type` / hover / the references splat. Must run
        // AFTER remap (the injected spans are already original) and BEFORE
        // `into_file_analysis` (it builds indices over everything).
        if let Some(plan) = &ctx.plan {
            inject_member_blocks(skel, plan, (self.pack)().annot_type);
        }
        // Expanded / blanked macro USES vanish from the parsed text, so no
        // query capture can ref them — re-mint each as a variable read at its
        // ORIGINAL span (the splice map, the member-block blank diff, AND the
        // between-splice text diff — which catches the length-preserving
        // declarator-macro strip — know every site), so find-references on a
        // macro reaches uses the expansion erased (rule #7/#9).
        mint_erased_macro_reads(skel, source, src, map, ctx.plan.as_ref());
        // Macro identity lane: collect every `#define` off the ORIGINAL
        // source (spans in user coordinates, no splice remap needed).
        let macro_defs = self.collect_macro_defs.map(|collect| collect(parser, source)).unwrap_or_default();
        // Nested-macro-body references: a use of macro `A` inside `B`'s
        // `#define` body is preproc-excluded from the code parse (one opaque
        // `preproc_arg` token), so no query capture reaches it and gr on `A`
        // goes dark. Scan each body for identifier tokens naming a KNOWN macro
        // (this file's `#define`s ∪ the include closure's) and mint a read at
        // the ORIGINAL span (rule #7). Only for drivers with the macro lane.
        if self.collect_macro_defs.is_some() {
            let mut known: std::collections::HashSet<String> =
                macro_defs.iter().map(|m| m.name.clone()).collect();
            known.extend(ctx.external.macro_names().map(str::to_string));
            let body_refs = crate::build::cpp_reparse::macro_body_name_refs(parser, source, &known);
            for (name, span) in body_refs.name_refs {
                skel.var_reads.push((name, crate::model::file_analysis::ScopeId(0), span));
            }
            // Field/member uses inside bodies (`->op_next`) — untyped here (the
            // receiver is a macro param), resolved to the declaring class and
            // minted as a class-frozen MethodCall ref in `into_file_analysis`.
            for (field, span) in body_refs.member_refs {
                skel.macro_body_member_reads.push((field, span));
            }
            // Include-guard `#define`s (`#ifndef X` / `#define X`) are pure
            // compilation plumbing, not program entities — mark their symbol so
            // outline / workspace-symbol fold it away (goto-def / references
            // still resolve it). Object-like macro symbols carry kind "var".
            let guards = crate::build::cpp_reparse::collect_include_guard_names(parser, source);
            if !guards.is_empty() {
                for s in skel.symbols.iter_mut() {
                    if s.kind == crate::build::query_extract::DefCap::Var && guards.contains(&s.name) {
                        s.attributes.push("include_guard".to_string());
                    }
                }
            }
        }
        // Function-like macro typing (the expansion flip's payoff): a
        // left-unexpanded macro call parses as `call_expression`, so the
        // macro is a package-global sub. Type it from its body — delegation
        // reuses the see-through target, else a param-independent body type
        // — and hand `into_file_analysis` the hints to lower onto the final
        // `SymbolId`s. `docs/adr/macro-handling.md`.
        skel.macro_returns = macro_return_hints(&macro_defs, parser);
        // Param-return macros need each call site's argument spans so the
        // call resolves to its n-th argument's own value witness (the
        // parametric-return chase). Original coords — same frame as the
        // remapped call/read witnesses.
        let param_macro_names: std::collections::HashSet<String> = skel
            .macro_returns
            .iter()
            .filter(|(_, h)| matches!(h, crate::build::query_extract::MacroReturnHint::Param(_)))
            .map(|(n, _)| n.clone())
            .collect();
        if !param_macro_names.is_empty() {
            skel.macro_call_arg_spans =
                crate::build::cpp_reparse::macro_call_arg_spans(parser, source, &param_macro_names);
        }
        macro_defs
    }

    /// Phase 7: post-assembly hooks — fields only fillable once the
    /// `FileAnalysis` exists (indices are built, symbols are final).
    #[allow(clippy::too_many_arguments)]
    fn register_post_build(
        &self,
        fa: &mut FileAnalysis,
        parser: &mut tree_sitter::Parser,
        source: &str,
        path: Option<&Path>,
        ctx: &PackContext,
        recovered: &[(String, String)],
        macro_defs: Vec<crate::model::file_analysis::MacroDef>,
    ) {
        fa.pack.macro_defs = macro_defs;
        fa.pack.file_module = ctx.module.clone();
        apply_attribute_macros(fa, recovered);
        // Access-specifier regions: a fresh parse of the ORIGINAL source
        // (spans already in original coords, no remap needed) tags each
        // member symbol non-public when its declaration falls under
        // `private:`/`protected:`.
        if let Some(f) = self.access_regions {
            let regions = crate::util::timings::phase("cpp.access_regions", || f(parser, source));
            stamp_access_regions(fa, &regions);
        }
        // The file's include closure is the cross-file visibility key
        // (`ScopedLookup`). Computed here — the driver holds the path the
        // resolver needs; empty on-open until the header cache warms.
        let mut closure_incomplete = false;
        if let (Some(f), Some(p)) = (self.include_closure, path) {
            let (closure, complete) =
                crate::util::timings::phase("cpp.include_closure", || f(p, source));
            fa.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
                closure.iter().map(|s| s.as_str()),
            );
            closure_incomplete = !complete;
        }
        // Never persist an analysis built from a partial dependency view: a
        // skipped gather (on-open cached-only miss, placeholder external table)
        // OR a truncated include closure (a header that resolved but failed to
        // read). Both would freeze a weaker-than-truth analysis behind a
        // deps_stamp that self-validates. `save_to_db` refuses `degraded`;
        // a complete gather next session re-derives the row.
        fa.degraded = ctx.external.degraded || closure_incomplete;
    }
}

#[cfg(feature = "cpp")]
fn cpp_driver() -> PackDriver {
    PackDriver {
        maturity: Maturity::Beta,
        // `.c` too — tree-sitter-cpp parses C (a near-subset), and MISRA /
        // embedded code is C-heavy. One driver serves both.
        exts: &["cpp", "cc", "cxx", "hpp", "hh", "h", "c"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_cpp::LANGUAGE.into()).expect("cpp grammar");
            p
        },
        pack: crate::build::packs::cpp_pack,
        // reparse past the preprocessor before extraction; the anchor map
        // carries the recovered spans back to the original coordinates.
        transform: Some(crate::build::cpp_reparse::preprocess_validated_with),
        gather_macros: Some(crate::build::cpp_reparse::included_macros_pre_expanded),
        collect_macro_defs: Some(crate::build::cpp_reparse::collect_macro_defs),
        member_blocks: Some(crate::build::cpp_reparse::plan_member_blocks),
        include_closure: Some(crate::build::cpp_reparse::include_closure),
        input_fingerprint: Some(crate::build::cpp_reparse::toolchain_fingerprint),
        access_regions: Some(crate::build::cpp_reparse::access_regions),
        file_module: None,
        dependencies: None,
    }
}

#[cfg(feature = "python")]
fn python_driver() -> PackDriver {
    PackDriver {
        maturity: Maturity::Alpha,
        exts: &["py"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_python::LANGUAGE.into()).expect("python grammar");
            p
        },
        pack: crate::build::packs::python_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        access_regions: None,
        file_module: Some(FileModuleRule { marker: crate::build::packs::PYTHON_PACKAGE_MARKER, name: crate::build::packs::python_file_module }),
        dependencies: Some(PackDependencies { locator: crate::build::venv::locator, manifests: &["pyvenv.cfg"] }),
    }
}

#[cfg(feature = "php")]
fn php_driver() -> PackDriver {
    PackDriver {
        maturity: Maturity::Alpha,
        // `.phtml` templates parse under the same HTML-interleaved grammar.
        exts: &["php", "phtml"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            // The `php` flavor, not `php_only`: real files open with `<?php`
            // and may interleave HTML, which this grammar keeps as inert text.
            p.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).expect("php grammar");
            p
        },
        pack: crate::build::packs::php_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        access_regions: None,
        file_module: None,
        dependencies: Some(PackDependencies {
            locator: crate::build::composer::locator,
            manifests: &["composer.json", "installed.json", "autoload_classmap.php"],
        }),
    }
}

#[cfg(feature = "r")]
fn r_driver() -> PackDriver {
    PackDriver {
        maturity: Maturity::Alpha,
        exts: &["R", "r"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_r::LANGUAGE.into()).expect("r grammar");
            p
        },
        pack: crate::build::packs::r_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        access_regions: None,
        file_module: None,
        dependencies: None,
    }
}

#[cfg(feature = "cmake")]
fn cmake_driver() -> PackDriver {
    PackDriver {
        // CMakeLists.txt (no extension match) is a follow-up; `.cmake` now.
        maturity: Maturity::Alpha,
        exts: &["cmake"],
        filenames: &["CMakeLists.txt"],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_cmake::LANGUAGE.into()).expect("cmake grammar");
            p
        },
        pack: crate::build::packs::cmake_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        access_regions: None,
        file_module: None,
        dependencies: None,
    }
}

/// Stamp attribute-macro signals onto recovered classes. For each
/// `(class_name, macro_token)` the declarator-macro strip recovered, look the
/// token up in the plugin-declared attribute-macro vocabulary; when known, add
/// its signal (`exported`/`deprecated`) to the class symbol's `attributes`.
/// The class is recovered either way (the strip is the unknown-macro safety
/// net) — only the SIGNAL is plugin-gated: core owns the recovery mechanism,
/// the plugin owns what the macro means (rule #10).
#[cfg(feature = "pack-langs")]
fn apply_attribute_macros(fa: &mut FileAnalysis, recovered: &[(String, String)]) {
    use crate::model::file_analysis::SymKind;
    if recovered.is_empty() {
        return;
    }
    let signals = crate::build::plugin::default_plugin_registry().attribute_macro_signals();
    for (class_name, macro_token) in recovered {
        let Some(signal) = signals.get(macro_token) else { continue };
        for sym in fa.symbols_mut() {
            if matches!(sym.kind, SymKind::Class)
                && &sym.name == class_name
                && !sym.attributes.contains(signal)
            {
                sym.attributes.push(signal.clone());
            }
        }
    }
}

/// Type each function-like macro from its body: delegation (`#define F(x)
/// G(x)`) reuses the see-through target as a value edge, else a param-
/// independent body type (`#define SQ(x) ((x)*(x))` → Numeric). First def wins
/// per name (a config-variant macro's arms are a later union tier). Object-like
/// macros are skipped — their value/type lanes ride edges, not the sub-return
/// path.
#[cfg(feature = "pack-langs")]
fn macro_return_hints(
    macro_defs: &[crate::model::file_analysis::MacroDef],
    parser: &mut tree_sitter::Parser,
) -> Vec<(String, crate::build::query_extract::MacroReturnHint)> {
    use crate::build::query_extract::MacroReturnHint;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in macro_defs.iter().filter(|m| m.params.is_some()) {
        if !seen.insert(m.name.clone()) {
            continue;
        }
        let hint = match &m.delegate {
            Some(g) => Some(MacroReturnHint::Delegate(g.clone())),
            None => {
                let params = m.params.as_deref().unwrap_or(&[]);
                // Param-dependent identity/projection first (`(x)`, `(b)`);
                // else the param-independent body type (`((x)*(x))`).
                crate::build::cpp_reparse::classify_param_return(parser, &m.body, params)
                    .map(MacroReturnHint::Param)
                    .or_else(|| {
                        crate::build::cpp_reparse::classify_body_type(parser, &m.body)
                            .map(MacroReturnHint::Concrete)
                    })
            }
        };
        if let Some(hint) = hint {
            out.push((m.name.clone(), hint));
        }
    }
    out
}

/// Inject the member-block synthetic bases + parent edges into the extracted
/// skeleton (`docs/adr/macro-handling.md`, "Member-block macros = roles"). The
/// macro's own `#define` symbol is reclassified Variable → Class (the navigable
/// base), members are minted under it (package = the macro), and each member
/// re-sources the SAME `TypeName` edge the expanded field would have. The
/// existing ancestor walk (`resolve_method_in_ancestors` / `parents_of`) then
/// delivers `o->op_type` resolution / hover / the references splat — no parallel
/// field resolution. Spans are already in ORIGINAL coordinates.
#[cfg_attr(not(feature = "cpp"), allow(dead_code))]
fn inject_member_blocks(
    skel: &mut crate::build::query_extract::SkeletonAnalysis,
    plan: &crate::build::cpp_reparse::MemberBlockPlan,
    annot_type: fn(&str) -> Option<crate::model::file_analysis::InferredType>,
) {
    use crate::model::file_analysis::{InferredType, Scope, ScopeId, ScopeKind};
    use crate::build::query_extract::SkelSymbol;
    use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

    if plan.is_empty() {
        return;
    }
    // `struct op → BASEOP`, one edge per pasting struct — the copypasta IS
    // inheritance. `into_file_analysis` folds these into `package_parents`.
    for (child, parent) in &plan.edges {
        skel.parents.push((child.clone(), parent.clone()));
    }
    for base in &plan.bases {
        // The macro's object-like `#define` symbol becomes the navigable base
        // Class (members nest under it; goto-def on the token still routes
        // through the macro identity lane). Both `#define` sites of a config-
        // variant macro reclassify; `into_file_analysis` dedups them by name.
        for s in &mut skel.symbols {
            if s.kind == crate::build::query_extract::DefCap::Var && s.name == base.macro_name && s.package.is_none() {
                s.kind = crate::build::query_extract::DefCap::Class;
            }
        }
        // One scope over the `#define` body, so `scope_at(member_point)` finds
        // it and the member's `Variable{name, scope}` type witness resolves.
        let scope_id = ScopeId(skel.scopes.len() as u32);
        skel.scopes.push(Scope {
            id: scope_id,
            parent: None,
            kind: ScopeKind::Class { name: base.macro_name.clone() },
            span: base.body_scope_span,
            package: Some(base.macro_name.clone()),
            owner: None,
            implicit_receiver: false,
            closed: false,
            captures: Vec::new(),
            direct: false,
            declaration_rules: Vec::new(),
        });
        skel.scope_count = skel.scopes.len();
        for m in &base.members {
            // Field kind + deref_stack + the explicit-annotation witness below:
            // the SAME payload a plainly-declared struct field carries, so no
            // renderer (hover stars, inlay suppression, `*field*` labeling) can
            // tell a macro-pasted member from a directly-declared one (rule #10).
            skel.symbols.push(SkelSymbol {
                kind: crate::build::query_extract::DefCap::Field,
                name: m.name.clone(),
                start: m.name_span.start,
                end: m.name_span.end,
                name_start: m.name_span.start,
                name_end: m.name_span.end,
                package: Some(base.macro_name.clone()),
                scope: scope_id,
                spelled: None,
                pair: None,
                return_type: None,
                deref_stack: m.deref_stack.clone(),
                attributes: Vec::new(),
                arity: None,
                qualifier_owned: false,
                flags: Default::default(),
            });
            // The role member emits the SAME `TypeName` edge an expanded field
            // does — the edge is canonical (the hover leaf + the type chase
            // resolve `op_type` → `unsigned short`). Tagged `Annotation(Declared)` (the
            // explicit-annotation source a plain field's declared type carries)
            // so priority and inlay suppression match field-for-field.
            let payload = match annot_type(&m.type_text) {
                Some(InferredType::ClassName(cn)) => {
                    Some(WitnessPayload::Edge(WitnessAttachment::TypeName(cn)))
                }
                Some(t) => Some(WitnessPayload::InferredType(t)),
                None => None,
            };
            if let Some(payload) = payload {
                skel.witnesses.push(Witness {
                    attachment: WitnessAttachment::Variable { name: m.name.clone(), scope: scope_id },
                    source: WitnessSource::Annotation(crate::model::witnesses::AnnotationKind::Declared),
                    payload,
                    span: m.name_span,
                });
            }
        }
    }
}

/// Tag every member symbol whose declaration falls inside a non-public
/// access region with a `"non_public"` attribute — a
/// value-borne fact on the symbol, so member completion filters by asking
/// the symbol, never by re-deriving visibility from a name/kind guess.
/// Span containment (not equality) because a region's span is the whole
/// declaration node (`field_declaration`/`function_definition`) while a
/// Method/Sub Symbol's own span can be the narrower body-having node.
#[cfg_attr(not(feature = "cpp"), allow(dead_code))]
fn stamp_access_regions(fa: &mut FileAnalysis, regions: &[crate::build::cpp_reparse::AccessRegion]) {
    use crate::model::file_analysis::Span;
    let contains = |o: &Span, i: &Span| {
        (o.start.row, o.start.column) <= (i.start.row, i.start.column)
            && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
    };
    for sym in fa.symbols_mut() {
        if sym.package.is_none() {
            continue;
        }
        let non_public = regions
            .iter()
            .filter(|r| contains(&r.span, &sym.span))
            .min_by_key(|r| {
                let s = r.span;
                (s.end.row - s.start.row, s.end.column.saturating_sub(s.start.column))
            })
            .is_some_and(|r| r.non_public);
        if non_public && !sym.flags.contains(crate::model::file_analysis::SymbolFlags::NON_PUBLIC) {
            sym.attributes.push("non_public".to_string());
            sym.flags.insert(crate::model::file_analysis::SymbolFlags::NON_PUBLIC);
        }
    }
}

/// Emit `TypeName(name) → …` witnesses for the type-alias `#define`s gathered
/// from a file's include closure. The cross-file `TypeName` chase resolves an
/// alias by `get_cached(name)` → the header defining it; that fails when the
/// header is gitignored (perl5's generated `config.h`, where `U16TYPE unsigned
/// short` lives), so the alias never resolves past that hop. The gather already
/// followed the `#include` and has the body, so carry it into THIS file's bag —
/// the hop then resolves locally. Gated on a type-shaped body so the sea of
/// value macros mints nothing. Non-cpp packs gather nothing (empty iterator).
#[cfg(feature = "pack-langs")]
fn emit_external_type_aliases(
    witnesses: &mut Vec<crate::model::witnesses::Witness>,
    external: &crate::build::cpp_reparse::PreExpandedExternal,
    annot_type: fn(&str) -> Option<crate::model::file_analysis::InferredType>,
) {
    use crate::model::file_analysis::Span;
    use tree_sitter::Point;
    for (name, body) in external.object_like_macros() {
        let body = body.trim();
        if !crate::build::query_extract::looks_like_type_spelling(body) {
            continue;
        }
        witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::TypeName(name.to_string()),
            source: crate::model::witnesses::WitnessSource::Builder("external-macro-alias".into()),
            payload: crate::build::query_extract::type_alias_payload(body, annot_type),
            span: Span { start: Point { row: 0, column: 0 }, end: Point { row: 0, column: 0 } },
        });
    }
}

/// Remap extracted skeleton spans from transformed coords back to
/// original source coords via the anchor map. A no-op for an identity
/// map (clean/pass-through files round-trip byte→point→byte unchanged),
/// so it's safe to always call.
///
/// EVERY span-bearing field the extraction produced pre-remap must pass
/// through here — one missed field means its consumer silently queries
/// transformed coordinates that no longer match anything after a
/// length-changing splice (the class of bug: member resolution dying on
/// the spliced line while the rest of the file works). The exhaustive
/// destructuring (no `..`) is the enforcement: adding a field to
/// `SkeletonAnalysis` / `SkelRef` / `SkelSymbol` fails to compile HERE
/// until the new field's spans — or its span-lessness, bound as `_` —
/// are accounted for.
#[cfg(feature = "pack-langs")]
fn remap_spans(
    skel: &mut crate::build::query_extract::SkeletonAnalysis,
    transformed: &str,
    original: &str,
    map: &crate::build::cpp_reparse::SpliceMap,
) {
    use tree_sitter::Point;
    let t = LineIndex::new(transformed);
    let o = LineIndex::new(original);
    let r = |p: Point| -> Point { o.point(map.to_original(t.byte(p))) };
    // A ref/read that came OUT of a macro expansion collapses to a
    // zero-width point under `to_original` (every expanded byte maps to the
    // splice site) — goto-def/hover would then miss it. Give it the macro
    // CALL site's extent instead, so `newThing(5)` resolves to the expanded
    // `Perl_newThing` (see-through to the function).
    let remap_span = |start: Point, end: Point| -> (Point, Point) {
        match map.replacement_at(t.byte(start)) {
            Some((os, oe)) => (o.point(os), o.point(oe)),
            None => (r(start), r(end)),
        }
    };
    let rspan = |sp: crate::model::file_analysis::Span| -> crate::model::file_analysis::Span {
        let (start, end) = remap_span(sp.start, sp.end);
        crate::model::file_analysis::Span { start, end }
    };

    let crate::build::query_extract::SkeletonAnalysis {
        symbols,
        refs,
        imports: _,
        import_sites,
        use_rows,
        namespace_ranges,
        scope_count: _,
        scopes,
        witnesses,
        parents: _,
        super_parents: _,
        var_reads,
        var_writes,
        write_sites,
        label_refs,
        flow_edges,
        moved_from,
        control_regions,
        param_regions,
        domain_sites,
        macro_returns: _,
        // Populated in enrich_skeleton (post-remap) already in original coords.
        macro_call_arg_spans: _,
        call_sites,
        receiver_sites,
        specializations: _,
        // name-keyed, ordered by byte position pre-remap — no spans to fix.
        template_params: _,
        // name/scope-keyed — no spans to fix.
        field_types: _,
        return_sites,
        receivers,
        param_sigs,
        // Populated later (enrich_skeleton) already in original coords — no remap.
        macro_body_member_reads: _,
        language: _,
        names: _,
        slot_writes,
        member_writes,
        // scope-keyed — no spans to fix.
        class_scopes: _,
    } = skel;

    for s in symbols.iter_mut().chain(slot_writes.iter_mut().map(|w| &mut w.symbol)) {
        let crate::build::query_extract::SkelSymbol {
            kind: _,
            name: _,
            start,
            end,
            name_start,
            name_end,
            package: _,
            scope: _,
            spelled: _,
            pair: _,
            return_type: _,
            deref_stack: _,
            attributes: _,
            arity: _,
            qualifier_owned: _,
            flags: _,
        } = s;
        *start = r(*start);
        *end = r(*end);
        *name_start = r(*name_start);
        *name_end = r(*name_end);
    }
    // Parameter-list spans feed the def-arity association (`into_file_analysis`,
    // which runs after this remap) — they must speak original coords like the
    // symbol spans they're matched against.
    for (span, _) in param_sigs.iter_mut() {
        *span = rspan(*span);
    }
    for rf in refs.iter_mut() {
        let crate::build::query_extract::SkelRef {
            kind: _,
            name: _,
            start,
            end,
            scope: _,
            invocant,
            member_op,
            arg_count: _,
            constructs: _,
        } = rf;
        (*start, *end) = remap_span(*start, *end);
        // The invocant span is consumed via `expr_type_at_span` (member
        // resolution); the member-op span rides the MethodCall ref. Both
        // die on the spliced line if left in transformed coords.
        if let Some((sp, _text)) = invocant {
            *sp = rspan(*sp);
        }
        if let Some((_op, sp)) = member_op {
            *sp = rspan(*sp);
        }
    }
    for (_, _, span) in var_reads.iter_mut().chain(var_writes.iter_mut()) {
        *span = rspan(*span);
    }
    for span in write_sites.iter_mut() {
        *span = rspan(*span);
    }
    // Call-site spans feed the call-value edge (`into_file_analysis`, after
    // this remap) and must speak original coords like the flow-edge source
    // (the same call span) they land beside.
    for site in call_sites.iter_mut() {
        site.span = rspan(site.span);
        site.callee_at = rspan(crate::model::file_analysis::Span { start: site.callee_at, end: site.callee_at }).start;
        if let Some(recv) = site.receiver.as_mut() {
            *recv = rspan(*recv);
        }
        for (arg, _) in site.args.iter_mut().flatten() {
            *arg = rspan(*arg);
        }
    }
    for site in receiver_sites.iter_mut() {
        site.span = rspan(site.span);
    }
    for (_, span) in return_sites.iter_mut().chain(receivers.iter_mut()) {
        *span = rspan(*span);
    }
    // Member writes settle through `expr_type_at_span` on the receiver and an
    // edge to the value's `Expr`, both of which speak original coords.
    for w in member_writes.iter_mut() {
        let crate::build::query_extract::MemberWrite { receiver, member: _, value } = w;
        *receiver = rspan(*receiver);
        *value = rspan(*value);
    }
    for sc in scopes.iter_mut() {
        sc.span.start = r(sc.span.start);
        sc.span.end = r(sc.span.end);
    }
    // `#include` path tokens — goto-def on the token is span-keyed.
    for (_, span) in import_sites.iter_mut() {
        *span = rspan(*span);
    }
    // `use` row name tokens — a class token inside one names the row.
    for row in use_rows.iter_mut() {
        row.span = rspan(row.span);
        row.reach = rspan(row.reach);
    }
    for section in namespace_ranges.iter_mut() {
        section.span = rspan(section.span);
    }
    // Witness spans (the type tier). A length-changing splice (`PBF op_type` →
    // `unsigned op_type`) shifts every span AFTER it, so a declared-type
    // witness left in transformed coords lands past the original query point
    // and the temporal fold drops it. Remap the witness `.span`, any span-
    // bearing attachment (`Expr`/`BranchArm`), and the same shapes anywhere in
    // the payload — so `expr_type_at_span` and the temporal ordering all speak
    // original coordinates, like refs. Both matches are exhaustive: a new
    // span-bearing shape fails to compile here until it is remapped.
    use crate::model::witnesses::WitnessAttachment;
    let mut remap_att = |a: &mut WitnessAttachment| match a {
        WitnessAttachment::Expr(sp) | WitnessAttachment::BranchArm(sp) => *sp = rspan(*sp),
        WitnessAttachment::Variable { .. }
        | WitnessAttachment::Expression(_)
        | WitnessAttachment::Symbol(_)
        | WitnessAttachment::HashKey { .. }
        | WitnessAttachment::PackageSymbol { .. }
        | WitnessAttachment::SymbolReturnArm(_)
        | WitnessAttachment::TypeName(_)
        | WitnessAttachment::Field { .. }
        | WitnessAttachment::Param { .. } => {}
    };
    for w in witnesses.iter_mut() {
        remap_att(&mut w.attachment);
        w.payload.for_each_attachment_mut(&mut remap_att);
        w.span = rspan(w.span);
    }
    // Value-flow edges (the provenance tier above the bag) + label/goto refs +
    // moved-from sites + domain-typing sites all carry transformed spans too.
    for fe in flow_edges.iter_mut() {
        let crate::model::file_analysis::FlowEdge {
            target_name: _,
            target_scope: _,
            target_at,
            source,
            extraction: _,
            reassigns: _,
        } = fe;
        *target_at = r(*target_at);
        *source = rspan(*source);
    }
    for (_, _, span) in label_refs.iter_mut() {
        *span = rspan(*span);
    }
    for (_, span, _) in moved_from.iter_mut() {
        *span = rspan(*span);
    }
    for span in control_regions.iter_mut() {
        *span = rspan(*span);
    }
    for span in param_regions.iter_mut() {
        *span = rspan(*span);
    }
    for ds in domain_sites.iter_mut() {
        let crate::model::file_analysis::DomainSite { slot: _, value: _, slot_span } = ds;
        *slot_span = rspan(*slot_span);
    }
}

/// Re-mint a variable read at every macro use the transform ERASED from the
/// parsed text — expansion splices (the map's edits, original coordinates)
/// and member-block blanks (length-preserving, recovered by diffing the
/// blanked source). Without these the use has no token in the tree, so no
/// query capture can ref it and find-references on the macro goes dark.
/// Runs after `remap_spans` (skeleton scopes already in original coords),
/// before `into_file_analysis` (which resolves/mints the actual refs).
#[cfg(feature = "pack-langs")]
fn mint_erased_macro_reads(
    skel: &mut crate::build::query_extract::SkeletonAnalysis,
    original: &str,
    transformed: &str,
    map: &crate::build::cpp_reparse::SpliceMap,
    plan: Option<&crate::build::cpp_reparse::MemberBlockPlan>,
) {
    use crate::model::file_analysis::{ScopeId, Span};
    let bytes = original.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    let mut sites: Vec<usize> = map.expansion_sites().map(|(os, _)| os).collect();
    if let Some(plan) = plan {
        let blanked = plan.blanked_source.as_bytes();
        if blanked.len() == bytes.len() && blanked != bytes {
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] != blanked[i] {
                    let start = i;
                    while i < bytes.len() && bytes[i] != blanked[i] {
                        i += 1;
                    }
                    sites.push(start);
                } else {
                    i += 1;
                }
            }
        }
    }
    // Between-splice diff: bytes the transform changed OUTSIDE any recorded
    // edit are length-preserving blanks (the declarator-macro strip runs
    // before expansion and records nothing in the map). Walk the pass-through
    // segments original↔transformed in lockstep and mint a site at every
    // differing run.
    {
        let tb = transformed.as_bytes();
        let diff_segment = |o_from: usize, o_to: usize, t_from: usize, sites: &mut Vec<usize>| {
            let len = o_to.saturating_sub(o_from);
            if t_from + len > tb.len() {
                return;
            }
            let mut i = 0;
            while i < len {
                if bytes[o_from + i] != tb[t_from + i] {
                    let start = o_from + i;
                    while i < len && bytes[o_from + i] != tb[t_from + i] {
                        i += 1;
                    }
                    sites.push(start);
                } else {
                    i += 1;
                }
            }
        };
        let mut o_pos = 0usize;
        let mut t_pos = 0usize;
        for &(os, oe, nlen) in map.edits() {
            diff_segment(o_pos, os, t_pos, &mut sites);
            t_pos += (os - o_pos) + nlen;
            o_pos = oe;
        }
        diff_segment(o_pos, bytes.len(), t_pos, &mut sites);
    }
    if sites.is_empty() {
        return;
    }
    sites.sort_unstable();
    sites.dedup();
    let o = LineIndex::new(original);
    for os in sites {
        let mut e = os;
        while e < bytes.len() && is_id(bytes[e]) {
            e += 1;
        }
        if e == os {
            continue;
        }
        let name = original[os..e].to_string();
        let span = Span { start: o.point(os), end: o.point(e) };
        // Innermost skeleton scope containing the site (root when none).
        let mut scope = ScopeId(0);
        let mut best: Option<crate::model::file_analysis::Span> = None;
        for sc in &skel.scopes {
            let within = (sc.span.start.row, sc.span.start.column)
                <= (span.start.row, span.start.column)
                && (span.end.row, span.end.column) <= (sc.span.end.row, sc.span.end.column);
            if within
                && best.is_none_or(|b| {
                    (sc.span.start.row, sc.span.start.column) >= (b.start.row, b.start.column)
                })
            {
                best = Some(sc.span);
                scope = sc.id;
            }
        }
        skel.var_reads.push((name, scope, span));
    }
}

/// Line-start byte offsets, for Point↔byte conversion (Point.column is a
/// byte offset within its row).
#[cfg(feature = "pack-langs")]
struct LineIndex {
    starts: Vec<usize>,
}

#[cfg(feature = "pack-langs")]
impl LineIndex {
    fn new(s: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in s.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }
    fn byte(&self, p: tree_sitter::Point) -> usize {
        self.starts.get(p.row).copied().unwrap_or(0) + p.column
    }
    fn point(&self, byte: usize) -> tree_sitter::Point {
        let row = self.starts.partition_point(|&s| s <= byte).saturating_sub(1);
        tree_sitter::Point { row, column: byte - self.starts[row] }
    }
}

/// The drivers this binary was compiled with. Perl always; pack languages
/// per feature. A process SERVES exactly one of them (`served`), chosen once
/// at startup; the rest of the catalog answers id lookups (capabilities,
/// `--languages`, `--parse`) and nothing else.
pub struct LanguageRegistry {
    drivers: Vec<Box<dyn LanguageDriver>>,
}

/// The language this process serves, latched by the first `served()` read or
/// `lock_process_language` call — whichever comes first. A later lock to a
/// DIFFERENT language is refused, so nothing can answer one request in one
/// language and the next in another.
static PROCESS_LANGUAGE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

impl LanguageRegistry {
    /// The catalog, built once per process. Which drivers a binary carries
    /// is fixed at compile time, and capability asks reach for it on the
    /// keystroke path, several per diagnostics publish.
    pub fn compiled() -> &'static Self {
        static REGISTRY: std::sync::OnceLock<LanguageRegistry> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| {
            #[cfg_attr(
                not(feature = "pack-langs"),
                allow(unused_mut)
            )]
            let mut drivers: Vec<Box<dyn LanguageDriver>> = vec![Box::new(PerlDriver)];
            #[cfg(feature = "cpp")]
            drivers.push(Box::new(cpp_driver()));
            #[cfg(feature = "python")]
            drivers.push(Box::new(python_driver()));
            #[cfg(feature = "r")]
            drivers.push(Box::new(r_driver()));
            #[cfg(feature = "cmake")]
            drivers.push(Box::new(cmake_driver()));
            #[cfg(feature = "php")]
            drivers.push(Box::new(php_driver()));
            // Registered here too, not only on analysis, because a warm start
            // answers from cached analyses no driver has built this process.
            for d in &drivers {
                if let Some(pack) = d.lang_pack() {
                    crate::model::conventions::register_lattice(d.id(), || pack.lattice);
                }
            }
            LanguageRegistry { drivers }
        })
    }

    /// The driver this process serves. Every file the process is handed —
    /// opened, watched, walked, named on the command line — is analyzed by
    /// it; the client's document selector decides which files arrive, not a
    /// per-path route here. Unlocked, the process serves Perl.
    pub fn served() -> &'static dyn LanguageDriver {
        let id = *PROCESS_LANGUAGE.get_or_init(|| PerlDriver.id());
        Self::compiled().for_id(id).expect("the locked language is compiled in")
    }

    /// Fix the process language. Called once, at startup, before anything
    /// reads `served()`. Refuses an id this binary was not compiled with,
    /// and a second lock to a different language.
    pub fn lock_process_language(id: &str) -> Result<&'static dyn LanguageDriver, String> {
        let reg = Self::compiled();
        let Some(driver) = reg.for_id(id) else {
            return Err(format!(
                "unknown language `{id}`; this build serves: {}",
                reg.languages().join(", ")
            ));
        };
        let locked = *PROCESS_LANGUAGE.get_or_init(|| driver.id());
        if locked != driver.id() {
            return Err(format!("this process already serves `{locked}`, not `{id}`"));
        }
        Ok(driver)
    }

    /// `lock_process_language` for a verb whose subject is one file and
    /// whose caller named no `--lang`: the file's language, by extension,
    /// Perl when no compiled driver claims it. A process already locked
    /// keeps its language.
    pub fn lock_process_language_for(path: &Path) -> Result<&'static dyn LanguageDriver, String> {
        if let Some(id) = PROCESS_LANGUAGE.get() {
            return Self::lock_process_language(id);
        }
        let id = Self::compiled().for_path(path).map(|d| d.id()).unwrap_or(PerlDriver.id());
        Self::lock_process_language(id)
    }

    /// May `driver` analyze `path`? Refused only when ANOTHER compiled driver
    /// claims it — an unclaimed path (an extensionless script, a `.def`
    /// handed over by the client) is the driver's to try. A refused file
    /// answers nothing rather than being read in the wrong language.
    pub fn admits(driver: &dyn LanguageDriver, path: &Path) -> Result<(), String> {
        match Self::compiled().for_path(path) {
            Some(owner) if owner.id() != driver.id() => Err(format!(
                "{} is a {} file; this process serves {} (run a separate `perl-lsp --lang {}`)",
                path.display(),
                Self::display_name(owner.id()),
                Self::display_name(driver.id()),
                owner.id(),
            )),
            _ => Ok(()),
        }
    }

    /// The compiled driver claiming `path` by filename or extension.
    pub fn for_path(&self, path: &Path) -> Option<&dyn LanguageDriver> {
        // Exact filename first (CMakeLists.txt has no extension), then ext.
        if let Some(name) = path.file_name().and_then(|f| f.to_str()) {
            if let Some(d) = self.drivers.iter().find(|d| d.filenames().contains(&name)) {
                return Some(d.as_ref());
            }
        }
        let ext = path.extension()?.to_str()?;
        self.drivers.iter().find(|d| d.extensions().contains(&ext)).map(|d| d.as_ref())
    }

    pub fn for_id(&self, id: &str) -> Option<&dyn LanguageDriver> {
        self.drivers.iter().find(|d| d.id() == id).map(|d| d.as_ref())
    }

    /// Compiled language ids — what `--lang` accepts.
    pub fn languages(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|d| d.id()).collect()
    }

    /// Whether `id` names a pack-served language — its driver carries a
    /// `LangPack` (Perl's doesn't, so it answers false with no
    /// language-name branch). THE routing fact `resolve()` reads off an
    /// origin's stamped `FileAnalysis.language` at CandidateSet
    /// construction. Memoized: the pack set is fixed at compile time.
    pub fn is_pack_language(id: &str) -> bool {
        static PACK_IDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
        PACK_IDS
            .get_or_init(|| {
                LanguageRegistry::compiled()
                    .drivers
                    .iter()
                    .filter(|d| d.lang_pack().is_some())
                    .map(|d| d.id())
                    .collect()
            })
            .iter()
            .any(|l| *l == id)
    }

    /// The visibility routing fact for `id`'s language, which
    /// `VisibilityAxis::for_origin` derives the origin's axis from: a pack
    /// whose class spellings resolve through a use-map is scoped by that
    /// use-map (`NameKeyed`), any other pack by its include closure, the
    /// host by its search path. Read off the pack's own `names`, never a
    /// language-name branch. Memoized like `is_pack_language`.
    pub fn pack_visibility(id: &str) -> crate::model::file_analysis::PackVisibility {
        use crate::model::file_analysis::PackVisibility;
        static ROUTES: std::sync::OnceLock<Vec<(&'static str, PackVisibility)>> = std::sync::OnceLock::new();
        ROUTES
            .get_or_init(|| {
                LanguageRegistry::compiled()
                    .drivers
                    .iter()
                    .filter_map(|d| {
                        let pack = d.lang_pack()?;
                        let route = match pack.names.use_map_sep() {
                            Some(_) => PackVisibility::NameKeyed,
                            None => PackVisibility::IncludePaths,
                        };
                        Some((d.id(), route))
                    })
                    .collect()
            })
            .iter()
            .find(|(l, _)| *l == id)
            .map_or(PackVisibility::Host, |(_, route)| *route)
    }

    /// The declared capabilities of `id`'s driver — THE generic capability
    /// asker (the collapse the two-boolean ceiling in docs/PARKED.md
    /// called for). An id no driver claims answers `DriverCaps::default()`
    /// (everything off), which is also what every serving site's generic
    /// arm assumed of an unknown language before the capabilities existed.
    pub fn caps(id: &str) -> DriverCaps {
        LanguageRegistry::compiled()
            .for_id(id)
            .map(|d| d.caps())
            .unwrap_or_default()
    }

    /// Does this language's pack declare `#include`-style path tokens (the
    /// header-is-the-module reference goto-def resolves and references
    /// reverses)? THE gate for the include-token lanes on BOTH serving
    /// surfaces — the LSP handlers and their CLI/--batch mirrors — so
    /// editor and gold cannot answer it differently.
    pub fn has_include_tokens(id: &str) -> bool {
        use crate::build::query_extract::{Capture, ImportCap};
        Self::document_mints(id, Capture::Import(ImportCap::Path))
    }

    /// Does this language's pack declare a C-style preprocessor — `#define`
    /// macros reachable through `#include`s that identifier-context
    /// completion offers? Same asked-never-named contract as
    /// `has_include_tokens`.
    pub fn has_preprocessor_macros(id: &str) -> bool {
        use crate::build::query_extract::{Capture, DefCap, DefPart};
        Self::document_mints(id, Capture::Def { kind: DefCap::Macro, part: DefPart::Node })
    }

    /// Does `id`'s query document mint `capture`? A capability the document
    /// states is read off what compiles, never a pack bool beside it that can
    /// disagree with the patterns (rule #15). The compilation is the
    /// extractor's own, and the answer is memoized per language because it
    /// cannot change within a process.
    fn document_mints(id: &str, capture: crate::build::query_extract::Capture) -> bool {
        use crate::build::query_extract::Capture;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        static MINTS: OnceLock<Mutex<HashMap<String, Arc<Vec<Capture>>>>> = OnceLock::new();
        let memo = MINTS.get_or_init(Default::default);
        if let Some(caps) = memo.lock().unwrap().get(id) {
            return caps.contains(&capture);
        }
        let caps: Arc<Vec<Capture>> = Arc::new(
            LanguageRegistry::compiled()
                .for_id(id)
                .and_then(|d| Some((d.make_parser().language()?.clone(), d.lang_pack()?)))
                .map(|(language, pack)| crate::build::query_extract::document_captures(&language, &pack))
                .unwrap_or_default(),
        );
        memo.lock().unwrap().insert(id.to_string(), Arc::clone(&caps));
        caps.contains(&capture)
    }

    /// Human-facing name for a pack language id, for startup banners and
    /// progress messages. Purely cosmetic — `for_id` still speaks the short
    /// id everywhere else. Falls back to the id itself for a language this
    /// mapping hasn't been told about yet (never a hard error over a
    /// display string).
    pub fn display_name(id: &str) -> &str {
        match id {
            "perl" => "Perl",
            "cpp" => "C/C++",
            "python" => "Python",
            "r" => "R",
            "cmake" => "CMake",
            "php" => "PHP",
            _ => id,
        }
    }
}

#[cfg(test)]
#[path = "language_driver_tests.rs"]
mod tests;
