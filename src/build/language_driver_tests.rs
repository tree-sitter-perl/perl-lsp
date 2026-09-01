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
    use crate::model::file_analysis::{InferredType, RefKind};
    let src = "void g(char *pv) {\n  auto rcpv = RCPVx(pv);\n  rcpv->refcount++;\n}\n";
    let fa = cpp_driver().analyze(src);
    let inv = fa
        .refs()
        .iter()
        .find_map(|r| match &r.kind {
            RefKind::MethodCall { invocant_span: Some(sp), .. } if r.target_name == "refcount" => {
                Some(*sp)
            }
            _ => None,
        })
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
    let outside_cands = fa.complete_members_for_class("Status", None, None);
    let outside: Vec<&str> = outside_cands.iter().map(|c| c.label.as_str()).collect();
    assert!(outside.contains(&"ok"), "{outside:?}");
    assert!(outside.contains(&"Update"), "{outside:?}");
    assert!(!outside.contains(&"Ref"), "private method leaked: {outside:?}");
    assert!(!outside.contains(&"Unref"), "private method leaked: {outside:?}");
    assert!(!outside.contains(&"rep_"), "private field leaked: {outside:?}");

    let inside = fa.complete_members_for_class("Status", None, Some("Status"));
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
    // explicit-annotation (`ANNOT_SOURCE`) witness — so hover keeps the `*` and
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
        fa.witnesses.has_builder_source(
            &crate::model::witnesses::WitnessAttachment::Variable {
                name: "op_type".into(),
                scope: op_type.scope,
            },
            crate::model::witnesses::ANNOT_SOURCE,
        ),
        "macro-body member carries the ANNOT_SOURCE witness"
    );

    // The renderers then agree: inlay over the member declarations emits no hint
    // (Field kind + ANNOT_SOURCE both suppress) — exactly like a plain struct,
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
    use crate::model::file_analysis::{RefKind, SymKind};
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
        .find_map(|r| match &r.kind {
            RefKind::MethodCall { invocant_span: Some(sp), .. } if r.target_name == "x" => Some(*sp),
            _ => None,
        })
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
    use crate::model::file_analysis::RefKind;
    fa.refs()
        .iter()
        .find_map(|r| match &r.kind {
            RefKind::MethodCall { invocant_span: Some(sp), member_op, .. }
                if r.target_name == "size" && r.span.start.row == 5 =>
            {
                Some((*sp, *member_op))
            }
            _ => None,
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
    skel.import_sites.push(("tail.h".to_string(), sp));
    remap_spans(&mut skel, &rewritten, &src, &map);
    let got = skel.import_sites[0].1;
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

// The implicit-`this->field` read pass is a C/C++ semantic (a bare name can
// mean `this->field`); the pack declares whether it applies. Only cpp mints
// the `SymKind::Field` + unresolved-bare-ref shape the pass keys on, so we
// drive it through cpp extraction and run `emit_return_fuel` with the flag
// both ways on fresh copies — the flag is the ONLY difference.
#[cfg(feature = "cpp")]
#[test]
fn implicit_field_read_pass_gated_by_pack_capability() {
    use crate::model::witnesses::WitnessSource;
    let src = "struct C { int inner_; int get() { return inner_; } };\n";
    let build = || {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let pack = crate::build::query_extract::cpp_pack();
        let mut skel = crate::build::query_extract::extract(&tree, src.as_bytes(), &pack).unwrap();
        let sites = std::mem::take(&mut skel.return_sites);
        (skel.into_file_analysis(), sites)
    };
    let count = |fa: &FileAnalysis| {
        fa.witnesses
            .all()
            .iter()
            .filter(|w| matches!(&w.source, WitnessSource::Builder(s) if s == "cpp_implicit_field_read"))
            .count()
    };

    let (mut fa_on, sites) = build();
    emit_return_fuel(&mut fa_on, &sites, true);
    assert_eq!(count(&fa_on), 1, "capability on → bare-member read minted");

    let (mut fa_off, sites) = build();
    emit_return_fuel(&mut fa_off, &sites, false);
    assert_eq!(count(&fa_off), 0, "capability off → pass gated, nothing minted");

    assert!(!crate::build::query_extract::python_pack().implicit_this_members,
        "python: a bare name is never self.field");
    assert!(crate::build::query_extract::cpp_pack().implicit_this_members,
        "cpp: methods read members with implicit this->");

    // Include-token capability: only C/C++ has `#include`-style path tokens;
    // name-keyed-import languages answer false, so goto-def / references gate
    // on the pack, never a language name.
    assert!(crate::build::query_extract::cpp_pack().include_path_tokens,
        "cpp: #include path tokens resolve to headers");
    assert!(!crate::build::query_extract::python_pack().include_path_tokens,
        "python: imports are name-keyed, no path tokens");

    // Preprocessor capability: only C/C++ has `#define` macros; other packs
    // answer false, so macro completion gates on the pack, never a language
    // name.
    assert!(crate::build::query_extract::cpp_pack().preprocessor_macros,
        "cpp: #define macros are a completion surface");
    assert!(!crate::build::query_extract::python_pack().preprocessor_macros,
        "python: no C preprocessor");
}

// The by-id capability askers on the registry are THE include-token /
// preprocessor gates for both serving surfaces (LSP handlers and their
// CLI/--batch mirrors) — pin their answers so the shared gate can't
// silently regress to a language-name probe on either side.
#[cfg(feature = "cpp")]
#[test]
fn capability_askers_answer_by_language_id() {
    use crate::build::language_driver::LanguageRegistry;
    assert!(LanguageRegistry::has_include_tokens("cpp"),
        "cpp declares include path tokens — CLI + server both gate on this");
    assert!(LanguageRegistry::has_preprocessor_macros("cpp"));
    assert!(!LanguageRegistry::has_include_tokens("perl"),
        "perl has no LangPack: the asker answers false, no name branch");
    assert!(!LanguageRegistry::has_preprocessor_macros("perl"));
    assert!(!LanguageRegistry::has_include_tokens("no-such-language"));
    #[cfg(feature = "python")]
    {
        assert!(!LanguageRegistry::has_include_tokens("python"),
            "python imports are name-keyed, no path tokens");
        assert!(!LanguageRegistry::has_preprocessor_macros("python"));
    }
}

// Implicit-`this` sibling method CALLs — the call half of the same
// capability. A bare `foo(...)` inside a method body pins its enclosing
// class onto the `FunctionCall`'s `resolved_package` (in-class AND
// out-of-line/template bodies — the class comes off the peeled method
// symbol, not the body scope which is package-less out of line), so
// goto-def lands on the sibling. A free-function-only name stays unpinned;
// the capability gate governs the whole pass.
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
    let build = || {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let pack = crate::build::query_extract::cpp_pack();
        let mut skel = crate::build::query_extract::extract(&tree, src.as_bytes(), &pack).unwrap();
        let sites = std::mem::take(&mut skel.return_sites);
        (skel.into_file_analysis(), sites)
    };
    let pin_of = |fa: &FileAnalysis, name: &str| -> Option<Option<String>> {
        fa.refs()
            .iter()
            .find(|r| r.target_name == name && matches!(r.kind, RefKind::FunctionCall))
            .map(|r| r.resolved_package().map(str::to_string))
    };

    let (mut fa, sites) = build();
    emit_return_fuel(&mut fa, &sites, true);
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

    let (mut fa_off, sites2) = build();
    emit_return_fuel(&mut fa_off, &sites2, false);
    assert_eq!(pin_of(&fa_off, "paint"), Some(None), "capability off → no sibling-call pin");
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
        fa.complete_members_for_class("Iterator", None, requesting)
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
            selection_range,
            synchronous_rebuild,
            context_gather,
            pack_invalidation,
            cross_file_words,
            entrypoint_symbols,
            runtime_invoked_methods,
            include_path_tokens,
            preprocessor_macros,
        } = d.caps();
        // The hub lanes (enrichment, native cursor/hover/rebuild verbs) and
        // the pack lanes (invalidator, gather, bare words) are disjoint
        // architectures today — one driver never straddles both.
        let hub_family = hub_enrichment
            || cursor_context
            || hover_info
            || signature_help
            || selection_range
            || synchronous_rebuild;
        let pack_family = pack_invalidation
            || context_gather
            || cross_file_words
            || include_path_tokens
            || preprocessor_macros
            || !entrypoint_symbols.is_empty()
            || !runtime_invoked_methods.is_empty();
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
