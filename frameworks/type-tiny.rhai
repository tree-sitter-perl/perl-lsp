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

// Manifest hooks are global (consulted by name), so triggers are irrelevant.
fn triggers() { [] }

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
    if name == "Bool" { return type_bool(); }
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

// `is_X($v)` checks `$v` against X; `assert_X($v)` dies unless it passes.
// Both guard `$v` as X's inner type: core folds `constraint` through
// `type_constraint_inner` and keeps the checks whose constraint has an inner
// (`is_Object` guards nothing). Types::Common re-exports Types::Standard's companions,
// so a check imported from either reaches the same entry.
fn type_check_functions() {
    let out = [];
    for lib in ["Types::Standard", "Types::Common"] {
        for t in types_standard_bases() {
            out += #{ "module": lib, name: "is_" + t, constraint: t, asserts: false };
            out += #{ "module": lib, name: "assert_" + t, constraint: t, asserts: true };
        }
    }
    out
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
