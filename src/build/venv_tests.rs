//! Python's reader: each environment's `site-packages`, the environments
//! the tier's roots.

use super::*;
use crate::build::dependency_map::{Ask, Located};
use crate::util::canonical_path::CanonicalPath;

fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("py-map-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    CanonicalPath::resolve(root).into_path_buf()
}

const SITE: &str = ".venv/lib/python3.12/site-packages";

fn at(map: &DependencyMap, name: &str) -> Vec<(PathBuf, Option<String>)> {
    map.locate(&Ask::Name(name.to_string()))
        .into_iter()
        .map(|Located { path, loads_as }| (path.into_path_buf(), loads_as))
        .collect()
}

#[test]
fn an_environment_s_site_packages_is_searched_and_the_environment_is_the_tier_s() {
    let site = |rel: &str| format!("{SITE}/{rel}");
    let root = project(
        "venv",
        &[
            (".venv/pyvenv.cfg", "home = /usr/bin\n"),
            (&site("acme/__init__.py"), ""),
            (&site("acme/models.py"), "class Base: pass\n"),
            (&site("six.py"), "class Six: pass\n"),
            ("app/user.py", "import six\n"),
        ],
    );
    let map = read_map(Some(&root)).unwrap();
    let s = root.join(SITE);
    assert_eq!(map.search_roots()[..], [s.clone()]);
    assert!(map.owns(&root.join(".venv/lib/python3.12/site-packages/six.py")));
    assert!(!map.owns(&root.join("app/user.py")));
    assert_eq!(at(&map, "acme"), [(s.join("acme/__init__.py"), Some("acme".into()))]);
    assert_eq!(at(&map, "acme.models"), [(s.join("acme/models.py"), Some("acme.models".into()))]);
    assert_eq!(at(&map, "six"), [(s.join("six.py"), Some("six".into()))]);
    // a member is placed by the longest module it lies in, loading as that
    // module, never by a package that merely encloses it
    assert_eq!(at(&map, "acme.models.Base"), [(s.join("acme/models.py"), Some("acme.models".into()))]);
    assert_eq!(at(&map, "acme.Missing"), [(s.join("acme/__init__.py"), Some("acme".into()))]);
    assert!(at(&map, "nowhere.Thing").is_empty());
    // asked by path, a file names the module it is
    let models = CanonicalPath::resolve(s.join("acme/models.py"));
    assert_eq!(map.locate(&Ask::File(models.clone())), [Located { path: models, loads_as: Some("acme.models".into()) }]);
    let init = CanonicalPath::resolve(s.join("acme/__init__.py"));
    assert_eq!(map.locate(&Ask::File(init.clone()))[0].loads_as.as_deref(), Some("acme"));
}

#[test]
fn a_package_answers_before_a_module_of_the_same_name() {
    let site = |rel: &str| format!("{SITE}/{rel}");
    let root = project(
        "pkgfirst",
        &[(".venv/pyvenv.cfg", ""), (&site("dual/__init__.py"), ""), (&site("dual.py"), "")],
    );
    let map = read_map(Some(&root)).unwrap();
    let got: Vec<PathBuf> = at(&map, "dual").into_iter().map(|(p, _)| p).collect();
    assert_eq!(got, [root.join(SITE).join("dual/__init__.py"), root.join(SITE).join("dual.py")]);
}

#[test]
fn every_environment_under_the_root_is_read_and_its_marker_is_an_input() {
    let root = project(
        "two",
        &[
            ("env-a/pyvenv.cfg", ""),
            ("env-a/lib/python3.11/site-packages/a.py", ""),
            ("env-b/pyvenv.cfg", ""),
            ("env-b/Lib/site-packages/b.py", ""),
            ("notenv/lib/python3.11/site-packages/c.py", ""),
        ],
    );
    let map = read_map(Some(&root)).unwrap();
    assert_eq!(
        map.search_roots()[..],
        [root.join("env-a/lib/python3.11/site-packages"), root.join("env-b/Lib/site-packages")]
    );
    assert!(at(&map, "c").is_empty(), "a directory without `pyvenv.cfg` is no environment");
    assert!(map.owns(&root.join("env-b/anything.py")));
    assert!(!map.owns(&root.join("notenv/lib/python3.11/site-packages/c.py")));
    for m in [".venv/pyvenv.cfg", "venv/pyvenv.cfg", "env-a/pyvenv.cfg", "env-b/pyvenv.cfg"] {
        assert!(map.is_input(&root.join(m)), "{m} is an input");
    }
}

#[test]
fn no_environment_is_a_map_that_places_nothing_and_no_project_is_no_map() {
    let root = project("none", &[("app/main.py", "import six\n")]);
    let map = read_map(Some(&root)).unwrap();
    assert!(map.search_roots().is_empty());
    assert!(map.tier_roots().is_empty());
    assert!(at(&map, "six").is_empty());
    // creating the conventional environment moves the map
    assert!(map.is_input(&root.join(".venv/pyvenv.cfg")));
    assert!(read_map(None).is_none());
}

#[test]
fn the_spelling_names_modules_and_packages_and_nothing_else() {
    let of = |rel: &str| (SPELLING.module_of)(Path::new(rel));
    assert_eq!(of("a/b/c.py").as_deref(), Some("a.b.c"));
    assert_eq!(of("a/b/__init__.py").as_deref(), Some("a.b"));
    assert_eq!(of("six.py").as_deref(), Some("six"));
    assert_eq!(of("__init__.py"), None);
    assert_eq!(of("a/b.so"), None);
    assert_eq!(of("a/README"), None);
    assert_eq!((SPELLING.module_paths)("a.b"), ["a/b/__init__.py", "a/b.py"]);
}
