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
    let forest = SyntaxForest::new(tree.root_node());
    for n in every_node(tree.root_node()) {
        assert_eq!(forest.parent(n), n.parent(), "parent of {n:?}");
        assert_eq!(forest.prev_sibling(n), n.prev_sibling(), "prev sibling of {n:?}");
        let ts_ancestors: Vec<_> = std::iter::successors(n.parent(), |p| p.parent()).collect();
        assert_eq!(forest.ancestors(n).collect::<Vec<_>>(), ts_ancestors, "ancestors of {n:?}");
        assert_eq!(forest.root_of(n), Some(tree.root_node()));
    }
}

/// The code of `src[range]`, parsed over the file's own bytes the way the
/// builder parses an `s///e` replacement.
fn parse_range(src: &str, range: std::ops::Range<usize>) -> tree_sitter::Tree {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let whole = parse(src);
    let host = every_node(whole.root_node()).into_iter().find(|n| n.byte_range() == range).unwrap();
    p.set_included_ranges(&[host.range()]).unwrap();
    p.parse(src, None).unwrap()
}

fn replacement_range(src: &str, nth: usize) -> std::ops::Range<usize> {
    let tree = parse(src);
    every_node(tree.root_node())
        .into_iter()
        .filter(|n| n.kind() == "replacement")
        .nth(nth)
        .unwrap()
        .byte_range()
}

const EVALS: &str = "my $a = 1;\n$s =~ s/x/$o->m($b = 2)/e;\nmy $c = 3;\n$t =~ s/y/$p = 4/e;\nmy $d = 5;\n";

#[test]
fn a_node_is_answered_by_its_own_parse_only() {
    let main = parse(EVALS);
    let r = replacement_range(EVALS, 0);
    let code = parse_range(EVALS, r.clone());
    let forest = SyntaxForest::with_injections(
        main.root_node(),
        vec![Injection { host: 0, replaces: r, root: code.root_node() }],
    );
    for (tree, root) in [(&main, main.root_node()), (&code, code.root_node())] {
        for n in every_node(tree.root_node()) {
            assert_eq!(forest.root_of(n), Some(root), "{n:?} answered by the wrong parse");
        }
    }
    let ids: Vec<usize> = every_node(main.root_node())
        .into_iter()
        .chain(every_node(code.root_node()))
        .map(|n| forest.lift(n).unwrap().id().index())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, (0..forest.len()).collect::<Vec<_>>(), "ids cover the forest once");
}

#[test]
fn a_file_order_scan_reads_each_injection_in_place_of_its_string() {
    let main = parse(EVALS);
    let (r0, r1) = (replacement_range(EVALS, 0), replacement_range(EVALS, 1));
    let (c0, c1) = (parse_range(EVALS, r0.clone()), parse_range(EVALS, r1.clone()));
    // Registered out of file order: the scan follows the file, not the list.
    let forest = SyntaxForest::with_injections(
        main.root_node(),
        vec![
            Injection { host: 0, replaces: r1, root: c1.root_node() },
            Injection { host: 0, replaces: r0.clone(), root: c0.root_node() },
        ],
    );
    let assign = kind_ids(&ts_parser_perl::LANGUAGE.into(), &["assignment_expression"]);
    let got: Vec<&str> = forest
        .file_nodes_of_kind(&assign)
        .iter()
        .map(|n| &EVALS[n.start_byte()..n.end_byte()])
        .collect();
    assert_eq!(got, ["my $a = 1", "$b = 2", "my $c = 3", "$p = 4", "my $d = 5"]);

    let string_view = every_node(main.root_node())
        .into_iter()
        .find(|n| n.kind() == "scalar" && n.start_byte() > r0.start && n.end_byte() <= r0.end)
        .unwrap();
    assert!(forest.is_replaced(string_view));
    assert!(!forest.is_replaced(main.root_node()));
    let repl = every_node(main.root_node()).into_iter().find(|n| n.byte_range() == r0).unwrap();
    assert_eq!(forest.injected_at(repl), Some(c0.root_node()));
}

#[test]
fn a_parse_of_a_subtree_stops_at_its_root() {
    let tree = parse("sub a { 1 }\n");
    let body = tree.root_node().named_child(0).unwrap().child_by_field_name("body").unwrap();
    let forest = SyntaxForest::new(body);
    assert_eq!(forest.parent(body), None);
    let inner = body.named_child(0).unwrap();
    assert_eq!(forest.parent(inner), Some(body));
}

#[test]
fn subtree_and_site_agree_with_tree_sitter_on_every_node() {
    let tree = parse(SRC);
    let forest = SyntaxForest::new(tree.root_node());
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
    let forest = SyntaxForest::new(tree.root_node());
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
    let forest = SyntaxForest::new(tree.root_node());
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

#[test]
fn a_range_is_covered_by_the_smallest_node_holding_it() {
    let tree = parse(SRC);
    let forest = SyntaxForest::new(tree.root_node());
    let nodes = every_node(tree.root_node());
    for n in nodes.iter().filter(|n| n.start_byte() < n.end_byte()) {
        let got = forest.covering(n.start_position(), n.end_position()).unwrap().ts();
        // The deepest node of `n`'s same-range chain: `n` itself or under it.
        assert_eq!(got.byte_range(), n.byte_range(), "covering {n:?}");
        assert!(got == *n || forest.ancestors(got).any(|a| a == *n), "{got:?} is not under {n:?}");
    }
    // A range no single node spans: the smallest node holding both ends.
    let x = nodes.iter().find(|n| n.kind() == "scalar" && n.utf8_text(SRC.as_bytes()) == Ok("$x")).unwrap();
    let decl = forest.ancestors(*x).find(|a| a.kind() == "variable_declaration").unwrap();
    let mut end = x.end_position();
    end.column += 1;
    assert_eq!(forest.covering(x.start_position(), end).unwrap().ts(), decl);
}

#[test]
fn a_climb_runs_from_injected_code_on_through_its_host() {
    let main = parse(EVALS);
    let r = replacement_range(EVALS, 0);
    let code = parse_range(EVALS, r.clone());
    let forest = SyntaxForest::with_injections(
        main.root_node(),
        vec![Injection { host: 0, replaces: r.clone(), root: code.root_node() }],
    );
    let b = every_node(code.root_node())
        .into_iter()
        .find(|n| n.kind() == "scalar" && n.utf8_text(EVALS.as_bytes()) == Ok("$b"))
        .unwrap();
    // A span inside the replaced string is answered by the injected parse.
    let found = forest.covering(b.start_position(), b.end_position()).unwrap();
    assert_eq!(found.ts(), b);
    let climb: Vec<Node<'_>> = forest.climb_in_file(found).map(|n| n.ts()).collect();
    let repl = every_node(main.root_node()).into_iter().find(|n| n.byte_range() == r).unwrap();
    let at = climb.iter().position(|n| *n == repl).expect("the climb reaches the replaced node");
    assert_eq!(climb[at - 1], code.root_node(), "the injected root stands in for it");
    assert_eq!(climb.last(), Some(&main.root_node()));
}
