//! Perl's reader: the project's libs ahead of `@INC`, carton's base the
//! tier's root.

use super::*;
use crate::util::canonical_path::CanonicalPath;

fn project(tag: &str, dirs: &[&str]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("perl-map-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for d in dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    CanonicalPath::resolve(root).into_path_buf()
}

#[test]
fn the_project_s_libs_go_ahead_and_carton_s_base_is_the_tier_root() {
    let root = project("carton", &["lib", "local/lib/perl5"]);
    let map = read_map(Some(&root));
    assert_eq!(map.search_roots()[..2], [root.join("local/lib/perl5"), root.join("lib")]);
    assert!(map.owns(&root.join("local/lib/perl5/Dep.pm")));
    assert!(!map.owns(&root.join("lib/App.pm")), "the walk keeps the project's own lib");
}

#[test]
fn a_project_without_carton_s_base_gives_the_tier_no_root() {
    let root = project("plain", &["lib"]);
    let map = read_map(Some(&root));
    assert!(map.tier_roots().is_empty());
    assert_eq!(map.search_roots()[0], root.join("lib"));
}

/// `@INC` comes from running `perl` (and its environment), and which
/// project dirs join it from whether they exist: no input stamps either, so
/// the map is read every run, never kept.
#[test]
fn perl_s_map_is_never_kept() {
    let root = project("unkept", &["lib", "local/lib/perl5"]);
    assert!(read_map(Some(&root)).encode().is_none());
}
