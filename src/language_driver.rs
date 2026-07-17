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
    /// Source + the file's path → `FileAnalysis`. The path lets a driver
    /// resolve cross-file context (C++ gathers `#define`s from `#include`d
    /// headers so namespace/export macros expand). Default ignores it.
    fn analyze_with_path(&self, source: &str, _path: Option<&Path>) -> FileAnalysis {
        self.analyze(source)
    }
    /// Completion trigger characters for this language — the registry
    /// unions them into the LSP `completionProvider` slot, so the client
    /// auto-fires completion (e.g. on `.`/`->`) for the right files.
    fn trigger_chars(&self) -> &[&'static str];
    /// The language's `LangPack` — the ONE per-language config (grammar facts
    /// the query engine AND the cursor-completion path both read). `None` for
    /// the native Perl path (it uses `cursor_context`, not the pack). Lets a
    /// caller reach the pack through the single `for_id` lookup, no parallel
    /// `lang_cfg` registry.
    fn lang_pack(&self) -> Option<crate::query_extract::LangPack> {
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
    /// Content sniff for a file whose extension no driver claims (the
    /// `for_path` registry lookup came up empty) — a structural signature
    /// over the first ~1KB, never a name/extension list (rule #10:
    /// `commands.def` is C, a Windows `.def` module-definition file isn't,
    /// same extension). Default: no opinion: an unclaimed extension is a
    /// weak, ambiguous signal, so only a driver that positively recognizes
    /// its own shape should claim it.
    fn sniff(&self, _prefix: &str) -> bool {
        false
    }
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
    fn make_parser(&self) -> tree_sitter::Parser {
        crate::builder::create_parser()
    }
    fn analyze(&self, source: &str) -> FileAnalysis {
        let mut parser = crate::builder::create_parser();
        match parser.parse(source, None) {
            Some(tree) => crate::builder::build(&tree, source.as_bytes()),
            None => FileAnalysis::new(Default::default()),
        }
    }
    fn trigger_chars(&self) -> &[&'static str] {
        // Sigils open variable completion; `>`/`:`/`{` open
        // method/pkg/hash-key slots; `(`/`,` are signature-help adjacent.
        &["$", "@", "%", ">", ":", "{", "(", ","]
    }
}

/// A pack-language driver — the generic, query-driven path. One value
/// per language: a grammar, a `LangPack` (capture predicates), and an
/// optional pre-parse `transform` (C++ uses it for macro reparse;
/// others pass through). The whole multi-language story for a language
/// whose extraction is query-shaped is a `PackDriver { ... }` literal.
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
pub struct PackDriver {
    id: &'static str,
    exts: &'static [&'static str],
    /// Exact filenames this driver also claims (extensionless conventions
    /// like `CMakeLists.txt`). Matched before extension.
    filenames: &'static [&'static str],
    make_parser: fn() -> tree_sitter::Parser,
    pack: fn() -> crate::query_extract::LangPack,
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
            &crate::cpp_reparse::PreExpandedExternal,
        ) -> (String, crate::cpp_reparse::SpliceMap, Vec<(String, String)>),
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
        ) -> std::sync::Arc<crate::cpp_reparse::PreExpandedExternal>,
    >,
    /// The macro identity/navigation lane collector (C preprocessor only):
    /// original source → every `#define` as a `MacroDef` (guard trail, def
    /// span, delegation callee). `None` for packs without a preprocessor.
    collect_macro_defs: Option<fn(&mut tree_sitter::Parser, &str) -> Vec<crate::file_analysis::MacroDef>>,
    /// Member-block macros as roles (C preprocessor only): classify a macro
    /// pasted standalone into a struct/class body, BLANK the use (so the base
    /// parses clean), and mint the synthetic base + parent edges. `None` for
    /// packs without a preprocessor.
    member_blocks: Option<fn(&mut tree_sitter::Parser, &str) -> crate::cpp_reparse::MemberBlockPlan>,
    /// Transitive `#include` closure (C preprocessor only): file path + source →
    /// canonical header paths this file reaches — the cross-file VISIBILITY key
    /// (`ScopedLookup` ranks `get_cached` candidates by it). `None` for packs
    /// with no include model.
    include_closure: Option<fn(&Path, &str) -> (Vec<String>, bool)>,
    /// External analysis-input fingerprint (see
    /// `LanguageDriver::analysis_input_fingerprint`). `None` = no external
    /// inputs (fingerprint 0).
    input_fingerprint: Option<fn() -> u64>,
    /// Content-sniff for the unknown-extension fallback (see
    /// `LanguageDriver::sniff`). `None` = this driver never claims an
    /// extension it doesn't already list (python/R/cmake have unambiguous
    /// extensions; only C/C++ shares its extension space with unrelated
    /// formats).
    sniff: Option<fn(&str) -> bool>,
    /// `public:`/`private:`/`protected:` region scan (C preprocessor-free
    /// languages only; `None` when the language has no access-specifier
    /// concept). Stamps a `non_public` attribute on member symbols so
    /// completion can filter by visibility.
    access_regions: Option<fn(&mut tree_sitter::Parser, &str) -> Vec<crate::cpp_reparse::AccessRegion>>,
}

/// Pre-parse external state gathered in phase 1 (`gather_pack_context`) and
/// threaded through phases 2 and 5 (`transform_and_parse`, `enrich_skeleton`)
/// — see the phase list on `analyze_with_path`.
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
struct PackContext {
    external: std::sync::Arc<crate::cpp_reparse::PreExpandedExternal>,
    plan: Option<crate::cpp_reparse::MemberBlockPlan>,
}

#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
impl LanguageDriver for PackDriver {
    fn id(&self) -> &'static str {
        self.id
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
    ///    so the base parses clean). Produces `PackContext`.
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
    ///    (unchanged; a method on `SkeletonAnalysis`, not a phase fn here).
    /// 7. `emit_return_fuel` — post-assembly implicit-return / implicit-
    ///    `this` interpretation over the FINAL FileAnalysis (stable
    ///    SymbolIds, resolved refs): an `auto`-returning function with no
    ///    declared type chains its Symbol onto its `return`-statement sites
    ///    (structural-only in the skeleton — `SkeletonAnalysis::return_sites`
    ///    — so `query_extract.rs` stays language-generic; the "this needs
    ///    implicit-return fuel" READING of that data is cpp semantics, so it
    ///    lives here). The `implicit_this_members`-gated half also mints
    ///    implicit-`this` FIELD-read edges AND pins bare sibling method
    ///    CALLs to the enclosing class (`resolved_package`), so both halves
    ///    of C++'s receiver elision resolve. MUST run after (6): needs final
    ///    SymbolIds + resolved `fa.refs`.
    /// 8. `register_post_build` — post-assembly hooks that stamp fields only
    ///    queryable once the FileAnalysis exists: macro defs, attribute-macro
    ///    signals, access-region visibility, include closure, degraded flag.
    ///
    /// Add a phase by inserting a numbered call here plus a fn beside the
    /// others below, in order — don't inline new logic into an existing
    /// phase's body.
    fn analyze_with_path(&self, source: &str, path: Option<&Path>) -> FileAnalysis {
        let mut parser = (self.make_parser)();
        let ctx = self.gather_pack_context(&mut parser, source, path);
        let Some((tree, src, map, recovered)) = self.transform_and_parse(&mut parser, source, &ctx)
        else {
            let mut fa = FileAnalysis::new(Default::default());
            fa.degraded = true;
            return fa;
        };
        let pack = (self.pack)();
        match crate::query_extract::extract(&tree, src.as_bytes(), &pack) {
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
                // `return_sites` is structural skeleton output; taken before
                // assembly consumes `skel` so phase 7 can interpret it against
                // the FINAL FileAnalysis.
                let return_sites = std::mem::take(&mut skel.return_sites);
                let mut fa = skel.into_file_analysis();
                emit_return_fuel(&mut fa, &return_sites, pack.implicit_this_members);
                self.register_post_build(&mut fa, &mut parser, source, path, &ctx, &recovered, macro_defs);
                fa
            }
            Err(e) => {
                // Fail LOUD, and mark the empty stand-in degraded so the
                // persist tier can't freeze it (a cached empty analysis would
                // be re-served forever — the source file never changes).
                log::warn!(
                    "query extract failed for {:?}: {e:?} — serving an empty (non-cacheable) analysis",
                    path
                );
                let mut fa = FileAnalysis::new(Default::default());
                fa.degraded = true;
                fa
            }
        }
    }
    fn lang_pack(&self) -> Option<crate::query_extract::LangPack> {
        Some((self.pack)())
    }
    fn trigger_chars(&self) -> &[&'static str] {
        (self.pack)().trigger_chars
    }
    fn analysis_input_fingerprint(&self) -> u64 {
        self.input_fingerprint.map(|f| f()).unwrap_or(0)
    }
    fn sniff(&self, prefix: &str) -> bool {
        self.sniff.is_some_and(|f| f(prefix))
    }
}

/// The pack analyze pipeline's phases (1/2/5/7 in `analyze_with_path`'s doc;
/// 3/4/6 are the free fns / `SkeletonAnalysis` method called between them).
/// Order is fixed and load-bearing — see that doc for the full contract.
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
impl PackDriver {
    /// Phase 1: pre-parse external context — the cross-file macro table and
    /// the member-block plan. Both read the ORIGINAL source only.
    fn gather_pack_context(&self, parser: &mut tree_sitter::Parser, source: &str, path: Option<&Path>) -> PackContext {
        // Cross-file macros from #included headers (C++), so a macro
        // #defined elsewhere (SPDLOG_NAMESPACE_BEGIN) expands here.
        let external = match (self.gather_macros, path) {
            (Some(g), Some(p)) => crate::timings::phase("cpp.gather", || g(p, source, parser)),
            _ => std::sync::Arc::new(crate::cpp_reparse::PreExpandedExternal::empty()),
        };
        // Member-block macros as roles: BLANK the standalone-in-struct-body uses
        // so `struct op { BASEOP };` parses clean, and mint the synthetic base +
        // parent edges (injected in phase 5, into the extracted skeleton). The
        // blank is length-preserving, so the transform + remap stay in original
        // coordinates; the ORIGINAL source keeps the token (goto-def-on-`BASEOP`
        // untouched). `docs/adr/macro-handling.md`.
        let plan = self.member_blocks.map(|f| crate::timings::phase("cpp.member_blocks", || f(parser, source)));
        PackContext { external, plan }
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
    ) -> Option<(tree_sitter::Tree, String, crate::cpp_reparse::SpliceMap, Vec<(String, String)>)> {
        let parse_input: &str = ctx.plan.as_ref().map(|p| p.blanked_source.as_str()).unwrap_or(source);
        let (src, map, recovered) = match self.transform {
            Some(t) => crate::timings::phase("cpp.transform", || t(parser, parse_input, &ctx.external)),
            None => (parse_input.to_string(), crate::cpp_reparse::SpliceMap::default(), Vec::new()),
        };
        let tree = parser.parse(&src, None)?;
        Some((tree, src, map, recovered))
    }

    /// Phase 5: post-remap, pre-assembly skeleton enrichment. Returns the
    /// macro-identity lane (`MacroDef`s) for phase 7 to stamp onto the built
    /// `FileAnalysis` — the skeleton itself has no field for it.
    fn enrich_skeleton(
        &self,
        skel: &mut crate::query_extract::SkeletonAnalysis,
        parser: &mut tree_sitter::Parser,
        source: &str,
        src: &str,
        map: &crate::cpp_reparse::SpliceMap,
        ctx: &PackContext,
    ) -> Vec<crate::file_analysis::MacroDef> {
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
            let body_refs = crate::cpp_reparse::macro_body_name_refs(parser, source, &known);
            for (name, span) in body_refs.name_refs {
                skel.var_reads.push((name, crate::file_analysis::ScopeId(0), span));
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
            let guards = crate::cpp_reparse::collect_include_guard_names(parser, source);
            if !guards.is_empty() {
                for s in skel.symbols.iter_mut() {
                    if s.kind == "var" && guards.contains(&s.name) {
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
            .filter(|(_, h)| matches!(h, crate::query_extract::MacroReturnHint::Param(_)))
            .map(|(n, _)| n.clone())
            .collect();
        if !param_macro_names.is_empty() {
            skel.macro_call_arg_spans =
                crate::cpp_reparse::macro_call_arg_spans(parser, source, &param_macro_names);
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
        macro_defs: Vec<crate::file_analysis::MacroDef>,
    ) {
        fa.macro_defs = macro_defs;
        apply_attribute_macros(fa, recovered);
        // Access-specifier regions: a fresh parse of the ORIGINAL source
        // (spans already in original coords, no remap needed) tags each
        // member symbol non-public when its declaration falls under
        // `private:`/`protected:`.
        if let Some(f) = self.access_regions {
            let regions = crate::timings::phase("cpp.access_regions", || f(parser, source));
            stamp_access_regions(fa, &regions);
        }
        // The file's include closure is the cross-file visibility key
        // (`ScopedLookup`). Computed here — the driver holds the path the
        // resolver needs; empty on-open until the header cache warms.
        let mut closure_incomplete = false;
        if let (Some(f), Some(p)) = (self.include_closure, path) {
            let (closure, complete) =
                crate::timings::phase("cpp.include_closure", || f(p, source));
            fa.include_closure = crate::file_analysis::path_intern::ClosureList::from_iter(
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
        id: "cpp",
        // `.c` too — tree-sitter-cpp parses C (a near-subset), and MISRA /
        // embedded code is C-heavy. One driver serves both.
        exts: &["cpp", "cc", "cxx", "hpp", "hh", "h", "c"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_cpp::LANGUAGE.into()).expect("cpp grammar");
            p
        },
        pack: crate::query_extract::cpp_pack,
        // reparse past the preprocessor before extraction; the anchor map
        // carries the recovered spans back to the original coordinates.
        transform: Some(crate::cpp_reparse::preprocess_validated_with),
        gather_macros: Some(crate::cpp_reparse::included_macros_pre_expanded),
        collect_macro_defs: Some(crate::cpp_reparse::collect_macro_defs),
        member_blocks: Some(crate::cpp_reparse::plan_member_blocks),
        include_closure: Some(crate::cpp_reparse::include_closure),
        input_fingerprint: Some(crate::cpp_reparse::toolchain_fingerprint),
        sniff: Some(crate::cpp_reparse::looks_like_c_family),
        access_regions: Some(crate::cpp_reparse::access_regions),
    }
}

#[cfg(feature = "python")]
fn python_driver() -> PackDriver {
    PackDriver {
        id: "python",
        exts: &["py"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_python::LANGUAGE.into()).expect("python grammar");
            p
        },
        pack: crate::query_extract::python_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        sniff: None,
        access_regions: None,
    }
}

#[cfg(feature = "r")]
fn r_driver() -> PackDriver {
    PackDriver {
        id: "r",
        exts: &["R", "r"],
        filenames: &[],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_r::LANGUAGE.into()).expect("r grammar");
            p
        },
        pack: crate::query_extract::r_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        sniff: None,
        access_regions: None,
    }
}

#[cfg(feature = "cmake")]
fn cmake_driver() -> PackDriver {
    PackDriver {
        // CMakeLists.txt (no extension match) is a follow-up; `.cmake` now.
        id: "cmake",
        exts: &["cmake"],
        filenames: &["CMakeLists.txt"],
        make_parser: || {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&tree_sitter_cmake::LANGUAGE.into()).expect("cmake grammar");
            p
        },
        pack: crate::query_extract::cmake_pack,
        transform: None,
        gather_macros: None,
        collect_macro_defs: None,
        member_blocks: None,
        include_closure: None,
        input_fingerprint: None,
        sniff: None,
        access_regions: None,
    }
}

/// Stamp attribute-macro signals onto recovered classes. For each
/// `(class_name, macro_token)` the declarator-macro strip recovered, look the
/// token up in the plugin-declared attribute-macro vocabulary; when known, add
/// its signal (`exported`/`deprecated`) to the class symbol's `attributes`.
/// The class is recovered either way (the strip is the unknown-macro safety
/// net) — only the SIGNAL is plugin-gated: core owns the recovery mechanism,
/// the plugin owns what the macro means (rule #10).
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
fn apply_attribute_macros(fa: &mut FileAnalysis, recovered: &[(String, String)]) {
    use crate::file_analysis::SymKind;
    if recovered.is_empty() {
        return;
    }
    let signals = crate::plugin::default_plugin_registry().attribute_macro_signals();
    for (class_name, macro_token) in recovered {
        let Some(signal) = signals.get(macro_token) else { continue };
        for sym in &mut fa.symbols {
            if matches!(sym.kind, SymKind::Class)
                && &sym.name == class_name
                && !sym.attributes.contains(signal)
            {
                sym.attributes.push(signal.clone());
            }
        }
    }
}

/// Inject the member-block synthetic bases + parent edges into the extracted
/// skeleton (`docs/adr/macro-handling.md`, "Member-block macros = roles"). The
/// macro's own `#define` symbol is reclassified Variable → Class (the navigable
/// base), members are minted under it (package = the macro), and each member
/// re-sources the SAME `TypeName` edge the expanded field would have. The
/// existing ancestor walk (`resolve_method_in_ancestors` / `parents_of`) then
/// delivers `o->op_type` resolution / hover / the references splat — no parallel
/// field resolution. Spans are already in ORIGINAL coordinates.
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
/// Type each function-like macro from its body: delegation (`#define F(x)
/// G(x)`) reuses the see-through target as a value edge, else a param-
/// independent body type (`#define SQ(x) ((x)*(x))` → Numeric). First def wins
/// per name (a config-variant macro's arms are a later union tier). Object-like
/// macros are skipped — their value/type lanes ride edges, not the sub-return
/// path.
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
fn macro_return_hints(
    macro_defs: &[crate::file_analysis::MacroDef],
    parser: &mut tree_sitter::Parser,
) -> Vec<(String, crate::query_extract::MacroReturnHint)> {
    use crate::query_extract::MacroReturnHint;
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
                crate::cpp_reparse::classify_param_return(parser, &m.body, params)
                    .map(MacroReturnHint::Param)
                    .or_else(|| {
                        crate::cpp_reparse::classify_body_type(parser, &m.body)
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

#[cfg_attr(not(feature = "cpp"), allow(dead_code))]
fn inject_member_blocks(
    skel: &mut crate::query_extract::SkeletonAnalysis,
    plan: &crate::cpp_reparse::MemberBlockPlan,
    annot_type: fn(&str) -> Option<crate::file_analysis::InferredType>,
) {
    use crate::file_analysis::{InferredType, Scope, ScopeId, ScopeKind};
    use crate::query_extract::SkelSymbol;
    use crate::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

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
            if s.kind == "var" && s.name == base.macro_name && s.package.is_none() {
                s.kind = "class".to_string();
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
        });
        skel.scope_count = skel.scopes.len();
        for m in &base.members {
            // Field kind + deref_stack + the explicit-annotation witness below:
            // the SAME payload a plainly-declared struct field carries, so no
            // renderer (hover stars, inlay suppression, `*field*` labeling) can
            // tell a macro-pasted member from a directly-declared one (rule #10).
            skel.symbols.push(SkelSymbol {
                kind: "field".to_string(),
                name: m.name.clone(),
                start: m.name_span.start,
                end: m.name_span.end,
                name_start: m.name_span.start,
                name_end: m.name_span.end,
                package: Some(base.macro_name.clone()),
                scope: scope_id,
                return_type: None,
                deref_stack: m.deref_stack.clone(),
                attributes: Vec::new(),
                arity: None,
                qualifier_owned: false,
            });
            // The role member emits the SAME `TypeName` edge an expanded field
            // does — the edge is canonical (the hover leaf + the type chase
            // resolve `op_type` → `unsigned short`). Tagged `ANNOT_SOURCE` (the
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
                    source: WitnessSource::Builder(crate::witnesses::ANNOT_SOURCE.into()),
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
fn stamp_access_regions(fa: &mut FileAnalysis, regions: &[crate::cpp_reparse::AccessRegion]) {
    use crate::file_analysis::Span;
    let contains = |o: &Span, i: &Span| {
        (o.start.row, o.start.column) <= (i.start.row, i.start.column)
            && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
    };
    for sym in &mut fa.symbols {
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
        if non_public && !sym.attributes.iter().any(|a| a == "non_public") {
            sym.attributes.push("non_public".to_string());
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
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
fn emit_external_type_aliases(
    witnesses: &mut Vec<crate::witnesses::Witness>,
    external: &crate::cpp_reparse::PreExpandedExternal,
    annot_type: fn(&str) -> Option<crate::file_analysis::InferredType>,
) {
    use crate::file_analysis::Span;
    use tree_sitter::Point;
    for (name, body) in external.object_like_macros() {
        let body = body.trim();
        if !crate::query_extract::looks_like_type_spelling(body) {
            continue;
        }
        witnesses.push(crate::witnesses::Witness {
            attachment: crate::witnesses::WitnessAttachment::TypeName(name.to_string()),
            source: crate::witnesses::WitnessSource::Builder("external-macro-alias".into()),
            payload: crate::query_extract::type_alias_payload(body, annot_type),
            span: Span { start: Point { row: 0, column: 0 }, end: Point { row: 0, column: 0 } },
        });
    }
}

/// Phase 7: the two CHAIN-FUEL gaps (`docs/PARKED.md`) — cpp's
/// implicit-return inference and the implicit `this->field` read that
/// feeds it. Reads the FINAL `FileAnalysis` (stable SymbolIds, resolved
/// refs) plus `return_sites` (the skeleton's purely structural "a `return`
/// happened here" record — `query_extract.rs` doesn't know what a return
/// MEANS for any language; this function is the cpp-semantic reading of it).
///
/// An `auto`-returning function/method has no declared-return witness (the
/// writeback inside `into_file_analysis` only fires when the syntax carries
/// a type), so `fa.witnesses.for_attachment(Symbol(sid))` being empty IS
/// the "undeclared" signal — reading the bag's own state rather than a
/// private skeleton field. For each qualifying site: one
/// `SymbolReturnArm(sid) → Edge(Expr(return_span))` witness plus one
/// `Symbol(sid) → Edge(SymbolReturnArm(sid))` chain witness, mirroring
/// Perl's `Builder::publish_return_arm_witnesses` — `SymbolReturnArmFold`
/// (`witnesses.rs`) already folds multi-arm agreement generically.
///
/// A bare identifier that resolves to no local var (an unresolved
/// `RefKind::Variable`) but names a field of its enclosing class
/// (`Scope::package`) is an implicit `this->field` read (`return inner_;`
/// with no explicit receiver) — mints the same `Expr(span) →
/// Edge(Variable{field, field's own scope})` edge the field's own
/// declared-type witness already resolves through, so the read chases the
/// general Variable path instead of dead-ending. General-purpose (any bare
/// field read, not gated on return position — rule #10). The same gate also
/// pins a bare sibling-method CALL (`foo()` = `this->foo()`) to the enclosing
/// class. Whether a bare name CAN elide `this->` — for members OR methods — is
/// a language fact the pack declares (`implicit_this_members`): true for C/C++,
/// false for Python/R where the receiver is mandatory.
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
fn emit_return_fuel(
    fa: &mut FileAnalysis,
    return_sites: &[(crate::file_analysis::ScopeId, crate::file_analysis::Span)],
    implicit_this_members: bool,
) {
    use crate::file_analysis::{RefKind, ScopeId, ScopeKind, SymKind, SymbolId};
    use crate::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
    use std::collections::HashMap;

    let scope_parent: HashMap<ScopeId, Option<ScopeId>> =
        fa.scopes.iter().map(|s| (s.id, s.parent)).collect();
    // A Sub/Method's body scope (`@scope.sub`) is minted on the SAME
    // `function_definition` node as its `@def.sub`/`@def.method` — same
    // span, different query pattern — so span equality joins scope → Symbol.
    let scope_to_symbol: HashMap<ScopeId, SymbolId> = fa
        .scopes
        .iter()
        .filter(|s| matches!(s.kind, ScopeKind::Sub { .. }))
        .filter_map(|s| {
            fa.symbols
                .iter()
                .find(|sym| matches!(sym.kind, SymKind::Sub | SymKind::Method) && sym.span == s.span)
                .map(|sym| (s.id, sym.id))
        })
        .collect();
    for (ret_scope, ret_span) in return_sites {
        let owner = std::iter::successors(Some(*ret_scope), |sc| {
            scope_parent.get(sc).copied().flatten()
        })
        .find_map(|sc| scope_to_symbol.get(&sc).copied());
        let Some(sid) = owner else { continue };
        if !fa.witnesses.for_attachment(&WA::Symbol(sid)).is_empty() {
            continue; // a declared return already carries its own witness
        }
        fa.witnesses.push(Witness {
            attachment: WA::SymbolReturnArm(sid),
            source: WitnessSource::Builder("cpp_return_arm".into()),
            payload: WP::Edge(WA::Expr(*ret_span)),
            span: *ret_span,
        });
        fa.witnesses.push(Witness {
            attachment: WA::Symbol(sid),
            source: WitnessSource::Builder("cpp_return_arm_chain".into()),
            payload: WP::Edge(WA::SymbolReturnArm(sid)),
            span: *ret_span,
        });
    }

    if !implicit_this_members {
        return;
    }
    let scope_package: HashMap<ScopeId, Option<String>> =
        fa.scopes.iter().map(|s| (s.id, s.package.clone())).collect();
    let field_scope: HashMap<(String, String), ScopeId> = fa
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Field))
        .filter_map(|s| s.package.clone().map(|p| ((p, s.name.clone()), s.scope)))
        .collect();
    let implicit_field_edges: Vec<(crate::file_analysis::Span, String, ScopeId)> = fa
        .refs
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Variable) && r.resolves_to.is_none())
        .filter_map(|r| {
            let class = scope_package.get(&r.scope)?.as_ref()?;
            let fscope = *field_scope.get(&(class.clone(), r.target_name.clone()))?;
            Some((r.span, r.target_name.clone(), fscope))
        })
        .collect();
    for (span, name, fscope) in implicit_field_edges {
        fa.witnesses.push(Witness {
            attachment: WA::Expr(span),
            source: WitnessSource::Builder("cpp_implicit_field_read".into()),
            payload: WP::Edge(WA::Variable { name, scope: fscope }),
            span,
        });
    }

    // Sibling method CALLs — the call half of the same implicit-`this`
    // fact the field pass covers. A bare `foo(...)` inside a method body is
    // `this->foo(...)` when the enclosing class declares a `foo` method; C++
    // name lookup finds the member before any free function of that name.
    // Pinning the call's `resolved_package` to the enclosing class routes it
    // through the SAME package-scoped callable resolution a qualified
    // `Class::foo()` uses (`package_scoped_callable`), so goto-def /
    // references / rename all land on the sibling. A name with no matching
    // member is left untouched — a free-function-only call still resolves
    // free.
    //
    // The enclosing class is the enclosing method SYMBOL's `package`, NOT the
    // ref's scope package: an out-of-line body (`void Buf<T>::reserve(…)`) is
    // lexically at file scope, so its body scope carries no package, but the
    // peeled method symbol does (`Buf`). Reading it off the symbol covers
    // in-class AND out-of-line, template or not, with one rule.
    let class_methods: std::collections::HashSet<(String, String)> = fa
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Method))
        .filter_map(|s| s.package.clone().map(|p| (p, s.name.clone())))
        .collect();
    // Each Sub/Method body scope → its owning class (the peeled symbol
    // package). Precomputed so the enclosing-class walk reads no `fa` borrow,
    // leaving `fa.refs` free to mutate below.
    let scope_class: HashMap<ScopeId, String> = scope_to_symbol
        .iter()
        .filter_map(|(&sc, &sid)| {
            fa.symbols
                .iter()
                .find(|s| s.id == sid)
                .and_then(|s| s.package.clone())
                .map(|p| (sc, p))
        })
        .collect();
    let class_of = |sc: ScopeId| -> Option<String> {
        std::iter::successors(Some(sc), |s| scope_parent.get(s).copied().flatten())
            .find_map(|s| scope_class.get(&s).cloned())
    };
    let sibling_pins: Vec<(usize, String)> = fa
        .refs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            if !matches!(r.kind, RefKind::FunctionCall { resolved_package: None }) {
                return None;
            }
            let class = class_of(r.scope)?;
            class_methods
                .contains(&(class.clone(), r.target_name.clone()))
                .then_some((i, class))
        })
        .collect();
    for (i, class) in sibling_pins {
        if let RefKind::FunctionCall { resolved_package } = &mut fa.refs[i].kind {
            *resolved_package = Some(class);
        }
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
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
fn remap_spans(
    skel: &mut crate::query_extract::SkeletonAnalysis,
    transformed: &str,
    original: &str,
    map: &crate::cpp_reparse::SpliceMap,
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
    let rspan = |sp: crate::file_analysis::Span| -> crate::file_analysis::Span {
        let (start, end) = remap_span(sp.start, sp.end);
        crate::file_analysis::Span { start, end }
    };

    let crate::query_extract::SkeletonAnalysis {
        symbols,
        refs,
        imports: _,
        import_sites,
        scope_count: _,
        scopes,
        witnesses,
        parents: _,
        var_reads,
        label_refs,
        receiver_names: _,
        flow_edges,
        moved_from,
        control_regions,
        param_regions,
        domain_sites,
        macro_returns: _,
        // Populated in enrich_skeleton (post-remap) already in original coords.
        macro_call_arg_spans: _,
        call_sites,
        specializations: _,
        // name-keyed, ordered by byte position pre-remap — no spans to fix.
        template_params: _,
        return_sites,
        param_sigs,
        // Populated later (enrich_skeleton) already in original coords — no remap.
        macro_body_member_reads: _,
    } = skel;

    for s in symbols.iter_mut() {
        let crate::query_extract::SkelSymbol {
            kind: _,
            name: _,
            start,
            end,
            name_start,
            name_end,
            package: _,
            scope: _,
            return_type: _,
            deref_stack: _,
            attributes: _,
            arity: _,
            qualifier_owned: _,
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
        let crate::query_extract::SkelRef {
            kind: _,
            name: _,
            start,
            end,
            scope: _,
            invocant,
            member_op,
            arg_count: _,
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
    for (_, _, span) in var_reads.iter_mut() {
        *span = rspan(*span);
    }
    // Call-site spans feed the call-value edge (`into_file_analysis`, after
    // this remap) and must speak original coords like the flow-edge source
    // (the same call span) they land beside.
    for (span, _) in call_sites.iter_mut() {
        *span = rspan(*span);
    }
    for (_, span) in return_sites.iter_mut() {
        *span = rspan(*span);
    }
    for sc in scopes.iter_mut() {
        sc.span.start = r(sc.span.start);
        sc.span.end = r(sc.span.end);
    }
    // `#include` path tokens — goto-def on the token is span-keyed.
    for (_, span) in import_sites.iter_mut() {
        *span = rspan(*span);
    }
    // Witness spans (the type tier). A length-changing splice (`PBF op_type` →
    // `unsigned op_type`) shifts every span AFTER it, so a declared-type
    // witness left in transformed coords lands past the original query point
    // and the temporal fold drops it. Remap the witness `.span`, any span-
    // bearing attachment (`Expr`/`BranchArm`), and the same shapes reached
    // through a payload edge target — so `expr_type_at_span` and the temporal
    // ordering all speak original coordinates, like refs.
    use crate::witnesses::{WitnessAttachment, WitnessPayload};
    let remap_att = |a: &mut WitnessAttachment| match a {
        WitnessAttachment::Expr(sp) | WitnessAttachment::BranchArm(sp) => *sp = rspan(*sp),
        _ => {}
    };
    for w in witnesses.iter_mut() {
        remap_att(&mut w.attachment);
        match &mut w.payload {
            WitnessPayload::Edge(t)
            | WitnessPayload::CallReturn { target: t, .. }
            | WitnessPayload::QualifiedCallReturn { method_lookup: t, .. }
            | WitnessPayload::Projected { base: t, .. } => remap_att(t),
            _ => {}
        }
        w.span = rspan(w.span);
    }
    // Value-flow edges (the provenance tier above the bag) + label/goto refs +
    // moved-from sites + domain-typing sites all carry transformed spans too.
    for fe in flow_edges.iter_mut() {
        let crate::file_analysis::FlowEdge {
            target_name: _,
            target_scope: _,
            target_at,
            source,
            extraction: _,
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
        let crate::file_analysis::DomainSite { slot: _, value: _, slot_span } = ds;
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
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
fn mint_erased_macro_reads(
    skel: &mut crate::query_extract::SkeletonAnalysis,
    original: &str,
    transformed: &str,
    map: &crate::cpp_reparse::SpliceMap,
    plan: Option<&crate::cpp_reparse::MemberBlockPlan>,
) {
    use crate::file_analysis::{ScopeId, Span};
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
        let mut best: Option<crate::file_analysis::Span> = None;
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
#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
struct LineIndex {
    starts: Vec<usize>,
}

#[cfg(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake"))]
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

/// The drivers this binary was compiled to serve. Perl always; pack
/// languages per feature.
pub struct LanguageRegistry {
    drivers: Vec<Box<dyn LanguageDriver>>,
}

impl LanguageRegistry {
    pub fn with_enabled() -> Self {
        #[cfg_attr(
            not(any(feature = "cpp", feature = "python", feature = "r", feature = "cmake")),
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
        LanguageRegistry { drivers }
    }

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

    /// `for_path`, falling back to a content sniff when no driver claims the
    /// extension. `source` is the file text the caller
    /// already has in hand — no extra I/O. Perl never sniffs (it's the
    /// default fallback the caller uses when this also returns `None`), so
    /// only pack drivers get a vote.
    pub fn for_path_sniffed(&self, path: &Path, source: &str) -> Option<&dyn LanguageDriver> {
        if let Some(d) = self.for_path(path) {
            return Some(d);
        }
        let mut cut = source.len().min(1024);
        while cut > 0 && !source.is_char_boundary(cut) {
            cut -= 1;
        }
        let prefix = &source[..cut];
        self.drivers.iter().find(|d| d.id() != "perl" && d.sniff(prefix)).map(|d| d.as_ref())
    }

    /// Configured language ids — what this distribution serves.
    pub fn languages(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|d| d.id()).collect()
    }

    /// Human-facing name for a pack language id, for startup banners and
    /// progress messages. Purely cosmetic — `for_id` still speaks the short
    /// id everywhere else. Falls back to the id itself for a language this
    /// mapping hasn't been told about yet (never a hard error over a
    /// display string).
    pub fn display_name(id: &str) -> &str {
        match id {
            "cpp" => "C/C++",
            "python" => "Python",
            "r" => "R",
            "cmake" => "CMake",
            _ => id,
        }
    }

    /// Union of every served language's completion trigger characters,
    /// for the LSP `completionProvider.triggerCharacters` slot.
    pub fn trigger_chars(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for d in &self.drivers {
            for c in d.trigger_chars() {
                if !out.iter().any(|s| s == c) {
                    out.push((*c).to_string());
                }
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "language_driver_tests.rs"]
mod tests;
