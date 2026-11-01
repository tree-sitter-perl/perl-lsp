use super::*;

/// H9-2 deferral coordinator: while the initial pack index is in flight,
/// watched-file changes DEFER into a bounded set (one entry per distinct path)
/// reconciled exactly once at completion; before/after the index they run
/// immediately. The flag check and queue insert are atomic with the drain, so a
/// save is neither dropped from the queue nor skipped by the normal path.
#[test]
fn pack_change_coordinator_defers_during_index_and_reconciles_once() {
    let coord = PackChangeCoordinator::default();
    let p = |s: &str| PathBuf::from(s);

    // Not in flight → run immediately (no deferral).
    assert!(!coord.note_change(&p("/w/a.h"), false));
    assert!(!coord.is_in_flight());

    // In flight → changes defer.
    coord.begin_index();
    assert!(coord.is_in_flight());
    assert!(coord.note_change(&p("/w/a.h"), false));
    assert!(coord.note_change(&p("/w/b.h"), false));
    // A repeated save of one path collapses to a single entry; the latest
    // delete flag wins (the reconcile reads current disk regardless).
    assert!(coord.note_change(&p("/w/a.h"), true));

    // Completion clears the flag and drains exactly the distinct deferred paths.
    let mut drained = coord.finish_index();
    drained.sort();
    assert!(!coord.is_in_flight());
    assert_eq!(drained.len(), 2, "distinct paths, deduped");
    assert_eq!(drained[0], (p("/w/a.h"), true), "last a.h save was a delete");
    assert_eq!(drained[1], (p("/w/b.h"), false));

    // After completion, changes run immediately again and the set is empty.
    assert!(!coord.note_change(&p("/w/c.h"), false));
    assert!(coord.finish_index().is_empty());
}

/// H9-1 source-generation guard: a claim succeeds iff its generation is ≥ the
/// one already registered, so a stale re-analysis (built from pre-save bytes →
/// a lower generation) can never revert a fresher registration, while a
/// serialized fresh re-registration (an equal-generation reconcile) still lands.
#[test]
fn claim_source_gen_orders_by_generation() {
    let inv = PackInvalidator::default();
    let p = Path::new("/fake/gen.cpp");
    // First claim wins from the baseline.
    assert!(inv.claim_source_gen(p, 5));
    // Strictly-older is REJECTED (the stale-winner race).
    assert!(!inv.claim_source_gen(p, 3));
    // Equal generation ties succeed (the reconcile running after the bulk pass).
    assert!(inv.claim_source_gen(p, 5));
    // Newer wins and advances the watermark.
    assert!(inv.claim_source_gen(p, 9));
    assert!(!inv.claim_source_gen(p, 8));
    // A different path is independent.
    let q = Path::new("/fake/other.cpp");
    assert!(inv.claim_source_gen(q, 1));
    assert!(inv.claim_source_gen(p, 10));
    // Forget resets to the baseline — a recreated file claims cleanly.
    inv.forget_source_gen(p);
    assert!(inv.claim_source_gen(p, 1));
}

#[cfg(feature = "cpp")]
fn register_pair(
    driver: &dyn crate::build::language_driver::LanguageDriver,
    pack: &ModuleIndex,
    paths: &[&Path],
) {
    for p in paths {
        let src = std::fs::read_to_string(p).unwrap();
        pack.register_symbols(p.to_path_buf(), Arc::new(driver.analyze_with_path(&src, Some(p))));
    }
}

#[cfg(any(feature = "cpp", feature = "python"))]
fn arc_of(pack: &ModuleIndex, path: &Path) -> usize {
    let mut found = None;
    pack.for_each_registered_file(&mut |cm| {
        if cm.path == crate::util::canonical_path::CanonicalPath::resolve(&path) {
            found = Some(Arc::as_ptr(&cm.analysis) as usize);
        }
    });
    found.expect("registered")
}

/// The pack-tier surface gate through the ONE entry point: a body/comment-only
/// edit in a header leaves its span-free surface unchanged, so `file_changed`
/// re-analyzes the header ALONE — every registered consumer survives untouched
/// AND the outcome's open-consumer set is empty (the gate covers both halves
/// of the consumer answer; the changed file's own open doc still refreshes).
/// A cross-file-visible edit (new method) re-analyzes consumers too and puts
/// the open consumer in the outcome.
#[cfg(feature = "cpp")]
#[test]
fn surface_gate_covers_registered_and_open_consumers() {
    let dir = std::env::temp_dir().join(format!("pack-surface-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hdr = dir.join("box.h");
    let tu = dir.join("use.cpp");
    std::fs::write(&hdr, "class Box { public: int width() { return 1; } };\n").unwrap();
    std::fs::write(&tu, "#include \"box.h\"\nint f() { Box b; return b.width(); }\n").unwrap();

    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let pack = Arc::new(ModuleIndex::new_for_test());
    register_pair(driver, &pack, &[&hdr, &tu]);
    let canon_tu = std::fs::canonicalize(&tu).unwrap();
    let canon_hdr = std::fs::canonicalize(&hdr).unwrap();

    // The TU is also OPEN — the outcome must answer the open half too.
    let files = OpenDocs::serving(driver);
    let tu_url = Url::from_file_path(&canon_tu).unwrap();
    let hdr_url = Url::from_file_path(&canon_hdr).unwrap();
    assert!(files.open(tu_url.clone(), std::fs::read_to_string(&tu).unwrap()));

    // Sanity: the consumer edge exists (use.cpp's closure holds box.h).
    let tu_before = arc_of(&pack, &canon_tu);
    let hdr_before = arc_of(&pack, &canon_hdr);

    let inv = PackInvalidator::serving(driver);

    // Body-only edit: return value changes, surface identical.
    std::fs::write(&hdr, "class Box { public: int width() { return 2; } };\n").unwrap();
    let outcome = inv.file_changed(None, &pack, &files, &hdr, false);
    assert!(!outcome.deferred);
    assert_ne!(arc_of(&pack, &canon_hdr), hdr_before, "changed file re-registered");
    assert_eq!(
        arc_of(&pack, &canon_tu),
        tu_before,
        "surface unchanged: consumer registration must survive untouched"
    );
    assert!(
        outcome.refresh_open.is_empty(),
        "surface unchanged: open consumers must not re-gather (same gate as the \
         registered storm); got {:?}",
        outcome.refresh_open
    );

    // Cross-file-visible edit: a new method lands on the surface.
    std::fs::write(
        &hdr,
        "class Box { public: int width() { return 2; } int height() { return 3; } };\n",
    )
    .unwrap();
    let outcome = inv.file_changed(None, &pack, &files, &hdr, false);
    assert_ne!(
        arc_of(&pack, &canon_tu),
        tu_before,
        "surface changed: consumer must re-analyze"
    );
    assert_eq!(
        outcome.refresh_open,
        vec![tu_url],
        "surface changed: the open consumer refreshes"
    );

    // The changed file's OWN open doc refreshes regardless of verdict.
    assert!(files.open(hdr_url.clone(), std::fs::read_to_string(&hdr).unwrap()));
    std::fs::write(&hdr, "class Box { public: int width() { return 3; } int height() { return 3; } };\n").unwrap();
    let outcome = inv.file_changed(None, &pack, &files, &hdr, false);
    assert!(
        outcome.refresh_open.contains(&hdr_url),
        "the edited file's own open doc always refreshes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The guard applied at the invalidation swap: a re-analysis whose event
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
    std::fs::write(&tu, "#include \"box.h\"\nint f() { Box b; return b.width(); }\n").unwrap();

    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let pack = Arc::new(ModuleIndex::new_for_test());
    register_pair(driver, &pack, &[&hdr, &tu]);
    let canon_hdr = std::fs::canonicalize(&hdr).unwrap();
    let canon_tu = std::fs::canonicalize(&tu).unwrap();
    let hdr_before = arc_of(&pack, &canon_hdr);
    let tu_before = arc_of(&pack, &canon_tu);

    let inv = PackInvalidator::serving(driver);
    // A fresher writer already claimed the maximum generation for both paths.
    assert!(inv.claim_source_gen(&canon_hdr, i64::MAX));
    assert!(inv.claim_source_gen(&canon_tu, i64::MAX));

    // A cross-file-visible edit whose event generation (mtime) is < MAX must be
    // rejected at the swap — the stale re-analysis loses to nothing.
    std::fs::write(
        &hdr,
        "class Box { public: int width() { return 2; } int height() { return 3; } };\n",
    )
    .unwrap();
    inv.file_changed(None, &pack, &OpenDocs::serving(driver), &hdr, false);
    assert_eq!(arc_of(&pack, &canon_hdr), hdr_before, "stale header re-register skipped");
    assert_eq!(arc_of(&pack, &canon_tu), tu_before, "stale consumer re-register skipped");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A change arriving mid-bulk-index defers (empty outcome) and lands via the
/// `finish_bulk_index` reconcile — through the invalidator's own entry
/// points, so the lock + coordinator + generation discipline are exercised
/// as one subsystem.
#[cfg(feature = "cpp")]
#[test]
fn file_changed_defers_during_bulk_index_and_reconciles() {
    let dir = std::env::temp_dir().join(format!("pack-defer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hdr = dir.join("box.h");
    std::fs::write(&hdr, "class Box { public: int width() { return 1; } };\n").unwrap();

    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let pack = Arc::new(ModuleIndex::new_for_test());
    register_pair(driver, &pack, &[&hdr]);
    let canon_hdr = std::fs::canonicalize(&hdr).unwrap();
    let before = arc_of(&pack, &canon_hdr);

    let inv = PackInvalidator::serving(driver);
    inv.begin_bulk_index();
    std::fs::write(&hdr, "class Box { public: int width() { return 1; } int h(); };\n").unwrap();
    let outcome = inv.file_changed(None, &pack, &OpenDocs::serving(driver), &hdr, false);
    assert!(outcome.deferred, "mid-index change defers");
    assert!(outcome.refresh_open.is_empty());
    assert_eq!(arc_of(&pack, &canon_hdr), before, "deferred: no swap yet");

    inv.finish_bulk_index(None, &pack);
    assert_ne!(arc_of(&pack, &canon_hdr), before, "reconcile lands the deferred change");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- The persist/strip licence ----
//
// These three drive `swap`'s persist path with a REAL cache DB
// (`with_test_cache_dir`), which `#[cfg(test)] open_cache_db -> None` made
// unreachable. The failure lanes are injected with SQLite triggers rather
// than lock timing, so each is deterministic: `RAISE(ROLLBACK)` discards the
// whole transaction (the commit-fail lane), `RAISE(ABORT)` scoped to one path
// fails that file's blob alone while the commit succeeds (the per-file lane).

#[cfg(feature = "cpp")]
struct SwapFixture {
    dir: PathBuf,
    hdr: PathBuf,
    tu: PathBuf,
    pack: Arc<ModuleIndex>,
    inv: PackInvalidator,
    files: OpenDocs,
}

#[cfg(feature = "cpp")]
fn swap_fixture(tag: &str) -> SwapFixture {
    let dir = std::env::temp_dir().join(format!("pack-persist-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let hdr = dir.join("box.h");
    let tu = dir.join("use.cpp");
    std::fs::write(&hdr, "class Box { public: int width() { return 1; } };\n").unwrap();
    std::fs::write(&tu, "#include \"box.h\"\nint f() { Box b; return b.width(); }\n").unwrap();

    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let pack = Arc::new(ModuleIndex::new_for_test());
    register_pair(driver, &pack, &[&hdr, &tu]);

    SwapFixture { dir, hdr, tu, pack, inv: PackInvalidator::serving(driver), files: OpenDocs::serving(driver) }
}

/// The registered analysis for `path` — the residency question these tests
/// ask ("was this copy stripped against a blob that landed?").
#[cfg(feature = "cpp")]
fn registered(pack: &ModuleIndex, path: &Path) -> Arc<FileAnalysis> {
    let canon = std::fs::canonicalize(path).unwrap();
    let mut found = None;
    pack.for_each_registered_file(&mut |cm| {
        if cm.path == crate::util::canonical_path::CanonicalPath::resolve(&canon) {
            found = Some(Arc::clone(&cm.analysis));
        }
    });
    found.expect("registered")
}

#[cfg(feature = "cpp")]
const TEST_BUSY: std::time::Duration = std::time::Duration::from_millis(50);

/// Control for the two failure-lane tests below: with a healthy DB the swap
/// DOES strip, so a "registered whole" assertion there is evidence about the
/// failure lane rather than evidence the branch was never reached.
#[cfg(feature = "cpp")]
#[test]
fn a_landed_persist_licenses_the_strip() {
    let f = swap_fixture("ok");
    std::fs::write(&f.hdr, "class Box { public: int width() { return 2; } int h() { return 3; } };\n").unwrap();
    module_cache::with_test_cache_dir(&f.dir, TEST_BUSY, || {
        f.inv.file_changed(None, &f.pack, &f.files, &f.hdr, false);
    });
    assert!(
        registered(&f.pack, &f.hdr).symbols_are_evicted(),
        "a committed blob licenses the strip — otherwise these tests prove nothing"
    );
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// The commit-fail lane. The transaction is discarded, so no blob landed for
/// ANY file in the batch and every copy must stay whole: a stripped copy
/// whose blob was rolled back rehydrates from the PREVIOUS generation's row
/// (`load_one_diag` skips stamp validation for a single-row path), so the
/// file answers pre-edit for the rest of the session with no rehydration miss
/// counted and no strict-residency panic.
#[cfg(feature = "cpp")]
#[test]
fn a_rolled_back_transaction_leaves_every_copy_whole() {
    let f = swap_fixture("rollback");
    std::fs::write(&f.hdr, "class Box { public: int width() { return 2; } int h() { return 3; } };\n").unwrap();
    module_cache::with_test_cache_dir(&f.dir, TEST_BUSY, || {
        {
            let conn = module_cache::open_cache_db(None, "cpp").expect("test cache db");
            conn.execute_batch(
                "CREATE TRIGGER kill_modules BEFORE INSERT ON modules \
                 BEGIN SELECT RAISE(ROLLBACK, 'injected'); END;",
            )
            .unwrap();
        }
        f.inv.file_changed(None, &f.pack, &f.files, &f.hdr, false);
    });
    for p in [&f.hdr, &f.tu] {
        let a = registered(&f.pack, p);
        assert!(
            !a.symbols_are_evicted() && !a.bag_is_evicted(),
            "{p:?} was stripped against a transaction that rolled back"
        );
    }
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// The per-file lane: one file's blob fails to land while the transaction
/// commits. `save_to_db` reports it with its `bool` return; the strip licence
/// is per PATH, so the failing file stays whole and its sibling still strips.
#[cfg(feature = "cpp")]
#[test]
fn a_file_whose_blob_did_not_land_is_not_stripped() {
    let f = swap_fixture("perfile");
    std::fs::write(&f.hdr, "class Box { public: int width() { return 2; } int h() { return 3; } };\n").unwrap();
    let canon_hdr = std::fs::canonicalize(&f.hdr).unwrap();
    module_cache::with_test_cache_dir(&f.dir, TEST_BUSY, || {
        {
            let conn = module_cache::open_cache_db(None, "cpp").expect("test cache db");
            // ABORT is statement-scoped: this file's INSERT fails, the
            // transaction stays open and commits with its siblings' rows.
            // Inlined, not bound — a trigger body cannot use parameters.
            let target = canon_hdr.to_string_lossy().replace('\'', "''");
            conn.execute_batch(&format!(
                "CREATE TRIGGER kill_one BEFORE INSERT ON modules \
                 WHEN NEW.path = '{target}' \
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;"
            ))
            .unwrap();
        }
        f.inv.file_changed(None, &f.pack, &f.files, &f.hdr, false);
    });
    assert!(
        !registered(&f.pack, &f.hdr).symbols_are_evicted(),
        "the file whose blob failed to save was stripped anyway"
    );
    assert!(
        registered(&f.pack, &f.tu).symbols_are_evicted(),
        "a sibling whose blob DID land must still strip — the licence is per path"
    );
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[cfg(feature = "python")]
fn class_names(pack: &ModuleIndex, path: &Path) -> Vec<String> {
    let mut names = Vec::new();
    pack.for_each_registered_file(&mut |cm| {
        if cm.path == crate::util::canonical_path::CanonicalPath::resolve(&path) {
            names = cm
                .analysis
                .symbols()
                .iter()
                .filter(|s| s.kind == crate::model::file_analysis::SymKind::Class)
                .map(|s| s.name.clone())
                .collect();
        }
    });
    names
}

/// A package marker created through the watcher path re-files the modules
/// under its directory — registered copies and open docs alike, though the
/// marker's own surface says nothing about them — and deleting it un-files
/// them. A save of a marker that already exists re-files nothing.
#[cfg(feature = "python")]
#[test]
fn a_package_marker_from_the_watcher_refiles_the_modules_under_it() {
    let dir = std::env::temp_dir().join(format!("pack-refile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/pkg")).unwrap();
    let dir = std::fs::canonicalize(&dir).unwrap();
    let module = dir.join("src/pkg/mod.py");
    let outside = dir.join("src/other.py");
    let init = dir.join("src/pkg/__init__.py");
    std::fs::write(&module, "class Widget:\n    pass\n").unwrap();
    std::fs::write(&outside, "class Other:\n    pass\n").unwrap();
    crate::build::walk_markers::record_walk("__init__.py", &dir, &[module.clone(), outside.clone()]);

    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("python").expect("python driver");
    let pack = Arc::new(ModuleIndex::new_for_test());
    for p in [&module, &outside] {
        let src = std::fs::read_to_string(p).unwrap();
        pack.register_symbols(p.to_path_buf(), Arc::new(driver.analyze_with_path(&src, Some(p))));
    }
    assert_eq!(class_names(&pack, &module), ["src.pkg.mod.Widget"]);
    let files = OpenDocs::serving(driver);
    let module_url = Url::from_file_path(&module).unwrap();
    assert!(files.open(module_url.clone(), std::fs::read_to_string(&module).unwrap()));
    let outside_before = arc_of(&pack, &outside);
    let inv = PackInvalidator::serving(driver);

    std::fs::write(&init, "").unwrap();
    let outcome = inv.file_changed(None, &pack, &files, &init, false);
    assert_eq!(class_names(&pack, &module), ["pkg.mod.Widget"], "pkg.mod appears");
    assert_eq!(outcome.refresh_open, vec![module_url.clone()], "the open module re-files too");
    assert_eq!(arc_of(&pack, &outside), outside_before, "a file outside the package is untouched");

    // Editing the marker it already holds: no flip, nothing under it moves.
    let module_before = arc_of(&pack, &module);
    std::fs::write(&init, "VERSION = 1\n").unwrap();
    let outcome = inv.file_changed(None, &pack, &files, &init, false);
    assert_eq!(arc_of(&pack, &module), module_before, "no re-file on a marker save");
    assert!(outcome.refresh_open.is_empty(), "{:?}", outcome.refresh_open);

    std::fs::remove_file(&init).unwrap();
    let outcome = inv.file_changed(None, &pack, &files, &init, true);
    assert_eq!(class_names(&pack, &module), ["src.pkg.mod.Widget"], "un-filed");
    assert_eq!(outcome.refresh_open, vec![module_url]);
    let _ = std::fs::remove_dir_all(&dir);
}
