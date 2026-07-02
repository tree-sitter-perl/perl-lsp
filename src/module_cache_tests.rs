use super::*;
use rusqlite::Connection;

fn test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    conn
}

fn parse_source_to_cached(source: &str, path: &std::path::Path) -> Arc<CachedModule> {
    use tree_sitter::Parser;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let fa = crate::builder::build(&tree, source.as_bytes());
    Arc::new(CachedModule::new(path.to_path_buf(), Arc::new(fa)))
}

#[test]
fn test_db_save_and_load_roundtrip() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_roundtrip.pm");
    std::fs::write(&pm, "package TestModule;\nour @EXPORT = qw(foo bar);\nour @EXPORT_OK = qw(baz);\nsub foo { 1 }\nsub bar { 2 }\nsub baz { 3 }\n1;\n").unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = Some(parse_source_to_cached(&source, &pm));
    save_to_db(&conn, "TestModule", &cached, "import");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, stale) = warm_cache(&conn, &cache);
    assert_eq!(n, 1);
    assert!(stale.is_empty());

    let loaded = cache.get("TestModule").unwrap();
    let loaded = loaded.as_ref().unwrap();
    assert_eq!(loaded.analysis.export, vec!["foo", "bar"]);
    assert_eq!(loaded.analysis.export_ok, vec!["baz"]);

    let _ = std::fs::remove_file(&pm);
}

/// Pin-the-fix: `plugin_namespaces` survives the bincode +
/// zstd + SQLite round trip with entities, bridges, and
/// plugin_id intact. Without this test, schema drift on the
/// PluginNamespace struct would silently truncate cached
/// modules and we'd notice only when cross-file bridge lookups
/// mysteriously missed entries.
#[test]
fn test_db_plugin_namespaces_roundtrip() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("TestMojoApp_namespaces.pm");
    // A Mojolicious::Lite script — mojo-lite + mojo-routes +
    // mojo-helpers should all emit namespaces that round-trip.
    std::fs::write(
        &pm,
        "package TestMojoApp;\n\
             use Mojolicious::Lite;\n\
             app->helper(current_user => sub { my ($c) = @_; });\n\
             get '/users' => sub { my $c = shift; };\n\
             1;\n",
    )
    .unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = Some(parse_source_to_cached(&source, &pm));
    let original_ns_count = cached.as_ref().unwrap().analysis.plugin_namespaces.len();
    assert!(
        original_ns_count > 0,
        "sanity: fixture must produce at least one PluginNamespace"
    );

    save_to_db(&conn, "TestMojoApp", &cached, "import");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, stale) = warm_cache(&conn, &cache);
    assert_eq!(n, 1);
    assert!(stale.is_empty(), "fresh insert should not be stale");

    let loaded = cache.get("TestMojoApp").unwrap();
    let loaded = loaded.as_ref().unwrap();
    let loaded_ns = &loaded.analysis.plugin_namespaces;
    assert_eq!(
        loaded_ns.len(),
        original_ns_count,
        "PluginNamespace count must round-trip; got: {:?}",
        loaded_ns
    );

    // Every namespace must preserve its plugin_id, kind, and at
    // least one Bridge::Class — the three fields that `bridges_index`
    // and `for_each_entity_bridged_to` depend on.
    for ns in loaded_ns {
        assert!(!ns.plugin_id.is_empty(), "plugin_id preserved");
        assert!(!ns.kind.is_empty(), "kind preserved");
        assert!(!ns.bridges.is_empty(), "bridges preserved");
        assert!(
            ns.bridges
                .iter()
                .any(|b| matches!(b, crate::file_analysis::Bridge::Class(_))),
            "at least one Class bridge survives"
        );
    }

    let _ = std::fs::remove_file(&pm);
}

#[test]
fn test_db_negative_result_roundtrip() {
    let conn = test_db();
    save_to_db(&conn, "Nonexistent::Module", &None, "import");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 1);

    let entry = cache.get("Nonexistent::Module").unwrap();
    assert!(entry.is_none());
}

#[test]
fn test_db_stale_entry_skipped() {
    let conn = test_db();

    let dir = std::env::temp_dir();
    let pm = dir.join("StaleModule_v9.pm");
    std::fs::write(
        &pm,
        "package StaleModule;\nour @EXPORT_OK = qw(old);\nsub old {}\n1;\n",
    )
    .unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = Some(parse_source_to_cached(&source, &pm));
    save_to_db(&conn, "StaleModule", &cached, "import");

    std::thread::sleep(std::time::Duration::from_secs(1));
    std::fs::write(
        &pm,
        "package StaleModule;\nour @EXPORT_OK = qw(v2 with more content);\n1;\n",
    )
    .unwrap();

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 0, "stale entry should not be loaded");
    assert!(!cache.contains_key("StaleModule"));

    let _ = std::fs::remove_file(&pm);
}

#[test]
fn test_db_plugin_fingerprint_invalidation() {
    let conn = test_db();

    // First run: claims plugin set fingerprint "hash-A".
    validate_plugin_fingerprint(&conn, "hash-A").unwrap();
    save_to_db(&conn, "Foo", &None, "import");

    // Same fingerprint → cache survives.
    validate_plugin_fingerprint(&conn, "hash-A").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 1, "cache should survive identical fingerprint");

    // Plugin set changed → cache cleared.
    validate_plugin_fingerprint(&conn, "hash-B").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 0, "cache should be empty after plugin set change");

    // Stamp persists — second run with hash-B doesn't re-clear.
    save_to_db(&conn, "Bar", &None, "import");
    validate_plugin_fingerprint(&conn, "hash-B").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 1, "stamp should persist between same-fingerprint runs");
}

#[test]
fn test_db_inc_hash_invalidation() {
    let conn = test_db();
    let paths1 = vec![PathBuf::from("/usr/lib/perl5")];
    let paths2 = vec![
        PathBuf::from("/usr/lib/perl5"),
        PathBuf::from("/home/user/lib"),
    ];

    validate_inc_paths(&conn, &paths1).unwrap();
    save_to_db(&conn, "Foo", &None, "import");

    validate_inc_paths(&conn, &paths2).unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 0, "cache should be empty after @INC change");
}

#[test]
fn test_db_schema_version_migration() {
    let conn = test_db();

    conn.execute(
        "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    save_to_db(&conn, "OldModule", &None, "import");

    init_schema(&conn).unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 0, "old data should be gone after migration");
}

#[test]
fn test_db_source_column() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("SourceTest_v9.pm");
    std::fs::write(
        &pm,
        "package SourceTest;\nour @EXPORT_OK = qw(foo);\nsub foo {}\n1;\n",
    )
    .unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = Some(parse_source_to_cached(&source, &pm));
    save_to_db(&conn, "SourceTest", &cached, "cpanfile");

    let source_val: String = conn
        .query_row(
            "SELECT source FROM modules WHERE module_name = 'SourceTest'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_val, "cpanfile");

    let _ = std::fs::remove_file(&pm);
}

#[test]
fn test_workspace_cache_dir_uniqueness() {
    let d1 = cache_dir_for_workspace(Some("file:///home/user/project-a"));
    let d2 = cache_dir_for_workspace(Some("file:///home/user/project-b"));
    let d_none = cache_dir_for_workspace(None);
    assert_ne!(d1, d2, "Different roots should produce different paths");
    assert_ne!(d1, d_none, "Root vs no-root should differ");
    assert_eq!(
        d1,
        cache_dir_for_workspace(Some("file:///home/user/project-a")),
        "Same root should produce same path"
    );
}

#[test]
fn test_full_file_analysis_survives_roundtrip() {
    // Verify that FileAnalysis fields lost in the old ModuleExports representation
    // (refs, type_constraints, call_bindings, full package_parents) now survive.
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("Fidelity_v9.pm");
    std::fs::write(
            &pm,
            "package Fidelity;\nuse parent 'Base';\nour @EXPORT_OK = qw(make);\nsub make { return { host => 1, port => 2 } }\n1;\n",
        )
        .unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = parse_source_to_cached(&source, &pm);
    let original_refs_count = cached.analysis.refs.len();
    let original_package_parents = cached.analysis.package_parents.clone();
    save_to_db(&conn, "Fidelity", &Some(Arc::clone(&cached)), "import");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 1);

    let loaded = cache.get("Fidelity").unwrap();
    let loaded = loaded.as_ref().unwrap();
    assert_eq!(
        loaded.analysis.refs.len(),
        original_refs_count,
        "refs survive roundtrip"
    );
    assert_eq!(
        loaded.analysis.package_parents, original_package_parents,
        "package_parents survive"
    );

    let _ = std::fs::remove_file(&pm);
}

/// M1: two same-length writes within the same whole second must still
/// invalidate the row — the stamp is nanosecond-mtime + size, not whole
/// seconds. Retries until both writes land in one second so the assertion
/// exercises exactly the old failure window.
#[test]
fn same_second_same_size_rewrite_invalidates_row() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("SubSecond_m1.pm");
    let secs = |t: std::time::SystemTime| {
        t.duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs()
    };
    for _ in 0..20 {
        std::fs::write(&pm, "package SubSecond;\nsub a { 1 }\n1;\n").unwrap();
        let s1 = std::fs::metadata(&pm).unwrap().modified().unwrap();
        let source = std::fs::read_to_string(&pm).unwrap();
        let cached = Some(parse_source_to_cached(&source, &pm));
        save_to_db(&conn, "SubSecond", &cached, "import");
        // Same byte length, different content.
        std::fs::write(&pm, "package SubSecond;\nsub b { 2 }\n1;\n").unwrap();
        let s2 = std::fs::metadata(&pm).unwrap().modified().unwrap();
        if secs(s1) == secs(s2) {
            let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
            let (n, _) = warm_cache(&conn, &cache);
            assert_eq!(n, 0, "same-second same-size rewrite must invalidate the row");
            let _ = std::fs::remove_file(&pm);
            return;
        }
        conn.execute("DELETE FROM modules", []).unwrap();
    }
    panic!("could not land both writes in one second");
}

/// M2: a consumer row is valid only while its whole include closure is
/// unchanged — its OWN (stamp, size) can't see a header edit, the
/// deps_stamp must.
#[test]
fn header_change_invalidates_consumer_row_via_deps_stamp() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let hdr = dir.join("dep_hdr_m2.h");
    std::fs::write(&hdr, "#define LIMIT 5\n").unwrap();
    let hdr_canon = hdr.canonicalize().unwrap().to_string_lossy().into_owned();
    let pm = dir.join("dep_consumer_m2.pm");
    std::fs::write(&pm, "package Consumer;\n1;\n").unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(&source, None).unwrap();
    let mut fa = crate::builder::build(&tree, source.as_bytes());
    fa.include_closure = vec![hdr_canon];
    let cached = Some(Arc::new(CachedModule::new(pm.clone(), Arc::new(fa))));
    save_to_db(&conn, "Consumer", &cached, "workspace");

    // Unchanged closure → row warms.
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 1, "row valid while the closure is unchanged");

    // Header changes; the consumer file itself does not.
    std::fs::write(&hdr, "#define LIMIT 5\n#define LIMIT2 7\n").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 0, "header edit must invalidate the consumer's row");

    let _ = std::fs::remove_file(&pm);
    let _ = std::fs::remove_file(&hdr);
}

/// H8: a degraded analysis (parse/extract failure, skipped gather) must
/// never be persisted — the row would validate on the source stamp alone
/// and re-serve the degraded blob every future session.
#[test]
fn degraded_analysis_is_not_persisted() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("Degraded_h8.pm");
    std::fs::write(&pm, "package Degraded;\n1;\n").unwrap();

    let source = std::fs::read_to_string(&pm).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(&source, None).unwrap();
    let mut fa = crate::builder::build(&tree, source.as_bytes());
    fa.degraded = true;
    let cached = Some(Arc::new(CachedModule::new(pm.clone(), Arc::new(fa))));
    save_to_db(&conn, "Degraded", &cached, "workspace");

    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM modules", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "degraded analyses must not reach the persist tier");
    let _ = std::fs::remove_file(&pm);
}

/// H8: the analysis-input fingerprint (toolchain identity, including its
/// probe FAILURE) hard-clears the table on change — a generation built
/// under degraded/different inputs is never warmed under the current ones.
#[test]
fn input_fingerprint_change_clears_table() {
    let conn = test_db();
    validate_input_fingerprint(&conn, 0xA).unwrap();
    save_to_db(&conn, "Foo", &None, "workspace");

    validate_input_fingerprint(&conn, 0xA).unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 1, "same inputs: cache survives");

    validate_input_fingerprint(&conn, 0xB).unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache);
    assert_eq!(n, 0, "changed inputs: table cleared");
}
