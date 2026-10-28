//! A search-path map locates a name's module, longest first.

use super::*;
use crate::build::language_driver::LanguageRegistry;

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
    let s = |rel: &str| dir.join("site").join(rel);
    assert_eq!(loc.locate("a.b.C"), [s("a/b.py"), s("a/b/__init__.py"), s("a/__init__.py")]);
    assert_eq!(loc.locate("c"), [s("c.py")]);
    assert!(loc.locate("d.E").is_empty());
    assert_eq!(marked_children(&dir, "pyvenv.cfg"), [dir.join("env")]);
}
