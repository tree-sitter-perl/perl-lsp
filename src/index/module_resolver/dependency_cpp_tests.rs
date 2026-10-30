//! C and C++ headers on the dependency tier: a header outside the project
//! that a unit's include closure reaches lands as DEPENDENCY, answers
//! goto-def, and warms back from the store.

use crate::build::language_driver::{LanguageDriver, LanguageRegistry};
use crate::index::module_index::ModuleIndex;
use crate::index::open_docs::{FileKey, OpenDocs};
use crate::index::resolve::{resolve, OverrideScope};
use crate::model::file_analysis::CrossFileLookup;
use std::path::{Path, PathBuf};
use tree_sitter::Point;

fn cpp() -> &'static dyn LanguageDriver {
    LanguageRegistry::compiled().for_id("cpp").expect("cpp driver")
}

/// A scratch tree: `files` are (relative path, contents).
fn tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cpp-deps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

const MAIN: &str = "#include <acme/widget.h>\nint main() { return widget_count(1) + detail_fn(); }\n";
const WIDGET: &str = "#pragma once\n#include \"detail.h\"\nint widget_count(int n);\n";
const DETAIL: &str = "#pragma once\nint detail_fn();\n";

/// `proj/` (the workspace) builds against `third_party/include`, a sibling
/// it names only through its compile database.
fn built(tag: &str) -> PathBuf {
    let base = tree(
        tag,
        &[
            ("proj/src/main.cpp", MAIN),
            ("third_party/include/acme/widget.h", WIDGET),
            ("third_party/include/acme/detail.h", DETAIL),
        ],
    );
    let proj = base.join("proj");
    let db = serde_json::json!([{
        "directory": proj,
        "file": "src/main.cpp",
        "arguments": ["c++", "-I../third_party/include", "-c", "src/main.cpp"],
    }]);
    std::fs::write(proj.join("compile_commands.json"), db.to_string()).unwrap();
    base
}

fn indexed(root: &Path) -> ModuleIndex {
    let idx = ModuleIndex::new_for_test();
    super::index_pack_language(cpp(), root, None, &idx, None, 0);
    idx.wait_idle();
    idx
}

fn at(src: &str, needle: &str) -> Point {
    let i = src.find(needle).unwrap();
    let row = src[..i].matches('\n').count();
    Point { row, column: i - src[..i].rfind('\n').map_or(0, |n| n + 1) }
}

/// goto-def's (file, row) answers at `at` in `file`, relative to `base`.
/// The file is analyzed as an editor opens it: its resident copy is
/// stripped once persisted.
fn defs(idx: &ModuleIndex, base: &Path, file: &Path, at: Point) -> Vec<(String, usize)> {
    let fa = std::sync::Arc::new(crate::build::source_files::analyze_file(cpp(), file, u64::MAX).unwrap());
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(file.to_path_buf()), at, Some(idx), OverrideScope::default());
    cs.definitions()
        .iter()
        .map(|d| match &d.key {
            FileKey::Path(p) => (p.strip_prefix(base).unwrap_or(p).display().to_string(), d.span.start.row),
            FileKey::Url(u) => (u.to_string(), d.span.start.row),
        })
        .collect()
}

#[test]
fn a_header_the_compile_database_places_outside_the_project_answers_goto_def() {
    let base = built("db");
    let proj = base.join("proj");
    let main = proj.join("src/main.cpp");
    let idx = indexed(&proj);
    let mut files = Vec::new();
    idx.for_each_workspace_file(&mut |cm| files.push(cm.path.strip_prefix(&proj).unwrap().display().to_string()));
    assert_eq!(files, ["src/main.cpp"]);

    let widget = "third_party/include/acme/widget.h";
    assert_eq!(defs(&idx, &base, &main, at(MAIN, "widget_count")), [(widget.to_string(), 2)]);
    // reached through widget.h's own quoted include
    assert_eq!(
        defs(&idx, &base, &main, at(MAIN, "detail_fn")),
        [("third_party/include/acme/detail.h".to_string(), 1)]
    );
    assert!(idx.is_dependency_path(&base.join(widget)));
}

#[test]
fn with_no_database_the_walk_up_places_a_header_above_the_project() {
    let base = tree(
        "walk-up",
        &[("proj/src/main.cpp", "#include <up.h>\nint main() { return up(); }\n"), ("include/up.h", "int up();\n")],
    );
    let main = base.join("proj/src/main.cpp");
    let idx = indexed(&base.join("proj"));
    assert_eq!(defs(&idx, &base, &main, Point { row: 1, column: 20 }), [("include/up.h".to_string(), 0)]);
    assert!(idx.is_dependency_path(&base.join("include/up.h")));
}

fn rewrite_unseen(p: &Path, from: &str, to: &str) {
    assert_eq!(from.len(), to.len());
    let mtime = std::fs::metadata(p).unwrap().modified().unwrap();
    let body = std::fs::read_to_string(p).unwrap().replace(from, to);
    std::fs::write(p, body).unwrap();
    std::fs::File::options().write(true).open(p).unwrap().set_modified(mtime).unwrap();
}

fn import_rows(store: &Path, like: &str) -> i64 {
    crate::index::module_cache::with_test_cache_dir(store, std::time::Duration::from_secs(5), || {
        crate::index::module_cache::open_cache_db(None, "cpp")
            .unwrap()
            .query_row("SELECT count(*) FROM modules WHERE source = 'import' AND path LIKE ?1", [like], |r| r.get(0))
            .unwrap()
    })
}

/// A second run against the same store warms both landed headers: each
/// was rewritten unseen between the runs, so a parse would lose the name.
#[test]
fn a_landed_header_warms_from_the_store() {
    let base = built("warm");
    let proj = base.join("proj");
    let main = proj.join("src/main.cpp");
    let store = std::env::temp_dir().join(format!("cpp-deps-warm-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).unwrap();
    let busy = std::time::Duration::from_secs(5);
    let widget = ("third_party/include/acme/widget.h".to_string(), 2);
    let detail = ("third_party/include/acme/detail.h".to_string(), 1);

    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&proj);
        assert_eq!(defs(&idx, &base, &main, at(MAIN, "widget_count")), [widget.clone()]);
        // The batched persist committed before idle, and only then stripped.
        let held = idx.def_candidates(&base.join(&widget.0).display().to_string());
        assert!(!held.is_empty() && held.iter().all(|c| c.analysis.bag_is_evicted()), "stripped once its rows committed");
    });
    assert_eq!(import_rows(&store, "%/third_party/%"), 2, "widget.h and detail.h persisted");

    rewrite_unseen(&base.join(&widget.0), "widget_count", "widget_cuont");
    rewrite_unseen(&base.join(&detail.0), "detail_fn", "detail_nf");

    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&proj);
        assert_eq!(defs(&idx, &base, &main, at(MAIN, "widget_count")), [widget.clone()], "widget.h was parsed again");
        assert_eq!(defs(&idx, &base, &main, at(MAIN, "detail_fn")), [detail.clone()], "detail.h was parsed again");
    });
}


/// A header lands under every name it declares, as one registration: its
/// edges are fed once, under the path it was asked by, so a header with
/// hundreds of names costs one feed rather than one per name.
#[test]
fn a_landed_header_registers_once_however_many_names_it_declares() {
    let base = tree(
        "fed-once",
        &[
            ("proj/src/main.cpp", "#include <many.h>\nint main() { return m1(); }\n"),
            ("include/many.h", "int m1();\nint m2();\nint m3();\n#define M4 4\ntypedef int m5_t;\nstruct m6 { int x; };\n"),
        ],
    );
    let idx = indexed(&base.join("proj"));
    let header = base.join("include/many.h");
    for name in ["m1", "m2", "m3", "M4", "m5_t", "m6"] {
        assert_eq!(idx.get_cached(name).map(|c| c.path.clone()), Some(header.clone()), "{name} binds the header");
    }
    let fed = idx.modules_with_symbol("m2");
    assert_eq!(fed.len(), 1, "fed under one name, not one per declared name: {fed:?}");
    assert_eq!(idx.get_cached(&fed[0]).map(|c| c.path.clone()), Some(header));
}

/// Landings persist through the batched writer: one transaction per chunk
/// rather than an autocommit per statement. Measured as WAL frames, the
/// pages SQLite wrote: a landing's ref and sym rows are hundreds of
/// statements, and autocommitting each rewrote the same b-tree pages every
/// time (2 GB written for 10 MB of blobs on a libstdc++ closure).
#[test]
fn dependency_persists_batch_into_one_transaction() {
    let header = |n: usize| (0..40).map(|i| format!("int f{n}_{i}(int a, int b);\n#define M{n}_{i} {i}\n")).collect::<String>();
    let bodies: Vec<String> = (0..6).map(header).collect();
    let files: Vec<(String, &str)> = bodies.iter().enumerate().map(|(i, b)| (format!("h{i}.h"), b.as_str())).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    let base = tree("batch", &refs);
    let landed: Vec<std::sync::Arc<crate::index::module_index::CachedModule>> = files
        .iter()
        .map(|(p, _)| {
            let path = base.join(p);
            let fa = crate::build::source_files::analyze_file(cpp(), &path, u64::MAX).unwrap();
            std::sync::Arc::new(crate::index::module_index::CachedModule::new(path, std::sync::Arc::new(fa)))
        })
        .collect();
    let busy = std::time::Duration::from_secs(5);
    let frames = |batched: bool| -> i64 {
        let store = std::env::temp_dir().join(format!("cpp-deps-batch-{batched}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        std::fs::create_dir_all(&store).unwrap();
        crate::index::module_cache::with_test_cache_dir(&store, busy, || {
            let conn = crate::index::module_cache::open_cache_db(None, "cpp").unwrap();
            conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
            if batched {
                let mut persists = super::DependencyPersists::default();
                for m in &landed {
                    persists.push(&m.path.display().to_string(), 0, Some(vec![m.clone()]));
                }
                persists.flush(&crate::index::module_index::IndexCore::new(), Some(&conn));
            } else {
                for m in &landed {
                    assert!(super::save_module_generation(&conn, &m.path.display().to_string(), &Some(vec![m.clone()])));
                }
            }
            let rows: i64 = conn
                .query_row("SELECT count(*) FROM modules WHERE source = 'import'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, landed.len() as i64, "every landing persisted (batched: {batched})");
            conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| r.get::<_, i64>(1)).unwrap()
        })
    };
    let (batched, autocommit) = (frames(true), frames(false));
    assert!(batched * 4 < autocommit, "batched {batched} WAL frames vs {autocommit} autocommitted");
}
