use super::*;
use std::sync::Arc;

fn parse(source: &str) -> FileAnalysis {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(source, None).unwrap();
    crate::build::builder::build(&tree, source.as_bytes())
}

fn cache(idx: &crate::index::module_index::ModuleIndex, name: &str, src: &str) {
    idx.insert_cache(
        name,
        Some(Arc::new(crate::model::file_analysis::CachedModule::new(
            crate::util::canonical_path::CanonicalPath::fixture(std::path::PathBuf::from(format!("/fake/g/{}.pm", name.replace("::", "/")))),
            Arc::new(parse(src)),
        ))),
    );
}

#[test]
fn walk_inherits_preserves_isa_order_and_caps_cycles() {
    // Diamond with a cycle: C isa (A, B); A isa Top; B isa Top; Top isa C (cycle).
    let fa = parse(
        "package Top;\nuse parent -norequire, 'C';\n\
         package A;\nuse parent -norequire, 'Top';\n\
         package B;\nuse parent -norequire, 'Top';\n\
         package C;\nuse parent -norequire, 'A', 'B';\n1;\n",
    );
    let g = GraphView::new(&fa, None);
    let mut order: Vec<String> = Vec::new();
    g.walk(Node::Class("C".into()), EdgeKindMask::INHERITS, &mut |n| {
        if let Node::Class(c) = n {
            order.push(c.clone());
        }
        WalkControl::Continue
    });
    // Perl DFS: A first, A's ancestors (Top, then the cycle back to C is
    // seen-guarded), then B.
    assert_eq!(order, vec!["A", "Top", "B"]);
}

#[test]
fn walk_descendants_matches_index_fan_out() {
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    cache(&idx, "My::Role", "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n1;\n");
    cache(&idx, "My::Composer", "package My::Composer;\nuse Moo;\nwith 'My::Role';\nsub fetch {1}\n1;\n");
    cache(&idx, "My::SubRole", "package My::SubRole;\nuse Moo::Role;\nwith 'My::Role';\n1;\n");
    cache(&idx, "My::Deep", "package My::Deep;\nuse Moo;\nwith 'My::SubRole';\nsub fetch {7}\n1;\n");

    let fa = parse("package Probe;\n1;\n");
    let g = GraphView::new(&fa, Some(&idx));
    let mut got: Vec<String> = Vec::new();
    g.walk(Node::Class("My::Role".into()), EdgeKindMask::INHERITS_INV, &mut |n| {
        if let Node::Class(c) = n {
            got.push(c.clone());
        }
        WalkControl::Continue
    });
    got.sort();

    // `for_each_descendant_package` is the ModuleIndex BFS — a
    // different implementation than the graph walk, so this is a real
    // cross-check, not a tautology.
    let mut index_bfs: Vec<String> = Vec::new();
    idx.for_each_descendant_package("My::Role", &mut |pkg: &str, _cached: &Arc<crate::model::file_analysis::CachedModule>| {
        index_bfs.push(pkg.to_string());
        std::ops::ControlFlow::Continue(())
    });
    index_bfs.sort();
    assert_eq!(got, index_bfs, "graph fan-out must match the index BFS");
    assert_eq!(got, vec!["My::Composer", "My::Deep", "My::SubRole"]);
}

#[test]
fn walk_bridges_reaches_plugin_modules_terminally() {
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let plugin_src = "package My::Plugin::W;\nuse Mojo::Base 'Mojolicious::Plugin';\n\
        sub register {\n    my ($self, $app) = @_;\n    $app->helper(wcount => sub {1});\n}\n1;\n";
    idx.register_workspace_module(
        std::path::PathBuf::from("/fake/g/W.pm"),
        Arc::new(parse(plugin_src)),
    );
    let fa = parse("package Probe;\n1;\n");
    let g = GraphView::new(&fa, Some(&idx));
    // bridges target the synthetic app surface; Controller reaches it
    // through the INHERITS synthetic edge — the masks compose the way
    // the separate ancestor + bridge walks once did, in ONE walker.
    let mut mods: Vec<String> = Vec::new();
    g.walk(
        Node::Class("Mojolicious::Controller".into()),
        EdgeKindMask::BRIDGES | EdgeKindMask::INHERITS | EdgeKindMask::APP_SURFACE,
        &mut |n| {
            if let Node::Module(m) = n {
                mods.push(m.clone());
            }
            WalkControl::Continue
        },
    );
    assert_eq!(mods, vec!["My::Plugin::W"]);
}


#[test]
fn class_isa_agrees_with_ancestor_walk() {
    // class_isa (reflexive check + walk) and for_each_ancestor_class
    // (self-visit + walk) compose the same INHERITS traversal two
    // ways; they must answer identically on every shape — reflexive,
    // direct, transitive, role, diamond, and negative.
    let fa = parse(
        "package Base;\n1;\n\
         package Mid;\nuse parent -norequire, 'Base';\n1;\n\
         package Leaf;\nuse parent -norequire, 'Mid';\n1;\n\
         package R;\nuse Moo::Role;\n1;\n\
         package Composer;\nuse Moo;\nwith 'R';\nextends 'Leaf';\n1;\n\
         package Unrelated;\n1;\n",
    );
    let cases = [
        ("Leaf", "Leaf", true),     // reflexive
        ("Leaf", "Mid", true),      // direct
        ("Leaf", "Base", true),     // transitive
        ("Composer", "Base", true), // through extends → Leaf → Mid → Base
        ("Composer", "R", true),    // role composition
        ("Leaf", "Unrelated", false),
        ("Base", "Leaf", false),    // wrong direction
    ];
    for (child, ancestor, want) in cases {
        // class_isa's answer
        let got = fa.class_isa(child, ancestor, None);
        // the include-self walk over the same data
        let mut legacy = child == ancestor;
        fa.for_each_ancestor_class_test(child, None, |c| {
            if c == ancestor {
                legacy = true;
            }
            std::ops::ControlFlow::Continue(())
        });
        assert_eq!(got, want, "class_isa({child}, {ancestor})");
        assert_eq!(got, legacy, "class_isa vs ancestor walk disagree on ({child}, {ancestor})");
    }
}

#[test]
fn edge_kind_all_covers_every_mask_bit() {
    // Lockstep guard: `flag()` is an exhaustive match (a variant
    // without a flag arm won't compile), and `edges_from` matches
    // exhaustively too — but `EdgeKind::ALL` is a fixed-length array,
    // so a variant added everywhere EXCEPT `ALL` would compile and
    // silently never be walked. This pins that the ALL-driven union
    // equals the full mask, catching that one hole.
    let union = EdgeKind::ALL
        .iter()
        .fold(EdgeKindMask::empty(), |acc, k| acc | k.flag());
    assert_eq!(
        union.bits(),
        EdgeKindMask::all().bits(),
        "an EdgeKind is missing from EdgeKind::ALL",
    );
    // and every flag is distinct (no two variants share a bit)
    assert_eq!(
        EdgeKind::ALL.len(),
        EdgeKind::ALL.iter().map(|k| k.flag().bits()).collect::<std::collections::HashSet<_>>().len(),
    );
}

#[test]
fn ancestor_funnel_includes_self_then_mro_order() {
    // The include-self funnel (for_each_ancestor_class) must visit the
    // origin FIRST, then proper ancestors in Perl's left-to-right DFS
    // MRO — the contract the ~7 method/dispatch/rename consumers rely
    // on. `A isa (Left, Right)`, each isa Base.
    let fa = parse(
        "package Base;\n1;\n\
         package Left;\nuse parent -norequire, 'Base';\n1;\n\
         package Right;\nuse parent -norequire, 'Base';\n1;\n\
         package A;\nuse parent -norequire, 'Left', 'Right';\n1;\n",
    );
    let mut order: Vec<String> = Vec::new();
    fa.for_each_ancestor_class_test("A", None, |c| {
        order.push(c.to_string());
        std::ops::ControlFlow::Continue(())
    });
    // self first; then Left and its ancestors (Base) before Right —
    // DFS, not BFS — and Base seen-once despite the diamond.
    assert_eq!(order, vec!["A", "Left", "Base", "Right"]);
}

#[test]
fn walk_specializes_is_family_view_only() {
    // Local (spec → primary) map drives the edge; member resolution's
    // INHERITS mask never traverses it.
    let mut fa = parse("package Probe;\n1;\n");
    fa.pack.specializes.insert("formatter<int, char>".into(), "formatter".into());
    fa.pack.specializes.insert("formatter<T*, char>".into(), "formatter".into());
    let g = GraphView::new(&fa, None);
    let mut fam: Vec<String> = Vec::new();
    g.walk(Node::Class("formatter".into()), EdgeKindMask::SPECIALIZES, &mut |n| {
        if let Node::Class(c) = n {
            fam.push(c.clone());
        }
        WalkControl::Continue
    });
    assert_eq!(fam, vec!["formatter<T*, char>", "formatter<int, char>"], "sorted, deterministic");
    // the inheritance mask sees nothing — a spec REPLACES, never inherits
    let mut inh: Vec<String> = Vec::new();
    g.walk(Node::Class("formatter".into()), EdgeKindMask::INHERITS | EdgeKindMask::INHERITS_INV, &mut |n| {
        if let Node::Class(c) = n {
            inh.push(c.clone());
        }
        WalkControl::Continue
    });
    assert!(inh.is_empty(), "member resolution must not fall through Specializes: {inh:?}");
}

#[test]
fn walk_prune_children_skips_expansion_but_continues() {
    // A isa (B, C); B isa D; C isa E. Pruning at B must skip D (B's own
    // expansion) while the walk still reaches C and E — the verdict the
    // role-requires gather relies on (prune at non-role nodes).
    let fa = parse(
        "package D;\n1;\n\
         package E;\n1;\n\
         package B;\nuse parent -norequire, 'D';\n1;\n\
         package C;\nuse parent -norequire, 'E';\n1;\n\
         package A;\nuse parent -norequire, 'B', 'C';\n1;\n",
    );
    let g = GraphView::new(&fa, None);
    let mut order: Vec<String> = Vec::new();
    g.walk(Node::Class("A".into()), EdgeKindMask::INHERITS, &mut |n| {
        let Node::Class(c) = n else { return WalkControl::Continue };
        order.push(c.clone());
        if c == "B" { WalkControl::PruneChildren } else { WalkControl::Continue }
    });
    assert_eq!(order, vec!["B", "C", "E"], "D pruned with B; C's line still walked");
}

#[test]
fn walk_depth_cap_bounds_pathological_chains() {
    // A 30-deep linear parent chain: the depth cap (21) bounds the walk
    // — both re-expressed bespoke walkers (trigger view, role requires)
    // inherit this backstop instead of hand-rolled caps.
    let mut src = String::from("package C30;\n1;\n");
    for i in (0..30).rev() {
        src.push_str(&format!(
            "package C{i};\nuse parent -norequire, 'C{}';\n1;\n",
            i + 1
        ));
    }
    let fa = parse(&src);
    let g = GraphView::new(&fa, None);
    let mut visited = 0usize;
    g.walk(Node::Class("C0".into()), EdgeKindMask::INHERITS, &mut |_| {
        visited += 1;
        WalkControl::Continue
    });
    assert_eq!(visited, 21, "the MAX_DEPTH backstop bounds a pathological chain");
}

/// DIVERGENT-CASE PIN, written BEFORE the walk engines collapse: the two
/// walkers bound differently — `walk_ancestry` budgets total visits (200),
/// `GraphView::walk` caps depth (21) — and a wide shallow fan-out passes
/// one while a deep chain passes the other. After the collapse both ride
/// one engine and nothing can DETECT a silently changed guarantee (the
/// consistency net compares siblings; there will be no siblings), so each
/// walker's guarantee is pinned in advance. This is the fan-out half: a
/// 300-child hierarchy enumerates COMPLETELY — implementations over a
/// wide schema must never be truncated by a small visit budget.
#[test]
fn wide_fanout_enumerates_completely_despite_any_visit_budget() {
    let mut src = String::from("package Base;\n1;\n");
    for i in 0..300 {
        src.push_str(&format!(
            "package Kid{i};\nuse parent -norequire, 'Base';\n1;\n"
        ));
    }
    let fa = parse(&src);
    let g = GraphView::new(&fa, None);
    let mut visited = 0usize;
    g.walk(Node::Class("Base".into()), EdgeKindMask::INHERITS_INV, &mut |_| {
        visited += 1;
        WalkControl::Continue
    });
    assert_eq!(visited, 300, "every child of a wide hierarchy is reached (depth 1, no budget bite)");
}
