use super::*;
use tree_sitter::{Parser, QueryCursor, StreamingIterator};

const SRC: &str = "$a = $b;\nif (1) { $c = $d; }\n";

fn perl() -> Language {
    ts_parser_perl::LANGUAGE.into()
}

/// What `@s` holds in each admitted match, in document order.
fn admitted(query_src: &str) -> Vec<String> {
    let lang = perl();
    let query = Query::new(&lang, query_src).expect("query compiles");
    check(&query, &lang, &[]).expect("predicates check");
    let mut parser = Parser::new();
    parser.set_language(&lang).unwrap();
    let tree = parser.parse(SRC, None).unwrap();
    let root = tree.root_node();
    let syntax = crate::cst::SyntaxForest::new([root]);
    let mut out = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(&query, root, SRC.as_bytes());
    while let Some(m) = it.next() {
        if admits(&query, m, || &syntax) {
            out.extend(
                m.captures
                    .iter()
                    .filter(|c| query.capture_names()[c.index as usize] == "s")
                    .map(|c| c.node.utf8_text(SRC.as_bytes()).unwrap().to_string()),
            );
        }
    }
    out
}

#[test]
fn parent_tests_the_immediate_parent_kind() {
    assert_eq!(admitted("((scalar) @s (#has-parent? @s assignment_expression))"), ["$a", "$b", "$c", "$d"]);
    assert!(admitted("((scalar) @s (#not-has-parent? @s assignment_expression))").is_empty());
    assert!(admitted("((scalar) @s (#has-parent? @s block))").is_empty(), "a grandparent is not the parent");
}

#[test]
fn ancestor_tests_every_proper_ancestor() {
    assert_eq!(admitted("((scalar) @s (#has-ancestor? @s block))"), ["$c", "$d"]);
    assert_eq!(admitted("((scalar) @s (#not-has-ancestor? @s block))"), ["$a", "$b"]);
    assert_eq!(admitted("((scalar) @s (#has-ancestor? @s conditional_statement block))"), ["$c", "$d"]);
}

#[test]
fn a_predicate_on_a_predicate_only_capture_filters_the_match() {
    assert_eq!(
        admitted("((assignment_expression left: (scalar) @s) @_e (#has-ancestor? @_e block))"),
        ["$c"],
    );
}

#[test]
fn an_unknown_predicate_or_kind_fails_the_check() {
    let lang = perl();
    let compile = |src: &str| check(&Query::new(&lang, src).unwrap(), &lang, &["receiver-isa?"]);
    assert!(compile("((scalar) @s (#has-parnet? @s block))").is_err(), "a misspelled predicate");
    assert!(compile("((scalar) @s (#has-parent? @s blok))").is_err(), "a kind the grammar lacks");
    assert!(compile("((scalar) @s (#has-parent? @s))").is_err(), "no kind");
    assert!(compile("((scalar) @s (#receiver-isa? @s \"X\"))").is_ok(), "a deferred predicate");
}

#[test]
fn is_kind_tests_the_captured_node_itself() {
    // `$a = $b` and `$c = $d`: the left side of each is a scalar, the
    // statement around it is not.
    assert_eq!(admitted("(assignment_expression left: (_) @s (#is-kind? @s scalar))"), ["$a", "$c"]);
    assert!(admitted("(assignment_expression left: (_) @s (#not-is-kind? @s scalar))").is_empty());
}
