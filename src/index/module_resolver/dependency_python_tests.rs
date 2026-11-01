//! The dependency tier over search roots: a python virtual environment's
//! `site-packages` places what the workspace imports, the walk leaves the
//! environment out, and a landed file is named from its search root.

use crate::build::language_driver::{LanguageDriver, LanguageRegistry};
use crate::index::module_index::ModuleIndex;
use crate::index::open_docs::{FileKey, OpenDocs};
use crate::index::resolve::{resolve, OverrideScope};
use crate::model::file_analysis::CrossFileLookup;
use std::path::{Path, PathBuf};
use tree_sitter::Point;

fn python() -> &'static dyn LanguageDriver {
    LanguageRegistry::compiled().for_id("python").expect("python driver")
}

fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("py-deps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

const SITE: &str = ".venv/lib/python3.12/site-packages";
const MODELS: &str = "class Root:\n    def root(self):\n        pass\n\nclass Base(Root):\n    def name(self):\n        pass\n";
const USER: &str = "from acme.models import Base\nimport six\n\nclass User(Base):\n    def go(self):\n        self.name()\n        self.root()\n\nclass S(six.Six):\n    def go(self):\n        self.six()\n";

fn venv_project(tag: &str) -> PathBuf {
    let site = |rel: &str| format!("{SITE}/{rel}");
    project(
        tag,
        &[
            (".venv/pyvenv.cfg", "home = /usr/bin\n"),
            (&site("acme/__init__.py"), ""),
            (&site("acme/models.py"), MODELS),
            (&site("acme/unused.py"), "class Unused:\n    pass\n"),
            (&site("six.py"), "class Six:\n    def six(self):\n        pass\n"),
            ("app/__init__.py", ""),
            ("app/user.py", USER),
        ],
    )
}

fn at(src: &str, needle: &str) -> Point {
    let i = src.find(needle).unwrap();
    let row = src[..i].matches('\n').count();
    Point { row, column: i - src[..i].rfind('\n').map_or(0, |n| n + 1) }
}

fn defs(idx: &ModuleIndex, root: &Path, rel: &str, at: Point) -> Vec<(String, usize)> {
    let p = root.join(rel);
    let fa = idx.workspace_file_for_test(&p).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p)), at, Some(idx), OverrideScope::default());
    cs.definitions()
        .iter()
        .map(|d| match &d.key {
            FileKey::Path(p) => (p.strip_prefix(root).unwrap().display().to_string(), d.span.start.row),
            FileKey::Url(u, _) => (u.to_string(), d.span.start.row),
        })
        .collect()
}

#[test]
fn a_venv_places_what_the_workspace_imports() {
    let root = venv_project("venv");
    let idx = ModuleIndex::new_for_test();
    super::index_pack_language(python(), &root, None, &idx, None, 0);
    idx.wait_idle();
    let mut files = Vec::new();
    idx.for_each_workspace_file(&mut |cm| files.push(cm.path.strip_prefix(&root).unwrap().display().to_string()));
    files.sort();
    assert_eq!(files, ["app/__init__.py", "app/user.py"], "the environment is not the project's");

    let models = format!("{SITE}/acme/models.py");
    assert_eq!(defs(&idx, &root, "app/user.py", at(USER, "name()")), [(models.clone(), 5)]);
    assert_eq!(defs(&idx, &root, "app/user.py", at(USER, "root()")), [(models.clone(), 1)]);
    // a module outside any package is named from the search root
    assert_eq!(defs(&idx, &root, "app/user.py", at(USER, "six()")), [(format!("{SITE}/six.py"), 1)]);
    assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&root.join(&models))));
    // each landed under the identity the import spells, so nothing widens;
    // `acme/__init__.py` encloses the name without declaring it, so it is
    // not a candidate
    assert_eq!(idx.def_candidates("acme.models.Base").len(), 1);
    assert_eq!(idx.def_candidates("six.Six").len(), 1);
    let fa = idx.workspace_file_for_test(&root.join("app/user.py")).unwrap();
    assert!(crate::lsp::symbols::widening_diagnostics(&fa, &idx).is_empty());
    // only what the project asks for is parsed
    assert!(idx.def_candidates("acme.unused.Unused").is_empty());
}

/// A venv module nothing imports completes by name, unparsed.
#[test]
fn an_unused_venv_module_completes_from_the_map() {
    let root = venv_project("venv-names");
    let idx = ModuleIndex::new_for_test();
    super::index_pack_language(python(), &root, None, &idx, None, 0);
    idx.wait_idle();
    let path = root.join("app/user.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let typed = "import acme.un";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
    let tree = parser.parse(typed, None).unwrap();
    let (items, _) = crate::lsp::backend::pack_completion(
        &OpenDocs::new(), &fa, typed, &tree, Point::new(0, typed.len()), "python", Some(&crate::util::canonical_path::CanonicalPath::resolve(&path)), &idx,
    );
    let got: Vec<(String, Option<String>)> = items.into_iter().map(|i| (i.label, i.detail)).collect();
    assert!(got.contains(&("acme.unused".to_string(), Some("available".to_string()))), "{got:?}");
    assert!(idx.def_candidates("acme.unused.Unused").is_empty(), "listed, not parsed");
}
