//! The shared walk: extensions, the size cap, skipped roots and ignores.

use super::*;

fn tree(tag: &str, files: &[(&str, usize)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("source-files-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, len) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x".repeat(*len)).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

#[test]
fn a_walk_keeps_its_language_under_the_cap_outside_skipped_roots() {
    let root = tree("walk", &[("a.php", 1), ("big.php", 10), ("b.txt", 1), ("dep/c.php", 1), ("sub/d.php", 1)]);
    let skip = [root.join("dep")];
    let w = Walk { extensions: &["php"], cap: 10, skip: &skip, ignores: true, follow_links: false };
    let walked = walk(&root, &w);
    assert_eq!(walked.files, [root.join("a.php"), root.join("sub/d.php")]);
    assert_eq!(walked.oversize, [(root.join("big.php"), 10)]);
    // a file root is that file
    assert_eq!(walk(&root.join("a.php"), &w).files, [root.join("a.php")]);
}

/// Projects ignore the directory their dependencies install into, so a
/// walk inside it must not honor that.
#[test]
fn only_a_walk_that_honors_ignores_drops_ignored_files() {
    let root = tree("ignores", &[(".git/HEAD", 1), ("vendor/x/y.php", 1), ("src/z.php", 1)]);
    std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    let w = |ignores| Walk { extensions: &["php"], cap: 100, skip: &[], ignores, follow_links: false };
    assert_eq!(walk(&root, &w(true)).files, [root.join("src/z.php")]);
    assert_eq!(walk(&root.join("vendor"), &w(false)).files, [root.join("vendor/x/y.php")]);
}
