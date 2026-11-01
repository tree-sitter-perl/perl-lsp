//! The skeleton data model: symbol/ref rows and `SkeletonAnalysis`,
//! including its projection into `FileAnalysis`.

use crate::model::file_analysis::SymbolFlags;
use super::*;

/// The skeleton's symbol row. Its kind is the `@def.<kind>` that minted it.
#[derive(Debug, Clone)]
pub struct SkelSymbol {
    pub kind: DefCap,
    pub name: String,
    pub start: Point,
    pub end: Point,
    pub name_start: Point,
    pub name_end: Point,
    /// Sticky `@context.package` value in force at the def site.
    pub package: Option<String>,
    pub scope: crate::model::file_analysis::ScopeId,
    /// The declaration as written around its name token (`@def.<kind>.spelled`:
    /// php's `$x` around the member name `x`), listed in place of the name.
    pub spelled: Option<String>,
    /// The `@def.pair` match this def shares its name token with: the two
    /// defs carrying one id are one declaration (`Symbol::declared_with`).
    /// A match id, not a position in `symbols`, so it survives every dedup.
    pub pair: Option<usize>,
    /// Declared return type (`@rettype`), for methods/functions — drives
    /// method-return resolution + chaining through PackageSymbol.
    pub return_type: Option<InferredType>,
    /// Pointer/reference declarator stack, unravelled by the `deref` walk
    /// from a `@nested.target` capture (empty otherwise). Flows to `Symbol.deref_stack`.
    pub deref_stack: Vec<crate::model::file_analysis::DerefStep>,
    /// Structural markers carried onto `Symbol.attributes`: "anonymous" when
    /// the name came from the pack's `default_name` (not addressable by name —
    /// completion skips it). The kind's own marker (`DefCap::marker`) is
    /// stamped in `into_file_analysis`.
    pub attributes: Vec<String>,
    /// Declared parameter arity for a callable, counted structurally from the
    /// def's parameter list (`@arity.sig`). `None` for non-callables and defs
    /// whose parameter list the query didn't capture. Flows to `Symbol.arity`.
    pub arity: Option<crate::model::file_analysis::ParamArity>,
    /// The `package` came from an explicit `::` qualifier on an out-of-line def
    /// (`Ret Class::m(){}`), not from lexical/sticky context. The class the
    /// qualifier names is authoritative EVEN when its body lives in another file
    /// (a header), so `reanchor_truncated_containers` must not re-attribute it to
    /// the enclosing namespace. Not serialized — a driver-internal marker.
    pub qualifier_owned: bool,
    /// Declaration facts a capture minted rather than an attribute token
    /// (a receiver parameter). Or-ed onto the flags the kind and the
    /// attributes give.
    pub flags: SymbolFlags,
}

impl crate::model::file_analysis::ScopeClasses for Vec<SkelSymbol> {
    fn declares_in(&self, scope: crate::model::file_analysis::ScopeId, identity: &str) -> bool {
        self.iter().any(|s| s.scope == scope && matches!(s.kind, DefCap::Class | DefCap::Package) && s.name == identity)
    }
}

#[derive(Debug, Clone)]
pub struct SkelRef {
    pub kind: RefCap,
    pub name: String,
    pub start: Point,
    pub end: Point,
    pub scope: crate::model::file_analysis::ScopeId,
    /// For a member access (`recv.field`) — the receiver subtree's span and
    /// its `Invocant`, minted by the extractor that saw whether the receiver
    /// is a written name or an expression. `into_file_analysis` puts it on a
    /// `RefKind::MethodCall`/`FieldAccess`, whose invocant types query-time
    /// via `expr_type_at_span(span)`. `None` for plain calls / var refs.
    pub invocant: Option<(crate::model::file_analysis::Span, crate::model::conventions::Invocant)>,
    /// The written member operator (`.`/`->`) + its span, mapped from the
    /// `@member.op.<which>` capture on the operator token, `Some` only when the
    /// IMMEDIATE receiver is a simple variable. Rides onto the MethodCall ref
    /// so operator-correctness is a ref query, not a separate walk.
    pub member_op: Option<(crate::model::file_analysis::MemberOp, crate::model::file_analysis::Span)>,
    /// Written argument count at a call ref (`call`/`qcall`/`member.call`),
    /// counted from the argument list the same match captured. Flows to
    /// `Ref.arg_count`; `None` for non-call refs.
    pub arg_count: Option<usize>,
    /// A member call that constructs (`new Foo(…)`): its token spells the
    /// class, not the member it calls.
    pub constructs: bool,
}

/// One `@expr.call` site.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub span: Span,
    /// The callee as its token spells it: a member's name, a free call's
    /// name token.
    pub callee: String,
    /// Where the callee's token starts: the site its name resolves at.
    pub callee_at: tree_sitter::Point,
    /// A member call's receiver expression; `None` for a free call.
    pub receiver: Option<Span>,
    /// The scope the call is written in, which says whether a bare callee
    /// may be a member of the body's class.
    pub scope: crate::model::file_analysis::ScopeId,
    /// The written arguments in order, each with whether it spreads
    /// (`@arity.spread`); `None` when the call's match captured no argument
    /// list.
    pub args: Option<Vec<(Span, bool)>>,
    /// The call constructs an instance of its receiver's class
    /// (`@expr.construct`).
    pub constructs: bool,
}

/// A receiver token that names a class (`@receiver.*`). The class is
/// resolved in `into_file_analysis`, where the use-map and the callables'
/// owners are in hand.
#[derive(Debug, Clone)]
pub struct ReceiverSite {
    pub span: Span,
    pub scope: crate::model::file_analysis::ScopeId,
    pub names: super::vocab::ReceiverCap,
    /// The token as written: the spelling a `Class` receiver resolves.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct SkeletonAnalysis {
    pub symbols: Vec<SkelSymbol>,
    pub refs: Vec<SkelRef>,
    pub imports: Vec<String>,
    /// The directories the file adds to its module search path
    /// (`@root`). Carried onto `FileAnalysis.search_roots`.
    pub search_roots: Vec<crate::model::file_analysis::SearchRoot>,
    /// `#include`/`import` path tokens with spans: (raw path text, path-token
    /// span). Goto-def on the token resolves the header; the span is what the
    /// bare `imports` list drops. Carried onto `FileAnalysis.pack.include_directives`.
    pub import_sites: Vec<(String, crate::model::file_analysis::Span)>,
    /// A use-map language's `use` rows. Carried onto
    /// `FileAnalysis.pack.use_rows`.
    pub use_rows: Vec<crate::model::file_analysis::UseRow>,
    /// A use-map language's namespace sections. Carried onto
    /// `FileAnalysis.namespace_ranges`.
    pub namespace_ranges: Vec<crate::model::file_analysis::NamespaceRange>,
    pub scope_count: usize,
    pub scopes: Vec<crate::model::file_analysis::Scope>,
    pub witnesses: Vec<crate::model::witnesses::Witness>,
    /// (child class, parent class) inheritance edges — `@parent` captures.
    pub parents: Vec<(String, String)>,
    /// The `parents` edges the document marks `@parent.super`, by index.
    pub super_parents: Vec<usize>,
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
    /// A data member's declared type, keyed by its (name, scope), for the
    /// slot's value edge: a type that mentions its class's template
    /// parameters substitutes the receiver's arguments.
    pub field_types: Vec<(String, crate::model::file_analysis::ScopeId, InferredType)>,
    /// Variable reads (`@expr.read.var`): (name, scope, span). Each resolves
    /// to the nearest visible Variable declaration by lexical scope walk →
    /// local goto-def + hover. Resolution runs in `into_file_analysis`.
    pub var_reads: Vec<(String, crate::model::file_analysis::ScopeId, crate::model::file_analysis::Span)>,
    /// Writes of a variable a def the write can see already declares (a
    /// `@def.assigned` that is not its name's first): (name, scope, name
    /// token span). Each resolves like a read and is minted as a write.
    pub var_writes: Vec<(String, crate::model::file_analysis::ScopeId, crate::model::file_analysis::Span)>,
    /// The member tokens assignments write (a `@flow.place` whose read
    /// projects a member): the reference at each is a write.
    pub write_sites: Vec<crate::model::file_analysis::Span>,
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
    /// The pack's language id (`LangPack::id`), stamped on the assembly.
    pub language: &'static str,
    /// The language's name spellings (`LangPack::names`), baked onto
    /// `PackFacts::names`.
    pub names: crate::model::file_analysis::NameSpellings,
    /// Value-flow edges minted from `@flow` captures (`source → target`,
    /// extraction). Lowered to type witnesses here; carried onto the FA as the
    /// provenance tier.
    pub flow_edges: Vec<crate::model::file_analysis::FlowEdge>,
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
    /// Call sites (`@expr.call`). The call's VALUE is an `Invoke` of the
    /// callee on its arguments, minted in `into_file_analysis` against the
    /// symbol table: a `Class` callee is a functional cast / constructor (→
    /// an instance of that class), a callable (sub / function-like macro)
    /// flows its return, and a name this file doesn't define is looked up
    /// across files at query time (no name-case guess —
    /// `docs/adr/macro-handling.md`).
    pub call_sites: Vec<CallSite>,
    /// Receiver tokens that name a class (`@receiver.*`).
    pub receiver_sites: Vec<ReceiverSite>,
    /// `return EXPR;` sites (`@expr.return.value`): (enclosing scope, the
    /// returned expression's span). Purely structural — this tier doesn't
    /// know what a `return` MEANS for any given language; the interpretation
    /// (join to the owning callable, chain it when the callable declares no
    /// return) is `into_file_analysis`'s.
    pub return_sites: Vec<(crate::model::file_analysis::ScopeId, Span)>,
    /// Receiver parameters (`@receiver`, python's `self`): (enclosing scope,
    /// the parameter's name token). `into_file_analysis` types each as its
    /// method's class, once the scopes know their callables. An undeclared
    /// receiver (`this`) is a `receiver_sites` entry.
    pub receivers: Vec<(crate::model::file_analysis::ScopeId, Span)>,
    /// Callable parameter arities keyed by parameter_list span (`@arity.sig`).
    /// Associated to def symbols by span containment in `into_file_analysis`.
    pub param_sigs: Vec<(crate::model::file_analysis::Span, crate::model::file_analysis::ParamArity)>,
    /// Writes through a receiver (`obj.x = …`, `@def.slot`), undecided: a
    /// slot each, if `declare_slots` finds its owner and no declaration of
    /// the name there.
    pub slot_writes: Vec<SlotWrite>,
    /// Writes to a member (an `@flow.place` a member read projects),
    /// undecided: which member each writes is the receiver's type's, settled by
    /// `settle_member_writes`.
    pub member_writes: Vec<MemberWrite>,
    /// The scope each class declared in this file opened as its own, by
    /// the class's identity: where a slot of it is filed.
    pub class_scopes: Vec<(String, crate::model::file_analysis::ScopeId)>,
    /// The `#if` trails the file's declarations sit under, the table every
    /// arm id points into. Carried onto `FileAnalysis.pack.cond_arms`.
    pub cond_arms: crate::model::file_analysis::CondArms,
}

/// A write through a receiver (`obj.x = …`): the slot it would declare
/// (no owner yet) and the receiver whose type names the owner.
#[derive(Debug, Clone)]
pub struct SlotWrite {
    pub symbol: SkelSymbol,
    pub receiver: String,
}

/// An assignment to a member through a receiver (`obj.x = v`): the
/// receiver expression, the member's name, and the value written.
#[derive(Debug, Clone)]
pub struct MemberWrite {
    pub receiver: crate::model::file_analysis::Span,
    pub member: String,
    pub value: crate::model::file_analysis::Span,
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
    /// The class `rel` names written in `class`: the class itself, or the
    /// one its `@parent.super` edge names.
    pub fn relative_class(&self, rel: super::vocab::RelativeClass, class: &str) -> Option<String> {
        match rel {
            super::vocab::RelativeClass::Enclosing => Some(class.to_string()),
            super::vocab::RelativeClass::Super => self
                .super_parents
                .iter()
                .map(|&i| &self.parents[i])
                .find(|(child, _)| child == class)
                .map(|(_, parent)| parent.clone()),
        }
    }

    /// An empty skeleton of `pack`'s language: what extraction fills, and
    /// what a file that never reached extraction assembles from.
    pub fn for_pack(pack: &super::LangPack) -> Self {
        // The analysis is stamped with the pack's language here, so its
        // lattice is registered here too: every reader of the stamp finds it.
        // Perl's rules are declared, never registered.
        if pack.id != crate::model::conventions::PERL_LANGUAGE {
            crate::model::conventions::register_lattice(pack.id, || pack.lattice.clone());
        }
        SkeletonAnalysis { language: pack.id, names: pack.names.clone(), ..Default::default() }
    }

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
            if !matches!(s.kind, DefCap::Class | DefCap::Union | DefCap::Package) {
                continue;
            }
            let (ns, ne) = (pt(s.name_start), pt(s.name_end));
            if source.get(ns..ne) != Some(s.name.as_str()) {
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
        use crate::model::file_analysis::{
            FileAnalysis, FileAnalysisParts, SymKind, Symbol, SymbolId,
        };
        let names = self.names.clone();
        let slot_writes = std::mem::take(&mut self.slot_writes);
        let member_writes = std::mem::take(&mut self.member_writes);
        let class_scopes = std::mem::take(&mut self.class_scopes);
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
                .filter(|s| s.kind == DefCap::Union)
                .map(key_of)
                .collect();
            let mut seen = std::collections::HashSet::new();
            self.symbols.retain(|s| match s.kind {
                DefCap::Class => !union_keys.contains(&key_of(s)) && seen.insert(key_of(s)),
                DefCap::Union => seen.insert(key_of(s)),
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
            let dedup = |k: DefCap| matches!(k, DefCap::Sub | DefCap::Method);
            type Key = (DefCap, usize, usize, usize, usize);
            let key_of = |s: &SkelSymbol| -> Key {
                (s.kind, s.name_start.row, s.name_start.column, s.name_end.row, s.name_end.column)
            };
            let mut resolved: HashMap<Key, bool> = HashMap::new();
            for s in &self.symbols {
                if dedup(s.kind) {
                    let has = s.return_type.is_some();
                    resolved.entry(key_of(s)).and_modify(|v| *v |= has).or_insert(has);
                }
            }
            let mut kept: std::collections::HashSet<Key> = Default::default();
            self.symbols.retain(|s| {
                if !dedup(s.kind) {
                    return true;
                }
                let key = key_of(s);
                // Keep the rettype-bearing copy; if none has one, keep the first.
                if resolved.get(&key) == Some(&true) && s.return_type.is_none() {
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
            for s in self.symbols.iter_mut() {
                if !matches!(s.kind, DefCap::Sub | DefCap::Method) {
                    continue;
                }
                s.arity = param_sigs
                    .iter()
                    .filter(|(sp, _)| {
                        after(sp.start, s.name_end)
                            && after(s.end, sp.end)
                            && after(sp.start, s.start)
                    })
                    .min_by_key(|(sp, _)| (sp.start.row, sp.start.column))
                    .map(|(_, ar)| *ar);
            }
        }
        let mut symbols: Vec<Symbol> = self
            .symbols
            .iter()
            .enumerate()
            .map(|(i, s)| skel_symbol(i, s))
            .collect();
        {
            let mut pairs: std::collections::HashMap<usize, Vec<usize>> = Default::default();
            for (i, s) in self.symbols.iter().enumerate() {
                if let Some(id) = s.pair {
                    pairs.entry(id).or_default().push(i);
                }
            }
            for at in pairs.values() {
                let [a, b] = at[..] else { continue };
                symbols[a].declared_with = Some(SymbolId(b as u32));
                symbols[b].declared_with = Some(SymbolId(a as u32));
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
                if self.symbols[idx].kind != DefCap::Enumerator {
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
        // A callable's body scope names the callable (`Scope::owner`). The
        // document mints `@scope.sub` and `@def.sub` / `@def.method` on the
        // same `function_definition` node, so the spans are equal; a shape
        // that declares on another node keeps no owner.
        {
            let by_span: std::collections::HashMap<(Point, Point), SymbolId> = symbols
                .iter()
                .rev()
                .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
                .map(|s| ((s.span.start, s.span.end), s.id))
                .collect();
            for sc in &mut self.scopes {
                if matches!(sc.kind, crate::model::file_analysis::ScopeKind::Sub { .. }) {
                    sc.owner = by_span.get(&(sc.span.start, sc.span.end)).copied();
                }
            }
        }
        // The class whose method each scope is the body of, or sits inside:
        // the nearest callable that declares one names it, in-class and
        // out-of-line alike (a `void Buf<T>::reserve(…)` body is lexically at
        // file scope, and only the method symbol carries `Buf`), so a closure
        // in a method reads the method's, and a free function's body has
        // none: a lambda (no owner) is transparent. Scopes are pushed in id
        // order, so a parent's answer is in hand before its children's.
        let method_class: Vec<Option<String>> = {
            let mut out: Vec<Option<String>> = Vec::with_capacity(self.scopes.len());
            for sc in &self.scopes {
                let class = match sc.owner.and_then(|sid| symbols.get(sid.0 as usize)) {
                    Some(owner) => owner.package.clone().filter(|_| matches!(owner.kind, SymKind::Method)),
                    None => sc.parent.and_then(|p| out.get(p.0 as usize).cloned().flatten()),
                };
                out.push(class);
            }
            out
        };
        // A receiver parameter holds an object of its method's class (the
        // method-class table's answer for the scope it binds in: a header
        // parameter binds in the body), flagged and typed from its binding on.
        let mut receiver_witnesses: Vec<crate::model::witnesses::Witness> = Vec::new();
        {
            use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
            for &(_, span) in &self.receivers {
                let Some(sym) = symbols.iter_mut().find(|s| s.selection_span == span) else { continue };
                sym.flags |= crate::model::file_analysis::SymbolFlags::RECEIVER;
                let Some(class) = method_class.get(sym.scope.0 as usize).cloned().flatten() else { continue };
                receiver_witnesses.push(Witness {
                    attachment: WA::Variable { name: sym.name.clone(), scope: sym.scope },
                    source: WitnessSource::Builder("receiver".into()),
                    payload: WP::InferredType(InferredType::ClassName(class)),
                    span: Span { start: span.start, end: span.start },
                });
            }
        }
        // Implicit receiver: where the document says a body elides it, a bare
        // name is a member of that body's method class.
        let elided_class: Vec<Option<String>> = {
            let mut out: Vec<Option<String>> = Vec::with_capacity(self.scopes.len());
            for (i, sc) in self.scopes.iter().enumerate() {
                let class = if sc.implicit_receiver {
                    method_class[i].clone()
                } else {
                    sc.parent.and_then(|p| out.get(p.0 as usize).cloned().flatten())
                };
                out.push(class);
            }
            out
        };
        // What a use-map call names a function by (`function_identity`); the
        // earliest declaration of a name wins, as in `callee_sid`.
        let mut free_functions: FreeFunctions = Default::default();
        for s in symbols.iter().filter(|s| matches!(s.kind, SymKind::Sub)) {
            free_functions.entry((s.package.as_deref().unwrap_or(""), s.name.as_str())).or_insert(s.id);
        }
        // A free call under a use-map, from its callee as written and where
        // that token stands: the function it names and the identity a class
        // of that spelling has there. Its `Invoke` and its reference (an
        // imported name's too) each ask it of their own capture.
        let free_call = |written: &str, at: tree_sitter::Point| {
            let map = crate::model::file_analysis::UseMap::at(&self.use_rows, &self.namespace_ranges, &self.scopes, &names, at, &symbols)?;
            let function = function_identity(&map, &names, written, &free_functions);
            Some((function, map.resolve(written).identity(map.sep)))
        };
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
                if !matches!(sym.kind, SymKind::Method | SymKind::Sub) {
                    continue;
                }
                if let Some(ret) = &self.symbols[i].return_type {
                    // A return that MENTIONS the owning class's template
                    // params publishes the deferred receiver-substituting
                    // shape (`ParamOf` — lazy, like `RowOf`); a concrete
                    // class-shaped return edges into the alias graph (it may
                    // be a typedef) instead of committing the spelling;
                    // primitives are leaves. `TypeName` resolves the typedef
                    // or falls back to the same `ClassName`, so struct
                    // returns are unchanged and aliased returns chase.
                    // (edges-not-values)
                    let class_params = sym
                        .package
                        .as_deref()
                        .and_then(|p| template_params.get(p));
                    let pay = match class_params.and_then(|ps| param_return_expr(ret, ps)) {
                        Some(re) => WP::ReturnExpr(re),
                        None => match ret {
                            InferredType::ClassName(cn) => WP::Edge(WA::TypeName(cn.clone())),
                            other => WP::InferredType(other.clone()),
                        },
                    };
                    bag.push(mk(WA::Symbol(sym.id), pay, sym.span));
                }
                if matches!(sym.kind, SymKind::Method) {
                    if let Some(class) = &sym.package {
                        bag.push(mk(
                            WA::PackageSymbol { package: class.clone(), name: sym.name.clone() },
                            WP::Edge(WA::Symbol(sym.id)),
                            sym.span,
                        ));
                    }
                }
            }
            // A data member's VALUE rides its own `Field` attachment, which a
            // member read's `ValueHop` chases (`docs/adr/member-kinds.md`): an
            // edge to the declaration's `Variable`, or, for a type that
            // mentions the class's template parameters, the deferred shape
            // that substitutes the receiver's arguments (`item_: T` read on a
            // `Box<int>` is `int`).
            let field_types: std::collections::HashMap<(&str, crate::model::file_analysis::ScopeId), &InferredType> =
                self.field_types.iter().map(|(n, sc, t)| ((n.as_str(), *sc), t)).collect();
            for sym in symbols.iter().filter(|s| matches!(s.kind, SymKind::Field)) {
                let Some(class) = &sym.package else { continue };
                let deferred = field_types
                    .get(&(sym.name.as_str(), sym.scope))
                    .zip(template_params.get(class))
                    .and_then(|(t, ps)| param_return_expr(t, ps));
                let pay = match deferred {
                    Some(re) => WP::ReturnExpr(re),
                    None => WP::Edge(WA::Variable { name: sym.name.clone(), scope: sym.scope }),
                };
                // Class-wide, like the declaration's own type witness: a
                // method reads a member declared below it.
                let span = self.scopes.get(sym.scope.0 as usize).map_or(sym.span, |sc| sc.span);
                bag.push(mk(WA::Field { owner: class.clone(), name: sym.name.clone() }, pay, span));
            }
            for w in receiver_witnesses.drain(..) {
                bag.push(w);
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
            // Call-site value resolution: a call's value is an `Invoke` of
            // its callee on its arguments. A callee this file defines — a
            // `Class` (functional cast / constructor → the class instance), a
            // sub, or a function-like macro — is resolved here, and its
            // Symbol answers `ClassName` for a class and its return for a
            // callable. Any other name is looked up across files at query
            // time; one nothing defines answers nothing (no name-case guess —
            // an unknown uppercase macro leaves the enclosing `auto x =
            // F(..)` honestly untyped). Where calling a class constructs it
            // (`NameSpellings::class_call`), prefer a `Class` over a
            // like-named callable (the constructor is the stronger claim),
            // and a name looked up across files carries the class identity
            // it spells here, so a class declared under it elsewhere answers
            // too; elsewhere a class is no callee at all. A method is
            // never a free callee: a bare call in a member body reaches it
            // through `member_of`, with the class as its receiver.
            let use_map_at = |site: tree_sitter::Point| {
                crate::model::file_analysis::UseMap::at(&self.use_rows, &self.namespace_ranges, &self.scopes, &self.names, site, &symbols)
            };
            let class_identity_at = |written: &str, site: tree_sitter::Point| {
                let map = use_map_at(site)?;
                Some(map.resolve(written).identity(map.sep))
            };
            // A receiver token that names a class has that class as its
            // value; a call on it dispatches on that value like on any
            // receiver's.
            for r in &self.receiver_sites {
                let class = match r.names {
                    super::vocab::ReceiverCap::Enclosing => method_class.get(r.scope.0 as usize).cloned().flatten(),
                    super::vocab::ReceiverCap::Class => {
                        Some(class_identity_at(&r.text, r.span.start).unwrap_or_else(|| r.text.clone()))
                    }
                    super::vocab::ReceiverCap::Super => method_class
                        .get(r.scope.0 as usize)
                        .cloned()
                        .flatten()
                        .and_then(|class| self.relative_class(super::vocab::RelativeClass::Super, &class)),
                    super::vocab::ReceiverCap::Declared => symbols
                        .iter()
                        .find(|s| s.kind == SymKind::Class && s.selection_span == r.span)
                        .map(|s| s.name.clone()),
                };
                let Some(class) = class else { continue };
                bag.push(Witness {
                    attachment: WA::Expr(r.span),
                    source: WitnessSource::Builder("receiver_class".into()),
                    payload: WP::InferredType(InferredType::ClassName(class)),
                    span: r.span,
                });
            }
            let calls_construct = self.names.class_call == crate::model::file_analysis::ClassCall::Constructs;
            let callee_sid: std::collections::HashMap<&str, SymbolId> = {
                let mut m: std::collections::HashMap<&str, SymbolId> =
                    std::collections::HashMap::new();
                for s in &symbols {
                    match s.kind {
                        SymKind::Class if calls_construct => {
                            m.insert(s.name.as_str(), s.id);
                        }
                        SymKind::Sub => {
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
            for site in &self.call_sites {
                let (span, name) = (&site.span, &site.callee);
                // Identity/projection macro: the call's value IS its n-th
                // argument. Edge to the argument's own `Expr` witness rather
                // than the param-agnostic Symbol return (edges-not-values).
                if let Some(MacroReturnHint::Param(n)) =
                    site.receiver.is_none().then(|| hint_of.get(name.as_str()).copied()).flatten()
                {
                    if let Some(arg) = call_args.get(span).and_then(|a| a.get(*n as usize)) {
                        bag.push(mk(WA::Expr(*span), WP::Edge(WA::Expr(*arg)), *span));
                        continue;
                    }
                }
                let args = site.args.as_ref().map(|args| {
                    args.iter()
                        .map(|&(arg, spread)| {
                            if spread {
                                crate::model::witnesses::CallArg::Spread(WA::Expr(arg))
                            } else {
                                crate::model::witnesses::CallArg::One(WA::Expr(arg))
                            }
                        })
                        .collect()
                });
                // A member call dispatches on its receiver's type at query
                // time; a free call's callee is the file's own definition
                // when it has one.
                let (callee, receiver) = match site.receiver {
                    Some(recv) => {
                        let lookup = crate::model::witnesses::Lookup::Receiver;
                        let callee = match site.constructs {
                            true => crate::model::witnesses::Callee::Construct { lookup },
                            false => crate::model::witnesses::Callee::Method { name: name.clone(), lookup, constructs: false },
                        };
                        (callee, Some(WA::Expr(recv)))
                    }
                    None => match free_call(name, site.callee_at) {
                        Some((fq, identity)) => {
                            // under a use-map a class is filed under the
                            // identity its spelling names at the call
                            let local = free_functions
                                .get(&(fq.namespace.as_str(), fq.leaf.as_str()))
                                .or_else(|| callee_sid.get(identity.as_str()))
                                .copied();
                            (
                                crate::model::witnesses::Callee::Function {
                                    name: fq.leaf.clone(),
                                    local,
                                    member_of: elided_class.get(site.scope.0 as usize).cloned().flatten(),
                                    constructs: calls_construct.then(|| identity.clone()),
                                },
                                None,
                            )
                        }
                        None => (
                            crate::model::witnesses::Callee::Function {
                                name: name.clone(),
                                local: callee_sid.get(name.as_str()).copied(),
                                member_of: elided_class.get(site.scope.0 as usize).cloned().flatten(),
                                constructs: calls_construct.then(|| name.clone()),
                            },
                            None,
                        ),
                    },
                };
                bag.push(mk(WA::Expr(*span), WP::Invoke { callee, receiver, args }, *span));
            }
        }
        // ---- Local/param vars: a variable READ resolves to the nearest
        // visible Variable declaration by the lookup walk → local
        // goto-def + hover. Declarations are already Variable symbols
        // (`@def.local`/`@def.var`; a python class-body binding's Variable
        // twin); fields are members, reached through a receiver. A variable
        // a write declares is its callable's from the top of the body, so a
        // read above its first write (a loop's) reaches it.
        let mut defs_by_name: std::collections::HashMap<
            &str,
            Vec<(crate::model::file_analysis::ScopeId, crate::model::file_analysis::VariableDecl<'_, SymbolId>)>,
        > = std::collections::HashMap::new();
        for s in &symbols {
            if let Some(d) = s.variable_decl() {
                defs_by_name.entry(s.name.as_str()).or_default().push((s.scope, d));
            }
        }
        let mut local_refs: Vec<crate::model::file_analysis::Ref> = Vec::new();
        // Reads that found no LOCAL decl — a Variable ref is still minted for
        // each (below, once call/member spans are known) so query-time
        // cross-file resolution can chase it by name (a file-scope value like
        // a C enum constant or global is registered in the pack index; the use
        // resolves the same way a bare call does). rule #7: the token gets a
        // ref whether or not the def is local.
        let mut unresolved_reads: Vec<(String, crate::model::file_analysis::ScopeId, Span)> = Vec::new();
        // The binders' one rule (`variable_use`); a use it finds no
        // declaration for is unresolved.
        let resolve_var = |name: &String, chain: crate::model::file_analysis::LookupChain, span: &Span| {
            let decls = defs_by_name.get(name.as_str())?;
            let decls_in = |sc| decls.iter().filter(move |(s, _)| *s == sc).map(|(_, d)| *d);
            match crate::model::file_analysis::variable_use(name, span.start, chain, None, &names, decls_in) {
                crate::model::file_analysis::VariableUse::Declared(id) => Some(id),
                crate::model::file_analysis::VariableUse::Global { .. } | crate::model::file_analysis::VariableUse::GatedOur => None,
            }
        };
        // A token an assignment writes is that write, not a read of it.
        let written: std::collections::HashSet<(Point, Point)> = self
            .var_writes
            .iter()
            .map(|(_, _, sp)| (sp.start, sp.end))
            .chain(
                self.symbols
                    .iter()
                    .filter(|s| s.kind == DefCap::Assigned)
                    .map(|s| (s.name_start, s.name_end)),
            )
            .collect();
        for (name, write_scope, write_span) in &self.var_writes {
            local_refs.push(crate::model::file_analysis::Ref {
                kind: crate::model::file_analysis::RefKind::Variable,
                span: *write_span,
                scope: *write_scope,
                target_name: name.clone(),
                access: crate::model::file_analysis::AccessKind::Write,
                binding: resolve_var(
                    name,
                    crate::model::file_analysis::write_lookup_chain(&self.scopes, *write_scope, name),
                    write_span,
                )
                .map(crate::model::file_analysis::RefBinding::Symbol),
                folded_from: None,
                arg_count: None,
            });
        }
        for (name, read_scope, read_span) in &self.var_reads {
            if written.contains(&(read_span.start, read_span.end)) {
                continue;
            }
            match resolve_var(
                name,
                crate::model::file_analysis::variable_lookup_chain(&self.scopes, *read_scope, name),
                read_span,
            ) {
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
        // Variable on the lookup chain, NO declared-before constraint (a
        // forward goto is valid).
        for (name, ref_scope, ref_span) in &self.label_refs {
            let Some(cands) = defs_by_name.get(name.as_str()) else { continue };
            let resolved: Option<SymbolId> = crate::model::file_analysis::lookup_chain(&self.scopes, *ref_scope)
                .scopes()
                .find_map(|sc| cands.iter().find(|(s, _)| *s == sc).map(|(_, d)| d.id));
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
        let write_sites: std::collections::HashSet<(Point, Point)> =
            self.write_sites.iter().map(|sp| (sp.start, sp.end)).collect();
        let mut refs: Vec<crate::model::file_analysis::Ref> = self
            .refs
            .iter()
            .filter_map(|r| {
                use crate::model::file_analysis::{RefBinding, RefKind};
                use super::vocab::RefCap;
                let mut span = Span { start: r.start, end: r.end };
                let mut binding = None;
                let kind = match r.kind {
                    // Under a use-map a call, and an imported name, is pinned
                    // to its function's namespace (`\Lib\f()` → `Lib`).
                    RefCap::Call | RefCap::Import => {
                        if let Some((function, _)) = free_call(&r.name, r.start) {
                            let namespace = function.namespace.clone();
                            binding = (!namespace.is_empty()).then_some(RefBinding::Function { package: namespace });
                        }
                        match r.kind {
                            RefCap::Import => RefKind::ImportedName,
                            _ => RefKind::FunctionCall,
                        }
                    }
                    // Qualified call (`fmt::format_to(...)`): Perl parity —
                    // the full path rides `target_name`, the qualifier the
                    // `Function` binding, and the span narrows to the bare
                    // name token (the rename/highlight unit; also what
                    // suppresses the `@expr.read.var` duplicate at the same
                    // start). The tail segment is an identifier, so it never
                    // spans rows — the end-anchored column math is safe.
                    //
                    // TODO(cpp-qualified-call-spelled): this is `@ref.call` on
                    // the leaf token plus `@ref.spelled` on the path, as php's
                    // qualified calls are, but cpp nests a qualifier per
                    // segment (`a::b::f` is `a::(b::f)`, a template call's
                    // leaf one level deeper), so the leaf needs a link walk
                    // down the `name:` fields the document can state.
                    RefCap::QCall => {
                        let (pkg, bare) = crate::model::file_analysis::split_qualified(&r.name, &names);
                        let pkg = pkg?;
                        span.start = tree_sitter::Point {
                            row: r.end.row,
                            column: r.end.column.saturating_sub(bare.len()),
                        };
                        binding = Some(RefBinding::Function { package: pkg.to_string() });
                        RefKind::FunctionCall
                    }
                    // `recv.field` / `recv->field`: a value read. The
                    // invocant types query-time via
                    // `expr_type_at_span(invocant_span)`.
                    RefCap::MemberValue => {
                        let (inv_span, invocant) = r.invocant.clone()?;
                        RefKind::FieldAccess {
                            invocant,
                            invocant_span: Some(inv_span),
                            member_name_span: Span { start: r.start, end: r.end },
                            member_op: r.member_op,
                        }
                    }
                    // `recv.f(…)`: the SAME MethodCall ref core resolves for
                    // Perl `$obj->m`; `find_definition`/`refs_to`/hover all
                    // flow from it.
                    RefCap::MemberCall => {
                        let (inv_span, invocant) = r.invocant.clone()?;
                        RefKind::MethodCall {
                            invocant,
                            invocant_span: Some(inv_span),
                            method_name_span: Span { start: r.start, end: r.end },
                            member_op: r.member_op,
                            named_by_string: false,
                            constructs: r.constructs,
                        }
                    }
                    // A type-position name (`Widget w;`, `struct op* o`, a
                    // base-class clause): the same PackageRef a Perl package
                    // use carries, so type gd/gr ride the Package machinery.
                    RefCap::Type => {
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
                    // A `goto` label resolves through `label_refs`; the
                    // remaining kinds stay skeleton-only rows.
                    RefCap::Label
                    | RefCap::Method
                    | RefCap::Var
                    | RefCap::Key => return None,
                };
                let access = match write_sites.contains(&(r.start, r.end)) {
                    true => crate::model::file_analysis::AccessKind::Write,
                    false => crate::model::file_analysis::AccessKind::Read,
                };
                Some(crate::model::file_analysis::Ref {
                    kind,
                    span,
                    scope: r.scope,
                    // A construction's token names the class it constructs,
                    // so its reference is retrieved under the class
                    // reference's key: the class its receiver's value states.
                    target_name: match r.constructs {
                        true => r
                            .invocant
                            .as_ref()
                            .and_then(|(at, _)| {
                                bag.iter_attachment(&crate::model::witnesses::WitnessAttachment::Expr(*at)).find_map(|w| match &w.payload {
                                    crate::model::witnesses::WitnessPayload::InferredType(InferredType::ClassName(c)) => Some(c.clone()),
                                    _ => None,
                                })
                            })
                            .unwrap_or_else(|| r.name.clone()),
                        false => r.name.clone(),
                    },
                    access,
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
        for (name, scope, span) in unresolved_reads {
            if claimed.contains(&(span.start.row, span.start.column, name.clone())) {
                continue;
            }
            local_refs.push(crate::model::file_analysis::Ref {
                kind: crate::model::file_analysis::RefKind::Variable,
                span,
                scope,
                target_name: name,
                access: crate::model::file_analysis::AccessKind::Read,
                binding: None,
                folded_from: None,
                arg_count: None,
            });
        }
        refs.extend(local_refs);
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
                    // The drill reads a stored member: a value access.
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
                            crate::model::file_analysis::FrozenReceiver::from_arms(
                                vec![crate::model::file_analysis::MethodTarget::Local {
                                    sym_id: *sym_id,
                                    invocant_class: class.to_string(),
                                }],
                                crate::model::file_analysis::ReceiverSource::Stated,
                                crate::model::file_analysis::ReceiverView::Local,
                            ),
                        )),
                        folded_from: None,
                        arg_count: None,
                    });
                }
            }
        }
        // Implicit receiver (`elided_class`): a bare name in a member body.
        // Its VALUE is the class's member when one answers, looked up at
        // query time across files and up the ancestry: a read projects the
        // class's slot, and a call is an `Invoke` with `member_of` (minted
        // with the call sites above). A field this file declares on the class
        // itself edges straight to its declaration, which keeps a template
        // class's own parameters spelled as themselves.
        //
        // This and the return arms below are pushed before
        // `finalize_post_walk` seals the enrichment baseline; above it, the
        // first enrichment truncates them away.
        //
        // TODO(implicit-receiver-nav): the BINDINGS below read this file's
        // symbols only, so goto-def / references on a bare use of a member
        // declared in a header or a base resolve it as a free name.
        {
            use crate::model::file_analysis::{RefKind, ScopeId};
            use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
            let implicit_class = |scope: ScopeId| elided_class.get(scope.0 as usize).and_then(|c| c.as_deref());
            let mut fields: std::collections::HashMap<(&str, &str), (ScopeId, SymbolId)> = Default::default();
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
            let mut bare_reads: Vec<(usize, String, Option<(SymbolId, ScopeId)>)> = Vec::new();
            let mut sibling_calls: Vec<(usize, String)> = Vec::new();
            for (i, r) in refs.iter().enumerate() {
                if matches!(r.kind, RefKind::Variable) && r.resolved_symbol().is_none() {
                    let Some(class) = implicit_class(r.scope) else { continue };
                    let local = fields.get(&(class, r.target_name.as_str())).map(|&(fscope, fsym)| (fsym, fscope));
                    bare_reads.push((i, class.to_string(), local));
                } else if matches!(r.kind, RefKind::FunctionCall) && r.binding.is_none() {
                    let Some(class) = implicit_class(r.scope) else { continue };
                    if methods.contains(&(class, r.target_name.as_str())) {
                        sibling_calls.push((i, class.to_string()));
                    }
                }
            }
            for (i, class, local) in bare_reads {
                // Pushed after the read's own edge to the name's `Variable`,
                // so the member answers first and a name no member answers (a
                // global, an enumerator) keeps that edge's value. The binding
                // answers goto-def / references / rename; one without the
                // other is the projection disagreement the consistency net
                // flags as I4.
                let name = refs[i].target_name.clone();
                let payload = match local {
                    Some((fsym, fscope)) => {
                        refs[i].bind_symbol(fsym);
                        WP::Edge(WA::Variable { name, scope: fscope })
                    }
                    None => WP::Projected {
                        base: WA::TypeName(class),
                        step: crate::model::witnesses::ProjectionStep::ValueHop { member: name },
                    },
                };
                bag.push(Witness {
                    attachment: WA::Expr(refs[i].span),
                    source: WitnessSource::Builder("implicit_field_read".into()),
                    payload,
                    span: refs[i].span,
                });
            }
            // A bare sibling call pinned to the class resolves through the same
            // package-scoped callable lookup a qualified `Class::foo()` uses. A
            // name no member answers stays free.
            for (i, class) in sibling_calls {
                refs[i].bind_function_package(class);
            }
        }
        // Implicit return: a callable whose syntax declares no return takes
        // its type from what it returns. `return_sites` is the extractor's
        // language-blind record (enclosing scope, returned expression); each
        // site becomes an arm of the nearest owning callable.
        {
            use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
            // Decided once per callable, from the bag as the declarations left
            // it: this loop writes the very `Symbol` witnesses a live check
            // would read, so the first arm would block every later one.
            let mut undeclared: std::collections::HashMap<SymbolId, bool> = Default::default();
            for (ret_scope, ret_span) in &self.return_sites {
                let Some(sid) = owning_callable(&self.scopes, *ret_scope) else { continue };
                let first = !undeclared.contains_key(&sid);
                let open = *undeclared
                    .entry(sid)
                    .or_insert_with(|| bag.for_attachment(&WA::Symbol(sid)).is_empty());
                if !open {
                    continue;
                }
                bag.push(Witness {
                    attachment: WA::SymbolReturnArm(sid),
                    source: WitnessSource::Builder("return_arm".into()),
                    payload: WP::Edge(WA::Expr(*ret_span)),
                    span: *ret_span,
                });
                // One chain edge per callable; its arms accumulate under it.
                if first {
                    bag.push(Witness {
                        attachment: WA::Symbol(sid),
                        source: WitnessSource::Builder("return_arm_chain".into()),
                        payload: WP::Edge(WA::SymbolReturnArm(sid)),
                        span: *ret_span,
                    });
                }
            }
        }
        let mut packages: std::collections::HashMap<
            String,
            crate::model::file_analysis::PackageFacts,
        > = std::collections::HashMap::new();
        for (child, parent) in &self.parents {
            packages.entry(child.clone()).or_default().parents.push(parent.clone());
        }
        // A class that defers its obligations to a composer (an interface, a
        // trait, an abstract class) is a role, and each contract it declares
        // is a require of it (docs/adr/role-contracts.md).
        for sym in &symbols {
            let defers = SymbolFlags::INTERFACE | SymbolFlags::TRAIT | SymbolFlags::ABSTRACT;
            if sym.kind == SymKind::Class && sym.flags.intersects(defers) {
                packages.entry(sym.name.clone()).or_default().is_role = true;
            }
            if sym.flags.contains(SymbolFlags::CONTRACT) {
                if let Some(pkg) = &sym.package {
                    let requires = &mut packages.entry(pkg.clone()).or_default().requires;
                    if !requires.contains(&sym.name) {
                        requires.push(sym.name.clone());
                    }
                }
            }
        }
        let pack = crate::model::file_analysis::PackFacts {
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
            include_directives: self
                .import_sites
                .drain(..)
                .map(|(raw, span)| (span, raw))
                .collect(),
            use_rows: std::mem::take(&mut self.use_rows),
            domain_sites: std::mem::take(&mut self.domain_sites),
            moved_from: std::mem::take(&mut self.moved_from),
            control_regions: std::mem::take(&mut self.control_regions),
            param_regions: std::mem::take(&mut self.param_regions),
            cond_arms: std::mem::take(&mut self.cond_arms),
            ..Default::default()
        };
        let fold_ranges = fold_ranges_of(&self.scopes);
        let mut fa = FileAnalysis::new(FileAnalysisParts {
            fold_ranges,
            scopes: self.scopes,
            symbols,
            refs,
            witnesses: bag,
            packages,
            pack,
            flow_edges: std::mem::take(&mut self.flow_edges),
            namespace_ranges: std::mem::take(&mut self.namespace_ranges),
            search_roots: std::mem::take(&mut self.search_roots),
            ..Default::default()
        });
        fa.language = self.language.to_string();
        // Seal base_*_count so a later enrich pass (the CLI/--batch path
        // runs it unconditionally) truncates to the FULL analysis, not to
        // zero — otherwise enrichment wipes every pack-language symbol.
        fa.finalize_post_walk();
        // Last, over the assembled analysis: an owner is a type, and the
        // registry answers types from the whole bag.
        declare_slots(&mut fa, slot_writes, &class_scopes);
        let writes_members = !member_writes.is_empty();
        settle_member_writes(&mut fa, member_writes, &class_scopes);
        // A settled write types the receivers that read the member, and the
        // seal's stamp froze them before it landed.
        if writes_members {
            fa.stamp_method_call_targets(None);
        }
        fa
    }
}

/// The free function `written` names at a use-map site, as its namespace
/// and leaf: a qualified spelling resolves like a class spelling, a bare
/// one by the language's `FunctionLookup`.
fn function_identity(
    map: &crate::model::file_analysis::UseMap,
    names: &crate::model::file_analysis::NameSpellings,
    written: &str,
    free_functions: &FreeFunctions,
) -> crate::model::file_analysis::QualifiedName {
    use crate::model::file_analysis::{FunctionLookup, QualifiedName};
    if crate::model::file_analysis::split_qualified(written, names).0.is_some() {
        return map.resolve(written);
    }
    match names.functions {
        FunctionLookup::Global => QualifiedName::new("", written),
        FunctionLookup::NamespaceThenGlobal => {
            let own = map.own_namespace.unwrap_or("");
            QualifiedName::new(if free_functions.contains_key(&(own, written)) { own } else { "" }, written)
        }
    }
}

/// A file's free functions by (namespace, leaf); the global namespace is `""`.
type FreeFunctions<'a> = std::collections::HashMap<(&'a str, &'a str), crate::model::file_analysis::SymbolId>;

/// The callable whose body a scope sits in: the nearest scope up the chain
/// that names its callable.
fn owning_callable(
    scopes: &[crate::model::file_analysis::Scope],
    scope: crate::model::file_analysis::ScopeId,
) -> Option<crate::model::file_analysis::SymbolId> {
    crate::model::file_analysis::scope_chain_of(scopes, scope)
        .into_iter()
        .find_map(|sc| scopes.get(sc.0 as usize).and_then(|s| s.owner))
}

/// Settle each write through a receiver. Its owner is the class the
/// receiver is typed as at the write, asked of the assembled analysis's own
/// registry (`inferred_type_via_bag`): the receiver parameter's class, an
/// annotated parameter's or local's annotation, a local's value
/// (`o = Counter()`). A write through an untyped receiver declares nothing.
/// A write declares the slot only when neither its class nor an ancestor
/// already declares the name — a class-body binding, a member, an earlier
/// slot of the class, or any slot write of an ancestor's — and every other
/// write stays the reference to that member it already is.
///
/// Runs after `finalize_post_walk`, adopting the slots into the sealed
/// baseline: it is the latest point a fact of the build can land. Anything
/// later is enrichment, which truncates back to that baseline.
///
/// TODO(slot-owner-deferred): an owner only the index can name — a
/// receiver typed by another file, a parent declared in another file — is
/// still decided here without it, so the declare rule sees the file's own
/// classes alone. The target is Perl's shape: the write is a ref whose
/// owner is a late binding (`fold.rs` `resolve_hash_key_owners` /
/// `fixup_call_bound_hash_key_owners`, `cursor_queries.rs`
/// `deferred_hash_key_owner`), and "declared" becomes the first write
/// whose decided owner is the class, possibly on the query-delayed-facts
/// deferred carrier.
///
/// TODO(assigned-unify): `unify_assigned` (extraction) is this rule for
/// variables over the write lookup chain. The two become one pass once a
/// slot's owner is a late binding too (`TODO(slot-owner-deferred)`): until
/// then a slot's owner needs the assembled analysis, a variable's does not.
fn declare_slots(
    fa: &mut crate::model::file_analysis::FileAnalysis,
    mut writes: Vec<SlotWrite>,
    class_scopes: &[(String, crate::model::file_analysis::ScopeId)],
) {
    use crate::model::file_analysis::{class_or_ancestor, SymKind};
    if writes.is_empty() {
        return;
    }
    writes.sort_by_key(|w| (w.symbol.name_start.row, w.symbol.name_start.column));
    // Each write's owner, by the receiver's type at the write.
    let owners: Vec<Option<String>> = writes
        .iter()
        .map(|w| {
            fa.inferred_type_via_bag(&w.receiver, w.symbol.start)
                .and_then(|t| t.class_name().map(str::to_string))
        })
        .collect();
    let written: std::collections::HashSet<(&str, &str)> = writes
        .iter()
        .zip(&owners)
        .filter_map(|(w, o)| Some((o.as_deref()?, w.symbol.name.as_str())))
        .collect();
    let mut accepted: std::collections::HashSet<(String, String)> = Default::default();
    let mut slots = Vec::new();
    for (w, owner) in writes.iter().zip(&owners) {
        let Some(class) = owner else { continue };
        let name = w.symbol.name.as_str();
        let declares = |c: &str| {
            fa.symbols_named(name).iter().map(|&sid| fa.symbol(sid)).any(|m| {
                m.package.as_deref() == Some(c) && matches!(m.kind, SymKind::Field | SymKind::Sub | SymKind::Method)
            }) || accepted.contains(&(c.to_string(), name.to_string()))
                || (c != class && written.contains(&(c, name)))
        };
        if class_or_ancestor(class, &fa.packages, declares) {
            continue;
        }
        accepted.insert((class.clone(), name.to_string()));
        let mut s = w.symbol.clone();
        s.package = Some(class.clone());
        if let Some((_, scope)) = class_scopes.iter().find(|(c, _)| c == class) {
            s.scope = *scope;
        }
        slots.push(skel_symbol(0, &s));
    }
    fa.adopt_path_symbols(slots);
}

/// Each member write is an assignment to the member of the class its
/// receiver's value is, asked of the assembled analysis's own registry, as
/// `declare_slots` asks for a slot's owner. A member this file declares on
/// that class or an ancestor takes the write the way a variable takes one:
/// into its declaration's `Variable`, whose fold ranks a declared type over
/// what flows in. Any other member — an instance slot, or one declared in
/// another file — takes it on `Field{owner, name}`, the attachment a member
/// read's `ValueHop` chases, where every write is one arm of the slot's
/// value. A write through a receiver of no known class states nothing.
///
/// Class-wide, as a member declaration's value is: a method above
/// `__init__` reads what `__init__` writes.
///
/// TODO(member-write-declared-elsewhere): a write to a member declared in
/// another file (a parent's annotated attribute) lands on the writer's
/// `Field`, and an answer there is found before the declaring file's, so
/// the write's value shadows the declared type. Deciding the declaration
/// needs the index, the same deferral as `TODO(slot-owner-deferred)`.
fn settle_member_writes(
    fa: &mut crate::model::file_analysis::FileAnalysis,
    writes: Vec<MemberWrite>,
    class_scopes: &[(String, crate::model::file_analysis::ScopeId)],
) {
    use crate::model::file_analysis::{class_or_ancestor, SymKind};
    use crate::model::witnesses::{Witness, WitnessAttachment as WA, WitnessPayload as WP, WitnessSource};
    // A receiver can be a member another write fills (`self.a.b = v` reads
    // `a`, which `self.a = A()` writes), so the writes settle to a fixed
    // point, clear-and-emit: each round re-derives every write against the
    // bag the last round left. Each round that moves settles a write it
    // could not before or retypes one, so the writes' count bounds it.
    let mut settled: Vec<Witness> = Vec::new();
    for _ in 0..=writes.len() {
        let mut values = Vec::new();
        for w in &writes {
            let Some(class) =
                fa.expr_type_at_span(w.receiver, None).and_then(|t| t.class_name().map(str::to_string))
            else {
                continue;
            };
            let mut declared = None;
            class_or_ancestor(&class, &fa.packages, |c| {
                declared = fa.symbols_named(&w.member).iter().map(|&sid| fa.symbol(sid)).find(|m| {
                    m.kind == SymKind::Field && m.package.as_deref() == Some(c)
                });
                declared.is_some()
            });
            let (attachment, scope) = match declared {
                Some(m) => (WA::Variable { name: m.name.clone(), scope: m.scope }, Some(m.scope)),
                None => (
                    WA::Field { owner: class.clone(), name: w.member.clone() },
                    class_scopes.iter().find(|(c, _)| *c == class).map(|&(_, scope)| scope),
                ),
            };
            values.push(Witness {
                attachment,
                source: WitnessSource::Builder(MEMBER_WRITE.into()),
                payload: WP::Edge(WA::Expr(w.value)),
                span: scope.map_or(w.value, |scope| fa.scope(scope).span),
            });
        }
        let same = |a: &[Witness], b: &[Witness]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.attachment == y.attachment && x.span == y.span)
        };
        if same(&values, &settled) {
            return;
        }
        fa.witnesses.remove_by_source_tag(MEMBER_WRITE);
        settled = values.clone();
        fa.adopt_witnesses(values);
    }
}

const MEMBER_WRITE: &str = "member_write";

/// The model symbol a skeleton def mints, as the `i`-th symbol.
fn skel_symbol(i: usize, s: &SkelSymbol) -> crate::model::file_analysis::Symbol {
    use crate::model::file_analysis::{Symbol, SymbolDetail, SymbolId};
    Symbol {
        id: SymbolId(i as u32),
        name: s.name.clone(),
        kind: s.kind.sym_kind(),
        span: Span { start: s.start, end: s.end },
        selection_span: Span { start: s.name_start, end: s.name_end },
        scope: s.scope,
        package: s.package.clone(),
        // A slot is its class's instance key, as Perl's `$self->{k}`.
        detail: match (s.kind, &s.package) {
            (DefCap::Slot, Some(class)) => SymbolDetail::HashKeyDef {
                owner: crate::model::file_analysis::HashKeyOwner::Class(class.clone()),
                is_dynamic: false,
            },
            _ => SymbolDetail::None,
        },
        namespace: crate::model::file_analysis::Namespace::Language,
        presentation: crate::model::file_analysis::Presentation {
            // An include-guard `#define` is compilation plumbing,
            // not a program entity — folded from listing views but
            // still resolvable (rule #7). The listing verdict is
            // stamped here so warm stub rebuilds mint it identically.
            hide_in_outline: symbol_flags_of(s.kind, &s.attributes)
                .contains(SymbolFlags::INCLUDE_GUARD),
            deprecation: None,
            doc: None,
            display: None,
            label: s.spelled.clone(),
        },
        attributes: {
            let mut a = s.attributes.clone();
            // The kind's marker (`union` for the hover-overlay /
            // outline-nesting consumers) — a value-borne property,
            // never a name test.
            a.extend(s.kind.marker().map(str::to_string));
            a
        },
        flags: symbol_flags_of(s.kind, &s.attributes) | s.flags,
        declared_with: None,
        deref_stack: s.deref_stack.clone(),
        arity: s.arity,
        arm: crate::model::file_analysis::ArmId::NONE,
    }
}

/// The closed flag set a pack symbol carries, from the skeleton's kind and
/// the pack's attribute vocabulary — the ONE place a pack's attribute
/// spellings become model facts (rule #12: the model asks the flag, never
/// the string).
fn symbol_flags_of(kind: DefCap, attributes: &[String]) -> SymbolFlags {
    let mut flags = SymbolFlags::empty();
    if let Some(f) = kind.marker().and_then(|m| SymbolFlags::try_from(m).ok()) {
        flags.insert(f);
    }
    // A variable an assignment declares is its callable's from the top of
    // the body (`defs::unify_assigned`).
    if kind == DefCap::Assigned {
        flags.insert(SymbolFlags::SCOPE_WIDE);
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

/// Folding follows the scopes the document minted, so an editor folds the
/// same structure navigation walks: every multi-line scope but the file is
/// a region, one per line pair (a definition and its body block often span
/// the same lines).
fn fold_ranges_of(scopes: &[crate::model::file_analysis::Scope]) -> Vec<crate::model::file_analysis::FoldRange> {
    use crate::model::file_analysis::{FoldKind, FoldRange, ScopeKind};
    let mut lines: Vec<(usize, usize)> = scopes
        .iter()
        .filter(|sc| !matches!(sc.kind, ScopeKind::File))
        .map(|sc| (sc.span.start.row, sc.span.end.row))
        .filter(|(start, end)| end > start)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
        .into_iter()
        .map(|(start_line, end_line)| FoldRange { start_line, end_line, kind: FoldKind::Region })
        .collect()
}
