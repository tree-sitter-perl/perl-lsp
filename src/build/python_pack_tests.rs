//! The Python pack's use-map: the module a file is, its import rows, and
//! the identities its classes and class spellings carry.

use super::*;
use crate::build::packs::{python_file_module, python_pack};
use crate::build::walk_markers::{MarkedDirs, MarkerSnapshot};
use crate::model::file_analysis::{FileAnalysis, InferredType, SymKind};
use std::path::Path;
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
    // no walk covers the tree: the disk answers
    let of = |rel: &str| python_file_module(&t.0.join(rel), &MarkedDirs::on_disk("__init__.py"));
    assert_eq!(of("src/pkg/sub/models.py"), Some(module(&["pkg", "sub", "models"], false)));
    assert_eq!(of("src/pkg/sub/__init__.py"), Some(module(&["pkg", "sub"], true)));
    // no package above it, and no workspace root containing it: its stem
    assert_eq!(of("loose/script.py"), Some(module(&["script"], false)));
    // a snapshot of another walk covers none of it
    let other = MarkerSnapshot::from_walk("__init__.py", Path::new("/elsewhere"), &[]);
    let of = |rel: &str| python_file_module(&t.0.join(rel), &MarkedDirs::over(&other));
    assert_eq!(of("src/pkg/sub/models.py"), Some(module(&["pkg", "sub", "models"], false)));
}

/// The walk's snapshot names modules with no filesystem access: none of
/// these directories exist.
#[test]
fn a_walk_snapshot_names_modules_without_the_disk() {
    let root = Path::new("/no/such/workspace");
    let files: Vec<std::path::PathBuf> = [
        "src/pkg/__init__.py",
        "src/pkg/sub/__init__.py",
        "src/pkg/sub/models.py",
        "src/pkg/plain/view.py",
        "tools/run.py",
    ]
    .iter()
    .map(|rel| root.join(rel))
    .collect();
    let snap = MarkerSnapshot::from_walk("__init__.py", root, &files);
    let dirs = MarkedDirs::over(&snap);
    let of = |rel: &str| python_file_module(&root.join(rel), &dirs);
    assert_eq!(of("src/pkg/sub/models.py"), Some(module(&["pkg", "sub", "models"], false)));
    assert_eq!(of("src/pkg/sub/__init__.py"), Some(module(&["pkg", "sub"], true)));
    // its own directory is no package: named from the walk's root
    assert_eq!(of("src/pkg/plain/view.py"), Some(module(&["src", "pkg", "plain", "view"], false)));
    assert_eq!(of("tools/run.py"), Some(module(&["tools", "run"], false)));
}

/// A blob filed under a module its path no longer names is not current —
/// the warm scan re-analyzes it — and a marker noted through the snapshot
/// is what makes it so.
#[cfg(feature = "python")]
#[test]
fn a_noted_marker_makes_old_filings_stale() {
    use crate::build::walk_markers;
    let t = Tree::new("noted", &[("src/pkg/mod.py", "class W:\n    pass\n")]);
    let root = t.0.clone();
    let file = root.join("src/pkg/mod.py");
    let init = root.join("src/pkg/__init__.py");
    walk_markers::record_walk("__init__.py", &root, std::slice::from_ref(&file));
    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("python").expect("python driver");
    let before = driver.analyze_with_path("class W:\n    pass\n", Some(&file));
    assert_eq!(before.pack.file_module, Some(module(&["src", "pkg", "mod"], false)));
    assert!(driver.path_inputs_current(&file, &before));
    let gen = walk_markers::generation_covering("__init__.py", &file).unwrap();

    // The snapshot answers, not the disk: creating the file alone changes nothing.
    std::fs::write(&init, "").unwrap();
    assert!(driver.path_inputs_current(&file, &before));
    assert_eq!(walk_markers::note_marker("__init__.py", &init, true), Some(root.join("src/pkg")));
    assert!(walk_markers::generation_covering("__init__.py", &file).unwrap() > gen);
    assert!(!driver.path_inputs_current(&file, &before), "filed as src.pkg.mod, now pkg.mod");
    let after = driver.analyze_with_path("class W:\n    pass\n", Some(&file));
    assert_eq!(after.pack.file_module, Some(module(&["pkg", "mod"], false)));
    // noting what the snapshot already holds is no change
    assert_eq!(walk_markers::note_marker("__init__.py", &init, true), None);
    // a file of another name is no marker
    assert_eq!(walk_markers::note_marker("__init__.py", &file, false), None);
}

#[test]
fn imports_are_rows_of_the_parts_they_bind() {
    let m = module(&["pkg", "sub", "mod"], false);
    let skel = skel_in(
        "from a.b import C as D\nimport a.b\nimport a.b as c\nfrom . import y\nfrom ..x import Z\n\
         from .. import w\nfrom x import V\nfrom .m.n import U as T\n",
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
            ("pkg", "w", None),
            ("x", "V", None),
            ("pkg.sub.m.n", "U", Some("T")),
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
    ]);
    let driver = LanguageRegistry::compiled().for_path(std::path::Path::new("x.py")).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    for rel in ["pkg/models.py", "pkg/legacy.py", "pkg/views.py"] {
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

/// Calling a class constructs it, so the call is a reference to the class
/// and its value an instance — the language data PHP declares the other
/// way (`php_pack_tests::calling_a_class_name_is_a_function_call`).
#[test]
fn calling_a_class_names_it() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use crate::model::file_analysis::{RefKind, RenameKind, Span};
    let m = module(&["app"], false);
    let fa = py_in("class Foo:\n    pass\nclass Bar(Foo):\n    pass\nFoo()\n", Some(&m));

    let call = fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::FunctionCall) && r.target_name == "Foo")
        .expect("the call is a ref");
    assert_eq!(fa.class_named_by_call(call, None).as_deref(), Some("app.Foo"));
    assert!(matches!(
        fa.rename_kind_at(Point::new(4, 1), None),
        Some(RenameKind::Package(ref c)) if c == "app.Foo"
    ));
    let call_span = Span { start: Point::new(4, 0), end: Point::new(4, 5) };
    assert_eq!(fa.expr_type_at_span(call_span, None), Some(InferredType::ClassName("app.Foo".to_string())));

    // the class's references, from its parent clause, take in the call
    let path = std::path::PathBuf::from("/fake/py/app.py");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), fa);
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(path), Point::new(2, 11), Some(&idx), OverrideScope::default());
    let mut rows: Vec<usize> = cs.rename_edits("Baz").unwrap().into_iter().map(|(l, _)| l.span.start.row).collect();
    rows.sort();
    assert_eq!(rows, [0, 2, 4]);
}


/// Analyze `files` (relative to a scratch tree) through the python driver
/// into one index, in the order given.
#[cfg(feature = "python")]
fn indexed(tag: &str, files: &[(&str, &str)]) -> (Tree, crate::index::module_index::ModuleIndex) {
    use crate::build::language_driver::LanguageRegistry;
    let t = Tree::new(tag, files);
    let driver = LanguageRegistry::compiled().for_path(std::path::Path::new("x.py")).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    for (rel, _) in files.iter().filter(|(rel, _)| !rel.ends_with("__init__.py")) {
        let p = t.0.join(rel);
        let src = std::fs::read_to_string(&p).unwrap();
        idx.insert_workspace_for_test(p.clone(), driver.analyze_with_path(&src, Some(&p)));
    }
    (t, idx)
}

/// The defining files' names and rows of the set's goto-def answer at
/// `at`, its hover candidate's file, and the labels it carries.
#[cfg(feature = "python")]
fn landing(
    idx: &crate::index::module_index::ModuleIndex,
    path: &std::path::Path,
    at: Point,
) -> (Vec<(String, usize)>, Option<String>, Vec<Option<String>>) {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let name_of = |k: &FileKey| match k {
        FileKey::Path(p) => p.file_name().unwrap().to_string_lossy().to_string(),
        FileKey::Url(u) => u.to_string(),
    };
    let fa = idx.workspace_file_for_test(path).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(path.to_path_buf()), at, Some(idx), OverrideScope::default());
    let defs = cs.definitions();
    (
        defs.iter().map(|d| (name_of(&d.key), d.span.start.row)).collect(),
        cs.hover_candidate().map(|d| name_of(&d.key)),
        defs.iter().map(|d| d.label.clone()).collect(),
    )
}

/// The member `name` as `class`'s ancestry answers it, through `path`'s
/// own scope.
#[cfg(feature = "python")]
fn member_on(
    idx: &crate::index::module_index::ModuleIndex,
    path: &std::path::Path,
    class: &str,
) -> Option<crate::model::file_analysis::MethodResolution> {
    use crate::build::language_driver::LanguageRegistry;
    use crate::model::file_analysis::{CrossFileLookup, ScopedLookup, VisibilityAxis};
    let fa = idx.workspace_file_for_test(path).unwrap();
    let base: &dyn CrossFileLookup = idx;
    let vis = LanguageRegistry::pack_visibility(&fa.language);
    let scoped = ScopedLookup::new(base, &fa.pack.include_closure, None, VisibilityAxis::for_origin(&fa, None, base, vis));
    fa.resolve_method_in_ancestors(class, "name", Some(&scoped))
}

#[cfg(feature = "python")]
fn widening_codes(idx: &crate::index::module_index::ModuleIndex, path: &std::path::Path) -> Vec<(usize, usize, String)> {
    let fa = idx.workspace_file_for_test(path).unwrap();
    crate::lsp::symbols::widening_diagnostics(&fa, idx)
        .into_iter()
        .map(|d| (d.range.start.line as usize, d.range.start.character as usize, d.message))
        .collect()
}

/// Two classes share the leaf `User`, and the stranger's file sorts first:
/// the leaf is registered for widening, yet every lane lands on the
/// identity the file imports.
#[cfg(feature = "python")]
#[test]
fn a_same_leaf_class_never_competes_with_the_identity() {
    use crate::model::file_analysis::{CrossFileLookup, MethodResolution};
    let (t, idx) = indexed("leafexact", &[
        ("pkg/__init__.py", ""),
        ("pkg/a_legacy.py", "class User:\n    def name(self):\n        return 2\n"),
        ("pkg/models.py", "class User:\n    def name(self):\n        return 1\n"),
        ("pkg/views.py", "from pkg.models import User\n\nclass Admin(User):\n    pass\n"),
    ]);
    assert_eq!(idx.leaf_def_candidates("User").len(), 2, "the leaf is registered");
    assert!(idx.def_candidates("User").is_empty(), "and answers no name lookup");
    let views = t.0.join("pkg/views.py");
    for at in [Point::new(0, 23), Point::new(2, 12)] {
        let (defs, hover, labels) = landing(&idx, &views, at);
        assert_eq!(defs, [("models.py".to_string(), 0)], "goto-def from {at:?}");
        assert_eq!(hover.as_deref(), Some("models.py"), "hover from {at:?}");
        assert_eq!(labels, [None]);
    }
    assert!(matches!(
        member_on(&idx, &views, "pkg.views.Admin"),
        Some(MethodResolution::CrossFile { ref class, widened: false, .. }) if class == "pkg.models.User"
    ));
    assert!(widening_codes(&idx, &views).is_empty());
}

/// A class imported from a module nothing indexes answers through its
/// leaf, and says so: goto-def and hover land on the stand-in, labeled; a
/// member found there is stamped widened; the diagnostic names both. Once
/// the module is indexed, the identity takes over.
#[cfg(feature = "python")]
#[test]
fn an_unindexed_identity_widens_to_its_leaf_out_loud() {
    use crate::model::file_analysis::MethodResolution;
    let files = [
        ("pkg/__init__.py", ""),
        ("pkg/legacy.py", "class User:\n    def name(self):\n        return 2\n"),
        ("pkg/vendored.py", "from vendor.lib import User as VU\n\nclass Sub(VU):\n    pass\n"),
    ];
    let (t, idx) = indexed("leafwide", &files);
    let vendored = t.0.join("pkg/vendored.py");
    let fa = idx.workspace_file_for_test(&vendored).unwrap();
    assert_eq!(fa.declared_parents("pkg.vendored.Sub"), ["vendor.lib.User"]);
    let (defs, hover, labels) = landing(&idx, &vendored, Point::new(2, 10));
    assert_eq!(defs, [("legacy.py".to_string(), 0)]);
    assert_eq!(hover.as_deref(), Some("legacy.py"));
    assert_eq!(labels, [Some("widened: `pkg.legacy.User` stands in for `vendor.lib.User`".to_string())]);
    assert!(matches!(
        member_on(&idx, &vendored, "pkg.vendored.Sub"),
        Some(MethodResolution::CrossFile { ref class, widened: true, .. }) if class == "pkg.legacy.User"
    ));
    let msg = "nothing indexed declares `vendor.lib.User`; resolved by its leaf to `pkg.legacy.User`".to_string();
    // at the class reference (an import's name is no class token here)
    assert_eq!(widening_codes(&idx, &vendored), [(2, 10, msg)]);

    // the module arrives: exact again, and quiet
    let mut with_vendor = files.to_vec();
    with_vendor.push(("vendor/__init__.py", ""));
    with_vendor.push(("vendor/lib.py", "class User:\n    def name(self):\n        return 3\n"));
    let (t, idx) = indexed("leafwide2", &with_vendor);
    let vendored = t.0.join("pkg/vendored.py");
    let (defs, _, labels) = landing(&idx, &vendored, Point::new(2, 10));
    assert_eq!(defs, [("lib.py".to_string(), 0)]);
    assert_eq!(labels, [None]);
    assert!(matches!(
        member_on(&idx, &vendored, "pkg.vendored.Sub"),
        Some(MethodResolution::CrossFile { ref class, widened: false, .. }) if class == "vendor.lib.User"
    ));
    assert!(widening_codes(&idx, &vendored).is_empty());
}

/// A class body is a namespace for the code written directly in it: a
/// nested class is filed under its enclosing class, a bare name there
/// that the class declares names the nested class first, and a method —
/// or a class nested one level further — sees past it to the module.
#[test]
fn a_class_body_is_a_namespace_for_its_own_code() {
    let m = module(&["pkg", "mod"], false);
    let src = "\
class Inner:
    pass
class Outer:
    class Inner:
        pass
    child: Inner = make()
    class Sub(Inner):
        peer: Inner = make()
    def build(self):
        x: Inner = make()
        class Local(Inner):
            pass
        return x
class Other(Outer.Inner):
    pass
";
    let fa = py_in(src, Some(&m));
    let classes: Vec<(&str, Option<&str>)> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Class)
        .map(|s| (s.name.as_str(), s.package.as_deref()))
        .collect();
    assert_eq!(
        classes,
        [
            ("pkg.mod.Inner", Some("pkg.mod")),
            ("pkg.mod.Outer", Some("pkg.mod")),
            ("pkg.mod.Outer.Inner", Some("pkg.mod.Outer")),
            ("pkg.mod.Outer.Sub", Some("pkg.mod.Outer")),
            // a class in a method joins the namespace the method sees
            ("pkg.mod.Local", Some("pkg.mod")),
            ("pkg.mod.Other", Some("pkg.mod")),
        ]
    );
    // directly in Outer's body: the nested class, in an annotation and a
    // parent clause alike
    assert_eq!(fa.inferred_type_via_bag("child", Point::new(5, 4)), Some(InferredType::ClassName("pkg.mod.Outer.Inner".into())));
    assert_eq!(fa.declared_parents("pkg.mod.Outer.Sub"), ["pkg.mod.Outer.Inner"]);
    // Sub's body and build's body see past Outer to the module's class
    assert_eq!(fa.inferred_type_via_bag("peer", Point::new(7, 8)), Some(InferredType::ClassName("pkg.mod.Inner".into())));
    assert_eq!(fa.inferred_type_via_bag("x", Point::new(9, 8)), Some(InferredType::ClassName("pkg.mod.Inner".into())));
    assert_eq!(fa.declared_parents("pkg.mod.Local"), ["pkg.mod.Inner"]);
    // a dotted spelling hangs its leaf off the class its head names
    assert_eq!(fa.declared_parents("pkg.mod.Other"), ["pkg.mod.Outer.Inner"]);
    // the lane holds the module alone: a class body is no namespace
    assert_eq!(fa.namespace_at(Point::new(5, 4)), Some("pkg.mod"));
    assert_eq!(fa.namespace_at(Point::new(9, 8)), Some("pkg.mod"));
    // a class header is written in the scope around the class, so the
    // reference there names what the edge does
    let header = fa.refs().iter().find(|r| r.span.start == Point::new(6, 14)).expect("the base is a reference");
    assert_eq!(fa.spelled_identity(header), "pkg.mod.Outer.Inner");
}

/// A base is written in the scope its class is declared in: `B(A)` in
/// `Outer`'s body names the sibling `Outer.A`, never a class B declares
/// itself, though B's own scope covers its header.
#[test]
fn a_parent_clause_names_a_sibling_nested_class() {
    let m = module(&["pkg", "mod"], false);
    let src = "\
class A:
    pass
class Outer:
    class A:
        pass
    class B(A):
        class A:
            pass
";
    let fa = py_in(src, Some(&m));
    assert_eq!(fa.declared_parents("pkg.mod.Outer.B"), ["pkg.mod.Outer.A"]);
    assert!(fa.symbols().iter().any(|s| s.kind == SymKind::Class && s.name == "pkg.mod.Outer.B.A"));
    let base = fa.refs().iter().find(|r| r.span.start == Point::new(5, 12)).expect("the base is a reference");
    assert_eq!(fa.spelled_identity(base), "pkg.mod.Outer.A");
}

/// A bare head no scope on the chain declares falls to the module, where
/// the file's imports bind first, exactly as at module level.
#[test]
fn a_head_the_class_does_not_declare_falls_to_the_module() {
    let m = module(&["pkg", "mod"], false);
    let src = "\
from lib.models import Imported
class Outer:
    class Inner:
        pass
    own: Helper = make()
    other: Imported = make()
";
    let fa = py_in(src, Some(&m));
    assert_eq!(fa.inferred_type_via_bag("own", Point::new(4, 4)), Some(InferredType::ClassName("pkg.mod.Helper".into())));
    assert_eq!(fa.inferred_type_via_bag("other", Point::new(5, 4)), Some(InferredType::ClassName("lib.models.Imported".into())));
}

/// A class attribute is the class's, not its methods': a bare read in a
/// method resolves to the module's variable of that name — the
/// declaration goto-def lands on and the type the bag answers — exactly
/// as Python reads it.
#[test]
fn a_method_reads_the_modules_variable_not_its_class_attribute() {
    let m = module(&["pkg", "mod"], false);
    let src = "\
class Inner:
    pass
x: Inner = make()
class Outer:
    class Inner:
        pass
    x: Inner = make()
    def build(self):
        return x
";
    let fa = py_in(src, Some(&m));
    // directly in the body, `x` is the class's own attribute
    assert_eq!(fa.inferred_type_via_bag("x", Point::new(6, 4)), Some(InferredType::ClassName("pkg.mod.Outer.Inner".into())));
    assert_eq!(fa.resolve_variable("x", Point::new(6, 5)).map(|s| s.span.start.row), Some(6));
    // in the method, it is the module's
    assert_eq!(fa.inferred_type_via_bag("x", Point::new(8, 15)), Some(InferredType::ClassName("pkg.mod.Inner".into())));
    assert_eq!(fa.resolve_variable("x", Point::new(8, 15)).map(|s| s.span.start.row), Some(2));
}

/// A class body whose only statement is a callable has that callable's
/// exact span. The body is written in the CLASS, so the section is the
/// class's — and a use in the method still reads past it.
#[test]
fn a_one_method_class_body_is_the_classs_section() {
    let m = module(&["pkg", "mod"], false);
    let src = "\
class Inner:
    pass
class Outer:
    def build(self):
        x: Inner = make()
        return x
";
    let fa = py_in(src, Some(&m));
    assert_eq!(fa.namespace_at(Point::new(4, 8)), Some("pkg.mod"));
    assert_eq!(fa.inferred_type_via_bag("x", Point::new(5, 15)), Some(InferredType::ClassName("pkg.mod.Inner".into())));
}

/// A document that does not mark its class scope keeps the chain it
/// always had: the same source, the same pack but for `@scope.direct`,
/// and a method reads its class's attribute. Where a class is filed is no
/// lookup: one declared in a method is the method's, not a member of the
/// class, either way.
#[test]
fn an_unmarked_class_scope_still_reaches_its_methods() {
    let m = module(&["pkg", "mod"], false);
    let mut pack = python_pack();
    let plain = pack.query_source.replace("@scope.direct", "@scope");
    pack.query_source = Box::leak(plain.into_boxed_str());
    let src = "\
class Inner:
    pass
x: Inner = make()
class Outer:
    class Inner:
        pass
    x: Inner = make()
    def build(self):
        class Local(Inner):
            pass
        return x
";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let fa = extract_in(&tree, src.as_bytes(), &pack, Some(&m)).unwrap().into_file_analysis();
    assert_eq!(fa.inferred_type_via_bag("x", Point::new(10, 15)), Some(InferredType::ClassName("pkg.mod.Outer.Inner".into())));
    assert_eq!(fa.resolve_variable("x", Point::new(10, 15)).map(|s| s.span.start.row), Some(6));
    assert!(fa.symbols().iter().any(|s| s.kind == SymKind::Class && s.name == "pkg.mod.Local"), "{:?}",
        fa.symbols().iter().filter(|s| s.kind == SymKind::Class).map(|s| s.name.as_str()).collect::<Vec<_>>());
    // the base in its header sees the class body the method sees
    assert_eq!(fa.declared_parents("pkg.mod.Local"), ["pkg.mod.Outer.Inner"]);
}

/// Without a module-level class of that leaf, a bare name in a method
/// names the module's (undeclared) class, never the nested one: the class
/// reference widens to it, out loud, as any undeclared identity does.
#[cfg(feature = "python")]
#[test]
fn a_method_never_reaches_its_class_body_by_a_bare_name() {
    let (t, idx) = indexed("nestedmethod", &[
        ("pkg/__init__.py", ""),
        ("pkg/mod.py", "class Outer:\n    class Inner:\n        pass\n    def build(self):\n        class Local(Inner):\n            pass\n"),
    ]);
    let path = t.0.join("pkg/mod.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    assert_eq!(fa.declared_parents("pkg.mod.Local"), ["pkg.mod.Inner"]);
    let (defs, _, labels) = landing(&idx, &path, Point::new(4, 20));
    assert_eq!(defs, [("mod.py".to_string(), 1)]);
    assert_eq!(labels, [Some("widened: `pkg.mod.Outer.Inner` stands in for `pkg.mod.Inner`".to_string())]);
    let msg = "nothing indexed declares `pkg.mod.Inner`; resolved by its leaf to `pkg.mod.Outer.Inner`".to_string();
    assert_eq!(widening_codes(&idx, &path), [(4, 20, msg)]);
}

/// Django's per-model `class Meta:` — one leaf, one class per model. Each
/// is an identity of its own: a reference reaches the one it names, a
/// rename rewrites only it, and members never union across models.
#[cfg(feature = "python")]
#[test]
fn same_leaf_nested_classes_stay_apart() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let models = "\
class Base:
    class Meta:
        def abstract(self): pass
class Article(Base):
    class Meta(Base.Meta):
        def ordering(self): pass
class Comment(Base):
    class Meta:
        def verbose(self): pass
class Audit(Comment.Meta):
    pass
";
    let (t, idx) = indexed("djangometa", &[("app/__init__.py", ""), ("app/models.py", models)]);
    let path = t.0.join("app/models.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let metas: Vec<&str> =
        fa.symbols().iter().filter(|s| s.kind == SymKind::Class && s.name.ends_with(".Meta")).map(|s| s.name.as_str()).collect();
    assert_eq!(metas, ["app.models.Base.Meta", "app.models.Article.Meta", "app.models.Comment.Meta"]);
    assert_eq!(fa.declared_parents("app.models.Article.Meta"), ["app.models.Base.Meta"]);

    // the reference in Article.Meta's header names Base.Meta alone
    let (defs, _, _) = landing(&idx, &path, Point::new(4, 20));
    assert_eq!(defs, [("models.py".to_string(), 1)]);
    let store = OpenDocs::new();
    let edits_from = |at: Point| {
        let cs = resolve(&store, &fa, FileKey::Path(path.clone()), at, Some(&idx), OverrideScope::default());
        let mut e: Vec<(usize, usize)> =
            cs.rename_edits("Options").unwrap().into_iter().map(|(l, _)| (l.span.start.row, l.span.start.column)).collect();
        e.sort();
        e
    };
    assert_eq!(edits_from(Point::new(4, 20)), [(1, 10), (4, 20)]);
    let (defs, _, _) = landing(&idx, &path, Point::new(9, 20));
    assert_eq!(defs, [("models.py".to_string(), 7)]);
    assert_eq!(edits_from(Point::new(9, 20)), [(7, 10), (9, 20)]);

    // the members each Meta declares, or inherits, of the three
    let members = |class: &str| -> Vec<String> {
        let mut v: Vec<String> = fa
            .complete_methods_for_class(class, None)
            .into_iter()
            .map(|c| c.label)
            .filter(|m| ["abstract", "ordering", "verbose"].contains(&m.as_str()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(members("app.models.Comment.Meta"), ["verbose"]);
    assert_eq!(members("app.models.Base.Meta"), ["abstract"]);
    // Article's Meta inherits Base's, and only Base's
    assert_eq!(members("app.models.Article.Meta"), ["abstract", "ordering"]);
}

/// `Outer.Inner` written in another module, after `from pkg.models import
/// Outer`, names the nested class by its identity: as a parent, as an
/// annotation, as the reference a rename rewrites, and as goto-def's
/// landing — never the same-leaf class of `pkg.other`.
#[cfg(feature = "python")]
#[test]
fn a_dotted_spelling_reaches_a_nested_class_across_modules() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use crate::model::file_analysis::MethodResolution;
    let (t, idx) = indexed("nestedxfile", &[
        ("pkg/__init__.py", ""),
        ("pkg/models.py", "class Outer:\n    class Inner:\n        def name(self):\n            return 1\n"),
        ("pkg/other.py", "class Inner:\n    def name(self):\n        return 2\n"),
        ("pkg/views.py", "from pkg.models import Outer\n\nclass V(Outer.Inner):\n    pass\n\nv: Outer.Inner = make()\n"),
    ]);
    let views = t.0.join("pkg/views.py");
    let fa = idx.workspace_file_for_test(&views).unwrap();
    assert_eq!(fa.declared_parents("pkg.views.V"), ["pkg.models.Outer.Inner"]);
    assert_eq!(
        fa.inferred_type_via_bag("v", Point::new(6, 0)),
        Some(InferredType::ClassName("pkg.models.Outer.Inner".to_string()))
    );
    assert!(matches!(
        member_on(&idx, &views, "pkg.views.V"),
        Some(MethodResolution::CrossFile { ref class, widened: false, .. }) if class == "pkg.models.Outer.Inner"
    ));
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(views.clone()), Point::new(2, 14), Some(&idx), OverrideScope::default());
    let mut edits: Vec<(String, usize, usize)> = cs
        .rename_edits("Nested")
        .unwrap()
        .into_iter()
        .map(|(l, _)| match &l.key {
            FileKey::Path(p) => (p.file_name().unwrap().to_string_lossy().to_string(), l.span.start.row, l.span.start.column),
            FileKey::Url(u) => (u.to_string(), l.span.start.row, l.span.start.column),
        })
        .collect();
    edits.sort();
    assert_eq!(edits, [("models.py".to_string(), 1, 10), ("views.py".to_string(), 2, 14)]);
    assert!(widening_codes(&idx, &views).is_empty());
    let (defs, hover, labels) = landing(&idx, &views, Point::new(2, 14));
    assert_eq!(defs, [("models.py".to_string(), 1)], "goto-def lands on the nested class");
    assert_eq!(hover.as_deref(), Some("models.py"));
    assert_eq!(labels, [None]);
}
