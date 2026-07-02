//! Driver + registry: Perl always; C++ under `--features cpp`.

use super::*;

#[test]
fn perl_driver_analyzes() {
    let fa = PerlDriver.analyze("package Foo;\nsub bar { 1 }\n");
    assert!(fa.symbols.iter().any(|s| s.name == "bar"), "perl driver finds the sub");
}

#[test]
fn registry_serves_perl_by_default() {
    let reg = LanguageRegistry::with_enabled();
    assert!(reg.languages().contains(&"perl"));
    assert_eq!(reg.for_path(std::path::Path::new("Foo.pm")).map(|d| d.id()), Some("perl"));
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_driver_analyzes_through_reparse() {
    // a declarator-position macro that would otherwise destroy the class
    let src = "#define API __attribute__((visibility(\"default\")))\nclass API Box { public: int width; };\n";
    let fa = cpp_driver().analyze(src);
    assert!(fa.symbols.iter().any(|s| s.name == "Box"), "macro-recovered class: {:?}", fa.symbols.iter().map(|s| &s.name).collect::<Vec<_>>());
    assert!(fa.symbols.iter().any(|s| s.name == "width"));
    // The unknown-macro safety net: `API` isn't in the attribute-macro
    // vocabulary, so the class is recovered but carries NO signal.
    let boxsym = fa.symbols.iter().find(|s| s.name == "Box").unwrap();
    assert!(boxsym.attributes.is_empty(), "unknown macro → no signal: {:?}", boxsym.attributes);
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_known_attribute_macro_signals_the_recovered_class() {
    // A KNOWN declarator macro (Qt's Q_CORE_EXPORT, in the bundled
    // cpp-attributes vocabulary) recovers the class AND stamps its signal.
    let src = "class Q_CORE_EXPORT Widget { public: int x; };\n";
    let fa = cpp_driver().analyze(src);
    let widget = fa.symbols.iter().find(|s| s.name == "Widget")
        .unwrap_or_else(|| panic!("Widget recovered: {:?}", fa.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()));
    assert!(widget.attributes.contains(&"exported".to_string()),
        "Q_CORE_EXPORT signals exported: {:?}", widget.attributes);
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_deprecated_attribute_macro_signals_the_recovered_class() {
    let src = "class Q_DEPRECATED OldThing { public: int x; };\n";
    let fa = cpp_driver().analyze(src);
    let sym = fa.symbols.iter().find(|s| s.name == "OldThing").expect("OldThing recovered");
    assert!(sym.attributes.contains(&"deprecated".to_string()),
        "Q_DEPRECATED signals deprecated: {:?}", sym.attributes);
}

#[cfg(feature = "cpp")]
#[test]
fn registry_serves_cpp_when_enabled() {
    let reg = LanguageRegistry::with_enabled();
    assert!(reg.languages().contains(&"cpp"));
    assert_eq!(reg.for_path(std::path::Path::new("x.cpp")).map(|d| d.id()), Some("cpp"));
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_macro_recovered_spans_are_in_original_coords() {
    // A declarator-position macro expands to a long attribute, shifting
    // byte positions. The recovered `Box` symbol must point at the
    // ORIGINAL `Box`, not the expanded coordinate.
    let src = "#define API __attribute__((visibility(\"default\")))\nclass API Box { public: int width; };\n";
    let fa = cpp_driver().analyze(src);
    let boxsym = fa.symbols.iter().find(|s| s.name == "Box").expect("Box recovered");
    // original: `class API Box {` → Box at row 1, col 10
    let p = boxsym.selection_span.start;
    assert_eq!((p.row, p.column), (1, 10), "Box span in ORIGINAL coords: {:?}", p);
    // and the original source at that point really is "Box"
    let line = src.lines().nth(1).unwrap();
    assert_eq!(&line[p.column..p.column + 3], "Box");
}

#[test]
fn perl_trigger_chars_unchanged() {
    let tc = LanguageRegistry::with_enabled().trigger_chars();
    // The Perl reference set — a perl-only build must keep exactly these.
    for c in ["$", "@", "%", ">", ":", "{", "(", ","] {
        assert!(tc.iter().any(|s| s == c), "missing perl trigger {c}");
    }
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_adds_dot_trigger() {
    let tc = LanguageRegistry::with_enabled().trigger_chars();
    assert!(tc.iter().any(|s| s == "."), "cpp build should add '.' trigger: {tc:?}");
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_enumerator_carries_parent_enum_as_container_and_type() {
    use crate::file_analysis::InferredType;
    // Hovering an enum member surfaces its enum, the same `name: type` way a
    // struct field renders: `RED: Color`. Wired as the enumerator's container
    // (package) + type (ClassName of the enum).
    let fa = cpp_driver().analyze("enum Color { RED, GREEN };\n");
    let red = fa
        .symbols
        .iter()
        .find(|s| s.name == "RED")
        .unwrap_or_else(|| panic!("RED enumerator: {:?}",
            fa.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()));
    assert_eq!(red.package.as_deref(), Some("Color"),
        "enum member's container is its enum");
    assert_eq!(
        fa.inferred_type_via_bag("RED", red.span.start),
        Some(InferredType::ClassName("Color".to_string())),
        "enum member's type is its enum, so hover renders `RED: Color`"
    );
    // A bare `enum` (no @scope) keeps members in the enclosing scope, so a
    // later bare read of RED still resolves to this def.
    assert_eq!(red.scope, crate::file_analysis::ScopeId(0),
        "enumerators leak into the enclosing (file) scope");
}


#[cfg(feature = "cpp")]
#[test]
fn function_like_macro_types_from_its_body() {
    // The expansion flip: `SQ(3)` is LEFT as a call, so the macro is a package-
    // global sub the sub-return path types. `((x)*(x))` is a numeric expression
    // whatever `x` is (param-independent), so the use types integer.
    use crate::file_analysis::InferredType;
    let src = "#define SQ(x) ((x) * (x))\nvoid g(void) { auto b = SQ(3); }\n";
    let fa = cpp_driver().analyze(src);
    assert!(fa.symbols.iter().any(|s| s.name == "SQ"), "macro is a sub symbol");
    assert_eq!(
        fa.inferred_type_via_bag("b", tree_sitter::Point { row: 1, column: 20 }),
        Some(InferredType::Numeric),
        "SQ(3) types integer from its body, not a phantom `SQ` class",
    );
}

#[cfg(feature = "cpp")]
#[test]
fn delegation_macro_types_as_the_wrapped_functions_return() {
    // `#define WRAP(x) real(x)` — F's return IS G's return, an edge to the
    // callee's own return (the see-through value-witness, reusing the slice-1
    // delegation target).
    use crate::file_analysis::InferredType;
    let src = "int real(int x) { return x; }\n#define WRAP(x) real(x)\nvoid g(void) { auto d = WRAP(4); }\n";
    let fa = cpp_driver().analyze(src);
    assert_eq!(
        fa.inferred_type_via_bag("d", tree_sitter::Point { row: 2, column: 20 }),
        Some(InferredType::Numeric),
        "WRAP delegates to real → real's return type flows through",
    );
    // exactly one `real` sub (the dual @def.sub patterns dedup by span).
    assert_eq!(fa.symbols.iter().filter(|s| s.name == "real").count(), 1);
}

#[cfg(feature = "cpp")]
#[test]
fn class_content_gate_admits_members_not_locals() {
    // The refs-symmetry def→uses gate: a member (or role-macro member, or
    // enum constant) is the class's OWN content; a lexical local inside an
    // inline method carries the class as sticky `package` too and must NOT
    // pass, or find-references on its decl would fan out name-keyed
    // across the workspace.
    let fa = cpp_driver().analyze(
        "class Box {\npublic:\n  void grow() { int localx = 1; localx += 2; }\n  int width;\n};\nenum Color { RED, GREEN };\n",
    );
    let sym = |n: &str| fa.symbols.iter().find(|s| s.name == n).unwrap();
    assert!(fa.symbol_is_class_content(sym("width")), "direct member");
    assert!(fa.symbol_is_class_content(sym("RED")), "enum constant (leaked scope)");
    assert!(
        !fa.symbol_is_class_content(sym("localx")),
        "a local in an inline method has the class as sticky package but is NOT class content"
    );
    // Role-macro members (`#define BASEOP ... op_type ...`) live in a
    // parentless synthetic scope inside the macro's Class span.
    let src = std::fs::read_to_string("gold-corpus/cpp-fixture/member_block.cpp").unwrap();
    let fa = cpp_driver().analyze(&src);
    assert!(fa.symbol_is_class_content(sym_in(&fa, "op_type")), "role-macro member");
    assert!(fa.symbol_is_class_content(sym_in(&fa, "op_refcnt")), "role-macro member");
    assert!(!fa.symbol_is_class_content(sym_in(&fa, "o")), "function param");
}

#[cfg(feature = "cpp")]
fn sym_in<'a>(
    fa: &'a crate::file_analysis::FileAnalysis,
    n: &str,
) -> &'a crate::file_analysis::Symbol {
    fa.symbols.iter().find(|s| s.name == n).unwrap()
}

#[cfg(feature = "cpp")]
#[test]
fn file_scope_value_gate() {
    // `#define MAX 1` mints a file-scope Variable symbol; `int g;` is a
    // global; both are bare-name-keyed values (FileScopeValue targets). A
    // local never is.
    let fa = cpp_driver().analyze("#define MAX 1\nint g;\nvoid f() { int loc = MAX + g; }\n");
    assert!(fa.symbol_is_file_scope_value(sym_in(&fa, "MAX")));
    assert!(fa.symbol_is_file_scope_value(sym_in(&fa, "g")));
    assert!(!fa.symbol_is_file_scope_value(sym_in(&fa, "loc")));
    assert!(fa.names_macro_def("MAX", None));
    assert!(!fa.names_macro_def("g", None));
}

#[cfg(feature = "cpp")]
#[test]
fn type_uses_are_package_refs() {
    use crate::file_analysis::RefKind;
    // `Widget` in `Widget make_widget();` / `Widget global_w;` is a USE of
    // the type (rule #7) — a PackageRef, same as a Perl package-name use —
    // while the decl's own name token stays the Symbol's alone.
    let fa = cpp_driver().analyze("struct Widget { int w; };\nWidget make_widget();\nWidget global_w;\n");
    let type_refs: Vec<_> = fa
        .refs
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef) && r.target_name == "Widget")
        .collect();
    assert_eq!(type_refs.len(), 2, "two uses, decl-name suppressed: {type_refs:?}");
    assert!(type_refs.iter().all(|r| r.span.start.row >= 1));
}

#[cfg(feature = "cpp")]
#[test]
fn expanded_macro_uses_still_carry_refs() {
    use crate::file_analysis::RefKind;
    // An object-like value macro's uses are EXPANDED out of the parsed text;
    // the splice map re-mints a Variable read at each original site so
    // find-references on the `#define` still reaches them (rule #7/#9).
    let src = std::fs::read_to_string("gold-corpus/cpp-fixture/macro_refs.h").unwrap();
    let fa = cpp_driver().analyze(&src);
    let uses: Vec<_> = fa
        .refs
        .iter()
        .filter(|r| {
            matches!(r.kind, RefKind::Variable)
                && r.target_name == "MYFLAG"
                && r.span.start.row > 0
        })
        .map(|r| (r.span.start.row, r.span.start.column))
        .collect();
    assert_eq!(uses, vec![(1, 12), (2, 12), (2, 21)], "all three expanded uses: {uses:?}");
    // Member-block (role) macro uses are BLANKED, not expanded — the blank
    // diff re-mints those too.
    let src = std::fs::read_to_string("gold-corpus/cpp-fixture/member_block.cpp").unwrap();
    let fa = cpp_driver().analyze(&src);
    let baseop_uses = fa
        .refs
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Variable) && r.target_name == "BASEOP")
        .count();
    assert_eq!(baseop_uses, 2, "struct op {{ BASEOP }} and struct unop {{ BASEOP ... }}");
}
