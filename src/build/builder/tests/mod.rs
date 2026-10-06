use super::*;

fn parse(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
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

#[path = "../narrowing_tests.rs"]
mod narrowing;

#[path = "../pattern_dispatch_tests.rs"]
mod pattern_dispatch;
