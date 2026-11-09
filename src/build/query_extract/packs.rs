//! `LangPack`: what a language declares to the query extractor — its
//! query document plus the minimal host predicates patterns can't express.
//! Each language's declaration lives in `build/packs/<lang>.rs`.

use super::*;
use crate::model::file_analysis::NameSpellings;

/// Per-language bundle: the query pack plus host predicates. The
/// predicates are the official escape hatch — kept
/// MINIMAL on purpose so the findings honestly measure how far
/// patterns alone go.
pub struct LangPack {
    /// The language's id — the driver it serves under and the
    /// `FileAnalysis::language` its assembly stamps.
    pub id: &'static str,
    pub query_source: &'static str,
    /// How the language spells names — its namespace separator and its
    /// variable sigils. Baked onto `PackFacts::names`; every key function
    /// reads it there.
    pub names: NameSpellings,
    /// Where a bare function call finds its function, registered by id
    /// with the lattice: the extractor names each free call by it and
    /// resolution looks across files by it, so no file carries it.
    pub functions: FunctionLookup,
    /// Which cross-family types the language's joins fold into one another
    /// (`docs/adr/union-types.md`). Registered by language id
    /// (`conventions::register_conventions`).
    pub lattice: crate::model::file_analysis::LatticeRules,
    /// The language's word for an engine type tag on a human surface
    /// (`Bool` → `bool`), registered by id with the lattice; `None` keeps
    /// the engine's.
    pub type_display: fn(tag: &str) -> Option<&'static str>,
    /// What a construction's reference to its constructor is keyed by,
    /// registered by id with the lattice: resolution reads it, no file.
    pub construction: crate::model::file_analysis::ConstructionKey,
    /// The namespaces the language's classes hold their members in,
    /// registered by id with the lattice. Its names are the document's
    /// `@space.<name>` vocabulary.
    pub member_spaces: crate::model::file_analysis::MemberSpaces,
    /// Which classes a protected member reaches beyond its own, registered
    /// by id with the lattice: access checks read it, no file.
    pub family_reach: crate::model::file_analysis::FamilyReach,
    /// Shape a captured name token's text into the identity it is filed
    /// under (cpp canonicalizes template spellings). Applied to every
    /// captured name alike.
    pub shape_name: fn(raw: &str) -> String,
    /// Name for defs with no name token (anonymous subs, anonymous
    /// classes), given where the def or its `@def.<kind>.anchor` starts. A
    /// kind whose instances must stay distinct spells the position in;
    /// a structure-only default (`(anon)`, `(union)`) ignores it.
    pub default_name: fn(kind: DefCap, at: Point) -> Option<String>,
    /// Map a `@type.annot` token's text to a type — the pack predicate
    /// for languages whose ring 3 is partly in the tree (`x: int`).
    pub annot_type: fn(text: &str) -> Option<InferredType>,
    /// The documentation a `@doc` comment's text states (php's docblock
    /// description, its decoration and tags stripped), or `None` when it
    /// says nothing. Which comments document which declarations is the
    /// document's.
    pub doc_text: fn(comment: &str) -> Option<String>,
    /// Module-name → workspace-relative candidate paths — the entire
    /// per-language cross-file resolution strategy ("the one executable
    /// line"). Python: `pkg.mod` → pkg/mod.py | pkg/mod/__init__.py.
    /// The Index-layer consumer is future work; the cross-file spike tests
    /// (`resolve_imports_with_pack` in query_extract_tests.rs) drive it today.
    #[allow(dead_code)]
    pub module_paths: fn(module: &str) -> Vec<String>,
    /// Symbols the runtime enters from OUTSIDE the source graph (C/C++
    /// `main`: reached through the ABI, never a source call site) — a
    /// zero-fan-in callable with one of these names is alive by contract.
    /// Empty for languages whose entry is the file itself (Perl, Python
    /// scripts). Consumed by the heatmap's reachability guard — asked of
    /// the pack, never a name/language branch (rule #10).
    pub entrypoint_symbols: &'static [&'static str],
    /// Container membership (class/struct/union/namespace) is delimited by
    /// literal `{`/`}` in the source, so a member that lost its enclosing
    /// container to a tree-sitter misparse can be re-anchored by matching the
    /// container's body braces on the ORIGINAL source
    /// (`reanchor_truncated_containers`). True for C/C++; false for
    /// indentation-scoped (Python) or non-nesting packs.
    /// `docs/adr/config-superposition-declarations.md`.
    pub brace_scoped_members: bool,
    /// Completion trigger characters for the LSP
    /// `completionProvider.triggerCharacters` slot — the client auto-fires
    /// completion (and reports the char in `CompletionContext`) when one is
    /// typed. C++ `. > :` cover `.`/`->`/`::`; the member path keys off them.
    pub trigger_chars: &'static [&'static str],
    /// The cursor's receiver peel over the sentinel-reparsed tree:
    /// transparent wrapper kinds (`(*p)`, `(&o)`, `(p)` → `p`) dropped,
    /// through their first named child. Extraction reads the document's
    /// `@expr.wrapper` edge instead and never consults this.
    pub recv_peel: &'static [&'static str],
    /// Simple-variable node kinds (`identifier`): the cursor sentinel's "is
    /// this receiver a bare variable" test, read off the raw node it climbs
    /// to like its other node-kind tables. Extraction never reads it: the
    /// document states a written-name receiver as `@member.recv.name`.
    pub simple_var_kinds: &'static [&'static str],
    /// Node kinds the sentinel must NOT splice into (string/char/comment).
    pub skip_kinds: &'static [&'static str],
    /// Equality-comparison node kinds (`binary_expression`) whose operand
    /// may be a domain-typed field — the type-constrained-completion slot
    /// (`o->op_type == |` ranks the field's DOMAIN members first,
    /// `docs/adr/cursor-slots.md`). The operand order is either side; the
    /// slot is the member-access operand, the value the other. Paired with
    /// `domain_compare_ops` so a `<`/`+` binary never opens the slot. Empty
    /// = no domain-comparison completion.
    pub domain_compare_kinds: &'static [&'static str],
    /// The operator tokens (`==`, `!=`) that make a `domain_compare_kinds`
    /// node a domain comparison — the pack owns which operators mean
    /// "equality against a domain value" (rule #10). Empty = feature off.
    pub domain_compare_ops: &'static [&'static str],
}

impl LangPack {
    /// What every file of this language shares, registered by its id.
    pub fn conventions(&self) -> crate::model::conventions::LanguageConventions {
        crate::model::conventions::LanguageConventions::of_pack(
            self.lattice.clone(),
            self.type_display,
            self.construction,
            self.member_spaces.clone(),
            self.functions,
            self.family_reach,
        )
    }

    /// Every `&'static str` this pack declares, tagged with the field it
    /// came from: what the rule #15 tripwires walk.
    ///
    /// The destructure names every field, so a new `LangPack` field fails to
    /// compile here until its strings are declared or it is named as
    /// holding none. `query_source` is the document itself and `names` is
    /// the language's spelling seam. A fn is code the reflection cannot see
    /// into, so a callee name inside one escapes it
    /// (`docs/adr/pack-vocabulary.md`).
    #[cfg(test)]
    pub(crate) fn declared_strings(&self) -> Vec<(&'static str, &'static str)> {
        let LangPack {
            id: _,
            query_source: _,
            names: _,
            functions: _,
            lattice: _,
            type_display: _,
            construction: _,
            // The document's own namespace words, checked against its
            // `@space.<name>` captures (`bundled_documents_speak_the_vocabulary`).
            member_spaces: _,
            family_reach: _,
            shape_name: _,
            default_name: _,
            annot_type: _,
            doc_text: _,
            module_paths: _,
            entrypoint_symbols,
            brace_scoped_members: _,
            trigger_chars,
            recv_peel,
            simple_var_kinds,
            skip_kinds,
            domain_compare_kinds,
            domain_compare_ops,
        } = self;
        let mut out: Vec<(&'static str, &'static str)> = Vec::new();
        let mut list = |field: &'static str, values: &'static [&'static str]| {
            out.extend(values.iter().map(|v| (field, *v)));
        };
        list("entrypoint_symbols", entrypoint_symbols);
        list("trigger_chars", trigger_chars);
        list("recv_peel", recv_peel);
        list("simple_var_kinds", simple_var_kinds);
        list("skip_kinds", skip_kinds);
        list("domain_compare_kinds", domain_compare_kinds);
        list("domain_compare_ops", domain_compare_ops);
        out.retain(|(_, v)| !v.is_empty());
        out
    }
}

/// Translate a member's declared return type into the deferred
/// receiver-substituting `ReturnExpr` when it MENTIONS one of the owning
/// class's template params: a bare param (`T get()`) becomes
/// `ParamOf(i, Receiver)`; a param one hop under a template spelling
/// (`vector<T> all()`) becomes `InstanceOf { base, args }` with the
/// param positions deferred and the literal positions baked. `None` when
/// no param occurs — the concrete-return path handles it.
pub(super) fn param_return_expr(
    ret: &InferredType,
    params: &[String],
) -> Option<crate::model::witnesses::ReturnExpr> {
    use crate::model::witnesses::{ParametricOp, ReturnExpr};
    match ret {
        InferredType::ClassName(n) => {
            params.iter().position(|p| p == n).map(|i| {
                ReturnExpr::Operator(ParametricOp::ParamOf {
                    index: i as u32,
                    of: Box::new(ReturnExpr::Receiver),
                })
            })
        }
        InferredType::Parametric(p) => match p {
            crate::model::file_analysis::ParametricType::ResultSet { .. }
            | crate::model::file_analysis::ParametricType::Row { .. } => None,
            crate::model::file_analysis::ParametricType::Instance { base, args } => {
                if !args.iter().any(|a| param_return_expr(a, params).is_some()) {
                    return None;
                }
                let exprs = args
                    .iter()
                    .map(|a| {
                        param_return_expr(a, params)
                            .unwrap_or_else(|| ReturnExpr::Concrete(a.clone()))
                    })
                    .collect();
                Some(ReturnExpr::Operator(ParametricOp::InstanceOf {
                    base: base.clone(),
                    args: exprs,
                }))
            }
        },
        _ => None,
    }
}

pub use crate::model::conventions::FunctionLookup;
