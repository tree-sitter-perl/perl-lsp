use super::*;
use std::sync::Arc;

fn parse(source: &str) -> FileAnalysis {
    let mut parser = crate::builder::create_parser();
    let tree = parser.parse(source, None).unwrap();
    crate::builder::build(&tree, source.as_bytes())
}

fn cache(idx: &crate::module_index::ModuleIndex, name: &str, src: &str) {
    idx.insert_cache(
        name,
        Some(Arc::new(crate::file_analysis::CachedModule::new(
            std::path::PathBuf::from(format!("/fake/g/{}.pm", name.replace("::", "/"))),
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
        std::ops::ControlFlow::Continue(())
    });
    // Perl DFS: A first, A's ancestors (Top, then the cycle back to C is
    // seen-guarded), then B.
    assert_eq!(order, vec!["A", "Top", "B"]);
}

#[test]
fn walk_descendants_matches_index_fan_out() {
    let idx = crate::module_index::ModuleIndex::new_for_test();
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
        std::ops::ControlFlow::Continue(())
    });
    got.sort();

    // `for_each_descendant_package` is the ModuleIndex BFS — a
    // different implementation than the graph walk, so this is a real
    // cross-check, not a tautology.
    let mut index_bfs: Vec<String> = Vec::new();
    idx.for_each_descendant_package("My::Role", &mut |pkg: &str, _cached: &Arc<crate::file_analysis::CachedModule>| {
        index_bfs.push(pkg.to_string());
        std::ops::ControlFlow::Continue(())
    });
    index_bfs.sort();
    assert_eq!(got, index_bfs, "graph fan-out must match the index BFS");
    assert_eq!(got, vec!["My::Composer", "My::Deep", "My::SubRole"]);
}

#[test]
fn walk_bridges_reaches_plugin_modules_terminally() {
    let idx = crate::module_index::ModuleIndex::new_for_test();
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
        EdgeKindMask::BRIDGES | EdgeKindMask::INHERITS,
        &mut |n| {
            if let Node::Module(m) = n {
                mods.push(m.clone());
            }
            std::ops::ControlFlow::Continue(())
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

// ── branded edges ───────────────────────────────────────────────────
// Two same-class receivers (here: two modules whose plugin namespaces
// all bridge to ONE surface class S) carry distinct plugin content
// without merging, because each namespace is branded by instance/file
// identity and the query is brand-scoped. See `docs/adr/branded-edges.md`.

/// Register a module whose single plugin namespace bridges to `surface`
/// under `brand`, owning one entity sub named `helper`.
fn branded_bridge_module(
    idx: &crate::module_index::ModuleIndex,
    mod_name: &str,
    surface: &str,
    brand: Option<&str>,
    helper: &str,
) {
    let mut fa = parse(&format!("package {mod_name};\nsub {helper} {{ 1 }}\n1;\n"));
    let sid = fa.symbols.iter().find(|s| s.name == helper).unwrap().id;
    let zero = crate::file_analysis::Span {
        start: tree_sitter::Point { row: 0, column: 0 },
        end: tree_sitter::Point { row: 0, column: 0 },
    };
    fa.plugin_namespaces.push(crate::file_analysis::PluginNamespace {
        id: format!("ns:{mod_name}"),
        plugin_id: "test".into(),
        kind: "app".into(),
        entities: vec![sid],
        bridges: vec![crate::file_analysis::Bridge::Class(surface.into())],
        brand: brand.map(|s| s.to_string()),
        decl_span: zero,
    });
    idx.insert_cache(
        mod_name,
        Some(Arc::new(crate::file_analysis::CachedModule::new(
            std::path::PathBuf::from(format!("/fake/g/{mod_name}.pm")),
            Arc::new(fa),
        ))),
    );
}

/// Names of entities reached from `surface` through the BRIDGES walk
/// under `brand`, read RAW off each reached module (the walk is the only
/// brand filter — the no-double-filter invariant).
fn helpers_visible(
    idx: &crate::module_index::ModuleIndex,
    surface: &str,
    brand: Option<&str>,
) -> Vec<String> {
    let probe = parse("package Probe;\n1;\n");
    let g = GraphView::new_branded(&probe, Some(idx), brand);
    let mut names: Vec<String> = Vec::new();
    g.walk(Node::Class(surface.into()), EdgeKindMask::BRIDGES, &mut |n| {
        if let Node::Module(m) = n {
            if let Some(cached) = idx.get_cached(m) {
                for ns in &cached.analysis.plugin_namespaces {
                    for sid in &ns.entities {
                        if let Some(sym) = cached.analysis.symbols.get(sid.0 as usize) {
                            names.push(sym.name.clone());
                        }
                    }
                }
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    names.sort();
    names
}

#[test]
fn branded_bridges_separate_same_class_receivers() {
    // R1: two apps' helpers bridge to the SAME surface S under distinct
    // brands; a brand-"one" query sees only app-one's helper.
    let idx = crate::module_index::ModuleIndex::new_for_test();
    let s = "App::Surface";
    branded_bridge_module(&idx, "App::One", s, Some("one"), "helper_one");
    branded_bridge_module(&idx, "App::Two", s, Some("two"), "helper_two");

    assert_eq!(helpers_visible(&idx, s, Some("one")), vec!["helper_one"]);
    assert_eq!(helpers_visible(&idx, s, Some("two")), vec!["helper_two"]);
}

#[test]
fn unbranded_bridge_is_global_under_every_brand() {
    // R2: an unbranded (global) namespace is visible under any brand
    // context AND under the agnostic (None) context; a branded one is
    // not — brands are additive, not a partition of the global set.
    let idx = crate::module_index::ModuleIndex::new_for_test();
    let s = "App::Surface";
    branded_bridge_module(&idx, "App::One", s, Some("one"), "helper_one");
    branded_bridge_module(&idx, "Shared::Plug", s, None, "global_helper");

    // brand "one": own + global, never app-two's (absent here) — global rides along.
    assert_eq!(
        helpers_visible(&idx, s, Some("one")),
        vec!["global_helper", "helper_one"],
    );
    // a different brand sees the global but NOT app-one's branded helper.
    assert_eq!(helpers_visible(&idx, s, Some("other")), vec!["global_helper"]);
    // agnostic (None) sees everything — the pre-brand behavior, no regression.
    assert_eq!(
        helpers_visible(&idx, s, None),
        vec!["global_helper", "helper_one"],
    );
}

#[test]
fn branded_filter_routes_through_the_one_bridge_primitive() {
    // R3: the brand filter lives behind the single bridge primitive, so
    // the graph walk and a direct `for_each_entity_bridged_to_branded`
    // call cannot disagree (no parallel walker). Also pins that the
    // unbranded `for_each_entity_bridged_to` == the None-brand case.
    let idx = crate::module_index::ModuleIndex::new_for_test();
    let s = "App::Surface";
    branded_bridge_module(&idx, "App::One", s, Some("one"), "helper_one");
    branded_bridge_module(&idx, "App::Two", s, Some("two"), "helper_two");

    let mut direct: Vec<String> = Vec::new();
    idx.for_each_entity_bridged_to_branded(
        s,
        Some("one"),
        |_m: &str, _c: &Arc<crate::file_analysis::CachedModule>, sym: &crate::file_analysis::Symbol| {
            direct.push(sym.name.clone());
        },
    );
    direct.sort();
    assert_eq!(direct, helpers_visible(&idx, s, Some("one")));

    // unbranded entry point == agnostic (None) == sees both
    let mut all: Vec<String> = Vec::new();
    idx.for_each_entity_bridged_to(
        s,
        |_m: &str, _c: &Arc<crate::file_analysis::CachedModule>, sym: &crate::file_analysis::Symbol| {
            all.push(sym.name.clone())
        },
    );
    all.sort();
    assert_eq!(all, vec!["helper_one", "helper_two"]);
}
