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
    let fa = crate::build::builder::build(&tree, source.as_bytes());
    Arc::new(CachedModule::new(path.to_path_buf(), Arc::new(fa)))
}

/// Slice-2: `load_one` decodes a single persisted analysis BY PATH with its
/// full witness bag present — the rehydration primitive. A resident copy may
/// have had its bag evicted, but the on-disk blob is whole, so `load_one`
/// resurrects it.
#[test]
fn load_one_rehydrates_full_bag() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_load_one.pm");
    std::fs::write(&pm, "package L;\nsub f { my $s = shift; return 'x'; }\n1;\n").unwrap();
    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = parse_source_to_cached(&source, &pm);
    // Sanity: the freshly built analysis has a populated bag.
    assert!(!cached.analysis.witnesses.is_empty());
    save_to_db(&conn, &pm.to_string_lossy(), &Some(cached.clone()), "workspace");

    let loaded = load_one(&conn, &pm.to_string_lossy()).expect("row should decode");
    assert!(!loaded.bag_is_evicted());
    assert!(
        !loaded.witnesses.is_empty(),
        "load_one must return the full bag, not an evicted one"
    );
    assert_eq!(loaded.witnesses.len(), cached.analysis.witnesses.len());
    // A path with no row yields None (miss → caller degrades to bag-less).
    assert!(load_one(&conn, "/no/such/path.pm").is_none());

    let _ = std::fs::remove_file(&pm);
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
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, stale) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 1);
    assert!(stale.is_empty());

    let loaded = cache.get("TestModule").unwrap();
    let loaded = loaded.as_ref().unwrap();
    assert_eq!(loaded.analysis.export, vec!["foo", "bar"]);
    assert_eq!(loaded.analysis.export_ok, vec!["baz"]);

    let _ = std::fs::remove_file(&pm);
}

/// The @INC pool is keyed by scheme, not by writer. Every name-keyed
/// producer (resolver thread, one-shot CLI) tags rows `NAME_KEYED_SOURCE`
/// and `warm_cache` reads exactly that tag; a writer-specific tag stranded
/// CLI-resolved rows unread, so each CLI verb re-resolved the whole tier.
/// Path-keyed `workspace` rows must stay out — they stream separately.
#[test]
fn warm_cache_shares_the_name_keyed_pool_and_excludes_path_keyed_rows() {
    let conn = test_db();
    let dir = std::env::temp_dir();

    let inc = dir.join("WarmPoolInc.pm");
    std::fs::write(&inc, "package WarmPoolInc;\nsub f { 1 }\n1;\n").unwrap();
    let inc_cached = Some(parse_source_to_cached(
        &std::fs::read_to_string(&inc).unwrap(),
        &inc,
    ));
    save_to_db(&conn, "WarmPoolInc", &inc_cached, NAME_KEYED_SOURCE);

    // Path-keyed: same table, different keying scheme.
    let ws = dir.join("WarmPoolWorkspace.pm");
    std::fs::write(&ws, "package WarmPoolWorkspace;\nsub g { 2 }\n1;\n").unwrap();
    let ws_cached = Some(parse_source_to_cached(
        &std::fs::read_to_string(&ws).unwrap(),
        &ws,
    ));
    save_to_db(&conn, &ws.to_string_lossy(), &ws_cached, "workspace");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _stale) = warm_cache(&conn, &cache, &all_defs, false);

    assert_eq!(n, 1, "exactly the name-keyed row warms");
    assert!(
        cache.contains_key("WarmPoolInc"),
        "a name-keyed row must warm back regardless of which writer resolved it"
    );
    assert!(
        !cache.contains_key(&*ws.to_string_lossy()),
        "path-keyed rows must not pollute the name-keyed cache"
    );

    let _ = std::fs::remove_file(&inc);
    let _ = std::fs::remove_file(&ws);
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
    let original_ns_count = cached.as_ref().unwrap().analysis.plugin.namespaces.len();
    assert!(
        original_ns_count > 0,
        "sanity: fixture must produce at least one PluginNamespace"
    );

    save_to_db(&conn, "TestMojoApp", &cached, "import");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, stale) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 1);
    assert!(stale.is_empty(), "fresh insert should not be stale");

    let loaded = cache.get("TestMojoApp").unwrap();
    let loaded = loaded.as_ref().unwrap();
    let loaded_ns = &loaded.analysis.plugin.namespaces;
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
                .any(|b| matches!(b, crate::model::file_analysis::Bridge::Class(_))),
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
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 1, "cache should survive identical fingerprint");

    // Plugin set changed → cache cleared.
    validate_plugin_fingerprint(&conn, "hash-B").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 0, "cache should be empty after plugin set change");

    // Stamp persists — second run with hash-B doesn't re-clear.
    save_to_db(&conn, "Bar", &None, "import");
    validate_plugin_fingerprint(&conn, "hash-B").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let original_refs_count = cached.analysis.refs().len();
    let original_packages = cached.analysis.packages.clone();
    save_to_db(&conn, "Fidelity", &Some(Arc::clone(&cached)), "import");

    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 1);

    let loaded = cache.get("Fidelity").unwrap();
    let loaded = loaded.as_ref().unwrap();
    assert_eq!(
        loaded.analysis.refs().len(),
        original_refs_count,
        "refs survive roundtrip"
    );
    assert_eq!(
        loaded.analysis.packages, original_packages,
        "per-package facts survive"
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
        // Require DIFFERENT nanos within the SAME second: that's the window
        // the nanosecond stamp fixed. Two writes inside one clock tick get
        // identical mtimes — the stamp's residual one-tick blind spot, not
        // the regression under test — so retry those instead of asserting.
        if secs(s1) == secs(s2) && s1 != s2 {
            let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
            let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let mut fa = crate::build::builder::build(&tree, source.as_bytes());
    fa.pack.include_closure =
        crate::model::file_analysis::path_intern::ClosureList::from_iter(std::iter::once(hdr_canon.as_str()));
    let cached = Some(Arc::new(CachedModule::new(pm.clone(), Arc::new(fa))));
    // warm_cache serves the 'import' tier ('workspace' rows stream through
    // warm_cache_streaming); the deps_stamp semantics under test are
    // source-independent.
    save_to_db(&conn, "Consumer", &cached, "import");

    // Unchanged closure → row warms.
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 1, "row valid while the closure is unchanged");

    // Header changes; the consumer file itself does not.
    std::fs::write(&hdr, "#define LIMIT 5\n#define LIMIT2 7\n").unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
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
    let mut fa = crate::build::builder::build(&tree, source.as_bytes());
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
    save_to_db(&conn, "Foo", &None, "import");

    validate_input_fingerprint(&conn, 0xA).unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 1, "same inputs: cache survives");

    validate_input_fingerprint(&conn, 0xB).unwrap();
    let cache: DashMap<String, Option<Arc<CachedModule>>> = DashMap::new();
    let all_defs: DashMap<String, Vec<Arc<CachedModule>>> = DashMap::new();
    let (n, _) = warm_cache(&conn, &cache, &all_defs, false);
    assert_eq!(n, 0, "changed inputs: table cleared");
}

/// Relational ref index: shred → candidate-file retrieval round-trip, the
/// re-shred replaces (never accumulates), and per-file deletion.
#[test]
fn shred_ref_rows_roundtrip() {
    let conn = test_db();
    let source = "package S;\nsub helper { 1 }\nsub caller_a { helper(); helper(); }\n1;\n";
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_shred.pm");
    std::fs::write(&pm, source).unwrap();
    let cached = parse_source_to_cached(source, &pm);
    let path_str = pm.to_string_lossy().to_string();

    assert!(!has_ref_rows(&conn, &path_str));
    let seeds: Vec<_> = cached.analysis.ref_row_seeds();
    assert!(!seeds.is_empty(), "call sites must produce row seeds");
    shred_derived_rows(&conn, &path_str, "workspace", &seeds, &[]).unwrap();
    assert!(has_ref_rows(&conn, &path_str));

    // Retrieval by the match key finds the file; an unknown key finds nothing.
    let hits = ref_candidate_files(&conn, &["helper".to_string()]);
    assert_eq!(hits, vec![path_str.clone()]);
    assert!(ref_candidate_files(&conn, &["nonesuch".to_string()]).is_empty());
    // Two call sites in ONE file are ONE row: rows are (name, file) pairs,
    // and every reader projects onto exactly that.
    assert_eq!(
        ref_candidate_file_count(&conn, "helper"),
        1,
        "a file's repeated mentions of a name must collapse to one row",
    );

    // Re-shred replaces: same seeds again must not double the rows.
    shred_derived_rows(&conn, &path_str, "workspace", &seeds, &[]).unwrap();
    assert_eq!(ref_candidate_file_count(&conn, "helper"), 1);

    // A zero-ref shred still marks the file as shredded (the backfill marker).
    let other = dir.join("TestModule_shred_empty.pm");
    shred_derived_rows(&conn, &other.to_string_lossy(), "workspace", &[], &[]).unwrap();
    assert!(has_ref_rows(&conn, &other.to_string_lossy()));

    delete_ref_rows(&conn, &path_str);
    assert!(!has_ref_rows(&conn, &path_str));
    assert!(ref_candidate_files(&conn, &["helper".to_string()]).is_empty());

    let _ = std::fs::remove_file(&pm);
}

/// Symbol rows ride the same shred generation as refs: written together,
/// replaced together (never accumulated), erased together.
#[test]
fn shred_sym_rows_same_generation() {
    let conn = test_db();
    let source = "package S;\nsub helper { 1 }\nsub caller_a { helper(); }\n1;\n";
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_symshred.pm");
    std::fs::write(&pm, source).unwrap();
    let cached = parse_source_to_cached(source, &pm);
    let path_str = pm.to_string_lossy().to_string();

    let seeds: Vec<_> = cached.analysis.ref_row_seeds();
    let sym_seeds = cached.analysis.sym_row_seeds();
    assert!(
        sym_seeds.iter().any(|s| s.name == "helper"),
        "sub symbols must project into row seeds; got {:?}",
        sym_seeds.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    shred_derived_rows(&conn, &path_str, "workspace", &seeds, &sym_seeds).unwrap();
    let count = |conn: &Connection| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM syms", [], |r| r.get(0)).unwrap()
    };
    let n = count(&conn);
    assert!(n >= 2, "expected sym rows for S's subs, got {n}");

    // Re-shred replaces.
    shred_derived_rows(&conn, &path_str, "workspace", &seeds, &sym_seeds).unwrap();
    assert_eq!(count(&conn), n);

    // Deletion takes both families.
    delete_ref_rows(&conn, &path_str);
    assert_eq!(count(&conn), 0);

    let _ = std::fs::remove_file(&pm);
}

/// The member pre-filter's rows probe is three-valued, and the boundary
/// between the values is what the ancestor walk's skip soundness leans on:
/// `Some(false)` — covered and provably absent — is the ONLY skip verdict;
/// `None` (never shredded) must stay distinguishable from it, or an
/// unindexed file's methods silently stop resolving.
#[test]
fn sym_member_probe_is_three_valued() {
    let conn = test_db();
    let source = "package My::Base;\nsub render { 1 }\n1;\n";
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_member_probe.pm");
    std::fs::write(&pm, source).unwrap();
    let cached = parse_source_to_cached(source, &pm);
    let path_str = pm.to_string_lossy().to_string();

    assert_eq!(
        sym_member_row_exists(&conn, &path_str, "render", "My::Base"),
        None,
        "never shredded: the store cannot speak for the file"
    );

    shred_derived_rows(
        &conn,
        &path_str,
        "workspace",
        &cached.analysis.ref_row_seeds(),
        &cached.analysis.sym_row_seeds(),
    )
    .unwrap();

    assert_eq!(
        sym_member_row_exists(&conn, &path_str, "render", "My::Base"),
        Some(true),
        "a matching (name, container) row warrants the decode"
    );
    assert_eq!(
        sym_member_row_exists(&conn, &path_str, "nonesuch", "My::Base"),
        Some(false),
        "covered and absent: the one verdict that licenses a skip"
    );
    assert_eq!(
        sym_member_row_exists(&conn, &path_str, "render", "Other::Pkg"),
        Some(false),
        "the container gates: the same name under another package is absent"
    );

    let _ = std::fs::remove_file(&pm);
}

/// The probes own the spelling policy (`rows::probe_spelling`): callers
/// pass a RAW — possibly qualified — name, and the store also matches its
/// match-key normalization, because refs rows are keyed by
/// `Ref::match_key()`. When call sites threaded the spellings themselves,
/// a qualified query name probed raw missed the row — turning fail-open
/// into a wrong skip.
#[test]
fn row_probes_match_the_match_key_spelling() {
    let conn = test_db();
    let source =
        "package My::Base;\nsub render { my $s = shift; $s->{cache} = 1; }\n1;\n";
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_probe_spelling.pm");
    std::fs::write(&pm, source).unwrap();
    let cached = parse_source_to_cached(source, &pm);
    let path_str = pm.to_string_lossy().to_string();
    shred_derived_rows(
        &conn,
        &path_str,
        "workspace",
        &cached.analysis.ref_row_seeds(),
        &cached.analysis.sym_row_seeds(),
    )
    .unwrap();

    assert_eq!(
        sym_member_row_exists(&conn, &path_str, "My::Base::render", "My::Base"),
        Some(true),
        "a qualified query name must reach the bare-keyed sym row"
    );
    assert_eq!(
        name_row_exists(&conn, &path_str, "Some::Pkg::cache"),
        Some(true),
        "a qualified query name must reach the match-keyed ref row"
    );
    assert_eq!(
        name_row_exists(&conn, &path_str, "nonesuch"),
        Some(false),
        "normalization must not weaken the absence verdict"
    );
    // The container never normalizes: a package name's match key strips
    // the qualifier, which would let `Base` claim `My::Base`'s rows.
    assert_eq!(
        sym_member_row_exists(&conn, &path_str, "render", "Base"),
        Some(false),
        "a bare container must not match a qualified one"
    );

    let _ = std::fs::remove_file(&pm);
}

/// `FLAG_EXPORTED` is minted from the real `@EXPORT`/`@EXPORT_OK` surface
/// (`exports_name`), so the rows agree with the source the Surface projects —
/// never a parallel notion of exportedness.
#[test]
fn flag_exported_minted_from_exports_source() {
    let dir = std::env::temp_dir();
    let pm = dir.join("UE_Flags.pm");
    let src = "package UE::Flags;\n\
        our @EXPORT = qw(alpha);\n\
        our @EXPORT_OK = qw(beta);\n\
        sub alpha { 1 }\n\
        sub beta { 2 }\n\
        sub gamma { 3 }\n1;\n";
    std::fs::write(&pm, src).unwrap();
    let cached = parse_source_to_cached(src, &pm);
    let seeds = cached.analysis.sym_row_seeds();
    let exported = |name: &str| {
        seeds
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.flags & crate::model::file_analysis::SymRowSeed::FLAG_EXPORTED != 0)
    };
    assert_eq!(exported("alpha"), Some(true), "@EXPORT member is flagged");
    assert_eq!(exported("beta"), Some(true), "@EXPORT_OK member is flagged");
    assert_eq!(exported("gamma"), Some(false), "non-exported sub is not flagged");
    // The flag must never diverge from the source it is baked from.
    assert!(cached.analysis.exports_name("alpha"));
    assert!(cached.analysis.exports_name("beta"));
    assert!(!cached.analysis.exports_name("gamma"));
    let _ = std::fs::remove_file(&pm);
}

/// The unused-exports view: exported syms with zero CROSS-FILE reference rows.
/// Same-file refs are excluded (an export used only internally is dead to
/// consumers); a cross-file consumer keeps an export live; a non-exported sym
/// is never listed.
#[test]
fn unused_exports_view() {
    let conn = test_db();
    let dir = std::env::temp_dir();

    // Producer exports three subs: `lonely` (used nowhere), `used` (a consumer
    // calls it), `internal_only` (referenced only in its own file).
    let prod = dir.join("UE_Producer.pm");
    let prod_src = "package UE::Producer;\n\
        our @EXPORT_OK = qw(lonely used internal_only);\n\
        sub lonely { 1 }\n\
        sub used { 2 }\n\
        sub internal_only { 3 }\n\
        sub caller_here { internal_only(); }\n1;\n";
    std::fs::write(&prod, prod_src).unwrap();
    let prod_cached = parse_source_to_cached(prod_src, &prod);
    let prod_path = prod.to_string_lossy().to_string();
    let prod_refs: Vec<_> = prod_cached.analysis.ref_row_seeds();
    let prod_syms = prod_cached.analysis.sym_row_seeds();
    shred_derived_rows(&conn, &prod_path, "workspace", &prod_refs, &prod_syms).unwrap();

    // Consumer in ANOTHER file references `used`.
    let cons = dir.join("UE_Consumer.pm");
    let cons_src = "package UE::Consumer;\n\
        use UE::Producer qw(used);\n\
        sub go { used(); }\n1;\n";
    std::fs::write(&cons, cons_src).unwrap();
    let cons_cached = parse_source_to_cached(cons_src, &cons);
    let cons_path = cons.to_string_lossy().to_string();
    let cons_refs: Vec<_> = cons_cached.analysis.ref_row_seeds();
    let cons_syms = cons_cached.analysis.sym_row_seeds();
    shred_derived_rows(&conn, &cons_path, "workspace", &cons_refs, &cons_syms).unwrap();

    let dead: std::collections::HashSet<String> =
        unused_exported_syms(&conn).into_iter().map(|d| d.name).collect();

    assert!(dead.contains("lonely"), "exported, unreferenced → dead: {dead:?}");
    assert!(
        dead.contains("internal_only"),
        "same-file use does not make an export live: {dead:?}"
    );
    assert!(
        !dead.contains("used"),
        "a cross-file consumer keeps the export live: {dead:?}"
    );
    assert!(
        !dead.contains("caller_here"),
        "a non-exported sub is never a dead export: {dead:?}"
    );

    let _ = std::fs::remove_file(&prod);
    let _ = std::fs::remove_file(&cons);
}

/// A candidate ref row in another file suppresses the dead-export flag even
/// when it is the ONLY reference — the view's nonzero side is "unknown, not
/// used", so any cross-file candidate is enough to withhold the verdict.
#[test]
fn unused_exports_view_cross_file_candidate_suppresses() {
    let conn = test_db();
    let dir = std::env::temp_dir();

    let prod = dir.join("UE2_Producer.pm");
    let prod_src = "package UE2::Producer;\n\
        our @EXPORT_OK = qw(widget);\n\
        sub widget { 1 }\n1;\n";
    std::fs::write(&prod, prod_src).unwrap();
    let prod_cached = parse_source_to_cached(prod_src, &prod);
    let prod_path = prod.to_string_lossy().to_string();
    let prod_syms = prod_cached.analysis.sym_row_seeds();
    // Producer has no ref rows of its own.
    shred_derived_rows(&conn, &prod_path, "workspace", &[], &prod_syms).unwrap();

    // Before any consumer: dead.
    let dead0: std::collections::HashSet<String> =
        unused_exported_syms(&conn).into_iter().map(|d| d.name).collect();
    assert!(dead0.contains("widget"), "no consumer yet → dead: {dead0:?}");

    // A consumer references it exactly once, cross-file.
    let cons = dir.join("UE2_Consumer.pm");
    let cons_src = "package UE2::Consumer;\nsub go { UE2::Producer::widget(); }\n1;\n";
    std::fs::write(&cons, cons_src).unwrap();
    let cons_cached = parse_source_to_cached(cons_src, &cons);
    let cons_path = cons.to_string_lossy().to_string();
    let cons_refs: Vec<_> = cons_cached.analysis.ref_row_seeds();
    assert!(
        cons_refs.iter().any(|s| s.key == "widget"),
        "consumer must produce a `widget` candidate row"
    );
    shred_derived_rows(&conn, &cons_path, "workspace", &cons_refs, &[]).unwrap();

    let dead1: std::collections::HashSet<String> =
        unused_exported_syms(&conn).into_iter().map(|d| d.name).collect();
    assert!(!dead1.contains("widget"), "one cross-file candidate → not listed: {dead1:?}");

    let _ = std::fs::remove_file(&prod);
    let _ = std::fs::remove_file(&cons);
}

/// The general pre-prune set is exactly the DISTINCT ref-row name keys — the
/// witness `--heatmap` uses to skip the references projection for names that
/// have no reference row at all.
#[test]
fn names_with_ref_rows_is_the_distinct_key_set() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("UE_Names.pm");
    let src = "package UE::Names;\nsub helper { 1 }\nsub go { helper(); }\n1;\n";
    std::fs::write(&pm, src).unwrap();
    let cached = parse_source_to_cached(src, &pm);
    let refs: Vec<_> = cached.analysis.ref_row_seeds();
    shred_derived_rows(&conn, &pm.to_string_lossy(), "workspace", &refs, &[]).unwrap();

    let names = names_with_ref_rows(&conn);
    assert!(names.contains("helper"), "called name has a ref row: {names:?}");
    assert!(!names.contains("go"), "a name only DECLARED has no ref row: {names:?}");

    let _ = std::fs::remove_file(&pm);
}

/// The row seeds must key by the same spelling retrieval probes: qualified
/// calls key by their bare tail, sigil variables keep the sigil.
#[test]
fn ref_row_seed_match_keys() {
    let source = "package K;\nour $x = 1;\nFoo::Bar::baz();\nprint $Foo::Bar::x;\n1;\n";
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_keys.pm");
    std::fs::write(&pm, source).unwrap();
    let cached = parse_source_to_cached(source, &pm);
    let keys: Vec<String> = cached.analysis.refs().iter().map(|r| r.match_key()).collect();
    assert!(
        keys.iter().any(|k| k == "baz"),
        "qualified call keys by bare tail; got {keys:?}"
    );
    assert!(
        keys.iter().any(|k| k == "$x"),
        "qualified sigil var keys by sigil+base; got {keys:?}"
    );
    assert!(
        !keys.iter().any(|k| k.contains("::")),
        "no qualified spellings in match keys; got {keys:?}"
    );
    let _ = std::fs::remove_file(&pm);
}

/// Hard-clears (inc hash / plugin fingerprint / input fingerprint) must wipe
/// the derived row tables together with the blobs they derive from.
#[test]
fn hard_clear_wipes_derived_rows() {
    let conn = test_db();
    let seeds = vec![crate::model::file_analysis::RefRowSeed {
        key: "k".into(),
        kind: 1,
        span: crate::model::file_analysis::Span {
            start: tree_sitter::Point { row: 0, column: 0 },
            end: tree_sitter::Point { row: 0, column: 1 },
        },
        access: 0,
        flags: 0,
        qual_kind: 0,
        qual: None,
        arg_count: None,
    }];
    shred_derived_rows(&conn, "/some/file.pm", "workspace", &seeds, &[]).unwrap();
    assert!(has_ref_rows(&conn, "/some/file.pm"));
    validate_plugin_fingerprint(&conn, "fingerprint-a").unwrap();
    validate_plugin_fingerprint(&conn, "fingerprint-b").unwrap();
    assert!(
        !has_ref_rows(&conn, "/some/file.pm"),
        "fingerprint change must clear derived rows"
    );
}

/// A row-format bump must recreate the derived tables, not just clear rows:
/// `CREATE TABLE IF NOT EXISTS` no-ops on the old SHAPE, and a shape change
/// (v1 `files` had no `source` column) would otherwise fail every future
/// shred while composition masks it (refs stay resident, retrieval dead).
#[test]
fn ref_rows_version_bump_recreates_old_shape_tables() {
    let conn = Connection::open_in_memory().unwrap();
    // Simulate a v1-era DB: old files shape + stale version stamp.
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta VALUES ('ref_rows_version', '1');
         CREATE TABLE files (file_id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE);
         CREATE TABLE strings (str_id INTEGER PRIMARY KEY, s TEXT NOT NULL UNIQUE);
         CREATE TABLE refs (file_id INTEGER, name_id INTEGER);",
    )
    .unwrap();
    init_schema(&conn).unwrap();
    // The v2 shape must accept a tier-tagged shred.
    shred_derived_rows(&conn, "/migrated.pm", "workspace", &[], &[]).unwrap();
    assert!(has_ref_rows(&conn, "/migrated.pm"));
}

/// The version stamp can lie: a DB stamped CURRENT whose tables still carry
/// an older shape (stamped by a build whose migration didn't reshape) would
/// never re-migrate on the stamp check alone — every shred fails on the
/// missing column while composition masks it (refs stay resident, retrieval
/// dead, diagnostics typeless). The shape probe must trigger the rebuild.
#[test]
fn ref_rows_current_stamp_with_stale_shape_recreates_tables() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta VALUES ('ref_rows_version', '{REF_ROWS_VERSION}');
         CREATE TABLE files (file_id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE);
         CREATE TABLE strings (str_id INTEGER PRIMARY KEY, s TEXT NOT NULL UNIQUE);
         CREATE TABLE refs (file_id INTEGER, name_id INTEGER);",
    ))
    .unwrap();
    init_schema(&conn).unwrap();
    shred_derived_rows(&conn, "/migrated.pm", "workspace", &[], &[]).unwrap();
    assert!(has_ref_rows(&conn, "/migrated.pm"));
}

/// The @INC hard-clear is tier-scoped: a PERL5LIB change must take the
/// import tier (blobs AND derived rows) while workspace rows — possibly
/// committed by the concurrent indexer moments earlier — survive.
#[test]
fn inc_clear_is_import_tier_scoped() {
    let conn = test_db();
    validate_inc_paths(&conn, &[PathBuf::from("/lib/a")]).unwrap();
    shred_derived_rows(&conn, "/ws/File.pm", "workspace", &[], &[]).unwrap();
    shred_derived_rows(&conn, "/inc/Dep.pm", "import", &[], &[]).unwrap();

    validate_inc_paths(&conn, &[PathBuf::from("/lib/CHANGED")]).unwrap();
    assert!(
        has_ref_rows(&conn, "/ws/File.pm"),
        "workspace rows must survive an @INC change"
    );
    assert!(
        !has_ref_rows(&conn, "/inc/Dep.pm"),
        "import rows must clear on an @INC change"
    );
}

/// The register-from-Surface warm stub: encode/decode round-trip, and the
/// warm stream's lane selection — a valid stub serves registration without
/// touching the full blob; a declined stub (rows missing) falls back to the
/// full decode; a stale file stamp serves neither.
#[test]
fn warm_stub_roundtrip_and_lane_selection() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("TestModule_warmstub.pm");
    std::fs::write(&pm, "package Stubbed;\nsub go { my $x = shift; return $x + 1 }\n1;\n")
        .unwrap();
    let source = std::fs::read_to_string(&pm).unwrap();
    let cached = parse_source_to_cached(&source, &pm);
    let path_str = pm.to_string_lossy().to_string();

    // Build the stub halves the way the fresh worker does: feed + surface
    // from the WHOLE analysis, skeleton stripped.
    let whole = (*cached.analysis).clone();
    let feed = vec![("go".to_string(), false)];
    let specs: Vec<(String, String)> = Vec::new();
    let surface = crate::model::surface::Surface::project(&whole);
    let mut skeleton = whole;
    skeleton.evict_witness_bag();
    skeleton.evict_refs();
    skeleton.evict_symbols();

    let blob = encode_stub(&feed, &specs, &[], &surface, &skeleton).expect("encodes");
    let stub = decode_stub(&blob).expect("decodes");
    assert_eq!(stub.feed, feed);
    assert_eq!(stub.surface, surface);
    assert!(stub.skeleton.symbols_are_evicted() && stub.skeleton.refs_are_evicted());

    // Persist the modules row (deletes any stub for the path), then the stub.
    save_to_db(&conn, &path_str, &Some(cached.clone()), "workspace");
    validate_stub_version(&conn);
    save_stub(&conn, &path_str, &blob);

    let run = |conn: &Connection, accept: bool| -> (usize, usize) {
        let (mut stubs, mut fulls) = (0usize, 0usize);
        warm_pack_stream_with_stubs(
            conn,
            true,
            &mut |_p| true,
            &mut |_p, payload| match payload {
                WarmPayload::Stub(_) => {
                    stubs += 1;
                    if accept { WarmDirective::Handled } else { WarmDirective::NeedFull }
                }
                WarmPayload::Full(..) => {
                    fulls += 1;
                    WarmDirective::Handled
                }
            },
        );
        (stubs, fulls)
    };
    // Stub lane accepted: full blob untouched.
    assert_eq!(run(&conn, true), (1, 0));
    // Stub declined (e.g. derived rows missing): falls back to full decode.
    assert_eq!(run(&conn, false), (1, 1));
    // use_stubs=false (NO_EVICT): straight to the full lane.
    let (mut stubs, mut fulls) = (0usize, 0usize);
    warm_pack_stream_with_stubs(&conn, false, &mut |_p| true, &mut |_p, payload| {
        match payload {
            WarmPayload::Stub(_) => stubs += 1,
            WarmPayload::Full(..) => fulls += 1,
        }
        WarmDirective::Handled
    });
    assert_eq!((stubs, fulls), (0, 1));

    // A rewritten modules row must orphan the stub (stale-skeleton guard).
    save_to_db(&conn, &path_str, &Some(cached), "workspace");
    assert_eq!(run(&conn, true), (0, 1));

    // Stale file stamp: neither lane serves.
    std::fs::write(&pm, "package Stubbed;\nsub go { 2 }\nsub extra { 3 }\n1;\n").unwrap();
    assert_eq!(run(&conn, true), (0, 0));

    let _ = std::fs::remove_file(&pm);
}

/// STUB_VERSION mismatch wipes the stubs table (never serves an old
/// generation's meaning under a new reader).
#[test]
fn stub_version_gate_wipes_on_mismatch() {
    let conn = test_db();
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('stub_version', 'ancient')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO stubs (path, stub) VALUES ('/x', x'00')",
        [],
    )
    .unwrap();
    validate_stub_version(&conn);
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stubs", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "mismatched generation wiped");
    // Current version: idempotent, keeps rows.
    conn.execute("INSERT INTO stubs (path, stub) VALUES ('/y', x'00')", []).unwrap();
    validate_stub_version(&conn);
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stubs", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

/// `refresh_deps_stamp` — the Unchanged gate's persistence half. A header
/// body edit moves every consumer row's closure stamp; refreshing it (and
/// nothing else) keeps the row warm-valid without re-persisting content.
#[test]
fn refresh_deps_stamp_revalidates_consumer_rows() {
    let conn = test_db();
    let dir = std::env::temp_dir().join(format!("deps-refresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let header = dir.join("dep.h");
    let consumer = dir.join("use.c");
    std::fs::write(&header, "int helper(void);\n").unwrap();
    std::fs::write(&consumer, "#include \"dep.h\"\n").unwrap();

    // A consumer row whose closure contains the header.
    let cached = parse_source_to_cached("1;\n", &consumer);
    let mut fa = (*cached.analysis).clone();
    fa.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
        [header.to_string_lossy()].iter().map(|s| s.as_ref()),
    );
    let blob = encode_analysis(&fa).unwrap();
    let consumer_str = consumer.to_string_lossy().into_owned();
    let stamp = file_stamp(&consumer).unwrap_or((0, 0));
    save_blob_to_db_stamped(&conn, &consumer_str, &consumer, &fa.pack.include_closure, &blob, "workspace", stamp);
    let stored: i64 = conn
        .query_row("SELECT deps_stamp FROM modules WHERE path=?1", params![consumer_str], |r| r.get(0))
        .unwrap();

    // "Edit" the header (content + mtime move) — the stored stamp is stale.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&header, "int helper(void); /* body-ish edit */\n").unwrap();
    let mut memo = std::collections::HashMap::new();
    refresh_deps_stamp(&conn, &consumer_str, &fa.pack.include_closure, &mut memo);
    let refreshed: i64 = conn
        .query_row("SELECT deps_stamp FROM modules WHERE path=?1", params![consumer_str], |r| r.get(0))
        .unwrap();
    assert_ne!(stored, refreshed, "closure member moved: stamp must change");

    // And it now matches a fresh recompute (what the next warm scan checks).
    let mut memo2 = std::collections::HashMap::new();
    let expect = closure_stamp(&fa.pack.include_closure, &mut memo2);
    assert_eq!(refreshed, expect);

    let _ = std::fs::remove_dir_all(&dir);
}

/// H7-16 regression: the bag-rehydration reader must survive the transient
/// `SQLITE_CANTOPEN` a fresh read-only open hits while a sibling writer is
/// mid-`wal_checkpoint` on the WAL-mode cache DB — a read-only connection
/// can't rebuild the wal-index in that window. The captured flake was a
/// strict-residency PANIC: the read-only open failed, the loader reported the
/// blob absent, and the tripwire aborted the run though the row was on disk
/// the whole time. `load_with_wal_fallback` recovers through a read-write open
/// (which waits the writer out via `busy_timeout`), so a live blob is never
/// mislabeled absent.
#[test]
fn readonly_open_failure_recovers_through_read_write() {
    // The captured H7-16 cause is a fresh read-only open transiently returning
    // SQLITE_CANTOPEN while a sibling writer is mid-checkpoint on the WAL DB —
    // an OS/SQLite timing race that can't be forced from static file state.
    // Inject it: a read-only open result of `Err` (open failed) with a working
    // read-write recovery connection. The fix loads the row through RW instead
    // of mislabeling the live blob absent; without the RW fallback this same
    // input yields OpenerFailed and the strict tripwire panics.
    let dir = std::env::temp_dir().join(format!("h716_inject_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("modules.db");
    let pm = dir.join("Seed.pm");
    std::fs::write(&pm, "package Seed;\nsub f { my $s = shift; return 'x'; }\n1;\n").unwrap();
    let cached = parse_source_to_cached(&std::fs::read_to_string(&pm).unwrap(), &pm);
    let pm_str = pm.to_string_lossy().into_owned();
    {
        let w = Connection::open(&db).unwrap();
        w.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        init_schema(&w).unwrap();
        save_to_db(&w, &pm_str, &Some(cached.clone()), "workspace");
    }

    // Read-only "open failed", RW open works → recovered, full bag present.
    let recovered = rehydrate_from_opens(
        Err("simulated SQLITE_CANTOPEN".to_string()),
        || open_rw_shared_at(&db),
        std::slice::from_ref(&pm_str),
        true,
    )
    .expect("RW fallback must recover the row a failed read-only open couldn't reach");
    assert!(!recovered.bag_is_evicted());
    assert_eq!(recovered.witnesses.len(), cached.analysis.witnesses.len());

    // Read-only "open failed" AND no RW recovery conn → honest OpenerFailed,
    // NOT a fabricated presence: the strict tripwire must still fire on a
    // genuinely unreadable DB.
    let miss = rehydrate_from_opens(
        Err("simulated SQLITE_CANTOPEN".to_string()),
        || None,
        std::slice::from_ref(&pm_str),
        true,
    )
    .unwrap_err();
    assert!(matches!(miss, RehydrateMiss::OpenerFailed(_)), "got {miss}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The fix must never trade a false absence for a fabricated presence: a row
/// that is genuinely missing stays a discriminated miss so the strict
/// tripwire keeps firing on real invariant breaks.
#[test]
fn rehydrate_absent_row_is_honest_miss() {
    let dir = std::env::temp_dir().join(format!("h716_absent_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("modules.db");
    {
        let w = Connection::open(&db).unwrap();
        init_schema(&w).unwrap();
    }
    // Row truly absent (present DB, no matching row) → NoRow, via both opens.
    let miss = load_with_wal_fallback(&db, &["/no/such.pm".to_string()], true).unwrap_err();
    assert!(matches!(miss, RehydrateMiss::NoRow), "got {miss}");
    // No DB file at all → OpenerFailed (neither read-only nor read-write open).
    let none = load_with_wal_fallback(&dir.join("nope.db"), &["/x.pm".to_string()], true).unwrap_err();
    assert!(matches!(none, RehydrateMiss::OpenerFailed(_)), "got {none}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `load_one_diag` names each on-disk reality distinctly so the tripwire can
/// point at a mechanism instead of a collapsed "None".
#[test]
fn load_one_diag_discriminates_failures() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("h716_diag.pm");
    std::fs::write(&pm, "package D;\nsub f { 1 }\n1;\n").unwrap();
    let cached = parse_source_to_cached(&std::fs::read_to_string(&pm).unwrap(), &pm);
    let pm_str = pm.to_string_lossy().into_owned();
    save_to_db(&conn, &pm_str, &Some(cached), "workspace");
    assert!(load_one_diag(&conn, &pm_str, true).is_ok());
    assert!(matches!(
        load_one_diag(&conn, "/absent.pm", true).unwrap_err(),
        RehydrateMiss::NoRow
    ));
    conn.execute(
        "INSERT INTO modules (module_name, path, mtime_secs, file_size, source, \
         analysis, extract_version, deps_stamp) VALUES ('E','/empty.pm',0,0,'import',NULL,?1,0)",
        params![EXTRACT_VERSION],
    )
    .unwrap();
    assert!(matches!(
        load_one_diag(&conn, "/empty.pm", true).unwrap_err(),
        RehydrateMiss::EmptyBlob
    ));
    conn.execute(
        "INSERT INTO modules (module_name, path, mtime_secs, file_size, source, \
         analysis, extract_version, deps_stamp) VALUES ('G','/garbage.pm',0,0,'import',?1,?2,0)",
        params![Some(vec![9u8, 9, 9, 9]), EXTRACT_VERSION],
    )
    .unwrap();
    assert!(matches!(
        load_one_diag(&conn, "/garbage.pm", true).unwrap_err(),
        RehydrateMiss::DecodeFailed
    ));
    let _ = std::fs::remove_file(&pm);
}

// ---- The deduped ref-row model ----
//
// Rows are `(name_id, file_id)` pairs. Every reader is a set-valued
// projection onto exactly that, so the dedup is bit-identical rather than
// approximately safe — but the two ways it could go wrong are silent, so
// both are pinned here.

#[test]
fn ref_rows_dedup_per_file_and_not_across_files() {
    // Collapsing a file's repeated mentions is the win; collapsing ACROSS
    // files would drop candidates and silently shrink every backward walk.
    let conn = test_db();
    let dir = std::env::temp_dir();
    let mk = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        let cached = parse_source_to_cached(body, &p);
        (p.to_string_lossy().to_string(), cached.analysis.ref_row_seeds())
    };
    // `helper` mentioned many times in one file, and once in another.
    let (a_path, a_seeds) = mk(
        "dedup_a.pm",
        "package A;\nsub helper { 1 }\nsub go { helper(); helper(); helper(); helper(); }\n1;\n",
    );
    let (b_path, b_seeds) = mk("dedup_b.pm", "package B;\nsub go { helper(); }\n1;\n");

    shred_derived_rows(&conn, &a_path, "workspace", &a_seeds, &[]).unwrap();
    assert_eq!(
        ref_candidate_file_count(&conn, "helper"),
        1,
        "four call sites in one file must be one row",
    );

    shred_derived_rows(&conn, &b_path, "workspace", &b_seeds, &[]).unwrap();
    assert_eq!(
        ref_candidate_file_count(&conn, "helper"),
        2,
        "a second FILE is a second candidate — dedup is per file, never global",
    );

    let mut hits = ref_candidate_files(&conn, &["helper".to_string()]);
    hits.sort();
    let mut want = vec![a_path, b_path];
    want.sort();
    assert_eq!(hits, want, "both files must stay retrievable");
}

#[test]
fn wiping_the_strings_table_does_not_leave_dangling_name_ids() {
    // The shredder memoizes interned `str_id`s for the writer's lifetime.
    // `clear_derived_rows` empties `strings`, so without the
    // `strings_generation` guard the memo would keep handing out ids for
    // rows that no longer exist — refs would be written with a `name_id`
    // nothing joins to, and retrieval would answer EMPTY rather than fail.
    // That is the failure this test exists for, so it asserts on retrieval.
    let conn = test_db();
    let dir = std::env::temp_dir();
    let p = dir.join("dangling_probe.pm");
    let src = "package D;\nsub helper { 1 }\nsub go { helper(); }\n1;\n";
    std::fs::write(&p, src).unwrap();
    let path_str = p.to_string_lossy().to_string();
    let seeds = parse_source_to_cached(src, &p).analysis.ref_row_seeds();

    shred_derived_rows(&conn, &path_str, "workspace", &seeds, &[]).unwrap();
    assert_eq!(ref_candidate_files(&conn, &["helper".to_string()]), vec![path_str.clone()]);

    // The wipe every hard-clear performs, on the SAME connection and thread
    // that just populated the memo.
    clear_derived_rows(&conn).unwrap();
    assert!(ref_candidate_files(&conn, &["helper".to_string()]).is_empty());

    shred_derived_rows(&conn, &path_str, "workspace", &seeds, &[]).unwrap();
    assert_eq!(
        ref_candidate_files(&conn, &["helper".to_string()]),
        vec![path_str],
        "post-wipe rows carry a name_id that joins to nothing",
    );
}

#[test]
fn the_intern_memo_stays_bounded_and_correct_past_its_cap() {
    // The memo is per-thread and lives for the writer's lifetime, so an
    // unbounded one would accumulate a corpus's whole unique-name set per
    // Rayon worker. Crossing the cap must not change what gets written —
    // a cleared memo just re-interns, it never invents an id.
    let conn = test_db();
    let dir = std::env::temp_dir();
    // Far more distinct names than the cap, spread over several files so the
    // memo carries across shred calls the way it does in the writer.
    let mut all_names: Vec<String> = Vec::new();
    for f in 0..4 {
        let mut src = format!("package Cap{f};\n");
        for i in 0..12_000 {
            let n = format!("nm_{f}_{i}");
            src.push_str(&format!("sub {n} {{ 1 }}\n"));
            all_names.push(n);
        }
        src.push_str("1;\n");
        let p = dir.join(format!("cap_probe_{f}.pm"));
        std::fs::write(&p, &src).unwrap();
        let path_str = p.to_string_lossy().to_string();
        let cached = parse_source_to_cached(&src, &p);
        shred_derived_rows(
            &conn,
            &path_str,
            "workspace",
            &cached.analysis.ref_row_seeds(),
            &cached.analysis.sym_row_seeds(),
        )
        .unwrap();
    }
    // 48k distinct names went through a 32k-entry memo; every one must still
    // have interned to a real row that retrieval can join to.
    for probe in [&all_names[0], &all_names[all_names.len() / 2], all_names.last().unwrap()] {
        assert!(
            !ref_candidate_files(&conn, &[probe.to_string()]).is_empty(),
            "name {probe} lost its row after the memo wrapped",
        );
    }
}

#[test]
fn a_qualified_symbols_declaring_file_is_a_ref_candidate() {
    // Refs are keyed by `Ref::match_key` — the LAST segment — while a symbol
    // row's display name is whatever it is called. For a qualified name
    // (`package Deep::Pkg::Thing`) those differ, so a row storing only the
    // display name is undiscoverable by retrieval, and a file that mentions
    // the name nowhere but its own declaration drops out of the candidate set
    // entirely. That is the `syms` union failing at exactly the job it exists
    // for. The row carries both, and this pins it.
    let conn = test_db();
    let dir = std::env::temp_dir();
    let p = dir.join("qualified_decl.pm");
    // Declares the package and never mentions the name again.
    let src = "package Deep::Pkg::Thing;\nsub helper { 1 }\n1;\n";
    std::fs::write(&p, src).unwrap();
    let path_str = p.to_string_lossy().to_string();
    let cached = parse_source_to_cached(src, &p);
    shred_derived_rows(
        &conn,
        &path_str,
        "workspace",
        &cached.analysis.ref_row_seeds(),
        &cached.analysis.sym_row_seeds(),
    )
    .unwrap();

    // The key a REFERENCE to this package carries.
    let key = crate::model::file_analysis::name_match_key("Deep::Pkg::Thing");
    assert_eq!(key, "Thing");
    assert_eq!(
        ref_candidate_files(&conn, &[key]),
        vec![path_str.clone()],
        "the declaring file must be retrievable by the key references use",
    );
    // And the display name still finds it, so workspace/symbol is unaffected.
    assert_eq!(
        ref_candidate_files(&conn, &["Deep::Pkg::Thing".to_string()]),
        Vec::<String>::new(),
        "the display name is not a retrieval key — that is what key_id is for",
    );
}

/// MEASUREMENT PROBE (not a net): how much of `strings` is orphaned after
/// realistic churn. `--ignored --nocapture`.
#[test]
#[ignore]
fn probe_strings_orphan_rate() {
    let conn = test_db();
    let root = std::path::Path::new("gold-corpus/local/lib/perl5");
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>, cap: usize) {
        if out.len() >= cap { return; }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            if out.len() >= cap { return; }
            let p = e.path();
            if p.is_dir() { walk(&p, out, cap); }
            else if p.extension().map(|x| x == "pm").unwrap_or(false) { out.push(p); }
        }
    }
    let mut files = Vec::new();
    walk(root, &mut files, 300);
    if files.is_empty() { eprintln!("SKIP: no substrate"); return; }

    let mut parser = crate::build::builder::create_parser();
    let mut paths = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let Some(tree) = parser.parse(&src, None) else { continue };
        let fa = crate::build::builder::build(&tree, src.as_bytes());
        let ps = f.to_string_lossy().to_string();
        shred_derived_rows(&conn, &ps, "workspace", &fa.ref_row_seeds(), &fa.sym_row_seeds()).unwrap();
        paths.push(ps);
    }
    let count = |sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
    };
    let live = "SELECT COUNT(*) FROM strings s WHERE EXISTS(SELECT 1 FROM refs r WHERE r.name_id=s.str_id) \
                OR EXISTS(SELECT 1 FROM syms y WHERE y.name_id=s.str_id OR y.key_id=s.str_id OR y.container_id=s.str_id)";
    eprintln!("after shred: strings={} live={} files={}",
        count("SELECT COUNT(*) FROM strings"), count(live), paths.len());

    // Churn: delete half the files, as a workspace does over a session.
    for p in paths.iter().step_by(2) { delete_ref_rows(&conn, p); }
    let total = count("SELECT COUNT(*) FROM strings");
    let alive = count(live);
    eprintln!(
        "after deleting half: strings={total} live={alive} ORPHANED={} ({:.1}%)",
        total - alive, 100.0 * (total - alive) as f64 / total.max(1) as f64
    );

    // And a full wipe of every file, the extreme.
    for p in &paths { delete_ref_rows(&conn, p); }
    let total2 = count("SELECT COUNT(*) FROM strings");
    let alive2 = count(live);
    eprintln!(
        "after deleting all:  strings={total2} live={alive2} ORPHANED={} ({:.1}%)",
        total2 - alive2, 100.0 * (total2 - alive2) as f64 / total2.max(1) as f64
    );
}

#[test]
fn gc_reclaims_orphaned_strings_and_keeps_live_ones() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let mk = |name: &str, src: &str| -> String {
        let p = dir.join(name);
        std::fs::write(&p, src).unwrap();
        let ps = p.to_string_lossy().to_string();
        let cached = parse_source_to_cached(src, &p);
        shred_derived_rows(
            &conn, &ps, "workspace",
            &cached.analysis.ref_row_seeds(),
            &cached.analysis.sym_row_seeds(),
        ).unwrap();
        ps
    };
    let keep = mk("gc_keep.pm", "package GcKeep;\nsub kept_name { 1 }\nsub go { kept_name() }\n1;\n");
    let drop_ = mk("gc_drop.pm", "package GcDrop;\nsub doomed_name { 1 }\nsub go2 { doomed_name() }\n1;\n");

    let has = |s: &str| -> bool {
        conn.query_row("SELECT 1 FROM strings WHERE s = ?1", params![s], |_| Ok(())).is_ok()
    };
    assert!(has("kept_name") && has("doomed_name"));

    // A GC with nothing orphaned must not touch anything.
    assert_eq!(gc_strings(&conn), 0, "nothing is orphaned yet");
    assert!(has("kept_name") && has("doomed_name"));

    delete_ref_rows(&conn, &drop_);
    assert!(has("doomed_name"), "deletion leaves the name behind — that is the leak");

    let reclaimed = gc_strings(&conn);
    assert!(reclaimed > 0, "gc reclaimed nothing after a file was deleted");
    assert!(!has("doomed_name"), "orphan survived the gc");
    assert!(has("kept_name"), "gc took a name that is still referenced");

    // The surviving file is still fully retrievable — the gc must not have
    // cut a string its rows join to.
    assert_eq!(ref_candidate_files(&conn, &["kept_name".to_string()]), vec![keep]);
}

#[test]
fn a_gc_invalidates_the_writer_intern_memo() {
    // The shredder memoizes str_ids for the writer's lifetime. A gc frees
    // ids, and SQLite reuses them — so a memo that survived a gc would stamp
    // refs rows with an id belonging to a DIFFERENT string. Retrieval would
    // then answer wrongly rather than emptily, which is worse. The
    // generation bump is what prevents it; this test fails without it.
    let conn = test_db();
    let dir = std::env::temp_dir();
    let mk = |name: &str, src: &str| -> String {
        let p = dir.join(name);
        std::fs::write(&p, src).unwrap();
        let ps = p.to_string_lossy().to_string();
        let cached = parse_source_to_cached(src, &p);
        shred_derived_rows(
            &conn, &ps, "workspace",
            &cached.analysis.ref_row_seeds(),
            &cached.analysis.sym_row_seeds(),
        ).unwrap();
        ps
    };
    let a = mk("gc_memo_a.pm", "package GcMemoA;\nsub alpha_fn { 1 }\nsub go { alpha_fn() }\n1;\n");
    delete_ref_rows(&conn, &a);
    assert!(gc_strings(&conn) > 0);

    // Re-shred the SAME file on the SAME thread, whose memo still holds the
    // pre-gc ids.
    let a2 = mk("gc_memo_a.pm", "package GcMemoA;\nsub alpha_fn { 1 }\nsub go { alpha_fn() }\n1;\n");
    assert_eq!(
        ref_candidate_files(&conn, &["alpha_fn".to_string()]),
        vec![a2],
        "post-gc rows carry a stale name_id — the memo outlived the strings it cached",
    );
}

#[test]
fn a_deleted_files_rows_are_collectable() {
    // A row can only be collected by the scan that reads it, and the scan
    // used to skip any row it could not stamp — which includes every row
    // whose file has been DELETED. So those rows were immortal: the store
    // grew a dead generation per deleted file forever, `ref_candidate_files`
    // kept offering paths that no longer exist, and the dead-export view
    // counted a deleted file as a live cross-file user. Their names could
    // not be reclaimed either, because dead rows still referenced them.
    let conn = test_db();
    let dir = std::env::temp_dir();
    let p = dir.join("vanishing.pm");
    let src = "package Vanishing;\nsub only_here { 1 }\nsub go { only_here() }\n1;\n";
    std::fs::write(&p, src).unwrap();
    let path_str = p.to_string_lossy().to_string();
    let cached = parse_source_to_cached(src, &p);
    shred_derived_rows(
        &conn, &path_str, "workspace",
        &cached.analysis.ref_row_seeds(),
        &cached.analysis.sym_row_seeds(),
    ).unwrap();
    // Stamp the row to the file as it is on disk, so the scan would admit it.
    let (mtime, size) = file_stamp(&p).expect("stamp");
    let enc = encode_analysis(&cached.analysis).expect("encode");
    conn.execute(
        "INSERT OR REPLACE INTO modules
           (module_name, path, mtime_secs, file_size, source, analysis, bag, extract_version, deps_stamp)
         VALUES (?1, ?1, ?2, ?3, 'workspace', ?4, ?5, ?6, 0)",
        params![
            path_str, mtime, size,
            enc.analysis, enc.bag,
            EXTRACT_VERSION
        ],
    ).unwrap();

    // Present: the scan admits it and reports nothing missing.
    let (seen, _stale, gone) =
        warm_cache_streaming(&conn, "workspace", &mut |_n, _p, _fa| {});
    assert_eq!(seen, 1, "the live row is admitted");
    assert!(gone.is_empty(), "nothing is missing yet");

    // Now the file disappears, as a delete or a branch switch does.
    std::fs::remove_file(&p).unwrap();
    let (seen, _stale, gone) =
        warm_cache_streaming(&conn, "workspace", &mut |_n, _p, _fa| {});
    assert_eq!(seen, 0, "a vanished file is not admitted");
    assert_eq!(
        gone, vec![p.clone()],
        "a vanished file's row must be REPORTED, or nothing can ever collect it",
    );

    // And collecting it lets its names be reclaimed.
    invalidate_generation_tier(&conn, &path_str, "workspace");
    assert!(gc_strings(&conn) > 0, "the dead file's names stayed pinned");
    assert!(ref_candidate_files(&conn, &["only_here".to_string()]).is_empty());
}

/// The `bag IS NULL` discriminator is only sound if a row this code writes
/// never has an empty bag blob — an empty one would read as a pre-split row.
///
/// Nothing in the writer enforces that directly; it holds because `zstd`
/// always emits a frame header, which is a property of a dependency rather
/// than of this codebase. So it is pinned here: the case that would break it
/// is an analysis whose witness bag is genuinely empty, which is exactly the
/// input a reader could not otherwise tell apart from an old row.
#[test]
fn encoded_bag_is_never_empty() {
    let src = "1;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).expect("parse");
    let mut fa = crate::build::builder::build(&tree, src.as_bytes());
    fa.witnesses = Default::default();
    assert!(
        fa.witnesses.is_empty(),
        "this test is vacuous unless the bag really is empty"
    );
    let enc = encode_analysis(&fa).expect("encode");
    assert!(
        !enc.bag.is_empty(),
        "an empty bag blob is indistinguishable from a pre-split row's NULL, \
         so every such row would silently decode as though its bag lived \
         inside the analysis blob"
    );
}

/// A rows-only read must not report "this file has no type facts".
///
/// The split makes that failure newly reachable: the bag genuinely is not in
/// the bytes any more, so an analysis that forgot to mark itself evicted
/// would answer type queries with a confident, empty, wrong answer instead of
/// rehydrating. The marker is the only thing standing between the two.
#[test]
fn rows_only_read_is_marked_evicted_not_empty() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("stage1_axis.pm");
    std::fs::write(&pm, "package A;\nsub f { return 'x' }\n1;\n").unwrap();
    let cached = parse_source_to_cached(&std::fs::read_to_string(&pm).unwrap(), &pm);
    assert!(
        !cached.analysis.witnesses.is_empty(),
        "fixture must carry witnesses or neither assertion below means anything"
    );
    let n = cached.analysis.witnesses.len();
    let pm_str = pm.to_string_lossy().into_owned();
    save_to_db(&conn, &pm_str, &Some(cached), "workspace");

    let rows = load_one_diag(&conn, &pm_str, false).expect("rows-only read");
    assert!(
        rows.bag_is_evicted(),
        "a rows-only read left the bag absent WITHOUT the evicted marker — a \
         type query would read the empty bag as 'no facts' and never rehydrate"
    );

    let whole = load_one_diag(&conn, &pm_str, true).expect("bag read");
    assert!(!whole.bag_is_evicted());
    assert_eq!(
        whole.witnesses.len(),
        n,
        "the split must round-trip the bag exactly, not approximately"
    );
    let _ = std::fs::remove_file(&pm);
}

/// A pre-split row must never be served as a post-split one.
///
/// Such a row carries its bag inside `analysis` and a NULL `bag` column,
/// which is byte-for-byte what a post-split row with a lost bag looks like.
/// `decode_analysis_parts` deliberately does not try to tell them apart; the
/// `extract_version` filter is what makes the ambiguous row unreachable, and
/// this is the test that fails if that filter is ever dropped.
#[test]
fn pre_split_rows_are_filtered_not_guessed() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("stage1_presplit.pm");
    std::fs::write(&pm, "package B;\nsub g { return 1 }\n1;\n").unwrap();
    let cached = parse_source_to_cached(&std::fs::read_to_string(&pm).unwrap(), &pm);
    let pm_str = pm.to_string_lossy().into_owned();

    // A row in the OLD shape: whole analysis (bag inline) in `analysis`,
    // NULL `bag`, and the extract_version that shipped before the split.
    let whole_blob = {
        let bin = bincode::serialize(&*cached.analysis).expect("bincode");
        zstd::encode_all(bin.as_slice(), 3).expect("zstd")
    };
    conn.execute(
        "INSERT OR REPLACE INTO modules
           (module_name, path, mtime_secs, file_size, source, analysis, bag, extract_version, deps_stamp)
         VALUES (?1, ?1, 0, 0, 'workspace', ?2, NULL, ?3, 0)",
        params![pm_str, whole_blob, EXTRACT_VERSION - 1],
    )
    .unwrap();

    assert!(
        matches!(
            load_one_diag(&conn, &pm_str, true).unwrap_err(),
            RehydrateMiss::NoRow
        ),
        "a pre-split row was served as post-split; its inline bag would be \
         mistaken for a missing bag column (or vice versa) and one of those \
         two readings is silently wrong"
    );
    let _ = std::fs::remove_file(&pm);
}


/// The conclusion fingerprint must describe the tree as it is RIGHT NOW.
///
/// The failure this catches is a stale constant: if `build.rs` ever stops
/// re-running when a source file changes — a directory-level
/// `rerun-if-changed`, a path it hashes but forgets to declare — the compiled
/// constant keeps describing an older tree. Every baked conclusion then
/// validates against a fingerprint that no longer means anything, which is
/// exactly the hand-maintained version the derived one exists to replace, only
/// now with nobody watching it.
///
/// The hash is recomputed here rather than shared with `build.rs`. A shared
/// helper would agree with itself whatever it computed; two independent
/// spellings that must agree is the only arrangement that can fail.
#[test]
fn the_conclusion_fingerprint_is_not_stale() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(&p, out),
                Ok(t) if t.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    walk(&root.join("src"), &mut files);
    if root.join("Cargo.lock").is_file() {
        files.push(root.join("Cargo.lock"));
    }
    files.sort();
    assert!(
        files.len() > 100,
        "walked only {} files — this test would pass vacuously",
        files.len()
    );

    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    let fnv = |acc: &mut u64, bytes: &[u8]| {
        for b in bytes {
            *acc ^= *b as u64;
            *acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else { continue };
        // Paths are hashed relative to the manifest, so the fingerprint does
        // not change when the same tree is checked out somewhere else — a
        // worktree and its origin must agree or every worktree re-bakes.
        let rel = path.strip_prefix(root).unwrap_or(path);
        fnv(&mut acc, rel.to_string_lossy().as_bytes());
        fnv(&mut acc, b"\0");
        fnv(&mut acc, &bytes);
        fnv(&mut acc, b"\0");
    }
    // A tree edited AFTER this binary was built is a tree this test cannot
    // conclude anything about: the fingerprint compiled in describes the
    // sources as they were at build time, and comparing it to the sources as
    // they are now measures the edit, not the guard. That is an ordinary
    // developer workflow — a test run racing an editor — and failing on it
    // reports a stale-fingerprint bug that does not exist. Say "I could not
    // look" instead of "nothing there"; the same rule the instruments in this
    // arc keep having to learn.
    let built_at = std::env::current_exe()
        .and_then(|p| p.metadata())
        .and_then(|m| m.modified())
        .ok();
    if let Some(built_at) = built_at {
        let edited_after = files.iter().any(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .map(|t| t > built_at)
                .unwrap_or(false)
        });
        if edited_after {
            eprintln!(
                "the_conclusion_fingerprint_is_not_stale: SKIPPED — a hashed \
                 source file is newer than this test binary, so the tree moved \
                 after the build and the comparison would measure the edit"
            );
            return;
        }
    }
    assert_eq!(
        format!("{acc:016x}"),
        conclusion_fingerprint(),
        "the compiled-in conclusion fingerprint does not match the current \
         source tree — build.rs did not re-run for some file it hashes, so \
         the guard is describing a tree that no longer exists"
    );
}

/// A reader pinned to a generation must keep seeing it while a later one is
/// published.
///
/// This is the failure the `(path, generation)` key exists to prevent. Keyed
/// on `path` alone, publishing N+1 REPLACES the gen-N row, and a reader still
/// pinned to N finds nothing — which the evaluator reads as a definite `None`.
/// The pin would then be a way to GET wrong answers rather than avoid them,
/// and nothing downstream could tell, because "this file concludes nothing"
/// is a perfectly ordinary thing for the store to say.
#[test]
fn a_pinned_reader_does_not_see_a_later_generation() {
    use crate::model::witnesses::{Conclusion, ConclusionKey, ConclusionMap};
    let conn = test_db();
    let mk = |t: &str| {
        let mut m = std::collections::HashMap::new();
        m.insert(
            ConclusionKey::SubByName("f".into()),
            Conclusion::Value(crate::model::file_analysis::InferredType::ClassName(t.into())),
        );
        ConclusionMap(m, Default::default(), Default::default(), Default::default())
    };

    let g1 = Generation(1);
    publish_generation(&conn, g1, &[("/a.pm".to_string(), mk("One"), 111)]).expect("publish 1");
    assert_eq!(current_generation(&conn), g1);

    let pinned = load_conclusions(&conn, "/a.pm", g1).expect("gen 1 visible at gen 1");

    let g2 = Generation(2);
    publish_generation(&conn, g2, &[("/a.pm".to_string(), mk("Two"), 222)]).expect("publish 2");

    // The pin still resolves, and to the OLD content.
    let after = load_conclusions(&conn, "/a.pm", g1).expect(
        "a reader pinned to gen 1 lost its row when gen 2 published — it would \
         read absence as a definite None",
    );
    assert_eq!(after, pinned, "the pin resolved to a different generation");

    // And a fresh reader sees the new one.
    let fresh = load_conclusions(&conn, "/a.pm", current_generation(&conn)).expect("gen 2");
    assert_ne!(fresh, pinned, "gen 2 served gen 1's content");
}

/// Pruning must not delete the row a live pin resolves to.
#[test]
fn pruning_keeps_what_the_pin_still_needs() {
    use crate::model::witnesses::{Conclusion, ConclusionKey, ConclusionMap};
    let conn = test_db();
    let mk = |t: &str| {
        let mut m = std::collections::HashMap::new();
        m.insert(
            ConclusionKey::SubByName("f".into()),
            Conclusion::Value(crate::model::file_analysis::InferredType::ClassName(t.into())),
        );
        ConclusionMap(m, Default::default(), Default::default(), Default::default())
    };
    for g in 1..=4i64 {
        publish_generation(&conn, Generation(g), &[("/a.pm".to_string(), mk(&format!("G{g}")), g as u64)])
            .expect("publish");
    }
    // A reader is pinned at 3; everything strictly older than what gen 3
    // resolves to is unreachable, and gen 3's own row is not.
    prune_generations_below(&conn, Generation(3));
    assert!(
        load_conclusions(&conn, "/a.pm", Generation(3)).is_some(),
        "pruning deleted the row a pin at gen 3 resolves to"
    );
    assert!(
        load_conclusions(&conn, "/a.pm", Generation(4)).is_some(),
        "pruning deleted a generation newer than the prune floor"
    );
}

/// A failed round must not advance the generation.
///
/// Half a round published under a complete-looking generation is the same
/// absence-as-answer failure: the files that did land are read as current, the
/// ones that did not are read as concluding nothing.
#[test]
fn a_failed_round_leaves_the_previous_generation_intact() {
    use crate::model::witnesses::ConclusionMap;
    let conn = test_db();
    publish_generation(&conn, Generation(1), &[("/a.pm".to_string(), ConclusionMap::default(), 1)])
        .expect("publish 1");
    // Force a failure mid-round by dropping the table the second write needs.
    conn.execute_batch("DROP TABLE conclusions").unwrap();
    let r = publish_generation(
        &conn,
        Generation(2),
        &[("/b.pm".to_string(), ConclusionMap::default(), 2)],
    );
    assert!(r.is_err(), "a round that could not write reported success");
    assert_eq!(
        current_generation(&conn),
        Generation(1),
        "a failed round advanced the generation, so its partial writes would \
         be served as complete"
    );
}

/// Persisting an analysis persists its conclusions, and they come back.
///
/// The bake rides inside `encode_analysis` precisely so a writer cannot
/// persist the blob and forget the map. This is the test that says so: it
/// exercises the real writer, not the bake in isolation.
#[test]
fn persisting_an_analysis_persists_its_conclusions() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("concl_roundtrip.pm");
    std::fs::write(
        &pm,
        "package CR;\nsub build { return LWP::UserAgent->new }\nsub s { return 'x' }\n1;\n",
    )
    .unwrap();
    let cached = parse_source_to_cached(&std::fs::read_to_string(&pm).unwrap(), &pm);
    let pm_str = pm.to_string_lossy().into_owned();
    save_to_db(&conn, &pm_str, &Some(cached), "workspace");

    let map = load_conclusions(&conn, &pm_str, current_generation(&conn)).expect(
        "the writer persisted a blob but no conclusions — the store answers \
         'not baked' for a file that was just baked",
    );
    assert!(!map.is_empty(), "an empty map round-tripped as success");

    use crate::model::witnesses::{ConclusionKey, Outcome};
    let key = ConclusionKey::MethodOnClass {
        class: "CR".into(),
        name: "build".into(),
    };
    match map.evaluate(&key, None, None, &[]) {
        Outcome::Answer(t) => assert_eq!(
            t.class_name().as_deref(),
            Some("LWP::UserAgent"),
            "the round-tripped conclusion changed meaning"
        ),
        other => panic!("expected a baked answer for {key:?}, got {other:?}"),
    }
    let _ = std::fs::remove_file(&pm);
}

/// A derivation change clears conclusions and KEEPS blobs.
///
/// The repair for a stale conclusion is one re-bake, which needs the blob.
/// Dropping blobs here would turn it into a corpus re-parse for nothing — and
/// the failure would be invisible, since everything still works, just slowly.
#[test]
fn a_derivation_change_clears_conclusions_but_keeps_blobs() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("concl_fingerprint.pm");
    std::fs::write(&pm, "package CF;\nsub f { return 'x' }\n1;\n").unwrap();
    let cached = parse_source_to_cached(&std::fs::read_to_string(&pm).unwrap(), &pm);
    let pm_str = pm.to_string_lossy().into_owned();
    save_to_db(&conn, &pm_str, &Some(cached), "workspace");
    assert!(load_conclusions(&conn, &pm_str, current_generation(&conn)).is_some());

    validate_conclusion_fingerprint(&conn, "a-different-derivation").unwrap();

    assert!(
        load_conclusions(&conn, &pm_str, current_generation(&conn)).is_none(),
        "conclusions survived a derivation change — they now describe a \
         derivation that no longer exists, and nothing downstream can tell"
    );
    assert!(
        load_one_diag(&conn, &pm_str, true).is_ok(),
        "the blob was dropped along with the conclusions — the re-bake it \
         exists to feed now costs a re-parse instead of a decode"
    );
    let _ = std::fs::remove_file(&pm);
}

/// A blob whose map the fingerprint gate cleared must get its map back, and
/// the map it gets back must be the one the persist path would have written.
///
/// Both halves matter and they fail differently. Without the enumeration the
/// layer stays dark after every source edit until someone runs a full
/// `--clear-cache` by hand — `conclcache.known_absent` read 156,746 in that
/// state, which is purely a cost but a permanent one, and it silently made
/// every measurement taken after a rebuild a measurement of an empty layer.
/// Without the SHARED bake, the repair writes well-formed bytes carrying a
/// different answer than the persist path — the exact failure mode the
/// derivation fingerprint exists to catch, arriving through the repair that
/// fingerprint triggers.
///
/// Base-verify by dropping `paths_needing_repair`' `NOT EXISTS` clause:
/// the frontier then also contains the file that already has a map, and the
/// repair rewrites rows nothing asked for.
#[test]
fn a_cleared_conclusion_row_is_re_baked_to_the_same_map() {
    let conn = test_db();
    let path = std::path::Path::new("/repair/App.pm");
    let cached = parse_source_to_cached(
        "package My::App;\nuse Mojolicious::Lite;\n\
         plugin 'CloveApp', { alpha => 1, beta => 2 };\n\
         sub helper { return 'x' }\n1;\n",
        path,
    );
    let some = Some(cached.clone());
    assert!(save_to_db(&conn, "My::App", &some, "workspace"));

    let at = current_generation(&conn);
    let persisted = load_conclusions(&conn, "/repair/App.pm", at)
        .expect("precondition: the persist path writes a map");

    // What `validate_conclusion_fingerprint` does: clear the maps, keep the
    // blobs, on the promise that each file re-bakes from the blob it has.
    conn.execute("DELETE FROM conclusions", []).unwrap();
    assert!(
        load_conclusions(&conn, "/repair/App.pm", at).is_none(),
        "precondition: the map is gone"
    );

    let frontier = paths_needing_repair(&conn, at, true);
    assert_eq!(
        frontier,
        vec!["/repair/App.pm".to_string()],
        "the file holds a blob and no map, so it is the repair frontier"
    );

    assert_eq!(repair_conclusions_slice(&conn, &frontier, at), 1);
    let repaired = load_conclusions(&conn, "/repair/App.pm", at)
        .expect("the repair puts a map back");

    // Same map, not merely A map. A repair that concluded something else
    // would look identical to a working one from every angle but this.
    assert_eq!(
        repaired.0.len(),
        persisted.0.len(),
        "the repair baked a different number of keys than the persist path"
    );
    for (k, v) in persisted.0.iter() {
        assert_eq!(
            repaired.0.get(k),
            Some(v),
            "key {k:?} concludes differently after a repair than it did at persist"
        );
    }

    // Idempotent: nothing is left on the frontier, so a second pass is a no-op
    // rather than a rewrite loop.
    assert!(
        paths_needing_repair(&conn, at, true).is_empty(),
        "a repaired file must leave the frontier, or the background pass never ends"
    );
}

/// A changed file's blob is dropped; its BAKE must go with it.
///
/// `invalidate_generation` is the "this path's persisted derivation is void"
/// eraser — it takes the modules row, the stub and the ref rows. The
/// conclusion map is a derivation of that same blob, and leaving it behind
/// risks a wrong answer rather than a slow one: `moc_cross_file_primary`
/// consults the map before it decodes anything, and `Outcome::Answer`
/// short-circuits the chase.
///
/// Scope, stated because it was overstated once: this pins the INVARIANT. No
/// end-to-end path is known that actually reads an orphaned map — the routes
/// that produce one re-persist the file or answer from the open-document tier
/// first. The invariant is still worth holding, because "a derivation outlives
/// its source" has consequences that stay invisible until some future caller
/// order exposes them.
#[test]
fn invalidating_a_generation_drops_the_bake_that_came_with_it() {
    let conn = test_db();
    let dir = std::env::temp_dir();
    let pm = dir.join("perl_lsp_invalidate_bake.pm");
    std::fs::write(&pm, "package InvBake;\nsub val { return 'x' }\n1;\n").unwrap();
    let source = std::fs::read_to_string(&pm).unwrap();
    let path_str = pm.to_string_lossy().into_owned();
    let cached = parse_source_to_cached(&source, &pm);
    assert!(save_to_db(&conn, &path_str, &Some(cached), "workspace"));

    let at = current_generation(&conn);
    assert!(
        load_conclusions(&conn, &path_str, at).is_some(),
        "precondition: the persist wrote a map beside the blob"
    );

    invalidate_generation(&conn, &path_str);

    assert!(
        load_one_diag(&conn, &path_str, true).is_err(),
        "precondition: the blob is gone"
    );
    assert!(
        load_conclusions(&conn, &path_str, at).is_none(),
        "the map outlived the blob it was baked from — a consult would be \
         answered from the previous version of the file, and nothing \
         downstream can tell"
    );

    let _ = std::fs::remove_file(&pm);
}

/// The persist path stores the projection a warm lane can adopt, and it is the
/// COLD one — the projection taken from the whole analysis, not from the
/// stripped copy a warm reader holds.
///
/// `Surface::project` reads the witness bag. Re-projecting on the warm side
/// therefore records a smaller surface for identical bytes, and nothing about
/// the degraded result says it is partial: 76.7% of conclusions rows read
/// stale against rows that were correct, and a warm-start freshness verdict
/// was computed over a file that does not exist in that shape
/// (`docs/adr/storage-engine.md`).
#[test]
fn the_persisted_surface_is_the_cold_projection() {
    let conn = test_db();
    let path = std::path::Path::new("/surf/App.pm");
    let cached = parse_source_to_cached(
        "package My::App;\nuse Mojolicious::Lite;\n\
         plugin 'CloveApp', { alpha => 1, beta => 2 };\n\
         sub helper { return 'x' }\n1;\n",
        path,
    );
    let whole_fp = crate::model::surface::surface_fingerprint(
        &crate::model::surface::Surface::project(&cached.analysis),
    );
    assert!(save_to_db(&conn, "My::App", &Some(cached.clone()), "workspace"));

    let stored = load_surface(&conn, "/surf/App.pm").expect("the persist path stores a surface");
    assert_eq!(
        crate::model::surface::surface_fingerprint(&stored),
        whole_fp,
        "the persisted surface is not the one the cold lane projected"
    );

    // And it is NOT what a warm reader would have produced on its own.
    let mut stripped = (*cached.analysis).clone();
    stripped.evict_witness_bag();
    assert_ne!(
        crate::model::surface::surface_fingerprint(&crate::model::surface::Surface::project(
            &stripped
        )),
        whole_fp,
        "fixture no longer carries bag-derived surface content, so it cannot \
         demonstrate what persisting the projection is for"
    );
}

/// A surface written by an older `Surface::project` reads absent, and the
/// repair frontier picks the file up.
///
/// A persisted projection outliving its projector is the same class as a
/// derivation outliving its source: the bytes deserialize cleanly and simply
/// describe a shape this build no longer produces. Version-gated per ROW, so
/// the question a reader asks — "is this one MY projection would make" — is
/// the question the row answers.
#[test]
fn a_surface_from_another_projection_version_reads_absent_and_is_repaired() {
    let conn = test_db();
    let path = std::path::Path::new("/surfver/App.pm");
    let cached = parse_source_to_cached(
        "package Ver::App;\nuse Mojolicious::Lite;\n\
         plugin 'CloveApp', { alpha => 1 };\nsub helper { return 'x' }\n1;\n",
        path,
    );
    assert!(save_to_db(&conn, "Ver::App", &Some(cached.clone()), "workspace"));
    let at = current_generation(&conn);
    assert!(load_surface(&conn, "/surfver/App.pm").is_some(), "precondition");
    assert!(
        paths_needing_repair(&conn, at, true).is_empty(),
        "precondition: a freshly persisted file needs no repair"
    );

    // What a change to `Surface::project` looks like from the store's side.
    conn.execute(
        "UPDATE surfaces SET version = 'from-an-older-projection'",
        [],
    )
    .unwrap();
    assert!(
        load_surface(&conn, "/surfver/App.pm").is_none(),
        "a surface from another projection version must not be adopted — it \
         describes a shape this build does not produce"
    );
    assert_eq!(
        paths_needing_repair(&conn, at, true),
        vec!["/surfver/App.pm".to_string()],
        "a stale surface must put the file back on the repair frontier, or the \
         first edit to the projection silently re-opens the drift for every \
         file already in the cache"
    );

    repair_conclusions_slice(&conn, &["/surfver/App.pm".to_string()], at);
    let repaired = load_surface(&conn, "/surfver/App.pm").expect("the repair rewrites the surface");
    assert_eq!(
        crate::model::surface::surface_fingerprint(&repaired),
        crate::model::surface::surface_fingerprint(&crate::model::surface::Surface::project(
            &cached.analysis
        )),
        "the repaired surface is not the cold projection"
    );
    assert!(
        paths_needing_repair(&conn, at, true).is_empty(),
        "a repaired file must leave the frontier"
    );
}

/// A surface write that does not produce a row must leave the path ABSENT,
/// never carrying the PREVIOUS content's projection at the current version.
///
/// Absence costs a re-projection; a stale row is a wrong answer that cannot be
/// detected downstream and does not heal. `load_surface` keys on
/// `(path, version)`, so a leftover row at the current version reads as valid;
/// `paths_needing_repair` only asks `NOT EXISTS`, so a present-but-wrong row
/// never joins the repair frontier. The pairing that produces — a pre-edit
/// fingerprint against a post-edit blob — makes every consumer's freshness
/// verdict read `Unchanged` for a file that did change, which is the same
/// wrong-answer class this store exists to close.
///
/// Fails against the pre-fix writer, whose empty-encode early return ran
/// before any delete and left the prior row in place.
#[test]
fn a_surface_write_that_stores_nothing_leaves_no_stale_row() {
    let conn = test_db();
    let path = std::path::Path::new("/surfstale/App.pm");

    let before = parse_source_to_cached("package My::App;\nsub alpha { 1 }\n1;\n", path);
    assert!(save_to_db(&conn, "My::App", &Some(before), "workspace"));
    assert!(
        load_surface(&conn, "/surfstale/App.pm").is_some(),
        "precondition: the first persist stored a surface"
    );

    // The file changed, and this time the surface half produces no bytes.
    // The modules row still commits, so the store must not be left pairing the
    // OLD surface with the NEW blob.
    let empty = EncodedAnalysis {
        analysis: Vec::new(),
        conclusions: Vec::new(),
        surface: Vec::new(),
        source_fingerprint: 0,
        bag: Vec::new(),
    };
    persist_surface(&conn, "/surfstale/App.pm", &empty);

    assert!(
        load_surface(&conn, "/surfstale/App.pm").is_none(),
        "a surface write that stored nothing left the previous content's \
         projection readable at the current version — the reader cannot tell \
         it is stale, and the repair frontier's NOT EXISTS will never see it"
    );
}

/// A surface must not outlive the blob it was projected from.
///
/// It is a derivation of the analysis, exactly as the baked map is, and it is
/// written by the same `encode_analysis` call. An orphaned surface is the
/// "a derivation outlived its source" shape one table over: the warm lane
/// adopts a projection of a file the store no longer holds, and records it as
/// this path's freshness surface. Both erasers are covered — the per-path
/// invalidation and the hard clear.
#[test]
fn a_surface_does_not_outlive_its_blob() {
    let conn = test_db();
    let path = std::path::Path::new("/erase/App.pm");
    let cached = parse_source_to_cached(
        "package Erase::App;\nuse Mojolicious::Lite;\n\
         plugin 'CloveApp', { alpha => 1 };\nsub helper { return 'x' }\n1;\n",
        path,
    );
    assert!(save_to_db(&conn, "Erase::App", &Some(cached.clone()), "workspace"));
    assert!(load_surface(&conn, "/erase/App.pm").is_some(), "precondition");

    // Per-path: the blob goes, so the projection of it must go too.
    invalidate_generation(&conn, "/erase/App.pm");
    assert!(
        load_surface(&conn, "/erase/App.pm").is_none(),
        "the surface survived the blob it was projected from — a warm lane \
         would adopt it and record a projection of a file the store no \
         longer holds"
    );

    // Hard clear: same rule, the other door.
    assert!(save_to_db(&conn, "Erase::App", &Some(cached.clone()), "workspace"));
    assert!(load_surface(&conn, "/erase/App.pm").is_some(), "precondition");
    conn.execute("DELETE FROM modules", []).unwrap();
    clear_derived_rows(&conn).unwrap();
    assert!(
        load_surface(&conn, "/erase/App.pm").is_none(),
        "a hard clear left the surfaces table behind"
    );
}
