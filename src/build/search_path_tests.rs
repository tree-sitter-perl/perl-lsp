//! A search-path map locates a name's module, longest first.

use super::*;
use crate::build::language_driver::{LanguageRegistry, Located, LocatedBy};

#[cfg(feature = "python")]
#[test]
fn a_name_is_its_module_or_the_module_declaring_it() {
    let dir = std::env::temp_dir().join(format!("search-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for rel in ["site/a/__init__.py", "site/a/b.py", "site/a/b/__init__.py", "site/c.py", "env/pyvenv.cfg"] {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    let dir = std::fs::canonicalize(&dir).unwrap();
    let python = LanguageRegistry::compiled().for_id("python").unwrap();
    let loc = SearchPathLocator::new(python, vec![dir.join("site")], vec![], vec![]);
    let s = |rel: &str, by| Located { path: dir.join("site").join(rel), by };
    // Only the file the name spells is the name's own; the rest enclose it.
    assert_eq!(
        loc.locate("a.b.C"),
        [s("a/b.py", LocatedBy::Enclosing), s("a/b/__init__.py", LocatedBy::Enclosing), s("a/__init__.py", LocatedBy::Enclosing)]
    );
    assert_eq!(loc.locate("a.b"), [s("a/b.py", LocatedBy::Exact), s("a/b/__init__.py", LocatedBy::Exact), s("a/__init__.py", LocatedBy::Enclosing)]);
    assert_eq!(loc.locate("c"), [s("c.py", LocatedBy::Exact)]);
    assert!(loc.locate("d.E").is_empty());
    assert_eq!(marked_children(&dir, "pyvenv.cfg"), [dir.join("env")]);
}

fn scratch(tag: &str, files: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("search-path-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for rel in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

fn names(loc: &dyn DependencyLocator, workspace: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    loc.for_each_name(workspace, &mut |n| out.push(n.to_string()));
    out.sort();
    out
}

/// Perl's names are its `.pm` files by path, over every root.
#[test]
fn a_perl_search_root_lists_its_pm_files_by_path() {
    let dir = scratch("perl", &["a/Foo.pm", "a/Foo/Bar.pm", "a/Foo/notes.txt", "a/Foo/script.pl", "b/Baz.pm"]);
    let perl = LanguageRegistry::compiled().for_id("perl").unwrap();
    let loc = SearchPathLocator::new(perl, vec![dir.join("a"), dir.join("b")], vec![], vec![]);
    assert_eq!(names(&loc, None), ["Baz", "Foo", "Foo::Bar"]);
}

/// The approved exception: a filesystem root, or a root the workspace sits
/// in, is never walked; a root inside the workspace still is.
#[test]
fn a_root_the_workspace_sits_in_is_never_listed() {
    let dir = scratch("listable", &["ws/lib/Inside.pm", "ws/Top.pm"]);
    let ws = dir.join("ws");
    assert!(!listable(Path::new("/"), Some(&ws)));
    assert!(!listable(&dir, Some(&ws)));
    assert!(!listable(&ws, Some(&ws)));
    assert!(listable(&ws.join("lib"), Some(&ws)));
    let perl = LanguageRegistry::compiled().for_id("perl").unwrap();
    let loc = SearchPathLocator::new(perl, vec![ws.clone(), ws.join("lib")], vec![], vec![]);
    assert_eq!(names(&loc, Some(&ws)), ["Inside"]);
}

/// A package is its `__init__.py`'s directory, a module its file; a path
/// that spells no importable name (a dist-info directory) is no module.
#[cfg(feature = "python")]
#[test]
fn a_python_search_root_lists_packages_and_modules() {
    let dir = scratch(
        "python",
        &["site/pkg/__init__.py", "site/pkg/mod.py", "site/top.py", "site/__init__.py", "site/pkg-1.0.dist-info/x.py"],
    );
    let python = LanguageRegistry::compiled().for_id("python").unwrap();
    let loc = SearchPathLocator::new(python, vec![dir.join("site")], vec![], vec![]);
    assert_eq!(names(&loc, None), ["pkg", "pkg.mod", "top"]);
}
