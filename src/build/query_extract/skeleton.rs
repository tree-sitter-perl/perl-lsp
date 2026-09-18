//! The skeleton data model: symbol/ref rows and `SkeletonAnalysis`,
//! including its projection into `FileAnalysis`.

use crate::model::file_analysis::SymbolFlags;
use super::*;

/// The skeleton's symbol row — deliberately stringly-kinded: the kind
/// vocabulary comes from capture names, so the driver never enumerates
/// language entity kinds.
#[derive(Debug, Clone)]
pub struct SkelSymbol {
    pub kind: String,
    pub name: String,
    pub start: Point,
    pub end: Point,
    pub name_start: Point,
    pub name_end: Point,
    /// Sticky `@context.package` value in force at the def site.
    pub package: Option<String>,
    pub scope: crate::model::file_analysis::ScopeId,
    /// The declared return as ONE deferred shape (`@rettype` through the
    /// pack's `declared_return`, or a docblock row where the syntax carried
    /// nothing): a concrete type, the receiver placeholder for php's
    /// `static`/`$this`/`self`, or `InstanceOf{base, [Receiver]}` for
    /// `@return Base<static>`. The writeback publishes it.
    pub declared_return: Option<crate::model::witnesses::ReturnExpr>,
    /// The return annotation as the language writes it (`: string`), minted
    /// from the same `@rettype` capture through the pack's own
    /// `return_annotation_template`. `None` where the pack writes no return
    /// annotations, so a prefix-typed language (C) mints nothing to append.
    pub return_annotation: Option<String>,
    /// Pointer/reference declarator stack, unravelled by `peel_nested` from
    /// a `@nested.target` capture (empty otherwise). Flows to `Symbol.deref_stack`.
    pub deref_stack: Vec<crate::model::file_analysis::DerefStep>,
    /// Structural markers carried onto `Symbol.attributes`: "anonymous" when
    /// the name came from the pack's `default_name` (not addressable by name —
    /// completion skips it), "union" for union containers (the hover-overlay /
    /// outline-nesting key), stamped in `into_file_analysis` from the kind.
    pub attributes: Vec<String>,
    /// Declared parameter arity for a callable, counted structurally from the
    /// def's parameter list (`@arity.sig`). `None` for non-callables and defs
    /// whose parameter list the query didn't capture. Flows to `Symbol.arity`.
    pub arity: Option<crate::model::file_analysis::ParamArity>,
    /// The callable's declared parameters, in source order — joined from the
    /// same `@arity.sig` list as `arity` and flowed to `SymbolDetail::Sub`.
    /// Empty for non-callables and for a signature the query didn't capture.
    pub params: Vec<crate::model::file_analysis::ParamInfo>,
    /// The other symbol this ONE declaration token minted (php's promoted
    /// constructor property and its parameter). Minted as a pair where the
    /// query captures both, so no consumer re-derives the relation from
    /// spans (`Symbol::declared_with`, rule #11).
    pub declared_with: Option<crate::model::file_analysis::SymbolId>,
    /// The `package` came from an explicit `::` qualifier on an out-of-line def
    /// (`Ret Class::m(){}`), not from lexical/sticky context. The class the
    /// qualifier names is authoritative EVEN when its body lives in another file
    /// (a header), so `reanchor_truncated_containers` must not re-attribute it to
    /// the enclosing namespace. Not serialized — a driver-internal marker.
    pub qualifier_owned: bool,
    /// Documentation text joined from the comment directly above the def
    /// (`DocFact::Description`); flows to `Presentation.doc`.
    pub doc: Option<String>,
    /// `@deprecated` text (or `Some(None)`-less: the attribute form has
    /// no text) — present iff the symbol carries the `deprecated` attribute.
    pub deprecation: Option<String>,
    /// Declaration facts the extractor minted from a CAPTURE rather than
    /// from a written attribute token — a receiver parameter, a
    /// constructor. Or-ed onto the flags the kind and the attributes give,
    /// so the two mints never race for one bit.
    pub flags: SymbolFlags,
}

#[derive(Debug, Clone)]
pub struct SkelRef {
    pub kind: String,
    pub name: String,
    pub start: Point,
    pub end: Point,
    pub scope: crate::model::file_analysis::ScopeId,
    /// For a member access (`recv.field`) — the receiver subtree's (span,
    /// text), so `into_file_analysis` mints a `RefKind::MethodCall` whose
    /// invocant types query-time via `expr_type_at_span(span)` (text → the
    /// `InvocantName`). `None` for plain calls / var refs.
    pub invocant: Option<(crate::model::file_analysis::Span, String)>,
    /// For a `"dispatch"` ref (`@ref.dispatch.named.<rail>`): the dispatching
    /// function's name (`do_action`, `apply_filters`) — the `RefKind::
    /// DispatchCall::dispatcher` label. `None` for every other kind.
    pub via: Option<String>,
    /// The written member operator (`.`/`->`) + its span, named by the
    /// document's own `@member.op.<which>` capture, `Some` only when the
    /// IMMEDIATE receiver is a simple variable. Rides onto the MethodCall ref
    /// so operator-correctness is a ref query, not a separate walk.
    pub member_op: Option<(crate::model::file_analysis::MemberOp, crate::model::file_analysis::Span)>,
    /// Written argument count at a call ref (`call`/`qcall`/`member`), counted
    /// structurally from the argument list. Flows to `Ref.arg_count`; `None`
    /// for non-call refs.
    pub arg_count: Option<usize>,
    /// A member token read as a VALUE (no argument list follows it): mints
    /// `RefKind::FieldAccess`; a callable member mints `MethodCall`
    /// (`docs/adr/member-kinds.md`). Only member refs set it.
    pub value_read: bool,
    /// Named by a string literal (`[$obj, 'method']`) — see
    /// `RefKind::MethodCall::named_by_string`.
    pub named_by_string: bool,
}

/// A string array key and the element it heads — nesting is span
/// containment among elements.
#[derive(Debug, Clone)]
pub struct KeyDef {
    pub key: String,
    pub key_span: crate::model::file_analysis::Span,
    pub elem_span: crate::model::file_analysis::Span,
}

#[derive(Debug, Default)]
pub struct SkeletonAnalysis {
    pub symbols: Vec<SkelSymbol>,
    pub refs: Vec<SkelRef>,
    pub imports: Vec<String>,
    /// `#include`/`import` rows: the path-token span, the raw text, and what
    /// the row binds. Goto-def on the token resolves the header; the span and
    /// the binding are what the bare `imports` list drops. Carried onto
    /// `FileAnalysis.pack.include_directives` unchanged.
    pub import_sites: Vec<crate::model::file_analysis::ImportRow>,
    /// `use A\B as C` rows: (alias, namespace, real leaf). Carried onto
    /// `FileAnalysis.pack.use_aliases`.
    pub use_aliases: Vec<(String, String, String)>,
    /// Class spellings written WITH a qualifier: (leaf, written prefix).
    /// Carried onto `FileAnalysis.pack.qualified_spellings`.
    pub qualified_spellings: Vec<(String, String)>,
    pub scope_count: usize,
    pub scopes: Vec<crate::model::file_analysis::Scope>,
    pub witnesses: Vec<crate::model::witnesses::Witness>,
    /// (child class, parent class) inheritance edges — `@parent` captures.
    pub parents: Vec<(String, String)>,
    /// (specialization, primary) family edges — a `@spec.primary` capture in
    /// a class-def match whose name is a template spelling. Rides onto
    /// `FileAnalysis.pack.specializes`; the graph's `Specializes` edge derives from
    /// it (member resolution never traverses it — a spec REPLACES wholesale).
    pub specializations: Vec<(String, String)>,
    /// (owner class, param name, param position) triples from
    /// `@tmpl.owner`/`@tmpl.param` — one per template parameter, joined per
    /// match. Sorted by position into `FileAnalysis.pack.template_params`
    /// (declaration order is the `ParamOf` index axis).
    pub template_params: Vec<(String, String, usize)>,
    /// Variable reads (`@expr.read.var`): (name, scope, span). Each resolves
    /// to the nearest visible Variable declaration by lexical scope walk →
    /// local goto-def + hover. Resolution runs in `into_file_analysis`.
    pub var_reads: Vec<(String, crate::model::file_analysis::ScopeId, crate::model::file_analysis::Span)>,
    /// `goto LABEL` refs (`@ref.label`): (name, scope, span). Resolve to the
    /// `LABEL:` def (a Variable symbol from `@def.label`) function-wide —
    /// scope-chain walk WITHOUT the declared-before constraint (a forward
    /// goto is valid C).
    pub label_refs: Vec<(String, crate::model::file_analysis::ScopeId, crate::model::file_analysis::Span)>,
    /// Member-access field uses recovered from inside `#define` macro bodies:
    /// `(field name, span)`. A macro body is one opaque `preproc_arg` token, so
    /// a `->op_next` buried in a def has no query capture. `into_file_analysis`
    /// resolves each against THIS file's field symbols and — when the name maps
    /// to a UNIQUE declaring class — mints a class-frozen `MethodCall` ref, so
    /// references on the field include the in-body use (rule #7). The receiver
    /// is a macro parameter with no type, hence the class is frozen from the
    /// field decl rather than inferred from the (untypeable) invocant.
    pub macro_body_member_reads: Vec<(String, crate::model::file_analysis::Span)>,
    /// The pack that produced this skeleton. Set on the analysis so a
    /// consumer reaching a language's spellings by id (rule #14) finds them
    /// on an analysis the driver has not stamped yet.
    pub lang_id: &'static str,
    /// `@ref.var.implicit` — spans whose variable read the RUNTIME binds
    /// (php `$this`, the superglobals). The minted ref carries
    /// `RefBinding::Runtime`, so the undefined-variable lane never sees an
    /// unbound read instead of consulting a name list.
    pub runtime_bound_reads: Vec<Span>,
    /// Member tokens on the left of an assignment (dynamic property sites).
    pub member_writes: Vec<Span>,
    /// Whole import-statement spans (`use A\B;` rows), for the insertion
    /// point of an import quick-fix.
    pub import_rows: Vec<Span>,
    /// The language's write/display spellings (`LangPack::spellings`),
    /// carried to `PackFacts` as a pointer — per-language constants, never
    /// copied per file (rule #14).
    pub spellings: Option<&'static crate::model::file_analysis::PackSpellings>,
    /// rail → the parameter separator a use's name ends at (`rails.json`).
    pub rail_name_seps: Vec<(String, String)>,
    /// The rails the pack's documents declare class-keyed (`names_are`).
    pub class_named_rails: Vec<String>,
    /// Expression spans whose value an overlay declared (`@expr.annot`) —
    /// the callee-return edge is not minted for them.
    pub annot_expr_spans: Vec<crate::model::file_analysis::Span>,
    /// The last row of the file preamble (open tag, `declare` rows): an
    /// inserted import goes after it when no import or namespace anchors.
    pub preamble_end: Option<usize>,
    /// Imported names a doc comment mentions (`@var Foo`, `@throws Foo`,
    /// `@see Foo`): a use the tree never shows.
    pub doc_mentions: Vec<String>,
    /// The pack's `function_scoped_vars` fact (php) — drives the var
    /// unification pass in `into_file_analysis`.
    pub function_scoped_vars: bool,
    /// The pack's constructor-method names, riding to `PackFacts`.
    /// The language's name spellings (`LangPack::names`), baked onto
    /// `PackFacts::names`.
    pub names: crate::model::file_analysis::NameSpellings,
    /// Value-flow edges minted from `@flow` captures (`source → target`,
    /// extraction). Lowered to type witnesses here; carried onto the FA as the
    /// provenance tier.
    pub flow_edges: Vec<crate::model::file_analysis::FlowEdge>,
    /// Declarations whose docblock type contradicts the declared one; the
    /// declaration won, and the pair rides to the `doc-type-mismatch` hint.
    pub doc_disagreements: Vec<crate::model::file_analysis::DocDisagreement>,
    /// `std::move(x)` sites: (moved var name, move-call span, enclosing scope).
    /// A read of the var after the call and before its next rebind is a
    /// use-after-move bug (`FileAnalysis::use_after_move_reads`).
    pub moved_from: Vec<(String, crate::model::file_analysis::Span, crate::model::file_analysis::ScopeId)>,
    /// Control-flow construct spans (`if`/`while`/`for`/`switch`/ternary/preproc
    /// conditionals). The use-after-move check reads these to decide whether a
    /// move is straight-line in its enclosing scope (`use_after_move_reads`).
    pub control_regions: Vec<crate::model::file_analysis::Span>,
    /// Parameter-list spans (`@param.region`). The use-after-move check reads
    /// these to tell a moved parameter from a moved local (`use_after_move_reads`).
    pub param_regions: Vec<crate::model::file_analysis::Span>,
    /// Existence-probe argument spans (`@probe.region`); the member lanes
    /// stay silent inside them.
    pub probe_regions: Vec<crate::model::file_analysis::Span>,
    /// A callable's by-reference parameter positions: (its parameter-list
    /// span, the position, the parameter's variable name, the name token).
    /// Joined to the def symbol by the same containment as `param_sigs`, and
    /// minted as `Param{..} → Edge(Variable{param, body})` witnesses — the
    /// binding mode is a fact the callee's bag holds
    /// (`docs/adr/by-ref-binding.md`), never a bit on the arity.
    pub by_ref_params: Vec<(Span, u32, String, Span)>,
    /// Fold-only regions (`@fold` / `@fold.comment`, the bool = comment);
    /// joined with the scopes into `fold_ranges`.
    pub fold_regions: Vec<(crate::model::file_analysis::Span, bool)>,
    /// Rail-suffixed handler defs / dispatch refs (`@def.handler.named.<rail>`,
    /// `@ref.dispatch.named.<rail>`): the token span → the rail name. Read
    /// at mint time to give the Handler / DispatchCall a `HandlerOwner::Rail`.
    pub rails: Vec<(crate::model::file_analysis::Span, String)>,
    /// Class-keyed rails (`@def.handler.class.<rail>` / `.by.<rail>`,
    /// `@ref.dispatch.class.<rail>`): token span → rail name. The owner is
    /// the same `HandlerOwner::Rail`; what the rail's names DENOTE is the
    /// document's own declaration (`docs/adr/laravel-rails.md` §Identity).
    pub class_rails: Vec<(crate::model::file_analysis::Span, String)>,
    /// Array-key DEF candidates (`@def.handler.key` on a string key, its
    /// element on `@key.elem`): promoted to rail names by the driver when
    /// the file's path rail says so (`config/app.php` → `app.<key>`).
    pub key_defs: Vec<KeyDef>,
    /// Domain-typing sites: a `@domain.slot` field access compared/assigned
    /// against a `@domain.value` token. Raw (value's enum resolves cross-file
    /// at query time); folds onto `Field{owner, name}` for the int-used-as-enum
    /// domain (`op_type` → `opcode`).
    pub domain_sites: Vec<crate::model::file_analysis::DomainSite>,
    /// Function-like macros the driver typed from their bodies (the expansion
    /// flip's payoff: a left-unexpanded macro call is a `call_expression`, so
    /// the macro IS a package-global sub the sub-return path types). Resolved to
    /// final `SymbolId`s in `into_file_analysis`: the macro's `Symbol` gets its
    /// return witness, and each call site an `Expr → Edge(Symbol)` so the call
    /// reflects the body's type. `docs/adr/macro-handling.md`.
    pub macro_returns: Vec<(String, MacroReturnHint)>,
    /// Per-call-site argument spans for function-like macro calls, keyed by
    /// the call-expression span (original coords). Lets a `Param(n)` macro's
    /// call site edge `Expr(call) → Edge(Expr(arg_n))` so the parametric
    /// return chases the argument's own value witness. Populated by the
    /// driver's macro lane (it holds the tree); empty for languages with no
    /// macro model.
    pub macro_call_arg_spans: Vec<(Span, Vec<Span>)>,
    /// Call sites (`@expr.call`): (call-expression span, callee name). The
    /// call's VALUE is the callee's own resolution — resolved in
    /// `into_file_analysis` against the symbol table: a `Class` callee is a
    /// functional cast / constructor (→ an instance of that class), a
    /// callable (sub / function-like macro) flows its return, an unresolvable
    /// name yields NO witness (no name-case guess — `docs/adr/macro-handling.md`).
    pub call_sites: Vec<(Span, String)>,
    /// `return EXPR;` sites (`@expr.return.value`): (enclosing scope, the
    /// returned expression's span). Purely structural — this tier doesn't
    /// know what a `return` MEANS for any given language; the interpretation
    /// (join to an owning function, decide whether it needs implicit-return
    /// fuel) is cpp-specific and lives in `language_driver.rs`'s post-
    /// extraction pipeline (`emit_return_fuel`).
    pub return_sites: Vec<(crate::model::file_analysis::ScopeId, Span)>,
    /// Scopes in which a call to one of the pack's dynamic-argument /
    /// dynamic-variable marker names appeared, with the flag that call
    /// declares. The arity join walks each scope up to its owning callable
    /// (`Scope::owner`) and stamps the flag there, so the callable STATES the
    /// property and no consumer joins call spans to callable spans.
    pub dynamic_markers: Vec<(crate::model::file_analysis::ScopeId, SymbolFlags)>,
    /// Callable parameter arities and the parameters themselves, keyed by
    /// parameter_list span (`@arity.sig`). Associated to def symbols by span
    /// containment in `into_file_analysis`, which lands the parameters on the
    /// def's `SymbolDetail::Sub` — the same shape Perl's subs carry, so one
    /// reader serves both (signature help, hover, inlay hints).
    pub param_sigs: Vec<(
        crate::model::file_analysis::Span,
        crate::model::file_analysis::ParamArity,
        Vec<crate::model::file_analysis::ParamInfo>,
    )>,
}

/// What a function-like macro's return resolves to (`SkeletonAnalysis::
/// macro_returns`). `Delegate` reuses the existing see-through target
/// (`MacroDef::delegate`) as a value edge (F's return = G's return);
/// `Concrete` is a body-classified param-independent type.
#[derive(Debug, Clone)]
pub enum MacroReturnHint {
    Delegate(String),
    Concrete(InferredType),
    /// The body is (or reduces to, under paren/cast wrappers) the macro's
    /// `n`-th parameter — an identity/projection macro (`#define ID(x) (x)`,
    /// `#define SEL2(a,b) (b)`). The return is the call's `n`-th argument's
    /// type; the Symbol carries `ReturnExpr::Arg(n)` and each call site chases
    /// the argument's own `Expr` witness. `docs/adr/macro-handling.md`.
    Param(u32),
}

/// Locate a brace-delimited body: from byte `from`, the matching close of the
/// first top-level `{`, skipping line/block comments and string/char/raw-string
/// literals so their braces never miscount. Returns `(open, close)` byte
/// offsets, or `None` when a `;` or EOF is reached before any `{` (a forward
/// declaration — no body). The single trustworthy container-extent primitive
/// for the re-anchor pass, run on ORIGINAL source (balanced braces).
fn brace_body_extent(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let n = bytes.len();
    let mut i = from;
    let mut depth = 0i32;
    let mut open: Option<usize> = None;
    while i < n {
        match bytes[i] {
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                i += 2;
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            // Raw string `R"delim( ... )delim"` — a `R` immediately before the
            // quote, not part of a longer identifier. Its body is verbatim, so
            // braces inside must not count.
            b'"' if i > from
                && bytes[i - 1] == b'R'
                && !bytes.get(i.wrapping_sub(2)).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') =>
            {
                let dstart = i + 1;
                let mut j = dstart;
                while j < n && bytes[j] != b'(' && bytes[j] != b'"' {
                    j += 1;
                }
                if j < n && bytes[j] == b'(' {
                    let delim = &bytes[dstart..j];
                    let mut k = j + 1;
                    i = n;
                    while k < n {
                        if bytes[k] == b')'
                            && bytes[k + 1..].starts_with(delim)
                            && bytes.get(k + 1 + delim.len()) == Some(&b'"')
                        {
                            i = k + 2 + delim.len();
                            break;
                        }
                        k += 1;
                    }
                } else {
                    i = j; // malformed; resume scan
                }
            }
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'{' => {
                if open.is_none() {
                    open = Some(i);
                }
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return open.map(|o| (o, i));
                }
                i += 1;
            }
            b';' if open.is_none() => return None,
            _ => i += 1,
        }
    }
    None
}

impl SkeletonAnalysis {
    /// Re-anchor members that lost their enclosing container to a tree-sitter
    /// misparse — the re-anchor invariant (`docs/adr/config-superposition-declarations.md`).
    /// When a deep misparse truncates a `class_specifier`/`namespace` node
    /// (json.hpp's `basic_json`: a `#if` in ctor-initializer position closes
    /// the class ~4400 lines early), every member after the truncation becomes
    /// a sibling in the enclosing scope and loses its `package`. This recovers
    /// them positionally.
    ///
    /// MUST run on ORIGINAL-coordinate symbols (post-`remap_spans`): the C++
    /// macro-expansion transform unbalances braces (measured on json.hpp's
    /// `basic_json`: transformed 682/710 vs original 646/646), so only the
    /// ORIGINAL source's braces locate a container's true extent.
    ///
    /// Anti-fabrication: a container is only "computable" when its name span in
    /// the source spells its own name (macro-synthesized namespaces like
    /// `nlohmann`, whose span covers `NLOHMANN_JSON_NAMESPACE_BEGIN`, are
    /// excluded) and it has a real brace body. Re-anchoring is UPGRADE-ONLY —
    /// a symbol moves to the innermost container that textually encloses it
    /// only when its current package is None, an ancestor of that container, or
    /// a non-computable (macro/external) scope. A `::`-qualifier attribution
    /// (out-of-line def) names a container that does NOT enclose the symbol, so
    /// it is left untouched. No membership is invented: every target is a real
    /// declared container whose braces enclose the member.
    pub fn reanchor_truncated_containers(&mut self, source: &str) {
        let bytes = source.as_bytes();
        // Point → byte: tree-sitter columns ARE byte offsets within the line.
        let mut line_start = Vec::with_capacity(bytes.len() / 32 + 1);
        line_start.push(0usize);
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                line_start.push(i + 1);
            }
        }
        let pt = |p: Point| -> usize {
            line_start.get(p.row).copied().unwrap_or(bytes.len()) + p.column
        };

        // Computable containers: a declared class/union/namespace whose name
        // span in the source spells its name AND that opens a brace body.
        struct Container {
            name: String,
            open: usize,
            close: usize,
        }
        let mut containers: Vec<Container> = Vec::new();
        for s in &self.symbols {
            if !matches!(s.kind.as_str(), "class" | "union" | "package") {
                continue;
            }
            let (ns, ne) = (pt(s.name_start), pt(s.name_end));
            // A default-named container (php's anonymous class) is spelled
            // by its anchor token, not its name; the brace body follows
            // the anchor the same way.
            let anchored = s.attributes.iter().any(|a| a == "anonymous");
            if !anchored && source.get(ns..ne) != Some(s.name.as_str()) {
                continue; // macro-synthesized or shaped name: not textually locatable
            }
            if let Some((open, close)) = brace_body_extent(bytes, ne) {
                containers.push(Container { name: s.name.clone(), open, close });
            }
        }
        if containers.is_empty() {
            return;
        }

        for s in self.symbols.iter_mut() {
            // An explicit `::` qualifier is authoritative — the owning class it
            // names may live in a header not present here, so its absence as a
            // local container is NOT a truncation fall-through to recover.
            if s.qualifier_owned {
                continue;
            }
            let sb = pt(s.start);
            // innermost = the smallest container range strictly enclosing sb.
            let Some(t0) = containers
                .iter()
                .filter(|c| c.open < sb && sb < c.close)
                .min_by_key(|c| c.close - c.open)
            else {
                continue;
            };
            let upgrade = match &s.package {
                None => true,
                Some(p) if *p == t0.name => false,
                Some(p) => {
                    // A `::`-qualifier names a computable container that does
                    // NOT enclose this symbol (out-of-line def) — leave it. A
                    // non-computable scope (macro namespace) or an enclosing
                    // ancestor container is a truncation fall-through — upgrade.
                    let p_computable_here =
                        containers.iter().any(|c| c.name == *p && c.open < sb && sb < c.close);
                    let p_is_container = containers.iter().any(|c| c.name == *p);
                    !p_is_container || p_computable_here
                }
            };
            if upgrade {
                s.package = Some(t0.name.clone());
            }
        }
    }

    /// Assemble a REAL `FileAnalysis` — production model, production
    /// indices, production reducer registry behind every query — from
    /// nothing but capture events. The existence proof that the engine
    /// is language-agnostic above this seam.
    pub fn into_file_analysis(mut self) -> crate::model::file_analysis::FileAnalysis {
        let rails = std::mem::take(&mut self.rails);
        let class_rails = std::mem::take(&mut self.class_rails);
        let rail_name_seps = std::mem::take(&mut self.rail_name_seps);
        // Every rail is named: the extractor mints a handler def / dispatch
        // ref only from a rail-suffixed capture, so a token with no rail
        // entry cannot arise from any document — only from an extractor arm
        // that forgot to record one, or from a span join the transform's
        // remap broke. A default name here would file the token under a
        // namespace no overlay declared, which reads as a wrong answer;
        // minting nothing loses one fact and keeps the file analysable.
        let rail_owner = |span: Span| -> Option<crate::model::file_analysis::HandlerOwner> {
            let rail = class_rails.iter().chain(rails.iter()).find(|(s, _)| *s == span);
            debug_assert!(rail.is_some(), "a handler token names its rail");
            rail.map(|(_, rail)| crate::model::file_analysis::HandlerOwner::Rail(rail.clone()))
        };
        use crate::model::file_analysis::{
            FileAnalysis, FileAnalysisParts, SymKind, Symbol, SymbolDetail, SymbolId,
        };
        // Function-scoped variable unification (pack fact — php): every
        // assignment mints a var def, so one variable becomes an island
        // per assignment and a rename from any island rewrites a
        // fragment. Per (name, owning sub scope): the FIRST def is the
        // declaration, re-anchored to the sub scope so every use in
        // every block binds it through the chain; the rest demote to
        // WRITE references. Runs before anything reads `self.symbols`,
        // so the parallel-index passes below stay aligned.
        let mut var_rebind_refs: Vec<(String, crate::model::file_analysis::ScopeId, Span)> =
            Vec::new();
        if self.function_scoped_vars {
            use crate::model::file_analysis::ScopeKind;
            let owning_sub = |mut s: crate::model::file_analysis::ScopeId| {
                loop {
                    let sc = &self.scopes[s.0 as usize];
                    if matches!(sc.kind, ScopeKind::Sub { .. }) {
                        return s;
                    }
                    match sc.parent {
                        Some(p) => s = p,
                        None => return s,
                    }
                }
            };
            let mut first: std::collections::HashMap<
                (String, crate::model::file_analysis::ScopeId),
                usize,
            > = std::collections::HashMap::new();
            let mut keep = vec![true; self.symbols.len()];
            for (i, s) in self.symbols.iter().enumerate() {
                if s.kind != "var" {
                    continue;
                }
                let key = (s.name.clone(), owning_sub(s.scope));
                match first.entry(key) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(i);
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {
                        keep[i] = false;
                        var_rebind_refs.push((
                            s.name.clone(),
                            s.scope,
                            Span { start: s.name_start, end: s.name_end },
                        ));
                    }
                }
            }
            for ((_, owner), i) in first {
                self.symbols[i].scope = owner;
            }
            let mut it = keep.iter();
            self.symbols.retain(|_| *it.next().unwrap());
        }
        let names = self.names.clone();
        // A NAMED typedef `typedef struct N {...} N;` matches both the
        // struct_specifier and the type_definition → two `class N` AT THE
        // SAME SPAN (one node, two capture patterns — e.g. the bodied
        // pattern and the inheritance pattern both fire for `class Circle :
        // public Shape {...}`). Key on (name, name span) rather than name
        // alone: two class_specifiers that happen to share a
        // bare name at DIFFERENT spans (forward decl + definition, same
        // short name in different scopes) are genuinely distinct symbols and
        // must both survive — only an exact same-span re-capture is the
        // duplicate-emission bug. "class" and "union" are one type-kind
        // family (a bodied named union matches both the generic class
        // pattern and the union-tagged one); the union-tagged row wins at a
        // shared span so the "union" attribute survives.
        {
            type ClassKey = (String, (usize, usize), (usize, usize));
            let key_of = |s: &SkelSymbol| -> ClassKey {
                (
                    s.name.clone(),
                    (s.name_start.row, s.name_start.column),
                    (s.name_end.row, s.name_end.column),
                )
            };
            let union_keys: std::collections::HashSet<ClassKey> = self
                .symbols
                .iter()
                .filter(|s| s.kind == "union")
                .map(key_of)
                .collect();
            let mut seen = std::collections::HashSet::new();
            self.symbols.retain(|s| match s.kind.as_str() {
                "class" => !union_keys.contains(&key_of(s)) && seen.insert(key_of(s)),
                "union" => seen.insert(key_of(s)),
                _ => true,
            });
        }
        // A free function matches both the rettype-carrying and the rettype-free
        // `@def.sub` pattern (a type-less constructor/K&R def only the latter),
        // and a trailing-return function/method matches both its leading-`auto`
        // pattern and the trailing sibling — one node, two SkelSymbols. Keep
        // the one WITH a return type; dedup by the name span (same node →
        // identical span). Per kind, so a sub and a method never cross-dedup.
        {
            use std::collections::HashMap;
            let dedup_kinds = ["sub", "method"];
            // A synthesized member has no node of its own — several share
            // the container's name token by construction, so the "one node,
            // two patterns" rule below would keep exactly one of them.
            let from_a_node = |s: &SkelSymbol| !s.attributes.iter().any(|a| a == "synthesized");
            let mut best: HashMap<(&str, usize, usize, usize, usize), bool> = HashMap::new();
            for s in &self.symbols {
                if dedup_kinds.contains(&s.kind.as_str()) && from_a_node(s) {
                    let key = (s.kind.as_str(), s.name_start.row, s.name_start.column, s.name_end.row, s.name_end.column);
                    let has = s.declared_return.is_some();
                    best.entry(key).and_modify(|v| *v |= has).or_insert(has);
                }
            }
            let resolved: HashMap<(String, usize, usize, usize, usize), bool> = best
                .into_iter()
                .map(|((k, a, b, c, d), v)| ((k.to_string(), a, b, c, d), v))
                .collect();
            let mut kept: std::collections::HashSet<(String, usize, usize, usize, usize)> =
                Default::default();
            self.symbols.retain(|s| {
                if !dedup_kinds.contains(&s.kind.as_str()) || !from_a_node(s) {
                    return true;
                }
                let key = (s.kind.clone(), s.name_start.row, s.name_start.column, s.name_end.row, s.name_end.column);
                // Keep the rettype-bearing copy; if none has one, keep the first.
                if resolved.get(&key) == Some(&true) && s.declared_return.is_none() {
                    return false;
                }
                kept.insert(key)
            });
        }
        let mut bag = crate::model::witnesses::WitnessBag::default();
        for w in std::mem::take(&mut self.witnesses) {
            bag.push(w);
        }
        // Associate each callable def with its parameter arity: the def's OWN
        // parameter list is the one span-contained in the def and starting
        // earliest at/after the name token (a nested function-pointer decl or
        // lambda param list starts later). `@arity.sig` fires a separate match
        // from the def name, so this join is by span, not match_id.
        {
            let param_sigs = std::mem::take(&mut self.param_sigs);
            let after = |a: Point, b: Point| (a.row, a.column) >= (b.row, b.column);
            let by_ref_params = std::mem::take(&mut self.by_ref_params);
            for (i, s) in self.symbols.iter_mut().enumerate() {
                if !matches!(s.kind.as_str(), "sub" | "method") {
                    continue;
                }
                let sig = param_sigs
                    .iter()
                    .filter(|(sp, _, _)| {
                        after(sp.start, s.name_end)
                            && after(s.end, sp.end)
                            && after(sp.start, s.start)
                    })
                    .min_by_key(|(sp, _, _)| (sp.start.row, sp.start.column));
                s.arity = sig.map(|(_, ar, _)| *ar);
                s.params = sig.map(|(_, _, ps)| ps.clone()).unwrap_or_default();
                // The callable's body scope — the `Sub` scope its span opens
                // — names this symbol as its owner (`Scope::owner`), the one
                // hop from a scope to its parameters; the by-reference
                // positions then bind through it. The scope may open at the
                // def's own START, not after its name: a `function_definition`
                // carries its own body scope, so a definition's scope spans
                // the whole declaration while a prototype's spans only its
                // parameter list.
                let body = self
                    .scopes
                    .iter()
                    .filter(|sc| {
                        matches!(sc.kind, crate::model::file_analysis::ScopeKind::Sub { .. })
                            && after(sc.span.start, s.start)
                            && after(s.end, sc.span.end)
                    })
                    .min_by_key(|sc| (sc.span.start.row, sc.span.start.column))
                    .map(|sc| sc.id);
                if let Some(body) = body {
                    self.scopes[body.0 as usize].owner = Some(SymbolId(i as u32));
                }
                let (Some((sig_span, _, _)), Some(body)) = (sig, body) else { continue };
                for (_, index, pname, name_span) in
                    by_ref_params.iter().filter(|(sp, _, _, _)| sp == sig_span)
                {
                    bag.push(crate::model::witnesses::Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Param {
                            package: s.package.clone().unwrap_or_default(),
                            name: s.name.clone(),
                            index: *index,
                        },
                        source: crate::model::witnesses::WitnessSource::Builder("by_ref_param".into()),
                        payload: crate::model::witnesses::WitnessPayload::Edge(
                            crate::model::witnesses::WitnessAttachment::Variable {
                                name: pname.clone(),
                                scope: body,
                            },
                        ),
                        span: Span { start: name_span.start, end: name_span.start },
                    });
                }
            }
        }
        // A co-declared pair on ONE token is ONE entry in a listing: an
        // Eloquent relation's property stands on its method's own name
        // token, so the outline shows the method — the rule `CLASS_RAIL`
        // applies to a rail handler sitting on another symbol's token. The
        // promoted-ctor pair is not this shape (neither half is callable).
        let twin_hidden: std::collections::HashSet<usize> = self
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| !matches!(s.kind.as_str(), "method" | "sub"))
            .filter(|(_, s)| {
                s.declared_with.is_some_and(|t| {
                    self.symbols.get(t.0 as usize).is_some_and(|o| {
                        matches!(o.kind.as_str(), "method" | "sub") && o.name_start == s.name_start
                    })
                })
            })
            .map(|(i, _)| i)
            .collect();
        let mut symbols: Vec<Symbol> = self
            .symbols
            .iter()
            .enumerate()
            .map(|(i, s)| Symbol {
                id: SymbolId(i as u32),
                name: s.name.clone(),
                kind: match s.kind.as_str() {
                    "package" => SymKind::Package,
                    // "union": a named union TYPE (its members are its own).
                    "class" | "union" => SymKind::Class,
                    // "macro": a function-like `#define` — a real callable
                    // Sub everywhere (dispatch/completion/goto-def), tagged
                    // "macro" below so hover/labels say so.
                    "sub" | "anon" | "constant" | "macro" => SymKind::Sub,
                    // "reexport": `using Base::m;` in a class body — a Method
                    // in the class's API surface; the "reexport" attribute
                    // (added below) makes resolution see through it.
                    "method" | "reexport" => SymKind::Method,
                    // a plain struct/class data member — distinct from a
                    // local/global Variable so hover/outline say "field".
                    "field" => SymKind::Field,
                    // a named enum value — distinct from both Variable and
                    // Field.
                    "enumerator" => SymKind::Enumerator,
                    // a class-scoped compile-time constant (PHP `const`):
                    // Enumerator's outline/completion shape WITHOUT the
                    // parent-enum value typing (a const's value is its
                    // initializer, not the owning class).
                    "const" => SymKind::Enumerator,
                    // a string-named registration on a rail
                    // (`@def.handler.named.<rail>`): the model's Handler —
                    // same-named registrations stack.
                    "handler" => SymKind::Handler,
                    // "unionfield" (an inline union member-field container)
                    // stays Variable — its "union" attribute drives the
                    // outline-nesting branch keyed on SymKind::Variable below.
                    _ => SymKind::Variable,
                },
                span: Span { start: s.start, end: s.end },
                selection_span: Span { start: s.name_start, end: s.name_end },
                scope: s.scope,
                package: s.package.clone(),
                detail: if let Some(owner) = (s.kind == "handler")
                    .then(|| rail_owner(Span { start: s.name_start, end: s.name_end }))
                    .flatten()
                {
                    // A flat namespace (a rail): the string alone is the
                    // identity, no receiver.
                    SymbolDetail::Handler { owner, dispatchers: Vec::new(), params: Vec::new() }
                } else if matches!(s.kind.as_str(), "sub" | "method") {
                    // The signature the `@arity.sig` join read. Perl's subs
                    // carry theirs the same way, so signature help, hover and
                    // the outline's parameter suffix have ONE reader for both.
                    // Doc text stays on `presentation` (where the pack tier
                    // has always put it); arity stays on `Symbol.arity`, which
                    // `param_arity()` prefers.
                    SymbolDetail::Sub {
                        params: s.params.clone(),
                        is_method: s.kind == "method",
                        doc: None,
                        opaque_return: false,
                        is_constant: false,
                        lexical: false,
                        declared_return: s.return_annotation.clone(),
                    }
                } else {
                    SymbolDetail::None
                },
                namespace: crate::model::file_analysis::Namespace::Language,
                presentation: crate::model::file_analysis::Presentation {
                    // An include-guard `#define` is compilation plumbing,
                    // not a program entity — folded from listing views but
                    // still resolvable (rule #7). The listing verdict is
                    // stamped here so warm stub rebuilds mint it identically.
                    // A class-rail handler sits on another symbol's token (a
                    // listener's `handle`); the outline shows that one.
                    hide_in_outline: twin_hidden.contains(&i)
                        || symbol_flags_of(&s.kind, &s.attributes)
                            .intersects(SymbolFlags::INCLUDE_GUARD | SymbolFlags::CLASS_RAIL),
                    doc: s.doc.clone(),
                    deprecation: s.deprecation.clone(),
                    display: None,
                    label: None,
                },
                attributes: {
                    let mut a = s.attributes.clone();
                    // union containers carry the marker the hover-overlay /
                    // outline-nesting consumers key on — a value-borne
                    // property, never a name test.
                    if matches!(s.kind.as_str(), "union" | "unionfield") {
                        a.push("union".to_string());
                    }
                    if s.kind == "reexport" {
                        a.push("reexport".to_string());
                    }
                    if s.kind == "macro" {
                        a.push("macro".to_string());
                    }
                    a
                },
                flags: symbol_flags_of(&s.kind, &s.attributes) | s.flags,
                declared_with: s.declared_with,
                deref_stack: s.deref_stack.clone(),
                arity: s.arity,
            })
            .collect();
        // A callable that reads its arguments or materializes its variables
        // dynamically says so on its own symbol. The marker call recorded a
        // SCOPE; the first scope up its chain that owns a callable is the one
        // the property belongs to (`Scope::owner`, filled in the arity join
        // above) — a call at file level owns nothing and stamps nothing.
        for (scope, flag) in std::mem::take(&mut self.dynamic_markers) {
            let mut at = Some(scope);
            while let Some(id) = at {
                let Some(sc) = self.scopes.get(id.0 as usize) else { break };
                if let Some(owner) = sc.owner {
                    if let Some(sym) = symbols.get_mut(owner.0 as usize) {
                        sym.flags.insert(flag);
                    }
                    break;
                }
                at = sc.parent;
            }
        }
        // Tag a typedef-struct's members with its name. `typedef struct
        // {...} T;` names the type AFTER its body, so @context.class can't
        // reach the members (already walked). For each class, members
        // directly in its body scope that are still UNTAGGED inherit the
        // class name. Idempotent for normal classes (members already tagged
        // via @context.class are skipped).
        let class_bodies: Vec<(crate::model::file_analysis::ScopeId, String)> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Class))
            .filter_map(|c| {
                let cs = c.span;
                self.scopes
                    .iter()
                    .find(|s| {
                        s.span != cs
                            && (s.span.start.row, s.span.start.column)
                                >= (cs.start.row, cs.start.column)
                            && (s.span.end.row, s.span.end.column) <= (cs.end.row, cs.end.column)
                    })
                    .map(|s| (s.id, c.name.clone()))
            })
            .collect();
        for (body, cname) in class_bodies {
            for s in &mut symbols {
                if s.package.is_none() && s.scope == body {
                    s.package = Some(cname.clone());
                }
            }
        }

        // Enum members carry their parent enum as BOTH container (package)
        // and type: `enum Color { RED }` → RED's package + type is `Color`,
        // so hover renders `RED: Color` the same `name: type` way a struct
        // field does. The enum is the smallest Class whose span contains the
        // enumerator (span-, not scope-contained: C enumerators leak into the
        // enclosing scope, so they stay there — the query mints no @scope for
        // the enum body — and a bare `x = RED` still resolves).
        {
            use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
            let class_spans: Vec<(Span, String)> = symbols
                .iter()
                .filter(|s| matches!(s.kind, SymKind::Class))
                .map(|s| (s.span, s.name.clone()))
                .collect();
            let contains = |o: &Span, i: &Span| {
                (o.start.row, o.start.column) <= (i.start.row, i.start.column)
                    && (i.end.row, i.end.column) <= (o.end.row, o.end.column)
            };
            for idx in 0..symbols.len() {
                if self.symbols[idx].kind != "enumerator" {
                    continue;
                }
                let esp = symbols[idx].span;
                // Innermost containing Class = the parent enum (latest start).
                let Some(enum_name) = class_spans
                    .iter()
                    .filter(|(cs, _)| contains(cs, &esp))
                    .max_by_key(|(cs, _)| (cs.start.row, cs.start.column))
                    .map(|(_, n)| n.clone())
                else {
                    continue;
                };
                // The enum is the tightest container, tighter than any
                // enclosing namespace the sticky context may have tagged.
                symbols[idx].package = Some(enum_name.clone());
                bag.push(Witness {
                    attachment: WA::Variable {
                        name: symbols[idx].name.clone(),
                        scope: symbols[idx].scope,
                    },
                    source: WitnessSource::Builder("cpp_enumerator".into()),
                    payload: WP::InferredType(InferredType::ClassName(enum_name)),
                    span: esp,
                });
            }
        }

        // A function whose owning package is a CLASS is a method. Covers
        // template members, which tree-sitter parses as a plain
        // `declaration` (identifier, not field_identifier) so they classify
        // as Sub — but a sub owned by a class is a method, by definition.
        let class_names: std::collections::HashSet<String> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Class))
            .map(|s| s.name.clone())
            .collect();
        for s in &mut symbols {
            if matches!(s.kind, SymKind::Sub)
                && s.package.as_deref().is_some_and(|p| class_names.contains(p))
            {
                s.kind = SymKind::Method;
            }
        }
        // Per-class template parameter lists (declaration order) — the
        // `ParamOf` index axis. Built here so the writeback below can route
        // param-mentioning member returns through the deferred shape; rides
        // onto `FileAnalysis.pack.template_params` after construction.
        let template_params: std::collections::HashMap<String, Vec<String>> = {
            let mut map: std::collections::HashMap<String, Vec<String>> = Default::default();
            for (owner, param, _) in &self.template_params {
                let v = map.entry(owner.clone()).or_default();
                if !v.contains(param) {
                    v.push(param.clone());
                }
            }
            map
        };
        // Writeback-lite: route method returns through PackageSymbol so
        // `box.getInner().` chains, inherited returns, and cross-file all
        // resolve via the SAME chase Perl uses — no bypass. Declared
        // returns need no fixpoint (the type is in the syntax); only the
        // edge-minting half of the fold's writeback is needed here.
        {
            use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
            let mk = |att: WA, pay: WP, span: Span| Witness {
                attachment: att,
                source: WitnessSource::Builder("cpp_method_return".into()),
                payload: pay,
                span,
            };
            for (i, sym) in symbols.iter().enumerate() {
                if !matches!(sym.kind, SymKind::Method | SymKind::Sub | SymKind::Enumerator) {
                    continue;
                }
                // The declared return goes out as the pack minted it. A
                // receiver-shaped one (`: static`, `@return Base<static>`)
                // substitutes at the call site, so the member-chain arm
                // threads the real receiver and the class-keyed lookup's
                // default receiver (`ClassName(class)`) covers the MCB path
                // — both fluent. (`self` strictly means the DEFINING class;
                // substituting the receiver over-approximates only where a
                // subclass inherits the method — accepted residual.)
                if let Some(declared) = &self.symbols[i].declared_return {
                    // A CONCRETE return is refined against the owning class's
                    // template params, which only this pass knows: a return
                    // that MENTIONS one publishes the deferred
                    // receiver-substituting shape (`ParamOf` — lazy, like
                    // `RowOf`); a concrete class-shaped return edges into the
                    // alias graph (it may be a typedef) instead of committing
                    // the spelling; primitives are leaves. `TypeName` resolves
                    // the typedef or falls back to the same `ClassName`, so
                    // struct returns are unchanged and aliased returns chase.
                    // (edges-not-values)
                    let pay = match declared {
                        crate::model::witnesses::ReturnExpr::Concrete(ret) => {
                            let class_params =
                                sym.package.as_deref().and_then(|p| template_params.get(p));
                            match class_params.and_then(|ps| param_return_expr(ret, ps)) {
                                Some(re) => WP::ReturnExpr(re),
                                None => match ret {
                                    InferredType::ClassName(cn) => {
                                        WP::Edge(WA::TypeName(cn.clone()))
                                    }
                                    other => WP::InferredType(other.clone()),
                                },
                            }
                        }
                        other => WP::ReturnExpr(other.clone()),
                    };
                    bag.push(mk(WA::Symbol(sym.id), pay, sym.span));
                }
                if matches!(sym.kind, SymKind::Method | SymKind::Enumerator) {
                    // Enumerators too: `Level::Debug` / a class const is a
                    // class-keyed member access, and its hop witness chases
                    // the same PackageSymbol edge a method return does.
                    if let Some(class) = &sym.package {
                        bag.push(mk(
                            WA::PackageSymbol { package: class.clone(), name: sym.name.clone() },
                            WP::Edge(WA::Symbol(sym.id)),
                            sym.span,
                        ));
                        // A TRUE enum case's value is an instance of its
                        // enum (php `Level::Debug`, cpp `Color::kRed`). A
                        // class CONST (extraction kind "const", flattened
                        // to the same SymKind) is its literal's value —
                        // typing it as the class would be wrong, so it
                        // stays untyped here (residual: thread the value
                        // span).
                        if self.symbols[i].kind == "enumerator" {
                            bag.push(mk(
                                WA::Symbol(sym.id),
                                WP::InferredType(InferredType::ClassName(class.clone())),
                                sym.span,
                            ));
                        }
                    }
                }
            }
            // A class VIEWED AS A CALLABLE (a functional cast / constructor,
            // `Widget(x)`) produces an instance of itself. Its `Symbol` answers
            // `ClassName(name)` so a call site edging to it materializes the
            // instance — the ctor's value is the class's identity, asked of the
            // resolved class rather than guessed from the callee's name case.
            for sym in &symbols {
                if matches!(sym.kind, SymKind::Class) {
                    bag.push(mk(
                        WA::Symbol(sym.id),
                        WP::InferredType(InferredType::ClassName(sym.name.clone())),
                        sym.span,
                    ));
                }
            }
            // Inheritance edges: PackageSymbol{child,m} → Edge(parent,m), so
            // the registry walks the MRO for an inherited method's return.
            for (child, parent) in &self.parents {
                for sym in &symbols {
                    if matches!(sym.kind, SymKind::Method)
                        && sym.package.as_deref() == Some(parent.as_str())
                    {
                        bag.push(mk(
                            WA::PackageSymbol { package: child.clone(), name: sym.name.clone() },
                            WP::Edge(WA::PackageSymbol { package: parent.clone(), name: sym.name.clone() }),
                            sym.span,
                        ));
                    }
                }
            }
            // Function-like macro typing: the macro's `Symbol` carries its
            // body's implied return (delegation → an Edge to the callee's own
            // return, else the classified concrete type), so a left-unexpanded
            // call `F(args)` types through the sub-return path.
            let sub_sid: std::collections::HashMap<&str, SymbolId> = symbols
                .iter()
                .filter(|s| matches!(s.kind, SymKind::Sub))
                .map(|s| (s.name.as_str(), s.id))
                .collect();
            use crate::build::query_extract::MacroReturnHint;
            let hint_of: std::collections::HashMap<&str, &MacroReturnHint> =
                self.macro_returns.iter().map(|(n, h)| (n.as_str(), h)).collect();
            for (name, hint) in &self.macro_returns {
                let Some(&sid) = sub_sid.get(name.as_str()) else { continue };
                let span = symbols[sid.0 as usize].span;
                let pay = match hint {
                    MacroReturnHint::Delegate(g) => {
                        sub_sid.get(g.as_str()).map(|&gsid| WP::Edge(WA::Symbol(gsid)))
                    }
                    MacroReturnHint::Concrete(t) => Some(WP::InferredType(t.clone())),
                    // The parametric identity/projection return is a deferred
                    // `Arg(n)`; a call site substitutes it by chasing its own
                    // argument below (`Arg` alone answers `None` at a bare sym
                    // probe, exactly like `Receiver`).
                    MacroReturnHint::Param(n) => {
                        Some(WP::ReturnExpr(crate::model::witnesses::ReturnExpr::Arg(*n)))
                    }
                };
                if let Some(pay) = pay {
                    bag.push(mk(WA::Symbol(sid), pay, span));
                }
            }
            // Call-site value resolution: a call's value IS the callee's own
            // resolution. A callee that names a symbol — a `Class` (functional
            // cast / constructor → the class instance), a sub, or a function-
            // like macro — edges `Expr(call) → Edge(Symbol(callee))`; the
            // Symbol answers `ClassName` for a class and its return for a
            // callable. A callee that resolves to NOTHING mints no witness (no
            // name-case guess — an unknown uppercase macro leaves the enclosing
            // `auto x = F(..)` honestly untyped). Prefer a `Class` over a
            // like-named callable (the constructor is the stronger claim).
            let callee_sid: std::collections::HashMap<&str, SymbolId> = {
                let mut m: std::collections::HashMap<&str, SymbolId> =
                    std::collections::HashMap::new();
                for s in &symbols {
                    match s.kind {
                        SymKind::Class => {
                            m.insert(s.name.as_str(), s.id);
                        }
                        SymKind::Sub | SymKind::Method => {
                            m.entry(s.name.as_str()).or_insert(s.id);
                        }
                        _ => {}
                    }
                }
                m
            };
            // Per-call-site argument spans (original coords) so a `Param(n)`
            // call resolves to its n-th argument's value witness.
            let call_args: std::collections::HashMap<Span, &Vec<Span>> =
                self.macro_call_arg_spans.iter().map(|(s, a)| (*s, a)).collect();
            let annot_exprs: std::collections::HashSet<Span> =
                self.annot_expr_spans.iter().copied().collect();
            for (span, name) in &self.call_sites {
                // An overlay declared this call's value (`@expr.annot`) —
                // the callee's return is not its type.
                if annot_exprs.contains(span) {
                    continue;
                }
                // Identity/projection macro: the call's value IS its n-th
                // argument. Edge to the argument's own `Expr` witness rather
                // than the param-agnostic Symbol return (edges-not-values).
                if let Some(MacroReturnHint::Param(n)) = hint_of.get(name.as_str()).copied() {
                    if let Some(arg) = call_args.get(span).and_then(|a| a.get(*n as usize)) {
                        bag.push(mk(WA::Expr(*span), WP::Edge(WA::Expr(*arg)), *span));
                        continue;
                    }
                }
                if let Some(&sid) = callee_sid.get(name.as_str()) {
                    bag.push(mk(WA::Expr(*span), WP::Edge(WA::Symbol(sid)), *span));
                }
            }
        }
        // ---- Local/param vars: a variable READ resolves to the nearest
        // visible Variable declaration by lexical scope walk → local
        // goto-def + hover. Declarations are already Variable symbols
        // (`@def.local`/`@def.var`); fields are excluded naturally (their
        // class scope isn't on a local read's scope chain).
        let mut defs_by_name: std::collections::HashMap<
            String,
            Vec<(crate::model::file_analysis::ScopeId, Span, SymbolId)>,
        > = std::collections::HashMap::new();
        for s in &symbols {
            if matches!(s.kind, SymKind::Variable) {
                defs_by_name
                    .entry(s.name.clone())
                    .or_default()
                    .push((s.scope, s.selection_span, s.id));
            }
        }
        let scope_parent: std::collections::HashMap<
            crate::model::file_analysis::ScopeId,
            Option<crate::model::file_analysis::ScopeId>,
        > = self.scopes.iter().map(|s| (s.id, s.parent)).collect();
        let mut local_refs: Vec<crate::model::file_analysis::Ref> = Vec::new();
        // Reads that found no LOCAL decl — a Variable ref is still minted for
        // each (below, once call/member spans are known) so query-time
        // cross-file resolution can chase it by name (a file-scope value like
        // a C enum constant or global is registered in the pack index; the use
        // resolves the same way a bare call does). rule #7: the token gets a
        // ref whether or not the def is local.
        let mut unresolved_reads: Vec<(String, crate::model::file_analysis::ScopeId, Span)> = Vec::new();
        for (name, read_scope, read_span) in &self.var_reads {
            let rp = (read_span.start.row, read_span.start.column);
            let resolved = defs_by_name.get(name).and_then(|cands| {
                let mut cur = Some(*read_scope);
                while let Some(sc) = cur {
                    // nearest decl of this name in THIS scope level, declared at
                    // or before the read (latest-wins for redeclaration).
                    let mut best: Option<((usize, usize), SymbolId)> = None;
                    for (dscope, dspan, did) in cands {
                        let dp = (dspan.start.row, dspan.start.column);
                        if *dscope == sc && dp <= rp && best.is_none_or(|(bp, _)| dp > bp) {
                            best = Some((dp, *did));
                        }
                    }
                    if let Some((_, did)) = best {
                        return Some(did);
                    }
                    cur = scope_parent.get(&sc).copied().flatten();
                }
                None
            });
            match resolved {
                Some(did) => local_refs.push(crate::model::file_analysis::Ref {
                    kind: crate::model::file_analysis::RefKind::Variable,
                    span: *read_span,
                    scope: *read_scope,
                    target_name: name.clone(),
                    access: crate::model::file_analysis::AccessKind::Read,
                    binding: Some(crate::model::file_analysis::RefBinding::Symbol(did)),
                    folded_from: None,
                    arg_count: None,
                }),
                None => unresolved_reads.push((name.clone(), *read_scope, *read_span)),
            }
        }
        // `goto LABEL` → the `LABEL:` def, function-wide: first matching
        // Variable on the scope chain, NO declared-before constraint (a
        // forward goto is valid).
        for (name, ref_scope, ref_span) in &self.label_refs {
            let Some(cands) = defs_by_name.get(name) else { continue };
            let mut cur = Some(*ref_scope);
            let mut resolved: Option<SymbolId> = None;
            while let Some(sc) = cur {
                if let Some((_, _, did)) = cands.iter().find(|(dscope, _, _)| *dscope == sc) {
                    resolved = Some(*did);
                    break;
                }
                cur = scope_parent.get(&sc).copied().flatten();
            }
            if let Some(did) = resolved {
                local_refs.push(crate::model::file_analysis::Ref {
                    kind: crate::model::file_analysis::RefKind::Variable,
                    span: *ref_span,
                    scope: *ref_scope,
                    target_name: name.clone(),
                    access: crate::model::file_analysis::AccessKind::Read,
                    binding: Some(crate::model::file_analysis::RefBinding::Symbol(did)),
                    folded_from: None,
                    arg_count: None,
                });
            }
        }

        // A `@ref.type` on a def's OWN name token (class/enum/typedef
        // declaring itself) is the declaration, not a use — suppress by
        // exact selection-span match so the Symbol stays the only claimant.
        let member_write_spans: std::collections::HashSet<(usize, usize, usize, usize)> = self
            .member_writes
            .iter()
            .map(|sp| (sp.start.row, sp.start.column, sp.end.row, sp.end.column))
            .collect();
        let decl_name_spans: std::collections::HashSet<(usize, usize, usize, usize)> = symbols
            .iter()
            .map(|s| {
                (
                    s.selection_span.start.row,
                    s.selection_span.start.column,
                    s.selection_span.end.row,
                    s.selection_span.end.column,
                )
            })
            .collect();
        let mut refs: Vec<crate::model::file_analysis::Ref> = self
            .refs
            .iter()
            .filter_map(|r| {
                use crate::model::file_analysis::{RefBinding, RefKind};
                let mut span = Span { start: r.start, end: r.end };
                let mut binding = None;
                let mut name = r.name.clone();
                let kind = match r.kind.as_str() {
                    "call" => RefKind::FunctionCall,
                    // Qualified call (`fmt::format_to(...)`): Perl parity —
                    // the full path rides `target_name`, the qualifier the
                    // `Function` binding, and the span narrows to the bare
                    // name token (the rename/highlight unit; also what
                    // suppresses the `@expr.read.var` duplicate at the same
                    // start). The tail segment is an identifier, so it never
                    // spans rows — the end-anchored column math is safe.
                    "qcall" => {
                        let (pkg, bare) = crate::model::file_analysis::split_qualified(&r.name, &names);
                        let pkg = pkg?;
                        span.start = tree_sitter::Point {
                            row: r.end.row,
                            column: r.end.column.saturating_sub(bare.len()),
                        };
                        binding = Some(RefBinding::Function { package: pkg.to_string() });
                        RefKind::FunctionCall
                    }
                    // `recv.field` / `recv->field`: the SAME MethodCall ref
                    // core resolves for Perl `$obj->m`. The invocant types
                    // query-time via `expr_type_at_span(invocant_span)`;
                    // `find_definition`/`refs_to`/hover all flow from it.
                    "member" => {
                        let (inv_span, inv_text) = r.invocant.clone()?;
                        let invocant = crate::model::conventions::Invocant::assume_canonical(inv_text);
                        if r.value_read {
                            // A value read is its own ref kind: it resolves to
                            // the class's value members and never to a
                            // same-named method (`docs/adr/member-kinds.md`).
                            RefKind::FieldAccess {
                                invocant,
                                invocant_span: Some(inv_span),
                                member_name_span: Span { start: r.start, end: r.end },
                                member_op: r.member_op,
                            }
                        } else {
                            RefKind::MethodCall {
                                invocant,
                                invocant_span: Some(inv_span),
                                method_name_span: Span { start: r.start, end: r.end },
                                member_op: r.member_op,
                                named_by_string: r.named_by_string,
                            }
                        }
                    }
                    // A hook-firing string (`do_action('init')` arg 1): the
                    // model's DispatchCall on its rail (see the "handler"
                    // symbol arm) — refs_to pairs it with the stacked Handler
                    // registrations by name+owner equality.
                    "dispatch" => {
                        let owner = rail_owner(Span { start: r.start, end: r.end })?;
                        // A rail with a parameter separator: the use names
                        // the head (`throttle:60,1` → `throttle`), and the
                        // span ends with it — a string never spans rows.
                        if let crate::model::file_analysis::HandlerOwner::Rail(rail) = &owner {
                            if let Some((_, sep)) = rail_name_seps.iter().find(|(rl, _)| rl == rail) {
                                if let Some(cut) = name.find(sep.as_str()) {
                                    if cut > 0 {
                                        span.end = tree_sitter::Point {
                                            row: span.start.row,
                                            column: span.start.column + cut,
                                        };
                                        name.truncate(cut);
                                    }
                                }
                            }
                        }
                        binding = Some(RefBinding::Handler { owner, sym: None });
                        RefKind::DispatchCall {
                            dispatcher: r.via.clone().unwrap_or_default(),
                        }
                    }
                    // A type-position name (`Widget w;`, `struct op* o`, a
                    // base-class clause): the same PackageRef a Perl package
                    // use carries, so type gd/gr ride the Package machinery.
                    "type" => {
                        if decl_name_spans.contains(&(
                            r.start.row,
                            r.start.column,
                            r.end.row,
                            r.end.column,
                        )) {
                            return None;
                        }
                        RefKind::PackageRef
                    }
                    _ => return None,
                };
                Some(crate::model::file_analysis::Ref {
                    kind,
                    span,
                    scope: r.scope,
                    target_name: name,
                    access: if member_write_spans.contains(&(span.start.row, span.start.column, span.end.row, span.end.column)) {
                        crate::model::file_analysis::AccessKind::Write
                    } else {
                        crate::model::file_analysis::AccessKind::Read
                    },
                    binding,
                    folded_from: None,
                    arg_count: r.arg_count,
                })
            })
            .collect();
        // The `(identifier) @expr.read.var` catch-all also fires on a call's
        // function-name (`foo` in `foo()`) and on a def's OWN name (`compute`
        // in `int compute(...)`), neither of which is a bare value read — the
        // first already carries a FunctionCall ref, the second IS the
        // declaration. Don't shadow either with a stray unresolved Variable
        // ref (it would displace the decl from references/highlight). Claims
        // are NAME-keyed (a ref by its unqualified TAIL, so a qualified
        // call's name token still claims the read at its own start): a read
        // spelling a DIFFERENT name at the same start is a different token —
        // the erased-macro re-mint (`ABSL_GUARDED_BY`) must survive the
        // expansion-body ref (`guarded_by`) remapped onto the same call
        // site, or the macro's use vanishes from gr.
        let claimed: std::collections::HashSet<(usize, usize, String)> = refs
            .iter()
            .map(|r| {
                (
                    r.span.start.row,
                    r.span.start.column,
                    r.unqualified_target_name(&names).to_string(),
                )
            })
            .chain(symbols.iter().map(|s| {
                (
                    s.selection_span.start.row,
                    s.selection_span.start.column,
                    s.name.clone(),
                )
            }))
            .collect();
        let runtime_bound: std::collections::HashSet<(usize, usize)> = self
            .runtime_bound_reads
            .iter()
            .map(|s| (s.start.row, s.start.column))
            .collect();
        for (name, scope, span) in unresolved_reads {
            if claimed.contains(&(span.start.row, span.start.column, name.clone())) {
                continue;
            }
            let runtime = runtime_bound.contains(&(span.start.row, span.start.column));
            local_refs.push(crate::model::file_analysis::Ref {
                kind: crate::model::file_analysis::RefKind::Variable,
                span,
                scope,
                target_name: name,
                access: crate::model::file_analysis::AccessKind::Read,
                binding: runtime.then_some(crate::model::file_analysis::RefBinding::Runtime),
                folded_from: None,
                arg_count: None,
            });
        }
        refs.extend(local_refs);
        // Demoted re-assignments (function-scoped vars): the site is a
        // WRITE of the one declaration, so references/rename see it and
        // documentHighlight classifies it honestly.
        for (name, scope, span) in var_rebind_refs {
            refs.push(crate::model::file_analysis::Ref {
                kind: crate::model::file_analysis::RefKind::Variable,
                span,
                scope,
                target_name: name,
                access: crate::model::file_analysis::AccessKind::Write,
                binding: None,
                folded_from: None,
                arg_count: None,
            });
        }
        // Field/member uses recovered from `#define` bodies (`->op_next`): the
        // receiver is a macro parameter with no type, so resolve the field to
        // its declaring class from THIS file's own field symbols and freeze the
        // class on the ref. Only a name that maps to ONE declaring class is
        // minted — an ambiguous field (same name on two structs) or a
        // cross-file-only field stays silent (documented residual), keeping the
        // over-approximation honest. The frozen `MethodTarget` is exactly what
        // `refs_to`'s member arm reads, so references on the field include the
        // in-body use without needing the invocant to type.
        if !self.macro_body_member_reads.is_empty() {
            // field name → every (declaring class, decl SymbolId). The receiver
            // in a macro body is an untypeable macro parameter, so the class is
            // taken from the field DECL. A name shared by several structs —
            // overwhelmingly a member-block-replicated field (perl5 `op_next` is
            // spliced into every OP struct via `BASEOP`) — is genuinely one
            // logical field; attribute the use to EACH declaring class so a
            // references query on any of them counts it. Per query that is +1
            // (the class-frozen ref matches only that class's target); the
            // over-approximation is only across unrelated same-named fields (a
            // documented, references-tolerant honest over-count, not a wrong
            // "is this a field" — the token IS a field member access).
            let mut field_owners: std::collections::HashMap<
                &str,
                Vec<(&str, crate::model::file_analysis::SymbolId)>,
            > = std::collections::HashMap::new();
            for s in &symbols {
                if s.kind == SymKind::Field {
                    if let Some(pkg) = s.package.as_deref() {
                        let v = field_owners.entry(s.name.as_str()).or_default();
                        if !v.iter().any(|(p, _)| *p == pkg) {
                            v.push((pkg, s.id));
                        }
                    }
                }
            }
            let claimed: std::collections::HashSet<(usize, usize)> =
                refs.iter().map(|r| (r.span.start.row, r.span.start.column)).collect();
            for (field, span) in &self.macro_body_member_reads {
                if claimed.contains(&(span.start.row, span.start.column)) {
                    continue;
                }
                let Some(owners) = field_owners.get(field.as_str()) else { continue };
                for (class, sym_id) in owners {
                    // The drill reads a stored member, so the ref is the value
                    // access (`docs/adr/member-kinds.md`): a field target admits
                    // it and a call target never claims it.
                    refs.push(crate::model::file_analysis::Ref {
                        kind: crate::model::file_analysis::RefKind::FieldAccess {
                            invocant: crate::model::conventions::Invocant::assume_canonical(String::new()),
                            invocant_span: None,
                            member_name_span: *span,
                            member_op: None,
                        },
                        span: *span,
                        scope: crate::model::file_analysis::ScopeId(0),
                        target_name: field.clone(),
                        access: crate::model::file_analysis::AccessKind::Read,
                        // Local edge (the field decl IS in this file): references
                        // matches on `invocant_class`, and goto-def from the
                        // in-body use lands on `sym_id` — no query-time invocant
                        // typing (the receiver is an untypeable macro parameter).
                        binding: Some(crate::model::file_analysis::RefBinding::Method(
                            crate::model::file_analysis::MethodTarget::Local {
                                sym_id: *sym_id,
                                invocant_class: class.to_string(),
                            },
                        )),
                        folded_from: None,
                        arg_count: None,
                    });
                }
            }
        }
        // Implicit receiver: where the document says a body elides it
        // (`@scope.sub.implicit_receiver`), a bare name is a member of the
        // enclosing class. The scope chain carries the fact — a nested block
        // or lambda body inside such a body elides too — and the declaring
        // scope's `owner` names the callable whose package IS that class,
        // in-class and out-of-line alike (a `void Buf<T>::reserve(…)` body is
        // lexically at file scope, so only the peeled method symbol carries
        // `Buf`). Whether a class is there at all is resolution: a free
        // function's scope carries the flag and its owner names no package,
        // so nothing binds.
        {
            use crate::model::file_analysis::RefKind;
            use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
            let implicit_class = |scope: crate::model::file_analysis::ScopeId| -> Option<&str> {
                std::iter::successors(Some(scope), |sc| scope_parent.get(sc).copied().flatten())
                    .find(|sc| self.scopes[sc.0 as usize].implicit_receiver)
                    .and_then(|sc| self.scopes[sc.0 as usize].owner)
                    .and_then(|sid| symbols.get(sid.0 as usize))
                    .and_then(|s| s.package.as_deref())
            };
            let mut fields: std::collections::HashMap<
                (&str, &str),
                (crate::model::file_analysis::ScopeId, SymbolId),
            > = Default::default();
            let mut methods: std::collections::HashSet<(&str, &str)> = Default::default();
            for s in &symbols {
                let Some(pkg) = s.package.as_deref() else { continue };
                match s.kind {
                    SymKind::Field => {
                        fields.entry((pkg, s.name.as_str())).or_insert((s.scope, s.id));
                    }
                    SymKind::Method => {
                        methods.insert((pkg, s.name.as_str()));
                    }
                    _ => {}
                }
            }
            let mut field_reads: Vec<(usize, SymbolId, String, crate::model::file_analysis::ScopeId, Span)> =
                Vec::new();
            let mut sibling_calls: Vec<(usize, String)> = Vec::new();
            for (i, r) in refs.iter().enumerate() {
                if matches!(r.kind, RefKind::Variable) && r.resolved_symbol().is_none() {
                    let Some(class) = implicit_class(r.scope) else { continue };
                    let Some(&(fscope, fsym)) = fields.get(&(class, r.target_name.as_str())) else {
                        continue;
                    };
                    field_reads.push((i, fsym, r.target_name.clone(), fscope, r.span));
                } else if matches!(r.kind, RefKind::FunctionCall) && r.binding.is_none() {
                    let Some(class) = implicit_class(r.scope) else { continue };
                    if methods.contains(&(class, r.target_name.as_str())) {
                        sibling_calls.push((i, class.to_string()));
                    }
                }
            }
            for (i, fsym, name, fscope, span) in field_reads {
                // Bind the read to the field it names, so goto-def /
                // references / rename land on the declaration; the edge to the
                // field's own `Variable` attachment is what types it, chasing
                // the general Variable path instead of dead-ending. A binding
                // without the edge answers goto-def and types nothing; the
                // edge without the binding types and answers goto-def empty
                // while references still names the declaration — the
                // projection disagreement the consistency net flags as I4.
                refs[i].bind_symbol(fsym);
                bag.push(Witness {
                    attachment: WA::Expr(span),
                    source: WitnessSource::Builder("implicit_field_read".into()),
                    payload: WP::Edge(WA::Variable { name, scope: fscope }),
                    span,
                });
            }
            // Pinning a bare sibling call's `Function` binding to the class
            // routes it through the SAME package-scoped callable resolution a
            // qualified `Class::foo()` uses, so goto-def / references / rename
            // land on the sibling. A name no member answers to is left alone —
            // a free-function call still resolves free.
            for (i, class) in sibling_calls {
                refs[i].bind_function_package(class);
            }
        }
        let mut packages: std::collections::HashMap<
            String,
            crate::model::file_analysis::PackageFacts,
        > = std::collections::HashMap::new();
        for (child, parent) in &self.parents {
            packages.entry(child.clone()).or_default().parents.push(parent.clone());
        }
        // Contracts: an interface, a trait or an abstract class is a role —
        // it defers its obligations to a concrete composer — and each of
        // its contract callables is a require that composer must provide
        // (`unfulfilled_role_requires`, docs/adr/role-contracts.md). The
        // contract symbols are excluded from provision the way Perl's
        // `requires` markers are.
        let mut contract_symbols: std::collections::HashSet<SymbolId> = Default::default();
        for (i, sym) in symbols.iter().enumerate() {
            let defers = sym
                .flags
                .intersects(SymbolFlags::INTERFACE | SymbolFlags::TRAIT | SymbolFlags::ABSTRACT);
            if sym.kind == SymKind::Class && defers {
                packages.entry(sym.name.clone()).or_default().is_role = true;
            }
            if matches!(sym.kind, SymKind::Sub | SymKind::Method)
                && sym.flags.contains(SymbolFlags::CONTRACT)
            {
                contract_symbols.insert(SymbolId(i as u32));
                if let Some(pkg) = &sym.package {
                    let facts = packages.entry(pkg.clone()).or_default();
                    if !facts.requires.contains(&sym.name) {
                        facts.requires.push(sym.name.clone());
                    }
                }
            }
        }
        // `$var = $recv->method()` bindings: hand the assignment to the
        // language-generic MCB→bag bridge (`emit_method_call_binding_edges`),
        // which resolves the receiver and chases the method's return lazily —
        // at finalize AND at every enrichment re-run, so a receiver whose
        // class only types once imports land still resolves. Join: the flow
        // edge's SOURCE opens at a member ref's invocant; the rightmost such
        // token is the chain's last hop. A chained receiver's invocant text
        // (`$u->a()`) names no variable and no-ops harmlessly — single-hop
        // bindings are the ones that type here.
        let method_call_bindings: Vec<crate::model::file_analysis::MethodCallBinding> = self
            .flow_edges
            .iter()
            .filter_map(|fe| {
                self.refs
                    .iter()
                    .filter_map(|r| {
                        let (inv_span, inv_text) = r.invocant.as_ref()?;
                        (r.kind == "member"
                            && inv_span.start == fe.source.start
                            && (r.end.row, r.end.column)
                                <= (fe.source.end.row, fe.source.end.column))
                            .then(|| (r, inv_text.clone()))
                    })
                    .max_by_key(|(r, _)| (r.start.row, r.start.column))
                    .map(|(r, inv)| crate::model::file_analysis::MethodCallBinding {
                        variable: fe.target_name.clone(),
                        invocant_var: inv,
                        method_name: r.name.clone(),
                        scope: fe.target_scope,
                        span: fe.source,
                    })
            })
            .collect();
        let pack = crate::model::file_analysis::PackFacts {
            // Pack-declared receiver names ride the FA so core's member /
            // outline filters can exclude them generically (lang semantics in
            // the pack, generic logic in core).
            import_rows: std::mem::take(&mut self.import_rows),
            spellings: self.spellings,
            preamble_end: self.preamble_end,
            doc_mentions: std::mem::take(&mut self.doc_mentions),
            names: std::mem::take(&mut self.names),
            // Specialization family edges (spec → primary). NOT an inheritance
            // edge: a spec inherits nothing from its primary (it replaces
            // wholesale), so member resolution must never fall through this
            // edge — only the graph's `Specializes` family view reads it.
            specializes: self.specializations.drain(..).collect(),
            // Per-class ordered template params — the substitution axis the
            // dispatch ladder + field substitution read (methods already carry
            // `ParamOf` witnesses from the writeback above).
            template_params,
            // Include/import path tokens carry a span so goto-def can resolve
            // the header (the bare `imports` list is span-less). Resolution to
            // an absolute path happens where the file path is in hand (the
            // driver), which also fills `macro_defs` / `include_closure`.
            include_directives: self.import_sites.drain(..).collect(),
            use_aliases: std::mem::take(&mut self.use_aliases),
            qualified_spellings: std::mem::take(&mut self.qualified_spellings),
            domain_sites: std::mem::take(&mut self.domain_sites),
            moved_from: std::mem::take(&mut self.moved_from),
            doc_disagreements: std::mem::take(&mut self.doc_disagreements),
            control_regions: std::mem::take(&mut self.control_regions),
            param_regions: std::mem::take(&mut self.param_regions),
            probe_regions: std::mem::take(&mut self.probe_regions),
            class_named_rails: std::mem::take(&mut self.class_named_rails),
            ..Default::default()
        };
        // Folding follows the scopes the skeleton minted: a class body, a
        // function body, a block — every multi-line one is a region.
        let mut fold_ranges: Vec<crate::model::file_analysis::FoldRange> = Vec::new();
        let scope_folds = self
            .scopes
            .iter()
            .filter(|sc| !matches!(sc.kind, crate::model::file_analysis::ScopeKind::File))
            .map(|sc| (sc.span, false));
        for (span, comment) in scope_folds.chain(self.fold_regions.drain(..)) {
            let (start_line, end_line) = (span.start.row, span.end.row);
            if end_line > start_line
                && !fold_ranges.iter().any(|f| f.start_line == start_line && f.end_line == end_line)
            {
                fold_ranges.push(crate::model::file_analysis::FoldRange {
                    start_line,
                    end_line,
                    kind: if comment {
                        crate::model::file_analysis::FoldKind::Comment
                    } else {
                        crate::model::file_analysis::FoldKind::Region
                    },
                });
            }
        }
        fold_ranges.sort_by_key(|f| (f.start_line, f.end_line));
        let mut fa = FileAnalysis::new(FileAnalysisParts {
            scopes: self.scopes,
            fold_ranges,
            contract_symbols,
            symbols,
            refs,
            witnesses: bag,
            packages,
            pack,
            method_call_bindings,
            flow_edges: std::mem::take(&mut self.flow_edges),
            ..Default::default()
        });
        // The pack that built it. The driver stamps the same id; setting it
        // here means an analysis produced without one still answers the
        // by-language-id lookups (spellings, the document's own literals).
        fa.language = self.lang_id.to_string();
        // Seal base_*_count so a later enrich pass (the CLI/--batch path
        // runs it unconditionally) truncates to the FULL analysis, not to
        // zero — otherwise enrichment wipes every pack-language symbol.
        fa.finalize_post_walk();
        fa
    }
}

/// The closed flag set a pack symbol carries, from the skeleton's kind and
/// the pack's attribute vocabulary — the ONE place a pack's attribute
/// spellings become model facts (rule #12: the model asks the flag, never
/// the string).
fn symbol_flags_of(kind: &str, attributes: &[String]) -> SymbolFlags {
    let mut flags = SymbolFlags::empty();
    // A structural kind that IS a flag (`unionfield` is a union's member
    // container); the canonical table answers the rest.
    if let Ok(f) = SymbolFlags::try_from(if kind == "unionfield" { "union" } else { kind }) {
        flags.insert(f);
    }
    // A `@sym.attr` token is source text (cpp's `register`, a php `#[Attr]`
    // name): a spelling the table does not know is display-only, never an
    // error — the query-DECLARED spellings are validated at overlay compile.
    for a in attributes {
        if let Ok(f) = SymbolFlags::try_from(a.as_str()) {
            flags.insert(f);
        }
    }
    flags
}
