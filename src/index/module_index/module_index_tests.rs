use super::*;

fn parse_source_to_cached(source: &str, module_name: &str) -> Arc<CachedModule> {
    use tree_sitter::Parser;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let analysis = crate::build::builder::build(&tree, source.as_bytes());
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
    use crate::index::pack_bag_cache::PackBagCache;
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

/// The symbols-axis reader: a bag-only-evicted copy (the @INC strip) answers
/// with the RESIDENT arc — no loader call, no whole-blob decode — because the
/// MRO existence walk reads only symbols. A symbols-evicted copy (workspace
/// strip) still rehydrates. If the fast path regressed, every idle-sweep
/// ancestor hop would re-decode a blob to scan symbols it already holds.
#[test]
fn symbols_present_answers_resident_when_only_bag_evicted() {
    use crate::index::pack_bag_cache::PackBagCache;
    use crate::model::file_analysis::CrossFileLookup;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let path = full.path.clone();

    // Loader that PANICS: proof the bag-only-evicted path never rehydrates.
    let cache = Arc::new(PackBagCache::new(8 * 1024 * 1024, |_p: &std::path::Path| {
        panic!("symbols_present must not rehydrate a symbols-resident copy")
    }));
    let idx = ModuleIndex::new_for_cli().with_bag_cache(cache);

    let mut bag_only = (*full.analysis).clone();
    bag_only.evict_to(crate::model::file_analysis::Residency::RowsOnly);
    let bag_only_cached = Arc::new(CachedModule::new(path.clone(), Arc::new(bag_only)));
    let got = idx.symbols_present(&bag_only_cached);
    assert!(
        Arc::ptr_eq(&got, &bag_only_cached.analysis),
        "bag-only-evicted copy answers resident (ptr-identical)"
    );
    assert!(got.has_sub_in_package("make", "Widget"));

}

/// The absence-as-answer tripwire: a WORKSPACE-strip copy (bag + refs +
/// SYMBOLS evicted) asked an existence question whose true answer is YES
/// must rehydrate and answer YES. If `symbols_present` ever degrades to
/// "resident or empty", the evicted-empty symbol table reads as "class
/// defines nothing" — goto-def/hover silently resolve to nothing, no error,
/// no crash. Empty from this reader must always mean genuinely-no-symbol.
#[test]
fn symbols_present_rehydrates_evicted_symbols_never_absence_by_eviction() {
    use crate::index::pack_bag_cache::PackBagCache;
    use crate::model::file_analysis::CrossFileLookup;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let full_for_loader = full.analysis.clone();
    let cache = Arc::new(PackBagCache::new(8 * 1024 * 1024, move |_p: &std::path::Path| {
        Ok((*full_for_loader).clone())
    }));
    let idx = ModuleIndex::new_for_cli().with_bag_cache(cache);
    let mut stripped = (*full.analysis).clone();
    stripped.evict_to(crate::model::file_analysis::Residency::Skeleton);
    assert!(stripped.symbols_are_evicted(), "fixture must model the workspace strip");
    let stripped_cached = Arc::new(CachedModule::new(full.path.clone(), Arc::new(stripped)));
    let got = idx.symbols_present(&stripped_cached);
    assert!(!got.symbols_are_evicted(), "rehydrated view carries symbols");
    assert!(
        got.has_sub_in_package("make", "Widget"),
        "true-YES existence must survive symbol eviction"
    );
}

/// The refs-axis reader's resident fast path: a bag-only-evicted copy (the
/// @INC strip leaves refs AND symbols resident) answers with the resident
/// arc — no decode. The panicking loader is the proof.
#[test]
fn refs_present_answers_resident_when_only_bag_evicted() {
    use crate::index::pack_bag_cache::PackBagCache;
    use crate::model::file_analysis::CrossFileLookup;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\nWidget::make('Widget');\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let cache = Arc::new(PackBagCache::new(8 * 1024 * 1024, |_p: &std::path::Path| {
        panic!("refs_present must not rehydrate a rows-resident copy")
    }));
    let idx = ModuleIndex::new_for_cli().with_bag_cache(cache);
    let mut bag_only = (*full.analysis).clone();
    bag_only.evict_to(crate::model::file_analysis::Residency::RowsOnly);
    let cached = Arc::new(CachedModule::new(full.path.clone(), Arc::new(bag_only)));
    let got = idx.refs_present(&cached);
    assert!(Arc::ptr_eq(&got, &cached.analysis), "rows-resident copy answers resident");
    assert!(!got.refs().is_empty(), "refs axis is populated on the fast path");
}

/// The absence-as-answer tripwire for the refs axis: a WORKSPACE-strip copy
/// (bag + refs + symbols evicted) asked a refs question whose true answer is
/// non-empty must rehydrate and answer it. If `refs_present` ever degrades
/// to "resident or empty", the evicted-empty ref table reads as "this file
/// has no matching refs" — `references` under-reports with no error, and
/// gold may hold no row covering the missing file.
#[test]
fn refs_present_rehydrates_evicted_refs_never_absence_by_eviction() {
    use crate::index::pack_bag_cache::PackBagCache;
    use crate::model::file_analysis::CrossFileLookup;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\nWidget::make('Widget');\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let full_for_loader = full.analysis.clone();
    let cache = Arc::new(PackBagCache::new(8 * 1024 * 1024, move |_p: &std::path::Path| {
        Ok((*full_for_loader).clone())
    }));
    let idx = ModuleIndex::new_for_cli().with_bag_cache(cache);
    let mut stripped = (*full.analysis).clone();
    stripped.evict_to(crate::model::file_analysis::Residency::Skeleton);
    assert!(stripped.refs_are_evicted(), "fixture must model the workspace strip");
    let cached = Arc::new(CachedModule::new(full.path.clone(), Arc::new(stripped)));
    let got = idx.refs_present(&cached);
    assert!(!got.refs_are_evicted(), "rehydrated view carries refs");
    assert!(!got.symbols_are_evicted(), "rehydrated view carries symbols (matcher reads both)");
    assert!(
        got.refs().iter().any(|r| r.target_name.contains("make")),
        "true non-empty refs answer must survive eviction"
    );
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
    use crate::model::file_analysis::InferredType;

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
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(orphaned, None).unwrap();
    let analysis = Arc::new(crate::build::builder::build(&tree, orphaned.as_bytes()));
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
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
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
            .is_some_and(|c| c.analysis.symbols().iter().any(|s| s.name == name))
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
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
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
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
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
            .is_some_and(|c| c.analysis.symbols().iter().any(|s| s.name == name))
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

/// A changed file re-registers via unregister-then-register (the
/// `PackInvalidator` swap): names its new version no longer defines
/// must not linger in any view.
#[cfg(feature = "cpp")]
#[test]
fn edit_swap_drops_names_the_new_version_lost() {
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
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
    use crate::index::pack_bag_cache::PackBagCache;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    let full_syms = full.analysis.symbols().len();
    assert!(full_syms > 0);
    let path = full.path.clone();

    let idx = ModuleIndex::new_for_cli();
    let arc = idx.register_symbols_stripping((*path).to_path_buf(), (*full.analysis).clone(), crate::model::file_analysis::Residency::Skeleton);
    assert!(arc.symbols_are_evicted() && arc.symbols().is_empty(), "stored copy is stripped");
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
    assert_eq!(whole.symbols().len(), full_syms);

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

    // Seam 2: the PackageSymbol cross-file primary — B::make as a method
    // call target.
    let t2 = a
        .analysis
        .find_method_return_type("B", "make", Some(&idx), None)
        .expect("PackageSymbol chase must fill through the overlay");
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
    impl crate::model::file_analysis::CrossFileLookup for Bare {
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
            _f: &mut dyn FnMut(&str, &Arc<CachedModule>, &crate::model::file_analysis::Symbol),
        ) {
        }
        fn direct_children_of(&self, _p: &str) -> Vec<(String, String)> {
            Vec::new()
        }
        fn for_each_loader_shape(
            &self,
            _f: &mut dyn FnMut(&str, &crate::model::file_analysis::InferredType),
        ) {
        }
    }
    let cm = parse_source_to_cached("package P;\nsub f { return 1 }\n1;\n", "P");
    let lk = Bare(Arc::clone(&cm));
    let e = crate::model::file_analysis::CrossFileLookup::enriched_present(&lk, &cm);
    let b = crate::model::file_analysis::CrossFileLookup::bag_present(&lk, &cm);
    assert!(Arc::ptr_eq(&e, &b), "default enriched view IS the raw bag view");
}

/// Build a `FileAnalysis` and cache it under `module_name` with a plugin
/// namespace bridging to `bridge_class`, its entity being the named sub.
/// Exercises the bridged-entity hop of the `PackageSymbol` fallback.
fn cache_bridged(
    idx: &ModuleIndex,
    module_name: &str,
    source: &str,
    entity_sub: &str,
    bridge_class: &str,
) {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(source, None).unwrap();
    let mut fa = crate::build::builder::build(&tree, source.as_bytes());
    let entity_id = fa
        .symbols()
        .iter()
        .find(|s| s.name == entity_sub)
        .map(|s| s.id)
        .expect("entity sub must exist");
    fa.plugin.namespaces.push(crate::model::file_analysis::PluginNamespace {
        id: format!("test:{module_name}"),
        plugin_id: "test".into(),
        kind: "emitter".into(),
        entities: vec![entity_id],
        bridges: vec![crate::model::file_analysis::Bridge::Class(bridge_class.into())],
        decl_span: crate::model::file_analysis::Span {
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
/// after the bridging file is itself enriched. The `PackageSymbol` bridged
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
        .symbols()
        .iter()
        .find(|s| s.name == "render")
        .map(|s| s.id)
        .unwrap();
    assert_eq!(
        br.analysis.symbol_return_type_via_bag(render_id, None),
        None,
        "fixture must dead-end on the raw bag"
    );

    // PackageSymbol{Painter, render}: hop (1)/(2) find nothing (no Painter
    // module, no parents); the bridged hop (3) retries through the enriched
    // overlay and resolves Widget.
    let t = idx_find_method_return(&idx, "Painter", "render");
    assert_eq!(
        t.as_ref().and_then(|t| t.class_name().map(|s| s.to_string())).as_deref(),
        Some("Widget"),
        "bridged entity resolved through Br's enriched overlay: {t:?}"
    );
}

/// Route a `PackageSymbol{package, name}` query from a throwaway consumer FA
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
/// for symmetry with the PackageSymbol primary; this test guards the refactor
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
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(app_src, None).unwrap();
    let fa = crate::build::builder::build(&tree, app_src.as_bytes());

    // `$c` rode `Edge(Expr($s->{conn}))`, which drilled `SlotType{Store, conn}`
    // into Store's own file (hop 1) and found the `Conn->new` slot write.
    let t = fa.inferred_type_via_bag_ctx(
        "$c",
        tree_sitter::Point { row: 4, column: 0 },
        Some(&idx),
    );
    assert_eq!(
        t.as_ref().and_then(|t| t.class_name().map(|s| s.to_string())).as_deref(),
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
    let mut fa = crate::build::builder::build(&tree, src.as_bytes());
    fa.evict_to(crate::model::file_analysis::Residency::Skeleton);
    let cm = Arc::new(CachedModule::new(
        PathBuf::from("/fake/Ghost.pm"),
        Arc::new(fa),
    ));

    let before = crate::index::module_index::rehydration_miss_count();
    let served = crate::model::file_analysis::CrossFileLookup::bag_present(&idx, &cm);
    assert!(
        Arc::ptr_eq(&served, &cm.analysis),
        "miss degrades to the stripped resident copy, never fabricates"
    );
    assert!(
        crate::index::module_index::rehydration_miss_count() > before,
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
    let whole = crate::build::builder::build(&tree, src.as_bytes());
    let whole_arc = Arc::new(whole);
    let mut stripped = (*whole_arc).clone();
    stripped.evict_to(crate::model::file_analysis::Residency::Skeleton);
    let path = PathBuf::from("/fake/hub/Ghost.pm");

    // Install the hub's rehydration cell with a loader that serves the
    // whole copy (stands in for the modules.db blob load).
    let served = Arc::clone(&whole_arc);
    hub.set_bag_cache(Arc::new(crate::index::pack_bag_cache::PackBagCache::new(
        128 * 1024 * 1024,
        move |p: &std::path::Path| {
            if p == std::path::Path::new("/fake/hub/Ghost.pm") {
                Ok((*served).clone())
            } else {
                Err(crate::index::module_cache::RehydrateMiss::NoRow)
            }
        },
    )));

    // Attach a pack sub-index (its own bag cache can never serve the path).
    let sub = Arc::new(ModuleIndex::new_for_test());
    hub.attach_pack_index("cpp", Arc::clone(&sub));

    // The misrouted ask: the sub-index handed a hub-owned stripped copy.
    let cm = Arc::new(CachedModule::new(path, Arc::new(stripped)));
    let before = crate::index::module_index::rehydration_miss_count();
    let full = crate::model::file_analysis::CrossFileLookup::whole_present(sub.as_ref(), &cm);
    assert!(
        full.symbols().iter().any(|s| s.name == "boo"),
        "foreign route must serve the owner's WHOLE copy, not the stripped resident"
    );
    assert_eq!(
        crate::index::module_index::rehydration_miss_count(),
        before,
        "a foreign-routed answer is a hit, not a residency miss"
    );
}

/// Build a FileAnalysis from source (whole, never registered).
fn build_fa(src: &str) -> crate::model::file_analysis::FileAnalysis {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    crate::build::builder::build(&tree, src.as_bytes())
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
    use crate::index::module_index::SurfaceWrite;
    use crate::model::surface::SurfaceVerdict;
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
    use crate::index::module_index::SurfaceWrite;
    use crate::model::surface::SurfaceVerdict;
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

/// D2 drift pin: a plugin-carrying module resolved VIA THE RESOLVER THREAD
/// feeds `loader_config_shapes` exactly like a direct `insert_cache` — both
/// route through `IndexCore::insert_resolved`, whose projections run on the
/// WHOLE analysis before the registration-owned strip drops the bag. The
/// prior thread path fed `edges` only, so an @INC-resolved loader's config
/// shape never reached `for_each_loader_shape` (the enrichment read that
/// types `$conf` in the plugin's `register`).
#[test]
fn thread_path_resolution_feeds_loader_config_shapes() {
    use crate::model::file_analysis::CrossFileLookup;
    let dir = std::env::temp_dir().join(format!(
        "qx-thread-shapes-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("lib/My")).unwrap();
    std::fs::write(
        dir.join("lib/My/App.pm"),
        "package My::App;\nuse Mojolicious::Lite;\nplugin 'CloveApp', { minion => 1, redis => 'r' };\n1;\n",
    )
    .unwrap();

    let idx = ModuleIndex::new_for_test();
    // file:// spelling — the thread's project-lib discovery strips the
    // scheme to find `lib/`.
    idx.set_workspace_root(Some(&format!("file://{}", dir.display())));
    idx.request_resolve("My::App");
    assert!(
        idx.wait_resolved("My::App", std::time::Duration::from_secs(30)),
        "resolver thread should resolve the project-lib module",
    );

    let mut shapes: Vec<String> = Vec::new();
    idx.for_each_loader_shape(&mut |name, _t| shapes.push(name.to_string()));
    assert!(
        shapes.iter().any(|n| n == "CloveApp"),
        "thread-path resolution must feed loader_config_shapes; got {shapes:?}",
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Tripwire for the enrichment-key memo: every writer that mutates the
/// key's read set (registration gens, freshness records/removes, cache
/// slots, loader shapes) must move `enrichment_epoch()`, or the memo
/// serves a stale key — silent wrong cross-file answers, not slowness.
/// A new mutation path that fails this test needs a bump at its owning
/// choke point (`gen_counter` mint, `FreshnessIndex` write, or
/// `note_shape_change`), never a call-site bump.
#[test]
fn enrichment_epoch_moves_on_every_writer() {
    let idx = ModuleIndex::new_for_test();
    let mut last = idx.enrichment_epoch();
    let mut expect_move = |idx: &ModuleIndex, what: &str| {
        let now = idx.enrichment_epoch();
        assert!(now > last, "{what} must move the enrichment epoch");
        last = now;
    };

    // 1. Workspace registration front door (gen mint + surface record).
    let a = parse_source_to_cached("package EpochA;\nsub go { 1 }\n1;\n", "EpochA");
    idx.register_workspace_module(a.path.to_path_buf(), Arc::clone(&a.analysis));
    expect_move(&idx, "register_workspace_module");

    // 2. @INC/CLI cache insertion (insert_resolved).
    let b = parse_source_to_cached("package EpochB;\nsub b { 2 }\n1;\n", "EpochB");
    idx.insert_cache("EpochB", Some(Arc::clone(&b)));
    expect_move(&idx, "insert_cache");

    // 3. A CHANGED surface record (freshness write).
    let a2 = build_fa("package EpochA;\nsub go { 1 }\nsub extra { 3 }\n1;\n");
    idx.record_and_dirty(&a.path, &a2, SurfaceWrite::Background);
    expect_move(&idx, "record_and_dirty (Changed)");

    // 4. Loader-shape rewrite.
    idx.record_workspace_projections(&a.path, &a.analysis);
    expect_move(&idx, "record_workspace_projections");

    // 5. Surface removal (file deleted).
    idx.remove_surface(&a.path);
    expect_move(&idx, "remove_surface");

    // 6. Workspace unregistration (cache-slot drop).
    idx.unregister_workspace_path(&a.path);
    expect_move(&idx, "unregister_workspace_path");

    // 7. Pack-file unregistration (cache-slot survivor re-pick).
    idx.unregister_file(&b.path);
    expect_move(&idx, "unregister_file");
}

/// The memo itself: identical epoch → the key is served from the memo
/// (same value); any writer between consults → recompute. Guards the
/// read-epoch-BEFORE-walk ordering too (a mid-walk mutation may store a
/// mixed key, but only under the pre-mutation epoch).
#[test]
fn enrichment_key_memo_serves_stable_then_invalidates() {
    let idx = ModuleIndex::new_for_test();
    let consumer = parse_source_to_cached(
        "package MemoApp;\nuse Memo::Dep 'make';\nsub go { my $x = make(); return $x }\n1;\n",
        "MemoApp",
    );
    idx.register_workspace_module(consumer.path.to_path_buf(), Arc::clone(&consumer.analysis));
    let dep = parse_source_to_cached(
        "package Memo::Dep;\nour @EXPORT_OK = ('make');\nsub make { return bless {}, 'M1' }\n1;\n",
        "Memo::Dep",
    );
    idx.insert_cache("Memo::Dep", Some(dep));

    let k1 = idx.enrichment_key_memoized(&consumer);
    let k1b = idx.enrichment_key_memoized(&consumer);
    assert_eq!(k1, k1b, "stable epoch: memoized key is identical");

    // A dep re-resolve must invalidate the memo AND change the key.
    let dep2 = parse_source_to_cached(
        "package Memo::Dep;\nour @EXPORT_OK = ('make');\nsub make { return bless {}, 'M2' }\n1;\n",
        "Memo::Dep",
    );
    idx.insert_cache("Memo::Dep", Some(dep2));
    let k2 = idx.enrichment_key_memoized(&consumer);
    assert_ne!(k1, k2, "dep re-resolve must move the memoized key");
}

/// The batch-fire convergence guarantee: every drained batch that resolved
/// at least one module ends with an `on_resolved` fire, so once the queue
/// drains the diagnostics refresh has fired AFTER the last resolution and
/// open docs converge. Guards against a regression (fixed-size batches,
/// timer-only fires) that would leave a trailing partial batch's
/// resolutions unpublished. Three sequential resolves force three separate
/// batches; each must be followed by its own fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolver_fires_refresh_after_every_drained_batch() {
    use tower_lsp::lsp_types::{InitializeParams, InitializeResult};
    use tower_lsp::{jsonrpc, LanguageServer, LspService};
    struct Stub;
    #[tower_lsp::async_trait]
    impl LanguageServer for Stub {
        async fn initialize(
            &self,
            _: InitializeParams,
        ) -> jsonrpc::Result<InitializeResult> {
            Ok(InitializeResult::default())
        }
        async fn shutdown(&self) -> jsonrpc::Result<()> {
            Ok(())
        }
    }
    let (client_tx, client_rx) = std::sync::mpsc::channel();
    let (_service, _socket) = LspService::new(move |client: tower_lsp::Client| {
        client_tx.send(client.clone()).unwrap();
        Stub
    });
    let client = client_rx.recv().unwrap();

    let fires = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fires_cb = Arc::clone(&fires);
    let idx = ModuleIndex::new(client, move || {
        fires_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });

    // Project-local lib with three modules; no cpanfile, so the resolver
    // sends no progress traffic over the unread socket.
    let dir = std::env::temp_dir().join(format!(
        "qx-batch-fire-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("lib/Batch")).unwrap();
    for m in ["A", "B", "C"] {
        std::fs::write(
            dir.join(format!("lib/Batch/{m}.pm")),
            format!("package Batch::{m};\nsub go {{ 1 }}\n1;\n"),
        )
        .unwrap();
    }
    idx.set_workspace_root(Some(&format!("file://{}", dir.display())));

    for name in ["Batch::A", "Batch::B", "Batch::C"] {
        let before = fires.load(std::sync::atomic::Ordering::SeqCst);
        idx.request_resolve(name);
        assert!(
            idx.wait_resolved(name, std::time::Duration::from_secs(30)),
            "{name} should resolve from the project lib"
        );
        // The fire lands right after the batch loop's last module; give the
        // thread a moment to reach it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fires.load(std::sync::atomic::Ordering::SeqCst) <= before
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            fires.load(std::sync::atomic::Ordering::SeqCst) > before,
            "queue drained after {name} but no diagnostics refresh fired"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Perl package identity: name → MANY files (the candidate relation) ----

/// A multi-package `.pm` is ordinary Perl: EVERY declared package must be
/// reachable by name, not just the first (`package_names`, not a
/// first-package key).
#[test]
fn second_package_in_file_is_reachable_by_name() {
    let idx = ModuleIndex::new_for_test();
    let fa = build_fa(
        "package Alpha::First;\nsub one { 1 }\npackage Alpha::Second;\nsub two { 2 }\n1;\n",
    );
    let path = PathBuf::from("/fake/pkgid/Alpha.pm");
    let _ = idx.register_workspace_resident(path.clone(), Arc::new(fa));
    for name in ["Alpha::First", "Alpha::Second"] {
        let cm = idx.get_cached(name).unwrap_or_else(|| panic!("{name} unreachable"));
        assert_eq!(cm.path, path);
        assert!(idx.is_workspace_module(name), "{name} not marked workspace");
    }
    // The reverse index sees the second package's sub too.
    assert!(!idx.modules_with_symbol("two").is_empty());
}

/// Two files reopening one package: the cache winner is decided by the
/// candidate SET (smallest path), never by arrival order, and the candidate
/// table keeps both files either way.
#[test]
fn same_package_two_files_is_order_independent() {
    use crate::model::file_analysis::CrossFileLookup;
    let src_a = "package Shared::Thing;\nsub from_alpha { 1 }\n1;\n";
    let src_b = "package Shared::Thing;\nsub from_beta { 3 }\n1;\n";
    let pa = PathBuf::from("/fake/pkgid/Alpha.pm");
    let pb = PathBuf::from("/fake/pkgid/Beta.pm");
    let mut winners = Vec::new();
    for order in [[(&pa, src_a), (&pb, src_b)], [(&pb, src_b), (&pa, src_a)]] {
        let idx = ModuleIndex::new_for_test();
        for (path, src) in order {
            let _ = idx.register_workspace_resident(path.clone(), Arc::new(build_fa(src)));
        }
        let cands = idx.def_candidates("Shared::Thing");
        let mut paths: Vec<_> = cands.iter().map(|c| c.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec![pa.clone(), pb.clone()], "both files stay candidates");
        winners.push(idx.get_cached("Shared::Thing").expect("winner").path.clone());
    }
    assert_eq!(winners[0], winners[1], "winner must not depend on registration order");
    assert_eq!(winners[0], pa, "smallest-path tie-break");
}

/// Re-registering one file must not wipe a same-named sibling's edges
/// (`purge_module` used to drop the loser's contributions with no replay).
/// The sibling here is SYMBOL-EVICTED, so the re-feed exercises the
/// path-keyed name-record replay, not a live symbol scan.
#[test]
fn reregistering_one_file_keeps_evicted_sibling_edges() {
    let idx = ModuleIndex::new_for_test();
    let src_a = "package Shared::Thing;\nsub from_alpha { 1 }\n1;\n";
    let src_b = "package Shared::Thing;\nsub from_beta { 3 }\n1;\n";
    let pa = PathBuf::from("/fake/pkgid/Alpha.pm");
    let pb = PathBuf::from("/fake/pkgid/Beta.pm");
    // The sibling registers through the stripping door: its resident copy
    // has NO symbols, only the pre-strip name record.
    let _ = idx.register_workspace_stripping(pb.clone(), build_fa(src_b), crate::model::file_analysis::Residency::Skeleton);
    let _ = idx.register_workspace_resident(pa.clone(), Arc::new(build_fa(src_a)));
    assert!(!idx.modules_with_symbol("from_beta").is_empty(), "sibling edges fed");
    // Re-register the whole file; the evicted sibling's names must replay.
    let _ = idx.register_workspace_resident(pa.clone(), Arc::new(build_fa(src_a)));
    assert!(
        !idx.modules_with_symbol("from_beta").is_empty(),
        "re-registering Alpha.pm wiped the evicted sibling's name edges"
    );
    assert!(!idx.modules_with_symbol("from_alpha").is_empty());
}

/// A package reopened in three files: all three stay candidates; removing
/// one re-picks deterministically among survivors; removing all empties the
/// slot and the workspace mark.
#[test]
fn package_reopened_in_three_files_unregisters_cleanly() {
    use crate::model::file_analysis::CrossFileLookup;
    let idx = ModuleIndex::new_for_test();
    let paths: Vec<PathBuf> = ["A", "B", "C"]
        .iter()
        .map(|n| PathBuf::from(format!("/fake/pkgid3/{n}.pm")))
        .collect();
    for (i, p) in paths.iter().enumerate() {
        let src = format!("package Tri::Pod;\nsub sub{i} {{ {i} }}\n1;\n");
        let _ = idx.register_workspace_resident(p.clone(), Arc::new(build_fa(&src)));
    }
    assert_eq!(idx.def_candidates("Tri::Pod").len(), 3);
    assert_eq!(idx.get_cached("Tri::Pod").unwrap().path, paths[0]);

    // Removing the WINNER re-picks the next-smallest path.
    idx.unregister_workspace_path(&paths[0]);
    assert_eq!(idx.def_candidates("Tri::Pod").len(), 2);
    assert_eq!(idx.get_cached("Tri::Pod").unwrap().path, paths[1]);
    // The departed file's sub left the reverse index; survivors' stayed.
    assert!(idx.modules_with_symbol("sub0").is_empty());
    assert!(!idx.modules_with_symbol("sub2").is_empty());

    idx.unregister_workspace_path(&paths[1]);
    idx.unregister_workspace_path(&paths[2]);
    assert!(idx.get_cached("Tri::Pod").is_none(), "no survivors: slot removed");
    assert!(!idx.is_workspace_module("Tri::Pod"));
}

/// An edit that drops a package must shed its name registrations (the
/// adopt path diffs the per-path record, the Perl twin of
/// `edit_swap_drops_names_the_new_version_lost`).
#[test]
fn reregister_sheds_dropped_package_names() {
    let idx = ModuleIndex::new_for_test();
    let path = PathBuf::from("/fake/pkgid/Edit.pm");
    let v1 = build_fa("package Keep::Me;\nsub k { 1 }\npackage Drop::Me;\nsub d { 2 }\n1;\n");
    let _ = idx.register_workspace_resident(path.clone(), Arc::new(v1));
    assert!(idx.get_cached("Drop::Me").is_some());
    let v2 = build_fa("package Keep::Me;\nsub k { 1 }\n1;\n");
    let _ = idx.register_workspace_resident(path.clone(), Arc::new(v2));
    assert!(idx.get_cached("Drop::Me").is_none(), "dropped package lingers");
    assert!(!idx.is_workspace_module("Drop::Me"));
    assert!(idx.get_cached("Keep::Me").is_some());
}

// ---- Reverse-index bucket contracts ----
//
// `ModuleBucket` replaced a `Vec<String>` whose per-insert linear scan made
// bulk feeds quadratic in bucket size, and `purge_module` gained a
// never-fed guard because its sweep is O(every bucket of every map). Both
// are performance changes whose FAILURE MODE is silent wrong answers — a
// dropped edge, or a stale one that survives a purge — so the contracts
// they rest on are pinned here.

#[test]
fn a_bucket_dedups_and_keeps_first_fed_order() {
    use crate::index::module_index::ModuleBucket;
    let mut b = ModuleBucket::default();
    b.insert("B");
    b.insert("A");
    b.insert("B"); // re-feed must not grow the bucket
    assert_eq!(b.as_slice(), &["B".to_string(), "A".to_string()]);
    b.remove("B");
    assert_eq!(b.as_slice(), &["A".to_string()]);
    assert!(!b.is_empty());
    b.remove("A");
    assert!(b.is_empty());
}

#[test]
fn a_bucket_still_dedups_past_the_set_threshold() {
    // The membership set materializes only once a bucket is big enough for
    // the scan to cost more than the hash. Crossing that boundary must not
    // change behavior — the worst real bucket (`new`, declared by every
    // module in the workspace) lives far past it.
    use crate::index::module_index::ModuleBucket;
    let mut b = ModuleBucket::default();
    for i in 0..200 {
        b.insert(&format!("M{i}"));
    }
    assert_eq!(b.as_slice().len(), 200);
    for i in 0..200 {
        b.insert(&format!("M{i}")); // every one a duplicate
    }
    assert_eq!(b.as_slice().len(), 200, "re-feed grew a set-backed bucket");
    b.remove("M150");
    assert_eq!(b.as_slice().len(), 199);
    b.insert("M150"); // removed from the set too, so it can come back
    assert_eq!(b.as_slice().len(), 200);
}

#[test]
fn purge_reaches_every_edge_a_publication_created() {
    // The never-fed guard makes `purge_module` a no-op for a module with no
    // edges. If ANY write path published edges without marking, the guard
    // would skip a module that does have them and the stale edge would
    // outlive its file — which is why the spec/children writes go through
    // `publish_*` rather than touching the maps directly.
    use crate::index::module_index::ModuleEdgeIndexes;
    let edges = ModuleEdgeIndexes::new();
    edges.publish_spec("Primary", "Spec::Impl");
    edges.publish_child("Base", "Derived");
    assert!(!edges.specs_for("Primary").is_empty());
    assert!(!edges.children_of("Base").is_empty());

    edges.purge_module("Spec::Impl");
    edges.purge_module("Derived");
    assert!(
        edges.specs_for("Primary").is_empty(),
        "a directly-published spec edge survived its module's purge",
    );
    assert!(
        edges.children_of("Base").is_empty(),
        "a directly-published child edge survived its module's purge",
    );
}

#[test]
fn purging_a_never_fed_module_is_a_no_op_not_a_wipe() {
    use crate::index::module_index::ModuleEdgeIndexes;
    let edges = ModuleEdgeIndexes::new();
    edges.publish_child("Base", "Derived");
    // A name nothing ever fed: the guard skips the sweep, and the sweep
    // skipping must not be observable as lost edges for OTHER modules.
    edges.purge_module("Never::Fed");
    assert_eq!(edges.children_of("Base"), vec!["Derived".to_string()]);
}

// ---- the resolution session (docs/adr/resolution-session.md) ----

/// One backward walk asks the same cross-file question at every call site.
/// Without the session each ask re-derives the whole `PackageSymbol`
/// lattice; with it the candidate's contribution is derived ONCE and the
/// answer must not move. Both halves are asserted here — a memo whose
/// failure mode is a wrong answer needs the answer pinned, not just the
/// consult count.
#[test]
fn a_session_derives_a_cross_file_answer_once_and_answers_the_same() {
    use crate::model::witnesses::ResolutionSession;
    let provider = parse_source_to_cached(
        "package Prov;\nsub make { return bless {}, 'Res' }\n1;\n",
        "Prov",
    );
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(provider.path.to_path_buf(), Arc::clone(&provider.analysis));
    let caller = parse_source_to_cached(
        "package Caller;\nuse Prov;\nsub go { my $r = Prov->make(); return $r }\n1;\n",
        "Caller",
    );
    let lookup: &dyn crate::model::file_analysis::CrossFileLookup = &idx;

    let ask = || {
        caller
            .analysis
            .find_method_return_type("Prov", "make", Some(lookup), None)
            .and_then(|t| t.class_name().map(|s| s.to_string()))
    };

    let unsessioned: Vec<_> = (0..8).map(|_| ask()).collect();
    assert_eq!(
        unsessioned[0].as_deref(),
        Some("Res"),
        "fixture must resolve cross-file, or the test proves nothing"
    );

    let sessioned: Vec<_> = {
        let _s = ResolutionSession::enter(Some(lookup));
        let answers: Vec<_> = (0..8).map(|_| ask()).collect();
        let stats = ResolutionSession::stats().expect("session is open");
        assert_eq!(
            stats.consults, 1,
            "8 identical asks must consult the provider once, not eight times"
        );
        assert_eq!(stats.hits, 7, "the other seven ride the memo");
        answers
    };
    assert_eq!(sessioned, unsessioned, "the memo changed an answer");

    // And the memo is scoped to the walk: outside it, consults resume.
    assert!(ResolutionSession::stats().is_none());
    assert_eq!(ask(), unsessioned[0]);
}

/// The memo rides `resolution_epoch`. A registration moves it, so a
/// remembered answer never survives the index change that could invalidate
/// it — over-invalidation is the safe direction.
#[test]
fn a_session_memo_drops_when_the_index_moves() {
    use crate::model::witnesses::ResolutionSession;
    let provider = parse_source_to_cached(
        "package Prov;\nsub make { return bless {}, 'Res' }\n1;\n",
        "Prov",
    );
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(provider.path.to_path_buf(), Arc::clone(&provider.analysis));
    let caller = parse_source_to_cached("package Caller;\nuse Prov;\n1;\n", "Caller");
    let lookup: &dyn crate::model::file_analysis::CrossFileLookup = &idx;
    let ask = || {
        caller
            .analysis
            .find_method_return_type("Prov", "make", Some(lookup), None)
            .and_then(|t| t.class_name().map(|s| s.to_string()))
    };

    let _s = ResolutionSession::enter(Some(lookup));
    assert_eq!(ask().as_deref(), Some("Res"));
    assert_eq!(ask().as_deref(), Some("Res"));
    assert_eq!(ResolutionSession::stats().unwrap().consults, 1);

    let other = parse_source_to_cached("package Other;\nsub o { 1 }\n1;\n", "Other");
    idx.register_workspace_module(other.path.to_path_buf(), Arc::clone(&other.analysis));

    assert_eq!(ask().as_deref(), Some("Res"), "answer survives the drop");
    assert_eq!(
        ResolutionSession::stats().unwrap().consults,
        2,
        "an index mutation must invalidate the remembered answer"
    );
}

/// Even memoized, some query at some scale outruns any bound. The budget
/// makes that case DEGRADE — a marked-incomplete answer, promptly — rather
/// than run forever. Zero fuel is the extreme: every cross-file consult is
/// refused, the walk still terminates and still answers what it can.
#[test]
fn an_exhausted_consult_budget_degrades_instead_of_running_forever() {
    use crate::model::witnesses::ResolutionSession;
    let provider = parse_source_to_cached(
        "package Prov;\nsub make { return bless {}, 'Res' }\n1;\n",
        "Prov",
    );
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(provider.path.to_path_buf(), Arc::clone(&provider.analysis));
    let caller = parse_source_to_cached("package Caller;\nuse Prov;\n1;\n", "Caller");
    let lookup: &dyn crate::model::file_analysis::CrossFileLookup = &idx;

    let _s = ResolutionSession::enter_with_budget(Some(lookup), 0);
    let answer = caller
        .analysis
        .find_method_return_type("Prov", "make", Some(lookup), None);
    assert!(answer.is_none(), "no budget ⇒ no cross-file answer");
    assert!(
        ResolutionSession::degraded(),
        "and the walk must SAY it under-answered"
    );
}

/// The ladder, proved over EVERY inhabitant of the type.
///
/// The hazard this replaces was `evict_axes(strip_bag, strip_rows)`: four
/// spellings for three states, and the fourth — rows dropped, bag kept —
/// was prevented only by no production site passing it. It is not a crash
/// if someone does. `bag_present` returns a copy whose symbols were
/// evicted, and a consumer that reads both (cross-file import enrichment
/// walks `symbols` off a bag view) reads absence-by-eviction as
/// absence-in-fact: a silently smaller answer.
///
/// `Residency` has no spelling for it, so this asserts the property that
/// makes the bag-then-symbols reads correct BY CONSTRUCTION rather than by
/// audit: **dropping the rows always drops the bag**, i.e. bag present
/// implies rows present.
///
/// A `trybuild` compile-fail case would show one bad line failing to
/// compile; enumerating the variants proves the invariant for the whole
/// domain, needs no extra dependency, and runs on every `cargo test`.
#[test]
fn an_evicted_copy_still_answers_its_export_surface() {
    use crate::model::file_analysis::Residency;
    // The enrichment provider chase skips a candidate that exports none of the
    // names the consumer needs, and it asks the RESIDENT copy so the skip costs
    // no rehydrate. That is only sound while the export surface survives the
    // strip: if eviction ever took `export`/`export_ok`/`export_lookup` with it,
    // every candidate would answer "exports nothing", the chase would skip
    // providers it must visit, and imported return types would quietly stop
    // resolving — no panic, no error, just types going missing.
    let src = "package Widget;\nour @EXPORT = ('make');\nour @EXPORT_OK = ('helper');\n\
               sub make { my $c = shift; return bless {}, $c; }\nsub helper { return 1 }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");
    assert!(
        full.analysis.exports_name("make") && full.analysis.exports_name("helper"),
        "fixture must export both names before eviction"
    );

    for level in [Residency::Whole, Residency::RowsOnly, Residency::Skeleton] {
        let mut fa = (*full.analysis).clone();
        fa.evict_to(level);
        assert!(
            fa.exports_name("make"),
            "@EXPORT name unreachable at {level:?} — the chase's resident gate \
             would skip this provider and lose its imported types"
        );
        assert!(
            fa.exports_name("helper"),
            "@EXPORT_OK name unreachable at {level:?} — same silent loss"
        );
        assert!(
            !fa.exports_name("never_exported"),
            "the gate must stay an over-approximation, not answer true for everything"
        );
    }
}

#[test]
fn residency_is_a_ladder_so_a_bag_view_always_carries_its_rows() {
    use crate::model::file_analysis::Residency;
    let src = "package Widget;\nsub make { my $c = shift; return bless {}, $c; }\n1;\n";
    let full = parse_source_to_cached(src, "Widget");

    // EVERY level the type can express. A new variant added without a case
    // here fails to compile on the match below, not silently at runtime.
    for level in [Residency::Whole, Residency::RowsOnly, Residency::Skeleton] {
        let mut fa = (*full.analysis).clone();
        fa.evict_to(level);
        let (bag_gone, refs_gone, syms_gone) =
            (fa.bag_is_evicted(), fa.refs_are_evicted(), fa.symbols_are_evicted());

        // THE invariant: no level keeps the bag while dropping the rows.
        assert!(
            !(refs_gone || syms_gone) || bag_gone,
            "{level:?} drops a row axis while keeping the bag — the combination \
             that makes `bag_present` hand out an evicted-symbols copy"
        );
        // The row axes move together: they persist as one generation.
        assert_eq!(refs_gone, syms_gone, "{level:?} split the row axes");

        // And each level strips exactly what it advertises.
        let expected = match level {
            Residency::Whole => (false, false),
            Residency::RowsOnly => (true, false),
            Residency::Skeleton => (true, true),
        };
        assert_eq!((bag_gone, refs_gone), expected, "{level:?} stripped the wrong axes");
        assert_eq!(fa.is_fully_resident(), level == Residency::Whole);
    }
}

/// `for_strip` is the one place the "rows only once the blob can rehydrate
/// them" rule lives — it used to be `let strip_rows = strip_bag && rows_ok`
/// written out at each tier. Eviction off means whole regardless of
/// `rows_ok`; rows_ok false never yields a rows strip.
#[test]
fn for_strip_never_yields_rows_without_bag() {
    use crate::model::file_analysis::Residency;
    for rows_ok in [false, true] {
        assert_eq!(Residency::for_strip(false, rows_ok), Residency::Whole);
    }
    assert_eq!(Residency::for_strip(true, false), Residency::RowsOnly);
    assert_eq!(Residency::for_strip(true, true), Residency::Skeleton);
}

/// An enriched copy must carry the flags that describe the analysis it came
/// from. `degraded`, `bag_evicted` and the ref/symbol eviction flags are
/// `serde(skip)`, so the bincode round-trip this copy used to make reset them
/// to false and `after_deserialize` never put them back: the enriched view of
/// a DEGRADED analysis reported itself whole, and the consumers that gate on
/// degradation could not see it.
#[test]
fn an_enriched_copy_keeps_the_degraded_marker() {
    let idx = ModuleIndex::new_for_test();
    let lib = parse_source_to_cached(
        "package Lib;\nour @EXPORT_OK = ('make');\nsub make { my %h = (id => 1); return \\%h }\n1;\n",
        "Lib",
    );
    let consumer = parse_source_to_cached(
        "package App;\nuse Lib 'make';\nsub go { my $x = make(); return $x }\n1;\n",
        "App",
    );
    idx.register_workspace_module(lib.path.to_path_buf(), Arc::clone(&lib.analysis));

    // Mark the consumer degraded, as a parse/extract shortfall would.
    let mut fa = (*consumer.analysis).clone();
    fa.degraded = true;
    let degraded_consumer = Arc::new(CachedModule::new(
        consumer.path.to_path_buf(),
        Arc::new(fa),
    ));
    idx.register_workspace_module(
        degraded_consumer.path.to_path_buf(),
        Arc::clone(&degraded_consumer.analysis),
    );

    let snap = idx
        .enriched_snapshot(&degraded_consumer)
        .expect("a degraded analysis still enriches");
    assert!(
        snap.degraded,
        "the enriched copy dropped the degraded marker, so it claims to be whole",
    );
}

/// A deleted file's loader-config shapes must go with it. `record_workspace_projections`
/// records them keyed by the contributor's path, and `record_loader_shapes`
/// retracts a contributor's entries only when that SAME file re-registers — so a
/// file that is deleted rather than edited never retracts, and its shapes type
/// `$conf` in a plugin's `register` for the rest of the session from a
/// contributor that no longer exists.
///
/// (`loaded_modules` deliberately has no inverse: several files may load one
/// module, so removing on one file's deletion would wrongly un-suppress the
/// entrypoint lint. Its reader is biased honest-quiet, so never-remove is the
/// safe direction there — unlike here, where the stale value is a TYPE.)
#[test]
fn unregistering_a_workspace_file_retracts_its_loader_shapes() {
    let dir = std::env::temp_dir().join(format!(
        "qx-unreg-shapes-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("App.pm");
    std::fs::write(
        &path,
        "package My::App;\nuse Mojolicious::Lite;\nplugin 'CloveApp', { minion => 1 };\n1;\n",
    )
    .unwrap();
    let canon = std::fs::canonicalize(&path).unwrap();

    let idx = ModuleIndex::new_for_test();
    let fa = build_fa(&std::fs::read_to_string(&path).unwrap());
    idx.record_workspace_projections(&canon, &fa);
    let _ = idx.register_workspace_resident(canon.clone(), Arc::new(fa));

    let shapes = |idx: &ModuleIndex| {
        let mut out: Vec<String> = Vec::new();
        idx.for_each_loader_shape(&mut |name, _t| out.push(name.to_string()));
        out
    };
    assert!(
        shapes(&idx).iter().any(|n| n == "CloveApp"),
        "precondition: registration records the shape; got {:?}",
        shapes(&idx)
    );

    idx.unregister_workspace_path(&canon);
    assert!(
        !shapes(&idx).iter().any(|n| n == "CloveApp"),
        "a departed contributor's loader shape survived unregistration: {:?}",
        shapes(&idx)
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The reverse record must retract EXACTLY what the whole-map sweep did.
///
/// `purge_module` no longer scans every bucket; it walks the keys the module
/// was fed under. That is a pure performance change whose failure mode is a
/// stale edge surviving a purge — silent, and only visible later as a
/// phantom module in a lookup. So this compares the two implementations
/// directly on the same state rather than asserting a hand-written expected
/// shape, and it does it on the case that motivated the change: SEVERAL
/// FILES feeding under ONE module name, which is what an installed @INC tree
/// is full of and what `rebuild_name_registration` replays after each purge.
#[test]
fn the_record_driven_purge_matches_the_whole_map_sweep() {
    use crate::index::module_index::ModuleEdgeIndexes;

    // Overlapping symbol names across modules is what makes shared buckets;
    // two files under `Dup::Mod` is the duplicate-name shape.
    let srcs: Vec<(&str, &str)> = vec![
        ("Dup::Mod", "package Dup::Mod; use parent 'Base::One'; sub new {} sub run {}"),
        ("Dup::Mod", "package Dup::Mod; use parent 'Base::Two'; sub new {} sub helper {}"),
        ("Other::Mod", "package Other::Mod; use parent 'Base::One'; sub new {} sub run {}"),
        ("Third::Mod", "package Third::Mod; sub new {} sub only_here {}"),
    ];

    let build = || {
        let edges = ModuleEdgeIndexes::new();
        for (i, (name, src)) in srcs.iter().enumerate() {
            let fa = build_fa(src);
            edges.feed(name, &PathBuf::from(format!("/fake/f{i}.pm")), &fa);
        }
        edges.publish_spec("Primary::T", "Dup::Mod");
        edges.publish_child("Base::One", "Dup::Mod");
        edges
    };

    // Every module in play, including one never fed.
    for target in ["Dup::Mod", "Other::Mod", "Third::Mod", "Never::Fed"] {
        let by_record = build();
        let by_sweep = build();
        assert_eq!(
            by_record.snapshot(),
            by_sweep.snapshot(),
            "the two indexes did not start identical for {target}"
        );
        by_record.purge_module(target);
        by_sweep.purge_module_by_sweep(target);
        assert_eq!(
            by_record.snapshot(),
            by_sweep.snapshot(),
            "record-driven purge of {target} diverged from the whole-map sweep"
        );
    }
}

/// A purge must retract a sibling's contribution too, not just the last
/// feed's. Both files under `Dup::Mod` publish `new`; if the record replaced
/// instead of unioning, the first file's keys would go unrecorded and its
/// edge would outlive the purge.
#[test]
fn a_purge_retracts_every_sibling_feed_under_one_name() {
    use crate::index::module_index::ModuleEdgeIndexes;
    let edges = ModuleEdgeIndexes::new();
    for (i, src) in [
        "package Dup::Mod; sub only_in_first {}",
        "package Dup::Mod; sub only_in_second {}",
    ]
    .iter()
    .enumerate()
    {
        let fa = build_fa(src);
        edges.feed("Dup::Mod", &PathBuf::from(format!("/fake/s{i}.pm")), &fa);
    }
    edges.purge_module("Dup::Mod");
    assert!(
        edges.snapshot().is_empty(),
        "a sibling feed's edges survived the purge: {:?}",
        edges.snapshot()
    );
}
