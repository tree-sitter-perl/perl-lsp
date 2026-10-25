//! The Python pack's use-map: the module a file is, its import rows, and
//! the identities its classes and class spellings carry.

use super::*;
use crate::build::packs::{python_file_module, python_pack};
use crate::model::file_analysis::{FileAnalysis, InferredType, SymKind};
use tree_sitter::Point;

fn module(segments: &[&str], is_package: bool) -> FileModule {
    FileModule { segments: segments.iter().map(|s| s.to_string()).collect(), is_package }
}

fn skel_in(src: &str, m: Option<&FileModule>) -> SkeletonAnalysis {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    extract_in(&tree, src.as_bytes(), &python_pack(), m).unwrap()
}

fn py_in(src: &str, m: Option<&FileModule>) -> FileAnalysis {
    skel_in(src, m).into_file_analysis()
}

/// A scratch tree of files, removed when dropped.
struct Tree(std::path::PathBuf);

impl Tree {
    fn new(tag: &str, files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("py-use-map-{tag}-{}", std::process::id()));
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        Tree(root)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn a_file_is_the_module_its_package_chain_names() {
    let t = Tree::new("modpath", &[
        ("src/pkg/__init__.py", ""),
        ("src/pkg/sub/__init__.py", ""),
        ("src/pkg/sub/models.py", ""),
        ("loose/script.py", ""),
    ]);
    let of = |rel: &str| python_file_module(&t.0.join(rel));
    assert_eq!(of("src/pkg/sub/models.py"), Some(module(&["pkg", "sub", "models"], false)));
    assert_eq!(of("src/pkg/sub/__init__.py"), Some(module(&["pkg", "sub"], true)));
    // no package above it, and no workspace root containing it: its stem
    assert_eq!(of("loose/script.py"), Some(module(&["script"], false)));
}

#[test]
fn imports_are_rows_of_the_parts_they_bind() {
    let m = module(&["pkg", "sub", "mod"], false);
    let skel = skel_in(
        "from a.b import C as D\nimport a.b\nimport a.b as c\nfrom . import y\nfrom ..x import Z\n",
        Some(&m),
    );
    // the modules the statements load stay the import list
    assert!(skel.imports.iter().any(|i| i == "a.b"), "{:?}", skel.imports);
    let fa = skel.into_file_analysis();
    let rows: Vec<(&str, &str, Option<&str>)> = fa
        .pack
        .use_rows
        .iter()
        .map(|r| (r.name.namespace.as_str(), r.name.leaf.as_str(), r.alias.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("a.b", "C", Some("D")),
            ("", "a", None),
            ("a", "b", Some("c")),
            ("pkg.sub", "y", None),
            ("pkg.x", "Z", None),
        ]
    );
}

#[test]
fn classes_parents_and_annotations_carry_identities() {
    let m = module(&["pkg", "mod"], false);
    let src = "\
class Early(C):
    pass
from a.b import C as D
import a.b as c
class K(D, c.E, Local):
    def run(self):
        pass
def f():
    from q import R
    class L(R):
        pass
class M(R):
    pass
x: D = make()
def g(v):
    if isinstance(v, D):
        v.run()
";
    let fa = py_in(src, Some(&m));
    let classes: Vec<(String, Option<String>)> =
        fa.symbols().iter().filter(|s| s.kind == SymKind::Class).map(|s| (s.name.clone(), s.package.clone())).collect();
    assert_eq!(
        classes,
        [
            ("pkg.mod.Early".to_string(), Some("pkg.mod".to_string())),
            ("pkg.mod.K".to_string(), Some("pkg.mod".to_string())),
            ("pkg.mod.L".to_string(), Some("pkg.mod".to_string())),
            ("pkg.mod.M".to_string(), Some("pkg.mod".to_string())),
        ]
    );
    // an import binds from where it runs, in the scope it runs in
    assert_eq!(fa.declared_parents("pkg.mod.Early"), ["pkg.mod.C"]);
    assert_eq!(fa.declared_parents("pkg.mod.K"), ["a.b.C", "a.b.E", "pkg.mod.Local"]);
    assert_eq!(fa.declared_parents("pkg.mod.L"), ["q.R"]);
    assert_eq!(fa.declared_parents("pkg.mod.M"), ["pkg.mod.R"]);
    let run = fa.symbols().iter().find(|s| s.name == "run").unwrap();
    assert_eq!(run.package.as_deref(), Some("pkg.mod.K"));
    assert_eq!(
        fa.inferred_type_via_bag("x", Point::new(14, 0)),
        Some(InferredType::ClassName("a.b.C".to_string()))
    );
    assert_eq!(
        fa.inferred_type_via_bag("v", Point::new(16, 8)),
        Some(InferredType::ClassName("a.b.C".to_string()))
    );
}

/// A class's uses in another module reach it by identity — from a parent
/// clause, the import that binds it, or a call that constructs it —
/// never the same-leaf class of `pkg.legacy`.
#[cfg(feature = "python")]
#[test]
fn a_class_resolves_across_modules_by_identity() {
    use crate::build::language_driver::LanguageRegistry;
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let t = Tree::new("xfile", &[
        ("pkg/__init__.py", ""),
        ("pkg/models.py", "class User:\n    def name(self):\n        return 1\n"),
        ("pkg/legacy.py", "class User:\n    pass\n"),
        ("pkg/views.py", "from pkg.models import User\n\nclass Admin(User):\n    pass\n\nu = User()\n"),
        ("pkg/vendored.py", "from vendor.lib import User as VU\n\nclass Sub(VU):\n    pass\n"),
    ]);
    let driver = LanguageRegistry::compiled().for_path(std::path::Path::new("x.py")).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    for rel in ["pkg/models.py", "pkg/legacy.py", "pkg/views.py", "pkg/vendored.py"] {
        let p = t.0.join(rel);
        let src = std::fs::read_to_string(&p).unwrap();
        idx.insert_workspace_for_test(p.clone(), driver.analyze_with_path(&src, Some(&p)));
    }
    let p_views = t.0.join("pkg/views.py");
    let fa = idx.workspace_file_for_test(&p_views).unwrap();
    assert_eq!(fa.declared_parents("pkg.views.Admin"), ["pkg.models.User"]);
    let store = OpenDocs::new();
    let name_of = |k: &FileKey| match k {
        FileKey::Path(p) => p.file_name().unwrap().to_string_lossy().to_string(),
        FileKey::Url(u) => u.to_string(),
    };
    let want_edits: Vec<(String, usize, usize)> =
        [("models.py", 0, 6), ("views.py", 0, 23), ("views.py", 2, 12), ("views.py", 5, 4)]
            .iter()
            .map(|(f, r, c)| (f.to_string(), *r, *c))
            .collect();
    // the parent clause, the import's name, the constructing call
    for (row, column) in [(2, 12), (0, 23), (5, 4)] {
        let at = Point { row, column };
        let cs = resolve(&store, &fa, FileKey::Path(p_views.clone()), at, Some(&idx), OverrideScope::default());
        let defs: Vec<(String, usize)> = cs.definitions().iter().map(|d| (name_of(&d.key), d.span.start.row)).collect();
        assert_eq!(defs, [("models.py".to_string(), 0)], "goto-def from {at:?}");
        let mut edits: Vec<(String, usize, usize)> = cs
            .rename_edits("Member")
            .unwrap()
            .into_iter()
            .map(|(l, _)| (name_of(&l.key), l.span.start.row, l.span.start.column))
            .collect();
        edits.sort();
        assert_eq!(edits, want_edits, "rename from {at:?}");
    }

    // a class named through a module nothing indexes keeps that identity;
    // no same-leaf class stands in for it
    let p_vendored = t.0.join("pkg/vendored.py");
    let vendored = idx.workspace_file_for_test(&p_vendored).unwrap();
    assert_eq!(vendored.declared_parents("pkg.vendored.Sub"), ["vendor.lib.User"]);
    assert!(vendored.resolve_method_in_ancestors("pkg.vendored.Sub", "name", Some(&idx)).is_none());
}

/// A call of a class this module declares constructs it, whatever
/// namespace the class is filed under.
#[test]
fn calling_a_local_class_constructs_it() {
    let m = module(&["app", "shapes"], false);
    let fa = py_in("class Widget:\n    def grow(self):\n        pass\n\nw = Widget()\n", Some(&m));
    assert_eq!(
        fa.inferred_type_via_bag("w", Point::new(5, 0)),
        Some(InferredType::ClassName("app.shapes.Widget".to_string()))
    );
    // and its members file under that identity
    let members: Vec<String> =
        fa.complete_methods_for_class("app.shapes.Widget", None).into_iter().map(|c| c.label).collect();
    assert!(members.iter().any(|m| m == "grow"), "{members:?}");
}
