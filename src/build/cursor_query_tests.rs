//! The cursor-time query runner: what it reads off the document.

use super::*;

/// Extract one small file so the language's query is compiled and
/// remembered, then hand back the object a cursor verb would get.
#[cfg(any(feature = "cpp"))]
fn warm_query(
    language: tree_sitter::Language,
    pack: &LangPack,
    src: &str,
) -> &'static tree_sitter::Query {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(src, None).unwrap();
    crate::build::query_extract::extract(&tree, src.as_bytes(), pack).unwrap();
    crate::build::query_extract::pack_query(pack).expect("the extractor remembered its query")
}

#[test]
#[cfg(feature = "cpp")]
fn fires_at_asks_the_patterns_not_the_kind() {
    let pack = crate::build::query_extract::cpp_pack();
    let src = "void f(int a, int b) { a + b; a == b; }\n";
    let query = warm_query(tree_sitter_cpp::LANGUAGE.into(), &pack, src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let at = |needle: &str| {
        let start = src.find(needle).unwrap();
        tree.root_node().descendant_for_byte_range(start, start + needle.len()).unwrap()
    };
    // Both are `binary_expression`; only the one whose operator the
    // document's `#any-of?` names is a domain comparison.
    let (plus, eq) = (at("a + b"), at("a == b"));
    assert_eq!(plus.kind(), eq.kind());
    assert!(!fires_at(query, plus, src.as_bytes(), "domain.compare.op"), "`+` is not a comparison");
    assert!(fires_at(query, eq, src.as_bytes(), "domain.compare.op"), "`==` is");
    // The capture sits on the operator token, a CHILD: it fires at the
    // comparison without the comparison itself being captured.
    assert!(!is_captured_as(query, eq, src.as_bytes(), "domain.compare.op"));
}

/// A directive states a fact about its own pattern's matches only, and never
/// filters them.
#[test]
fn pattern_properties_are_per_pattern() {
    let language: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
    let source = r#"((bareword) @a (#set! construct.method "new"))
(bareword) @b
"#;
    let query = super::super::cached_query(&language, source).unwrap();
    assert_eq!(pattern_property(query, 0, "construct.method"), Some("new"));
    assert_eq!(pattern_property(query, 1, "construct.method"), None);
    assert_eq!(pattern_property(query, 0, "other"), None);
}
