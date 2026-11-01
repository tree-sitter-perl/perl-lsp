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
        FileKey::Url(u, _) => u.to_string(),
    };
    let want_edits: Vec<(String, usize, usize)> =
        [("models.py", 0, 6), ("views.py", 0, 23), ("views.py", 2, 12), ("views.py", 5, 4)]
            .iter()
            .map(|(f, r, c)| (f.to_string(), *r, *c))
            .collect();
    // the parent clause, the import's name, the constructing call
    for (row, column) in [(2, 12), (0, 23), (5, 4)] {
        let at = Point { row, column };
        let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p_views.clone())), at, Some(&idx), OverrideScope::default());
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

/// A value inside an assignment's right-hand side is not the variable's:
/// `w = Widget()[0]` is an element, not a `Widget`. A transparent wrapper
/// still holds its operand's value.
#[test]
fn a_value_inside_the_right_hand_side_is_not_the_variables() {
    let src = "class Widget:\n    pass\n\nw = Widget()[0]\np = (Widget())\nq = ((p))\n";
    let fa = py_in(src, None);
    let widget = Some(InferredType::ClassName("Widget".to_string()));
    assert_eq!(fa.inferred_type_via_bag("w", Point::new(6, 0)), None);
    assert_eq!(fa.inferred_type_via_bag("p", Point::new(6, 0)), widget);
    assert_eq!(fa.inferred_type_via_bag("q", Point::new(6, 0)), widget, "parentheses hold a variable's value too");
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
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path)), Point::new(2, 11), Some(&idx), OverrideScope::default());
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
        FileKey::Url(u, _) => u.to_string(),
    };
    let fa = idx.workspace_file_for_test(path).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path.to_path_buf())), at, Some(idx), OverrideScope::default());
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
    // (each declared type holds from the end of its write)
    assert_eq!(fa.inferred_type_via_bag("peer", Point::new(7, 28)), Some(InferredType::ClassName("pkg.mod.Inner".into())));
    assert_eq!(fa.inferred_type_via_bag("x", Point::new(9, 25)), Some(InferredType::ClassName("pkg.mod.Inner".into())));
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

/// A def's header runs where the def is written. In a method of `Outer`
/// the parameter annotation, the default, the return annotation, like a
/// sibling's base list, name `Outer`'s nested class; the method's body
/// sees past the class to the module, and the parameter names are the
/// method's locals.
#[test]
fn a_header_resolves_in_the_scope_its_def_is_written_in() {
    use crate::model::file_analysis::{RefKind, ScopeKind};
    let m = module(&["pkg", "mod"], false);
    let src = "\
class Inner:
    pass
class Outer:
    class Inner:
        pass
    class Sub(Inner):
        pass
    def m(self, x: Inner = Inner()) -> Inner:
        return Inner()
";
    let fa = py_in(src, Some(&m));
    let named_at = |row: usize, column: usize| {
        let r = fa.refs().iter().find(|r| r.span.start == Point::new(row, column)).expect("a reference");
        fa.spelled_identity(r)
    };
    // the base list, the annotation, the default, the return annotation
    assert_eq!(fa.declared_parents("pkg.mod.Outer.Sub"), ["pkg.mod.Outer.Inner"]);
    for (at, what) in [((5, 14), "base"), ((7, 19), "annotation"), ((7, 27), "default"), ((7, 39), "return annotation")] {
        assert_eq!(named_at(at.0, at.1), "pkg.mod.Outer.Inner", "the {what}");
    }
    let default = fa.refs().iter().find(|r| r.span.start == Point::new(7, 27) && matches!(r.kind, RefKind::FunctionCall)).unwrap();
    assert_eq!(fa.class_named_by_call(default, None).as_deref(), Some("pkg.mod.Outer.Inner"));
    // the body reads past the class
    assert_eq!(named_at(8, 15), "pkg.mod.Inner");
    // the parameters bind in the method, not in the class body
    let x = fa.resolve_variable("x", Point::new(8, 8)).expect("x is in scope in the body");
    assert!(matches!(fa.scope(x.scope).kind, ScopeKind::Sub { .. }), "{:?}", fa.scope(x.scope));
    assert!(fa.resolve_variable("x", Point::new(4, 8)).is_none());
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
        let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path.clone())), at, Some(&idx), OverrideScope::default());
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
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(views.clone())), Point::new(2, 14), Some(&idx), OverrideScope::default());
    let mut edits: Vec<(String, usize, usize)> = cs
        .rename_edits("Nested")
        .unwrap()
        .into_iter()
        .map(|(l, _)| match &l.key {
            FileKey::Path(p) => (p.file_name().unwrap().to_string_lossy().to_string(), l.span.start.row, l.span.start.column),
            FileKey::Url(u, _) => (u.to_string(), l.span.start.row, l.span.start.column),
        })
        .collect();
    edits.sort();
    // the parent clause and the annotation alike
    assert_eq!(
        edits,
        [("models.py".to_string(), 1, 10), ("views.py".to_string(), 2, 14), ("views.py".to_string(), 5, 9)]
    );
    assert!(widening_codes(&idx, &views).is_empty());
    let (defs, hover, labels) = landing(&idx, &views, Point::new(2, 14));
    assert_eq!(defs, [("models.py".to_string(), 1)], "goto-def lands on the nested class");
    assert_eq!(hover.as_deref(), Some("models.py"));
    assert_eq!(labels, [None]);
    let (defs, _, _) = landing(&idx, &views, Point::new(5, 9));
    assert_eq!(defs, [("models.py".to_string(), 1)], "and from the annotation");
}

/// A rename from a Python class's own name token, or from a local, writes
/// each site's whole token: a pack's names carry no sigil for the edit to
/// step past (`php_pack_tests::a_cpp_local_renames_whole`).
#[cfg(feature = "python")]
#[test]
fn a_python_rename_writes_whole_tokens() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let src = "class Foo:\n    pass\n\ndef helper(n):\n    total = n\n    return total\n";
    let (t, idx) = indexed("wholetokens", &[("app/__init__.py", ""), ("app/mod.py", src)]);
    let path = t.0.join("app/mod.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let store = OpenDocs::new();
    let edits_from = |row: usize, column: usize| {
        let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), Point { row, column }, Some(&idx), OverrideScope::default());
        let mut e: Vec<(usize, usize, usize)> = cs
            .rename_edits("Renamed")
            .unwrap()
            .into_iter()
            .map(|(l, text)| {
                assert_eq!(text, "Renamed");
                (l.span.start.row, l.span.start.column, l.span.end.column)
            })
            .collect();
        e.sort();
        e
    };
    assert_eq!(edits_from(0, 7), [(0, 6, 9)], "the class's own name token");
    assert_eq!(edits_from(4, 5), [(4, 4, 9), (5, 11, 16)], "a local, from its binding");
    assert_eq!(edits_from(5, 12), [(4, 4, 9), (5, 11, 16)], "a local, from a read");
}

const MEMBERS: &str = "\
class Counter:
    count = 0
    a: int
    b: int = 5
    total = count + 1
    def __init__(self, other):
        self.n = 1
        other.z = 1
        self.count += 1
    def bump(self):
        self.n = 2
        return self.n + count

c = Counter()
x = c.a
";

fn members_fa() -> FileAnalysis {
    py_in(MEMBERS, Some(&module(&["pkg", "mod"], false)))
}

/// The class's members as (name, row, is an instance slot): a class-body
/// field, or a class-owned key a receiver write declares.
fn fields_of(fa: &FileAnalysis) -> Vec<(&str, usize, bool)> {
    fa.symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Field | SymKind::HashKeyDef) && s.package.as_deref() == Some("pkg.mod.Counter"))
        .map(|s| (s.name.as_str(), s.span.start.row, s.kind == SymKind::HashKeyDef))
        .collect()
}

/// A name a class body binds, annotated or assigned, is a field of the
/// class, and its hover shows the annotated type.
#[test]
fn a_class_body_binding_is_a_field_of_the_class() {
    let fa = members_fa();
    let fields = fields_of(&fa);
    for name in ["count", "a", "b", "total"] {
        assert!(fields.iter().any(|&(n, _, through)| n == name && !through), "{name} in {fields:?}");
    }
    for (at, line) in [(Point::new(2, 4), "a: int"), (Point::new(3, 4), "b: int = 5"), (Point::new(14, 6), "a: int")] {
        let hover = fa.hover_info(at, MEMBERS, None).unwrap_or_default();
        assert!(hover.contains(line) && hover.contains("*type: Numeric*"), "{at:?}: {hover}");
    }
}

/// A class-body binding's Variable twin is a bare name for the code
/// written directly in the body, and for nothing nested in it.
#[test]
fn a_class_body_binding_is_a_bare_name_in_the_body_only() {
    let fa = members_fa();
    let read = Point::new(4, 12);
    let r = fa.refs().iter().find(|r| r.span.start == read).expect("the read is a reference");
    let bound = r.resolved_symbol().map(|sid| fa.symbol(sid)).expect("the read binds");
    assert_eq!((bound.kind, bound.span.start.row), (SymKind::Variable, 1));
    let field = fa.symbol(bound.declared_with.expect("declared with its field"));
    assert_eq!((field.kind, field.package.as_deref()), (SymKind::Field, Some("pkg.mod.Counter")));
    // the method reads past the class body
    assert!(fa.resolve_variable("count", Point::new(11, 24)).is_none());
}

/// The first write through a method's receiver declares an instance slot
/// on the receiver's class; later writes declare nothing, a write through
/// an untyped name declares nothing, a write of a class-body name declares
/// nothing, and the slot is no bare name.
#[test]
fn the_first_write_through_self_declares_an_instance_slot() {
    let fa = members_fa();
    let ns: Vec<_> = fields_of(&fa).into_iter().filter(|&(n, ..)| n == "n").collect();
    assert_eq!(ns, [("n", 6, true)]);
    assert!(!fa.symbols().iter().any(|s| s.name == "z"), "a write through an untyped parameter declares nothing");
    assert!(!fa.symbols().iter().any(|s| s.name == "count" && s.span.start.row != 1));
    assert!(fa.resolve_variable("n", Point::new(11, 20)).is_none());
    assert!(fa.resolve_variable("n", Point::new(12, 0)).is_none());
}

/// In-scope completion offers what a bare name can reach from the
/// cursor: the class body's names to code written directly in it, never
/// to its methods, and never a member written through `self`.
#[test]
fn in_scope_completion_follows_the_lookup_chain() {
    let fa = members_fa();
    let labels = |row: usize, column: usize| -> std::collections::HashSet<String> {
        crate::lsp::symbols::in_scope_completion(&fa, Point::new(row, column)).into_iter().map(|c| c.label).collect()
    };
    let body = labels(4, 4);
    for name in ["count", "a", "b"] {
        assert!(body.contains(name), "{name} in the class body: {body:?}");
    }
    assert!(!body.contains("n"), "a member through self is no bare name: {body:?}");
    let method = labels(11, 8);
    assert!(method.contains("self") && method.contains("c"), "{method:?}");
    for name in ["count", "a", "b", "total", "n"] {
        assert!(!method.contains(name), "{name} is the class body's, not the method's: {method:?}");
    }
}

/// A write through any receiver declares on the class the receiver is
/// typed as: an annotated parameter or local names the owner as `self`
/// does, and an untyped receiver declares nothing.
#[test]
fn a_write_through_a_typed_receiver_declares_on_its_type() {
    let src = "\
class Counter:
    pass

def reset(other: Counter, o):
    other.z = 1
    o.w = 1
    local: Counter = make()
    local.y = 1
    other.z = 2
";
    let fa = py_in(src, Some(&module(&["pkg", "mod"], false)));
    let mut fields = fields_of(&fa);
    fields.sort();
    assert_eq!(fields, [("y", 7, true), ("z", 4, true)]);
    assert!(!fa.symbols().iter().any(|s| s.name == "w"), "an untyped receiver declares nothing");
    let later = fa.refs().iter().find(|r| r.span.start == Point::new(8, 10)).expect("the later write is a reference");
    assert_eq!(later.target_name, "z");
}

/// A local typed only by its value is typed by the time slots are
/// settled: `o = Counter()` makes `o.z = 1` a write of `Counter.z`, and a
/// read through it lands there.
#[test]
fn a_write_through_a_constructed_local_declares_on_its_class() {
    let src = "\
class Counter:
    pass

o = Counter()
o.z = 1
x = o.z
";
    let fa = py_in(src, Some(&module(&["pkg", "mod"], false)));
    assert_eq!(fields_of(&fa), [("z", 4, true)]);
    let read = fa.refs().iter().find(|r| r.span.start == Point::new(5, 6)).expect("the read is a reference");
    assert_eq!(read.target_name, "z");
    #[cfg(feature = "python")]
    {
        let (t, idx) = indexed("pyvaluetyped", &[("pkg/__init__.py", ""), ("pkg/mod.py", src)]);
        let (defs, _, _) = landing(&idx, &t.0.join("pkg/mod.py"), Point::new(5, 6));
        assert_eq!(defs, [("mod.py".to_string(), 4)]);
    }
}

/// Member completion on an instance offers the class-body fields, the
/// instance fields and the methods.
#[test]
fn instance_completion_offers_every_member() {
    let fa = members_fa();
    let labels: std::collections::HashSet<String> =
        fa.complete_members_for_class("pkg.mod.Counter", None, None).into_iter().map(|c| c.label).collect();
    for name in ["a", "b", "count", "total", "n", "bump", "__init__"] {
        assert!(labels.contains(name), "{name} in {labels:?}");
    }
}

/// goto-def on a member written or read through `self` lands on the field
/// that declares it.
#[cfg(feature = "python")]
#[test]
fn a_member_through_self_lands_on_its_field() {
    let (t, idx) = indexed("pymembers", &[("pkg/__init__.py", ""), ("pkg/mod.py", MEMBERS)]);
    let path = t.0.join("pkg/mod.py");
    for (at, row) in [(Point::new(8, 14), 1), (Point::new(10, 13), 6), (Point::new(11, 20), 6), (Point::new(14, 6), 2)] {
        let (defs, _, _) = landing(&idx, &path, at);
        assert_eq!(defs, [("mod.py".to_string(), row)], "goto-def from {at:?}");
    }
}

/// A class-body binding with a value is the class's own attribute; a bare
/// annotation declares an instance's and stays unmarked.
#[test]
fn a_valued_class_body_binding_is_static() {
    use crate::model::file_analysis::SymbolFlags;
    let fa = members_fa();
    let field = |name: &str| {
        fa.symbols()
            .iter()
            .find(|s| s.kind == SymKind::Field && s.name == name)
            .unwrap_or_else(|| panic!("{name} is a field"))
    };
    for name in ["count", "b", "total"] {
        assert!(field(name).flags.contains(SymbolFlags::STATIC), "{name}");
    }
    assert!(!field("a").flags.contains(SymbolFlags::STATIC));
}

const SLOTS: &str = "\
class Counter:
    count = 0
    def __init__(self):
        self.n = 1
    def bump(self):
        self.n = 2
        self.count = 5
    b = count + 1

c = Counter()
x = c.n
y = Counter.count
";

/// Every edit a rename to `new` from `at` writes, as (row, column).
#[cfg(feature = "python")]
fn renamed_at(
    idx: &crate::index::module_index::ModuleIndex,
    path: &std::path::Path,
    at: Point,
    new: &str,
) -> Vec<(usize, usize)> {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let fa = idx.workspace_file_for_test(path).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path.to_path_buf())), at, Some(idx), OverrideScope::default());
    let mut e: Vec<(usize, usize)> = cs
        .rename_edits(new)
        .unwrap_or_default()
        .into_iter()
        .map(|(l, text)| {
            assert_eq!(text, new);
            (l.span.start.row, l.span.start.column)
        })
        .collect();
    e.sort();
    e.dedup();
    e
}

/// A write through `self` declares one instance slot of the class — a
/// class-owned key, as Perl's `$self->{n}` — however many methods write
/// it, and none for a name the class body binds. A read through an
/// instance lands on it and a rename from any spelling moves them all.
#[cfg(feature = "python")]
#[test]
fn a_write_through_self_declares_one_class_owned_slot() {
    use crate::model::file_analysis::{HashKeyOwner, SymbolDetail};
    let (t, idx) = indexed("pyslots", &[("pkg/__init__.py", ""), ("pkg/mod.py", SLOTS)]);
    let path = t.0.join("pkg/mod.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let mut members = fields_of(&fa);
    members.sort();
    assert_eq!(members, [("b", 7, false), ("count", 1, false), ("n", 3, true)]);
    let slot = fa.symbols().iter().find(|s| s.kind == SymKind::HashKeyDef).unwrap();
    assert!(matches!(
        &slot.detail,
        SymbolDetail::HashKeyDef { owner: HashKeyOwner::Class(c), .. } if c == "pkg.mod.Counter"
    ));
    for at in [Point::new(5, 13), Point::new(10, 6)] {
        let (defs, _, _) = landing(&idx, &path, at);
        assert_eq!(defs, [("mod.py".to_string(), 3)], "goto-def from {at:?}");
    }
    let all = [(3, 13), (5, 13), (10, 6)];
    assert_eq!(renamed_at(&idx, &path, Point::new(10, 6), "m"), all);
    assert_eq!(renamed_at(&idx, &path, Point::new(3, 13), "m"), all);
}

/// A class-body binding is two names from one token: the class's member
/// `Counter.count` and the bare name the body reads. Each spelling binds
/// its own, and a rename from either moves both.
#[cfg(feature = "python")]
#[test]
fn a_class_body_binding_is_a_member_and_a_bare_name_renamed_together() {
    let (t, idx) = indexed("pytwin", &[("pkg/__init__.py", ""), ("pkg/mod.py", SLOTS)]);
    let path = t.0.join("pkg/mod.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let read = fa.refs().iter().find(|r| r.span.start == Point::new(7, 8)).expect("the body read is a reference");
    assert_eq!(read.resolved_symbol().map(|sid| fa.symbol(sid).kind), Some(SymKind::Variable));
    for at in [Point::new(7, 8), Point::new(11, 12), Point::new(6, 13)] {
        let (defs, _, _) = landing(&idx, &path, at);
        assert_eq!(defs, [("mod.py".to_string(), 1)], "goto-def from {at:?}");
    }
    let all = [(1, 4), (6, 13), (7, 8), (11, 12)];
    for at in [Point::new(1, 4), Point::new(7, 8), Point::new(11, 12)] {
        assert_eq!(renamed_at(&idx, &path, at, "total"), all, "rename from {at:?}");
    }
}

/// A subclass reaches its base's class-body binding as the same group,
/// across files: a rename from the subclass's read moves the member and
/// the bare name the base's body reads.
#[cfg(feature = "python")]
#[test]
fn an_inherited_class_body_binding_renames_as_one_group_across_files() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let base = "\
class Base:
    count = 0
    double = count * 2
";
    let sub = "\
from pkg.base import Base

class Sub(Base):
    def go(self):
        n = self.count
        return n
";
    let (t, idx) = indexed("pyinherit", &[("pkg/__init__.py", ""), ("pkg/base.py", base), ("pkg/sub.py", sub)]);
    let edits = |file: &str, at: Point| {
        let path = t.0.join(file);
        let fa = idx.workspace_file_for_test(&path).unwrap();
        let store = OpenDocs::new();
        let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path.clone())), at, Some(&idx), OverrideScope::default());
        let mut e: Vec<(String, usize, usize)> = cs
            .rename_edits("total")
            .unwrap_or_default()
            .into_iter()
            .map(|(l, _)| {
                let name = match &l.key {
                    FileKey::Path(p) => p.file_name().unwrap().to_string_lossy().to_string(),
                    FileKey::Url(u, _) => u.to_string(),
                };
                (name, l.span.start.row, l.span.start.column)
            })
            .collect();
        e.sort();
        e.dedup();
        e
    };
    let all = [("base.py".to_string(), 1, 4), ("base.py".to_string(), 2, 13), ("sub.py".to_string(), 4, 17)];
    assert_eq!(edits("pkg/sub.py", Point::new(4, 17)), all, "from the subclass's read");
    assert_eq!(edits("pkg/base.py", Point::new(2, 13)), all, "from the base's body read");
}

/// A subclass's write of a name its base declares — by a class-body
/// binding or a slot of its own — is a use of the base's member.
#[test]
fn a_write_of_an_inherited_name_declares_no_slot() {
    let src = "\
class Base:
    n = 0
    def __init__(self):
        self.k = 1
class Sub(Base):
    def go(self):
        self.n = 1
        self.k = 2
        self.m = 3
";
    let fa = py_in(src, Some(&module(&["pkg", "mod"], false)));
    let mut slots: Vec<(&str, Option<&str>)> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::HashKeyDef)
        .map(|s| (s.name.as_str(), s.package.as_deref()))
        .collect();
    slots.sort();
    assert_eq!(slots, [("k", Some("pkg.mod.Base")), ("m", Some("pkg.mod.Sub"))]);
}

/// Member completion after `obj.` offers the instance's slots alongside
/// the class's fields and methods.
#[cfg(feature = "python")]
#[test]
fn completion_after_an_instance_offers_slots_and_members() {
    let src = "\
class Counter:
    count = 0
    def __init__(self):
        self.n = 1
    def bump(self):
        pass

def f(c: Counter):
    c.
";
    let (t, idx) = indexed("pycomplete", &[("pkg/__init__.py", ""), ("pkg/mod.py", src)]);
    let path = t.0.join("pkg/mod.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let store = crate::index::open_docs::OpenDocs::new();
    let (items, _) =
        crate::lsp::backend::pack_completion(&store, &fa, src, &tree, Point::new(8, 6), "python", Some(&crate::util::canonical_path::CanonicalPath::resolve(path.clone())), &idx);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    for name in ["n", "count", "bump", "__init__"] {
        assert!(labels.contains(&name), "{name} in {labels:?}");
    }
    assert_eq!(labels.iter().filter(|&&l| l == "count").count(), 1, "one entry per member: {labels:?}");
}

/// A member read that is the last token of a body is still a member
/// read: a def's node ends there, its name does not.
#[test]
fn a_member_read_ending_a_body_is_a_member_read() {
    use crate::model::file_analysis::RefKind;
    let src = "\
class A:
    def go(self):
        return self.count
    def put(self):
        y = self.count
    n = A.count

def f(o):
    return o.count
";
    let fa = py_in(src, Some(&module(&["pkg", "mod"], false)));
    let mut reads: Vec<(usize, bool)> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "count")
        .map(|r| (r.span.start.row, matches!(r.kind, RefKind::FieldAccess { .. })))
        .collect();
    reads.sort();
    assert_eq!(reads, [(2, true), (4, true), (5, true), (8, true)]);
}

/// A write through `self` states the slot's value: a read of the slot,
/// in a method above the write, through a subclass in another file, or
/// on an annotated instance in a third, has the written value's type.
#[cfg(feature = "python")]
#[test]
fn a_slot_write_states_the_slots_value() {
    let holder = "\
class Widget:
    def spin(self):
        return 1

class Holder:
    def early(self):
        return self.made.spin()
    def __init__(self):
        self.made = Widget()
";
    let child = "\
from pkg.holder import Holder

class Child(Holder):
    def go(self):
        return self.made.spin()
";
    let main = "\
from pkg.holder import Holder

def f(h: Holder):
    return h.made.spin()
";
    let (t, idx) = indexed("pyslotvalue", &[
        ("pkg/__init__.py", ""),
        ("pkg/holder.py", holder),
        ("pkg/child.py", child),
        ("pkg/main.py", main),
    ]);
    for (file, at) in [("pkg/holder.py", Point::new(6, 25)), ("pkg/child.py", Point::new(4, 25)), ("pkg/main.py", Point::new(3, 18))] {
        let (defs, _, _) = landing(&idx, &t.0.join(file), at);
        assert_eq!(defs, [("holder.py".to_string(), 1)], "goto-def on spin in {file}");
    }
}

/// A write to a member is an assignment to it, through any receiver: a
/// declared attribute keeps its annotation over what is written to it,
/// and an undeclared one whose writes disagree on the class is either.
#[cfg(feature = "python")]
#[test]
fn a_member_write_is_an_assignment_to_the_member() {
    let src = "\
class Widget:
    def spin(self):
        return 1

class Gadget:
    def spin(self):
        return 2

class Holder:
    made: Widget
    def __init__(self):
        self.made = Gadget()
        self.either = Widget()
    def swap(self):
        self.either = Gadget()
    def go(self):
        return self.made.spin()
    def maybe(self):
        return self.either.spin()

def fill(h: Holder):
    h.kept = Gadget()

def use(h: Holder):
    return h.kept.spin()

class Outer:
    def __init__(self):
        self.inner = Holder()
    def fill(self):
        self.inner.deep = Gadget()
    def go(self):
        return self.inner.deep.spin()
";
    let (t, idx) = indexed("pymemberwrite", &[("pkg/__init__.py", ""), ("pkg/mod.py", src)]);
    let path = t.0.join("pkg/mod.py");
    let (defs, _, _) = landing(&idx, &path, Point::new(16, 25));
    assert_eq!(defs, [("mod.py".to_string(), 1)], "the annotation governs `made`");
    let (defs, _, _) = landing(&idx, &path, Point::new(18, 27));
    assert_eq!(
        defs,
        [("mod.py".to_string(), 5), ("mod.py".to_string(), 1)],
        "`either` is written a Widget and a Gadget, so it dispatches on both",
    );
    let (defs, _, _) = landing(&idx, &path, Point::new(24, 18));
    assert_eq!(defs, [("mod.py".to_string(), 5)], "a write through a parameter states the value");
    let (defs, _, _) = landing(&idx, &path, Point::new(32, 32));
    assert_eq!(defs, [("mod.py".to_string(), 5)], "a nested write states the value");
}

/// A call is typed by what its callee returns: the `-> T` it declares,
/// spelled through the def's own imports, else every `return` agreeing,
/// in the defining file and from another one.
#[cfg(feature = "python")]
#[test]
fn a_call_is_typed_by_its_callees_return() {
    let lib = "\
from pkg.widgets import Widget

class Gadget:
    def spin(self):
        return 2

def make():
    return Gadget()

def declared() -> Widget:
    return build()

def nothing() -> None:
    pass

g = make()
g.spin()
";
    let widgets = "\
class Widget:
    def spin(self):
        return 1
";
    let main = "\
from pkg.lib import make, declared

def f():
    make().spin()
    declared().spin()
";
    let (t, idx) = indexed("pyreturns", &[
        ("pkg/__init__.py", ""),
        ("pkg/widgets.py", widgets),
        ("pkg/lib.py", lib),
        ("pkg/main.py", main),
    ]);
    for (file, at, home) in [
        ("pkg/lib.py", Point::new(16, 2), ("lib.py", 3)),
        ("pkg/main.py", Point::new(3, 11), ("lib.py", 3)),
        ("pkg/main.py", Point::new(4, 15), ("widgets.py", 1)),
    ] {
        let (defs, _, _) = landing(&idx, &t.0.join(file), at);
        assert_eq!(defs, [(home.0.to_string(), home.1)], "goto-def on spin in {file} at {at:?}");
    }
    let fa = idx.workspace_file_for_test(&t.0.join("pkg/lib.py")).unwrap();
    assert_eq!(fa.sub_return_type_at_arity("nothing", Some(0)), None, "`-> None` names no class");
}

/// Calling a class another file declares constructs it: the call is an
/// instance of the class its spelling names through the file's imports,
/// whether the call is assigned, chained, or returned by a def.
#[cfg(feature = "python")]
#[test]
fn calling_a_class_from_another_file_constructs_it() {
    let widgets = "\
class Widget:
    def spin(self):
        return 1

class Holder:
    def __init__(self):
        self.made = Widget()
";
    let main = "\
from pkg.widgets import Widget, Holder

w = Widget()
w.spin()
Widget().spin()
Holder().made.spin()

def make():
    return Widget()

make().spin()
";
    let (t, idx) = indexed("pyclasscall", &[
        ("pkg/__init__.py", ""),
        ("pkg/widgets.py", widgets),
        ("pkg/main.py", main),
    ]);
    let path = t.0.join("pkg/main.py");
    for at in [Point::new(3, 2), Point::new(4, 9), Point::new(5, 14), Point::new(10, 7)] {
        let (defs, _, _) = landing(&idx, &path, at);
        assert_eq!(defs, [("widgets.py".to_string(), 1)], "goto-def on spin at {at:?}");
    }
}

/// An assignment's left side is written: `self.n = 0` and `self.n += 1`
/// write the member, and a read of it stays a read.
#[test]
fn a_member_an_assignment_writes_is_a_write() {
    use crate::model::file_analysis::{AccessKind, RefKind};
    let fa = py_in(
        "class C:
    def __init__(self):
        self.n = 0
    def bump(self):
        self.n += 1
        return self.n
",
        None,
    );
    let n: Vec<(usize, AccessKind)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::FieldAccess { .. }) && r.target_name == "n")
        .map(|r| (r.span.start.row, r.access))
        .collect();
    assert_eq!(n, [(2, AccessKind::Write), (4, AccessKind::Write), (5, AccessKind::Read)]);
}

/// A write lands once the assignment has read what it reads, and replaces
/// what the variable held: `n = n.nxt` reads the old `n`, and after
/// `x = {}` the variable is no longer the `Foo` it was.
#[test]
fn a_write_replaces_the_value_after_its_right_side_is_read() {
    let src = "class Foo:
    nxt: Foo
def f():
    x = Foo()
    x = {}
    n = Foo()
    n = n.nxt
    return x
";
    let fa = py_in(src, None);
    let foo = Some(InferredType::ClassName("Foo".into()));
    assert_ne!(fa.inferred_type_via_bag("x", Point::new(7, 11)), foo, "the later write replaces it");
    assert_eq!(fa.inferred_type_via_bag("n", Point::new(7, 4)), foo, "the right side read the old `n`");
}

/// A variable is the function's it is assigned in: the first write
/// declares it, and every later write and every read is that one variable.
/// A nested function that assigns the name makes its own, and reads the
/// one around it until it does.
#[test]
fn a_variable_is_one_across_its_writes_in_its_function() {
    use crate::model::file_analysis::{RefKind, SymKind};
    let fa = py_in(
        "x = 0
def f():
    x = 1
    if x:
        x = 2
    def g():
        return x
    def h():
        x = 3
        return x
    return x
",
        None,
    );
    let decls: Vec<(usize, usize)> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Variable && s.name == "x")
        .map(|s| (s.selection_span.start.row, s.selection_span.start.column))
        .collect();
    assert_eq!(decls, [(0, 0), (2, 4), (8, 8)], "the module's, f's and h's");
    let decl_row = |r: &crate::model::file_analysis::Ref| {
        r.resolved_symbol().map(|sid| fa.symbol(sid).selection_span.start.row)
    };
    let mut uses: Vec<(usize, Option<usize>)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Variable) && r.target_name == "x")
        .map(|r| (r.span.start.row, decl_row(r)))
        .collect();
    uses.sort();
    assert_eq!(
        uses,
        [(3, Some(2)), (4, Some(2)), (6, Some(2)), (9, Some(8)), (10, Some(2))],
        "the write in a branch, g's read and f's return are f's; h's are h's"
    );
}

/// `__init__` is the constructor by its flag, so a class that declares
/// none constructs through the one it inherits; a module-level function
/// of that name is no constructor.
#[test]
fn init_is_the_constructor_a_class_inherits() {
    let m = module(&["app"], false);
    let fa = py_in(
        "class Base:\n    def __init__(self, n):\n        pass\nclass Kid(Base):\n    pass\nclass Deco:\n    @staticmethod\n    def __init__(self):\n        pass\ndef __init__():\n    pass\n",
        Some(&m),
    );
    let flagged: Vec<(usize, bool)> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "__init__")
        .map(|s| (s.selection_span.start.row, s.is_constructor()))
        .collect();
    assert_eq!(flagged, [(1, true), (7, true), (9, false)]);
    assert_eq!(fa.constructor_of("app.Kid", None), Some(("app.Base".to_string(), "__init__".to_string())));
    assert_eq!(fa.constructor_of("app.Deco", None), Some(("app.Deco".to_string(), "__init__".to_string())));
}

/// A name `from m import` binds is a reference to the def it names, pinned
/// as a call to it is, but its kind says it is an import: it calls
/// nothing, so no call lane reads it and it never constructs the class
/// it names.
#[test]
fn an_imported_name_is_its_own_kind_pinned_like_a_call() {
    use crate::model::file_analysis::RefKind;
    let m = module(&["app"], false);
    let fa = py_in("from lib.model import Model
m = Model()
", Some(&m));
    let sites: Vec<(usize, bool, Option<&str>)> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "Model" && matches!(r.kind, RefKind::FunctionCall | RefKind::ImportedName))
        .map(|r| (r.span.start.row, matches!(r.kind, RefKind::FunctionCall), r.resolved_package()))
        .collect();
    assert_eq!(sites.iter().map(|&(row, call, _)| (row, call)).collect::<Vec<_>>(), [(0, false), (1, true)]);
    assert_eq!(sites[0].2, sites[1].2, "the import is pinned as the call is");
}

/// A receiver expression is typed by its value, never read as a class
/// spelling: `self.either` (written two classes) is either class,
/// `self.unknown` (never written) types nothing, and a class name read
/// still types the class.
#[cfg(feature = "python")]
#[test]
fn an_attribute_receiver_is_no_class_spelling() {
    let src = "\
class Widget:
    def spin(self):
        return 1

class Gadget:
    def spin(self):
        return 2

class Holder:
    def __init__(self):
        self.either = Widget()
    def swap(self):
        self.either = Gadget()
    def maybe(self):
        return self.either.spin()
    def gone(self):
        return self.unknown.spin()

def make():
    return Widget.spin(Widget())
";
    let (t, idx) = indexed("pyattrrecv", &[("pkg/__init__.py", ""), ("pkg/mod.py", src)]);
    let path = t.0.join("pkg/mod.py");
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let recv = |at: Point| fa.method_call_invocant_type(fa.ref_at(at).unwrap(), Some(&idx));
    let either = recv(Point::new(14, 27)).expect("a slot written two classes");
    let mut classes: Vec<String> =
        either.receiver_arms().iter().filter_map(|a| a.class_name().map(str::to_string)).collect();
    classes.sort();
    assert_eq!(classes, ["pkg.mod.Gadget", "pkg.mod.Widget"], "a slot written two classes is either");
    assert_eq!(recv(Point::new(16, 28)), None, "a slot never written");
    assert_eq!(
        recv(Point::new(19, 18)).and_then(|t| t.class_name().map(str::to_string)).as_deref(),
        Some("pkg.mod.Widget"),
        "a class name read",
    );
}

/// A method's receiver is its first parameter, whatever it is called; a
/// `@staticmethod` and a free function have none, whatever theirs is.
#[cfg(feature = "python")]
#[test]
fn a_receiver_is_a_methods_first_parameter_by_position() {
    let src = "\
class A:
    def m(this, y):
        return this
    @staticmethod
    def s(self):
        return self
    @classmethod
    def c(klass: \"A\"):
        return klass

def f(self):
    return self
";
    let fa = py_in(src, None);
    let receiver = |name: &str, row: usize| {
        fa.symbols()
            .iter()
            .find(|s| s.name == name && s.selection_span.start.row == row)
            .unwrap_or_else(|| panic!("{name} at {row}"))
            .flags
            .contains(crate::model::file_analysis::SymbolFlags::RECEIVER)
    };
    assert!(receiver("this", 1));
    assert!(!receiver("y", 1));
    assert!(!receiver("self", 4), "a staticmethod has no receiver");
    assert!(receiver("klass", 7));
    assert!(!receiver("self", 10), "a free function has no receiver");
    assert_eq!(fa.inferred_type_via_bag("this", Point::new(2, 15)), Some(InferredType::ClassName("A".into())));
    assert_eq!(fa.inferred_type_via_bag("self", Point::new(11, 11)), None);
}
