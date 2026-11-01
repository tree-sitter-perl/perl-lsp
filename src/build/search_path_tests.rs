//! A search-path map locates a name's module, longest first.

use super::*;
use crate::build::language_driver::{LanguageRegistry, Located, LocatedBy};

#[cfg(feature = "python")]
#[test]
fn a_name_is_its_module_or_the_module_declaring_it() {
    let dir = std::env::temp_dir().join(format!("search-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for rel in ["site/a/__init__.py", "site/a/b.py", "site/a/b/__init__.py", "site/c.py"] {
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
}
