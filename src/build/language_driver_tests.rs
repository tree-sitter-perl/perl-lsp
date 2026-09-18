//! Driver + registry: Perl always; C++ under `--features cpp`.

use super::*;

#[test]
fn perl_driver_analyzes() {
    let fa = PerlDriver.analyze("package Foo;\nsub bar { 1 }\n");
    assert!(fa.symbols().iter().any(|s| s.name == "bar"), "perl driver finds the sub");
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
    assert!(fa.symbols().iter().any(|s| s.name == "Box"), "macro-recovered class: {:?}", fa.symbols().iter().map(|s| &s.name).collect::<Vec<_>>());
    assert!(fa.symbols().iter().any(|s| s.name == "width"));
    // The unknown-macro safety net: `API` isn't in the attribute-macro
    // vocabulary, so the class is recovered but carries NO signal.
    let boxsym = fa.symbols().iter().find(|s| s.name == "Box").unwrap();
    assert!(boxsym.attributes.is_empty(), "unknown macro → no signal: {:?}", boxsym.attributes);
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_known_attribute_macro_signals_the_recovered_class() {
    // A KNOWN declarator macro (Qt's Q_CORE_EXPORT, in the bundled
    // cpp-attributes vocabulary) recovers the class AND stamps its signal.
    let src = "class Q_CORE_EXPORT Widget { public: int x; };\n";
    let fa = cpp_driver().analyze(src);
    let widget = fa.symbols().iter().find(|s| s.name == "Widget")
        .unwrap_or_else(|| panic!("Widget recovered: {:?}", fa.symbols().iter().map(|s| &s.name).collect::<Vec<_>>()));
    assert!(widget.attributes.contains(&"exported".to_string()),
        "Q_CORE_EXPORT signals exported: {:?}", widget.attributes);
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_deprecated_attribute_macro_signals_the_recovered_class() {
    let src = "class Q_DEPRECATED OldThing { public: int x; };\n";
    let fa = cpp_driver().analyze(src);
    let sym = fa.symbols().iter().find(|s| s.name == "OldThing").expect("OldThing recovered");
    assert!(sym.attributes.contains(&"deprecated".to_string()),
        "Q_DEPRECATED signals deprecated: {:?}", sym.attributes);
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_include_guard_define_is_hidden_from_outline_but_resolvable() {
    // `#ifndef X` / `#define X` include guards are compilation plumbing —
    // folded from outline / workspace-symbol, but the symbol survives so
    // goto-def / references still resolve (rule #7).
    let src = "#ifndef FOO_BAR_H_\n#define FOO_BAR_H_\nint real_thing;\n#endif\n";
    let fa = cpp_driver().analyze(src);
    let guard = fa.symbols().iter().find(|s| s.name == "FOO_BAR_H_")
        .expect("guard symbol still exists (resolvable)");
    assert!(guard.hidden_in_outline(), "include guard hidden from listing views");
    assert!(guard.attributes.iter().any(|a| a == "include_guard"),
        "guard carries the value-borne marker: {:?}", guard.attributes);
    // A non-guard object-like macro stays visible.
    let src2 = "#define MAXLEN 100\nint real_thing;\n";
    let fa2 = cpp_driver().analyze(src2);
    if let Some(m) = fa2.symbols().iter().find(|s| s.name == "MAXLEN") {
        assert!(!m.hidden_in_outline(), "a plain object-like macro is NOT hidden");
    }
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
    let boxsym = fa.symbols().iter().find(|s| s.name == "Box").expect("Box recovered");
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
    use crate::model::file_analysis::InferredType;
    // Hovering an enum member surfaces its enum, the same `name: type` way a
    // struct field renders: `RED: Color`. Wired as the enumerator's container
    // (package) + type (ClassName of the enum).
    let fa = cpp_driver().analyze("enum Color { RED, GREEN };\n");
    let red = fa
        .symbols()
        .iter()
        .find(|s| s.name == "RED")
        .unwrap_or_else(|| panic!("RED enumerator: {:?}",
            fa.symbols().iter().map(|s| &s.name).collect::<Vec<_>>()));
    assert_eq!(red.package.as_deref(), Some("Color"),
        "enum member's container is its enum");
    assert_eq!(
        fa.inferred_type_via_bag("RED", red.span.start),
        Some(InferredType::ClassName("Color".to_string())),
        "enum member's type is its enum, so hover renders `RED: Color`"
    );
    // A bare `enum` (no @scope) keeps members in the enclosing scope, so a
    // later bare read of RED still resolves to this def.
    assert_eq!(red.scope, crate::model::file_analysis::ScopeId(0),
        "enumerators leak into the enclosing (file) scope");
}


#[cfg(feature = "cpp")]
#[test]
fn function_like_macro_types_from_its_body() {
    // The expansion flip: `SQ(3)` is LEFT as a call, so the macro is a package-
    // global sub the sub-return path types. `((x)*(x))` is a numeric expression
    // whatever `x` is (param-independent), so the use types integer.
    use crate::model::file_analysis::InferredType;
    let src = "#define SQ(x) ((x) * (x))\nvoid g(void) { auto b = SQ(3); }\n";
    let fa = cpp_driver().analyze(src);
    assert!(fa.symbols().iter().any(|s| s.name == "SQ"), "macro is a sub symbol");
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
    use crate::model::file_analysis::InferredType;
    let src = "int real(int x) { return x; }\n#define WRAP(x) real(x)\nvoid g(void) { auto d = WRAP(4); }\n";
    let fa = cpp_driver().analyze(src);
    assert_eq!(
        fa.inferred_type_via_bag("d", tree_sitter::Point { row: 2, column: 20 }),
        Some(InferredType::Numeric),
        "WRAP delegates to real → real's return type flows through",
    );
    // exactly one `real` sub (the dual @def.sub patterns dedup by span).
    assert_eq!(fa.symbols().iter().filter(|s| s.name == "real").count(), 1);
}

/// An annotation-less local initialized from an UNRESOLVABLE uppercase call
/// (`auto rcpv = RCPVx(pv)` — no local class/struct/typedef, no known return)
/// must NOT type as `ClassName("RCPVx")`. The retired ctor-convention heuristic
/// minted a phantom class from name case alone; deferred resolution yields no
/// witness when the callee resolves to nothing, so the receiver stays honestly
/// untyped rather than wrongly typed. Real coordinates: perl5 op.c `RCPVx(pv)`.
#[cfg(feature = "cpp")]
#[test]
fn ctor_convention_unresolvable_uppercase_call_no_phantom_class() {
    use crate::model::file_analysis::InferredType;
    let src = "void g(char *pv) {\n  auto rcpv = RCPVx(pv);\n  rcpv->refcount++;\n}\n";
    let fa = cpp_driver().analyze(src);
    let inv = fa
        .refs()
        .iter()
        .find_map(|r| (r.target_name == "refcount").then(|| r.member_site()?.invocant_span).flatten())
        .expect("rcpv->refcount minted a member ref with an invocant span");
    let ty = fa.expr_type_at_span(inv, None);
    assert!(
        !matches!(&ty, Some(InferredType::ClassName(n)) if n == "RCPVx"),
        "unresolvable uppercase call must not mint a phantom ClassName: {ty:?}"
    );
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
    let sym = |n: &str| fa.symbols().iter().find(|s| s.name == n).unwrap();
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
#[test]
fn member_completion_filters_by_access_specifier() {
    // Member completion filters by access specifier: from
    // OUTSIDE the class only public members offer; from a method of the
    // SAME class (self-access) everything offers, including private ones.
    // Must go through `cpp_driver().analyze` (not the raw skeleton→FA path)
    // — the access-region stamp is a language_driver post-process pass.
    let fa = cpp_driver().analyze(
        "class Status {\n\
         public:\n  bool ok() const;\n  void Update(int x);\n\
         private:\n  void Ref();\n  void Unref();\n  int rep_;\n\
         };\n",
    );
    let outside_cands = fa.complete_members_for_class("Status", None, None, crate::model::file_analysis::MemberAccess::Instance);
    let outside: Vec<&str> = outside_cands.iter().map(|c| c.label.as_str()).collect();
    assert!(outside.contains(&"ok"), "{outside:?}");
    assert!(outside.contains(&"Update"), "{outside:?}");
    assert!(!outside.contains(&"Ref"), "private method leaked: {outside:?}");
    assert!(!outside.contains(&"Unref"), "private method leaked: {outside:?}");
    assert!(!outside.contains(&"rep_"), "private field leaked: {outside:?}");

    let inside = fa.complete_members_for_class("Status", None, Some("Status"), crate::model::file_analysis::MemberAccess::Instance);
    let inside_labels: Vec<&str> = inside.iter().map(|c| c.label.as_str()).collect();
    for want in ["ok", "Update", "Ref", "Unref", "rep_"] {
        assert!(inside_labels.contains(&want), "{want} missing from self-access: {inside_labels:?}");
    }
}

#[cfg(feature = "cpp")]
fn sym_in<'a>(
    fa: &'a crate::model::file_analysis::FileAnalysis,
    n: &str,
) -> &'a crate::model::file_analysis::Symbol {
    fa.symbols().iter().find(|s| s.name == n).unwrap()
}

#[cfg(feature = "cpp")]
#[test]
fn macro_body_member_carries_field_payload_like_plain_field() {
    // hitlist-4 family C (findings 4 + 6a): a member declared inside a
    // `#define BASEOP` body must arrive with the SAME payload a plainly-declared
    // struct field carries — Field kind, the pointer deref_stack, and the
    // explicit-annotation (`Annotation(Declared)`) witness — so hover keeps the `*` and
    // the redundant inlay hint is suppressed.
    use crate::model::file_analysis::{InferredType, SymKind};
    let src = "\
#define BASEOP OP* op_next; unsigned op_type:9;
struct op { BASEOP };
";
    let fa = cpp_driver().analyze(src);

    let op_next = sym_in(&fa, "op_next");
    assert_eq!(op_next.kind, SymKind::Field, "macro-body member is a Field, not a Variable");
    assert!(!op_next.deref_stack.is_empty(), "pointer member keeps its deref_stack");
    // finding 4: hover renders the pointer star through the single display path.
    assert_eq!(
        op_next.display_type(&InferredType::ClassName("OP".into())),
        "OP*",
        "hover keeps the pointer star"
    );

    // finding 6a: the explicit-annotation witness the inlay suppressor keys on
    // is present on the member's own scope (parity with a plain field).
    let op_type = sym_in(&fa, "op_type");
    assert_eq!(op_type.kind, SymKind::Field);
    assert!(
        fa.witnesses.has_annotation(
            &crate::model::witnesses::WitnessAttachment::Variable {
                name: "op_type".into(),
                scope: op_type.scope,
            },
        ),
        "macro-body member carries the declared-type witness"
    );

    // The renderers then agree: inlay over the member declarations emits no hint
    // (Field kind + `Annotation` both suppress) — exactly like a plain struct,
    // whose fields are never hinted either.
    let full = crate::lsp::symbols::inlay_hints(
        &fa,
        tower_lsp::lsp_types::Range {
            start: tower_lsp::lsp_types::Position { line: 0, character: 0 },
            end: tower_lsp::lsp_types::Position { line: 2, character: 0 },
        },
    );
    assert!(full.is_empty(), "no inlay hints echo a macro-body member's declared type: {full:?}");
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
    use crate::model::file_analysis::RefKind;
    // `Widget` in `Widget make_widget();` / `Widget global_w;` is a USE of
    // the type (rule #7) — a PackageRef, same as a Perl package-name use —
    // while the decl's own name token stays the Symbol's alone.
    let fa = cpp_driver().analyze("struct Widget { int w; };\nWidget make_widget();\nWidget global_w;\n");
    let type_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef) && r.target_name == "Widget")
        .collect();
    assert_eq!(type_refs.len(), 2, "two uses, decl-name suppressed: {type_refs:?}");
    assert!(type_refs.iter().all(|r| r.span.start.row >= 1));
}

#[cfg(feature = "cpp")]
#[test]
fn expanded_macro_uses_still_carry_refs() {
    use crate::model::file_analysis::RefKind;
    // An object-like value macro's uses are EXPANDED out of the parsed text;
    // the splice map re-mints a Variable read at each original site so
    // find-references on the `#define` still reaches them (rule #7/#9).
    let src = std::fs::read_to_string("gold-corpus/cpp-fixture/macro_refs.h").unwrap();
    let fa = cpp_driver().analyze(&src);
    let uses: Vec<_> = fa
        .refs()
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
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Variable) && r.target_name == "BASEOP")
        .count();
    assert_eq!(baseop_uses, 2, "struct op {{ BASEOP }} and struct unop {{ BASEOP ... }}");
}

// --- H3: brace-init declarations must survive `strip_declarator_macros` ---

#[cfg(feature = "cpp")]
#[test]
fn cpp_brace_init_declaration_survives_declarator_strip() {
    use crate::model::file_analysis::SymKind;
    let src = "struct Point { int x; int y; };\nint main() {\n  struct Point p {1, 2};\n  return p.x;\n}\n";
    let fa = cpp_driver().analyze(src);
    // No phantom Class minted from the declared variable.
    assert!(
        !fa.symbols().iter().any(|s| s.name == "p" && s.kind == SymKind::Class),
        "brace-init var must not become a Class: {:?}",
        fa.symbols().iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
    );
    // The type use on the declaration line keeps its ref.
    assert!(
        fa.refs().iter().any(|r| r.target_name == "Point" && r.span.start.row == 2),
        "Point use on the brace-init line refs: {:?}",
        fa.refs().iter().map(|r| (&r.target_name, r.span.start)).collect::<Vec<_>>()
    );
    // Member resolution through the declared variable still works.
    let inv = fa
        .refs()
        .iter()
        .find_map(|r| (r.target_name == "x").then(|| r.member_site()?.invocant_span).flatten())
        .expect("p.x minted a member ref with an invocant span");
    let t = fa.expr_type_at_span(inv, None).expect("receiver types");
    assert_eq!(t.class_name(), Some("Point"), "p types as Point: {t:?}");
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_empty_brace_init_not_stripped() {
    use crate::model::file_analysis::SymKind;
    let src = "void f() {\n  struct sockaddr_in addr {};\n}\n";
    let fa = cpp_driver().analyze(src);
    assert!(
        !fa.symbols().iter().any(|s| s.name == "addr" && s.kind == SymKind::Class),
        "empty brace-init var must not become a Class: {:?}",
        fa.symbols().iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
    );
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_range_for_struct_binding_not_stripped() {
    use crate::model::file_analysis::SymKind;
    let src = "struct Point { int x; };\nvoid f(int n) {\n  for (struct Point q : points) { n += q.x; }\n}\n";
    let fa = cpp_driver().analyze(src);
    assert!(
        !fa.symbols().iter().any(|s| s.name == "q" && s.kind == SymKind::Class),
        "range-for binding must not become a Class: {:?}",
        fa.symbols().iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
    );
    assert!(
        fa.refs().iter().any(|r| r.target_name == "Point" && r.span.start.row == 2),
        "Point use inside the for head refs"
    );
}

// --- H4: every span-bearing skeleton field is remapped after a splice ---

/// The doc repro: an object-like macro expansion on the SAME line before a
/// member access shifts every following column; the four fields
/// (`invocant` / `member_op` / `import_sites` / `domain_sites`) must come
/// back in ORIGINAL coordinates like refs/witnesses do.
#[cfg(feature = "cpp")]
fn h4_fixture() -> crate::model::file_analysis::FileAnalysis {
    let src = "#define LOG emit_log_record_with_a_long_name(1, 2, 3)\nvoid emit_log_record_with_a_long_name(int a, int b, int c);\nstruct Widget { int size; };\nint main() {\n  struct Widget w;\n  LOG; w.size = 5;\n  return w.size;\n}\n";
    cpp_driver().analyze(src)
}

#[cfg(feature = "cpp")]
fn h4_member_ref(
    fa: &crate::model::file_analysis::FileAnalysis,
) -> (crate::model::file_analysis::Span, Option<(crate::model::file_analysis::MemberOp, crate::model::file_analysis::Span)>) {
    fa.refs()
        .iter()
        .find_map(|r| {
            (r.target_name == "size" && r.span.start.row == 5)
                .then(|| r.member_site().and_then(|m| Some((m.invocant_span?, m.member_op.copied()))))
                .flatten()
        })
        .expect("w.size on the spliced line minted a member ref with an invocant span")
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_splice_remaps_invocant_span() {
    let fa = h4_fixture();
    let (inv, _) = h4_member_ref(&fa);
    // original line 5: `  LOG; w.size = 5;` — `w` at col 7.
    assert_eq!(
        ((inv.start.row, inv.start.column), (inv.end.row, inv.end.column)),
        ((5, 7), (5, 8)),
        "invocant span in ORIGINAL coords: {inv:?}"
    );
    // The money query: member resolution through the remapped span.
    let t = fa.expr_type_at_span(inv, None).expect("receiver types after splice");
    assert_eq!(t.class_name(), Some("Widget"), "w types as Widget: {t:?}");
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_splice_remaps_member_op_span() {
    use crate::model::file_analysis::MemberOp;
    let fa = h4_fixture();
    let (_, op) = h4_member_ref(&fa);
    let (op, sp) = op.expect("member op recorded");
    assert_eq!(op, MemberOp::Dot);
    assert_eq!(
        ((sp.start.row, sp.start.column), (sp.end.row, sp.end.column)),
        ((5, 8), (5, 9)),
        "member-op span in ORIGINAL coords: {sp:?}"
    );
}

/// Synthetic single-splice map + skeleton: pin each remaining field family
/// (`import_sites`, `domain_sites`) through `remap_spans` directly, so a
/// same-line shift is exercised even where real syntax can't put one (an
/// `#include` must be line-initial).
#[cfg(feature = "cpp")]
fn h4_synthetic() -> (String, String, crate::build::cpp_reparse::SpliceMap) {
    let src = "#define LOG emit_log_record_with_a_long_name(1, 2, 3)\nvoid f() { LOG; tail(); }\n";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    let (rewritten, map, _) = crate::build::cpp_reparse::preprocess_validated_with(
        &mut parser,
        src,
        &crate::build::cpp_reparse::PreExpandedExternal::empty(),
    );
    assert_ne!(rewritten, src, "the LOG use must actually splice");
    (src.to_string(), rewritten, map)
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_splice_remaps_import_sites() {
    use crate::model::file_analysis::Span;
    use tree_sitter::Point;
    let (src, rewritten, map) = h4_synthetic();
    // `tail` in original coords (row 1 col 16); its transformed column
    // shifted right by the splice on the same line.
    let tcol = rewritten.lines().nth(1).unwrap().find("tail").unwrap();
    assert_ne!(tcol, 16, "splice shifted the same-line column");
    let sp = Span {
        start: Point { row: 1, column: tcol },
        end: Point { row: 1, column: tcol + 4 },
    };
    let mut skel = crate::build::query_extract::SkeletonAnalysis::default();
    skel.import_sites.push(crate::model::file_analysis::ImportRow {
        span: sp,
        raw: "tail.h".to_string(),
        binds: Default::default(),
        bound: None,
    });
    remap_spans(&mut skel, &rewritten, &src, &map);
    let got = skel.import_sites[0].span;
    assert_eq!(
        ((got.start.row, got.start.column), (got.end.row, got.end.column)),
        ((1, 16), (1, 20)),
        "import-site span back in ORIGINAL coords: {got:?}"
    );
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_splice_remaps_domain_sites() {
    use crate::model::file_analysis::{DomainSite, Span};
    use tree_sitter::Point;
    let (src, rewritten, map) = h4_synthetic();
    let tcol = rewritten.lines().nth(1).unwrap().find("tail").unwrap();
    let sp = Span {
        start: Point { row: 1, column: tcol },
        end: Point { row: 1, column: tcol + 4 },
    };
    let mut skel = crate::build::query_extract::SkeletonAnalysis::default();
    skel.domain_sites.push(DomainSite {
        slot: "op_type".to_string(),
        value: "OP_NULL".to_string(),
        slot_span: sp,
    });
    remap_spans(&mut skel, &rewritten, &src, &map);
    let got = skel.domain_sites[0].slot_span;
    assert_eq!(
        ((got.start.row, got.start.column), (got.end.row, got.end.column)),
        ((1, 16), (1, 20)),
        "domain-site span back in ORIGINAL coords: {got:?}"
    );
}

// A bare name that names a field of the enclosing class is an implicit
// `this->field` read where the document says the body elides the receiver
// (`@scope.sub.implicit_receiver`). The read binds to the field AND mints
// the `Expr → Edge(Variable{field})` edge that types it; a lambda body
// nested in the method elides too, because the scope CHAIN carries the
// fact.
#[cfg(feature = "cpp")]
#[test]
fn implicit_field_read_binds_and_types_through_the_scope_chain() {
    use crate::model::file_analysis::{RefKind, SymKind};
    use crate::model::witnesses::WitnessSource;
    let src = "struct C { int inner_; int get() { return inner_; } \
int lam() { auto f = [&]{ return inner_; }; return f(); } };\n";
    let fa = cpp_driver().analyze(src);
    let edges = fa
        .witnesses
        .all()
        .iter()
        .filter(|w| matches!(&w.source, WitnessSource::Builder(s) if s == "implicit_field_read"))
        .count();
    assert_eq!(edges, 2, "both bare reads type through the field's own attachment");
    let field = fa
        .symbols()
        .iter()
        .find(|s| s.name == "inner_" && s.kind == SymKind::Field)
        .expect("the field declaration");
    let bound: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "inner_" && matches!(r.kind, RefKind::Variable))
        .map(|r| r.resolved_symbol())
        .collect();
    assert_eq!(
        bound,
        vec![Some(field.id), Some(field.id)],
        "the method body AND the lambda body inside it bind to the field"
    );
}

// The complement: php spells the receiver, so its document leaves every
// sub scope plain and a bare name inside a method binds to nothing —
// whatever the class declares.
#[cfg(feature = "php")]
#[test]
fn a_spelled_receiver_language_declares_no_implicit_scope() {
    use crate::model::file_analysis::{RefKind, SymKind};
    let src = "<?php\nclass C { private int $inner; function get() { return $inner; } }\n";
    let fa = crate::build::language_driver::LanguageRegistry::with_enabled()
        .for_id("php")
        .expect("php driver")
        .analyze(src);
    assert!(
        fa.scopes.iter().all(|sc| !sc.implicit_receiver),
        "php's document states the fact nowhere"
    );
    // Not vacuous: the property IS declared and the bare name IS read.
    assert!(
        fa.symbols().iter().any(|s| s.name == "inner" && s.kind == SymKind::Field),
        "the property is a Field symbol"
    );
    let read = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "$inner" && matches!(r.kind, RefKind::Variable))
        .expect("the bare read is a Variable ref");
    assert_eq!(read.resolved_symbol(), None, "php: a bare name is never the property");
}

// A C callback member (`int (*read)(char *)`) is a stored slot the source
// CALLS. The declarator says so — the peel's `@deref.callable` level mints
// `SymbolFlags::CALLABLE_VALUE` — so `ops->read(buf)` resolves to the slot
// while a call on a plain `int count` still resolves to nothing. The rule is
// on the declaration, never on a list of callback names.
#[cfg(feature = "cpp")]
#[test]
fn a_callback_member_answers_a_call_and_a_plain_one_does_not() {
    use crate::model::file_analysis::{SymKind, SymbolFlags};
    let src = "\
struct Ops {\n\
  int (*read)(char *buf);\n\
  int count;\n\
};\n\
int a(struct Ops *o) { return o->read(\"x\"); }\n\
int b(struct Ops *o) { return o->count(1); }\n";
    let fa = cpp_driver().analyze(src);
    let field = |name: &str| {
        fa.symbols()
            .iter()
            .find(|s| s.name == name && s.kind == SymKind::Field)
            .unwrap_or_else(|| panic!("{name} is a Field"))
    };
    assert!(
        field("read").flags.contains(SymbolFlags::CALLABLE_VALUE),
        "the function-pointer declarator states the slot is invoked"
    );
    assert!(
        !field("count").flags.contains(SymbolFlags::CALLABLE_VALUE),
        "a plain int member states nothing of the kind"
    );
    let at = |row: usize, needle: &str| {
        let line = src.lines().nth(row).unwrap();
        tree_sitter::Point { row, column: line.find(needle).unwrap() }
    };
    assert_eq!(
        fa.find_definition(at(4, "read("), None),
        Some(field("read").selection_span),
        "the call lands on the callback member"
    );
    assert_eq!(
        fa.find_definition(at(5, "count("), None),
        None,
        "a call on a non-callable slot resolves to nothing"
    );
}

// Implicit-`this` sibling method CALLs — the call half of the same fact. A
// bare `foo(...)` inside a body that elides the receiver pins its enclosing
// class onto the `FunctionCall`'s `resolved_package` (in-class AND
// out-of-line/template bodies — the class comes off the scope's OWNER
// symbol, not the body scope which is package-less out of line), so
// goto-def lands on the sibling. A free-function-only name stays unpinned.
#[cfg(feature = "cpp")]
#[test]
fn sibling_method_call_pins_enclosing_class() {
    use crate::model::file_analysis::{RefKind, SymKind};
    let src = "\
struct Widget {\n\
    void paint();\n\
    void render() { paint(); }\n\
};\n\
template <class T> struct Buf { void grow(int n); void reserve(int n); };\n\
template <class T> void Buf<T>::reserve(int n) { grow(n); }\n\
int helper();\n\
struct Gadget { void run() { helper(); } };\n";
    let pin_of = |fa: &FileAnalysis, name: &str| -> Option<Option<String>> {
        fa.refs()
            .iter()
            .find(|r| r.target_name == name && matches!(r.kind, RefKind::FunctionCall))
            .map(|r| r.resolved_package().map(str::to_string))
    };

    let fa = cpp_driver().analyze(src);
    assert_eq!(pin_of(&fa, "paint"), Some(Some("Widget".into())), "in-class sibling call pins its class");
    assert_eq!(pin_of(&fa, "grow"), Some(Some("Buf".into())), "out-of-line template sibling call pins the peeled class");
    assert_eq!(pin_of(&fa, "helper"), Some(None), "free-function-only call stays unpinned");

    // The pin makes goto-def (via the model's `find_definition`) land on the
    // sibling method decl, in-class and out-of-line alike.
    for (call, kind_pkg) in [("paint", "Widget"), ("grow", "Buf")] {
        let cref = fa
            .refs()
            .iter()
            .find(|r| r.target_name == call && matches!(r.kind, RefKind::FunctionCall { .. }))
            .unwrap();
        let decl = fa
            .symbols()
            .iter()
            .find(|s| s.name == call && matches!(s.kind, SymKind::Method) && s.package.as_deref() == Some(kind_pkg))
            .unwrap();
        assert_eq!(
            fa.find_definition(cref.span.start, None),
            Some(decl.selection_span),
            "{call}: sibling call resolves to the class method"
        );
    }
}

// H7-13: a CLASS FIELD used as a member-access receiver must type to its
// declared class, exactly like a function PARAMETER receiver does — the
// asymmetry that made `iter_->` dump the in-scope grab-bag while `iter->`
// (a param) narrowed. A C++ data member is visible class-wide regardless of
// declaration order, so a field declared in a `private:` section BELOW the
// method that reads it must still resolve (the witness-bag temporal filter,
// correct for sequential locals, must not reject a class-wide member).
#[cfg(feature = "cpp")]
#[test]
fn h13_field_receiver_types_like_param_receiver() {
    use crate::model::file_analysis::{InferredType, RefKind};
    let src = "\
class Iterator { public: int value(); };\n\
class DBIter : public Iterator {\n\
 public:\n\
  DBIter(Iterator* iter) : iter_(iter) {}\n\
  int value() const override { return iter_->value(); }\n\
  int b(Iterator* p) const { return p->value(); }\n\
 private:\n\
  Iterator* const iter_;\n\
};\n";
    let fa = cpp_driver().analyze(src);
    let recv_ty = |name: &str, row: usize| -> Option<InferredType> {
        let r = fa.refs().iter().find(|r| {
            matches!(r.kind, RefKind::Variable) && r.target_name == name && r.span.start.row == row
        })?;
        fa.expr_type_at_span(r.span, None)
    };
    // The field receiver `iter_` (declared line 8, read line 5 — decl BELOW
    // the read) types to its class, matching the param receiver `p`.
    assert_eq!(
        recv_ty("iter_", 4),
        Some(InferredType::ClassName("Iterator".into())),
        "field receiver types to its declared class regardless of decl order",
    );
    assert_eq!(
        recv_ty("p", 5),
        Some(InferredType::ClassName("Iterator".into())),
        "param receiver control: unchanged",
    );
}

// The completion-slot end-to-end: `iter_->|` detects a Member slot whose
// receiver resolves to the field's class, so the narrowed member list (not
// the in-scope grab-bag) is served. Drives `detect_slot` — the same entry
// backend completion uses.
#[cfg(feature = "cpp")]
#[test]
fn h13_field_receiver_member_slot_resolves() {
    use crate::lsp::cursor_slot::{detect_slot, Slot};
    // Cursor point right after `marker` in `src` (byte offset → Point).
    let point_after = |src: &str, marker: &str| -> tree_sitter::Point {
        let byte = src.find(marker).unwrap() + marker.len();
        let mut row = 0;
        let mut col = 0;
        for (i, ch) in src.char_indices() {
            if i == byte {
                break;
            }
            if ch == '\n' {
                row += 1;
                col = 0;
            } else {
                col += ch.len_utf8();
            }
        }
        tree_sitter::Point::new(row, col)
    };
    let member_class = |src: &str, marker: &str| -> Option<String> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let fa = cpp_driver().analyze(src);
        let detected = detect_slot(&fa, &tree, src, point_after(src, marker), "cpp", None);
        match detected.slot {
            Slot::Member { receiver, .. } => receiver
                .receiver_type
                .as_ref()
                .and_then(|t| t.class_name())
                .map(str::to_string),
            other => panic!("expected Member slot for `{marker}`, got {other:?}"),
        }
    };
    // Pointer field receiver `iter_->|value()`.
    let src = "\
class Iterator { public: int value(); };\n\
class DBIter : public Iterator {\n\
  int value() const override { return iter_->value(); }\n\
  Iterator* const iter_;\n\
};\n";
    assert_eq!(
        member_class(src, "iter_->").as_deref(),
        Some("Iterator"),
        "pointer-field receiver's Member slot resolves to the field's class",
    );
    // Value field receiver `options_.|n` narrows the same way (dot access).
    let src2 = "\
struct Options { int n; };\n\
struct RE {\n\
  int f() const { return options_.n; }\n\
  Options options_;\n\
};\n";
    assert_eq!(
        member_class(src2, "options_.").as_deref(),
        Some("Options"),
        "value-field receiver resolves to its class",
    );
}

// Member completion: a data member keeps its trailing underscore
// (`cleanup_head_`, not `cleanup_head`), and a bodiless method DECLARATION's
// parameters (`arg1`/`arg2` of `RegisterCleanup`) — which land on the class
// body scope with the sticky class package — do NOT leak in as data members.
// Visibility: the access-specifier gate stands (non-public members offer only
// from inside their own class); a private member the access-region stamp
// missed harmlessly over-offers, matching clangd, which surfaces privates in
// many completion contexts.
#[cfg(feature = "cpp")]
#[test]
fn h13_member_completion_no_param_leak_keeps_underscore() {
    let src = "\
class Iterator {\n\
 public:\n\
  int value() const;\n\
  void RegisterCleanup(void* arg1, void* arg2);\n\
 private:\n\
  struct CleanupNode { void* arg1; void* arg2; };\n\
  CleanupNode cleanup_head_;\n\
};\n";
    let fa = cpp_driver().analyze(src);
    // `requesting`: None = completing from OUTSIDE the class (public only);
    // Some("Iterator") = from a method of the SAME class (privates too).
    let has = |n: &str, requesting: Option<&str>| {
        fa.complete_members_for_class("Iterator", None, requesting, crate::model::file_analysis::MemberAccess::Instance)
            .iter()
            .any(|c| c.label == n)
    };
    assert!(has("value", None), "public method offered from outside");
    // The private field keeps its trailing underscore; it offers from inside
    // the class (the access-specifier gate — non-public members are self-only,
    // matching clangd's context sensitivity), and never as a truncated label.
    assert!(
        has("cleanup_head_", Some("Iterator")),
        "private data member keeps its trailing underscore (self-access)",
    );
    assert!(!has("cleanup_head", Some("Iterator")), "no truncated label");
    assert!(
        !has("cleanup_head_", None),
        "private member is not offered from outside the class",
    );
    // A bodiless method DECLARATION's parameters land on the class body scope
    // with the sticky class package; they must NOT leak as data members from
    // any vantage.
    for requesting in [None, Some("Iterator")] {
        assert!(
            !has("arg1", requesting) && !has("arg2", requesting),
            "declaration parameters do not leak as data members ({requesting:?})",
        );
    }
}

// The cross-file implicit-`this` member: an out-of-line method body
// (`void C::m() { field_->x(); }`) reads a field DECLARED in another file. No
// local witness exists, and a member reassignment (`field_ = f(...*2/3)`)
// leaves a phantom-local flow witness that would mis-type the receiver — so
// receiver typing resolves the member on the enclosing class, ahead of the
// bag. A genuine local/param is untouched (it has a Variable symbol).
#[cfg(feature = "cpp")]
#[test]
fn h13_implicit_receiver_class_and_local_discriminator() {
    let src = "\
struct Prog { int Size() const; };\n\
struct RE {\n\
  void Init();\n\
  Prog* prog_;\n\
};\n\
void RE::Init() {\n\
  prog_ = new Prog();\n\
  int local = prog_->Size() * 2 / 3;\n\
  (void)local;\n\
}\n";
    let fa = cpp_driver().analyze(src);
    // The out-of-line body's enclosing class is read off the peeled method sym.
    let at_prog = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "prog_" && r.span.start.row == 7)
        .map(|r| r.span.start)
        .expect("prog_ receiver ref on the `prog_->Size()` line");
    assert_eq!(
        fa.implicit_receiver_class_at(at_prog).as_deref(),
        Some("RE"),
        "out-of-line method body's implicit-this class is its peeled class",
    );
    // `prog_` (a member write, no declarator) has no local Variable symbol;
    // `local` (a real declaration) does.
    assert!(
        !fa.has_local_variable_at("prog_", at_prog),
        "a member-assigned name has no local Variable declaration",
    );
    assert!(
        fa.has_local_variable_at("local", at_prog),
        "a genuinely declared local is recognised as local",
    );
}

// The DriverCaps exhaustiveness witness: destructure every field with no
// `..`, so ADDING a capability is a compile error here until every
// driver's declared answer has been reviewed (the same enforcement shape
// as `FileAnalysis::surface_feed`). A silently-defaulted axis is how a
// caps struct decays back into the language branching it replaced.
#[test]
fn driver_caps_axes_are_reviewed_exhaustively() {
    let reg = LanguageRegistry::with_enabled();
    for d in reg.languages().into_iter().filter_map(|id| reg.for_id(id)) {
        let DriverCaps {
            hub_enrichment,
            cursor_context,
            hover_info,
            signature_help,
            pack_signature_help,
            selection_range,
            synchronous_rebuild,
            context_gather,
            pack_invalidation,
            cross_file_words,
        } = d.caps();
        // The hub lanes (enrichment, native cursor/hover/rebuild verbs) and
        // the pack lanes (invalidator, gather, bare words) are disjoint
        // architectures today — one driver never straddles both.
        // selectionRange is a tree-ancestor walk with no language in it —
        // both architectures serve it, so it belongs to neither family.
        let _ = selection_range;
        let hub_family = hub_enrichment
            || cursor_context
            || hover_info
            || signature_help
            || synchronous_rebuild;
        let pack_family = pack_invalidation
            || pack_signature_help
            || context_gather
            || cross_file_words;
        assert!(
            !(hub_family && pack_family),
            "driver {} declares capabilities from both serving architectures",
            d.id()
        );
    }
}

// Exactly one driver serves unclaimed files — the fallback is a declared
// property, never a registry position.
#[test]
fn exactly_one_fallback_driver() {
    let reg = LanguageRegistry::with_enabled();
    let n = reg
        .languages()
        .into_iter()
        .filter_map(|id| reg.for_id(id))
        .filter(|d| d.claims_unclaimed())
        .count();
    assert_eq!(n, 1, "exactly one driver claims unclaimed files");
    assert!(reg.fallback().claims_unclaimed());
}

/// A key-less array literal returned from a php function is a positional
/// TUPLE (docs/adr/destructuring.md): its `Tuple` witness of element edges
/// materializes to `Sequence([Queue, Agent])` through the return-fuel
/// chain — both for an undeclared return and for a bare `: array`, which
/// the tuple REFINES (the chain rides at annot priority so it beats the
/// `HashRef` annot). A keyed literal never becomes a tuple.
#[cfg(feature = "php")]
#[test]
fn php_tuple_literal_returns_type_as_sequence() {
    use crate::model::file_analysis::InferredType;
    let src = "<?php\nnamespace App;\nclass Queue {}\nclass Agent {}\nclass T {\n    private function lit(): array { $q = new Queue(); $a = new Agent(); return [$q, $a]; }\n    private function undecl() { $q = new Queue(); $a = new Agent(); return [$q, $a]; }\n    private function keyed(): array { $q = new Queue(); return ['q' => $q]; }\n}\n";
    let reg = LanguageRegistry::with_enabled();
    let fa = reg.for_path(std::path::Path::new("T.php")).unwrap().analyze(src);
    let tuple = Some(InferredType::Sequence(vec![
        InferredType::ClassName("App\\Queue".into()),
        InferredType::ClassName("App\\Agent".into()),
    ]));
    assert_eq!(fa.sub_return_type_at_arity("lit", Some(0)), tuple, "bare `: array` refined");
    assert_eq!(fa.sub_return_type_at_arity("undecl", Some(0)), tuple, "undeclared return");
    assert!(
        !matches!(fa.sub_return_type_at_arity("keyed", Some(0)), Some(InferredType::Sequence(_))),
        "a keyed literal is not a tuple"
    );
}

/// Every return site of a function contributes an arm — the gate that
/// decides "declared vs. body-typed" is snapshotted per function, never
/// re-read from the witnesses the loop itself just pushed (which typed a
/// two-return function by its FIRST return only). Agreeing arms type the
/// function; disagreeing arms honestly refuse.
#[cfg(feature = "php")]
#[test]
fn php_every_return_site_contributes_an_arm() {
    use crate::model::file_analysis::InferredType;
    let src = "<?php\nnamespace App;\nclass Queue {}\nclass Agent {}\nfunction agree($c) { if ($c) { return new Queue(); } return new Queue(); }\nfunction disagree($c) { if ($c) { return new Queue(); } return new Agent(); }\n";
    let reg = LanguageRegistry::with_enabled();
    let fa = reg.for_path(std::path::Path::new("T.php")).unwrap().analyze(src);
    assert_eq!(
        fa.sub_return_type_at_arity("agree", Some(1)),
        Some(InferredType::ClassName("App\\Queue".into()))
    );
    // Both arms land (the fold's own agreement policy decides the answer;
    // the pin is that the SECOND site is no longer dropped).
    let sid = fa.symbols().iter().find(|s| s.name == "disagree").unwrap().id;
    let arms = fa
        .witnesses
        .for_attachment(&crate::model::witnesses::WitnessAttachment::SymbolReturnArm(sid))
        .len();
    assert_eq!(arms, 2, "every return site contributes an arm");
}

/// A named `/** @var Sub $p */` above a RE-assignment casts the local from
/// that site on — the factory-narrowing idiom. The cast rides at annot
/// priority: the call-binding edge the same assignment mints lands later
/// in the bag and would otherwise override it with the declared base.
#[cfg(feature = "php")]
#[test]
fn php_named_var_doc_casts_a_rebound_local() {
    use crate::model::file_analysis::InferredType;
    let src = "<?php\nnamespace App;\nclass Base { public static function make(string $n): Base { return new Base(); } }\nclass Sub extends Base {}\nfunction go(): void {\n    $p = Base::make('x');\n    /** @var Sub $p */\n    $p = Base::make('y');\n    $p->x();\n}\n";
    let reg = LanguageRegistry::with_enabled();
    let fa = reg.for_path(std::path::Path::new("T.php")).unwrap().analyze(src);
    assert_eq!(
        fa.inferred_type_via_bag("$p", tree_sitter::Point { row: 8, column: 4 }),
        Some(InferredType::ClassName("App\\Sub".into())),
        "cast applies from the rebind on"
    );
}


#[cfg(feature = "cpp")]
#[test]
fn cpp_callable_carries_its_parameters_as_facts() {
    use crate::model::file_analysis::SymbolDetail;
    // The parameter list is walked once, by the arity walk; names, defaults
    // and binding sites come off that walk (rule #11) rather than a second
    // scan of the source by whoever needs to render a signature.
    let src = "template <class... A>\nvoid dispatch(int first, int limit = 10, A... rest) {}\n";
    let fa = cpp_driver().analyze(src);
    let sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "dispatch")
        .unwrap_or_else(|| panic!("dispatch: {:?}", fa.symbols().iter().map(|s| &s.name).collect::<Vec<_>>()));
    let SymbolDetail::Sub { params, .. } = &sym.detail else {
        panic!("callable carries a Sub detail, got {:?}", sym.detail)
    };
    assert_eq!(
        params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        vec!["first", "limit", "rest"],
        "every parameter, in source order"
    );
    assert_eq!(params[0].default, None);
    assert_eq!(params[1].default.as_deref(), Some("10"), "the default is source text");
    assert_eq!(params[0].declared_type.as_deref(), Some("int"), "the declared type is source text");
    assert_eq!(params[2].declared_type.as_deref(), Some("A"), "a pack expansion keeps its element type");
    assert!(!params[1].is_slurpy);
    assert!(params[2].is_slurpy, "a pack expansion is slurpy");
    // Each binding site is the parameter's own name token.
    for p in params {
        let site = p.binding_site.expect("a written parameter binds at its name token");
        let line = src.lines().nth(site.row).unwrap();
        assert!(
            line[site.column..].starts_with(&p.name),
            "binding site of {} points at its name token, got {:?}",
            p.name,
            &line[site.column..]
        );
    }
    // The counts stay on `arity`, which `param_arity()` still prefers.
    let arity = sym.param_arity().expect("a callable has an arity");
    assert_eq!((arity.total, arity.required, arity.variadic), (2, 1, true));
}

#[cfg(feature = "cpp")]
#[test]
fn cpp_include_row_binds_a_type() {
    use crate::model::file_analysis::ImportBinds;
    // C has one kind of `#include`, so its rows carry the default binding —
    // the value a consumer reads instead of guessing from the leaf's case.
    let fa = cpp_driver().analyze("#include \"box.h\"\n#include <vector>\n");
    let raws: Vec<&str> = fa.pack.include_directives.iter().map(|r| r.raw.as_str()).collect();
    assert_eq!(raws, vec!["box.h", "<vector>"], "both rows: {raws:?}");
    for r in &fa.pack.include_directives {
        assert_eq!(r.binds, ImportBinds::Type, "unsuffixed capture binds a type");
    }
}

/// An OVERLAY declaring two ordinary function names as dynamic-surface
/// markers. No bundled pack declares any (cpp has no such surface), so the
/// marker → flag path needs a declarer to have a subject at all — and a
/// document is how a third party declares one.
#[cfg(feature = "cpp")]
const MARKER_OVERLAY: &str = "((call_expression function: (identifier) @call.dynamic_args)
 (#eq? @call.dynamic_args \"read_args\"))
((call_expression function: (identifier) @call.dynamic_vars)
 (#eq? @call.dynamic_vars \"make_vars\"))
";

#[cfg(feature = "cpp")]
fn marker_pack() -> crate::build::query_extract::LangPack {
    crate::build::query_extract::LangPack {
        bundled_overlays: &[("dynamic-markers.scm", MARKER_OVERLAY)],
        ..crate::build::query_extract::cpp_pack()
    }
}

#[cfg(feature = "cpp")]
#[test]
fn dynamic_markers_land_on_the_enclosing_callable() {
    use crate::model::file_analysis::SymbolFlags;
    let driver = PackDriver { pack: marker_pack, ..cpp_driver() };
    let src = "int read_args();\nint make_vars();\nint g = read_args();\n\
               void wide() { read_args(); }\nvoid narrow() { if (1) { make_vars(); } }\n\
               void plain() { }\n";
    let fa = driver.analyze(src);
    let flags = |name: &str| {
        fa.symbols()
            .iter()
            .find(|s| s.name == name && s.span.start.row >= 3)
            .unwrap_or_else(|| panic!("{name}: {:?}", fa.symbols().iter().map(|s| &s.name).collect::<Vec<_>>()))
            .flags
    };
    assert!(flags("wide").contains(SymbolFlags::DYNAMIC_ARGS));
    assert!(!flags("wide").contains(SymbolFlags::DYNAMIC_VARS));
    // Through a nested block: the scope chain, not the immediate scope.
    assert!(flags("narrow").contains(SymbolFlags::DYNAMIC_VARS));
    assert!(!flags("narrow").contains(SymbolFlags::DYNAMIC_ARGS));
    assert!(!flags("plain").intersects(SymbolFlags::DYNAMIC_ARGS | SymbolFlags::DYNAMIC_VARS));
    // The file-scope initializer's call owns no callable and stamps nothing.
    let stamped: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| s.flags.intersects(SymbolFlags::DYNAMIC_ARGS | SymbolFlags::DYNAMIC_VARS))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(stamped, vec!["wide", "narrow"], "only the two containing callables");
}

/// The undefined-variable lane runs on FACTS, not on a per-language switch:
/// a read the runtime binds carries `RefBinding::Runtime` (php's
/// `@ref.var.implicit`), and a pack whose document binds nothing must
/// produce no unbound reads at all. Deleting the lane's on-switch is only
/// safe if that holds, so it is pinned here for the packs that declare no
/// implicit variables.
#[test]
fn packs_that_bind_nothing_implicitly_report_no_undefined_variables() {
    let mut checked: Vec<&str> = Vec::new();
    #[cfg(feature = "cpp")]
    {
        let fa = cpp_driver().analyze(
            "int g = 1;\nint f(int a) { int b = a + g; for (int i = 0; i < b; i++) { b += i; } return b; }\n",
        );
        assert_undefined_variable_silence(&fa, "cpp");
        checked.push("cpp");
    }
    #[cfg(feature = "python")]
    {
        let fa = python_driver().analyze(
            "g = 1\nclass C:\n    def m(self, a):\n        b = a + g\n        return b + self.x\n",
        );
        assert_undefined_variable_silence(&fa, "python");
        checked.push("python");
    }
    #[cfg(feature = "r")]
    {
        let fa = r_driver().analyze("g <- 1\nf <- function(a) {\n  b <- a + g\n  b\n}\n");
        assert_undefined_variable_silence(&fa, "r");
        checked.push("r");
    }
    assert!(!checked.is_empty(), "no pack language in this build");
}

#[cfg(any(feature = "cpp", feature = "python", feature = "r"))]
fn assert_undefined_variable_silence(
    fa: &crate::model::file_analysis::FileAnalysis,
    lang: &str,
) {
    let diags = crate::lsp::symbols::pack_symbol_diagnostics(fa, None);
    let hits: Vec<&tower_lsp::lsp_types::Diagnostic> = diags
        .iter()
        .filter(|d| {
            matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c)) if c == "undefined-variable")
        })
        .collect();
    assert!(hits.is_empty(), "{lang} must report no undefined variables: {hits:?}");
}

/// The name an import row binds is what the DOCUMENT captured
/// (`@import.binds`), not a leaf the lane re-derives: an alias binds the
/// alias, a group clause binds its own leaf or alias, and every php row
/// carries one.
#[cfg(feature = "php")]
#[test]
fn php_import_rows_carry_the_name_they_bind() {
    let fa = php_driver().analyze(
        "<?php\nnamespace App;\nuse A\\B\\C;\nuse A\\B\\D as E;\nuse F;\nuse G as H;\nuse function A\\slug;\nuse const A\\MAX;\nuse A\\{P, Q as R};\n",
    );
    let mut rows: Vec<(&str, Option<&str>)> = fa
        .pack
        .include_directives
        .iter()
        .map(|r| (r.raw.as_str(), r.bound.as_deref()))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("A\\B\\C", Some("C")),
            ("A\\B\\D", Some("E")),
            ("A\\MAX", Some("MAX")),
            ("A\\P", Some("P")),
            ("A\\Q", Some("R")),
            ("A\\slug", Some("slug")),
            ("F", Some("F")),
            ("G", Some("H")),
        ],
        "every php row binds exactly one name, and `as` wins"
    );
}

/// `from x import y` binds `y` — so the never-spelled lane reports `y` and
/// never the module it came from, and stays silent once `y` is spelled.
/// `import x` binds the head package, which no token of the row spells: the
/// row states no binding and the lane says nothing about it.
#[cfg(feature = "python")]
#[test]
fn python_from_import_binds_the_name_not_the_module() {
    let unused = |src: &str| -> Vec<String> {
        let fa = python_driver().analyze(src);
        crate::lsp::symbols::pack_symbol_diagnostics(&fa, None)
            .iter()
            .filter(|d| {
                matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c)) if c == "unused-import")
            })
            .map(|d| d.message.clone())
            .collect()
    };
    assert!(unused("from a.b import c\n\nc()\n").is_empty(), "a spelled name is used");
    let hits = unused("import os\nfrom a.b import c\n\nprint(1)\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("'c'"), "reports the bound name: {hits:?}");
    assert!(!hits[0].contains("a.b"), "never the module: {hits:?}");
    let aliased = unused("import os.path as p\n\nprint(1)\n");
    assert_eq!(aliased.len(), 1, "{aliased:?}");
    assert!(aliased[0].contains("'p'"), "an alias binds the alias: {aliased:?}");
}
