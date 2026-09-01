//! `LangPack` and the per-language pack definitions: the query pack
//! plus the minimal host predicates patterns can't express.

use super::*;


/// Per-language bundle: the query pack plus host predicates. The
/// predicates are the official escape hatch — kept
/// MINIMAL on purpose so the findings honestly measure how far
/// patterns alone go.
pub struct LangPack {
    pub query_source: &'static str,
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
    /// Shape a captured name token's text (e.g. keep the sigil on a
    /// Perl variable). `capture_kind` is the vocabulary name
    /// (`def.var`, `ref.method`, ...) so one pack hook serves all.
    pub shape_name: fn(capture_kind: &str, raw: &str) -> String,
    /// Name for defs with no name token (anonymous subs).
    pub default_name: fn(kind: &str) -> Option<&'static str>,
    /// Map a `@type.annot` token's text to a type — the pack predicate
    /// for languages whose ring 3 is partly in the tree (`x: int`).
    pub annot_type: fn(text: &str) -> Option<InferredType>,
    /// Does a `@rettype` spelling name the RECEIVER rather than a concrete
    /// type (PHP `static`/`$this`/`self`)? The writeback then publishes
    /// `ReturnExpr::Receiver` so fluent builders chain — asked of the pack,
    /// never a name branch in the engine (rule #10).
    pub rettype_receiver: fn(text: &str) -> bool,
    /// Display vocabulary: engine type tag → this language's spelling
    /// (php `"HashRef"` → `"array"`). Rides `PackFacts.type_display`;
    /// every human surface translates through it. Empty = engine tags.
    pub type_display: &'static [(&'static str, &'static str)],
    /// Do this language's UNQUALIFIED parent names bind namespace-
    /// relatively (php: an un-imported `extends Base` in `namespace App`
    /// means `App\Base`, aliases and imports first)? Turns on the use-map
    /// parent resolution: `@parent` leaves resolve through the file's
    /// `@use.*` captures (alias → the real leaf) and every parent edge
    /// records its namespace for FQ chain validation. False = parents pass
    /// through verbatim with no namespace rows (Perl's package names are
    /// absolute; cpp identity is its own arc).
    pub namespace_relative_parents: bool,
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
    /// Are local variables FUNCTION-scoped (php: an assignment inside an
    /// `if` block declares for the whole function, and re-assignment is a
    /// REBIND of the same variable, not a fresh declaration)? Var defs
    /// then anchor to the nearest enclosing sub scope and same-scope
    /// re-assignments demote to write references — one identity per
    /// function, so references/rename see every site instead of
    /// per-assignment islands (round-3 R5: a rename from any island
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
    pub doc_types: fn(text: &str) -> Vec<DocFact>,
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

/// The declarator peel for C/C++ struct fields and locals: pointer/reference
/// wrappers, `field_identifier`/`identifier` leaves, recording the deref stack.
/// The cpp pack's `nested_peel` AND the member-block synth lane
/// (`cpp_reparse::synth_base`) both peel through this, so a pointer field's
/// `*`s are extracted by ONE walker whether the field was written plainly or
/// pasted from a `#define BASEOP` body (rule #10 — no second deref walker).
pub(crate) const C_FIELD_DECL_PEEL: PeelSpec = PeelSpec {
    wrappers: &[
        ("pointer_declarator", crate::model::file_analysis::DerefKind::Pointer),
        ("reference_declarator", crate::model::file_analysis::DerefKind::Reference),
    ],
    annot_kinds: &["type_qualifier"],
    leaf_to_def: &[("identifier", "def.local"), ("field_identifier", "def.field")],
    record_stack: true,
};

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
    /// `@var T` — the documented type of the property/variable below.
    Var(String),
    /// `@method [static] T name(...)` on a CLASS docblock — a documented
    /// virtual method (Laravel facades, Eloquent's `__call` surface). The
    /// join synthesizes a real method symbol on the class below, spanning
    /// the fact's own `@method` line (`line` = 0-based offset within the
    /// comment) so each row is a distinct, honest gd target.
    Method { name: String, ret: Option<String>, line: usize },
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

/// The Perl-on-query-engine seam (go-live map ARC 3, the builder.rs shrink):
/// not registered as a driver — the native builder still owns Perl — but the
/// parity tests in query_extract_tests.rs measure it against the builder so
/// the migration path stays proven.
#[allow(dead_code)]
pub fn perl_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/perl/skeleton.scm"),
        lang_id: "perl",
        bundled_entry_markers: &[],
        shape_name: |kind, raw| match kind {
            // The builder stores variable symbols WITH sigil; varname
            // captures are sigil-less. Predicate re-attaches nothing —
            // def.var captures the whole `(scalar)` node so raw text
            // already carries the sigil.
            _ => raw.to_string(),
        },
        default_name: |kind| match kind {
            "anon" => Some("(anon)"),
            _ => None,
        },
        annot_type: |_| None,
        rettype_receiver: |_| false,
        type_display: &[],
        namespace_relative_parents: false,
        field_registry_edges: false,
        super_receiver: |_| false,
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_| vec![],
        module_paths: |m| vec![format!("{}.pm", m.replace("::", "/"))],
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |_| vec![],
        narrow_guard: |_, _| None,
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        runtime_invoked_methods: &[],
        brace_scoped_members: false,
        trigger_chars: &["$", "@", "%", ">", ":", "{"],
        receiver_names: &[],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        qualifier_peel: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}

// Registered by `python_driver` only under `feature = "python"` (and driven by
// the pack tests); dead weight in a single-language build like `cpp`-only.
#[allow(dead_code)]
pub fn python_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/python/skeleton.scm"),
        lang_id: "python",
        bundled_entry_markers: &[],
        shape_name: |_, raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |text| match text.trim() {
            "str" => Some(InferredType::String),
            "int" | "float" => Some(InferredType::Numeric),
            "list" => Some(InferredType::ArrayRef),
            "dict" => Some(InferredType::HashRef),
            t if t.chars().next().is_some_and(|c| c.is_uppercase()) => {
                Some(InferredType::ClassName(t.to_string()))
            }
            _ => None,
        },
        rettype_receiver: |_| false,
        type_display: &[],
        namespace_relative_parents: false,
        field_registry_edges: false,
        super_receiver: |_| false,
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_| vec![],
        module_paths: |m| {
            let base = m.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__init__.py")]
        },
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |_| vec![],
        // `isinstance(x, Foo)` narrows x to Foo inside the guard.
        narrow_guard: |guard, ty| (guard == Some("isinstance")).then(|| InferredType::ClassName(ty.to_string())),
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        runtime_invoked_methods: &[],
        brace_scoped_members: false,
        trigger_chars: &["."],
        receiver_names: &["self", "cls"],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec {
            wrappers: &[("parenthesized_expression", crate::model::file_analysis::DerefKind::Pointer)],
            annot_kinds: &[],
            leaf_to_def: &[],
            record_stack: false,
        },
        // Python has one member operator (`.`), so no op-DX (op_map empty).
        op_map: &[],
        simple_var_kinds: &["identifier"],
        qualifier_peel: &[],
        member_kinds: &["attribute"],
        skip_kinds: &["string", "string_content", "comment", "concatenated_string"],
        call_kinds: &["call"],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}

// Live only under `feature = "r"` (or the pack tests); see `python_pack`.
#[allow(dead_code)]
pub fn r_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/r/skeleton.scm"),
        lang_id: "r",
        bundled_entry_markers: &[],
        shape_name: |_, raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
        rettype_receiver: |_| false,
        type_display: &[],
        namespace_relative_parents: false,
        field_registry_edges: false,
        super_receiver: |_| false,
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_| vec![],
        // No reliable lexical ctor convention in R (S4/R5 exist but
        // rare); class typing arrives via shapes and S3 later.
        // source("util.R") hands us the path verbatim; library(pkg)
        // resolves into the installed-library tree (a real install
        // would consult .libPaths() — not modeled here).
        module_paths: |m| vec![m.to_string()],
        shape_ctor: |callee| matches!(callee, "list" | "data.frame" | "tibble"),
        import_call: |callee, arg| match callee {
            "library" | "require" | "source" => Some(arg.to_string()),
            _ => None,
        },
        cmd_effects: |_| vec![],
        narrow_guard: |_, _| None,
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        runtime_invoked_methods: &[],
        brace_scoped_members: false,
        trigger_chars: &["$", "@", ":"],
        receiver_names: &[],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        qualifier_peel: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}

// Live only under `feature = "cmake"` (or the pack tests); see `python_pack`.
// Sole constructor of the `CmdEffect` variants.
#[allow(dead_code)]
pub fn cmake_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/cmake/skeleton.scm"),
        lang_id: "cmake",
        bundled_entry_markers: &[],
        shape_name: |_, raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
        rettype_receiver: |_| false,
        type_display: &[],
        namespace_relative_parents: false,
        field_registry_edges: false,
        super_receiver: |_| false,
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_| vec![],
        // include(util.cmake) is a literal path; add_subdirectory(src)
        // means src/CMakeLists.txt. The whole resolution strategy.
        module_paths: |m| {
            if m.ends_with(".cmake") {
                vec![m.to_string()]
            } else {
                vec![format!("{m}/CMakeLists.txt"), format!("{m}.cmake")]
            }
        },
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |cmd| match cmd.to_ascii_lowercase().as_str() {
            "set" | "option" => vec![CmdEffect::Def { kind: "var", name_arg: 0 }],
            "add_library" | "add_executable" | "add_custom_target" => {
                // Targets. SymKind::Target is the real future; "sub"
                // rides the full rename/refs machinery today.
                vec![CmdEffect::Def { kind: "sub", name_arg: 0 }]
            }
            "target_link_libraries" | "target_include_directories"
            | "target_compile_definitions" | "target_sources" => vec![
                CmdEffect::RefArgsFrom { from: 0 },
            ],
            "include" | "add_subdirectory" => vec![CmdEffect::Import { arg: 0 }],
            _ => vec![],
        },
        narrow_guard: |_, _| None,
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        runtime_invoked_methods: &[],
        brace_scoped_members: false,
        trigger_chars: &["{", "("],
        receiver_names: &[],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        qualifier_peel: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}

// Live only under `feature = "php"` (or the pack tests); see `python_pack`.
#[allow(dead_code)]
/// php's type-spelling predicate — declared syntax types AND phpdoc rows
/// both parse through here. A named fn (not a closure) because the
/// sequence spellings recurse on their element.
fn php_annot_type(text: &str) -> Option<InferredType> {
    use InferredType::*;
    let t = text.trim().trim_start_matches('?');
    if t.contains('|') || t.contains('&') {
        return None;
    }
    // Sequence spellings — `list<X>` / `array<X>` / `iterable<X>`,
    // `array<K, V>` (the element is V), `Type[]`. A homogeneous sequence
    // carries its element as a one-slot `Sequence` (`element_at(0)` and
    // the foreach `Element` peel both read it). The element recurses
    // through this same predicate, so `\App\User[]` leafs like any class
    // spelling. Without these arms the whole spelling fell to the
    // ClassName fallback and minted a bogus class `list<X>`.
    if let Some(inner) = t.strip_suffix("[]") {
        return php_annot_type(inner).map(|e| Sequence(vec![e]));
    }
    for prefix in ["list<", "array<", "iterable<"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            let inner = rest.strip_suffix('>')?;
            // `array<K, V>`: the element is the LAST top-level argument.
            let mut depth = 0usize;
            let mut last_start = 0usize;
            for (i, c) in inner.char_indices() {
                match c {
                    '<' | '{' | '(' => depth += 1,
                    '>' | '}' | ')' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => last_start = i + 1,
                    _ => {}
                }
            }
            return php_annot_type(&inner[last_start..]).map(|e| Sequence(vec![e]));
        }
    }
    match t {
        "string" => Some(String),
        "int" | "float" => Some(Numeric),
        "bool" | "false" | "true" => Some(Bool),
        "array" | "iterable" => Some(HashRef),
        "void" | "null" | "mixed" | "never" | "object" | "callable" | "self"
        | "static" | "parent" => None,
        t => {
            // `\App\Models\User` / `App\User` key by the unqualified
            // leaf — the same identity classes are filed under.
            let leaf = t.rsplit('\\').next().unwrap_or(t);
            (!leaf.is_empty()
                && leaf.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && !leaf.contains(['<', '>', '[', ']', '{', '}']))
            .then(|| ClassName(leaf.to_string()))
        }
    }
}

pub fn php_pack() -> LangPack {
    LangPack {
        // Base skeleton + the bundled framework overlays (pure query
        // vocabulary — see each overlay's header for the doctrine note).
        query_source: concat!(
            include_str!("../../../queries/php/skeleton.scm"),
            "\n",
            include_str!("../../../queries/php/frameworks/laravel.scm"),
            "\n",
            include_str!("../../../queries/php/frameworks/wordpress.scm"),
        ),
        lang_id: "php",
        bundled_entry_markers: &[
            include_str!("../../../queries/php/frameworks/phpunit.entry.json"),
            include_str!("../../../queries/php/frameworks/laravel.entry.json"),
            include_str!("../../../queries/php/frameworks/symfony.entry.json"),
        ],
        // variable_name captures carry the `$` (PHP spells it at every
        // use, like Perl); names/classes pass through verbatim. A
        // `self::`/`static::` receiver IS the enclosing class — spelled as
        // the model's current-package invocant token so relative static
        // dispatch resolves like Perl's `__PACKAGE__->` (late static
        // binding over-approximates to the writing class; accepted).
        // `parent::` stays a residual (needs the SUPER method-token lane).
        shape_name: |kind, raw| {
            if kind == "member.recv" && matches!(raw, "self" | "static") {
                return "__PACKAGE__".to_string();
            }
            // The chain-hop lane's receiver: `$this` is the enclosing class
            // instance, so a `$this->a()->b()` chain bases its first hop on
            // the class (`self`/`static` arrive already canonicalized by the
            // member.recv arm above). Scoped to hop shaping — the minted
            // ref's invocant keeps the written `$this` spelling.
            if kind == "hop.recv" && matches!(raw, "$this" | "self" | "static") {
                return "__PACKAGE__".to_string();
            }
            raw.to_string()
        },
        default_name: |kind| match kind {
            "anon" => Some("(anon)"),
            _ => None,
        },
        // Declared types (params, properties, returns) ARE the witness
        // source — PHP's gradual typing seeds the bag, inference covers
        // the untyped legacy tier. `?T` peels to T (nullability is not a
        // navigation fact); unions/intersections defer (None → the flow
        // edge carries); `self`/`static` receiver substitution is a
        // documented residual (needs ReturnExpr::Receiver plumbing).
        annot_type: php_annot_type,
        // `: static` / `: $this` are late-bound to the call's receiver —
        // fluent builders chain through `ReturnExpr::Receiver`. `self`
        // strictly means the defining class; substituting the receiver
        // over-approximates only for inherited methods (accepted).
        rettype_receiver: |text| {
            matches!(text.trim().trim_start_matches('?'), "static" | "$this" | "self")
        },
        namespace_relative_parents: true,
        field_registry_edges: true,
        super_receiver: |t| t == "parent",
        function_scoped_vars: true,
        constructor_names: &["__construct"],
        // phpdoc: the type vocabulary of REAL PHP — most of WordPress and
        // half of Laravel's public API type only here.
        doc_types: php_doc_types,
        // PHP's own spellings for the engine's value lattice; a PHP array
        // is one type whichever rep the engine inferred.
        type_display: &[
            ("String", "string"),
            ("Numeric", "int|float"),
            ("Bool", "bool"),
            ("HashRef", "array"),
            ("ArrayRef", "array"),
            ("Undef", "null"),
            ("CodeRef", "callable"),
        ],
        // PSR-4's real map lives in composer.json (autoload roots); the
        // one executable line is the namespace-mirrors-directories shape.
        module_paths: |m| {
            let base = m.trim_start_matches('\\').replace('\\', "/");
            vec![format!("{base}.php")]
        },
        // `['k' => v]` / `array('k', v)` construct keyed values; the
        // shape query gates on a string-keyed element, so these tokens
        // only ever arrive for genuinely keyed literals.
        shape_ctor: |callee| matches!(callee, "[" | "array"),
        import_call: |_, _| None,
        cmd_effects: |_| vec![],
        // `$x instanceof User` refines $x to User inside the guard.
        narrow_guard: |guard, ty| {
            (guard == Some("instanceof")).then(|| InferredType::ClassName(ty.to_string()))
        },
        rebind_method: |_| false,
        // `$this->` is mandatory — no receiver elision (unlike C++).
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        runtime_invoked_methods: &[
            "__toString", "__invoke", "__get", "__set", "__isset", "__unset",
            "__call", "__callStatic", "__clone", "__destruct", "__wakeup",
            "__sleep", "__serialize", "__unserialize", "__debugInfo",
            "__set_state", "__toBool",
        ],
        // class/trait/interface bodies are brace-delimited, so a member
        // orphaned by a misparse can re-anchor positionally.
        brace_scoped_members: true,
        trigger_chars: &["$", ">", ":"],
        receiver_names: &["$this"],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec {
            wrappers: &[("parenthesized_expression", crate::model::file_analysis::DerefKind::Pointer)],
            annot_kinds: &[],
            leaf_to_def: &[],
            record_stack: false,
        },
        // one meaningful member operator family (`->`/`?->`): no op-DX.
        op_map: &[],
        simple_var_kinds: &["variable_name"],
        qualifier_peel: &[],
        // calls included: PHP's method call is ONE flat node (unlike cpp,
        // where the call wraps a field_expression), so mid-token member
        // completion (`->ma|p`) must climb to the call node itself.
        member_kinds: &[
            "member_access_expression",
            "member_call_expression",
            "nullsafe_member_call_expression",
        ],
        skip_kinds: &["string", "string_content", "comment"],
        call_kinds: &[
            "function_call_expression",
            "member_call_expression",
            "scoped_call_expression",
            "nullsafe_member_call_expression",
            "object_creation_expression",
        ],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}

pub fn cpp_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/cpp/skeleton.scm"),
        lang_id: "cpp",
        bundled_entry_markers: &[],
        // Template spellings get ONE canonical whitespace form so a
        // specialization's identity (`formatter<int, char>`) matches
        // however the source wrapped it. Identity for every non-template
        // name (no whitespace, no comma → unchanged).
        shape_name: |_, raw| canonical_template_spelling(raw),
        // an anonymous inline union has no name token of its own; the
        // synthetic container is outline structure, not an addressable
        // member (the "anonymous" attribute keeps it out of completion).
        default_name: |kind| match kind {
            "unionfield" => Some("(union)"),
            _ => None,
        },
        // C++ declared types ARE the witness source. Primitives → the
        // value lattice; `auto`/`void` defer (None → edge carries);
        // anything else identifier-shaped is a class instance.
        annot_type: |text| {
            use InferredType::*;
            match text.trim() {
                "int" | "long" | "short" | "unsigned" | "size_t" | "int32_t" | "int64_t"
                | "uint32_t" | "uint64_t" | "double" | "float" | "char" => Some(Numeric),
                "bool" => Some(Bool),
                "std::string" | "string" | "std::string_view" => Some(String),
                "auto" | "void" => None,
                t => {
                    // Elaborated type specifier `struct op` / `union u` /
                    // `enum e` — the dominant C spelling (`struct op* o`).
                    // The tag names the type; strip the keyword so it resolves
                    // the same as the bare/typedef'd name.
                    let tag = t
                        .strip_prefix("struct ")
                        .or_else(|| t.strip_prefix("union "))
                        .or_else(|| t.strip_prefix("enum "))
                        .unwrap_or(t)
                        .trim();
                    // A template spelling (`Box<Widget>`, `vector<int>`)
                    // peels into the Instance flavor: dispatch keys the
                    // BASE so members resolve through the plain-class
                    // machinery; the args ride along for substitution.
                    if let Some(p) =
                        crate::model::file_analysis::ParametricType::instance_from_spelling(tag)
                    {
                        return Some(Parametric(p));
                    }
                    let typeish = !tag.is_empty()
                        && !tag.contains(' ')
                        && tag.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
                    // Strip the namespace qualifier — classes/members are
                    // keyed by the unqualified name (@context.class), so
                    // `geo::Circle` must type as `Circle` to resolve.
                    typeish.then(|| ClassName(tag.rsplit("::").next().unwrap_or(tag).to_string()))
                }
            }
        },
        rettype_receiver: |_| false,
        type_display: &[],
        namespace_relative_parents: false,
        field_registry_edges: false,
        super_receiver: |_| false,
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_| vec![],
        // #include "a/b.h" / <vector>: strip the delimiters; a quoted
        // path is workspace-relative verbatim, a system header resolves
        // through include dirs (library_roots, later). Tier 1: identity.
        module_paths: |m| {
            let p = m.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
            vec![p.to_string()]
        },
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |_| vec![],
        // Two narrowings, both keyed on what the value IS, not a name allowlist:
        //   `if (dynamic_cast<Derived*>(b))` — b is a Derived inside (ty is the
        //     template arg; pointer-ness dropped for navigation, like locals).
        //   `if (opt)` / `if (opt.has_value())` — an engaged std::optional<T>
        //     holds a T inside (ty is opt's DECLARED type; peel the inner T).
        //     The bare form carries no guard token; `.has_value()` gates the
        //     method so `opt.value_or(x)` (not an engagement test) won't narrow.
        narrow_guard: |guard, ty| {
            let class = match guard {
                Some("dynamic_cast") => ty.to_string(),
                None | Some("has_value") => optional_inner(ty)?,
                _ => return None,
            };
            Some(InferredType::ClassName(class))
        },
        // Rebinding methods: a moved-from object is put back into a known state
        // by these std container/optional/smart-ptr resets, so a use after one
        // is NOT a use-after-move. (An ordinary `x.use()` is not here, so the
        // canonical bug still flags.)
        rebind_method: |m| {
            matches!(m, "clear" | "reset" | "assign" | "emplace" | "swap")
        },
        // C/C++ methods read members with an implicit `this->`.
        implicit_this_members: true,
        include_path_tokens: true,
        preprocessor_macros: true,
        entrypoint_symbols: &["main"],
        runtime_invoked_methods: &[],
        brace_scoped_members: true,
        trigger_chars: &[".", ">", ":"],
        receiver_names: &["this"],
        // `field_identifier` only ever names a struct/class member (the
        // grammar's own distinction from a plain `identifier` local), so
        // "def.field" matches the plain (non-pointer) field pattern above.
        // Shared with the member-block synth lane (rule #10).
        nested_peel: C_FIELD_DECL_PEEL,
        // DerefKind placeholder — record_stack false, so it's never read.
        recv_peel: PeelSpec {
            wrappers: &[
                ("parenthesized_expression", crate::model::file_analysis::DerefKind::Pointer),
                ("pointer_expression", crate::model::file_analysis::DerefKind::Pointer),
            ],
            annot_kinds: &[],
            leaf_to_def: &[],
            record_stack: false,
        },
        op_map: &[
            ("->", crate::model::file_analysis::MemberOp::Arrow),
            (".", crate::model::file_analysis::MemberOp::Dot),
        ],
        simple_var_kinds: &["identifier"],
        // a templated qualifier (`Buf<T>::grow`) owns by its BASE class name
        qualifier_peel: &["template_type"],
        member_kinds: &["field_expression"],
        skip_kinds: &["string_literal", "char_literal", "raw_string_literal", "comment"],
        call_kinds: &["call_expression"],
        domain_compare_kinds: &["binary_expression"],
        domain_compare_ops: &["==", "!="],
        // out-of-line defs (`Ret Class::m(){}`): peel pointer/reference/
        // parenthesized returns to the function declarator, then walk the
        // qualified name to its leaf + owning class.
        oolfn: OutOfLineSpec {
            declarator_wrappers: &[
                "pointer_declarator",
                "reference_declarator",
                "parenthesized_declarator",
            ],
            function_declarator: "function_declarator",
            qualified_name: "qualified_identifier",
        },
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

/// phpdoc `@return` / `@param` / `@var` facts out of one `/** */` comment.
/// Only doc comments participate (a `//` or plain `/* */` never carries
/// the vocabulary); each tag line yields at most one fact.
fn php_doc_types(text: &str) -> Vec<DocFact> {
    if !text.starts_with("/**") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        // Normalize both spellings: a `* @param` continuation line and the
        // single-line `/** @return X */` form.
        let l = line
            .trim()
            .trim_start_matches('/')
            .trim_start_matches('*')
            .trim_end_matches('/')
            .trim_end_matches('*')
            .trim();
        if let Some(rest) = l.strip_prefix("@return ") {
            // `Base<static>` / `Base<self>` / `Base<$this>`: the value is
            // an instance of Base parametrized by the RECEIVER — a
            // deferred shape, not a strippable generic.
            let head = rest.split_whitespace().next().unwrap_or("");
            let recv_inst = head
                .strip_suffix('>')
                .and_then(|h| h.split_once('<'))
                .filter(|(_, arg)| matches!(*arg, "static" | "self" | "$this"))
                .and_then(|(base, _)| phpdoc_type(base))
                // Leafed: dispatch is leaf-keyed, and an FQ base
                // (`\Illuminate\...\Builder<static>`) would miss it.
                .map(|b| b.rsplit('\\').next().unwrap_or(&b).to_string());
            if let Some(base) = recv_inst {
                out.push(DocFact::ReturnRecvInstance { base });
            } else if let Some(t) = phpdoc_type(rest) {
                out.push(DocFact::Return(t));
            }
        } else if let Some(rest) = l.strip_prefix("@template ")
            .or_else(|| l.strip_prefix("@template-covariant "))
        {
            if let Some(name) = rest.split_whitespace().next() {
                if !name.is_empty()
                    && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                {
                    out.push(DocFact::Template { name: name.to_string(), line: lineno });
                }
            }
        } else if let Some(rest) = l
            .strip_prefix("@param ")
            .or_else(|| l.strip_prefix("@global "))
        {
            // `@global wpdb $wpdb` types the `global $wpdb;` binding the
            // def below declares — same (type, $name) shape as @param,
            // same join (the first var of that name declared in the def).
            // `@param string $name description`; the typeless `@param $x`
            // form and variadics (`...$args`) carry nothing typeable.
            let mut it = rest.split_whitespace();
            if let (Some(ty), Some(name)) = (it.next(), it.next()) {
                if name.starts_with('$') {
                    if let Some(t) = phpdoc_type(ty) {
                        out.push(DocFact::Param {
                            name: name.trim_end_matches(',').to_string(),
                            ty: t,
                        });
                    }
                }
            }
        } else if let Some(rest) = l.strip_prefix("@var ") {
            if let Some(t) = rest.split_whitespace().next().and_then(phpdoc_type) {
                out.push(DocFact::Var(t));
            }
        } else if let Some(rest) = l.strip_prefix("@method ") {
            // `@method [static] T name(args)`; the type is optional
            // (`@method foo()`), the name token is whatever carries the
            // `(`. `static` is dispatch surface, not a return spelling.
            let rest = rest.strip_prefix("static ").unwrap_or(rest);
            let mut it = rest.split_whitespace();
            if let Some(t0) = it.next() {
                let (ret, name_tok) = if t0.contains('(') {
                    (None, t0)
                } else {
                    match it.next() {
                        Some(t1) => (Some(t0), t1),
                        None => (None, t0),
                    }
                };
                let name = name_tok.split('(').next().unwrap_or("");
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c == '_' || c.is_ascii_alphanumeric())
                {
                    out.push(DocFact::Method {
                        name: name.to_string(),
                        ret: ret.and_then(phpdoc_type),
                        line: lineno,
                    });
                }
            }
        }
    }
    out
}

/// Normalize one phpdoc type expression to a spelling `annot_type` speaks:
/// generics stripped (`Collection<int,User>` → `Collection`), `User[]` is
/// an array, the `null` arm of a union dropped (`?T` too), a REAL union
/// (`string|false`) rejected — a two-armed claim is not a type answer.
fn phpdoc_type(raw: &str) -> Option<String> {
    let raw = raw.split_whitespace().next()?.trim_start_matches('?');
    let arms: Vec<&str> = raw
        .split('|')
        .filter(|a| !a.eq_ignore_ascii_case("null") && !a.is_empty())
        .collect();
    let [one] = arms.as_slice() else { return None };
    // Sequence spellings survive WHOLE — `annot_type` parses the element
    // (`list<X>` / `array<K,V>` / `iterable<X>` / `X[]` → a one-slot
    // `Sequence`); every other generic still strips to its base class
    // (`Collection<int,User>` → `Collection`).
    if one.ends_with("[]")
        || (one.ends_with('>')
            && ["list<", "array<", "iterable<"].iter().any(|p| one.starts_with(p)))
    {
        return Some(one.to_string());
    }
    let base = one.split('<').next().unwrap_or(one);
    (!base.is_empty()).then(|| base.to_string())
}

/// Peel `T` out of a `std::optional<T>` declared-type text, unqualified
/// (matching how `annot_type` keys classes by their last `::` segment). `None`
/// when the text isn't an optional — the type-side gate that keeps the
/// token-less `if (opt)` narrowing from firing on non-optional subjects.
fn optional_inner(ty: &str) -> Option<String> {
    let inner = ty
        .trim()
        .strip_prefix("std::optional<")
        .or_else(|| ty.trim().strip_prefix("optional<"))?
        .strip_suffix('>')?
        .trim();
    let leaf = inner.rsplit("::").next().unwrap_or(inner).trim();
    (!leaf.is_empty()
        && !leaf.contains(' ')
        && leaf.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
    .then(|| leaf.to_string())
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
