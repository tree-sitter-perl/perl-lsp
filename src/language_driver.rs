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
    /// Module name → workspace-relative candidate paths.
    fn module_paths(&self, module: &str) -> Vec<String>;
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
    fn module_paths(&self, module: &str) -> Vec<String> {
        vec![format!("{}.pm", module.replace("::", "/"))]
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
    include_closure: Option<fn(&Path, &str) -> Vec<String>>,
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
    fn analyze_with_path(&self, source: &str, path: Option<&Path>) -> FileAnalysis {
        let mut parser = (self.make_parser)();
        // Cross-file macros from #included headers (C++), so a macro
        // #defined elsewhere (SPDLOG_NAMESPACE_BEGIN) expands here.
        let external = match (self.gather_macros, path) {
            (Some(g), Some(p)) => {
                crate::timings::phase("cpp.gather", || g(p, source, &mut parser))
            }
            _ => std::sync::Arc::new(crate::cpp_reparse::PreExpandedExternal::empty()),
        };
        // Member-block macros as roles: BLANK the standalone-in-struct-body uses
        // so `struct op { BASEOP };` parses clean, and mint the synthetic base +
        // parent edges (injected below, into the extracted skeleton). The blank
        // is length-preserving, so the transform + remap stay in original
        // coordinates; the ORIGINAL source keeps the token (goto-def-on-`BASEOP`
        // untouched). `docs/adr/macro-handling.md`.
        let plan = self
            .member_blocks
            .map(|f| crate::timings::phase("cpp.member_blocks", || f(&mut parser, source)));
        let parse_input: &str = plan.as_ref().map(|p| p.blanked_source.as_str()).unwrap_or(source);
        let (src, map, recovered) = match self.transform {
            Some(t) => crate::timings::phase("cpp.transform", || t(&mut parser, parse_input, &external)),
            None => (parse_input.to_string(), crate::cpp_reparse::SpliceMap::default(), Vec::new()),
        };
        let Some(tree) = parser.parse(&src, None) else { return FileAnalysis::new(Default::default()) };
        match crate::query_extract::extract(&tree, src.as_bytes(), &(self.pack)()) {
            Ok(mut skel) => {
                // remap extracted spans from transformed → original coords
                // (no-op for identity / pass-through languages).
                remap_spans(&mut skel, &src, source, &map);
                // Type-alias `#define`s gathered from the include closure ride
                // into this file's bag as `TypeName` witnesses (span-less, so
                // post-remap is fine): the cross-file chase can't index a
                // gitignored generated header (`config.h`'s `U16TYPE`), but the
                // gather reached it — so carry the alias here.
                emit_external_type_aliases(&mut skel.witnesses, &external, (self.pack)().annot_type);
                // Member-block roles: inject the synthetic bases + parent edges
                // (original coords) into the skeleton, so the ONE ancestor walk
                // resolves `o->op_type` / hover / the references splat. Must run
                // AFTER remap (the injected spans are already original) and BEFORE
                // `into_file_analysis` (it builds indices over everything).
                if let Some(plan) = &plan {
                    inject_member_blocks(&mut skel, plan, (self.pack)().annot_type);
                }
                // Expanded / blanked macro USES vanish from the parsed text,
                // so no query capture can ref them — re-mint each as a
                // variable read at its ORIGINAL span (the splice map + the
                // member-block blank diff know every site), so find-references
                // on a macro reaches uses the expansion erased (rule #7/#9).
                mint_erased_macro_reads(&mut skel, source, &map, plan.as_ref());
                // Macro identity lane: collect every `#define` off the ORIGINAL
                // source (spans in user coordinates, no splice remap needed).
                let macro_defs = self
                    .collect_macro_defs
                    .map(|collect| collect(&mut parser, source))
                    .unwrap_or_default();
                // Function-like macro typing (the expansion flip's payoff): a
                // left-unexpanded macro call parses as `call_expression`, so the
                // macro is a package-global sub. Type it from its body — delegation
                // reuses the see-through target, else a param-independent body type
                // — and hand `into_file_analysis` the hints to lower onto the
                // final `SymbolId`s. `docs/adr/macro-handling.md`.
                skel.macro_returns = macro_return_hints(&macro_defs, &mut parser);
                let mut fa = skel.into_file_analysis();
                fa.macro_defs = macro_defs;
                apply_attribute_macros(&mut fa, &recovered);
                // The file's include closure is the cross-file visibility key
                // (`ScopedLookup`). Computed here — the driver holds the path the
                // resolver needs; empty on-open until the header cache warms.
                if let (Some(f), Some(p)) = (self.include_closure, path) {
                    fa.include_closure = crate::timings::phase("cpp.include_closure", || f(p, source));
                }
                fa
            }
            Err(_) => FileAnalysis::new(Default::default()),
        }
    }
    fn module_paths(&self, module: &str) -> Vec<String> {
        ((self.pack)().module_paths)(module)
    }
    fn lang_pack(&self) -> Option<crate::query_extract::LangPack> {
        Some((self.pack)())
    }
    fn trigger_chars(&self) -> &[&'static str] {
        (self.pack)().trigger_chars
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
            None => crate::cpp_reparse::classify_body_type(parser, &m.body)
                .map(MacroReturnHint::Concrete),
        };
        if let Some(hint) = hint {
            out.push((m.name.clone(), hint));
        }
    }
    out
}

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
            skel.symbols.push(SkelSymbol {
                kind: "var".to_string(), // a data member (Variable), package-tagged
                name: m.name.clone(),
                start: m.name_span.start,
                end: m.name_span.end,
                name_start: m.name_span.start,
                name_end: m.name_span.end,
                package: Some(base.macro_name.clone()),
                scope_depth: 1,
                scope: scope_id,
                return_type: None,
                deref_stack: Vec::new(),
            });
            // The role member emits the SAME `TypeName` edge the expanded field
            // did — the emission site moved, the edge is canonical (slice 2's
            // hover leaf + the type chase resolve `op_type` → `unsigned short`).
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
                    source: WitnessSource::Builder("member-block".into()),
                    payload,
                    span: m.name_span,
                });
            }
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

/// Remap extracted skeleton spans from transformed coords back to
/// original source coords via the anchor map. A no-op for an identity
/// map (clean/pass-through files round-trip byte→point→byte unchanged),
/// so it's safe to always call. Covers navigation spans (symbols / refs
/// / scopes); witness spans (type queries) are the follow-up.
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
    for s in &mut skel.symbols {
        s.start = r(s.start);
        s.end = r(s.end);
        s.name_start = r(s.name_start);
        s.name_end = r(s.name_end);
    }
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
    for rf in &mut skel.refs {
        (rf.start, rf.end) = remap_span(rf.start, rf.end);
    }
    for (_, _, span) in &mut skel.var_reads {
        (span.start, span.end) = remap_span(span.start, span.end);
    }
    for sc in &mut skel.scopes {
        sc.span.start = r(sc.span.start);
        sc.span.end = r(sc.span.end);
    }
    // Witness spans (the type tier). A length-changing splice (`PBF op_type` →
    // `unsigned op_type`) shifts every span AFTER it, so a declared-type
    // witness left in transformed coords lands past the original query point
    // and the temporal fold drops it. Remap the witness `.span`, any span-
    // bearing attachment (`Expr`/`BranchArm`), and the same shapes reached
    // through a payload edge target — so `expr_type_at_span` and the temporal
    // ordering all speak original coordinates, like refs.
    use crate::witnesses::{WitnessAttachment, WitnessPayload};
    let rspan = |sp: crate::file_analysis::Span| -> crate::file_analysis::Span {
        let (start, end) = remap_span(sp.start, sp.end);
        crate::file_analysis::Span { start, end }
    };
    let remap_att = |a: &mut WitnessAttachment| match a {
        WitnessAttachment::Expr(sp) | WitnessAttachment::BranchArm(sp) => *sp = rspan(*sp),
        _ => {}
    };
    for w in &mut skel.witnesses {
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
    // moved-from sites all carry transformed spans too.
    for fe in &mut skel.flow_edges {
        fe.target_at = r(fe.target_at);
        fe.source = rspan(fe.source);
    }
    for (_, _, span) in &mut skel.label_refs {
        *span = rspan(*span);
    }
    for (_, span, _) in &mut skel.moved_from {
        *span = rspan(*span);
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
    if sites.is_empty() {
        return;
    }
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

    /// Configured language ids — what this distribution serves.
    pub fn languages(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|d| d.id()).collect()
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
