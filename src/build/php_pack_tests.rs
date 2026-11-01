//! The PHP pack: namespaces, class declarations and the file's use-map.

use super::*;
use crate::build::packs::php_pack;
use crate::model::file_analysis::{FileAnalysis, SymKind};

fn php(src: &str) -> FileAnalysis {
    crate::build::language_driver::LanguageRegistry::compiled()
        .for_path(std::path::Path::new("t.php"))
        .unwrap()
        .analyze(src)
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
    let cs = resolve(&store, &user, FileKey::Path(p_user.clone()), on_extends, Some(&idx), OverrideScope::default());
    let defs = cs.definitions();
    assert!(
        defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if *p == p_model) && d.span.start.row == 2),
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
        let cs = resolve(&store, &user, FileKey::Path(p_user.clone()), at, Some(&idx), OverrideScope::default());
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

const MEMBERS: &str = "<?php
namespace App;

use Lib\\Clock;

class Order {
    public int $total = 0, $count;
    protected static ?Clock $clock;
    private readonly string $id;
    var $legacy;
    public int|string $key;
    public function name() {}
    public string $name;
    public function __construct(private Clock $made, public readonly int &$qty = 1, $plain = 0) {}
}
";

/// A property is a member of its class under its bare name; the `$` it is
/// declared with is its listed spelling, and the name token alone is the
/// span a rename rewrites.
#[test]
fn properties_are_members_of_their_class() {
    use crate::model::file_analysis::SymbolFlags as F;
    let fa = php(MEMBERS);
    assert_eq!(
        declared(&fa, SymKind::Field),
        owned(&[
            ("total", Some("App\\Order")),
            ("count", Some("App\\Order")),
            ("clock", Some("App\\Order")),
            ("id", Some("App\\Order")),
            ("legacy", Some("App\\Order")),
            ("key", Some("App\\Order")),
            ("name", Some("App\\Order")),
            ("made", Some("App\\Order")),
            ("qty", Some("App\\Order")),
        ])
    );
    let field = |name: &str| fa.symbols().iter().find(|s| s.kind == SymKind::Field && s.name == name).unwrap();
    assert_eq!(field("total").flags, F::empty());
    assert_eq!(field("clock").flags, F::STATIC | F::NON_PUBLIC);
    assert_eq!(field("made").flags, F::NON_PUBLIC);
    assert_eq!(field("qty").flags, F::empty());
    assert_eq!(field("id").attributes, ["readonly"]);
    assert_eq!(field("qty").attributes, ["readonly"]);
    assert_eq!(field("count").presentation.label.as_deref(), Some("$count"));
    assert_eq!(field("qty").presentation.label.as_deref(), Some("$qty"));
    let at = |s: &crate::model::file_analysis::Span| (s.start.row, s.start.column, s.end.column);
    assert_eq!(at(&field("count").selection_span), (6, 28, 33));
    assert_eq!(at(&field("qty").selection_span), (13, 75, 78));
    // a promoted property belongs to the class body, not the constructor
    let class_body = field("total").scope;
    assert_eq!(field("made").scope, class_body);
    assert_eq!(field("qty").scope, class_body);
}

/// A declared type is the member's value, read where a member access
/// reads it; a class spelling resolves through the use-map. A property and
/// a method of one name are separate families.
#[test]
fn a_declared_property_type_is_the_members_value() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(MEMBERS);
    let order = T::ClassName("App\\Order".into());
    let value = |m: &str| fa.field_value_type(&order, m, None);
    assert_eq!(value("total"), Some(T::Numeric));
    assert_eq!(value("count"), Some(T::Numeric));
    assert_eq!(value("id"), Some(T::String));
    assert_eq!(value("name"), Some(T::String));
    assert_eq!(value("clock"), Some(T::ClassName("Lib\\Clock".into())));
    assert_eq!(value("made"), Some(T::ClassName("Lib\\Clock".into())));
    assert_eq!(value("qty"), Some(T::Numeric));
    assert_eq!(value("legacy"), None);
    assert_eq!(value("key"), None, "a union has no single type");
    assert_eq!(value("plain"), None, "a plain parameter is no property");
}

#[test]
fn the_outline_lists_properties_under_their_class() {
    let fa = php(MEMBERS);
    let outline = fa.document_symbols();
    let class = outline
        .iter()
        .flat_map(|o| std::iter::once(o).chain(&o.children))
        .find(|o| o.name.ends_with("Order"))
        .unwrap();
    let names: Vec<&str> = class.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["$total", "$count", "$clock", "$id", "$legacy", "$key", "name", "$name", "__construct", "$made", "$qty"]
    );
    let ctor = class.children.iter().find(|c| c.name == "__construct").unwrap();
    assert!(ctor.children.is_empty(), "the constructor lists none of its promoted properties");
}

/// A property's hover names it as it is listed, `$` and all.
#[test]
fn a_property_hovers_by_its_listed_name() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let path = std::path::PathBuf::from("/fake/php/App/Order.php");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), php(MEMBERS));
    let file = idx.workspace_file_for_test(&path).unwrap();
    let store = OpenDocs::new();
    let at = tree_sitter::Point { row: 6, column: 17 };
    let cs = resolve(&store, &file, FileKey::Path(path), at, Some(&idx), OverrideScope::default()).with_source(MEMBERS);
    let hover = crate::lsp::symbols::pack_hover_markdown(&cs, "php").unwrap();
    assert!(hover.contains("\n$total: "), "{hover}");
}

/// Rename from a property's declaration rewrites its bare name, never the
/// `$` (a typed `$` is the sigil, not part of the name), and never the
/// method that shares the name.
#[test]
fn a_property_renames_from_its_declaration() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use std::path::PathBuf;
    let path = PathBuf::from("/fake/php/App/Order.php");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), php(MEMBERS));
    let store = OpenDocs::new();
    let file = idx.workspace_file_for_test(&path).unwrap();
    let edits_from = |row: usize, column: usize| {
        let at = tree_sitter::Point { row, column };
        let cs = resolve(&store, &file, FileKey::Path(path.clone()), at, Some(&idx), OverrideScope::default());
        let mut sites: Vec<(usize, usize, usize, String)> = cs
            .rename_edits("$label")
            .unwrap()
            .into_iter()
            .map(|(loc, text)| (loc.span.start.row, loc.span.start.column, loc.span.end.column, text))
            .collect();
        sites.sort();
        sites
    };
    let edit = |r: usize, c: usize, e: usize| (r, c, e, "label".to_string());
    assert_eq!(edits_from(12, 20), [edit(12, 19, 23)]);
    assert_eq!(edits_from(13, 48), [edit(13, 47, 51)]);
    assert_eq!(edits_from(11, 21), [edit(11, 20, 24)], "the method keeps to its family");
}

/// Rename edits from `at` in a one-file workspace, as (row, start, end),
/// through the driver that serves the file's language.
#[cfg(any(feature = "php", feature = "cpp"))]
fn renames_in(file: &str, src: &str, row: usize, column: usize) -> Vec<(usize, usize, usize)> {
    use crate::build::language_driver::LanguageRegistry;
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let path = std::path::PathBuf::from(file);
    let fa = LanguageRegistry::compiled().for_path(&path).unwrap().analyze(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), fa);
    let store = OpenDocs::new();
    let file = idx.workspace_file_for_test(&path).unwrap();
    let at = tree_sitter::Point { row, column };
    let cs = resolve(&store, &file, FileKey::Path(path.clone()), at, Some(&idx), OverrideScope::default());
    let mut sites: Vec<(usize, usize, usize)> = cs
        .rename_edits("renamed")
        .unwrap()
        .into_iter()
        .map(|(loc, text)| {
            assert_eq!(text, "renamed");
            (loc.span.start.row, loc.span.start.column, loc.span.end.column)
        })
        .collect();
    sites.sort();
    sites
}

#[cfg(feature = "php")]
fn renames(src: &str, row: usize, column: usize) -> Vec<(usize, usize, usize)> {
    renames_in("/fake/php/App/One.php", src, row, column)
}

const PARAMS: &str = "<?php
namespace App;

use Lib\\Clock;

function tick(?Clock $at, int|Clock $n = 1, &$out, Clock ...$rest) {
    return $at;
}
class Timer {
    public function start(Clock $c) { $c; $at; return fn(int $c) => $c; }
}
";

/// A parameter is a local of its callable under the name after its `$`;
/// a closure's parameters are the closure's.
#[test]
fn parameters_are_locals_of_their_callable() {
    let fa = php(PARAMS);
    let locals: Vec<(String, usize)> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Variable)
        .map(|s| (s.name.clone(), s.selection_span.start.column))
        .collect();
    let want: Vec<(String, usize)> =
        [("at", 22), ("n", 37), ("out", 46), ("rest", 61), ("c", 33), ("c", 62)]
            .iter()
            .map(|(n, c)| (n.to_string(), *c))
            .collect();
    assert_eq!(locals, want);
    let at = fa.symbols().iter().find(|s| s.name == "at").unwrap();
    assert!(fa.scope_within_sub_body(at.scope));
    let in_closure = fa.symbols().iter().filter(|s| s.name == "c").map(|s| s.scope).collect::<Vec<_>>();
    assert_ne!(in_closure[0], in_closure[1], "the closure's parameter is its own");
    assert!(fa.document_symbols().iter().all(|o| o.name != "at" && o.name != "c"));
}

/// A declared type is the parameter's value from its binding site: a class
/// spelling resolves through the use-map, a union has no single type.
#[test]
fn a_parameters_declared_type_is_its_value_in_the_body() {
    use crate::model::file_analysis::InferredType as T;
    use crate::model::witnesses::WitnessAttachment as WA;
    let fa = php(PARAMS);
    let body = tree_sitter::Point { row: 6, column: 12 };
    assert_eq!(fa.inferred_type_via_bag("at", body), Some(T::ClassName("Lib\\Clock".into())));
    assert_eq!(fa.inferred_type_via_bag("n", body), None);
    assert_eq!(fa.inferred_type_via_bag("rest", body), None, "a variadic's type is its elements'");
    let at = fa.symbols().iter().find(|s| s.name == "at").unwrap();
    let declared: Vec<_> = fa
        .witnesses
        .for_attachment(&WA::Variable { name: "at".into(), scope: at.scope })
        .into_iter()
        .map(|w| w.span.start)
        .collect();
    assert_eq!(declared, [at.selection_span.start], "the witness stands at the binding site");
    // the read's value is the parameter's
    let read = crate::model::file_analysis::Span {
        start: tree_sitter::Point { row: 6, column: 11 },
        end: tree_sitter::Point { row: 6, column: 14 },
    };
    assert_eq!(fa.expr_type_at_span(read, None), Some(T::ClassName("Lib\\Clock".into())));
}

/// A use of `$x` in the body is a reference to the parameter: the name
/// token is the reference, so rename never touches the `$`. A read the
/// callable declares no parameter for binds to nothing.
#[test]
fn a_parameter_use_binds_to_the_parameter() {
    use crate::model::file_analysis::RefKind;
    let fa = php(PARAMS);
    let bound: Vec<(usize, usize, String)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Variable) && r.span.start.row > 5)
        .filter_map(|r| {
            let sym = fa.symbol(r.resolved_symbol()?);
            let decl = format!("{}@{}", sym.name, sym.selection_span.start.column);
            Some((r.span.start.row, r.span.start.column, decl))
        })
        .collect();
    let row = |r: usize, c: usize, s: &str| (r, c, s.to_string());
    assert!(bound.contains(&row(6, 12, "at@22")), "{bound:?}");
    assert!(bound.contains(&row(9, 39, "c@33")), "{bound:?}");
    assert!(bound.contains(&row(9, 69, "c@62")), "the closure's own: {bound:?}");
    assert!(!bound.iter().any(|(r, c, _)| (*r, *c) == (9, 43)), "`$at` in the method is not tick's");
}

#[cfg(feature = "php")]
#[test]
fn a_parameter_renames_by_its_name_token() {
    assert_eq!(renames(PARAMS, 6, 13), [(5, 22, 24), (6, 12, 14)]);
    assert_eq!(renames(PARAMS, 9, 39), [(9, 33, 34), (9, 39, 40)]);
}

const PROMOTED: &str = "<?php
class Order {
    public function __construct(private Clock $made) { $made->start(); }
    public function age() { return $this->made; }
}
";

/// A promoted constructor parameter is the property and the constructor's
/// local on one token, one declaration: rename from either side rewrites
/// the token, the body's uses and the property's reads alike.
#[test]
fn a_promoted_parameter_is_the_property_and_the_local_as_one() {
    let fa = php(PROMOTED);
    let field = fa.symbols().iter().find(|s| s.kind == SymKind::Field).unwrap();
    let local = fa.symbols().iter().find(|s| s.kind == SymKind::Variable && s.name == "made").unwrap();
    assert_eq!(field.declared_with, Some(local.id));
    assert_eq!(local.declared_with, Some(field.id));
    assert_eq!(field.selection_span, local.selection_span);
    assert_ne!(field.scope, local.scope, "the property is the class's, the local the constructor's");
}

#[cfg(feature = "php")]
#[test]
fn a_promoted_parameter_renames_as_one_from_either_side() {
    let both = [(2, 47, 51), (2, 56, 60), (3, 42, 46)];
    assert_eq!(renames(PROMOTED, 2, 48), both, "from the declaration");
    assert_eq!(renames(PROMOTED, 2, 57), both, "from a body use");
}

/// Every class a type position names is a class reference through the
/// use-map, so rename of the class rewrites property, parameter, nullable,
/// union, qualified and return spellings.
#[test]
fn class_names_in_type_positions_are_class_references() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    use std::path::PathBuf;
    let src_clock = "<?php\nnamespace Lib;\nclass Clock {}\n";
    let src_user = "<?php
namespace App;
use Lib\\Clock;
class Job {
    private ?Clock $clock;
    public function __construct(private Clock $made) {}
    public function at(Clock|int $c, \\Lib\\Clock $d, self $e): Clock { return $c; }
}
";
    let (p_clock, p_user) = (PathBuf::from("/fake/php/Lib/Clock.php"), PathBuf::from("/fake/php/App/Job.php"));
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(p_clock.clone(), php(src_clock));
    idx.insert_workspace_for_test(p_user.clone(), php(src_user));
    let store = OpenDocs::new();
    let clock = idx.workspace_file_for_test(&p_clock).unwrap();
    let at = tree_sitter::Point { row: 2, column: 7 };
    let cs = resolve(&store, &clock, FileKey::Path(p_clock.clone()), at, Some(&idx), OverrideScope::default());
    let mut sites: Vec<(usize, usize)> = cs
        .rename_edits("Timer")
        .unwrap()
        .into_iter()
        .filter(|(loc, _)| matches!(&loc.key, FileKey::Path(p) if *p == p_user))
        .map(|(loc, _)| (loc.span.start.row, loc.span.start.column))
        .collect();
    sites.sort();
    assert_eq!(sites, [(2, 8), (4, 13), (5, 40), (6, 23), (6, 42), (6, 62)]);
}

/// A sigil-less pack's local renames whole, its first letter included.
#[cfg(feature = "cpp")]
#[test]
fn a_cpp_local_renames_whole() {
    let src = "int f(int abc) {\n  return abc + 1;\n}\n";
    assert_eq!(renames_in("/fake/cpp/f.cpp", src, 1, 10), [(0, 10, 13), (1, 9, 12)]);
}

/// Where `needle` starts in `src`, `offset` bytes in.
fn at(src: &str, needle: &str, offset: usize) -> tree_sitter::Point {
    let i = src.find(needle).unwrap_or_else(|| panic!("`{needle}` in the source")) + offset;
    let row = src[..i].matches('\n').count();
    tree_sitter::Point { row, column: i - src[..i].rfind('\n').map_or(0, |n| n + 1) }
}

/// A workspace of php files, asked as the editor asks: by file and point.
#[cfg(feature = "php")]
struct Workspace {
    idx: crate::index::module_index::ModuleIndex,
    store: crate::index::open_docs::OpenDocs,
    files: Vec<(std::path::PathBuf, &'static str)>,
}

/// (file name, row, start column) of each location, sorted.
#[cfg(feature = "php")]
type Sites = Vec<(String, usize, usize)>;

#[cfg(feature = "php")]
impl Workspace {
    fn new(files: &[(&str, &'static str)]) -> Self {
        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        let files: Vec<_> = files.iter().map(|(p, src)| (std::path::PathBuf::from(p), *src)).collect();
        // Through the driver that serves the file, which stamps its
        // language: the pack's lexical lanes read it.
        let driver = crate::build::language_driver::LanguageRegistry::compiled();
        for (path, src) in &files {
            idx.insert_workspace_for_test(path.clone(), driver.for_path(path).unwrap().analyze(src));
        }
        Workspace { idx, store: crate::index::open_docs::OpenDocs::new(), files }
    }

    fn ask<T>(&self, file: &str, needle: &str, offset: usize, f: impl FnOnce(&crate::index::resolve::CandidateSet) -> T) -> T {
        use crate::index::open_docs::FileKey;
        use crate::index::resolve::{resolve, OverrideScope};
        let (path, src) = self.files.iter().find(|(p, _)| p.ends_with(file)).expect("a workspace file");
        let fa = self.idx.workspace_file_for_test(path).unwrap();
        let point = at(src, needle, offset);
        f(&resolve(&self.store, &fa, FileKey::Path(path.clone()), point, Some(&self.idx), OverrideScope::default()))
    }

    fn sites(locs: impl IntoIterator<Item = crate::index::resolve::RefLocation>) -> Sites {
        use crate::index::open_docs::FileKey;
        let mut out: Sites = locs
            .into_iter()
            .map(|l| {
                let FileKey::Path(p) = &l.key else { panic!("a workspace file") };
                (p.file_name().unwrap().to_string_lossy().to_string(), l.span.start.row, l.span.start.column)
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn defs(&self, file: &str, needle: &str, offset: usize) -> Sites {
        self.ask(file, needle, offset, |cs| Self::sites(cs.definitions()))
    }

    fn renames(&self, file: &str, needle: &str, offset: usize) -> Sites {
        self.ask(file, needle, offset, |cs| Self::sites(cs.rename_edits("renamed").unwrap().into_iter().map(|(l, _)| l)))
    }
}

#[cfg(feature = "php")]
fn site(file: &str, row: usize, column: usize) -> (String, usize, usize) {
    (file.to_string(), row, column)
}

const CALLS: &str = "<?php
namespace App;

class Clock { public function start() {} }

class Timer {
    public function __construct(private Clock $made) {}
    public static function make() {}
    public function name() {}
    public string $name;
    public function run(Clock $c, ?Timer $t, array $xs) {
        $c->start(1, ...$xs);
        $t?->name();
        $t?->name;
        $this->made->start();
        $this->name();
        self::make();
        static::make();
        Timer::make();
        \\App\\Timer::make();
        return fn() => $this->name;
    }
}
";

/// A member call is a call ref on the method's name token and an `Invoke`
/// on its receiver; a property read is a value ref and the receiver's
/// slot. Every member call dispatches on its receiver's value, and a
/// receiver that names its class (`$this`, `self`, `Timer`) has that class
/// as its value.
#[test]
fn member_calls_and_reads_name_their_member_on_the_receiver() {
    use crate::model::file_analysis::RefKind;
    use crate::model::witnesses::{CallArg, Callee, Lookup, WitnessPayload};
    let fa = php(CALLS);
    let members: Vec<(&str, usize, &str)> = fa
        .refs()
        .iter()
        .filter_map(|r| match r.kind {
            RefKind::MethodCall { .. } => Some(("call", r.span.start.row, r.target_name.as_str())),
            RefKind::FieldAccess { .. } => Some(("read", r.span.start.row, r.target_name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        members,
        [
            ("call", 11, "start"),
            ("call", 12, "name"),
            ("read", 13, "name"),
            ("read", 14, "made"),
            ("call", 14, "start"),
            ("call", 15, "name"),
            ("call", 16, "make"),
            ("call", 17, "make"),
            ("call", 18, "make"),
            ("call", 19, "make"),
            ("read", 20, "name"),
        ]
    );
    let mut calls: Vec<(usize, String, Option<String>, Vec<bool>)> = fa
        .witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Method { name, lookup, .. }, receiver, args } => Some((
                w.span.start.row,
                name.clone(),
                {
                    assert_eq!(*lookup, Lookup::Receiver);
                    match receiver {
                        Some(crate::model::witnesses::WitnessAttachment::Expr(recv)) => {
                            fa.expr_type_at_span(*recv, None).and_then(|t| t.class_name().map(str::to_string))
                        }
                        _ => None,
                    }
                },
                args.iter().flatten().map(|a| matches!(a, CallArg::Spread(_))).collect(),
            )),
            _ => None,
        })
        .collect();
    calls.sort();
    let timer = || Some("App\\Timer".to_string());
    let clock = || Some("App\\Clock".to_string());
    let call = |row: usize, name: &str, named: Option<String>, args: &[bool]| (row, name.to_string(), named, args.to_vec());
    assert_eq!(
        calls,
        [
            call(11, "start", clock(), &[false, true]),
            call(12, "name", timer(), &[]),
            call(14, "start", clock(), &[]),
            call(15, "name", timer(), &[]),
            call(16, "make", timer(), &[]),
            call(17, "make", timer(), &[]),
            call(18, "make", timer(), &[]),
            call(19, "make", timer(), &[]),
        ]
    );
    // `$this` is no local: no read of a variable `this`, and its value is
    // the method's class, in a closure of the method too
    assert!(fa.refs().iter().all(|r| r.target_name != "this"));
    let this = |needle: &str| {
        let start = at(CALLS, needle, 0);
        let span = crate::model::file_analysis::Span { start, end: tree_sitter::Point { column: start.column + 5, ..start } };
        fa.expr_type_at_span(span, None)
    };
    let timer = Some(crate::model::file_analysis::InferredType::ClassName("App\\Timer".into()));
    assert_eq!(this("$this->made"), timer);
    assert_eq!(this("$this->name;"), timer);
}

/// Goto-def from a member's name at a call site lands on its declaration,
/// whatever typed the receiver: a parameter, a nullable parameter, a
/// promoted property, `$this`, or a class named by `self`, `static` or its
/// spelling. A property read lands on the property, never on the method
/// that shares its name.
#[cfg(feature = "php")]
#[test]
fn a_member_at_a_call_site_resolves_to_its_declaration() {
    let ws = Workspace::new(&[("/fake/php/App/Timer.php", CALLS)]);
    let def = |needle: &str, offset: usize| ws.defs("Timer.php", needle, offset);
    let decl = |row: usize, column: usize| vec![site("Timer.php", row, column)];
    assert_eq!(def("start(1", 0), decl(3, 30), "a parameter's declared class");
    assert_eq!(def("name();\n        $t?", 0), decl(8, 20), "nullsafe call");
    assert_eq!(def("name;\n        $this->made", 0), decl(9, 19), "nullsafe read: the property");
    assert_eq!(def("made->start", 0), decl(6, 47), "a promoted property");
    assert_eq!(def("start();\n        $this->name", 0), decl(3, 30), "through a promoted property's type");
    assert_eq!(def("name();\n        self", 0), decl(8, 20), "`$this`");
    assert_eq!(def("make();\n        static", 0), decl(7, 27), "`self::`");
    assert_eq!(def("make();\n        Timer", 0), decl(7, 27), "`static::`");
    assert_eq!(def("make();\n        \\", 0), decl(7, 27), "a class named by its spelling");
    assert_eq!(def("make();\n        return", 0), decl(7, 27), "a qualified spelling");
    assert_eq!(def("name;\n    }", 0), decl(9, 19), "`$this` in a closure of the method");
}

/// A method and a property of one name are separate members: a rename of
/// either rewrites its declaration and its own uses alone, from the
/// declaration or from a use. A class named by a static call is a class
/// reference.
#[cfg(feature = "php")]
#[test]
fn a_member_renames_with_its_call_sites_and_keeps_to_its_family() {
    let ws = Workspace::new(&[("/fake/php/App/Timer.php", CALLS)]);
    let rows = |sites: Sites| sites.into_iter().map(|(_, r, c)| (r, c)).collect::<Vec<_>>();
    let method = [(8, 20), (12, 13), (15, 15)];
    assert_eq!(rows(ws.renames("Timer.php", "name() {}", 0)), method, "from the declaration");
    assert_eq!(rows(ws.renames("Timer.php", "name();\n        self", 0)), method, "from a call site");
    let property = [(9, 19), (13, 13), (20, 30)];
    assert_eq!(rows(ws.renames("Timer.php", "name;\n    public function run", 0)), property);
    assert_eq!(rows(ws.renames("Timer.php", "name;\n    }", 0)), property);
    let make = [(7, 27), (16, 14), (17, 16), (18, 15), (19, 20)];
    assert_eq!(rows(ws.renames("Timer.php", "make();\n        Timer", 0)), make);
    // the class token of a static call renames with the class
    assert!(rows(ws.renames("Timer.php", "Timer {", 0)).contains(&(18, 8)));
}

const BASE: &str = "<?php
namespace Lib;

class Base {
    public string $label;
    public function greet() {}
    public static function make() {}
}
";

const CHILD: &str = "<?php
namespace App;

use Lib\\Base;

class Child extends Base {
    public function run(Child $c, ?Base $b) {
        $c->greet();
        $this->greet();
        $b?->greet();
        $c->label;
        self::make();
        Base::make();
    }
}
";

/// A member inherited from a class in another file resolves up the
/// `extends` chain, and renames across both files from either side.
#[cfg(feature = "php")]
#[test]
fn an_inherited_member_resolves_and_renames_across_files() {
    let ws = Workspace::new(&[("/fake/php/Lib/Base.php", BASE), ("/fake/php/App/Child.php", CHILD)]);
    let greet = vec![site("Base.php", 5, 20)];
    assert_eq!(ws.defs("Child.php", "greet();\n        $this", 0), greet, "a parameter typed by the child");
    assert_eq!(ws.defs("Child.php", "greet();\n        $b", 0), greet, "`$this`");
    assert_eq!(ws.defs("Child.php", "greet();\n        $c->label", 0), greet, "a nullsafe call");
    assert_eq!(ws.defs("Child.php", "label", 0), vec![site("Base.php", 4, 19)], "an inherited property");
    let make = vec![site("Base.php", 6, 27)];
    assert_eq!(ws.defs("Child.php", "make();\n        Base", 0), make, "`self::` up the chain");
    assert_eq!(ws.defs("Child.php", "make();\n    }", 0), make, "a class named through the use-map");

    let greets = vec![site("Base.php", 5, 20), site("Child.php", 7, 12), site("Child.php", 8, 15), site("Child.php", 9, 13)];
    assert_eq!(ws.renames("Base.php", "greet", 0), greets, "from the declaration");
    assert_eq!(ws.renames("Child.php", "greet();\n        $this", 0), greets, "from a call site");
    assert_eq!(ws.renames("Child.php", "label", 0), vec![site("Base.php", 4, 19), site("Child.php", 10, 12)]);
}

/// Hover on a member at a call site shows the declaration it resolves to.
#[test]
fn a_member_call_hovers_as_its_declaration() {
    let fa = php(CALLS);
    let hover = fa.hover_info(at(CALLS, "start(1", 0), CALLS, None).unwrap_or_default();
    assert!(hover.contains("public function start()"), "{hover}");
}

const HELPERS: &str = "<?php
namespace Lib;

function helper(...$xs) {}
";

/// A free call is an `Invoke` of the function its name names, and its ref
/// is pinned to that function's namespace: a bare name is the namespace in
/// force's own function when the file declares one, else the global one; a
/// qualified name resolves like a class spelling.
#[cfg(feature = "php")]
#[test]
fn a_free_call_is_an_invoke_of_its_function() {
    use crate::model::file_analysis::RefKind;
    let src = "<?php
namespace App;

function local() {}
local(1, ...$xs);
\\Lib\\helper(2);
Sub\\helper();
strlen('x');
";
    let fa = php(src);
    assert_eq!(
        call_invokes(&fa),
        [
            ("local".to_string(), true, Some(vec![false, true])),
            ("helper".to_string(), false, Some(vec![false])),
            ("helper".to_string(), false, Some(vec![])),
            ("strlen".to_string(), false, Some(vec![false])),
        ]
    );
    let pins: Vec<(&str, Option<&str>)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::FunctionCall))
        .map(|r| (r.target_name.as_str(), r.resolved_package()))
        .collect();
    assert_eq!(
        pins,
        [("local", Some("App")), ("\\Lib\\helper", Some("Lib")), ("Sub\\helper", Some("App\\Sub")), ("strlen", None)]
    );

    let ws = Workspace::new(&[("/fake/php/Lib/helpers.php", HELPERS), ("/fake/php/App/main.php", src)]);
    assert_eq!(ws.defs("main.php", "helper(2)", 0), vec![site("helpers.php", 3, 9)]);
    assert_eq!(ws.defs("main.php", "local(1", 0), vec![site("main.php", 3, 9)]);
}

/// (callee, defined in this file, which arguments spread) of each free call.
fn call_invokes(fa: &FileAnalysis) -> Vec<(String, bool, Option<Vec<bool>>)> {
    use crate::model::witnesses::{CallArg, Callee, WitnessPayload};
    fa.witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Function { name, local, .. }, args, .. } => Some((
                name.clone(),
                local.is_some(),
                args.as_ref().map(|a| a.iter().map(|a| matches!(a, CallArg::Spread(_))).collect()),
            )),
            _ => None,
        })
        .collect()
}

/// A promoted property read in another file renames the property, the
/// constructor's local and its body uses as one.
#[cfg(feature = "php")]
#[test]
fn a_promoted_property_renames_from_a_read_in_another_file() {
    let user = "<?php\nfunction age(Order $o) { return $o->made; }\n";
    let ws = Workspace::new(&[("/fake/php/Order.php", PROMOTED), ("/fake/php/user.php", user)]);
    assert_eq!(
        ws.renames("user.php", "made", 0),
        [site("Order.php", 2, 47), site("Order.php", 2, 56), site("Order.php", 3, 42), site("user.php", 1, 36)]
    );
}

const RETURNS: &str = "<?php
namespace App;

use Lib\\Clock;

class Builder {
    public function with(): static { return $this; }
    public function again(): self { return $this; }
    public function maybe(): ?Builder {}
    public function nullable(): ?static {}
    public function clock(): Clock {}
    public function full(): \\Lib\\Clock {}
    public function either(): Clock|int {}
    public function nothing(): void {}
    public function count(): int {}
    public function bare() {}
    public function run() {
        $this->with()->again()->maybe()->nullable()->clock()->tick();
    }
}

function make(): Builder {}
make()->full()->tick();
";

const CLOCK: &str = "<?php
namespace Lib;

class Clock {
    public function tick(): int {}
}
";

/// A declared return is the callable's value: a class spelling resolves
/// through the use-map, `self` and `static` name the class the method is
/// written in, a nullable is its class, and a union, `void` or no
/// declaration states no type.
#[test]
fn a_declared_return_is_the_callables_value() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(RETURNS);
    let ret = |m: &str| fa.find_method_return_type("App\\Builder", m, None, None);
    let builder = Some(T::ClassName("App\\Builder".into()));
    let clock = Some(T::ClassName("Lib\\Clock".into()));
    assert_eq!(ret("with"), builder, "`static`");
    assert_eq!(ret("again"), builder, "`self`");
    assert_eq!(ret("maybe"), builder, "a nullable class");
    assert_eq!(ret("nullable"), builder, "a nullable `static`");
    assert_eq!(ret("clock"), clock, "an imported class");
    assert_eq!(ret("full"), clock, "a fully qualified class");
    assert_eq!(ret("count"), Some(T::Numeric));
    assert_eq!(ret("either"), None, "a union has no single type");
    let union = php("<?php\nclass A {\n    public function orNull(): self|null {}\n}\n");
    assert_eq!(union.find_method_return_type("A", "orNull", None, None), None, "a union with `self` too");
    assert_eq!(ret("nothing"), None);
    assert_eq!(ret("bare"), None);
    assert_eq!(fa.sub_return_type_at_arity("make", Some(0)), builder, "a function's declared return");
}

/// `self` and `static` name the class they are written in wherever a type
/// is written: a parameter, a property, a promoted parameter, a closure's
/// parameter, nullable or not.
#[test]
fn self_names_the_enclosing_class_in_every_written_type() {
    use crate::model::file_analysis::RefKind;
    let fa = php("<?php
namespace App;

class Node {
    private ?self $next;
    public function __construct(private self $peer) {}
    public function link(self $other) {
        $other->hop();
        $this->next->hop();
        $this->peer->hop();
        $f = function (?self $n) { $n->hop(); };
    }
    public function hop() {}
}
");
    let receivers: Vec<Option<String>> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "hop")
        .map(|r| fa.receiver_classes(r, None, crate::model::file_analysis::ReceiverPolicy::Navigate).single().map(str::to_string))
        .collect();
    let node = || Some("App\\Node".to_string());
    assert_eq!(receivers, [node(), node(), node(), node()]);
}

/// A call's value is its callee's declared return, so a chain of calls
/// types hop by hop: every receiver in `$this->with()->again()->…->tick()`
/// is the class the previous call returns, and a free call's value starts
/// a chain too.
#[test]
fn a_call_chain_types_through_declared_returns() {
    use crate::model::file_analysis::RefKind;
    let fa = php(RETURNS);
    let receivers: Vec<(&str, Option<String>)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }))
        .map(|r| (r.target_name.as_str(), fa.receiver_classes(r, None, crate::model::file_analysis::ReceiverPolicy::Navigate).single().map(str::to_string)))
        .collect();
    let builder = || Some("App\\Builder".to_string());
    let clock = || Some("Lib\\Clock".to_string());
    assert_eq!(
        receivers,
        [
            ("with", builder()),
            ("again", builder()),
            ("maybe", builder()),
            ("nullable", builder()),
            ("clock", builder()),
            ("tick", clock()),
            ("full", builder()),
            ("tick", clock()),
        ]
    );
}

/// From a chained receiver, goto-def and rename reach the member of the
/// class the previous call returns, in another file too, and hover on a
/// call shows the return it resolves to.
#[cfg(feature = "php")]
#[test]
fn a_chained_member_resolves_across_files_and_hovers_its_return() {
    let ws = Workspace::new(&[("/fake/php/Lib/Clock.php", CLOCK), ("/fake/php/App/Builder.php", RETURNS)]);
    let tick = vec![site("Clock.php", 4, 20)];
    assert_eq!(ws.defs("Builder.php", "tick();\n    }", 0), tick, "a method chain");
    assert_eq!(ws.defs("Builder.php", "full()->tick", 8), tick, "a chain from a free call");
    assert_eq!(ws.defs("Builder.php", "clock()->tick", 0), vec![site("Builder.php", 10, 20)]);
    assert_eq!(
        ws.renames("Clock.php", "tick", 0),
        [site("Builder.php", 17, 62), site("Builder.php", 22, 16), site("Clock.php", 4, 20)]
    );

    let fa = php(RETURNS);
    let hover = fa.hover_info(at(RETURNS, "clock()->tick", 0), RETURNS, None).unwrap_or_default();
    assert!(hover.contains("*returns: Lib\\Clock*"), "{hover}");
    let hover = fa.hover_info(at(RETURNS, "with()->again", 0), RETURNS, None).unwrap_or_default();
    assert!(hover.contains("*returns: App\\Builder*"), "{hover}");
}

const FLUENT_BASE: &str = "<?php
namespace Lib;

class Query {
    public function where(): static {}
    public function first(): Row {}
}

class Row {
    public function id(): int {}
}
";

const FLUENT_CHILD: &str = "<?php
namespace App;

use Lib\\Query;

class Users extends Query {
    public function active(): static {}
    public function run(Users $q) {
        $q->active()->where()->first()->id();
        $this->where()->first();
    }
}
";

/// An inherited method's declared return types a chain off the child, and
/// a `static` return names the class the method is written in: `where()`
/// declared on `Query` answers `Query`, wherever it is called from.
#[cfg(feature = "php")]
#[test]
fn an_inherited_declared_return_types_the_chain_across_files() {
    use crate::model::file_analysis::RefKind;
    let ws = Workspace::new(&[("/fake/php/Lib/Query.php", FLUENT_BASE), ("/fake/php/App/Users.php", FLUENT_CHILD)]);
    assert_eq!(ws.defs("Users.php", "where()->first()->id", 0), vec![site("Query.php", 4, 20)]);
    assert_eq!(ws.defs("Users.php", "first()->id", 0), vec![site("Query.php", 5, 20)]);
    assert_eq!(ws.defs("Users.php", "id()", 0), vec![site("Query.php", 9, 20)]);
    assert_eq!(ws.defs("Users.php", "first();", 0), vec![site("Query.php", 5, 20)]);

    let child = ws.idx.workspace_file_for_test(std::path::Path::new("/fake/php/App/Users.php")).unwrap();
    let receivers: Vec<(&str, Option<String>)> = child
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }))
        .map(|r| (r.target_name.as_str(), child.receiver_classes(r, Some(&ws.idx), crate::model::file_analysis::ReceiverPolicy::Navigate).single().map(str::to_string)))
        .collect();
    let s = |c: &str| Some(c.to_string());
    assert_eq!(
        receivers,
        [
            ("active", s("App\\Users")),
            ("where", s("App\\Users")),
            ("first", s("Lib\\Query")),
            ("id", s("Lib\\Row")),
            ("where", s("App\\Users")),
            ("first", s("Lib\\Query")),
        ]
    );
    // a call resolved in another file hovers with that file's declared
    // return, once the file is enriched as an open document is
    let mut open = (*child).clone();
    open.enrich_imported_types_with_keys(Some(&ws.idx));
    let hover = |needle: &str| open.hover_info(at(FLUENT_CHILD, needle, 0), FLUENT_CHILD, Some(&ws.idx)).unwrap_or_default();
    assert!(hover("where()->first();").contains("*returns: Lib\\Query*"), "{}", hover("where()->first();"));
    assert!(hover("first()->id").contains("*returns: Lib\\Row*"), "{}", hover("first()->id"));
}


const VARS: &str = "<?php
namespace App;

use Lib\\Clock;
use Lib\\Timer;

class Box {
    public ?Clock $clock;
    public function clock(): Clock {}
    public function run(Timer $t, array $xs) {
        $b = $this->clock();
        $b->tick();
        if ($t) { $d = $b; }
        $d->tick();
        $b = $t;
        $b->stop();
        foreach ($xs as $k => $v) { $v; }
        $this->clock = $b;
        $f = function () use ($b, &$z) { $e = $b; return $e; };
        $g = fn() => $d;
        $z;
        $e;
        $i = $this->clock()[0];
    }
}
";

const TIMER: &str = "<?php
namespace Lib;

class Timer {
    public function stop() {}
}
";

/// A body that sees no variable around it answers no type for one: a
/// function's `$config` is not the script's, and a method's bare `$peer`
/// is not the property a promoted parameter declares.
#[test]
fn a_closed_body_types_no_variable_of_the_code_around_it() {
    use crate::model::file_analysis::InferredType as T;
    let src = "<?php
namespace App;

class Clock { public function tick() {} }
function clock(): Clock {}

$config = clock();
function f() { return $config; }
class Box {
    public function __construct(private Clock $peer) {}
    public function m() { return $peer; }
}
";
    let fa = php(src);
    let clock = Some(T::ClassName("App\\Clock".into()));
    assert_eq!(fa.inferred_type_via_bag("config", at(src, "; }", 0)), None);
    assert_eq!(fa.inferred_type_via_bag("config", at(src, "\nfunction f", 0)), clock, "the script's own");
    assert_eq!(fa.inferred_type_via_bag("peer", at(src, "; }\n}", 0)), None);
}

/// (name, row, column) of every local `Variable` the analysis declares.
fn locals(fa: &FileAnalysis) -> Vec<(String, usize, usize)> {
    fa.symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Variable)
        .map(|s| (s.name.clone(), s.selection_span.start.row, s.selection_span.start.column))
        .collect()
}

/// A variable is its function's: the first write declares it, a write in
/// a branch is no block of its own, and every later write is a write of
/// that declaration. A closure's own writes are its own, and `use (&$z)`
/// declares `$z` in the function around the closure.
#[test]
fn a_write_declares_its_variable_once_per_function() {
    let fa = php(VARS);
    let mut got = locals(&fa);
    got.sort();
    let want: Vec<(String, usize, usize)> = [
        ("b", 10, 9),
        ("d", 12, 19),
        ("e", 18, 42),
        ("f", 18, 9),
        ("g", 19, 9),
        ("i", 22, 9),
        ("k", 16, 25),
        ("t", 9, 31),
        ("v", 16, 31),
        ("xs", 9, 41),
        ("z", 18, 36),
    ]
    .iter()
    .map(|(n, r, c)| (n.to_string(), *r, *c))
    .collect();
    assert_eq!(got, want);
    let scope_of = |n: &str| fa.symbols().iter().find(|s| s.kind == SymKind::Variable && s.name == n).unwrap().scope;
    assert_eq!(scope_of("d"), scope_of("b"), "a branch is no scope of its own");
    assert_eq!(scope_of("z"), scope_of("b"), "a by-reference capture declares around the closure");
    assert_ne!(scope_of("e"), scope_of("b"), "a closure's write is the closure's");
}

/// A by-name ask agrees with the binding a read carries: a read above the
/// write that declares its variable (a loop's) binds to that declaration,
/// and `resolve_variable` there names it too.
#[test]
fn a_by_name_ask_names_the_declaration_the_read_binds() {
    let src = "<?php\nfunction f($xs) {\n    foreach ($xs as $v) {\n        if ($acc) { $acc->add($v); }\n        $acc = $v;\n    }\n}\n";
    let fa = php(src);
    let read = Point::new(3, 13);
    let bound = fa.ref_at(read).and_then(|r| r.resolved_symbol()).expect("the read binds");
    assert_eq!(fa.symbol(bound).selection_span.start, Point::new(4, 9), "to the declaring write");
    assert_eq!(fa.resolve_variable("acc", read).map(|s| s.id), Some(bound));
}

/// A caught exception is an instance of the one class its clause names; a
/// clause naming several states no single type.
#[test]
fn a_caught_exception_is_its_clauses_class() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php("<?php\nfunction f() {\n    try {} catch (Oops $e) { $e; }\n    try {} catch (A | B $u) { $u; }\n}\n");
    assert_eq!(fa.inferred_type_via_bag("e", Point::new(2, 30)), Some(T::ClassName("Oops".into())));
    assert_eq!(fa.inferred_type_via_bag("u", Point::new(3, 31)), None);
}

/// A variable's value is what was last written to it: an assignment edges
/// to its right-hand side, so a declared return flows through it, a write
/// replaces what came before, and a value inside the right-hand side (a
/// subscripted call) is not the variable's.
#[test]
fn a_variables_value_flows_from_its_last_write() {
    use crate::model::file_analysis::{InferredType as T, Span};
    let fa = php(VARS);
    let recv = |needle: &str| {
        let start = at(VARS, needle, 0);
        let end = tree_sitter::Point { column: start.column + 2, ..start };
        fa.expr_type_at_span(Span { start, end }, None)
    };
    let clock = Some(T::ClassName("Lib\\Clock".into()));
    assert_eq!(recv("$b->tick"), clock, "a declared return");
    assert_eq!(recv("$d->tick"), clock, "through another variable");
    assert_eq!(recv("$b->stop"), Some(T::ClassName("Lib\\Timer".into())), "the later write replaces the earlier");
    assert_eq!(fa.inferred_type_via_bag("i", at(VARS, "    }\n}", 0)), None, "a subscripted call is not its value");
    // the closure reads the captured variable's value where it is written
    assert_eq!(fa.inferred_type_via_bag("e", at(VARS, "return $e", 7)), Some(T::ClassName("Lib\\Timer".into())));
}

/// Every use of a variable in its function is a reference to its one
/// declaration: rename rewrites the declaration, the later writes, the
/// reads above and below them, a closure's capture and its body's use, and
/// an arrow function's read. A closure's own variable stays the closure's.
#[cfg(feature = "php")]
#[test]
fn a_local_renames_with_every_write_and_read_in_its_function() {
    let at = |needle: &str| at(VARS, needle, 1);
    let renames = |p: tree_sitter::Point| renames(VARS, p.row, p.column);
    let sites = |rows: &[(usize, usize)]| rows.iter().map(|&(r, c)| (r, c, c + 1)).collect::<Vec<_>>();
    let b = sites(&[(10, 9), (11, 9), (12, 24), (14, 9), (15, 9), (17, 24), (18, 31), (18, 47)]);
    assert_eq!(renames(at("$b = $t")), b, "from a later write");
    assert_eq!(renames(at("$b; return")), b, "from the closure's use");
    assert_eq!(renames(at("$b, &")), b, "from the capture");
    assert_eq!(renames(at("$d->tick")), sites(&[(12, 19), (13, 9), (19, 22)]), "an arrow function sees the function's");
    assert_eq!(renames(at("$e = $b")), sites(&[(18, 42), (18, 58)]), "the closure's own");
    assert_eq!(renames(at("$z;")), sites(&[(18, 36), (20, 9)]), "declared by the by-reference capture");
    let ws = Workspace::new(&[("/fake/php/App/Box.php", VARS)]);
    assert_eq!(ws.defs("Box.php", "$b->stop", 1), vec![site("Box.php", 10, 9)]);
    assert_eq!(ws.defs("Box.php", "$e;\n        $i", 1), vec![], "no variable of the closure's");
}

/// Highlight tells a write from a read, and `$this->p = v` writes the
/// property.
#[cfg(feature = "php")]
#[test]
fn a_write_highlights_as_a_write() {
    use crate::model::file_analysis::{AccessKind, RefKind};
    let ws = Workspace::new(&[("/fake/php/App/Box.php", VARS)]);
    let mut kinds: Vec<(usize, usize, AccessKind)> = ws.ask("Box.php", "$b->stop", 1, |cs| {
        cs.highlights().into_iter().map(|l| (l.span.start.row, l.span.start.column, l.access)).collect()
    });
    kinds.sort_by_key(|&(r, c, _)| (r, c));
    assert_eq!(kinds[0], (10, 9, AccessKind::Declaration));
    assert_eq!(kinds[3], (14, 9, AccessKind::Write));
    assert!(kinds.iter().filter(|k| k.2 == AccessKind::Read).count() == kinds.len() - 2, "{kinds:?}");
    let fa = php(VARS);
    let clock: Vec<(usize, AccessKind)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::FieldAccess { .. }) && r.target_name == "clock")
        .map(|r| (r.span.start.row, r.access))
        .collect();
    assert_eq!(clock, [(17, AccessKind::Write)]);
}

/// An assignment writes the place its left side denotes: `$this->p = v`
/// gives the property `v`'s value and writes its token, and a compound
/// write writes it too.
#[test]
fn an_assignment_writes_the_member_its_left_side_reads() {
    use crate::model::file_analysis::{AccessKind, RefKind};
    let fa = php("<?php
namespace App;

class Clock { public function tick() {} }
function clock(): Clock {}

class Box {
    public $c;
    public $n;
    public function fill() {
        $this->c = clock();
        $this->n += 1;
        $this->c->tick();
    }
}
");
    let access = |name: &str| -> Vec<(usize, AccessKind)> {
        fa.refs()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::FieldAccess { .. }) && r.target_name == name)
            .map(|r| (r.span.start.row, r.access))
            .collect()
    };
    assert_eq!(access("c"), [(10, AccessKind::Write), (12, AccessKind::Read)]);
    assert_eq!(access("n"), [(11, AccessKind::Write)]);
    let tick = fa.refs().iter().find(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "tick").unwrap();
    assert_eq!(fa.receiver_classes(tick, None, crate::model::file_analysis::ReceiverPolicy::Navigate).single().map(str::to_string).as_deref(), Some("App\\Clock"));
}

/// A function sees no variable of the file around it: the script's
/// `$config` and the function's are two variables.
#[test]
fn a_function_sees_no_variable_of_the_code_around_it() {
    let src = "<?php\n$config = 1;\nfunction f() { $config = 2; return $config; }\n$config;\n";
    let fa = php(src);
    assert_eq!(locals(&fa), [("config".to_string(), 1, 1), ("config".to_string(), 2, 16)]);
    let bound = |row: usize| {
        let r = fa.refs().iter().find(|r| r.span.start.row == row && r.target_name == "config").unwrap();
        fa.symbol(r.resolved_symbol().unwrap()).selection_span.start.row
    };
    assert_eq!(bound(2), 2);
    assert_eq!(bound(3), 1);
}

/// A variable's call dispatches on the value written to it, in another
/// file too.
#[cfg(feature = "php")]
#[test]
fn a_call_on_a_variable_resolves_across_files() {
    let ws = Workspace::new(&[
        ("/fake/php/App/Box.php", VARS),
        ("/fake/php/Lib/Clock.php", CLOCK),
        ("/fake/php/Lib/Timer.php", TIMER),
    ]);
    assert_eq!(ws.defs("Box.php", "tick();\n        if", 0), vec![site("Clock.php", 4, 20)]);
    assert_eq!(ws.defs("Box.php", "tick();\n        $b", 0), vec![site("Clock.php", 4, 20)]);
    assert_eq!(ws.defs("Box.php", "stop()", 0), vec![site("Timer.php", 4, 20)]);
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
    let cs = resolve(&store, &fa, FileKey::Path(path), Point::new(3, 19), Some(&idx), OverrideScope::default());
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
    let cs = resolve(&store, &fa, FileKey::Path(p), at, Some(idx), OverrideScope::default());
    let short = |k: &FileKey| match k {
        FileKey::Path(p) => p.strip_prefix("/fake/php").unwrap().display().to_string(),
        FileKey::Url(u) => u.to_string(),
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

/// A member call up an `extends` chain whose parent nothing indexes
/// (`use Vendor\Model;`) lands on the same-leaf stand-in's member, and
/// says so: the diagnostic fires on the class tokens naming the parent.
/// A rename from the call never reaches the stand-in's declaration.
#[cfg(feature = "php")]
#[test]
fn a_member_call_through_an_unindexed_parent_widens_out_loud() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let sub = ("/fake/php/App/Sub.php", "<?php\nnamespace App;\nuse Vendor\\Model;\nclass Sub extends Model {\n    function go() { $this->name(); }\n}\n");
    let idx = php_indexed(&[PHP_STRANGER, sub]);
    let call = at(sub.1, "name()", 0);
    let (defs, hover) = php_landing(&idx, sub.0, call);
    assert_eq!(defs, [("A/Model.php".to_string(), 2, None)]);
    assert_eq!(hover.as_deref(), Some("A/Model.php"));
    let widened = |row: u32, column: u32| {
        (row, column, "nothing indexed declares `Vendor\\Model`; resolved by its leaf to `A\\Model`".to_string())
    };
    assert_eq!(php_widening(&idx, sub.0), [widened(2, 11), widened(3, 18)]);
    let p = std::path::PathBuf::from(sub.0);
    let fa = idx.workspace_file_for_test(&p).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(p.clone()), call, Some(&idx), OverrideScope::default());
    let edits: Vec<std::path::PathBuf> = cs
        .rename_edits("renamed")
        .unwrap()
        .into_iter()
        .map(|(l, _)| match l.key {
            FileKey::Path(p) => p,
            FileKey::Url(u) => u.to_file_path().unwrap(),
        })
        .collect();
    assert_eq!(edits, [p]);
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
/// One system types `$this`: one capture states it, one table names its
/// class, so its value is one witness.
#[test]
fn this_is_typed_once() {
    use crate::model::witnesses::WitnessAttachment as WA;
    let fa = php("<?php\nclass A {\n  function f() { return $this; }\n}\n");
    let this = Span { start: Point::new(2, 24), end: Point::new(2, 29) };
    let typed: Vec<_> = fa.witnesses.for_attachment(&WA::Expr(this)).into_iter().map(|w| w.source.clone()).collect();
    assert_eq!(typed.len(), 1, "one witness types `$this`: {typed:?}");
}

/// A call on a receiver that names a class dispatches on the receiver's
/// value, the one store of that class: no second, span-keyed copy picks a
/// named lookup.
#[test]
fn a_class_receiver_dispatches_on_its_value() {
    use crate::model::witnesses::{Callee, Lookup, WitnessAttachment as WA, WitnessPayload as WP};
    let fa = php("<?php\nclass A {\n  static function m() { return 1; }\n  function f() { return self::m(); }\n}\n");
    let call = Span { start: Point::new(3, 24), end: Point::new(3, 33) };
    let lookups: Vec<_> = fa
        .witnesses
        .for_attachment(&WA::Expr(call))
        .into_iter()
        .filter_map(|w| match &w.payload {
            WP::Invoke { callee: Callee::Method { lookup, .. }, .. } => Some(lookup.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(lookups, [Lookup::Receiver]);
    let recv = Span { start: Point::new(3, 24), end: Point::new(3, 28) };
    assert_eq!(fa.expr_type_at_span(recv, None), Some(InferredType::ClassName("A".into())));
}

#[test]
fn this_is_the_receiver_of_its_method() {
    let fa = php("<?php\nclass A {\n  function f() { return $this; }\n}\nfunction g() { return $this; }\n");
    let at = |row, col| fa.expr_type_at_span(Span { start: Point::new(row, col), end: Point::new(row, col + 5) }, None);
    assert_eq!(at(2, 24), Some(InferredType::ClassName("A".into())));
    assert_eq!(at(4, 22), None);
}
