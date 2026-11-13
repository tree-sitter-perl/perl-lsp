use super::*;
use crate::cst::{conditional_arm, node_to_span, ArmBound, SyntaxForest};
use crate::model::file_analysis::Span;

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
        out.push(captured(m.captures.iter().map(|c| (caps[c.index as usize], c.node))));
    }
    control_view(out)
}

fn is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "subroutine_declaration_statement" | "method_declaration_statement" | "anonymous_subroutine_expression" | "source_file"
    )
}

/// `xor` evaluates both operands, so its right one is no arm; the walker's
/// climb counts it as one.
// TODO(xor-arm): `cst::conditional_arm` treats `xor`'s right operand as
// conditionally run; fix it with a census, since it bounds observations.
fn walker_counts_xor(arm: tree_sitter::Node, source: &str) -> bool {
    arm.parent().is_some_and(|p| {
        p.kind() == "lowprec_logical_expression" && (0..p.child_count()).filter_map(|i| p.child(i)).any(|c| !c.is_named() && c.utf8_text(source.as_bytes()) == Ok("xor"))
    })
}

/// Every named node's innermost arm, as the view reads it from the document
/// and as `cst::conditional_arm` climbs to it, inside the node's callable.
/// An `else` arm is its block in the document and the `else` node in the
/// walker; both hold the same statements. The walker stops at an `elsif`
/// (its whole chain is one arm of the `if`), where the document goes on to
/// the `elsif`'s own arms, so the document's answer is lifted out of every
/// `elsif` before the two are compared.
fn disagreements(source: &str) -> Vec<String> {
    let tree = parse(source);
    let view = view_of(&tree, source);
    let syn = SyntaxForest::new(tree.root_node());
    let mut elsifs = std::collections::HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.kind() == "elsif" {
            elsifs.insert(node_to_span(n));
        }
        stack.extend((0..n.named_child_count()).filter_map(|i| n.named_child(i)));
    }
    let owner = |arm: &Arm| view.constructs.iter().find(|c| c.arms.contains(arm)).map(|c| c.span);
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        for i in (0..n.named_child_count()).rev() {
            stack.push(n.named_child(i).unwrap());
        }
        // An `else` node is its keyword plus its block; the block is the arm.
        if matches!(n.kind(), "label" | "source_file" | "else") {
            continue;
        }
        let Some(within) = syn.ancestors(n).find(|a| is_callable(a.kind())) else { continue };
        let walker = conditional_arm(&syn, n, ArmBound::Sub);
        if walker.is_some_and(|a| walker_counts_xor(a, source)) {
            continue;
        }
        let walker: Option<Span> = walker.map(|a| match a.kind() {
            "else" => node_to_span(a.child_by_field_name("block").unwrap_or(a)),
            _ => node_to_span(a),
        });
        let within = node_to_span(within);
        let mut doc = view.arm_around(node_to_span(n), within);
        while let Some(elsif) = doc.and_then(owner).filter(|c| elsifs.contains(c)) {
            doc = view.arm_around(elsif, within);
        }
        let doc = doc.map(|a| a.span);
        if walker != doc {
            out.push(format!(
                "{} at {:?}: walker {:?}, document {:?}",
                n.kind(),
                n.start_position(),
                walker.map(|s| (s.start, s.end)),
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
fn the_document_finds_the_arms_the_walker_climbs_to() {
    assert_eq!(disagreements(SHAPES), Vec::<String>::new());
}

#[test]
fn the_document_and_the_walker_agree_on_the_repository_perl() {
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
            let around = control_view_at(nodes, source.as_bytes());
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

