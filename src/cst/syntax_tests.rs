use super::*;

fn parse(src: &str) -> tree_sitter::Tree {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    p.parse(src, None).unwrap()
}

fn every_node(root: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        out.push(n);
        for i in (0..n.child_count()).rev() {
            stack.push(n.child(i).unwrap());
        }
    }
    out
}

// Includes a MISSING node (the unterminated call), zero-width and
// same-start chains, comments (extras), and nesting.
const SRC: &str = "package Foo;\n# c\nsub a { my ($x) = @_; return $x->{k}{j} if $x; }\n\
                   my %h = (a => 1, b => [2, 3]);\nfoo(1, 2\n";

#[test]
fn parent_and_siblings_agree_with_tree_sitter_on_every_node() {
    let tree = parse(SRC);
    let forest = SyntaxForest::new([tree.root_node()]);
    for n in every_node(tree.root_node()) {
        assert_eq!(forest.parent(n), n.parent(), "parent of {n:?}");
        assert_eq!(forest.prev_sibling(n), n.prev_sibling(), "prev sibling of {n:?}");
        let ts_ancestors: Vec<_> = std::iter::successors(n.parent(), |p| p.parent()).collect();
        assert_eq!(forest.ancestors(n).collect::<Vec<_>>(), ts_ancestors, "ancestors of {n:?}");
        assert_eq!(forest.root_of(n), Some(tree.root_node()));
    }
}

#[test]
fn a_node_is_answered_by_its_own_parse_only() {
    let main = parse("my $x = 1;\n");
    let other = parse("my $x = 1;\n");
    let forest = SyntaxForest::new([main.root_node(), other.root_node()]);
    for (tree, root) in [(&main, main.root_node()), (&other, other.root_node())] {
        for n in every_node(tree.root_node()) {
            assert_eq!(forest.root_of(n), Some(root), "{n:?} answered by the wrong parse");
        }
    }
}

#[test]
fn a_parse_of_a_subtree_stops_at_its_root() {
    let tree = parse("sub a { 1 }\n");
    let body = tree.root_node().named_child(0).unwrap().child_by_field_name("body").unwrap();
    let forest = SyntaxForest::new([body]);
    assert_eq!(forest.parent(body), None);
    let inner = body.named_child(0).unwrap();
    assert_eq!(forest.parent(inner), Some(body));
}
