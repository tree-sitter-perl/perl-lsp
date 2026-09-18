//! `LangPack` — the pack CONTRACT: the query pack plus the minimal host
//! predicates patterns can't express. What each language declares lives
//! in `build::packs::<lang>`, re-exported here.

use super::*;
use crate::model::file_analysis::NameSpellings;

// The per-language declarations, re-exported so `query_extract::packs` stays
// the one path every caller spells.
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
    /// Does this receiver spelling mean "dispatch from the parent of the
    /// writing class, skipping it" (php `parent::`)? The ref is then
    /// minted with the model's SUPER method token (`SUPER::name`, the
    /// Perl `$self->SUPER::m` spelling) and a current-package invocant,
    /// so goto-def, references, and rename all ride the existing SUPER
    /// lane (`resolve_super_method`, refs_to's SUPER arm) — asked of the
    /// pack, never a name branch in the engine (rule #10).
    pub super_receiver: fn(text: &str) -> bool,
    /// Receiver tokens that name the ENCLOSING class itself for member
    /// access (php `self::` / `static::`): no typeable value node, the class
    /// is read off the cursor's scope chain — the `receiver_names` rule for
    /// a scoped access. Empty = none.
    pub self_class_tokens: &'static [&'static str],
    /// Node kinds of a bare CLASS TOKEN in receiver position (php `Foo::m(`,
    /// `App\Foo::CONST` — `name` / `qualified_name`): the receiver's value
    /// is the class it spells (leaf-keyed, like every class identity).
    /// Empty = none.
    pub class_token_kinds: &'static [&'static str],
    /// Are local variables FUNCTION-scoped (php: an assignment inside an
    /// `if` block declares for the whole function, and re-assignment is a
    /// REBIND of the same variable, not a fresh declaration)? Var defs
    /// then anchor to the nearest enclosing sub scope and same-scope
    /// re-assignments demote to write references — one identity per
    /// function, so references/rename see every site instead of
    /// per-assignment islands (a rename from any island
    /// rewrote a fragment and broke the code). False = block-scoped
    /// (cpp) or handled natively (Perl's `my`).
    pub function_scoped_vars: bool,
    /// The pack's constructor-method names (php `__construct`): a Method
    /// target with one of these names is the class's constructor, and its
    /// references include the class's `new Foo(...)` sites (non-rewritable
    /// — the token spells the class). Rides `PackFacts::constructor_names`.
    pub constructor_names: &'static [&'static str],
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
    /// Can a bare, receiver-less identifier resolve through an implicit
    /// `this->` — both a field read (`return inner_;` = `this->inner_`) AND a
    /// sibling method call (`foo()` = `this->foo()`)? True for C/C++ (the
    /// receiver is elided for both members and methods); false for Python/R
    /// (the receiver is mandatory for both). One language fact, not two: no
    /// language elides fields but not methods. Gates the member-access half of
    /// `language_driver::emit_return_fuel` — asked of the pack, never a
    /// language-name branch.
    pub implicit_this_members: bool,
    /// Container membership (class/struct/union/namespace) is delimited by
    /// literal `{`/`}` in the source, so a member that lost its enclosing
    /// container to a tree-sitter misparse can be re-anchored by matching the
    /// container's body braces on the ORIGINAL source
    /// (`reanchor_truncated_containers`). True for C/C++; false for
    /// indentation-scoped (Python) or non-nesting packs.
    /// `docs/adr/config-superposition-declarations.md`.
    pub brace_scoped_members: bool,
    /// The call expressions signature help can anchor on: node kind, the
    /// field naming the callee token, the field holding the argument list.
    /// Empty = the language declares no signature help.
    pub call_shapes: &'static [CallShape],
    /// Variables the runtime binds without a declaration (php's `$this`
    /// and superglobals): never "undefined".
    pub implicit_variables: &'static [&'static str],
    /// The language's THROWAWAY binding names (php `$_` in `foreach ($a
    /// as $k => $_)`): written to be discarded, so never "unused".
    pub throwaway_names: &'static [&'static str],
    /// Methods whose presence makes a class answer ANY member name
    /// (php `__call`/`__callStatic`, `__get`) — the undefined-member lanes
    /// stay silent on such a class, as Perl's do on `AUTOLOAD`.
    pub catch_all_methods: &'static [&'static str],
    /// The node kind of a first-class-callable placeholder in an argument
    /// list (php `f(...)` → `variadic_placeholder`): such a call passes no
    /// arguments, so it mints no count. Empty = none.
    pub callable_placeholder_kind: &'static str,
    /// The key/value arrow inside a list literal (php `'k' => $v`): what a
    /// destructuring slot's key is read before, and what makes a list keyed
    /// rather than positional. Empty for a language whose lists carry no
    /// written keys.
    pub pair_arrow: &'static str,
    /// The node kind of an argument SPREAD (php `f(...$args)` →
    /// `variadic_unpacking`): the call's count is unknowable, so it mints
    /// none and the arity lane stands down. Empty = none.
    pub spread_arg_kind: &'static str,
    /// Field a named argument carries its label under (php `f(name: 1)`):
    /// positional parameter hints stop at the first one. Empty = the pack
    /// has no named-argument form.
    pub named_arg_field: &'static str,
    /// An import row binds a NAME the file then spells (php `use A\B;`),
    /// as opposed to splicing text (`#include`). Only bound names can be
    /// unused.
    pub imports_bind_names: bool,
    /// The attribute that marks a declaration deprecated (php
    /// `#[Deprecated]`); empty = none. Lands as the `deprecated` symbol
    /// attribute exactly like the docblock tag.
    pub deprecated_attribute: &'static str,
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
    pub enum_members: &'static [EnumMember],
    /// The node kind of ONE argument inside a call's argument list (php
    /// `argument`); empty = every named child of the list is an argument.
    pub arg_kind: &'static str,
    /// Completion trigger characters for the LSP
    /// `completionProvider.triggerCharacters` slot — the client auto-fires
    /// completion (and reports the char in `CompletionContext`) when one is
    /// typed. C++ `. > :` cover `.`/`->`/`::`; the member path keys off them.
    pub trigger_chars: &'static [&'static str],
    /// The language's method-RECEIVER parameter names (Python `self`/`cls`,
    /// C++ `this`). A receiver param is lexically inside the class body, so
    /// the sticky class context tags it — but it is NOT a member. Extraction
    /// clears its package so it reads as a plain local. Lang-specific
    /// semantics → the pack owns it (NOT core `conventions.rs`, which is
    /// Perl's `$self`/`$class`).
    pub receiver_names: &'static [&'static str],
    /// The member-access RECEIVER peel: transparent expression wrappers
    /// (`(*p)`, `(&o)`, `(p)` → `p`) dropped so the invocant types via the
    /// inner. The SAME `peel`, no stack, any leaf.
    pub recv_peel: PeelSpec,
    /// Member-access node kinds (`receiver OP member`) — extraction records
    /// each site (simple-variable receiver, operator token span, `->` vs
    /// `.`) for the operator-DX consumer (`p.` on a `Box*` should be `->`).
    /// The member operator's grammar token KIND → the `MemberOp` it means
    /// (`"->"`→Arrow, `"."`→Dot). The `operator:` field of a member access is
    /// captured as `@member.op`; the engine maps its `kind()` through this
    /// table. An OPEN set: unmapped kinds (`.*`) get no op-DX, never a guess.
    /// Empty = no member-operator DX (Perl, single-operator packs).
    pub op_map: &'static [(&'static str, crate::model::file_analysis::MemberOp)],
    /// Simple-variable node kinds (`identifier`). op-DX fires ONLY when the
    /// IMMEDIATE member-access receiver is one — the receiver whose
    /// `deref_stack` resolves by name to decide the expected operator. Also the
    /// cursor-completion "is this receiver a bare variable" test.
    pub simple_var_kinds: &'static [&'static str],
    /// Names whose CALL makes the enclosing callable read arguments it never
    /// declared (php `func_get_args` / `func_num_args` / `func_get_arg`).
    /// The extractor stamps `SymbolFlags::DYNAMIC_ARGS` on the callable that
    /// contains such a call, so the arity lanes ask the callable rather than
    /// re-scanning its body. Empty = the language has no such surface.
    pub dynamic_arg_markers: &'static [&'static str],
    /// Names whose CALL makes the enclosing callable materialize variables no
    /// declaration names (php `extract` / `get_defined_vars` / `eval` /
    /// `parse_str` / `compact`). The extractor stamps
    /// `SymbolFlags::DYNAMIC_VARS` on the containing callable, which is what
    /// the undefined-variable lane asks. Empty = no such surface.
    pub dynamic_var_markers: &'static [&'static str],
    /// Member-access node kinds (`field_expression` / `attribute`): a `recv.m`
    /// the cursor-completion path climbs to + types the receiver of. Empty =
    /// no member-access completion (Perl uses `cursor_context`).
    pub member_kinds: &'static [&'static str],
    /// Node kinds the sentinel must NOT splice into (string/char/comment).
    pub skip_kinds: &'static [&'static str],
    /// Call-expression node kinds (`call_expression`/`call`) — a chained
    /// receiver `f().attr` types through the call's inner member.
    pub call_kinds: &'static [&'static str],
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
            super_receiver: _,
            self_class_tokens,
            class_token_kinds,
            function_scoped_vars: _,
            constructor_names,
            doc_types: _,
            doc_uses_method_tags,
            module_paths: _,
            import_module: _,
            narrow_type: _,
            implicit_this_members: _,
            brace_scoped_members: _,
            call_shapes,
            implicit_variables,
            throwaway_names,
            catch_all_methods,
            callable_placeholder_kind,
            pair_arrow,
            spread_arg_kind,
            named_arg_field,
            imports_bind_names: _,
            deprecated_attribute,
            bundled_builtin_types: _,
            enum_members,
            arg_kind,
            trigger_chars,
            receiver_names,
            recv_peel,
            op_map,
            simple_var_kinds,
            dynamic_arg_markers,
            dynamic_var_markers,
            member_kinds,
            skip_kinds,
            call_kinds,
            domain_compare_kinds,
            domain_compare_ops,
        } = self;
        let mut out: Vec<(&'static str, &'static str)> = Vec::new();
        fn list(
            out: &mut Vec<(&'static str, &'static str)>,
            field: &'static str,
            values: &'static [&'static str],
        ) {
            out.extend(values.iter().map(|v| (field, *v)));
        }
        list(&mut out, "self_class_tokens", self_class_tokens);
        list(&mut out, "class_token_kinds", class_token_kinds);
        list(&mut out, "constructor_names", constructor_names);
        list(&mut out, "doc_uses_method_tags", doc_uses_method_tags);
        list(&mut out, "implicit_variables", implicit_variables);
        list(&mut out, "throwaway_names", throwaway_names);
        list(&mut out, "catch_all_methods", catch_all_methods);
        out.extend(enum_members.iter().map(|m| ("enum_members", m.name)));
        list(&mut out, "trigger_chars", trigger_chars);
        list(&mut out, "receiver_names", receiver_names);
        list(&mut out, "simple_var_kinds", simple_var_kinds);
        list(&mut out, "dynamic_arg_markers", dynamic_arg_markers);
        list(&mut out, "dynamic_var_markers", dynamic_var_markers);
        list(&mut out, "member_kinds", member_kinds);
        list(&mut out, "skip_kinds", skip_kinds);
        list(&mut out, "call_kinds", call_kinds);
        list(&mut out, "domain_compare_kinds", domain_compare_kinds);
        list(&mut out, "domain_compare_ops", domain_compare_ops);
        for (field, one) in [
            ("callable_placeholder_kind", *callable_placeholder_kind),
            ("pair_arrow", *pair_arrow),
            ("spread_arg_kind", *spread_arg_kind),
            ("named_arg_field", *named_arg_field),
            ("deprecated_attribute", *deprecated_attribute),
            ("arg_kind", *arg_kind),
        ] {
            if !one.is_empty() {
                out.push((field, one));
            }
        }
        for c in *call_shapes {
            out.push(("call_shapes", c.kind));
            out.push(("call_shapes", c.callee_field));
            out.push(("call_shapes", c.args_field));
        }
        out.extend(recv_peel.wrappers.iter().map(|(k, _)| ("recv_peel", *k)));
        list(&mut out, "recv_peel", recv_peel.annot_kinds);
        for (leaf, _) in recv_peel.leaf_to_def {
            out.push(("recv_peel", *leaf));
        }
        out.extend(op_map.iter().map(|(k, _)| ("op_map", *k)));
        out.retain(|(_, v)| !v.is_empty());
        out
    }
}

/// A declarative peel: descend a wrapper chain tree-sitter's fixed-depth
/// S-expression queries cannot express, to the leaf, optionally accumulating a
/// per-level deref stack. The pack parameterizes it: `recv_peel` (expr
/// wrappers, no stack, any leaf) is what it carries. Empty `wrappers` = the
/// capture is absent.
#[derive(Clone, Copy)]
pub struct PeelSpec {
    /// Wrapper node kinds → the `DerefKind` each contributes (only consulted
    /// when `record_stack`; a placeholder otherwise).
    pub wrappers: &'static [(&'static str, crate::model::file_analysis::DerefKind)],
    /// Per-level annotation node kinds (cv-qualifiers) collected onto a step.
    pub annot_kinds: &'static [&'static str],
    /// Leaf node kind → the `def.*` capture the synthetic leaf event mints
    /// (`identifier`→`def.local`, `field_identifier`→`def.var`). EMPTY = accept
    /// ANY leaf and mint no def (the receiver-peel case — the leaf is an
    /// invocant, not a declaration).
    pub leaf_to_def: &'static [(&'static str, &'static str)],
    /// Accumulate the per-level `DerefStep` stack (pointer depth) vs descend only.
    pub record_stack: bool,
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

/// A call-expression shape signature help climbs to from the cursor
/// (`cursor_sentinel::call_at`).
#[derive(Debug, Clone, Copy)]
pub struct CallShape {
    pub kind: &'static str,
    /// Field naming the callee token (a member call's `name`, a function
    /// call's `function`); the LAST `name`-like descendant is the token.
    pub callee_field: &'static str,
    /// Field holding the argument list node.
    pub args_field: &'static str,
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

