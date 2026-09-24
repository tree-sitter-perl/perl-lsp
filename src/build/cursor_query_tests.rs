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

/// The literals come off the compiled query exactly: a literal holding the
/// character that closes a predicate, each capture's own set, and nothing
/// from a negated or capture-to-capture predicate.
#[test]
fn literals_are_read_off_the_compiled_query() {
    let language: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
    let source = r#"((bareword) @kw (#any-of? @kw "a)b" "c"))
((bareword) @other (#eq? @other "d"))
((bareword) @neg (#not-eq? @neg "e"))
((bareword) @x (bareword) @y (#eq? @x @y))
"#;
    let query = super::super::cached_query(&language, source).unwrap();
    let sorted = |cap: &str| {
        let mut v: Vec<&str> = capture_literals(query, cap).iter().copied().collect();
        v.sort();
        v
    };
    assert_eq!(sorted("kw"), ["a)b", "c"]);
    assert_eq!(sorted("other"), ["d"]);
    assert!(sorted("neg").is_empty(), "a negated predicate names no member of the set");
    assert!(sorted("x").is_empty() && sorted("y").is_empty(), "@x @y compares captures, not literals");
}
