use super::*;

fn parse(source: &str) -> Tree {
    create_parser().parse(source, None).unwrap()
}

fn build_fa(source: &str) -> FileAnalysis {
    let tree = parse(source);
    build(&tree, source.as_bytes())
}

/// `{}` — a closed shape with no keys yet.
fn empty_hash_shape() -> InferredType {
    InferredType::HashWithKeys { keys: SharedKeys::new(Vec::new()), open: false }
}

/// `{}` after it escaped: any key may have been written.
fn escaped_empty_hash_shape() -> InferredType {
    InferredType::HashWithKeys { keys: SharedKeys::new(Vec::new()), open: true }
}

mod core_tests;
mod invoke_tests;
mod refs_types_tests;
mod queries_recovery_tests;
mod inheritance_tests;
mod frameworks_tests;
mod plugins_mojo_tests;
mod plugins_queries_tests;
mod plugins_more_tests;
mod synthetic_isa_tests;
mod exports_runtime_tests;
mod globs_accessors_tests;
mod slots_hashkeys_tests;
mod assignment_ops_tests;
mod paren_spelling_tests;
mod comment_trivia_tests;
mod const_fold_tests;
mod union_tests;

#[path = "../narrowing_tests.rs"]
mod narrowing;

#[path = "../pattern_dispatch_tests.rs"]
mod pattern_dispatch;

/// A range-restricted fragment parse (the `s///e` replacements) must not
/// leave its included ranges on the reused parser for the next caller.
#[test]
fn the_reused_fragment_parser_drops_included_ranges() {
    let src = b"my $x = 1; my $y = 2;";
    let range = tree_sitter::Range {
        start_byte: 0,
        end_byte: 10,
        start_point: tree_sitter::Point::new(0, 0),
        end_point: tree_sitter::Point::new(0, 10),
    };
    let restricted = super::with_fragment_parser(|p| {
        p.set_included_ranges(&[range]).unwrap();
        p.parse(src, None).unwrap()
    });
    assert_eq!(restricted.root_node().end_byte(), 10);
    let whole = super::with_fragment_parser(|p| p.parse(src, None).unwrap());
    assert_eq!(whole.root_node().end_byte(), src.len());
}
