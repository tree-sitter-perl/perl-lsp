//! `LangPack` — the pack CONTRACT: the query pack plus the minimal host
//! predicates patterns can't express. What each language declares lives
//! in `build::packs::<lang>`, re-exported here.

use super::*;
use crate::model::file_analysis::NameSpellings;

// The per-language declarations, re-exported so `query_extract::packs` stays
// the one path every caller spells. A build with no pack language compiled
// spells none of them.
#[allow(unused_imports)]
pub use crate::build::packs::*;


/// Per-language bundle: the query pack plus host predicates. The
/// predicates are the official escape hatch — kept
/// MINIMAL on purpose so the findings honestly measure how far
/// patterns alone go.
pub struct LangPack {
    /// The base skeleton query. Bundled overlays (`bundled_overlays`) are
    /// appended at assembly, each test-compiled alone first, so one broken
    /// document drops with a diagnostic instead of taking the language out.
    pub query_source: &'static str,
    /// Bundled framework/stdlib overlays: (document name, source).
    pub bundled_overlays: &'static [(&'static str, &'static str)],
    /// The registry's language id (`"php"`, `"cpp"`, ...) — keys pack-plugin
    /// query overlays (`<plugin-dir>/<name>/queries/<lang_id>.scm`,
    /// docs/prompt-pack-plugins.md) onto the language they extend.
    pub lang_id: &'static str,
    /// Bundled framework-entry declarations (`entry.json` documents, see
    /// `EntryMarker`): which attribute names / method conventions mean "a
    /// runner invokes this" for the heatmap's framework-entry guard. The
    /// framework vocabulary lives in these DATA files (like the bundled
    /// `.scm` overlays), never in engine code; plugin dirs extend the set.
    pub bundled_entry_markers: &'static [&'static str],
    /// Rail documents (`rails.json`): text rails — string-named uses a
    /// grammar cannot see (a Blade template's `route('x')`), scanned as
    /// text into `DispatchCall` refs on the named rail.
    pub bundled_rail_docs: &'static [&'static str],
    /// The language's WRITE and DISPLAY spellings — what a quick-fix
    /// inserts and what a human surface renders. Per-language constants,
    /// so an analysis carries the pointer and every consumer reaches them
    /// by language id (rule #14); `PackSpellings::NONE` = declares none.
    pub spellings: &'static crate::model::file_analysis::PackSpellings,
    /// How the language spells names — its namespace separator and its
    /// variable sigils. Baked onto `PackFacts::names`; every key function
    /// reads it there.
    pub names: NameSpellings,
    /// Shape a captured name token's text (e.g. keep the sigil on a
    /// Perl variable). `capture_kind` is the vocabulary name
    /// (`def.var`, `ref.method`, ...) so one pack hook serves all.
    pub shape_name: fn(capture_kind: &str, raw: &str) -> String,
    /// Name for defs with no name token (anonymous subs, anonymous
    /// classes), given the def's 0-based start position: a kind whose
    /// instances must stay distinct (php's anonymous classes — two per test
    /// file is normal) spells the position in; structure-only defaults
    /// (`(anon)`, `(union)`) ignore it. The spelling must be
    /// identifier-shaped: the name rides the bareword-class lanes.
    pub default_name: fn(kind: &str, row: usize, col: usize) -> Option<String>,
    /// Map a `@type.annot` token's text to a type — the pack predicate
    /// for languages whose ring 3 is partly in the tree (`x: int`).
    pub annot_type: fn(text: &str) -> Option<InferredType>,
    /// A `@rettype` spelling as ONE deferred return shape: a concrete type,
    /// or the RECEIVER placeholder for the late-bound spellings (php
    /// `static`/`$this`/`self`) that make fluent builders chain. Text in,
    /// structure out — the engine never branches on the spelling itself
    /// (rule #10), and the writeback publishes what comes back.
    pub declared_return: fn(text: &str) -> Option<crate::model::witnesses::ReturnExpr>,
    /// Documentation-comment type facts (phpdoc `@return`/`@param`/`@var`):
    /// the pack parses ITS OWN doc vocabulary out of a `@doc.comment`
    /// capture's text, returning type spellings `annot_type` speaks.
    /// The engine joins each comment to the def directly below it and
    /// fills ONLY where the syntax declared nothing — declared types win
    /// (docblocks drift). Empty = no doc lane.
    pub doc_types: fn(text: &str, uses_method_tags: &[&str]) -> Vec<DocFact>,
    /// Docblock tags whose argument NAMES a sibling method a framework
    /// runner will invoke (`@dataProvider providerRows`). Data, not a
    /// literal in the reader: the tag is one framework's word, exactly
    /// like the attribute spellings the entry documents carry, and the
    /// reader is the engine's. Empty = no such tag.
    ///
    /// TODO: one framework's vocabulary, and that framework already has a
    /// bundled entry document (`queries/php/frameworks/phpunit.entry.json`)
    /// which is where framework vocabulary lives. The tag belongs in it, as
    /// a field the entry loader hands to `doc_types` — then a plugin dir can
    /// teach the doc lane a runner tag without a recompile.
    pub doc_uses_method_tags: &'static [&'static str],
    /// Module-name → workspace-relative candidate paths — the entire
    /// per-language cross-file resolution strategy ("the one executable
    /// line"). Python: `pkg.mod` → pkg/mod.py | pkg/mod/__init__.py.
    /// The Index-layer consumer is future work; the cross-file spike tests
    /// (`resolve_imports_with_pack` in query_extract_tests.rs) drive it today.
    #[allow(dead_code)]
    pub module_paths: fn(module: &str) -> Vec<String>,
    /// Languages where imports are CALLS, not statements (R's
    /// library()/source()): the module an `@import.call.<kind>` argument
    /// names. The KIND is the capture's suffix — which callees import is the
    /// document's — and this maps the argument text the kind carries.
    pub import_module: fn(kind: &str, arg: &str) -> Option<String>,
    /// The refinement a narrowed subject's type TEXT denotes: the
    /// `@narrow.type` capture where the guard names one
    /// (`dynamic_cast<Derived*>`), else the subject's DECLARED type, which
    /// an engagement guard peels (`std::optional<T>` → `T`). Text in,
    /// structure out — which guards narrow is the document's `#eq?`.
    /// `None` = this spelling refines nothing.
    pub narrow_type: fn(type_text: &str) -> Option<InferredType>,
    /// Container membership (class/struct/union/namespace) is delimited by
    /// literal `{`/`}` in the source, so a member that lost its enclosing
    /// container to a tree-sitter misparse can be re-anchored by matching the
    /// container's body braces on the ORIGINAL source
    /// (`reanchor_truncated_containers`). True for C/C++; false for
    /// indentation-scoped (Python) or non-nesting packs.
    /// `docs/adr/config-superposition-declarations.md`.
    pub brace_scoped_members: bool,
    /// Bundled builtin-type documents (`builtins.txt`): the class, interface
    /// and attribute names the language itself provides in its global
    /// namespace, one per line. A global reference to one of these is never a
    /// type missing its import. A runtime's surface grows and differs per
    /// build, so it is a document a plugin dir extends, never a table
    /// (rule #15) — read through `builtin_types_for`.
    pub bundled_builtin_types: &'static [&'static str],
    /// Members every enum carries by language rule (php: `->value`,
    /// `->name`, `::cases()`, `::from()`, `::tryFrom()`). PRODUCER-only
    /// data: the extractor mints each as a SYNTHESIZED member at every enum
    /// declaration, and every consumer resolves it like any other member —
    /// nothing downstream reads this list, so no consumer matches the names.
    ///
    /// TODO: still five token texts a plugin dir cannot extend, two fields
    /// below the builtin-class list that IS a document. The replacement is
    /// the same shape — `queries/<lang>/enum-members.txt`, one name per
    /// line with its callable-ness, read through the `builtins.txt` reader
    /// — and it retires this field's rule #15 allowlist entry with it.
    pub enum_members: &'static [EnumMember],
    /// Completion trigger characters for the LSP
    /// `completionProvider.triggerCharacters` slot — the client auto-fires
    /// completion (and reports the char in `CompletionContext`) when one is
    /// typed. C++ `. > :` cover `.`/`->`/`::`; the member path keys off them.
    pub trigger_chars: &'static [&'static str],
}

impl LangPack {
    /// Every `&'static str` this pack DECLARES, tagged with the field it
    /// came from — the reflection the rule #15 tripwires walk.
    ///
    /// Hand-written and exhaustive on purpose: the destructure below makes
    /// a new `LangPack` field a compile error here until its strings are
    /// declared, which is what stops a fresh table of node kinds from
    /// arriving unwatched. The DOCUMENT fields (`query_source`, the bundled
    /// overlays, the entry markers, the rail docs, the builtin-type lists)
    /// are the documents themselves, `names` is the language's own spelling seam,
    /// and `lang_id` is a registration — none is a vocabulary this rule
    /// governs, so none is yielded.
    #[allow(dead_code)] // the rule #15 tripwires are its only caller
    pub(crate) fn declared_strings(&self) -> Vec<(&'static str, &'static str)> {
        let LangPack {
            query_source: _,
            bundled_overlays: _,
            lang_id: _,
            bundled_entry_markers: _,
            bundled_rail_docs: _,
            spellings: _,
            names: _,
            shape_name: _,
            default_name: _,
            annot_type: _,
            declared_return: _,
            doc_types: _,
            doc_uses_method_tags,
            module_paths: _,
            import_module: _,
            narrow_type: _,
            brace_scoped_members: _,
            bundled_builtin_types: _,
            enum_members,
            trigger_chars,
        } = self;
        let mut out: Vec<(&'static str, &'static str)> = Vec::new();
        fn list(
            out: &mut Vec<(&'static str, &'static str)>,
            field: &'static str,
            values: &'static [&'static str],
        ) {
            out.extend(values.iter().map(|v| (field, *v)));
        }
        list(&mut out, "doc_uses_method_tags", doc_uses_method_tags);
        out.extend(enum_members.iter().map(|m| ("enum_members", m.name)));
        list(&mut out, "trigger_chars", trigger_chars);
        out.retain(|(_, v)| !v.is_empty());
        out
    }
}


/// One member the LANGUAGE gives every enum of a language. Read at
/// extraction and nowhere else — the mint turns it into a real member.
#[derive(Debug, Clone, Copy)]
pub struct EnumMember {
    pub name: &'static str,
    /// A callable (php `::cases()`), as against a value read (`->value`).
    /// Decides which member kind the synthesis mints, so a call and a read
    /// of the same name can never answer for each other.
    pub callable: bool,
}

/// One type fact parsed from a documentation comment (`LangPack::doc_types`).
/// The type is a raw spelling the pack has already normalized to what its
/// `annot_type` accepts (generics stripped, `X|null` collapsed to `X`).
#[derive(Debug, Clone)]
pub enum DocFact {
    /// `@return T` — the documented return of the def below the comment.
    Return(String),
    /// `@param T $name` — a documented parameter type; `name` carries the
    /// language's own spelling (php keeps the `$`).
    Param { name: String, ty: String },
    /// `@var T [$name]` — the documented type of the property/variable
    /// below (or, with a `$name`, of that specific local — the inline
    /// `/** @var Type[] $rows */` idiom above an assignment).
    Var { ty: String, name: Option<String> },
    /// A `LangPack::doc_uses_method_tags` row naming a sibling METHOD a
    /// framework runner will invoke (`@dataProvider providerRows`). The
    /// join mints a real method reference (invocant = the enclosing class)
    /// on the fact's own line, so the named method gains fan-in and rename
    /// reaches the row.
    UsesMethod { name: String, line: usize, col: usize },
    /// `@method [static] T name(...)` on a CLASS docblock — a documented
    /// virtual method (Laravel facades, Eloquent's `__call` surface). The
    /// join synthesizes a real method symbol on the class below, spanning
    /// the fact's own `@method` line (`line` = 0-based offset within the
    /// comment) so each row is a distinct, honest gd target.
    Method { name: String, ret: Option<String>, line: usize, col: usize },
    /// `@deprecated [text]` — the declaration is deprecated; the text is
    /// what the diagnostic shows.
    Deprecated(Option<String>),
    /// `@template T [of X]` on a CLASS docblock — a declared generic
    /// parameter, in row order (`line` is the ordering key). Feeds the
    /// SAME per-class `template_params` axis cpp templates use, so a
    /// method whose `@return` names the param publishes `ParamOf(i)`
    /// through the existing writeback (Eloquent's `Builder<TModel>`).
    Template { name: String, line: usize },
    /// `@return Base<static|self|$this>` — the return is an instance of
    /// `base` PARAMETRIZED BY THE RECEIVER (`Model::query()` returns
    /// `Builder<static>`): the join publishes
    /// `Operator(InstanceOf{base, [Receiver]})`, so `Book::query()`
    /// carries `Builder<Book>` and a later `->first()` (`@return
    /// TModel`) projects `Book` back out.
    ReturnRecvInstance { base: String },
    /// The comment's summary paragraph — every line before the first
    /// `@tag`, joined; the text hover shows under the signature.
    Description(String),
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
            crate::model::file_analysis::ParametricType::ResultSet { .. } => None,
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

/// `member.op.<which>` suffix → the operator it names. ENGINE-side
/// vocabulary like `lit_type`: the suffix set names the model's `MemberOp`,
/// and a pack chooses which token carries each.
pub(super) fn member_op_suffix(suffix: &str) -> Option<crate::model::file_analysis::MemberOp> {
    use crate::model::file_analysis::MemberOp;
    match suffix {
        "arrow" => Some(MemberOp::Arrow),
        "dot" => Some(MemberOp::Dot),
        _ => None,
    }
}

/// `expr.lit.<t>` suffix → type. ENGINE-side vocabulary, not per-pack:
/// the suffix set names the engine's value lattice, packs just choose
/// which nodes carry each suffix.
pub(super) fn lit_type(suffix: &str) -> Option<InferredType> {
    match suffix {
        "string" => Some(InferredType::String),
        "number" => Some(InferredType::Numeric),
        "bool" => Some(InferredType::Bool),
        "arrayref" => Some(InferredType::ArrayRef),
        "hashref" => Some(InferredType::HashRef),
        _ => None,
    }
}

