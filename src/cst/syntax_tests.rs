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

#[test]
fn subtree_and_site_agree_with_tree_sitter_on_every_node() {
    let tree = parse(SRC);
    let forest = SyntaxForest::new([tree.root_node()]);
    let s = forest.main();
    for n in every_node(tree.root_node()) {
        let h = s.lift(n).unwrap();
        assert_eq!(h.subtree().len(), n.descendant_count(), "subtree of {n:?}");
        assert_eq!((h.start_byte(), h.end_byte()), (n.start_byte(), n.end_byte()));
        let range = n.byte_range();
        let mut head = n;
        while let Some(p) = head.parent().filter(|p| p.byte_range() == range) {
            head = p;
        }
        assert_eq!(h.site().ts(), head, "site of {n:?}");
        let sites: Vec<_> = h.enclosing_sites().map(|a| a.ts()).collect();
        let mut want = Vec::new();
        let mut cur = head.parent();
        while let Some(p) = cur {
            if p.parent().is_none_or(|g| g.byte_range() != p.byte_range()) {
                want.push(p);
            }
            cur = p.parent();
        }
        assert_eq!(sites, want, "enclosing sites of {n:?}");
    }
}

#[test]
fn facts_sit_on_their_nodes_in_arrival_order() {
    let tree = parse("my $x = 1;\n");
    let forest = SyntaxForest::new([tree.root_node()]);
    let s = forest.main();
    let ids: Vec<NodeId> = (0..s.len()).map(|i| s.node(NodeId(i as u32)).id()).collect();
    let facts = NodeFacts::new(s.len(), vec![(ids[2], "b"), (ids[1], "a"), (ids[2], "c")]);
    assert_eq!(&facts.all()[facts.on(ids[2])], ["b", "c"]);
    assert_eq!(&facts.all()[facts.on(ids[1])], ["a"]);
    // node 2 sits under node 1, so node 1's subtree holds all three
    assert_eq!(facts.positions(s.node(ids[1]).subtree()), 0..3);
    assert!(facts.on(ids[0]).is_empty());
}

#[test]
fn a_kind_scan_finds_what_a_walk_finds_under_every_node() {
    let tree = parse(SRC);
    let forest = SyntaxForest::new([tree.root_node()]);
    let nodes = every_node(tree.root_node());
    let mut kinds: Vec<u16> = nodes.iter().map(|n| n.kind_id()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    for n in &nodes {
        let lifted = forest.lift(*n).unwrap();
        for k in &kinds {
            let want: Vec<_> =
                every_node(*n).into_iter().skip(1).filter(|d| d.kind_id() == *k).collect();
            let got: Vec<_> = lifted.descendants_of_kind(&[*k]).map(|d| d.ts()).collect();
            assert_eq!(got, want, "kind {k} under {n:?}");
        }
    }
}
