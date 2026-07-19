use super::*;

fn parse_source_to_cached(source: &str, module_name: &str) -> Arc<CachedModule> {
    use tree_sitter::Parser;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let analysis = crate::builder::build(&tree, source.as_bytes());
    Arc::new(CachedModule::new(
        PathBuf::from(format!("/fake/{}.pm", module_name.replace("::", "/"))),
        Arc::new(analysis),
    ))
}

/// Slice-2 crux (R1): a bag-EVICTED cached analysis, queried through
/// `bag_present`, rehydrates to a bag-PRESENT analysis via the pack index's
/// `PackBagCache` — byte-identical bag whether the LRU retains (cap>0) or
/// re-decodes every time (cap==0). This is the seam every cross-file TYPE
/// query routes through; if it regressed, references/goto stay green while
/// type inference silently returns None into evicted files.
#[test]
fn bag_present_rehydrates_evicted_at_both_caps() {
    use crate::pack_bag_cache::PackBagCache;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\nsub name { my $s = shift; return 'w'; }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let full_bag_len = full.analysis.witnesses.len();
    assert!(full_bag_len > 0, "fixture must have a populated bag");
    let path = full.path.clone();

    // The resident copy the index would register: bag stripped.
    let mut stripped = (*full.analysis).clone();
    stripped.evict_witness_bag();
    assert!(stripped.bag_is_evicted() && stripped.witnesses.is_empty());
    let stripped_cached = Arc::new(CachedModule::new(path.clone(), Arc::new(stripped)));

    for cap in [8 * 1024 * 1024usize, 0] {
        // Loader hands back the FULL analysis (as SQLite would after decode).
        let full_for_loader = full.analysis.clone();
        let cache = Arc::new(PackBagCache::new(cap, move |_p| {
            Ok((*full_for_loader).clone())
        }));
        let idx = ModuleIndex::new_for_cli().with_bag_cache(cache);
        let got = idx.bag_present(&stripped_cached);
        assert!(!got.bag_is_evicted(), "cap={cap}: rehydrated must be bag-present");
        assert_eq!(
            got.witnesses.len(),
            full_bag_len,
            "cap={cap}: rehydrated bag must be byte-identical in length"
        );
    }

    // A non-evicted cached analysis (open doc / Perl hub) is a cheap pass-
    // through — no cache, no rehydration, same bag.
    let hub = ModuleIndex::new_for_cli(); // no bag_cache
    let got = hub.bag_present(&full);
    assert_eq!(got.witnesses.len(), full_bag_len);
    assert!(!got.bag_is_evicted());
}

#[test]
fn test_resolve_module_list_util() {
    let idx = ModuleIndex::new_for_test();
    let path = idx.resolve_module("List::Util");
    if !idx.inc_paths().is_empty() {
        assert!(path.is_some(), "List::Util should be resolvable");
        let p = path.unwrap();
        assert!(p.to_str().unwrap().contains("List/Util.pm"));
    }
}

#[test]
fn test_extract_exports_list_util() {
    let idx = ModuleIndex::new_for_test();
    if idx.inc_paths().is_empty() {
        return;
    }
    let cached = idx.get_cached_blocking("List::Util");
    assert!(cached.is_some(), "Should parse List::Util");
    let cached = cached.unwrap();
    assert!(
        cached.analysis.export_ok.contains(&"first".to_string()),
        "List::Util should export_ok 'first', got: {:?}",
        cached.analysis.export_ok
    );
    assert!(
        cached.analysis.export_ok.contains(&"any".to_string()),
        "List::Util should export_ok 'any'"
    );
    assert!(
        cached.analysis.export_ok.contains(&"min".to_string()),
        "List::Util should export_ok 'min'"
    );
}

#[test]
fn test_module_resolution_not_found() {
    let idx = ModuleIndex::new_for_test();
    assert!(idx.resolve_module("Nonexistent::Module::XYZ123").is_none());
}

#[test]
fn test_resolver_thread_flow() {
    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);
    if idx.inc_paths().is_empty() {
        return;
    }
    idx.request_resolve("Carp");
    assert!(
        idx.wait_resolved("Carp", std::time::Duration::from_secs(10)),
        "Carp should be resolved via thread"
    );
    let cached = idx.get_cached("Carp").unwrap();
    assert!(
        cached.analysis.export.contains(&"carp".to_string()),
        "Carp should export 'carp', got: {:?}",
        cached.analysis.export
    );
    assert!(
        cached.analysis.export.contains(&"croak".to_string()),
        "Carp should export 'croak'"
    );
}

#[test]
fn test_find_exporters() {
    let idx = ModuleIndex::new_for_test();

    let foobar_src = "package Foo::Bar;\nour @EXPORT = qw(alpha);\nour @EXPORT_OK = qw(beta);\nsub alpha {}\nsub beta {}\n1;";
    idx.insert_cache(
        "Foo::Bar",
        Some(parse_source_to_cached(foobar_src, "Foo::Bar")),
    );

    let bazqux_src =
        "package Baz::Qux;\nour @EXPORT_OK = qw(beta gamma);\nsub beta {}\nsub gamma {}\n1;";
    idx.insert_cache(
        "Baz::Qux",
        Some(parse_source_to_cached(bazqux_src, "Baz::Qux")),
    );

    assert_eq!(idx.find_exporters("alpha"), vec!["Foo::Bar"]);
    assert_eq!(idx.find_exporters("beta"), vec!["Baz::Qux", "Foo::Bar"]);
    assert!(idx.find_exporters("nonexistent").is_empty());
}

#[test]
fn test_find_exporters_uses_reverse_index() {
    let idx = ModuleIndex::new_for_test();
    let src = "package My::Mod;\nour @EXPORT = qw(foo);\nour @EXPORT_OK = qw(bar);\nsub foo {}\nsub bar {}\n1;";
    idx.insert_cache("My::Mod", Some(parse_source_to_cached(src, "My::Mod")));

    assert!(!idx.modules_with_symbol("foo").is_empty());
    assert!(!idx.modules_with_symbol("bar").is_empty());
    assert_eq!(idx.find_exporters("foo"), vec!["My::Mod"]);
    assert_eq!(idx.find_exporters("bar"), vec!["My::Mod"]);
}

#[test]
fn test_rebuild_reverse_index_recovers_warm_path_exporters() {
    // The warm path (`warm_cache`) writes straight into `cache_raw()` and
    // never touches the reverse index, so `find_exporters` is blind until a
    // rebuild. The export-only name (`weaken`-style XS export with no Perl
    // body, hence no `symbols` entry) is the case `indexable_symbol_names`
    // alone misses — the B6 cold/warm attribution regression.
    let idx = ModuleIndex::new_for_test();
    let src = "package Scalar::Util;\nour @EXPORT_OK = qw(weaken blessed);\n1;";
    let cached = parse_source_to_cached(src, "Scalar::Util");

    // Simulate warm_cache: direct insert, no reverse-index update.
    idx.cache_raw().insert("Scalar::Util".to_string(), Some(cached));
    assert!(
        idx.find_exporters("weaken").is_empty(),
        "warm insert must not populate the reverse index on its own"
    );

    idx.rebuild_reverse_index_from_cache();
    assert_eq!(idx.find_exporters("weaken"), vec!["Scalar::Util"]);
    assert_eq!(idx.find_exporters("blessed"), vec!["Scalar::Util"]);
}

#[test]
fn test_find_exporters_exporter_extensible() {
    // Names declared via `export(...)` and `:Export` attributes are
    // discoverable cross-file — the goto-def proxy for a consumer's import.
    let idx = ModuleIndex::new_for_test();
    let src = "package My::Ext;\nuse Exporter::Extensible -exporter_setup => 1;\nexport(qw( foo $bar -tag ));\nsub foo {}\nsub bar :Export {}\n1;";
    idx.insert_cache("My::Ext", Some(parse_source_to_cached(src, "My::Ext")));
    assert_eq!(idx.find_exporters("foo"), vec!["My::Ext"]);
    assert_eq!(idx.find_exporters("bar"), vec!["My::Ext"]);
    // Sigil'd / tag entries aren't subs — not advertised.
    assert!(idx.find_exporters("$bar").is_empty());
    assert!(idx.find_exporters("-tag").is_empty());
}

#[test]
fn test_find_exporters_exporter_declare() {
    let idx = ModuleIndex::new_for_test();
    let src = "package My::Decl;\nuse Exporter::Declare;\ndefault_export foo => sub { 1 };\nexport bar => sub { 2 };\nexports qw/a b/;\nsub bar {}\n1;";
    idx.insert_cache("My::Decl", Some(parse_source_to_cached(src, "My::Decl")));
    assert_eq!(idx.find_exporters("foo"), vec!["My::Decl"]);
    assert_eq!(idx.find_exporters("bar"), vec!["My::Decl"]);
    assert_eq!(idx.find_exporters("a"), vec!["My::Decl"]);
}

#[test]
fn test_find_exporters_importer_menu() {
    let idx = ModuleIndex::new_for_test();
    let src = "package My::Menu;\nsub IMPORTER_MENU {\n  return ( export => [qw/foo bar/], export_ok => ['baz'] );\n}\nsub foo {}\n1;";
    idx.insert_cache("My::Menu", Some(parse_source_to_cached(src, "My::Menu")));
    assert_eq!(idx.find_exporters("foo"), vec!["My::Menu"]);
    assert_eq!(idx.find_exporters("baz"), vec!["My::Menu"]);
}

#[test]
fn test_get_return_type_cached() {
    use crate::file_analysis::InferredType;

    let idx = ModuleIndex::new_for_test();

    // Source with two exported subs with clear return types.
    let src = r#"
package Config::DB;
our @EXPORT_OK = qw(get_config make_obj);

sub get_config {
    return { host => 'localhost', port => 5432 };
}

sub make_obj {
    return MyClass->new;
}

1;
"#;
    idx.insert_cache(
        "Config::DB",
        Some(parse_source_to_cached(src, "Config::DB")),
    );

    assert!(
        idx.get_return_type_cached("get_config").is_some_and(|t| t.is_hash_shaped()),
        "hash-shaped",
    );
    assert_eq!(
        idx.get_return_type_cached("make_obj"),
        Some(InferredType::ClassName("MyClass".into()))
    );
    assert_eq!(idx.get_return_type_cached("nonexistent"), None);
}

#[test]
fn runtime_exporter_names_resolve_as_exporters() {
    // A package whose exports come from a runtime exporter setup
    // (Sub::Exporter / Moose::Exporter / Type::Library) must be found
    // by `find_exporters` so consumer goto-def / diagnostics resolve.
    let idx = ModuleIndex::new_for_test();

    let sub_exp = "package Sugar::Sub;\n\
        use Sub::Exporter -setup => { exports => [qw/sweeten/] };\n\
        sub sweeten { }\n1;";
    idx.insert_cache("Sugar::Sub", Some(parse_source_to_cached(sub_exp, "Sugar::Sub")));

    let moose_exp = "package Sugar::Moose;\n\
        use Moose::Exporter;\n\
        Moose::Exporter->setup_import_methods(as_is => [qw/has_column/]);\n\
        sub has_column { }\n1;";
    idx.insert_cache("Sugar::Moose", Some(parse_source_to_cached(moose_exp, "Sugar::Moose")));

    let type_lib = "package My::Types;\n\
        use Type::Library -base;\n\
        __PACKAGE__->add_type({ name => 'PositiveInt' });\n\
        sub PositiveInt { }\n1;";
    idx.insert_cache("My::Types", Some(parse_source_to_cached(type_lib, "My::Types")));

    assert_eq!(idx.find_exporters("sweeten"), vec!["Sugar::Sub"]);
    assert_eq!(idx.find_exporters("has_column"), vec!["Sugar::Moose"]);
    assert_eq!(idx.find_exporters("PositiveInt"), vec!["My::Types"]);
}

#[test]
fn test_children_index_direct_and_transitive() {
    let idx = ModuleIndex::new_for_test();

    let role = "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n1;";
    idx.insert_cache("My::Role", Some(parse_source_to_cached(role, "My::Role")));

    // Direct composer.
    let composer = "package My::Composer;\nuse Moo;\nwith 'My::Role';\nsub fetch { }\n1;";
    idx.insert_cache("My::Composer", Some(parse_source_to_cached(composer, "My::Composer")));

    // Role-composing-role, then a composer of THAT role — the
    // transitive hop the descendant walk must reach.
    let subrole = "package My::SubRole;\nuse Moo::Role;\nwith 'My::Role';\n1;";
    idx.insert_cache("My::SubRole", Some(parse_source_to_cached(subrole, "My::SubRole")));
    let deep = "package My::Deep;\nuse Moo;\nwith 'My::SubRole';\nsub fetch { }\n1;";
    idx.insert_cache("My::Deep", Some(parse_source_to_cached(deep, "My::Deep")));

    assert_eq!(
        idx.modules_with_parent("My::Role"),
        vec!["My::Composer", "My::SubRole"],
        "direct children only",
    );

    let mut packages: Vec<String> = Vec::new();
    idx.for_each_descendant_package("My::Role", |pkg, _cached| {
        packages.push(pkg.to_string());
        std::ops::ControlFlow::Continue(())
    });
    packages.sort();
    assert_eq!(
        packages,
        vec!["My::Composer", "My::Deep", "My::SubRole"],
        "descendant walk crosses the role-composing-role hop",
    );
}

#[test]
fn test_children_index_survives_warm_rebuild_and_purge() {
    // B6: the children edge must be fed by the warm rebuild path, not
    // just the insert path — and purged on re-registration.
    let idx = ModuleIndex::new_for_test();
    let child = "package Kid;\nuse parent 'Base::Class';\n1;";
    let cached = parse_source_to_cached(child, "Kid");

    // Simulate warm_cache: direct insert, indexes untouched.
    idx.cache_raw().insert("Kid".to_string(), Some(cached));
    assert!(idx.modules_with_parent("Base::Class").is_empty());

    idx.rebuild_reverse_index_from_cache();
    assert_eq!(idx.modules_with_parent("Base::Class"), vec!["Kid"]);

    // Re-registration with the parent edge gone must drop the stale edge.
    let orphaned = "package Kid;\nsub solo { }\n1;";
    let mut parser = crate::builder::create_parser();
    let tree = parser.parse(orphaned, None).unwrap();
    let analysis = Arc::new(crate::builder::build(&tree, orphaned.as_bytes()));
    idx.register_workspace_module(PathBuf::from("/fake/Kid.pm"), analysis);
    assert!(
        idx.modules_with_parent("Base::Class").is_empty(),
        "purge on re-registration must drop the stale parent edge",
    );
}

/// Include-closure visibility: two files each declare `class Box`. A scoped
/// lookup resolves to the `Box` the querying file can SEE (its include set),
/// not the global path-order winner — and falls back to that winner when NONE
/// is reachable (no legit indirect resolution is dropped). `ScopedLookup` /
/// `docs/adr/macro-handling.md`, "the include-closure lie".
#[cfg(feature = "cpp")]
#[test]
fn get_cached_scoped_prefers_reachable_same_name_class() {
    use std::collections::HashSet;
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let a = Arc::new(driver.analyze_with_path(
        "class Box { public: void a_only(); };\n",
        Some(std::path::Path::new("/fake/a.cpp")),
    ));
    let b = Arc::new(driver.analyze_with_path(
        "class Box { public: void b_only(); };\n",
        Some(std::path::Path::new("/fake/b.cpp")),
    ));
    let idx = ModuleIndex::new_for_test();
    idx.register_symbols(PathBuf::from("/fake/a.cpp"), a);
    idx.register_symbols(PathBuf::from("/fake/b.cpp"), b);

    let has = |m: &Option<Arc<CachedModule>>, name: &str| {
        m.as_ref()
            .is_some_and(|c| c.analysis.symbols.iter().any(|s| s.name == name))
    };
    let scope = |p: &str| -> HashSet<String> { [p.to_string()].into_iter().collect() };

    // Empty scope = global winner: smallest canonical path (a.cpp).
    assert!(has(&idx.get_cached_scoped("Box", &HashSet::new()), "a_only"));
    // Scoped to each file → THAT file's Box.
    assert!(has(&idx.get_cached_scoped("Box", &scope("/fake/b.cpp")), "b_only"));
    assert!(has(&idx.get_cached_scoped("Box", &scope("/fake/a.cpp")), "a_only"));
    // Scope names an unrelated file → nothing reachable → global winner (a.cpp).
    assert!(has(&idx.get_cached_scoped("Box", &scope("/fake/other.cpp")), "a_only"));
}

/// Completion GATHERING over the include closure: only names with a candidate
/// inside the visibility set are enumerated — unlike resolution there is NO
/// global fallback, so a file that doesn't include a header never gets its
/// names offered. Deterministic (sorted by name).
#[cfg(feature = "cpp")]
#[test]
fn visible_defs_with_prefix_gates_on_closure() {
    use std::collections::HashSet;
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let header = Arc::new(driver.analyze_with_path(
        "enum opcode { OP_NULL, OP_SCOPE };\nint op_name(int t);\n",
        Some(std::path::Path::new("/fake/opcodes.h")),
    ));
    let other = Arc::new(driver.analyze_with_path(
        "int OP_ELSEWHERE = 1;\n",
        Some(std::path::Path::new("/fake/other.h")),
    ));
    let idx = ModuleIndex::new_for_test();
    idx.register_symbols(PathBuf::from("/fake/opcodes.h"), header);
    idx.register_symbols(PathBuf::from("/fake/other.h"), other);

    let closure: HashSet<String> = ["/fake/opcodes.h".to_string()].into_iter().collect();
    let names: Vec<String> = idx
        .visible_defs_with_prefix("OP_", &closure)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    // Sorted, closure-gated: OP_ELSEWHERE (other.h, unreachable) excluded.
    assert_eq!(names, vec!["OP_NULL", "OP_SCOPE"]);
    let funcs: Vec<String> = idx
        .visible_defs_with_prefix("op_", &closure)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(funcs, vec!["op_name"]);
    // Empty closure ⇒ nothing (no global fallback for gathering).
    assert!(idx.visible_defs_with_prefix("OP_", &HashSet::new()).is_empty());
}

/// The in-session inverse of `register_symbols` (H1): unregistering a file
/// removes its `all_defs` candidates and re-picks the global cache winner
/// among the survivors with the SAME total order registration uses.
#[cfg(feature = "cpp")]
#[test]
fn unregister_file_removes_defs_and_repicks_winner() {
    use std::collections::HashSet;
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let a = Arc::new(driver.analyze_with_path(
        "class Box { public: void a_only(); };\n",
        Some(std::path::Path::new("/fake/a.cpp")),
    ));
    let b = Arc::new(driver.analyze_with_path(
        "class Box { public: void b_only(); };\n",
        Some(std::path::Path::new("/fake/b.cpp")),
    ));
    let idx = ModuleIndex::new_for_test();
    idx.register_symbols(PathBuf::from("/fake/a.cpp"), a);
    idx.register_symbols(PathBuf::from("/fake/b.cpp"), b);

    let has = |m: &Option<Arc<CachedModule>>, name: &str| {
        m.as_ref()
            .is_some_and(|c| c.analysis.symbols.iter().any(|s| s.name == name))
    };
    // a.cpp holds the winner slot (smallest path).
    assert!(has(&idx.get_cached("Box"), "a_only"));

    idx.unregister_file(std::path::Path::new("/fake/a.cpp"));
    // The slot re-picks the surviving candidate...
    assert!(has(&idx.get_cached("Box"), "b_only"));
    // ...and the departed candidate is gone from the scoped view too.
    let scope: HashSet<String> = ["/fake/a.cpp".to_string()].into_iter().collect();
    assert!(has(&idx.get_cached_scoped("Box", &scope), "b_only"));

    idx.unregister_file(std::path::Path::new("/fake/b.cpp"));
    assert!(idx.get_cached("Box").is_none(), "no survivors: slot removed");
}

/// H9-1 source-generation guard: a claim succeeds iff its generation is ≥ the
/// one already registered, so a stale re-analysis (built from pre-save bytes →
/// a lower generation) can never revert a fresher registration, while a
/// serialized fresh re-registration (an equal-generation reconcile) still lands.
#[test]
fn claim_source_gen_orders_by_generation() {
    let idx = ModuleIndex::new_for_test();
    let p = std::path::Path::new("/fake/gen.cpp");
    // First claim wins from the baseline.
    assert!(idx.claim_source_gen(p, 5));
    // Strictly-older is REJECTED (the stale-winner race).
    assert!(!idx.claim_source_gen(p, 3));
    // Equal generation ties succeed (the reconcile running after the bulk pass).
    assert!(idx.claim_source_gen(p, 5));
    // Newer wins and advances the watermark.
    assert!(idx.claim_source_gen(p, 9));
    assert!(!idx.claim_source_gen(p, 8));
    // A different path is independent.
    let q = std::path::Path::new("/fake/other.cpp");
    assert!(idx.claim_source_gen(q, 1));
    assert!(idx.claim_source_gen(p, 10));
    // Forget resets to the baseline — a recreated file claims cleanly.
    idx.forget_source_gen(p);
    assert!(idx.claim_source_gen(p, 1));
}

/// The guard applied at the `pack_file_changed` swap: a re-analysis whose event
/// generation is OLDER than the one registered leaves the fresher registration
/// untouched (H9-1). Simulated by pre-claiming the max generation, so the real
/// (mtime-derived) event generation of the edit loses.
#[cfg(feature = "cpp")]
#[test]
fn pack_swap_skips_stale_generation() {
    let dir = std::env::temp_dir().join(format!("pack-gen-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hdr = dir.join("box.h");
    let tu = dir.join("use.cpp");
    std::fs::write(&hdr, "class Box { public: int width() { return 1; } };\n").unwrap();
    std::fs::write(
        &tu,
        "#include \"box.h\"\nint f() { Box b; return b.width(); }\n",
    )
    .unwrap();

    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let hub = ModuleIndex::new_for_test();
    let pack = Arc::new(ModuleIndex::new_for_test());
    hub.attach_pack_index("cpp", pack.clone());
    for p in [&hdr, &tu] {
        let src = std::fs::read_to_string(p).unwrap();
        pack.register_symbols(p.clone(), Arc::new(driver.analyze_with_path(&src, Some(p))));
    }
    let canon_hdr = std::fs::canonicalize(&hdr).unwrap();
    let canon_tu = std::fs::canonicalize(&tu).unwrap();
    let arc_of = |path: &std::path::Path| {
        let mut found = None;
        pack.for_each_registered_file(&mut |cm| {
            if cm.path == path {
                found = Some(Arc::as_ptr(&cm.analysis) as usize);
            }
        });
        found.expect("registered")
    };
    let hdr_before = arc_of(&canon_hdr);
    let tu_before = arc_of(&canon_tu);

    // A fresher writer already claimed the maximum generation for both paths.
    assert!(pack.claim_source_gen(&canon_hdr, i64::MAX));
    assert!(pack.claim_source_gen(&canon_tu, i64::MAX));

    // A cross-file-visible edit whose event generation (mtime) is < MAX must be
    // rejected at the swap — the stale re-analysis loses to nothing.
    std::fs::write(
        &hdr,
        "class Box { public: int width() { return 2; } int height() { return 3; } };\n",
    )
    .unwrap();
    crate::module_resolver::pack_file_changed(None, &hub, &hdr, false);
    assert_eq!(arc_of(&canon_hdr), hdr_before, "stale header re-register skipped");
    assert_eq!(arc_of(&canon_tu), tu_before, "stale consumer re-register skipped");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A changed file re-registers via unregister-then-register (the
/// `pack_file_changed` swap): names its new version no longer defines
/// must not linger in any view.
#[cfg(feature = "cpp")]
#[test]
fn edit_swap_drops_names_the_new_version_lost() {
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let v1 = Arc::new(driver.analyze_with_path(
        "class Box { public: int w; };\nint helper();\n",
        Some(std::path::Path::new("/fake/edit.h")),
    ));
    let idx = ModuleIndex::new_for_test();
    idx.register_symbols(PathBuf::from("/fake/edit.h"), v1);
    assert!(idx.get_cached("Box").is_some());
    assert!(idx.get_cached("helper").is_some());

    let v2 = Arc::new(driver.analyze_with_path(
        "class Crate { public: int w; };\n",
        Some(std::path::Path::new("/fake/edit.h")),
    ));
    idx.unregister_file(std::path::Path::new("/fake/edit.h"));
    idx.register_symbols(PathBuf::from("/fake/edit.h"), v2);
    assert!(idx.get_cached("Box").is_none(), "dropped class gone");
    assert!(idx.get_cached("helper").is_none(), "dropped function gone");
    assert!(idx.get_cached("Crate").is_some(), "new class registered");
}

/// Registration-owned strip: the name/edge feeds and the class-rank record
/// are extracted from the WHOLE analysis before `symbols` evicts, so
/// lookups, tie-breaks, and the unregister inverse all survive a
/// symbol-stripped resident copy.
#[test]
fn register_symbols_stripping_feeds_before_evict() {
    use crate::pack_bag_cache::PackBagCache;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let full_syms = full.analysis.symbols.len();
    assert!(full_syms > 0);
    let path = full.path.clone();

    let idx = ModuleIndex::new_for_cli();
    let arc = idx.register_symbols_stripping((*path).to_path_buf(), (*full.analysis).clone(), true, true);
    assert!(arc.symbols_are_evicted() && arc.symbols.is_empty(), "stored copy is stripped");
    assert!(arc.refs_are_evicted());

    // Name lookups still resolve — the feed ran on the whole copy. (`make`
    // is a Sub — C-linkage-visible; a Perl Package symbol is not part of
    // the pack feed, so the sub name is the right probe here.)
    let hit = idx.get_cached("make").expect("sub name registered pre-strip");
    assert_eq!(hit.path, path);
    assert!(!idx.def_candidates("make").is_empty(), "candidate table fed pre-strip");

    // whole_present rehydrates symbols through the LRU.
    let full_for_loader = full.analysis.clone();
    let cache = std::sync::Arc::new(PackBagCache::new(1024 * 1024, move |_p| {
        Ok((*full_for_loader).clone())
    }));
    let idx2 = ModuleIndex::new_for_cli().with_bag_cache(cache);
    let whole = idx2.whole_present(&hit);
    assert!(!whole.symbols_are_evicted());
    assert_eq!(whole.symbols.len(), full_syms);

    // The unregister inverse walks the recorded names, not the evicted vec.
    idx.unregister_file(&path);
    assert!(idx.get_cached("make").is_none(), "cache slot removed");
    assert!(idx.def_candidates("make").is_empty(), "candidates removed");
}

/// The enrichment overlay (R4): a snapshot is cached while its
/// fingerprint key stands (same Arc back), recomputes when a PROVIDER's
/// surface changes, and never mutates the shared workspace Arc.
#[test]
fn enriched_snapshot_caches_and_invalidates_on_provider_change() {
    let idx = ModuleIndex::new_for_test();
    let lib_v1 = parse_source_to_cached(
        "package Lib;\nour @EXPORT_OK = ('make');\nsub make { my %h = (id => 1); return \\%h }\n1;\n",
        "Lib",
    );
    let consumer = parse_source_to_cached(
        "package App;\nuse Lib 'make';\nsub go { my $x = make(); return $x }\n1;\n",
        "App",
    );
    idx.register_workspace_module(
        lib_v1.path.to_path_buf(),
        Arc::clone(&lib_v1.analysis),
    );
    idx.register_workspace_module(
        consumer.path.to_path_buf(),
        Arc::clone(&consumer.analysis),
    );

    let shared_witnesses_before = consumer.analysis.witnesses.len();
    let snap1 = idx.enriched_snapshot(&consumer).expect("snapshot");
    let snap2 = idx.enriched_snapshot(&consumer).expect("snapshot");
    assert!(
        Arc::ptr_eq(&snap1, &snap2),
        "key unchanged: the cached snapshot is returned, not recomputed"
    );
    assert_eq!(
        consumer.analysis.witnesses.len(),
        shared_witnesses_before,
        "the shared workspace Arc is never enriched in place"
    );

    // Provider contract change → the consumer's key moves → recompute.
    let lib_v2 = parse_source_to_cached(
        "package Lib;\nour @EXPORT_OK = ('make', 'other');\nsub make { my %h = (id => 1); return \\%h }\nsub other { return 2 }\n1;\n",
        "Lib",
    );
    idx.register_workspace_module(
        lib_v2.path.to_path_buf(),
        Arc::clone(&lib_v2.analysis),
    );
    let snap3 = idx.enriched_snapshot(&consumer).expect("snapshot");
    assert!(
        !Arc::ptr_eq(&snap1, &snap3),
        "provider surface changed: the stale snapshot must not be served"
    );

    // BODY edit to the consumer itself: the surface fingerprint stands,
    // but the rebuilt analysis is a NEW Arc — the snapshot must derive
    // from it (spans/refs moved even though the contract didn't).
    let consumer_v2 = parse_source_to_cached(
        "package App;\nuse Lib 'make';\nsub go { my $x = make();\n    return $x }\n1;\n",
        "App",
    );
    idx.register_workspace_module(
        consumer_v2.path.to_path_buf(),
        Arc::clone(&consumer_v2.analysis),
    );
    let snap4 = idx.enriched_snapshot(&consumer_v2).expect("snapshot");
    assert!(
        !Arc::ptr_eq(&snap3, &snap4),
        "body edit: the snapshot must derive from the rebuilt analysis"
    );
}

/// @INC providers carry a real registration generation (minted at
/// insert/warm), so a re-resolve moves the consumer's enrichment key even
/// though the provider is a RECORDLESS cache entry (no surface fingerprint —
/// the `None` arm that used to fall back to the Arc pointer). `insert_cache`
/// is the CLI/@INC insertion door; each Some insert mints a fresh gen.
#[test]
fn inc_provider_reresolve_moves_enrichment_key() {
    let idx = ModuleIndex::new_for_test();
    // Consumer imports from an @INC-tier provider inserted name-keyed (no
    // surface record → the recordless enrichment-key arm).
    let consumer = parse_source_to_cached(
        "package App;\nuse Ext::Lib 'make';\nsub go { my $x = make(); return $x }\n1;\n",
        "App",
    );
    idx.register_workspace_module(consumer.path.to_path_buf(), Arc::clone(&consumer.analysis));

    let prov_v1 = parse_source_to_cached(
        "package Ext::Lib;\nour @EXPORT_OK = ('make');\nsub make { return bless {}, 'W1' }\n1;\n",
        "Ext::Lib",
    );
    idx.insert_cache("Ext::Lib", Some(prov_v1));

    let snap1 = idx.enriched_snapshot(&consumer).expect("snapshot v1");
    let snap1b = idx.enriched_snapshot(&consumer).expect("snapshot v1 cached");
    assert!(
        Arc::ptr_eq(&snap1, &snap1b),
        "key stable across queries: the cached snapshot is returned"
    );

    // Re-resolve the provider (content changed): a new generation must move
    // the consumer's key even for a recordless @INC entry.
    let prov_v2 = parse_source_to_cached(
        "package Ext::Lib;\nour @EXPORT_OK = ('make', 'other');\nsub make { return bless {}, 'W2' }\nsub other { return 2 }\n1;\n",
        "Ext::Lib",
    );
    idx.insert_cache("Ext::Lib", Some(prov_v2));

    let snap2 = idx.enriched_snapshot(&consumer).expect("snapshot v2");
    assert!(
        !Arc::ptr_eq(&snap1, &snap2),
        "@INC provider re-resolve (gen bump) must move the enrichment key"
    );
}

/// The R4 always-enriched seams: a closed dep whose answer chains through
/// ITS OWN imports is a raw-bag dead end (the walker pins no edge for
/// imported calls); the fallback-on-miss retry through the enrichment
/// overlay fills it. Two seams, one fixture: B::make's return binds
/// through B's import of C.
#[test]
fn closed_dep_return_type_resolves_through_enriched_overlay() {
    // C: exports thing() with a concrete blessed return.
    let c = parse_source_to_cached(
        "package C;\nour @EXPORT_OK = ('thing');\nsub thing { return bless {}, 'Widget' }\n1;\n",
        "C",
    );
    // B: make()'s return type exists only through B's OWN import of C.
    let b = parse_source_to_cached(
        "package B;\nuse C 'thing';\nour @EXPORT_OK = ('make');\nsub make { my $x = thing(); return $x }\n1;\n",
        "B",
    );
    // A: the querying consumer, imports make from B.
    let a = parse_source_to_cached(
        "package A;\nuse B 'make';\nsub go { my $m = make(); return $m }\n1;\n",
        "A",
    );

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(c.path.to_path_buf(), Arc::clone(&c.analysis));
    idx.register_workspace_module(b.path.to_path_buf(), Arc::clone(&b.analysis));
    idx.register_workspace_module(a.path.to_path_buf(), Arc::clone(&a.analysis));

    // Precondition: B's RAW bag alone can't answer (else the seam proves
    // nothing) — thing() is imported, no local edge.
    assert_eq!(
        b.analysis.sub_return_type_at_arity("make", None),
        None,
        "fixture must dead-end without the index (raw bag)"
    );

    // Seam 1: the imported-sub recursion from A dead-ends on B's raw bag,
    // retries through the enrichment overlay, and resolves Widget.
    let t = a
        .analysis
        .sub_return_type_at_arity_ctx("make", None, Some(&idx))
        .expect("enriched overlay must fill the closed-dep chain");
    assert_eq!(
        t.class_name().as_deref(),
        Some("Widget"),
        "resolved through B's OWN import of C: {t:?}"
    );

    // Seam 2: the MethodOnClass cross-file primary — B::make as a method
    // call target.
    let t2 = a
        .analysis
        .find_method_return_type("B", "make", Some(&idx), None)
        .expect("MethodOnClass chase must fill through the overlay");
    assert_eq!(t2.class_name().as_deref(), Some("Widget"));
}

/// Mutual imports terminate: the thread-local cycle guard declines the
/// re-entrant enrich, the tainted build is remembered as a DECLINE (never
/// cached as a degraded copy, never rebuilt per query), and the acyclic
/// leg still resolves through the raw bag.
#[test]
fn enrichment_cycle_terminates_and_declines_deterministically() {
    let a = parse_source_to_cached(
        "package CycA;\nuse CycB 'bfn';\nour @EXPORT_OK = ('afn');\nsub afn { return bless {}, 'AObj' }\n1;\n",
        "CycA",
    );
    let b = parse_source_to_cached(
        "package CycB;\nuse CycA 'afn';\nour @EXPORT_OK = ('bfn');\nsub bfn { my $y = afn(); return $y }\n1;\n",
        "CycB",
    );
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(a.path.to_path_buf(), Arc::clone(&a.analysis));
    idx.register_workspace_module(b.path.to_path_buf(), Arc::clone(&b.analysis));

    // Terminates (no hang / overflow); B's enrichment resolves afn through
    // A's RAW bag (afn's return is local to A — no cycle needed for it).
    let snap = idx.enriched_snapshot(&b);
    assert!(snap.is_some(), "acyclic leg enriches");
    // And a consumer's query through the seam answers AObj.
    let consumer = parse_source_to_cached(
        "package Cons;\nuse CycB 'bfn';\nsub go { my $r = bfn(); return $r }\n1;\n",
        "Cons",
    );
    let t = consumer
        .analysis
        .sub_return_type_at_arity_ctx("bfn", None, Some(&idx))
        .expect("cycle must not block the resolvable leg");
    assert_eq!(t.class_name().as_deref(), Some("AObj"));
}

/// The trait DEFAULT stays unenriched-but-correct: an overlay-less impl
/// answers the raw bag view, so the seams' fallback is a no-op there.
#[test]
fn enriched_present_default_is_the_raw_bag() {
    struct Bare(Arc<CachedModule>);
    impl crate::file_analysis::CrossFileLookup for Bare {
        fn get_cached(&self, _m: &str) -> Option<Arc<CachedModule>> {
            Some(Arc::clone(&self.0))
        }
        fn parents_cached(&self, _m: &str) -> Vec<String> {
            Vec::new()
        }
        fn modules_with_symbol(&self, _n: &str) -> Vec<String> {
            Vec::new()
        }
        fn find_exporters(&self, _n: &str) -> Vec<String> {
            Vec::new()
        }
        fn defining_module_cached(
            &self,
            _entry: &str,
            _name: &str,
        ) -> Option<Arc<CachedModule>> {
            None
        }
        fn module_declaring_method_in_package(
            &self,
            _p: &str,
            _m: &str,
        ) -> Option<String> {
            None
        }
        fn for_each_cached(&self, _f: &mut dyn FnMut(&str, &Arc<CachedModule>)) {}
        fn for_each_reexport_module(
            &self,
            _start: Vec<String>,
            _visit: &mut dyn FnMut(&Arc<CachedModule>) -> std::ops::ControlFlow<()>,
        ) {
        }
        fn for_each_entity_bridged_to(
            &self,
            _c: &str,
            _f: &mut dyn FnMut(&str, &Arc<CachedModule>, &crate::file_analysis::Symbol),
        ) {
        }
        fn direct_children_of(&self, _p: &str) -> Vec<(String, String)> {
            Vec::new()
        }
        fn for_each_loader_shape(
            &self,
            _f: &mut dyn FnMut(&str, &crate::file_analysis::InferredType),
        ) {
        }
    }
    let cm = parse_source_to_cached("package P;\nsub f { return 1 }\n1;\n", "P");
    let lk = Bare(Arc::clone(&cm));
    let e = crate::file_analysis::CrossFileLookup::enriched_present(&lk, &cm);
    let b = crate::file_analysis::CrossFileLookup::bag_present(&lk, &cm);
    assert!(Arc::ptr_eq(&e, &b), "default enriched view IS the raw bag view");
}

/// Build a `FileAnalysis` and cache it under `module_name` with a plugin
/// namespace bridging to `bridge_class`, its entity being the named sub.
/// Exercises the bridged-entity hop of the `MethodOnClass` fallback.
fn cache_bridged(
    idx: &ModuleIndex,
    module_name: &str,
    source: &str,
    entity_sub: &str,
    bridge_class: &str,
) {
    let mut parser = crate::builder::create_parser();
    let tree = parser.parse(source, None).unwrap();
    let mut fa = crate::builder::build(&tree, source.as_bytes());
    let entity_id = fa
        .symbols
        .iter()
        .find(|s| s.name == entity_sub)
        .map(|s| s.id)
        .expect("entity sub must exist");
    fa.plugin_namespaces.push(crate::file_analysis::PluginNamespace {
        id: format!("test:{module_name}"),
        plugin_id: "test".into(),
        kind: "emitter".into(),
        entities: vec![entity_id],
        bridges: vec![crate::file_analysis::Bridge::Class(bridge_class.into())],
        decl_span: crate::file_analysis::Span {
            start: tree_sitter::Point { row: 0, column: 0 },
            end: tree_sitter::Point { row: 0, column: 0 },
        },
    });
    idx.insert_cache(
        module_name,
        Some(Arc::new(CachedModule::new(
            PathBuf::from(format!("/fake/{}.pm", module_name.replace("::", "/"))),
            Arc::new(fa),
        ))),
    );
}

/// Seam (a): a plugin-bridged entity whose return type materializes ONLY
/// after the bridging file is itself enriched. The `MethodOnClass` bridged
/// hop queries the bridging file's RAW bag first (dead-ends — `render`'s
/// value chains through the bridging file's import of C), then falls back to
/// the enriched overlay copy, which resolves it.
#[test]
fn bridged_entity_return_resolves_through_enriched_overlay() {
    let idx = ModuleIndex::new_for_test();
    // C exports thing() → Widget.
    let c = parse_source_to_cached(
        "package C;\nour @EXPORT_OK = ('thing');\nsub thing { return bless {}, 'Widget' }\n1;\n",
        "C",
    );
    idx.register_workspace_module(c.path.to_path_buf(), Arc::clone(&c.analysis));
    // Br bridges its `render` entity to class `Painter`; render's return type
    // exists only through Br's OWN import of C.
    let br_src = "package Br;\nuse C 'thing';\nsub render { my $x = thing(); return $x }\n1;\n";
    cache_bridged(&idx, "Br", br_src, "render", "Painter");

    // Precondition: Br's RAW bag alone can't type render (thing() imported,
    // no local edge) — else the enriched fallback proves nothing.
    let br = idx.get_cached("Br").expect("Br cached");
    let render_id = br
        .analysis
        .symbols
        .iter()
        .find(|s| s.name == "render")
        .map(|s| s.id)
        .unwrap();
    assert_eq!(
        br.analysis.symbol_return_type_via_bag(render_id, None),
        None,
        "fixture must dead-end on the raw bag"
    );

    // MethodOnClass{Painter, render}: hop (1)/(2) find nothing (no Painter
    // module, no parents); the bridged hop (3) retries through the enriched
    // overlay and resolves Widget.
    let t = idx_find_method_return(&idx, "Painter", "render");
    assert_eq!(
        t.as_ref().and_then(|t| t.class_name()).as_deref(),
        Some("Widget"),
        "bridged entity resolved through Br's enriched overlay: {t:?}"
    );
}

/// Route a `MethodOnClass{class, name}` query from a throwaway consumer FA
/// through the index (the bridged hop needs a `BagContext` with the index).
fn idx_find_method_return(
    idx: &ModuleIndex,
    class: &str,
    method: &str,
) -> Option<InferredType> {
    let consumer = parse_source_to_cached("package Q;\nsub noop { 1 }\n1;\n", "Q");
    consumer
        .analysis
        .find_method_return_type(class, method, Some(idx), None)
}

/// Seam (b), primary (hop 1): the cross-file `SlotType{class, key}` primary —
/// a typed slot WRITE in the class's OWN file, read from a consumer, resolves
/// through the extracted `attempt` closure. (The enriched-retry twin of this
/// hop is dormant today: SlotType seeds are build-gated on a resolvable RHS,
/// so a seed that exists already answers on the raw bag. The retry is wired
/// for symmetry with the MethodOnClass primary; this test guards the refactor
/// that extracted its `attempt` closure.)
#[test]
fn cross_file_slot_type_primary_resolves_hop1() {
    let idx = ModuleIndex::new_for_test();
    let store = parse_source_to_cached(
        "package Store;\nsub init {\n    my $self = shift;\n    $self->{conn} = Conn->new;\n}\n1;\n",
        "Store",
    );
    idx.register_workspace_module(store.path.to_path_buf(), Arc::clone(&store.analysis));

    let app_src =
        "package App;\nsub run {\n    my $s = Store->new;\n    my $c = $s->{conn};\n}\n1;\n";
    let mut parser = crate::builder::create_parser();
    let tree = parser.parse(app_src, None).unwrap();
    let fa = crate::builder::build(&tree, app_src.as_bytes());

    // `$c` rode `Edge(Expr($s->{conn}))`, which drilled `SlotType{Store, conn}`
    // into Store's own file (hop 1) and found the `Conn->new` slot write.
    let t = fa.inferred_type_via_bag_ctx(
        "$c",
        tree_sitter::Point { row: 4, column: 0 },
        Some(&idx),
    );
    assert_eq!(
        t.as_ref().and_then(|t| t.class_name()).as_deref(),
        Some("Conn"),
        "read narrows via Store's own slot write (cross-file SlotType primary): {t:?}"
    );
}

/// Seam (c): enrichment is TRANSITIVE through the overlay. A imports `make`
/// from B; B's `make` types only through B's import of C. Enriching A must
/// bake `make → Widget` into A's OWN bag — which requires A's import scan to
/// fall back to B's ENRICHED copy (B's raw bag dead-ends on the imported
/// `thing()`).
#[test]
fn enrichment_is_transitive_through_the_overlay() {
    let c = parse_source_to_cached(
        "package C;\nour @EXPORT_OK = ('thing');\nsub thing { return bless {}, 'Widget' }\n1;\n",
        "C",
    );
    let b = parse_source_to_cached(
        "package B;\nuse C 'thing';\nour @EXPORT_OK = ('make');\nsub make { my $x = thing(); return $x }\n1;\n",
        "B",
    );
    let a = parse_source_to_cached(
        "package A;\nuse B 'make';\nsub go { my $m = make(); return $m }\n1;\n",
        "A",
    );
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(c.path.to_path_buf(), Arc::clone(&c.analysis));
    idx.register_workspace_module(b.path.to_path_buf(), Arc::clone(&b.analysis));
    idx.register_workspace_module(a.path.to_path_buf(), Arc::clone(&a.analysis));

    // Precondition: A's RAW bag can't type go (make imported, no index).
    assert_eq!(a.analysis.sub_return_type_at_arity("go", None), None);

    // Enrich A through the overlay; its OWN bag must now answer go → Widget
    // WITHOUT an index, proving enrichment baked the A→B→C transitive type.
    let enriched_a = idx.enriched_snapshot(&a).expect("A enriches");
    let t = enriched_a
        .sub_return_type_at_arity("go", None)
        .expect("enrichment must bake make's C-derived return into A");
    assert_eq!(t.class_name().as_deref(), Some("Widget"), "{t:?}");
}

/// Seam (c), cycle: mutual imports whose exports type only through each
/// other exercise the ENRICHING re-entrant guard (enrichment's own import
/// scan is its first customer). The re-entrant enrich declines to the raw
/// bag, the cyclic build is tainted → answered as a DECLINE, and the tainted
/// copy is never cached — so a repeat query re-declines identically and the
/// call terminates (no hang / stack overflow).
#[test]
fn transitive_enrichment_mutual_import_terminates_without_poison() {
    let a = parse_source_to_cached(
        "package CycA;\nuse CycB 'bfn';\nour @EXPORT_OK = ('afn');\nsub afn { my $x = bfn(); return $x }\n1;\n",
        "CycA",
    );
    let b = parse_source_to_cached(
        "package CycB;\nuse CycA 'afn';\nour @EXPORT_OK = ('bfn');\nsub bfn { my $y = afn(); return $y }\n1;\n",
        "CycB",
    );
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(a.path.to_path_buf(), Arc::clone(&a.analysis));
    idx.register_workspace_module(b.path.to_path_buf(), Arc::clone(&b.analysis));

    // Terminates: the guard declines the re-entrant enrich, the tainted build
    // answers as a decline (None), never a cached degraded copy.
    let snap = idx.enriched_snapshot(&a);
    assert!(snap.is_none(), "mutually-cyclic enrich declines deterministically");
    // Deterministic + no poison: a second call re-declines identically.
    assert!(idx.enriched_snapshot(&a).is_none());
}

/// The rehydration-miss tripwire's observable: an evicted registered copy
/// with no rehydration source (no bag cache installed) is served as the
/// stripped resident AND counted — the "absence-as-answer" signature the
/// strict gate (`PERL_LSP_STRICT_RESIDENCY`) turns into a loud crash.
/// Strict mode is off in tests, so this exercises the counting arm.
#[test]
fn rehydration_miss_is_counted_and_serves_resident() {
    let idx = ModuleIndex::new_for_test();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let src = "package Ghost;\nsub boo { return 1 }\n1;\n";
    let tree = parser.parse(src, None).unwrap();
    let mut fa = crate::builder::build(&tree, src.as_bytes());
    fa.evict_axes(true, true);
    let cm = Arc::new(CachedModule::new(
        PathBuf::from("/fake/Ghost.pm"),
        Arc::new(fa),
    ));

    let before = crate::module_index::rehydration_miss_count();
    let served = crate::file_analysis::CrossFileLookup::bag_present(&idx, &cm);
    assert!(
        Arc::ptr_eq(&served, &cm.analysis),
        "miss degrades to the stripped resident copy, never fabricates"
    );
    assert!(
        crate::module_index::rehydration_miss_count() > before,
        "the miss must be counted — silent absence is the flake signature"
    );
}

/// The foreign-route half of rehydration: a sweep minting `CachedModule`s
/// from FileStore entries asks whatever index the query routed to — a cpp
/// query's workspace sweep hands PERL paths to the cpp sub-index, whose own
/// loader can never serve them (first caught live by the strict-residency
/// tripwire: cross-TU cpp references silently dropped every Perl workspace
/// file's matches). The sub-index must route a hub-owned path to the hub's
/// rehydration cell instead of degrading to the stripped resident.
#[test]
fn foreign_path_rehydrates_through_the_owning_sibling() {
    let hub = ModuleIndex::new_for_test();

    // A whole analysis the "hub blob store" can serve, and its stripped twin
    // the sweep would otherwise read empty.
    let src = "package Ghost;\nsub boo { return 1 }\n1;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let whole = crate::builder::build(&tree, src.as_bytes());
    let whole_arc = Arc::new(whole);
    let mut stripped = (*whole_arc).clone();
    stripped.evict_axes(true, true);
    let path = PathBuf::from("/fake/hub/Ghost.pm");

    // Install the hub's rehydration cell with a loader that serves the
    // whole copy (stands in for the modules.db blob load).
    let served = Arc::clone(&whole_arc);
    hub.set_bag_cache(Arc::new(crate::pack_bag_cache::PackBagCache::new(
        128 * 1024 * 1024,
        move |p: &std::path::Path| {
            if p == std::path::Path::new("/fake/hub/Ghost.pm") {
                Ok((*served).clone())
            } else {
                Err(crate::module_cache::RehydrateMiss::NoRow)
            }
        },
    )));

    // Attach a pack sub-index (its own bag cache can never serve the path).
    let sub = Arc::new(ModuleIndex::new_for_test());
    hub.attach_pack_index("cpp", Arc::clone(&sub));

    // The misrouted ask: the sub-index handed a hub-owned stripped copy.
    let cm = Arc::new(CachedModule::new(path, Arc::new(stripped)));
    let before = crate::module_index::rehydration_miss_count();
    let full = crate::file_analysis::CrossFileLookup::whole_present(sub.as_ref(), &cm);
    assert!(
        full.symbols.iter().any(|s| s.name == "boo"),
        "foreign route must serve the owner's WHOLE copy, not the stripped resident"
    );
    assert_eq!(
        crate::module_index::rehydration_miss_count(),
        before,
        "a foreign-routed answer is a hit, not a residency miss"
    );
}

/// Build a FileAnalysis from source (whole, never registered).
fn build_fa(src: &str) -> crate::file_analysis::FileAnalysis {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    crate::builder::build(&tree, src.as_bytes())
}

/// The buffer-vs-disk provenance rule (`SurfaceWrite`): while a doc is
/// open, consumers read its BUFFER, so the freshness baseline must track
/// the open-doc records — a background (disk) re-record must not replace
/// it. The trap this guards: buffer holds a contract change A', background
/// re-records disk state A over it, the user REVERTS the buffer to A — and
/// the revert must read CHANGED (consumers saw A'), not Unchanged against
/// the smuggled disk baseline.
#[test]
fn background_surface_write_yields_to_open_doc_record() {
    use crate::module_index::SurfaceWrite;
    use crate::surface::SurfaceVerdict;
    let idx = ModuleIndex::new_for_test();
    let path = PathBuf::from("/fake/prov/Widget.pm");
    let disk = build_fa("package Widget;\nsub base { 1 }\n1;\n");
    let buffer = build_fa("package Widget;\nsub base { 1 }\nsub extra { 2 }\n1;\n");

    // Indexer records the disk build, then the doc opens.
    assert_eq!(
        idx.record_and_dirty(&path, &disk, SurfaceWrite::Background).verdict,
        SurfaceVerdict::FirstSeen
    );
    idx.mark_doc_open(&path);

    // Buffer gains a contract change — consumers refreshed against A'.
    assert_eq!(
        idx.record_and_dirty(&path, &buffer, SurfaceWrite::OpenDoc).verdict,
        SurfaceVerdict::Changed
    );
    // A background tick re-records the DISK build: suppressed.
    assert_eq!(
        idx.record_and_dirty(&path, &disk, SurfaceWrite::Background).verdict,
        SurfaceVerdict::Unchanged,
        "background write on an open path must yield"
    );
    // The revert: buffer back to the disk state. Baseline must still be
    // the buffer record A' — this is CHANGED, the refresh consumers need.
    assert_eq!(
        idx.record_and_dirty(&path, &disk, SurfaceWrite::OpenDoc).verdict,
        SurfaceVerdict::Changed,
        "revert-to-disk must read Changed against the BUFFER baseline"
    );
}

/// didClose reconcile: consumers flip back to the indexed disk copy, so
/// `mark_doc_closed` re-records it (Changed when the buffer died with an
/// unsaved contract change) and background writes own the record again.
#[test]
fn close_reconciles_the_disk_record() {
    use crate::module_index::SurfaceWrite;
    use crate::surface::SurfaceVerdict;
    let idx = ModuleIndex::new_for_test();
    let path = PathBuf::from("/fake/prov/Gadget.pm");
    let disk = build_fa("package Gadget;\nsub base { 1 }\n1;\n");
    let buffer = build_fa("package Gadget;\nsub base { 1 }\nsub unsaved { 2 }\n1;\n");

    // The indexed disk copy (registration records Background — but the doc
    // opens first here, so the registration's record is suppressed and the
    // copy still registers).
    idx.mark_doc_open(&path);
    let _ = idx.register_workspace_resident(path.clone(), Arc::new(disk));
    // Open-doc record: the buffer's unsaved contract change. FirstSeen —
    // the registration's background record above was suppressed, so this
    // is the freshness index's first sight of the path.
    assert_eq!(
        idx.record_and_dirty(&path, &buffer, SurfaceWrite::OpenDoc).verdict,
        SurfaceVerdict::FirstSeen
    );

    // Close: reconcile against the registered disk copy.
    let sd = idx.mark_doc_closed(&path).expect("indexed copy exists");
    assert_eq!(
        sd.verdict,
        SurfaceVerdict::Changed,
        "buffer died with an unsaved contract change — the flip to disk is Changed"
    );
    // Background writers own the record again: re-recording disk is Unchanged.
    assert_eq!(
        idx.record_and_dirty(&path, &build_fa("package Gadget;\nsub base { 1 }\n1;\n"), SurfaceWrite::Background)
            .verdict,
        SurfaceVerdict::Unchanged
    );
}
