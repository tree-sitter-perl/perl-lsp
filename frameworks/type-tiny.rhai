// type-tiny: the Type::Tiny / Types::Standard constraint vocabulary used in
// `isa` (`has minion => (isa => InstanceOf['GenericCo::Minion'])`).
//
// A constraint constructor (`InstanceOf[...]`) is a *value* — a Type::Tiny
// object — not a class. The core types its call expression as
// `TypeConstraintOf(inner)` and an `isa => …` projects the inner (the
// constrained type) onto the accessor. This plugin owns the vocabulary:
// which names are constraints, and how each folds its params into the inner
// type. The core does the node-walking (rule #1) and hands over a flat param
// list; arity lives here.
//
// Scope: the "first string param is a class" shape — InstanceOf and its role
// twin ConsumerOf — plus `Maybe[T]`, a passthrough wrapper whose param is
// itself a constructor. That unlocks `$self->attr->method` chains where attr
// is `isa => InstanceOf['X']` or `isa => Maybe[InstanceOf['X']]`.
//
// Import scoping (docs/adr/type-constraints.md): when a package does `use Types::Standard
// qw/Str Int/`, those names become known via an Import action — suppressing
// unresolved-function diagnostics for explicitly imported type constants.
// `-all` / `:all` / bare use → full vocab emitted.  `GenericCo::Types` and
// other Type::Library re-exporters ride the same seam via their own plugin's
// `on_use` emitting `SyntheticUse "Types::Standard"`.

fn id() { "type-tiny" }

fn api() { 1 }

// Manifest hooks are global (consulted by name); the triggers gate only the
// patterns, which read a type library's own declarations.
fn triggers() {
    [ #{ UsesModule: "Type::Library" }, #{ UsesModule: "Type::Utils" } ]
}

// A type library's declarations, so a consumer's `is_X($v)` reads the X the
// library declares. Type::Library's `add_type(name => X, parent => P, …)` (on
// any receiver: `$meta`, `__PACKAGE__`, `__PACKAGE__->meta` — the
// `UsesModule` trigger is the gate) and Type::Utils' `declare X, as P, …`.
// X constrains no more than P does as far as a type can say: a constraint's
// own `where` refines a value no `InferredType` can follow, so X answers P's
// constraint value.
fn patterns() {
    [
        #{
            name: "add_type",
            query: `
(method_call_expression
  method: (_) @verb
  arguments: [
    (list_expression) @args
    (anonymous_hash_expression (list_expression) @args)
  ]
  (#eq? @verb "add_type"))
`,
            projections: #{ args: ["args"] },
            expect: [
                #{ src: "__PACKAGE__->add_type(name => Foo, parent => Str);", matches: 1 },
                #{ src: "$meta->add_type({ name => 'Foo', parent => Int });", matches: 1 },
                #{ src: "$meta->add_type;", matches: 0 },
            ],
        },
        #{
            name: "declare",
            // `as P, where {…}` passes its args as a list; `as InstanceOf[…]`
            // passes the one call bare. `parent` flattens either, so its
            // first arg is P.
            query: `
([
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @name . [
      (ambiguous_function_call_expression function: (_) @as arguments: (_) @parent)
      (function_call_expression function: (_) @as arguments: (_) @parent)
    ]))
  (function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @name . [
      (ambiguous_function_call_expression function: (_) @as arguments: (_) @parent)
      (function_call_expression function: (_) @as arguments: (_) @parent)
    ]))
]
(#eq? @verb "declare")
(#eq? @as "as"))
`,
            projections: #{ name: ["str"], parent: ["args"] },
            expect: [
                #{ src: "declare Foo, as Str, where { 1 };", matches: 1 },
                #{ src: "declare 'Foo', as InstanceOf['X'];", matches: 1 },
                #{ src: "declare(\"Foo\", as(Str));", matches: 1 },
                #{ src: "declare Foo;", matches: 0 },
                #{ src: "declare Foo, where { 1 };", matches: 0 },
            ],
        },
    ]
}

fn on_match(pattern, m) {
    let pkg = m["package"];
    if pkg == () { return []; }
    if pattern == "add_type" {
        let args = m.captures.args;
        if args == () || args.args == () { return []; }
        let name = ();
        let name_span = ();
        let parent = ();
        for pair in classified_pairs(args.args, 0) {
            if pair.key == "name" && pair.value.Str != () {
                name = pair.value.Str;
                name_span = pair.value_span;
            }
            if pair.key == "parent" { parent = pair.value_span; }
        }
        return declared_constraint(pkg, name, name_span, parent);
    }
    if pattern == "declare" {
        let parent = m.captures.parent;
        if parent == () || parent.args == () || parent.args.is_empty() { return []; }
        let name = m.captures.name;
        return declared_constraint(pkg, name.str, name.span, parent.args[0].span);
    }
    []
}

// The constant sub a declaration installs: calling `X` yields its parent's
// constraint value.
fn declared_constraint(pkg, name, span, parent) {
    if name == () || span == () || parent == () || !is_callable_sub_name(name) { return []; }
    [#{
        Method: #{
            name: name,
            span: span,
            selection_span: span,
            params: [],
            is_method: false,
            return_type: (),
            doc: (),
            on_class: pkg,
            display: (),
            hide_in_outline: false,
            opaque_return: false,
            outline_label: (),
            return_via_edge: #{ Expr: parent },
            attr: (),
        }
    }]
}

// Which call names are constraint constructors. The 0-arity base constants
// are constraint VALUES too — `isa => Int` is a Type::Tiny object exactly
// like `InstanceOf['X']` — so they belong here, and core gates them on the
// enclosing package having IMPORTED the name (a `use Types::Standard
// qw/Str Int/`, or the full vocab from `-all`). Without that scoping a
// user's own `sub Str` anywhere would type as a constraint; with it, only
// packages that actually pulled the name in are affected.
fn type_constraint_names() {
    ["InstanceOf", "ConsumerOf", "Maybe",
     "Str", "Int", "Num", "LaxNum", "StrictNum",
     "ArrayRef", "HashRef", "CodeRef", "RegexpRef", "Undef"]
}

// The rep a 0-arity base constant constrains to; () for names we cannot
// express. A parameterized container (`ArrayRef[Int]`) folds to its base
// rep — the element type has no InferredType slot to ride until
// sequence-types lands (docs/prompt-sequence-types.md).
fn base_constant_type(name) {
    if name == "Str" { return type_string(); }
    if name == "Int" || name == "Num" || name == "LaxNum" || name == "StrictNum" {
        return type_numeric();
    }
    if name == "ArrayRef" { return type_arrayref(); }
    if name == "HashRef" { return type_hashref(); }
    if name == "CodeRef" { return type_coderef(); }
    if name == "RegexpRef" { return type_regexp(); }
    if name == "Undef" { return type_undef(); }
    // Type::Tiny's Bool admits undef alongside 1, 0 and "".
    if name == "Bool" { return type_optional(type_bool()); }
    ()
}

// Names this vocabulary OWNS whose inner no `InferredType` describes:
// `Object` is "a blessed reference", `Any`/`Item` constrain nothing. They are
// constraints — typing them as user classes named `Object` is the bug this
// list exists to prevent — but they project no accessor type.
fn opaque_constraint_names() {
    ["Any", "Item", "Object", "Defined", "Ref", "Value",
     "ClassName", "RoleName", "GlobRef", "FileHandle", "ScalarRef",
     "Enum", "Dict", "Tuple", "Map", "Overload", "InstanceOf",
     "ConsumerOf", "HasMethods", "StrMatch", "Optional", "Maybe"]
}

// Fold the constructor's params into the constrained INNER type, or () to
// decline. `params` is a list of #{ string, ty }. Arity is ours to handle.
// Fold a constructor to the constraint VALUE it produces, or () when the
// name is not ours. `type_constraint(())` says "a constraint of mine, inner
// not expressible" — distinct from declining, which lets Moose read the name
// as a class.
fn type_constraint_inner(name, params) {
    if name == "Maybe" || name == "Optional" {
        // `Maybe[T]` permits undef OR a T — the honest `Optional<T>`, which
        // the lattice now models. The single param is a nested constructor
        // core typed `TypeConstraintOf(T)`; project T's inner and lift.
        // A bare `Maybe` with no parameter is still a constraint.
        if params.is_empty() { return type_constraint(()); }
        let inner = constrained_inner(params[0].ty);
        if inner == () { return type_constraint(()); }
        return type_constraint(type_optional(inner));
    }
    // 0-arity base constants take no params, so this precedes the arity gate.
    let base = base_constant_type(name);
    if base != () { return type_constraint(base); }

    // InstanceOf['Foo'] / ConsumerOf['Role'] — first string param is the class.
    if name == "InstanceOf" || name == "ConsumerOf" {
        if params.is_empty() { return type_constraint(()); }
        let cls = params[0].string;
        if cls == () { return type_constraint(()); }
        return type_constraint(type_class(cls));
    }

    // A name we own but cannot express an inner for. NOT a decline: reading
    // `Object` as a class named Object is exactly the miscarriage this
    // separation prevents.
    if opaque_constraint_names().contains(name) { return type_constraint(()); }

    // Not our vocabulary — the caller may legitimately read it as a class.
    ()
}

// ---- Import-scoped vocabulary ----

// Full export lists by module — authoritative from @EXPORT_OK at the
// installed version. These cover every name a user can legally import;
// the on_use hook narrows to exactly what they requested (or expands
// to all of them for -all / bare use). New type constants added to a
// future Types::Standard release land here, nowhere else.

// Types::Standard's base type constants. Each ships is_X / assert_X / to_X
// companions; the export list and the check guards both project from here.
fn types_standard_bases() {
    [
        "Any", "Item",
        "Bool",
        "Undef", "Defined", "Value",
        "Str", "LaxNum", "StrictNum", "Num", "Int",
        "ClassName", "RoleName",
        "Ref", "CodeRef", "RegexpRef", "GlobRef", "FileHandle",
        "ArrayRef", "HashRef", "ScalarRef",
        "Object",
        "Map", "Optional", "Slurpy", "Tuple", "CycleTuple", "Dict",
        "Maybe", "Overload", "StrMatch", "OptList", "Tied",
        "Enum",
        "InstanceOf", "ConsumerOf", "HasMethods",
    ]
}

fn types_standard_exports() {
    // Coercion / validation helpers: exported as plain functions, no companions.
    let standalone = ["slurpy", "MkOpt", "Join", "Split"];
    let out = [];
    for t in types_standard_bases() { out += t; out += "is_" + t; out += "assert_" + t; out += "to_" + t; }
    for t in standalone { out += t; }
    out
}

fn types_common_string_exports() {
    let base = [
        "SimpleStr", "NonEmptySimpleStr",
        "NonEmptyStr",
        "LowerCaseStr", "UpperCaseStr",
        "LowerCaseSimpleStr", "UpperCaseSimpleStr",
        "StrLength", "DelimitedStr",
        "NumericCode", "Password", "StrongPassword",
    ];
    let out = [];
    for t in base { out += t; out += "is_" + t; out += "assert_" + t; out += "to_" + t; }
    out
}

fn types_common_numeric_exports() {
    let base = [
        "PositiveNum", "PositiveOrZeroNum",
        "PositiveInt", "PositiveOrZeroInt",
        "NegativeNum", "NegativeOrZeroNum",
        "NegativeInt", "NegativeOrZeroInt",
        "SingleDigit",
        "NumRange", "IntRange",
    ];
    let out = [];
    for t in base { out += t; out += "is_" + t; out += "assert_" + t; out += "to_" + t; }
    out
}

// Type::Library gives every constraint X it declares the companions
// `is_X($v)` (checks `$v` against X) and `assert_X($v)` (dies unless it
// passes). Both guard `$v` as X's inner type, read from whichever library the
// call names: one listed in `constraint_libraries` folds X through
// `type_constraint_inner` (and `is_Object` guards nothing), any other answers
// from the X that library's own source declares.
fn type_check_functions() {
    [
        #{ prefix: "is_", asserts: false },
        #{ prefix: "assert_", asserts: true },
    ]
}

// Libraries this vocabulary speaks for. Types::Standard declares most of its
// types through `$meta->$add_core_type` and parents held in lexicals, which no
// pattern reads; Types::Common re-exports Types::Standard's.
fn constraint_libraries() {
    ["Types::Standard", "Types::Common"]
}

// `$type->check($v)` / `$type->assert_valid($v)` on a constraint VALUE
// (`Int`, `InstanceOf['Foo']`, a `$type` holding one): core narrows `$v` to
// whatever the receiver constrains, so a `check` on any other object proves
// nothing.
fn type_check_methods() {
    [
        #{ name: "check", asserts: false },
        #{ name: "assert_valid", asserts: true },
    ]
}

// `use Types::Standard qw/Str Int .../` → emit Import so the named constants
// are known in this package. The builder's `process_use` already pushes an
// Import entry for the literal qw-list, but that entry only covers what the
// user typed; this hook also handles `-all` / `:all` / bare use (where the
// qw-list is empty or contains a meta-import flag) by expanding to the full
// vocabulary. The same Import action is the hook that `GenericCo::Types` and other
// Type::Library re-exporters ride via `SyntheticUse "Types::Standard"` in
// their own plugin.
fn on_use(ctx) {
    let vocab = ();

    if ctx.module_name == "Types::Standard" {
        vocab = types_standard_exports();
    } else if ctx.module_name == "Types::Common::String" {
        vocab = types_common_string_exports();
    } else if ctx.module_name == "Types::Common::Numeric" {
        vocab = types_common_numeric_exports();
    } else if ctx.module_name == "Types::Common" {
        // Umbrella re-exporter: everything from all three sub-modules.
        vocab = types_standard_exports();
        for n in types_common_string_exports() { vocab += n; }
        for n in types_common_numeric_exports() { vocab += n; }
    } else {
        return [];
    }

    // Determine the effective import list. Meta-imports (-all, :all,
    // -default) and an empty list (bare `use`) both mean "all of vocab".
    // A genuine name list is used verbatim — it's already a subset of vocab,
    // and the caller is entitled to import names from any source; we don't
    // filter by our list so future additions don't break existing code.
    let requested = ctx.imports;
    let is_meta = requested.is_empty();
    if !is_meta {
        for r in requested {
            if r == "-all" || r == ":all" || r == "-default" || r.starts_with(":") {
                is_meta = true;
                break;
            }
        }
    }

    let names = if is_meta { vocab } else { requested };
    if names.is_empty() { return []; }

    let syms = [];
    for n in names { syms += #{ local_name: n }; }

    [#{
        Import: #{
            module_name: ctx.module_name,
            imported_symbols: syms,
            span: ctx.span,
        }
    }]
}
