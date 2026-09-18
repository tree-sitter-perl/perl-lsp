//! The cursor-time query runner: what it reads off the document, and the
//! cost signature the three bounds buy.

use super::*;

/// Extract one small file so the language's query is compiled and
/// remembered, then hand back the object a cursor verb would get.
#[cfg(any(feature = "php", feature = "cpp"))]
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

/// The node kinds a consumer may match a member access on come out of the
/// patterns that capture the receiver — the table `member_kinds` used to
/// hold, read off the document that already says it.
#[test]
#[cfg(feature = "php")]
fn pattern_root_kinds_reads_phps_member_shapes_off_the_document() {
    let pack = crate::build::query_extract::php_pack();
    let query = warm_query(tree_sitter_php::LANGUAGE_PHP.into(), &pack, "<?php\nclass A {}\n");
    let kinds = crate::build::query_extract::pattern_root_kinds(query, "member.recv");
    for expected in [
        "member_call_expression",
        "nullsafe_member_call_expression",
        "member_access_expression",
        "scoped_call_expression",
    ] {
        assert!(kinds.contains(expected), "{expected} roots a @member.recv pattern: {kinds:?}");
    }
    // A capture no pattern carries names no kind — the answer is empty, not
    // everything.
    assert!(
        crate::build::query_extract::pattern_root_kinds(query, "no.such.capture").is_empty()
    );
}

#[test]
#[cfg(feature = "cpp")]
fn pattern_root_kinds_reads_cpps_member_shapes_off_the_document() {
    let pack = crate::build::query_extract::cpp_pack();
    let query = warm_query(tree_sitter_cpp::LANGUAGE.into(), &pack, "struct A { int x; };\n");
    let kinds = crate::build::query_extract::pattern_root_kinds(query, "member.recv");
    for expected in ["field_expression", "call_expression"] {
        assert!(kinds.contains(expected), "{expected} roots a @member.recv pattern: {kinds:?}");
    }
}

/// The bounds, as a cost signature rather than a benchmark: on a file built
/// to punish an unbounded walk — a 2,000-hop member chain and a 5,000-method
/// class, cursor in the last method — classifying one node stays a
/// microsecond-scale operation and answers about THAT node only.
///
/// The ceiling is three orders of magnitude above the measured cost (~2 µs)
/// on purpose: it is a tripwire against reintroducing the prefix walk, not a
/// number to tune.
#[test]
#[cfg(feature = "php")]
fn the_bounded_runner_classifies_one_node_on_an_adversarial_file() {
    let pack = crate::build::query_extract::php_pack();
    let language: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let query = warm_query(language.clone(), &pack, "<?php\nclass A {}\n");

    let mut src = String::from("<?php\nclass Big {\n");
    for i in 0..5_000 {
        src.push_str(&format!("  function m{i}() {{ return {i}; }}\n"));
    }
    src.push_str("  function last() {\n    $chain = $seed");
    for i in 0..2_000 {
        src.push_str(&format!("->h{i}()"));
    }
    src.push_str(";\n    return $receiver->wanted;\n  }\n}\n");

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(&src, None).unwrap();

    // The cursor: the `$receiver->wanted` access in the last method.
    let at = src.find("$receiver->wanted").expect("the cursor site");
    let node = tree
        .root_node()
        .descendant_for_byte_range(at, at + 1)
        .and_then(|n| {
            let mut n = n;
            while n.kind() != "member_access_expression" {
                n = n.parent()?;
            }
            Some(n)
        })
        .expect("the member access at the cursor");

    // Warm the cursor's own allocations before timing.
    let _ = crate::build::query_extract::captures_at(query, node, src.as_bytes());
    let t = std::time::Instant::now();
    let captures = crate::build::query_extract::captures_at(query, node, src.as_bytes());
    let elapsed = t.elapsed();

    assert!(
        captures.iter().all(|(_, n)| n.byte_range().start >= node.byte_range().start
            && n.byte_range().end <= node.byte_range().end),
        "every capture is inside the cursor node"
    );
    let receivers: std::collections::HashSet<usize> = captures
        .iter()
        .filter(|(name, _)| *name == "member.recv")
        .map(|(_, n)| n.id())
        .collect();
    assert_eq!(receivers.len(), 1, "one receiver — the cursor's own: {captures:?}");
    let recv = captures.iter().find(|(name, _)| *name == "member.recv").unwrap().1;
    assert_eq!(recv.utf8_text(src.as_bytes()).unwrap(), "$receiver");

    assert!(
        elapsed < std::time::Duration::from_millis(5),
        "classifying one node took {elapsed:?} — a bound was dropped and the walk is \
         proportional to the file again"
    );

    // The depth cap, where it earns its keep: the enclosing method is a
    // 2,000-hop subtree, and only patterns rooted at the METHOD may answer
    // for it. Without the cap this matches every hop in the chain — the
    // 20 ms the bounds exist to avoid, and an answer about the wrong node.
    let method = {
        let mut n = node;
        while n.kind() != "method_declaration" {
            n = n.parent().expect("the enclosing method");
        }
        n
    };
    let t = std::time::Instant::now();
    let captures = crate::build::query_extract::captures_at(query, method, src.as_bytes());
    let elapsed = t.elapsed();
    assert!(
        !captures.iter().any(|(name, _)| *name == "member.recv"),
        "a receiver 2,000 hops below the cursor node answered: the depth cap is gone"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(5),
        "classifying a large node took {elapsed:?} — the walk descended into it"
    );
}
