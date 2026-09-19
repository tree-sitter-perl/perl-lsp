//! The cursor-time query runner: what it reads off the document, and the
//! cost signature the three bounds buy.

// `LangPack` and the runner's seams: every body below is per-language, so
// a build with neither pack spells none of them — the same gate they carry.
#[cfg(any(feature = "cpp"))]
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
fn pattern_root_kinds_reads_cpps_member_shapes_off_the_document() {
    let pack = crate::build::query_extract::cpp_pack();
    let query = warm_query(tree_sitter_cpp::LANGUAGE.into(), &pack, "struct A { int x; };\n");
    let kinds = crate::build::query_extract::pattern_root_kinds(query, "member.recv");
    assert!(kinds.contains("field_expression"), "field_expression roots @member.recv: {kinds:?}");
    // A chain hop's receiver is `@hop.recv`, so the member-access kinds
    // stay exactly the shapes member completion climbs to.
    let hops = crate::build::query_extract::pattern_root_kinds(query, "hop.recv");
    assert!(hops.contains("call_expression"), "call_expression roots @hop.recv: {hops:?}");
}

/// A predicate's literals may contain the character that ends the predicate.
/// Stopping at the first `)` dropped every argument after it and abandoned
/// the rest of the pattern — a keyword quietly missing from a set.
#[test]
fn a_predicate_literal_may_contain_a_closing_paren() {
    let mut out = std::collections::HashSet::new();
    super::collect_capture_literals(
        "((name) @kw (#any-of? @kw \"a)b\" \"c\"))\n((name) @other (#eq? @other \"d\"))",
        "kw",
        &mut out,
    );
    let mut got: Vec<&str> = out.into_iter().collect();
    got.sort();
    assert_eq!(got, ["a)b", "c"], "both literals, and the scan survives the first");
    let mut other = std::collections::HashSet::new();
    super::collect_capture_literals(
        "((name) @kw (#any-of? @kw \"a)b\" \"c\"))\n((name) @other (#eq? @other \"d\"))",
        "other",
        &mut other,
    );
    assert_eq!(
        other.into_iter().collect::<Vec<_>>(),
        ["d"],
        "a later predicate is still reached"
    );
}
