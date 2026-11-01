//! The PHP pack: namespaces, class declarations and the file's use-map.

use super::*;
use crate::build::packs::php_pack;
use crate::model::file_analysis::{FileAnalysis, SymKind};

fn php(src: &str) -> FileAnalysis {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    extract(&tree, src.as_bytes(), &php_pack()).unwrap().into_file_analysis()
}

/// (name, package) of every symbol of `kind`, in source order.
fn declared(fa: &FileAnalysis, kind: SymKind) -> Vec<(String, Option<String>)> {
    fa.symbols()
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| (s.name.clone(), s.package.clone()))
        .collect()
}

fn owned(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    pairs.iter().map(|(n, p)| (n.to_string(), p.map(str::to_string))).collect()
}

#[test]
fn a_class_is_filed_under_its_namespace() {
    let fa = php("<?php
namespace App\\Models;

class User {}
interface Contract {}
trait Greets {}
");
    assert_eq!(declared(&fa, SymKind::Package), owned(&[("App\\Models", None)]));
    assert_eq!(
        declared(&fa, SymKind::Class),
        owned(&[
            ("App\\Models\\User", Some("App\\Models")),
            ("App\\Models\\Contract", Some("App\\Models")),
            ("App\\Models\\Greets", Some("App\\Models")),
        ])
    );
    assert_eq!(fa.find_type_decl("App\\Models\\User").map(|s| s.start.row), Some(3));
}

#[test]
fn each_namespace_holds_until_the_next_and_a_braced_one_for_its_body() {
    let fa = php("<?php
class Bare {}
namespace A;
class X {}
namespace B;
class Y extends X {}
");
    assert_eq!(
        declared(&fa, SymKind::Class),
        owned(&[("Bare", None), ("A\\X", Some("A")), ("B\\Y", Some("B"))])
    );
    // an unimported leaf is the writing namespace's own class
    assert_eq!(fa.declared_parents("B\\Y"), ["B\\X"]);

    let fa = php("<?php
namespace A { class X {} }
namespace { class Y extends A\\X {} }
");
    assert_eq!(declared(&fa, SymKind::Class), owned(&[("A\\X", Some("A")), ("Y", None)]));
    assert_eq!(fa.declared_parents("Y"), ["A\\X"]);
}

#[test]
fn parents_resolve_through_the_use_map() {
    let fa = php("<?php
namespace App\\Models;

use Illuminate\\Database\\Eloquent\\Model;
use \\Vendor\\Pkg\\Speaker as Talks;
use App\\Contracts\\{Listener, Logger as Log};
use function App\\Helpers\\Countable;
use const App\\Limits\\Local;

class User extends Model implements Talks, Listener, Log, Countable, Local\\Thing {
    use HasNames, \\Traits\\Stamps;
}
interface Contract extends Listener {}
trait Greets { use Talks; }
");
    assert_eq!(
        fa.declared_parents("App\\Models\\User"),
        [
            "Illuminate\\Database\\Eloquent\\Model",
            "Vendor\\Pkg\\Speaker",
            "App\\Contracts\\Listener",
            "App\\Contracts\\Logger",
            // a function or constant import names no class
            "App\\Models\\Countable",
            "App\\Models\\Local\\Thing",
            "App\\Models\\HasNames",
            "Traits\\Stamps",
        ]
    );
    assert_eq!(fa.declared_parents("App\\Models\\Contract"), ["App\\Contracts\\Listener"]);
    assert_eq!(fa.declared_parents("App\\Models\\Greets"), ["Vendor\\Pkg\\Speaker"]);
}

#[test]
fn use_rows_and_aliases_are_the_files_use_map() {
    let fa = php("<?php
namespace App;

use Illuminate\\Support\\Collection;
use \\Vendor\\Pkg\\Speaker as Talks;
use App\\Contracts\\{Listener, Logger as Log, function helper};
use function App\\Helpers\\format;
use function App\\Helpers\\{wrap};
use const App\\Limits\\MAX;
");
    let rows: Vec<(&str, &str, Option<&str>)> = fa
        .pack
        .use_rows
        .iter()
        .map(|r| (r.name.namespace.as_str(), r.name.leaf.as_str(), r.alias.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("Illuminate\\Support", "Collection", None),
            ("Vendor\\Pkg", "Speaker", Some("Talks")),
            ("App\\Contracts", "Listener", None),
            ("App\\Contracts", "Logger", Some("Log")),
        ]
    );
    // a use-map language's rows are not import paths
    assert!(fa.pack.include_directives.is_empty());

    // The model reads the same map the extractor resolved parents through.
    let end = tree_sitter::Point::new(9, 0);
    assert_eq!(fa.class_spelling_identity("Collection", end), "Illuminate\\Support\\Collection");
    assert_eq!(fa.class_spelling_identity("Talks", end), "Vendor\\Pkg\\Speaker");
    assert_eq!(fa.class_spelling_identity("Log", end), "App\\Contracts\\Logger");
    assert_eq!(fa.class_spelling_identity("Collection\\Item", end), "Illuminate\\Support\\Collection\\Item");
    // an aliased row binds its alias, never its own leaf
    assert_eq!(fa.class_spelling_identity("Speaker", end), "App\\Speaker");
    assert_eq!(fa.leaf_namespace("Listener").as_deref(), Some("App\\Contracts"));
}

#[cfg(feature = "php")]
#[test]
fn the_php_driver_serves_php_files() {
    use crate::build::language_driver::LanguageRegistry;
    let driver = LanguageRegistry::compiled()
        .for_path(std::path::Path::new("src/Models/User.php"))
        .expect("a .php file has a driver");
    assert_eq!(driver.id(), "php");
    let fa = driver.analyze("<?php\nnamespace App;\nuse Base\\Model;\nclass User extends Model {}\n");
    assert_eq!(fa.declared_parents("App\\User"), ["Base\\Model"]);
}

#[test]
fn the_class_keyword_or_modifier_is_its_flavour_flag() {
    use crate::model::file_analysis::SymbolFlags;
    let fa = php("<?php
abstract class Shape {}
final class Square {}
interface Drawable {}
trait Named {}
");
    let flags = |name: &str| fa.symbols().iter().find(|s| s.name == name).unwrap().flags;
    assert_eq!(flags("Shape"), SymbolFlags::ABSTRACT);
    assert_eq!(flags("Square"), SymbolFlags::empty());
    assert_eq!(flags("Drawable"), SymbolFlags::INTERFACE);
    assert_eq!(flags("Named"), SymbolFlags::TRAIT);
}

#[test]
fn parent_names_and_imported_leaves_are_class_references() {
    use crate::model::file_analysis::RefKind;
    let src = "<?php
namespace App;
use Lib\\Model;
use Lib\\Contracts\\{Speaker};
use function Lib\\helper;
class User extends Model implements Speaker, \\Countable, Sub\\Local {
    use Traits\\Stamps;
}
";
    let fa = php(src);
    let lines: Vec<&str> = src.lines().collect();
    // (written spelling, the token the span covers, the identity it names)
    let refs: Vec<(String, &str, String)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef))
        .map(|r| {
            let token = &lines[r.span.start.row][r.span.start.column..r.span.end.column];
            (r.target_name.clone(), token, fa.spelled_identity(r))
        })
        .collect();
    let want: Vec<(String, &str, String)> = [
        ("Model", "Model", "Lib\\Model"),
        ("Speaker", "Speaker", "Lib\\Contracts\\Speaker"),
        ("Model", "Model", "Lib\\Model"),
        ("Speaker", "Speaker", "Lib\\Contracts\\Speaker"),
        ("\\Countable", "Countable", "Countable"),
        ("Sub\\Local", "Local", "App\\Sub\\Local"),
        ("Traits\\Stamps", "Stamps", "App\\Traits\\Stamps"),
    ]
    .iter()
    .map(|(n, t, i)| (n.to_string(), *t, i.to_string()))
    .collect();
    assert_eq!(refs, want);
}

/// A `use function` / `use const` row names no class, whether its keyword
/// sits on the statement (flat, or a whole group) or on one clause of a
/// group: it is neither a use-map row nor a class reference.
#[test]
fn function_and_const_imports_are_neither_rows_nor_class_references() {
    use crate::model::file_analysis::RefKind;
    let fa = php("<?php
namespace App;
use function Lib\\helper;
use const Lib\\LIMIT;
use function Lib\\Fns\\{wrap, unwrap};
use const Lib\\Limits\\{MAX};
use Lib\\Mixed\\{function f, const C, Klass};
");
    let rows: Vec<String> = fa.pack.use_rows.iter().map(|r| r.name.identity("\\")).collect();
    assert_eq!(rows, ["Lib\\Mixed\\Klass"]);
    let refs: Vec<&str> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef))
        .map(|r| r.target_name.as_str())
        .collect();
    assert_eq!(refs, ["Klass"]);
}

/// A class's uses in another file are found by the identity they name:
/// goto-def from a parent clause lands on the declaration, and rename
/// rewrites the declaration, the import's leaf and the parent's leaf.
#[test]
fn a_class_renames_across_files_through_its_identity() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use std::path::PathBuf;
    let src_model = "<?php\nnamespace Lib;\nclass Model {}\n";
    // `Other\Model` shares the leaf and must never be renamed with it.
    let src_user = "<?php\nnamespace App;\nuse Lib\\Model;\nclass User extends Model {}\nclass Twin extends Other\\Model {}\n";
    let (p_model, p_user) = (PathBuf::from("/fake/php/Lib/Model.php"), PathBuf::from("/fake/php/App/User.php"));

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(p_model.clone(), php(src_model));
    idx.insert_workspace_for_test(p_user.clone(), php(src_user));
    let store = OpenDocs::new();

    let user = idx.workspace_file_for_test(&p_user).unwrap();
    let on_extends = tree_sitter::Point { row: 3, column: 21 };
    let cs = resolve(&store, &user, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p_user.clone())), on_extends, Some(&idx), OverrideScope::default());
    let defs = cs.definitions();
    assert!(
        defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if *p == crate::util::canonical_path::CanonicalPath::resolve(&p_model)) && d.span.start.row == 2),
        "gd lands on the declaration: {defs:?}"
    );

    let mut sites: Vec<(String, usize, usize)> = cs
        .rename_edits("Entity")
        .unwrap()
        .into_iter()
        .map(|(loc, _)| {
            let FileKey::Path(p) = &loc.key else { panic!() };
            (p.file_name().unwrap().to_string_lossy().to_string(), loc.span.start.row, loc.span.start.column)
        })
        .collect();
    sites.sort();
    assert_eq!(
        sites,
        [
            ("Model.php".to_string(), 2, 6),
            ("User.php".to_string(), 2, 8),
            ("User.php".to_string(), 3, 19),
        ]
    );
}

#[test]
fn an_aliased_imports_leaf_renames_the_imported_class() {
    use crate::model::file_analysis::RenameKind;
    // the file's own `Model` is a different class from the one it imports
    let fa = php("<?php\nnamespace App;\nuse Lib\\Model as Base;\nclass Model extends Base {}\n");
    let on_leaf = tree_sitter::Point { row: 2, column: 9 };
    assert!(matches!(
        fa.rename_kind_at(on_leaf, None),
        Some(RenameKind::Package(ref c)) if c == "Lib\\Model"
    ));
    assert_eq!(fa.declared_parents("App\\Model"), ["Lib\\Model"]);
}

/// A `use` row binds in the namespace section it is written in: two
/// sections of one file importing the same leaf from different places each
/// resolve their own, in either namespace form.
#[test]
fn each_namespace_section_resolves_through_its_own_use_rows() {
    use crate::model::file_analysis::RefKind;
    let braced = "<?php
namespace A {
    use X\\Model;
    class Foo extends Model {}
}
namespace B {
    use Y\\Model;
    class Bar extends Model {}
}
namespace {
    use Z\\Model;
}
namespace C {
    class Baz extends Model {}
}
";
    let statement = "<?php
namespace A;
use X\\Model;
class Foo extends Model {}
namespace B;
use Y\\Model;
class Bar extends Model {}
class Qux extends Foo {}
";
    for src in [braced, statement] {
        let fa = php(src);
        assert_eq!(fa.declared_parents("A\\Foo"), ["X\\Model"], "{src}");
        assert_eq!(fa.declared_parents("B\\Bar"), ["Y\\Model"], "{src}");
        // a row of the global section never reaches a named one
        if src == braced {
            assert_eq!(fa.declared_parents("C\\Baz"), ["C\\Model"]);
        } else {
            // a bare leaf resolves into the namespace in force at the site
            assert_eq!(fa.declared_parents("B\\Qux"), ["B\\Foo"]);
        }
        let spelled: Vec<(usize, String)> = fa
            .refs()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::PackageRef) && r.target_name == "Model")
            .map(|r| (r.span.start.row, fa.spelled_identity(r)))
            .collect();
        let want: &[(usize, &str)] = if src == braced {
            &[(2, "X\\Model"), (3, "X\\Model"), (6, "Y\\Model"), (7, "Y\\Model"), (10, "Z\\Model"), (13, "C\\Model")]
        } else {
            &[(2, "X\\Model"), (3, "X\\Model"), (5, "Y\\Model"), (6, "Y\\Model")]
        };
        let want: Vec<(usize, String)> = want.iter().map(|(r, i)| (*r, i.to_string())).collect();
        assert_eq!(spelled, want, "{src}");
    }
}

/// Rename of a class reaches only the section that imports it: B's
/// same-leaf import of another class is left alone.
#[test]
fn a_rename_stays_in_the_section_that_imports_the_class() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use std::path::PathBuf;
    let src_x = "<?php\nnamespace X;\nclass Model {}\n";
    let src_y = "<?php\nnamespace Y;\nclass Model {}\n";
    let src_user = "<?php\nnamespace A;\nuse X\\Model;\nclass Foo extends Model {}\nnamespace B;\nuse Y\\Model;\nclass Bar extends Model {}\n";
    let (p_x, p_y, p_user) = (
        PathBuf::from("/fake/php/X/Model.php"),
        PathBuf::from("/fake/php/Y/Model.php"),
        PathBuf::from("/fake/php/App/Both.php"),
    );
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(p_x.clone(), php(src_x));
    idx.insert_workspace_for_test(p_y.clone(), php(src_y));
    idx.insert_workspace_for_test(p_user.clone(), php(src_user));
    let store = OpenDocs::new();
    let user = idx.workspace_file_for_test(&p_user).unwrap();

    let edits_from = |row: usize| {
        let at = tree_sitter::Point { row, column: 19 };
        let cs = resolve(&store, &user, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p_user.clone())), at, Some(&idx), OverrideScope::default());
        let mut sites: Vec<(String, usize, usize)> = cs
            .rename_edits("Entity")
            .unwrap()
            .into_iter()
            .map(|(loc, _)| {
                let FileKey::Path(p) = &loc.key else { panic!() };
                let dir = p.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
                (dir, loc.span.start.row, loc.span.start.column)
            })
            .collect();
        sites.sort();
        sites
    };
    let s = |d: &str, r: usize, c: usize| (d.to_string(), r, c);
    assert_eq!(edits_from(3), [s("App", 2, 6), s("App", 3, 18), s("X", 2, 6)]);
    assert_eq!(edits_from(6), [s("App", 5, 6), s("App", 6, 18), s("Y", 2, 6)]);
}

#[test]
fn functions_and_methods_are_callables_of_their_namespace_and_class() {
    let fa = php("<?php
namespace App;

function helper() {}

class User {
    public function name() { $local = 1; }
}
");
    assert_eq!(declared(&fa, SymKind::Sub), owned(&[("helper", Some("App"))]));
    assert_eq!(declared(&fa, SymKind::Method), owned(&[("name", Some("App\\User"))]));
    // a body's variables are not members of the class
    assert!(fa.symbols().iter().all(|s| s.kind != SymKind::Field));
}

#[test]
fn what_the_document_states_about_a_method_is_its_flag() {
    use crate::model::file_analysis::SymbolFlags as F;
    let fa = php("<?php
abstract class Repo {
    public function __construct() {}
    static private function make() {}
    protected static function find() {}
    abstract public function save();
    public function plain() {}
}
interface Store { public function load(); }
class Magic { public function __call($n, $a) {} }
trait Proxy { public function __get($n) {} }
");
    let flags = |name: &str| fa.symbols().iter().find(|s| s.name.ends_with(name)).unwrap().flags;
    assert_eq!(flags("__construct"), F::CONSTRUCTOR);
    assert_eq!(flags("make"), F::STATIC | F::NON_PUBLIC);
    assert_eq!(flags("find"), F::STATIC | F::NON_PUBLIC);
    assert_eq!(flags("save"), F::ABSTRACT | F::CONTRACT);
    assert_eq!(flags("plain"), F::empty());
    assert_eq!(flags("load"), F::CONTRACT);
    assert!(fa.symbols().iter().find(|s| s.name == "__construct").unwrap().is_constructor());
    // a catch-all member is a fact about the class that declares it
    assert_eq!(flags("Magic"), F::DYNAMIC_MEMBERS);
    assert_eq!(flags("Proxy"), F::TRAIT | F::DYNAMIC_MEMBERS);
    assert_eq!(flags("Repo"), F::ABSTRACT);
}

#[test]
fn an_interface_or_abstract_class_is_a_role_its_contracts_require() {
    let fa = php("<?php
namespace App;

interface Store { public function load(); public function save(); }
abstract class Base implements Store {
    public function load() {}
    abstract protected function key();
}
class Disk extends Base {
    public function save() {}
}
class Broken implements Store {
    public function load() {}
}
");
    assert!(fa.is_role_package("App\\Store"));
    assert!(fa.is_role_package("App\\Base"));
    assert!(!fa.is_role_package("App\\Disk"));
    assert_eq!(fa.role_requires("App\\Store"), ["load", "save"]);
    assert_eq!(fa.role_requires("App\\Base"), ["key"]);
    // Disk gets `load` from Base and states `save`, but nothing provides
    // Base's `key`; Broken provides `load` and not `save`. A contract never
    // provides itself, so Store's own declarations satisfy nothing.
    let mut missing: Vec<(String, String, String)> = fa
        .unfulfilled_role_requires(None)
        .into_iter()
        .map(|u| (u.package, u.role, u.name))
        .collect();
    missing.sort();
    let row = |p: &str, r: &str, n: &str| (p.to_string(), r.to_string(), n.to_string());
    assert_eq!(
        missing,
        vec![row("App\\Broken", "App\\Store", "save"), row("App\\Disk", "App\\Base", "key")]
    );
}

/// `Foo()` calls the function `Foo` even where a class `Foo` is in scope:
/// PHP constructs with `new`, so the call is no reference to the class
/// and its value is no instance of it. (Python's `Foo()` is both —
/// `python_pack_tests::calling_a_class_names_it`.)
#[test]
fn calling_a_class_name_is_a_function_call() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use crate::model::file_analysis::{RefKind, RenameKind, Span};
    use tree_sitter::Point;
    // the document's free-call pattern, which this pack does not carry yet
    let pack = LangPack {
        query_source: concat!(
            include_str!("../../queries/php/skeleton.scm"),
            "\n(function_call_expression function: (name) @ref.call",
            " arguments: (arguments ((_) @arity.arg \",\"?)*) @arity.args) @expr.call\n",
        ),
        ..php_pack()
    };
    let src = "<?php\nnamespace App;\nclass Foo {}\nclass Bar extends Foo {}\nFoo();\n";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let fa = extract(&tree, src.as_bytes(), &pack).unwrap().into_file_analysis();

    let call = fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::FunctionCall) && r.target_name == "Foo")
        .expect("the call is a ref");
    assert_eq!(fa.class_named_by_call(call, None), None);
    assert!(
        !matches!(fa.rename_kind_at(Point::new(4, 1), None), Some(RenameKind::Package(_))),
        "the call renames as a call"
    );
    let call_span = Span { start: Point::new(4, 0), end: Point::new(4, 5) };
    assert_eq!(fa.expr_type_at_span(call_span, None), None);

    // the class's references, from its parent clause, leave the call out
    let path = std::path::PathBuf::from("/fake/php/App/Foo.php");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), fa);
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path)), Point::new(3, 19), Some(&idx), OverrideScope::default());
    let mut rows: Vec<usize> = cs.rename_edits("Baz").unwrap().into_iter().map(|(l, _)| l.span.start.row).collect();
    rows.sort();
    assert_eq!(rows, [2, 3]);
}

/// Index `files` (path, source) through the php driver.
#[cfg(feature = "php")]
fn php_indexed(files: &[(&str, &str)]) -> crate::index::module_index::ModuleIndex {
    use crate::build::language_driver::LanguageRegistry;
    let driver = LanguageRegistry::compiled().for_path(std::path::Path::new("x.php")).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    for (path, src) in files {
        let p = std::path::PathBuf::from(path);
        idx.insert_workspace_for_test(p.clone(), driver.analyze_with_path(src, Some(&p)));
    }
    idx
}

/// goto-def's (file, row, label) answers at `at` in `path`, and the hover
/// candidate's file.
#[cfg(feature = "php")]
fn php_landing(
    idx: &crate::index::module_index::ModuleIndex,
    path: &str,
    at: tree_sitter::Point,
) -> (Vec<(String, usize, Option<String>)>, Option<String>) {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let p = std::path::PathBuf::from(path);
    let fa = idx.workspace_file_for_test(&p).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p)), at, Some(idx), OverrideScope::default());
    let short = |k: &FileKey| match k {
        FileKey::Path(p) => p.strip_prefix("/fake/php").unwrap().display().to_string(),
        FileKey::Url(u, _) => u.to_string(),
    };
    (
        cs.definitions().iter().map(|d| (short(&d.key), d.span.start.row, d.label.clone())).collect(),
        cs.hover_candidate().map(|d| short(&d.key)),
    )
}

/// The member `name` on `class`, through `path`'s own scope.
#[cfg(feature = "php")]
fn php_member(
    idx: &crate::index::module_index::ModuleIndex,
    path: &str,
    class: &str,
) -> Option<crate::model::file_analysis::MethodResolution> {
    use crate::build::language_driver::LanguageRegistry;
    use crate::model::file_analysis::{CrossFileLookup, ScopedLookup, VisibilityAxis};
    let fa = idx.workspace_file_for_test(std::path::Path::new(path)).unwrap();
    let base: &dyn CrossFileLookup = idx;
    let vis = LanguageRegistry::pack_visibility(&fa.language);
    let scoped = ScopedLookup::new(base, &fa.pack.include_closure, None, VisibilityAxis::for_origin(&fa, None, base, vis));
    fa.resolve_method_in_ancestors(class, "name", Some(&scoped))
}

#[cfg(feature = "php")]
fn php_widening(idx: &crate::index::module_index::ModuleIndex, path: &str) -> Vec<(u32, u32, String)> {
    let fa = idx.workspace_file_for_test(std::path::Path::new(path)).unwrap();
    crate::lsp::symbols::widening_diagnostics(&fa, idx)
        .into_iter()
        .map(|d| (d.range.start.line, d.range.start.character, d.message))
        .collect()
}

const PHP_STRANGER: (&str, &str) =
    ("/fake/php/A/Model.php", "<?php\nnamespace A;\nclass Model { function name() {} }\n");
const PHP_LIB: (&str, &str) =
    ("/fake/php/Lib/Model.php", "<?php\nnamespace Lib;\nclass Model { function name() {} }\n");

/// A same-leaf class in a namespace that sorts first is registered for
/// widening, yet goto-def, hover and the member walk land on the class
/// the `use` row names.
#[cfg(feature = "php")]
#[test]
fn a_same_leaf_class_never_competes_with_the_identity() {
    use crate::model::file_analysis::{CrossFileLookup, MethodResolution};
    use tree_sitter::Point;
    let user = ("/fake/php/App/User.php", "<?php\nnamespace App;\nuse Lib\\Model;\nclass User extends Model {}\n");
    let idx = php_indexed(&[PHP_STRANGER, PHP_LIB, user]);
    assert_eq!(idx.leaf_def_candidates("Model").len(), 2, "the leaf is registered");
    assert!(idx.def_candidates("Model").is_empty(), "and answers no name lookup");
    for at in [Point::new(2, 8), Point::new(3, 19)] {
        let (defs, hover) = php_landing(&idx, user.0, at);
        assert_eq!(defs, [("Lib/Model.php".to_string(), 2, None)], "goto-def from {at:?}");
        assert_eq!(hover.as_deref(), Some("Lib/Model.php"), "hover from {at:?}");
    }
    assert!(matches!(
        php_member(&idx, user.0, "App\\User"),
        Some(MethodResolution::CrossFile { ref class, widened: false, .. }) if class == "Lib\\Model"
    ));
    assert!(php_widening(&idx, user.0).is_empty());
}

/// `use Vendor\Model;` with no `Vendor\Model` indexed: the class answers
/// through its leaf, labeled, the member it finds is stamped widened, and
/// the diagnostic fires on each class token naming it — the `use` row's
/// leaf and the parent clause. Indexing `Vendor\Model` ends it.
#[cfg(feature = "php")]
#[test]
fn an_unindexed_identity_widens_to_its_leaf_out_loud() {
    use crate::model::file_analysis::MethodResolution;
    use tree_sitter::Point;
    let sub = ("/fake/php/App/Sub.php", "<?php\nnamespace App;\nuse Vendor\\Model;\nclass Sub extends Model {}\n");
    let idx = php_indexed(&[PHP_STRANGER, sub]);
    let (defs, hover) = php_landing(&idx, sub.0, Point::new(3, 19));
    assert_eq!(
        defs,
        [("A/Model.php".to_string(), 2, Some("widened: `A\\Model` stands in for `Vendor\\Model`".to_string()))]
    );
    assert_eq!(hover.as_deref(), Some("A/Model.php"));
    assert!(matches!(
        php_member(&idx, sub.0, "App\\Sub"),
        Some(MethodResolution::CrossFile { ref class, widened: true, .. }) if class == "A\\Model"
    ));
    let msg = "nothing indexed declares `Vendor\\Model`; resolved by its leaf to `A\\Model`".to_string();
    assert_eq!(php_widening(&idx, sub.0), [(2, 11, msg.clone()), (3, 18, msg)]);

    let vendor = ("/fake/php/Vendor/Model.php", "<?php\nnamespace Vendor;\nclass Model { function name() {} }\n");
    let idx = php_indexed(&[PHP_STRANGER, vendor, sub]);
    let (defs, _) = php_landing(&idx, sub.0, Point::new(3, 19));
    assert_eq!(defs, [("Vendor/Model.php".to_string(), 2, None)]);
    assert!(matches!(
        php_member(&idx, sub.0, "App\\Sub"),
        Some(MethodResolution::CrossFile { ref class, widened: false, .. }) if class == "Vendor\\Model"
    ));
    assert!(php_widening(&idx, sub.0).is_empty());
}

/// (namespace, start row, end row) of every lane section, in write order.
fn lane(fa: &FileAnalysis) -> Vec<(&str, usize, usize)> {
    fa.namespace_ranges.iter().map(|r| (r.namespace.as_str(), r.span.start.row, r.span.end.row)).collect()
}

#[test]
fn statement_namespaces_fill_the_lane_and_a_method_belongs_to_its_class() {
    let fa = php("<?php
namespace A;
class X {
    function m() { return 1; }
}
namespace B;
function f() { return 2; }
");
    assert_eq!(lane(&fa), vec![("A", 1, 5), ("B", 5, 7)]);
    let in_m = tree_sitter::Point::new(3, 20);
    // The namespace is not the class: `$this` in `m` is `A\X`.
    assert_eq!(fa.package_at(in_m), Some("A\\X"));
    assert_eq!(fa.namespace_at(in_m), Some("A"));
    assert_eq!(fa.namespace_at(tree_sitter::Point::new(6, 16)), Some("B"));
}

#[test]
fn braced_namespaces_cover_their_bodies() {
    let fa = php("<?php
namespace A {
    class X {
        function m() { return 1; }
    }
}
namespace B {
    function f() { return 2; }
}
");
    assert_eq!(lane(&fa), vec![("A", 1, 5), ("B", 6, 8)]);
    let in_m = tree_sitter::Point::new(3, 24);
    assert_eq!(fa.package_at(in_m), Some("A\\X"));
    assert_eq!(fa.namespace_at(in_m), Some("A"));
    assert_eq!(fa.namespace_at(tree_sitter::Point::new(7, 20)), Some("B"));
}

/// A fully-qualified class spelling (`\Lib\Model`) names its identity:
/// goto-def lands on the declaration, never the declaring file's top, and
/// the spelling widens nothing.
#[cfg(feature = "php")]
#[test]
fn a_fully_qualified_class_spelling_lands_on_its_declaration() {
    let sub = ("/fake/php/App/Sub.php", "<?php\nnamespace App;\nclass Sub extends \\Lib\\Model {}\n");
    let idx = php_indexed(&[PHP_STRANGER, PHP_LIB, sub]);
    let (defs, hover) = php_landing(&idx, sub.0, tree_sitter::Point::new(2, 24));
    assert_eq!(defs, [("Lib/Model.php".to_string(), 2, None)]);
    assert_eq!(hover.as_deref(), Some("Lib/Model.php"));
    assert!(php_widening(&idx, sub.0).is_empty());
}

/// A relative qualified parent (`Sub\Model` in `namespace App`) names
/// `App\Sub\Model`: goto-def lands on that declaration, never on the
/// file tops of the same-leaf classes.
#[cfg(feature = "php")]
#[test]
fn a_relative_qualified_parent_lands_on_its_resolved_class() {
    let model = ("/fake/php/App/Sub/Model.php", "<?php\nnamespace App\\Sub;\nclass Model { function name() {} }\n");
    let user = ("/fake/php/App/User.php", "<?php\nnamespace App;\nclass User extends Sub\\Model {}\n");
    let idx = php_indexed(&[PHP_STRANGER, PHP_LIB, model, user]);
    let (defs, hover) = php_landing(&idx, user.0, tree_sitter::Point::new(2, 23));
    assert_eq!(defs, [("App/Sub/Model.php".to_string(), 2, None)]);
    assert_eq!(hover.as_deref(), Some("App/Sub/Model.php"));
    assert!(php_widening(&idx, user.0).is_empty());
}

// `$this` is the receiver of the method around it, stated by the document:
// an object of that method's class. A function has no receiver.
#[test]
fn this_is_the_receiver_of_its_method() {
    let fa = php("<?php\nclass A {\n  function f() { return $this; }\n}\nfunction g() { return $this; }\n");
    let at = |row, col| fa.expr_type_at_span(Span { start: Point::new(row, col), end: Point::new(row, col + 5) }, None);
    assert_eq!(at(2, 24), Some(InferredType::ClassName("A".into())));
    assert_eq!(at(4, 22), None);
}
