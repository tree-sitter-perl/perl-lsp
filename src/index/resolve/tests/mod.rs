use super::*;
use std::path::PathBuf;

mod refs_tests;
use refs_tests::parse;
mod rename_tests;
mod groups_tests;
mod candidate_set_tests;

mod projection_consistency_tests;
#[cfg(all(feature = "php", feature = "python"))]
mod construction_tests;
mod union_nav_tests;

#[test]
fn a_qualifier_scan_never_splits_a_character() {
    // Every cursor along a line of multi-byte characters: the scan back for
    // a `::` owner reads bytes, so no offset lands inside a character.
    let line = "x = '¯\\_(ツ)_/¯'; W::f();";
    let mut column = 0;
    for ch in line.chars() {
        let _ = qualifier_at_point(line, tree_sitter::Point { row: 0, column });
        column += ch.len_utf8();
    }
    let at_f = line.find("f()").unwrap();
    assert_eq!(qualifier_at_point(line, tree_sitter::Point { row: 0, column: at_f }), Some("W"));
}
