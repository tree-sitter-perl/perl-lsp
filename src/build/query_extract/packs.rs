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
    /// Does a `@rettype` spelling name the RECEIVER rather than a concrete
    /// type (PHP `static`/`$this`/`self`)? The writeback then publishes
    /// `ReturnExpr::Receiver` so fluent builders chain — asked of the pack,
    /// never a name branch in the engine (rule #10).
    pub rettype_receiver: fn(text: &str) -> bool,
    /// Field types answer through the registry: each data-member decl mints
    /// `PackageSymbol{class, field} → Edge(Variable)` so a property-access
    /// hop (`$this->query->where(...)`) dispatches the field and chains.
    /// True only where the registry IS the field-type authority (php).
    /// False for cpp: its field answers go through the instantiation-aware
    /// `member_value_type` lane (template-param substitution, typedef
    /// display), and a registry edge answers the RAW declared type first —
    /// `item_: T` instead of the substituted `int`.
    pub field_registry_edges: bool,
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
    /// Does a call to `callee` construct a KEYED value whose named
    /// arguments are `$`-style accessible keys? (R: list / data.frame.)
    pub shape_ctor: fn(callee: &str) -> bool,
    /// Languages where imports are CALLS, not statements (R's
    /// library()/source()): map (callee, argument) → imported module.
    pub import_call: fn(callee: &str, arg: &str) -> Option<String>,
    /// Command-dispatched languages (CMake): what a command DOES with
    /// its positional arguments. The @cmd/@cmd.arg captures deliver
    /// (name, ordered args); this predicate classifies.
    pub cmd_effects: fn(cmd: &str) -> Vec<CmdEffect>,
    /// Guard narrowing: given the guard token (`@narrow.guard` — a
    /// function/operator like `isinstance`, `has_value`; `None` for the
    /// token-less `if (opt)` truthiness form) and the type text, the
    /// refined type that holds inside the guarded block, or `None` if this
    /// guard doesn't narrow. The type text is the `@narrow.type` capture
    /// when the guard names one (`dynamic_cast<Derived*>`), else the
    /// subject's DECLARED type (the optional-engagement form reads
    /// `std::optional<T>` off the declaration and peels `T`). The pack owns
    /// "which guard means which refinement" (rule #10); core just scopes
    /// the witness to the block.
    pub narrow_guard: fn(guard: Option<&str>, type_text: &str) -> Option<InferredType>,
    /// Callees that ASSERT their argument (php `assert`): a guard passed to
    /// one narrows the rest of the enclosing scope. The `@narrow.assert`
    /// capture fires for any call around a guard; core honours only these.
    pub narrow_assertions: &'static [&'static str],
    /// Does calling `method` on a variable REBIND it — putting a moved-from
    /// object back into a known state (`clear`/`reset`/`assign`/…)? Used to end
    /// a moved-from region (and any narrowing) at the reset call, so a use after
    /// it is clean. Pack-owned language vocab (like `op_map`): core asks the
    /// value, never enumerates names itself.
    pub rebind_method: fn(method: &str) -> bool,
    /// Can a bare, receiver-less identifier resolve through an implicit
    /// `this->` — both a field read (`return inner_;` = `this->inner_`) AND a
    /// sibling method call (`foo()` = `this->foo()`)? True for C/C++ (the
    /// receiver is elided for both members and methods); false for Python/R
    /// (the receiver is mandatory for both). One language fact, not two: no
    /// language elides fields but not methods. Gates the member-access half of
    /// `language_driver::emit_return_fuel` — asked of the pack, never a
    /// language-name branch.
    pub implicit_this_members: bool,
    /// Does this language have `#include`-style path tokens — a source-path
    /// reference (the header IS the module, `#include` = `use`) that goto-def
    /// resolves to a file and references reverses ("who includes this
    /// header")? True for C/C++; false for languages whose imports are
    /// name-keyed (Perl `use`, Python `import`). Gates the include-token lanes
    /// in goto-def / references — asked of the pack, never a language-name
    /// branch (the token is path-shaped, not name-shaped, so it stays ahead of
    /// the name-keyed CandidateSet).
    pub include_path_tokens: bool,
    /// Does this language have a C-style preprocessor — `#define` macros
    /// reachable through `#include`s that identifier-context completion offers
    /// as an API surface? True for C/C++; false for languages with no
    /// preprocessor (Perl, Python, R, CMake). Gates `macro_completion` — asked
    /// of the pack, never a language-name branch (rule #10).
    pub preprocessor_macros: bool,
    /// Symbols the runtime enters from OUTSIDE the source graph (C/C++
    /// `main`: reached through the ABI, never a source call site) — a
    /// zero-fan-in callable with one of these names is alive by contract.
    /// Empty for languages whose entry is the file itself (Perl, Python
    /// scripts). Consumed by the heatmap's reachability guard — asked of
    /// the pack, never a name/language branch (rule #10).
    pub entrypoint_symbols: &'static [&'static str],
    /// Method names the RUNTIME invokes structurally (php magic methods —
    /// `__toString`, `__invoke`, `__get`, ...): zero in-repo call sites is
    /// the EXPECTED state, so the heatmap's dead-code flagging shields
    /// them (the method-shaped sibling of `entrypoint_symbols`). The
    /// constructor stays on its own lane (`constructor_names` — its call
    /// sites are real `new` refs, so an unconstructed ctor honestly flags).
    pub runtime_invoked_methods: &'static [&'static str],
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
    /// Class, interface and attribute names the language itself provides
    /// in the global namespace (php's core + SPL): a global reference to
    /// one is never a type missing its import.
    pub builtin_types: &'static [&'static str],
    /// Members every enum carries by language rule (php: `->value`,
    /// `->name`, `::cases()`, `::from()`, `::tryFrom()`).
    pub enum_members: &'static [&'static str],
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
    /// The pointer/reference DECLARATOR peel: a `@nested.target` chain
    /// flattened to its leaf + per-level deref stack — `Box**`, `char****`,
    /// `Box* const&`. THE recursion S-queries can't express (unbounded depth);
    /// the pack declares the grammar, the generic `peel` walks it.
    pub nested_peel: PeelSpec,
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
    /// `@qualifier` node kinds whose `name` FIELD supplies the owner text —
    /// the structural peel for a templated qualifier (`Buf<T>::grow` files
    /// under class `Buf`, unifying the out-of-line def with the in-class
    /// decl). Never string-splitting on `<`. Empty = qualifiers verbatim.
    pub qualifier_peel: &'static [&'static str],
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
    /// Out-of-line-definition extraction (`@ool.def` — a `Ret Class::method(...)`
    /// body owned by a `::` qualifier). The grammar the canonical declarator
    /// unwrap + qualifier walk consume; `OutOfLineSpec::OFF` = feature off.
    pub oolfn: OutOfLineSpec,
}

/// A declarative peel: descend a wrapper chain tree-sitter's fixed-depth
/// S-expression queries cannot express, to the leaf, optionally accumulating a
/// per-level deref stack. ONE combinator the pack parameterizes — `nested_peel`
/// (declarators, stack, leaf→def) and `recv_peel` (expr wrappers, no stack, any
/// leaf) are both instances of it. Empty `wrappers` = the capture is absent.
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

/// Out-of-line-definition extraction (`Ret Class::method(...) {...}` bodies —
/// the owner is named by a `::` qualifier, not lexical nesting). Declares the
/// three grammar shapes the driver's canonical unwrap + qualifier walk consume:
/// the declarator WRAPPERS peeled (any depth) to reach the function declarator,
/// the FUNCTION-DECLARATOR node whose `declarator` field carries the (possibly
/// multi-level) qualified name, and the QUALIFIED-NAME node kind the walk
/// descends. Empty `declarator_wrappers` = feature off (a pack that mints no
/// `@ool.def` capture).
#[derive(Clone, Copy)]
pub struct OutOfLineSpec {
    pub declarator_wrappers: &'static [&'static str],
    pub function_declarator: &'static str,
    pub qualified_name: &'static str,
}

impl OutOfLineSpec {
    pub const OFF: OutOfLineSpec = OutOfLineSpec {
        declarator_wrappers: &[],
        function_declarator: "",
        qualified_name: "",
    };
}

/// Peel declarator wrappers (`pointer_declarator`/`reference_declarator`/
/// `parenthesized_declarator`, ANY depth) to the inner function declarator —
/// the arbitrary nesting S-queries can't express (`Foo**& Class::m()`). THE
/// out-of-line unwrap, spelled once so no call site enumerates wrapper kinds.
/// `None` when no function declarator is reachable (not a function-def shape).
pub(super) fn unwrap_to_function_declarator<'a>(
    mut node: tree_sitter::Node<'a>,
    spec: &OutOfLineSpec,
) -> Option<tree_sitter::Node<'a>> {
    for _ in 0..32 {
        if node.kind() == spec.function_declarator {
            return Some(node);
        }
        if !spec.declarator_wrappers.contains(&node.kind()) {
            return None;
        }
        // pointer_declarator carries its inner under `declarator:`; a
        // reference/parenthesized declarator holds it as the first named child
        // (the `&`/parens are anonymous tokens).
        node = node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))?;
    }
    None
}

/// Walk a qualified-name chain (`A::B::c`) to its leaf name token, returning the
/// full scope text (`A::B`) and the leaf node. THE out-of-line owner walk: the
/// owning class is the innermost scope — `rsplit("::")` of the returned text, as
/// the `def.` handler already does for single-hop qualifiers — and the leaf is
/// the member/ctor/dtor/operator name. A scope segment whose kind is in
/// `peel_kinds` (a templated owner `Buf<T>`) contributes its `name` field's text
/// (`Buf`), the same structural peel the single-capture qualifier path applies —
/// never a string split on `<`. `None` when the node is not a qualified name (a
/// free function / in-class method — its own pattern owns it).
pub(super) fn walk_qualifier_chain<'a>(
    mut node: tree_sitter::Node<'a>,
    qualified_kind: &str,
    peel_kinds: &[&str],
    src: &[u8],
) -> Option<(String, tree_sitter::Node<'a>)> {
    if node.kind() != qualified_kind {
        return None;
    }
    let mut scopes: Vec<String> = Vec::new();
    for _ in 0..32 {
        if node.kind() != qualified_kind {
            return Some((scopes.join("::"), node));
        }
        if let Some(scope) = node.child_by_field_name("scope") {
            let seg = if peel_kinds.contains(&scope.kind()) {
                scope.child_by_field_name("name").unwrap_or(scope)
            } else {
                scope
            };
            scopes.push(seg.utf8_text(src).unwrap_or("").to_string());
        }
        node = node.child_by_field_name("name")?;
    }
    None
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

/// One effect of a command-dispatched statement.
// Variants are constructed only by `cmake_pack` (command languages) and read by
// the generic cmd-effect match; both absent in a build without that feature.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum CmdEffect {
    /// Argument `name_arg` declares an entity of `kind` ("var",
    /// "sub", ...).
    Def { kind: &'static str, name_arg: usize },
    /// Arguments from `from` onward are name references (all-caps
    /// keyword arguments like PRIVATE/STATIC are skipped — CMake's
    /// keyword convention; a finer filter is a later predicate).
    RefArgsFrom { from: usize },
    /// Argument `arg` names an imported module (joins import_call's
    /// role for command languages).
    Import { arg: usize },
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

