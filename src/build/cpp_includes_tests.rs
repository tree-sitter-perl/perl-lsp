//! An `#include` → the file the compiler would read: a unit's compile
//! database flags, the toolchain roots, the walk-up fallback, quoted vs
//! angle order and `include_next`.

use super::*;

/// A scratch tree: `files` are (relative path, contents).
fn tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cpp-includes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

/// A compile database run in `dir`, naming `src/main.cpp` with `args`.
fn database(dir: &Path, args: &[&str]) -> String {
    let mut all = vec!["c++"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["-c", "src/main.cpp"]);
    serde_json::json!([{ "directory": dir, "file": "src/main.cpp", "arguments": all }]).to_string()
}

/// `tree` with a compile database run in the tree itself.
fn built(tag: &str, args: &[&str], files: &[(&str, &str)]) -> PathBuf {
    let root = tree(tag, files);
    std::fs::write(root.join("compile_commands.json"), database(&root, args)).unwrap();
    root
}

fn angle(p: &str) -> Directive {
    Directive { path: p.into(), kind: IncludeKind::Angle }
}
fn quoted(p: &str) -> Directive {
    Directive { path: p.into(), kind: IncludeKind::Quoted }
}
fn next(p: &str) -> Directive {
    Directive { path: p.into(), kind: IncludeKind::Next }
}

#[test]
fn a_spelling_carries_its_kind() {
    assert_eq!(Directive::spelled("<vector>"), angle("vector"));
    assert_eq!(Directive::spelled("\"a/b.h\""), quoted("a/b.h"));
    assert_eq!(Directive::spelled("a/b.h"), quoted("a/b.h"));
}

#[test]
fn a_command_splits_as_the_shell_would() {
    assert_eq!(
        split_command(r#"c++ -I "my dir" -I'x y' -DA=\"q\" -c f.cpp"#),
        vec!["c++", "-I", "my dir", "-Ix y", "-DA=\"q\"", "-c", "f.cpp"]
    );
}

#[test]
fn every_include_flag_lands_in_its_search_lane() {
    let args: Vec<String> =
        ["-Ia", "-I", "b", "-isystem", "c", "-isystemd", "-iquote", "q", "-idirafter", "z", "-Dx", "-o", "o"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let r = unit_roots(Path::new("/w"), &args);
    let p = |s: &[&str]| s.iter().map(|d| PathBuf::from("/w").join(d)).collect::<Vec<_>>();
    assert_eq!(r.angle, p(&["a", "b", "c", "d"]));
    assert_eq!(r.quote, p(&["q"]));
    assert_eq!(r.after, p(&["z"]));
}

#[test]
fn a_units_include_flag_places_a_header_outside_its_tree() {
    let root = built(
        "dash-i",
        &["-Ithird/inc"],
        &[("src/main.cpp", ""), ("third/inc/acme/x.h", "")],
    );
    let main = root.join("src/main.cpp");
    assert_eq!(resolve_among(&main, &main, &angle("acme/x.h"), &[]), Some(root.join("third/inc/acme/x.h")));
}

#[test]
fn a_toolchain_root_places_what_the_project_does_not() {
    let root = tree("system", &[("src/main.cpp", ""), ("sys/stdio.h", "")]);
    let main = root.join("src/main.cpp");
    let system = [root.join("sys")];
    assert_eq!(resolve_among(&main, &main, &angle("stdio.h"), &system), Some(root.join("sys/stdio.h")));
    assert_eq!(resolve_among(&main, &main, &angle("stdio.h"), &[]), None);
}

#[test]
fn with_no_database_the_includers_ancestors_and_their_include_dirs_are_searched() {
    let root = tree("walk-up", &[("src/deep/main.cpp", ""), ("include/w.h", ""), ("src/v.h", "")]);
    let main = root.join("src/deep/main.cpp");
    assert_eq!(resolve_among(&main, &main, &angle("w.h"), &[]), Some(root.join("include/w.h")));
    assert_eq!(resolve_among(&main, &main, &quoted("v.h"), &[]), Some(root.join("src/v.h")));
}

#[test]
fn a_database_unit_does_not_walk_up() {
    let root = built("no-walk-up", &[], &[("src/main.cpp", ""), ("include/w.h", "")]);
    let main = root.join("src/main.cpp");
    assert_eq!(resolve_among(&main, &main, &angle("w.h"), &[]), None);
}

#[test]
fn quoted_searches_its_own_directory_and_quote_roots_before_the_angle_roots() {
    let root = built(
        "order",
        &["-iquote", "q", "-Iinc"],
        &[("src/main.cpp", ""), ("src/own.h", ""), ("inc/own.h", ""), ("q/quote.h", ""), ("inc/quote.h", "")],
    );
    let main = root.join("src/main.cpp");
    let r = |d: Directive| resolve_among(&main, &main, &d, &[]);
    assert_eq!(r(quoted("own.h")), Some(root.join("src/own.h")));
    assert_eq!(r(angle("own.h")), Some(root.join("inc/own.h")));
    assert_eq!(r(quoted("quote.h")), Some(root.join("q/quote.h")));
    assert_eq!(r(angle("quote.h")), Some(root.join("inc/quote.h")));
}

#[test]
fn idirafter_follows_the_toolchain() {
    let root = built("after", &["-idirafter", "late"], &[("src/main.cpp", ""), ("late/l.h", ""), ("sys/l.h", "")]);
    let main = root.join("src/main.cpp");
    assert_eq!(resolve_among(&main, &main, &angle("l.h"), &[root.join("sys")]), Some(root.join("sys/l.h")));
    assert_eq!(resolve_among(&main, &main, &angle("l.h"), &[]), Some(root.join("late/l.h")));
}

#[test]
fn include_next_continues_after_the_root_that_answered() {
    let root = built(
        "next",
        &["-Ia", "-Ib"],
        &[("src/main.cpp", ""), ("a/n.h", ""), ("b/n.h", ""), ("sys/n.h", "")],
    );
    let main = root.join("src/main.cpp");
    let system = [root.join("sys")];
    let from = |inc: &str| resolve_among(&main, &root.join(inc), &next("n.h"), &system);
    assert_eq!(resolve_among(&main, &main, &angle("n.h"), &system), Some(root.join("a/n.h")));
    assert_eq!(from("a/n.h"), Some(root.join("b/n.h")));
    assert_eq!(from("b/n.h"), Some(root.join("sys/n.h")));
    assert_eq!(from("sys/n.h"), None);
}

#[test]
fn a_header_reached_from_a_unit_searches_with_the_units_flags() {
    let root = built(
        "borrow",
        &["-Ithird"],
        &[("src/main.cpp", ""), ("src/h.h", ""), ("third/t.h", "")],
    );
    let (main, h) = (root.join("src/main.cpp"), root.join("src/h.h"));
    assert_eq!(resolve_among(&main, &h, &angle("t.h"), &[]), Some(root.join("third/t.h")));
    assert_eq!(resolve_among(&h, &h, &angle("t.h"), &[]), None, "alone, h.h is no unit");
}

#[test]
fn a_database_in_build_describes_its_parent() {
    let root = tree("build-dir", &[("src/main.cpp", ""), ("third/t.h", "")]);
    std::fs::create_dir_all(root.join("build")).unwrap();
    std::fs::write(root.join("build/compile_commands.json"), database(&root, &["-Ithird"])).unwrap();
    let main = root.join("src/main.cpp");
    assert_eq!(resolve_among(&main, &main, &angle("t.h"), &[]), Some(root.join("third/t.h")));
    assert_ne!(roots_fingerprint(&main), 0);
}

#[test]
fn the_map_places_a_header_by_its_path_alone() {
    let root = tree("locate", &[("x.h", "")]);
    let map = IncludeMap { system: Vec::new(), inputs: Vec::new() };
    assert_eq!(map.locate(root.join("x.h").to_str().unwrap()), vec![Located { path: root.join("x.h"), by: LocatedBy::Exact }]);
    assert!(map.locate("x.h").is_empty(), "a spelling depends on its includer");
    assert!(map.locate(root.join("y.h").to_str().unwrap()).is_empty());
}

#[test]
fn the_roots_database_is_an_analysis_input() {
    let root = built("fingerprint", &["-Ia"], &[("src/main.cpp", "")]);
    let before = input_fingerprint(Some(&root));
    std::fs::write(root.join("compile_commands.json"), database(&root, &["-Ia", "-Ib"])).unwrap();
    assert_ne!(input_fingerprint(Some(&root)), before);
    assert_eq!(input_fingerprint(None), crate::build::cpp_reparse::toolchain_fingerprint());
}
