//! `LangPack` and the per-language pack definitions: the query pack
//! plus the minimal host predicates patterns can't express.

use super::*;


/// Per-language bundle: the query pack plus host predicates. The
/// predicates are the official escape hatch — kept
/// MINIMAL on purpose so the findings honestly measure how far
/// patterns alone go.
pub struct LangPack {
    pub query_source: &'static str,
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
        shape_name: |_, raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
        rettype_receiver: |_| false,
        type_display: &[],
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
        shape_name: |_, raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
        rettype_receiver: |_| false,
        type_display: &[],
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
pub fn php_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/php/skeleton.scm"),
        // variable_name captures carry the `$` (PHP spells it at every
        // use, like Perl); names/classes pass through verbatim.
        shape_name: |_, raw| raw.to_string(),
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
        annot_type: |text| {
            use InferredType::*;
            let t = text.trim().trim_start_matches('?');
            if t.contains('|') || t.contains('&') {
                return None;
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
                        && leaf.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
                    .then(|| ClassName(leaf.to_string()))
                }
            }
        },
        // `: static` / `: $this` are late-bound to the call's receiver —
        // fluent builders chain through `ReturnExpr::Receiver`. `self`
        // strictly means the defining class; substituting the receiver
        // over-approximates only for inherited methods (accepted).
        rettype_receiver: |text| {
            matches!(text.trim().trim_start_matches('?'), "static" | "$this" | "self")
        },
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
