use super::*;
use crate::cst::{node_to_span, SyntaxForest};

fn parse(source: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    parser.parse(source, None).unwrap()
}

fn view_of(tree: &tree_sitter::Tree, source: &str) -> ControlView {
    let (query, caps) = ctl_query();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut out = Vec::new();
    let mut it = cursor.matches(query, tree.root_node(), source.as_bytes());
    use tree_sitter::StreamingIterator;
    while let Some(m) = it.next() {
        out.push(captured(m.captures.iter().map(|c| (caps[c.index as usize], c.node)), source.as_bytes()));
    }
    control_view(out)
}

fn is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "subroutine_declaration_statement" | "method_declaration_statement" | "anonymous_subroutine_expression" | "source_file"
    )
}

/// Every named node's innermost arm, as the climb from the node reads it
/// ([`arm_around`], what the walk asks) and as the view of the whole file
/// holds it, inside the node's callable.
fn disagreements(source: &str) -> Vec<String> {
    let tree = parse(source);
    let view = view_of(&tree, source);
    let syn = SyntaxForest::new(tree.root_node());
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        for i in (0..n.named_child_count()).rev() {
            stack.push(n.named_child(i).unwrap());
        }
        if matches!(n.kind(), "label" | "source_file") {
            continue;
        }
        let Some(within) = syn.ancestors(n).find(|a| is_callable(a.kind())) else { continue };
        let climb = arm_around(&syn, source.as_bytes(), n, ArmBound::Sub).map(node_to_span);
        let doc = view.arm_around(node_to_span(n), node_to_span(within));
        let doc = doc.map(|a| a.span);
        if climb != doc {
            out.push(format!(
                "{} at {:?}: climb {:?}, document {:?}",
                n.kind(),
                n.start_position(),
                climb.map(|s| (s.start, s.end)),
                doc.map(|s| (s.start, s.end))
            ));
        }
    }
    out
}

const SHAPES: &str = r#"
sub f {
    my ($a, $b, @l) = @_;
    if ($a) { g(1) } elsif ($b) { g(2) } else { g(3) }
    unless ($a) { g(4) } elsif ($b) { g(5) } else { g(6) }
    while ($a) { last if $b } continue { next }
    until ($a) { redo }
    for (my $i = 0; $i < 3; $i++) { g($i) }
    LOOP: for my $x (@l) { next LOOP unless $x }
    g(7) if $a;
    g(8) unless $a;
    g(9) while $a;
    g(10) for @l;
    do { g(11) } while ($a);
    my $t = $a ? g(12) : g(13);
    $a and g(14);
    $a or g(15);
    $a && g(16);
    $a || g(17);
    $a // g(18);
    my $c = sub { return g(19) if $a; 1 };
    goto &g if $b;
    return $a;
}
"#;

#[test]
fn the_climb_finds_the_arms_the_whole_view_holds() {
    assert_eq!(disagreements(SHAPES), Vec::<String>::new());
}

#[test]
fn the_climb_and_the_whole_view_agree_on_the_repository_perl() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    let mut dirs = vec![root.join("test_files"), root.join("gold-corpus")];
    while let Some(d) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // `deepcst-fixture` nests thousands deep: every node's climb
                // walks them all, and the cross-check takes half an hour.
                if p.file_name().is_some_and(|n| n != "local" && n != "deepcst-fixture") {
                    dirs.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "pm" || x == "pl" || x == "t") {
                files.push(p);
            }
        }
    }
    assert!(files.len() > 50, "the repository's Perl files: {}", files.len());
    let mut all = Vec::new();
    for f in files {
        let source = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
        all.extend(disagreements(&source).into_iter().map(|d| format!("{}: {d}", f.display())));
    }
    assert!(all.is_empty(), "{} disagreements:\n{}", all.len(), all.join("\n"));
}

/// The view read at a file's assignments' ancestors holds, for each of them, the
/// same arms as the view of the whole file: the joins read the first.
#[test]
fn the_view_around_the_writes_holds_what_the_whole_view_does() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in ["test_files", "gold-corpus/run.pl"] {
        let mut files = vec![root.join(dir)];
        while let Some(p) = files.pop() {
            if p.is_dir() {
                files.extend(std::fs::read_dir(&p).unwrap().flatten().map(|e| e.path()));
                continue;
            }
            if !p.extension().is_some_and(|x| x == "pm" || x == "pl" || x == "t") {
                continue;
            }
            let source = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
            let tree = parse(&source);
            let whole = view_of(&tree, &source);
            let mut writes = Vec::new();
            let mut stack = vec![tree.root_node()];
            while let Some(n) = stack.pop() {
                stack.extend((0..n.named_child_count()).filter_map(|i| n.named_child(i)));
                if n.kind() == "assignment_expression" {
                    writes.push(n);
                }
            }
            let mut seen = std::collections::HashSet::new();
            let mut nodes = Vec::new();
            for w in &writes {
                let mut n = Some(*w);
                while let Some(x) = n {
                    if seen.insert(x.id()) {
                        nodes.push(x);
                    }
                    n = x.parent();
                }
            }
            let around = control_view_with_gates(nodes, source.as_bytes(), |_| false);
            for w in writes {
                let held = |v: &ControlView| {
                    let mut out: Vec<_> = v
                        .constructs
                        .iter()
                        .flat_map(|c| c.arms.iter().map(move |a| (c.span, c.kind, a.span, a.role)))
                        .filter(|(_, _, a, _)| a.start <= node_to_span(w).start && node_to_span(w).end <= a.end)
                        .collect();
                    out.sort_by_key(|(c, _, a, _)| (*c, *a));
                    out
                };
                assert_eq!(held(&around), held(&whole), "{} at {:?}", p.display(), w.start_position());
            }
        }
    }
}

#[test]
fn constructs_carry_their_roles_and_leaves_their_kinds() {
    let tree = parse(SHAPES);
    let view = view_of(&tree, SHAPES);
    let row = |r: usize| view.constructs.iter().filter(move |c| c.span.start.row == r);
    let roles = |r: usize| -> Vec<ArmRole> { row(r).flat_map(|c| c.arms.iter().map(|a| a.role)).collect() };
    assert_eq!(roles(3), vec![ArmRole::When(true), ArmRole::When(false), ArmRole::When(true), ArmRole::When(false)], "if / elsif / else");
    assert_eq!(roles(4)[..2], [ArmRole::When(false), ArmRole::When(true)], "unless runs its block where the test fails");
    assert!(row(13).any(|c| c.kind == ControlKind::Loop { once: true }), "do … while runs its body first");
    assert!(row(5).any(|c| c.arms.iter().any(|a| a.role == ArmRole::Step)), "a continue block is the loop's step");
    let kinds: Vec<LeaveKind> = view.leaves.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        vec![
            LeaveKind::Break,
            LeaveKind::Continue,
            LeaveKind::Continue,
            LeaveKind::Continue,
            LeaveKind::Return,
            LeaveKind::Jump,
            LeaveKind::Return
        ]
    );
}

/// From a construct, the view reaches the statements its arms run, a loop's
/// body among them, down to the ones that leave and the calls that gate.
#[test]
fn the_view_from_a_construct_reaches_the_leaves_in_its_arms() {
    let source = "if ($c) { foo(); return 1 if $d; die 'x'; } else { for (@l) { last } }\n";
    let tree = parse(source);
    let stmt = tree.root_node().named_child(0).unwrap();
    let view = control_view_with_gates([stmt], source.as_bytes(), |n| n.kind() == "ambiguous_function_call_expression");
    let mut leaves: Vec<_> = view.leaves.iter().map(|l| (l.kind, &source[l.span.start.column..l.span.end.column])).collect();
    leaves.sort_by_key(|(_, t)| *t);
    assert_eq!(leaves, vec![(LeaveKind::Break, "last"), (LeaveKind::Return, "return 1")]);
    let gates: Vec<_> = view.gates.iter().map(|g| &source[g.start.column..g.end.column]).collect();
    assert_eq!(gates, vec!["die 'x'"]);
    assert_eq!(view.constructs.len(), 3, "the `if`, the postfix `if`, and the loop: {view:?}");
}
