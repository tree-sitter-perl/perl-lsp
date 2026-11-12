//! The PHP pack: namespaces, class declarations and the file's use-map.

use super::*;
use crate::build::packs::php_pack;
use crate::model::file_analysis::{FileAnalysis, SymKind};

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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
    use crate::model::file_analysis::NameTable::{Class, Function};
    let rows: Vec<_> = fa
        .pack
        .use_rows
        .iter()
        .map(|r| (r.table, r.name.namespace.as_str(), r.name.leaf.as_str(), r.alias.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            (Class, "Illuminate\\Support", "Collection", None),
            (Class, "Vendor\\Pkg", "Speaker", Some("Talks")),
            (Class, "App\\Contracts", "Listener", None),
            (Class, "App\\Contracts", "Logger", Some("Log")),
            (Function, "App\\Contracts", "helper", None),
            (Function, "App\\Helpers", "format", None),
            (Function, "App\\Helpers", "wrap", None),
            (crate::model::file_analysis::NameTable::Constant, "App\\Limits", "MAX", None),
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

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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

/// A `use function` / `use const` row binds in the function or constant
/// table, whether its keyword sits on the clause, on a whole group, or on
/// one clause of a group: it names no class, and an imported function's
/// leaf is a reference to that function.
#[cfg(feature = "php")]
#[test]
fn function_and_const_imports_bind_in_their_own_tables() {
    use crate::model::file_analysis::RefKind;
    let fa = php("<?php
namespace App;
use function Lib\\helper;
use const Lib\\LIMIT;
use function Lib\\Fns\\{wrap, unwrap};
use const Lib\\Limits\\{MAX};
use Lib\\Mixed\\{function f, const C, Klass};
");
    use crate::model::file_analysis::NameTable::{Class, Constant, Function};
    let rows: Vec<_> = fa.pack.use_rows.iter().map(|r| (r.table, r.name.identity("\\"))).collect();
    let row = |t, n: &str| (t, n.to_string());
    assert_eq!(
        rows,
        [
            row(Function, "Lib\\helper"),
            row(Constant, "Lib\\LIMIT"),
            row(Function, "Lib\\Fns\\wrap"),
            row(Function, "Lib\\Fns\\unwrap"),
            row(Constant, "Lib\\Limits\\MAX"),
            row(Function, "Lib\\Mixed\\f"),
            row(Constant, "Lib\\Mixed\\C"),
            row(Class, "Lib\\Mixed\\Klass"),
        ]
    );
    let refs = |kind: RefKind| -> Vec<(String, Option<String>)> {
        fa.refs()
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| (r.target_name.clone(), r.resolved_package().map(str::to_string)))
            .collect()
    };
    assert_eq!(refs(RefKind::PackageRef), [("Klass".to_string(), None)]);
    let lib = |ns: &str| Some(format!("Lib{ns}"));
    assert_eq!(
        refs(RefKind::ImportedName),
        [
            ("helper".to_string(), lib("")),
            ("wrap".to_string(), lib("\\Fns")),
            ("unwrap".to_string(), lib("\\Fns")),
            ("f".to_string(), lib("\\Mixed")),
        ]
    );
    // no class of the leaf's spelling is what the row binds
    assert_eq!(fa.class_spelling_identity("helper", tree_sitter::Point::new(7, 0)), "App\\helper");
}

/// A class's uses in another file are found by the identity they name:
/// goto-def from a parent clause lands on the declaration, and rename
/// rewrites the declaration, the import's leaf and the parent's leaf.
#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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
    assert_eq!(flags("make"), F::STATIC | F::PRIVATE);
    assert_eq!(flags("find"), F::STATIC | F::PROTECTED);
    assert_eq!(flags("save"), F::ABSTRACT | F::CONTRACT);
    assert_eq!(flags("plain"), F::empty());
    assert_eq!(flags("load"), F::CONTRACT);
    assert!(fa.symbols().iter().find(|s| s.name == "__construct").unwrap().is_constructor());
    // a catch-all member is a fact about the class that declares it
    assert_eq!(flags("Magic"), F::DYNAMIC_MEMBERS);
    assert_eq!(flags("Proxy"), F::TRAIT | F::DYNAMIC_MEMBERS);
    assert_eq!(flags("Repo"), F::ABSTRACT);
}

#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
    assert_eq!(field("clock").flags, F::STATIC | F::PROTECTED);
    assert_eq!(field("made").flags, F::PRIVATE);
    assert_eq!(field("qty").flags, F::READONLY);
    assert_eq!(field("id").flags, F::PRIVATE | F::READONLY);
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
#[cfg(feature = "php")]
#[test]
fn a_declared_property_type_is_the_members_value() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(MEMBERS);
    let order = T::ClassName("App\\Order".into());
    let value = |m: &str| fa.field_value_type(&order, m, None, crate::model::file_analysis::MemberKind::VALUE);
    // A static property answers an ask in its own namespace (`Order::$clock`).
    let statics = crate::model::file_analysis::MemberKind {
        mode: crate::model::file_analysis::MemberMode::Value,
        spaces: Some(crate::model::file_analysis::SpaceAsk {
            search: crate::model::file_analysis::SpaceSet::of(fa.member_spaces().id_of("static_property").unwrap()),
            illegal: Default::default(),
        }),
    };
    let static_value = |m: &str| fa.field_value_type(&order, m, None, statics);
    assert_eq!(value("clock"), None, "`->` reaches no static property");
    assert_eq!(value("total"), Some(T::Numeric));
    assert_eq!(value("count"), Some(T::Numeric));
    assert_eq!(value("id"), Some(T::String));
    assert_eq!(value("name"), Some(T::String));
    let or = |a: T, b: T| crate::model::file_analysis::join_all(&[a, b], &crate::model::file_analysis::Unordered);
    assert_eq!(static_value("clock"), or(T::ClassName("Lib\\Clock".into()), T::Undef), "`?T` is `T|null`");
    assert_eq!(value("made"), Some(T::ClassName("Lib\\Clock".into())));
    assert_eq!(value("qty"), Some(T::Numeric));
    assert_eq!(value("legacy"), None);
    assert_eq!(value("key"), or(T::Numeric, T::String), "a union is the join of its arms");
    assert_eq!(value("plain"), None, "a plain parameter is no property");
}

/// An intersection or a DNF group names no single class, and an arm that
/// can't be read types nothing rather than claiming the arms that can.
#[cfg(feature = "php")]
#[test]
fn a_type_with_an_unreadable_arm_types_nothing() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php("<?php
class Box {
    public Clock&Countable $both;
    public (A&B)|null $group;
    public array|string $mixed;
    public ?string $maybe;
}
");
    let boxed = T::ClassName("Box".into());
    let value = |m: &str| fa.field_value_type(&boxed, m, None, crate::model::file_analysis::MemberKind::VALUE);
    assert_eq!(value("both"), None);
    assert_eq!(value("group"), None);
    assert_eq!(value("mixed"), None);
    let maybe = value("maybe");
    assert_eq!(maybe, crate::model::file_analysis::join_all(&[T::String, T::Undef], &crate::model::file_analysis::Unordered));
    assert_eq!(fa.render_type(&maybe.unwrap()), "string|null", "a nullable reads in PHP's words");
}

#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
    let cs = resolve(&store, &file, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path)), at, Some(&idx), OverrideScope::default()).with_source(MEMBERS);
    let hover = crate::lsp::symbols::pack_hover_markdown(&cs, "php").unwrap();
    assert!(hover.contains("\n$total: "), "{hover}");
}

/// Rename from a property's declaration rewrites its bare name, never the
/// `$` (a typed `$` is the sigil, not part of the name), and never the
/// method that shares the name.
#[cfg(feature = "php")]
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
        let cs = resolve(&store, &file, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), at, Some(&idx), OverrideScope::default());
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
    let cs = resolve(&store, &file, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), at, Some(&idx), OverrideScope::default());
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
#[cfg(feature = "php")]
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
/// spelling resolves through the use-map, a union is the join of its arms.
#[cfg(feature = "php")]
#[test]
fn a_parameters_declared_type_is_its_value_in_the_body() {
    use crate::model::file_analysis::InferredType as T;
    use crate::model::witnesses::WitnessAttachment as WA;
    let fa = php(PARAMS);
    let body = tree_sitter::Point { row: 6, column: 12 };
    let clock = || T::ClassName("Lib\\Clock".into());
    let or = |a: T, b: T| crate::model::file_analysis::join_all(&[a, b], &crate::model::file_analysis::Unordered);
    assert_eq!(fa.inferred_type_via_bag("at", body), or(clock(), T::Undef));
    assert_eq!(fa.inferred_type_via_bag("n", body), or(T::Numeric, clock()));
    assert_eq!(fa.inferred_type_via_bag("rest", body), None, "a variadic's type is its elements'");
    let at = fa.symbols().iter().find(|s| s.name == "at").unwrap();
    let declared: Vec<_> = fa
        .witnesses
        .for_attachment(&WA::Place(Box::new(fa.declared_place(at.id))))
        .into_iter()
        .map(|w| w.span.start)
        .collect();
    assert_eq!(declared, [at.selection_span.start], "the witness stands at the binding site");
    // the read's value is the parameter's
    let read = crate::model::file_analysis::Span {
        start: tree_sitter::Point { row: 6, column: 11 },
        end: tree_sitter::Point { row: 6, column: 14 },
    };
    assert_eq!(fa.expr_type_at_span(read, None), or(clock(), T::Undef));
}

/// A use of `$x` in the body is a reference to the parameter: the name
/// token is the reference, so rename never touches the `$`. A read the
/// callable declares no parameter for binds to nothing.
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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

const BARE: &str = "<?php
class A {
    private int $x = 1;
    public function f() { return $x; }
    public function g(int $x) { return $x; }
}
trait T {
    private $y;
    public function h() { return $y; }
}
";

/// PHP names a property only through a receiver, so a method's bare `$x`
/// is a local of the method or nothing — never the property the class
/// body declares (`@scope.direct`).
#[cfg(feature = "php")]
#[test]
fn a_bare_variable_in_a_method_never_names_a_property() {
    use crate::model::file_analysis::RefKind;
    let fa = php(BARE);
    let bound = |row: usize, col: usize| {
        let r = fa
            .refs()
            .iter()
            .find(|r| matches!(r.kind, RefKind::Variable) && (r.span.start.row, r.span.start.column) == (row, col))
            .unwrap_or_else(|| panic!("a ref at {row}:{col}"));
        let bound = r.resolved_symbol().map(|id| (fa.symbol(id).kind, fa.symbol(id).selection_span.start.column));
        let named = fa
            .resolve_variable(&r.target_name, r.span.start)
            .map(|s| (s.kind, s.selection_span.start.column));
        assert_eq!(bound, named, "the bound ref and a by-name ask agree at {row}:{col}");
        bound
    };
    assert_eq!(bound(3, 34), None, "no local `$x`: the property is not in force");
    assert_eq!(bound(4, 40), Some((SymKind::Variable, 27)), "the parameter is");
    assert_eq!(bound(8, 34), None, "a trait's property neither");
}

/// The promoted pair still binds in its constructor: the token is the
/// constructor's local, and the local is in force in the constructor body.
#[cfg(feature = "php")]
#[test]
fn a_promoted_parameter_binds_in_its_constructor() {
    use crate::model::file_analysis::RefKind;
    let fa = php(PROMOTED);
    let local = fa.symbols().iter().find(|s| s.kind == SymKind::Variable && s.name == "made").unwrap();
    let r = fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::Variable) && (r.span.start.row, r.span.start.column) == (2, 56))
        .unwrap();
    assert_eq!(r.resolved_symbol(), Some(local.id));
}

#[cfg(feature = "php")]
const UNBOUND: &str = "<?php
class C {
    public $x;
    public function foo() {}
    public function m() { return $x; }
    public function n() { return $foo; }
}
";

/// A token a query captured but nothing binds has no definition: the
/// by-word last resort is for tokens no query captures, so it never lands
/// a method's bare `$x` on the property, or a bare `$foo` on the method
/// its word spells.
#[cfg(feature = "php")]
#[test]
fn an_unbound_captured_token_has_no_definition() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let path = std::path::PathBuf::from("/fake/php/App/C.php");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), php(UNBOUND));
    let file = idx.workspace_file_for_test(&path).unwrap();
    let store = OpenDocs::new();
    let defs = |row: usize, column: usize| {
        let at = tree_sitter::Point { row, column };
        assert!(file.ref_at(at).is_some(), "a query captured {row}:{column}");
        resolve(&store, &file, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), at, Some(&idx), OverrideScope::default())
            .with_source(UNBOUND)
            .definitions()
            .into_iter()
            .map(|l| (l.span.start.row, l.span.start.column))
            .collect::<Vec<_>>()
    };
    assert_eq!(defs(4, 34), [], "a bare `$x` is not the property");
    assert_eq!(defs(5, 34), [], "a bare `$foo` is not the method");
}

/// Every class a type position names is a class reference through the
/// use-map, so rename of the class rewrites property, parameter, nullable,
/// union, qualified and return spellings.
#[cfg(feature = "php")]
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
    let cs = resolve(&store, &clock, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(p_clock.clone())), at, Some(&idx), OverrideScope::default());
    let mut sites: Vec<(usize, usize)> = cs
        .rename_edits("Timer")
        .unwrap()
        .into_iter()
        .filter(|(loc, _)| matches!(&loc.key, FileKey::Path(p) if p.as_path() == p_user.as_path()))
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
#[cfg(any(feature = "php", feature = "cpp"))]
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
        f(&resolve(&self.store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), point, Some(&self.idx), OverrideScope::default()))
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
#[cfg(feature = "php")]
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
                    use crate::model::witnesses::WitnessAttachment as WA;
                    match (lookup, receiver) {
                        (Lookup::Receiver, Some(WA::Expr(recv))) => {
                            // The class the call dispatches on: a `?Timer` receiver is
                            // `Timer|null`, and a call reaches the `Timer` arm.
                            fa.expr_type_at_span(*recv, None)
                                .and_then(|t| fa.dispatch_classes_of(&t, None).single().map(str::to_string))
                        }
                        // `self::m()` forwards: looked up from the class it
                        // names, run on the method's own receiver.
                        (Lookup::Named(class), Some(WA::Place(place))) => {
                            assert!(place.is_variable() && matches!(place.root, crate::model::file_analysis::Binding::Receiver(_)));
                            Some(class.clone())
                        }
                        other => panic!("unexpected call shape {other:?}"),
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
    // a reference that reads the method's receiver, declaring nothing
    let reads: Vec<_> = fa.refs().iter().filter(|r| r.target_name == "this").collect();
    assert!(!reads.is_empty());
    assert!(reads.iter().all(|r| r.receiver_method().is_some() && r.resolved_symbol().is_none()), "{reads:?}");
    assert!(fa.symbols().iter().all(|s| s.name != "this" && s.name != "$this"));
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
#[cfg(feature = "php")]
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

/// A free call is an `Invoke` of the function its name names, by identity,
/// and its ref is pinned to that function's namespace: a bare name is the
/// namespace in force's own function when the file declares one, else that
/// function falling back to the global one, and pins neither; a qualified
/// name resolves like a class spelling.
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
            ("App\\local".to_string(), None, true, Some(vec![false, true])),
            ("Lib\\helper".to_string(), None, false, Some(vec![false])),
            ("App\\Sub\\helper".to_string(), None, false, Some(vec![])),
            ("App\\strlen".to_string(), Some("strlen".to_string()), false, Some(vec![false])),
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

/// (callee, its fallback, defined in this file, which arguments spread) of
/// each free call.
fn call_invokes(fa: &FileAnalysis) -> Vec<(String, Option<String>, bool, Option<Vec<bool>>)> {
    use crate::model::witnesses::{CallArg, Callee, WitnessPayload};
    fa.witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Function { name, local, fallback, .. }, args, .. } => Some((
                name.clone(),
                fallback.clone(),
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
/// written in, a nullable class is the class or null, a union is the join
/// of its arms, and `void` or no declaration states no type.
#[cfg(feature = "php")]
#[test]
fn a_declared_return_is_the_callables_value() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(RETURNS);
    let ret = |m: &str| fa.find_method_return_type("App\\Builder", m, None, None);
    let builder = Some(T::ClassName("App\\Builder".into()));
    let clock = Some(T::ClassName("Lib\\Clock".into()));
    assert_eq!(ret("with"), builder, "`static`");
    assert_eq!(ret("again"), builder, "`self`");
    let or = |a: T, b: T| crate::model::file_analysis::join_all(&[a, b], &crate::model::file_analysis::Unordered);
    assert_eq!(ret("maybe"), or(T::ClassName("App\\Builder".into()), T::Undef), "a nullable class");
    assert_eq!(ret("nullable"), builder, "a nullable `static`");
    assert_eq!(ret("clock"), clock, "an imported class");
    assert_eq!(ret("full"), clock, "a fully qualified class");
    assert_eq!(ret("count"), Some(T::Numeric));
    assert_eq!(ret("either"), or(T::ClassName("Lib\\Clock".into()), T::Numeric), "a union is the join of its arms");
    let union = php("<?php\nclass A {\n    public function orNull(): self|null {}\n    public function either(): self|int {}\n}\n");
    let a = Some(T::ClassName("A".into()));
    assert_eq!(union.find_method_return_type("A", "orNull", None, None), a, "`self|null` reads as `?self`");
    assert_eq!(union.find_method_return_type("A", "either", None, None), None, "a union of `self` with another type");
    assert_eq!(ret("nothing"), None);
    assert_eq!(ret("bare"), None);
    assert_eq!(fa.sub_return_type_at_arity("make", Some(0)), builder, "a function's declared return");
}

/// `self` and `static` name the class they are written in wherever a type
/// is written: a parameter, a property, a promoted parameter, a closure's
/// parameter, nullable or not.
#[cfg(feature = "php")]
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

/// `self` and `parent` name a class only inside a class body: a method's
/// return, parameter and caught exception name the class around it (or
/// the one it extends), through a nested closure too, while a free
/// function's name none, whatever namespace holds it.
#[cfg(feature = "php")]
#[test]
fn self_and_parent_name_no_class_outside_a_class_body() {
    use crate::model::file_analysis::{InferredType as T, RefKind};
    let fa = php("<?php
namespace App;

class Base { public function hop() {} }
class Node extends Base {
    public function me(): self {}
    public function up(): parent {}
    public function link(self $other, parent $up) {
        try {} catch (self $e) { $e->hop(); }
        $other->hop();
        $up->hop();
        $f = function (self $n) { $n->hop(); };
    }
}
function made(): self {}
function loose(self $x, parent $y) {
    try {} catch (self $e) { $e->hop(); }
    $x->hop();
    $y->hop();
}
");
    let ret = |m: &str| fa.find_method_return_type("App\\Node", m, None, None);
    assert_eq!(ret("me"), Some(T::ClassName("App\\Node".into())));
    assert_eq!(ret("up"), Some(T::ClassName("App\\Base".into())));
    assert_eq!(fa.sub_return_type_at_arity("made", Some(0)), None, "a free function's `self`");
    let receivers: Vec<(usize, Option<String>)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "hop")
        .map(|r| {
            let class = fa.receiver_classes(r, None, crate::model::file_analysis::ReceiverPolicy::Navigate);
            (r.span.start.row, class.single().map(str::to_string))
        })
        .collect();
    let class = |c: &str| Some(c.to_string());
    assert_eq!(
        receivers,
        [
            (8, class("App\\Node")),
            (9, class("App\\Node")),
            (10, class("App\\Base")),
            (11, class("App\\Node")),
            (16, None),
            (17, None),
            (18, None),
        ]
    );
}

/// A call's value is its callee's declared return, so a chain of calls
/// types hop by hop: every receiver in `$this->with()->again()->…->tick()`
/// is the class the previous call returns, and a free call's value starts
/// a chain too.
#[cfg(feature = "php")]
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

/// Hover on a typed call into another file shows the method's declaration
/// and the file it lives in, never `name: Type` as if it were a field; a
/// property shows its type, and both read in PHP's words (`bool`, not the
/// engine's `Bool`).
#[cfg(feature = "php")]
#[test]
fn a_cross_file_member_call_hovers_as_its_declaration_in_php_words() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let handler = "<?php\nnamespace Lib;\n\nclass Handler {\n    public bool $on;\n    public function isHandling(int $level): bool {}\n}\n";
    let runner = "<?php\nnamespace App;\n\nuse Lib\\Handler;\n\nclass Runner {\n    public function run(Handler $h) {\n        $h->isHandling(1);\n        return $h->on;\n    }\n}\n";
    let root = std::env::temp_dir().join(format!("php-hover-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    for (name, src) in [("Handler.php", handler), ("Runner.php", runner)] {
        std::fs::write(root.join(name), src).unwrap();
        idx.insert_workspace_for_test(root.join(name), php(src));
    }
    let path = root.join("Runner.php");
    let file = idx.workspace_file_for_test(&path).unwrap();
    let docs = OpenDocs::new();
    let hover = |needle: &str| {
        let key = FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone()));
        let cs = resolve(&docs, &file, key, at(runner, needle, 0), Some(&idx), OverrideScope::default()).with_source(runner);
        crate::lsp::symbols::pack_hover_markdown(&cs, "php").unwrap_or_default()
    };
    let call = hover("isHandling(1)");
    let field = hover("on;");
    std::fs::remove_dir_all(&root).ok();
    assert!(call.contains("public function isHandling(int $level): bool"), "{call}");
    assert!(call.contains("Handler.php"), "{call}");
    assert!(!call.contains("*member*") && !call.contains("Bool"), "{call}");
    assert!(field.contains("on: bool"), "{field}");
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
/// a `static` return names the class the call is made on: `where()`
/// declared on `Query` and called on a `Users` answers `Users`.
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
            ("first", s("App\\Users")),
            ("id", s("Lib\\Row")),
            ("where", s("App\\Users")),
            ("first", s("App\\Users")),
        ]
    );
    // a call resolved in another file hovers with that file's declared
    // return, once the file is enriched as an open document is
    let mut open = (*child).clone();
    open.enrich_imported_types_with_keys(Some(&ws.idx));
    let hover = |needle: &str| open.hover_info(at(FLUENT_CHILD, needle, 0), FLUENT_CHILD, Some(&ws.idx)).unwrap_or_default();
    assert!(hover("where()->first();").contains("*returns: App\\Users*"), "{}", hover("where()->first();"));
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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
#[cfg(feature = "php")]
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

const NEW: &str = "<?php
namespace App;

use Lib\\Clock;

class Base {
    public function __construct(int $n) {}
}
class Box extends Base {
    public function __construct(Clock $c) {}
    public function make(Clock $k) {
        $a = new Clock(1);
        $b = new \\Lib\\Clock;
        $c = new self(new Clock());
        $d = new static($k);
        $e = new parent(2);
        $cls = Clock::class;
        $f = new $cls();
        $g = $cls::tick();
        $h = new class($a) extends Base {
            public function hi() { return $this; }
        };
        $h->hi();
        $i = self::class;
    }
}
";

/// A construction's value is an instance of the class it names, through
/// the use-map: `self` is the class the method is written in, and so is
/// `static` where no call says which class it is made on,
/// `parent` the one it extends, `new $cls` the class the variable holds,
/// and an anonymous class is its own class. A class literal is its class.
#[cfg(feature = "php")]
#[test]
fn a_construction_is_an_instance_of_its_class() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(NEW);
    let end = at(NEW, "    }\n}", 0);
    let class = |c: &str| Some(T::ClassName(c.into()));
    let ty = |v: &str| fa.inferred_type_via_bag(v, end);
    assert_eq!(ty("a"), class("Lib\\Clock"));
    assert_eq!(ty("b"), class("Lib\\Clock"), "fully qualified, without an argument list");
    assert_eq!(ty("c"), class("App\\Box"));
    assert_eq!(ty("d"), class("App\\Box"), "`static` asked with no call site");
    assert_eq!(ty("e"), class("App\\Base"));
    assert_eq!(ty("cls"), class("Lib\\Clock"), "a class literal");
    assert_eq!(ty("f"), class("Lib\\Clock"), "the class a variable holds");
    assert_eq!(ty("h"), class("App\\class@anonymous:20:18"));
    assert_eq!(ty("i"), class("App\\Box"));
}

const PARENT: &str = "<?php
namespace App;
interface Shape {}
trait Named { public function name(): string { return ''; } }
class Base {
    public function base(): self { return $this; }
}
class Kid extends Base implements Shape {
    use Named;
    public parent $p;
    public function own(parent $q): ?parent {
        $a = parent::base();
        $b = parent::class;
        $c = new parent();
        $d = $q;
        $e = $this->p;
        return $a;
    }
}
class Lone implements Shape {
    public function f(): parent {
        $x = new parent();
        $y = parent::class;
        $z = new $u();
    }
}
";

/// `parent` names the class the writing class extends, wherever it is
/// written: a call's receiver, a class literal, a construction, and a
/// written type. A class that extends nothing has no `parent`, whatever
/// it implements or uses.
#[cfg(feature = "php")]
#[test]
fn parent_names_the_class_extended() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(PARENT);
    let base = Some(T::ClassName("App\\Base".into()));
    let kid_end = at(PARENT, "        return $a;", 0);
    let ty = |v: &str, at| fa.inferred_type_via_bag(v, at);
    assert_eq!(ty("a", kid_end), base, "`parent::m()` dispatches on the extended class");
    assert_eq!(ty("b", kid_end), base, "`parent::class`");
    assert_eq!(ty("c", kid_end), base, "`new parent`");
    assert_eq!(ty("d", kid_end), base, "a parameter typed `parent`");
    assert_eq!(ty("e", kid_end), base, "a property typed `parent`");
    let ret = |class: &str, m: &str| fa.find_method_return_type(class, m, None, None);
    assert_eq!(ret("App\\Kid", "own"), base, "a nullable `parent` return");
    // an interface or a trait is never what `parent` names
    let lone_end = at(PARENT, "        $z = new $u();", 0);
    assert_eq!(ty("x", lone_end), None);
    assert_eq!(ty("y", lone_end), None);
    // a construction whose class nothing types answers nothing, and its
    // ref on the class token is not its own receiver
    assert_eq!(ty("z", at(PARENT, "$z = new $u();\n", 15)), None);
    assert_eq!(ret("App\\Lone", "f"), None);
}

/// A construction is an `Invoke` of the constructor on the class its
/// receiver's value is, with its arguments, and a reference on its class
/// token keyed by that class, never by a constructor name.
#[cfg(feature = "php")]
#[test]
fn a_construction_calls_its_classs_constructor() {
    use crate::model::witnesses::{Callee, Lookup, WitnessPayload};
    let fa = php(NEW);
    let calls: Vec<(String, usize)> = fa
        .witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Construct { lookup }, receiver, args } => {
                // the class is the receiver's value, the one store of it
                assert_eq!(*lookup, Lookup::Receiver);
                let on = match receiver {
                    Some(crate::model::witnesses::WitnessAttachment::Expr(recv)) => fa
                        .expr_type_at_span(*recv, None)
                        .and_then(|t| t.class_name().map(str::to_string))
                        .unwrap_or_else(|| "untyped".to_string()),
                    _ => "untyped".to_string(),
                };
                Some((on, args.as_ref().map_or(0, Vec::len)))
            }
            _ => None,
        })
        .collect();
    let want: Vec<(String, usize)> = [
        ("Lib\\Clock", 1),
        ("Lib\\Clock", 0),
        ("App\\Box", 1),
        ("Lib\\Clock", 0),
        ("App\\Box", 1),
        ("App\\Base", 1),
        ("Lib\\Clock", 0),
        ("App\\class@anonymous:20:18", 1),
    ]
    .iter()
    .map(|(c, n)| (c.to_string(), *n))
    .collect();
    assert_eq!(calls, want);
    let sites: Vec<(usize, usize, &str)> = fa
        .refs()
        .iter()
        .filter(|r| r.constructs())
        .map(|r| (r.span.start.row, r.span.start.column, r.target_name.as_str()))
        .collect();
    assert_eq!(
        sites,
        [
            (11, 17, "Lib\\Clock"),
            (12, 17, "Lib\\Clock"),
            (13, 17, "App\\Box"),
            (13, 26, "Lib\\Clock"),
            (14, 17, "App\\Box"),
            (15, 17, "App\\Base"),
            (17, 17, "Lib\\Clock"),
            (19, 17, "App\\class@anonymous:20:18"),
        ]
    );
}

/// The class token of a construction is the class: goto-def and a class
/// rename reach it. The site is a reference to the constructor, which
/// `self`, `static` and `parent` reach, and which rename leaves alone,
/// since no site spells its name.
#[cfg(feature = "php")]
#[test]
fn a_construction_names_its_class_and_reaches_its_constructor() {
    let ws = Workspace::new(&[("/fake/php/App/Box.php", NEW), ("/fake/php/Lib/Clock.php", CLOCK)]);
    let clock = vec![site("Clock.php", 3, 6)];
    assert_eq!(ws.defs("Box.php", "Clock(1)", 0), clock);
    assert_eq!(ws.defs("Box.php", "\\Lib\\Clock;", 5), clock);
    assert_eq!(ws.defs("Box.php", "Clock::class", 0), clock);
    assert_eq!(ws.defs("Box.php", "self(new", 0), vec![site("Box.php", 9, 20)]);
    assert_eq!(ws.defs("Box.php", "parent(2)", 0), vec![site("Box.php", 6, 20)]);
    assert_eq!(ws.defs("Box.php", "tick();", 0), vec![site("Clock.php", 4, 20)], "a class a variable holds");
    let refs = ws.ask("Box.php", "__construct(Clock", 0, |cs| Workspace::sites(cs.references()));
    // The constructor's references are its override family's: `parent`
    // and the anonymous subclass construct `Base`'s.
    let family = [(6, 20), (9, 20), (13, 17), (14, 17), (15, 17), (19, 17)];
    assert_eq!(refs, family.map(|(r, c)| site("Box.php", r, c)));
    assert!(ws.ask("Box.php", "__construct(Clock", 0, |cs| cs.rename_edits("make").is_err()));
    let renamed = ws.renames("Clock.php", "Clock {", 0);
    let in_box: Vec<(usize, usize)> =
        renamed.iter().filter(|(f, ..)| f == "Box.php").map(|&(_, r, c)| (r, c)).collect();
    assert_eq!(in_box, [(3, 8), (9, 32), (10, 25), (11, 17), (12, 22), (13, 26), (16, 15)]);
}

const NEW_BY_VARIABLE: &str = "<?php
namespace App;

use Lib\\Timer;

class Maker {
    public function make() {
        $cls = Timer::class;
        return new $cls(1);
    }
}
";

const NEW_BY_VARIABLE_CLASS: &str = "<?php
namespace Lib;

class Timer {
    public function __construct(int $n) {}
}
";

/// A construction through a variable is keyed by the class the variable
/// holds, so the constructor's references reach it from another file.
#[cfg(feature = "php")]
#[test]
fn a_constructor_reaches_a_construction_through_a_variable_in_another_file() {
    let ws = Workspace::new(&[("/fake/php/App/Maker.php", NEW_BY_VARIABLE), ("/fake/php/Lib/Timer.php", NEW_BY_VARIABLE_CLASS)]);
    let refs = ws.ask("Timer.php", "__construct", 0, |cs| Workspace::sites(cs.references()));
    assert_eq!(refs, vec![site("Maker.php", 8, 19), site("Timer.php", 4, 20)]);
}

const MODEL: &str = "<?php
namespace Lib;

class Model {
    public function __construct(array $a = []) {}
}
";

const MODELS: &str = "<?php
namespace App;

use Lib\\Model;

class User extends Model {}
class Post extends Model {
    public function __construct() {}
}
class Admin extends User {}
";

const MAKES: &str = "<?php
namespace App;

$u = new User();
$p = new Post();
$a = new Admin();
$m = new \\Lib\\Model([]);
";

/// A construction runs the constructor its class declares or inherits,
/// found by its flag, in whichever file declares it: a constructor's
/// references are the constructions of the classes that inherit it, from
/// files that never spell its name, and a class that overrides it
/// constructs through its own.
#[cfg(feature = "php")]
#[test]
fn a_construction_reaches_an_inherited_constructor_across_files() {
    let ws = Workspace::new(&[
        ("/fake/php/Lib/Model.php", MODEL),
        ("/fake/php/App/Models.php", MODELS),
        ("/fake/php/App/Makes.php", MAKES),
    ]);
    // The override family (the default scope) takes in `Post`'s, whose
    // constructions are its own.
    let family = vec![
        site("Makes.php", 3, 9),
        site("Makes.php", 4, 9),
        site("Makes.php", 5, 9),
        site("Makes.php", 6, 9),
        site("Model.php", 4, 20),
        site("Models.php", 7, 20),
    ];
    assert_eq!(ws.ask("Model.php", "__construct", 0, |cs| Workspace::sites(cs.references())), family);
    assert_eq!(ws.ask("Models.php", "__construct", 0, |cs| Workspace::sites(cs.references())), family);
    // Dispatch keeps to the constructions that run it: `User` and `Admin`
    // inherit `Model`'s, `Post` runs its own.
    let dispatch = |file: &str| {
        use crate::index::open_docs::FileKey;
        use crate::index::resolve::{resolve, OverrideScope};
        let (path, src) = ws.files.iter().find(|(p, _)| p.ends_with(file)).unwrap();
        let fa = ws.idx.workspace_file_for_test(path).unwrap();
        let cs = resolve(&ws.store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), at(src, "__construct", 0), Some(&ws.idx), OverrideScope::Dispatch);
        Workspace::sites(cs.references())
    };
    assert_eq!(
        dispatch("Model.php"),
        [site("Makes.php", 3, 9), site("Makes.php", 5, 9), site("Makes.php", 6, 9), site("Model.php", 4, 20)]
    );
    assert_eq!(dispatch("Models.php"), [site("Makes.php", 4, 9), site("Models.php", 7, 20)]);
    // A constructor is retrieved under the classes that may construct
    // through it, from the in-memory class graph.
    let model = ws.idx.workspace_file_for_test(&std::path::PathBuf::from("/fake/php/Lib/Model.php")).unwrap();
    let target = crate::index::resolve::TargetRef::method(
        "__construct".into(),
        "Lib\\Model".into(),
        None,
        &model,
        Some(&ws.idx),
        crate::index::resolve::OverrideScope::Dispatch,
    );
    let mut keys = target.retrieval_keys(model.names());
    keys.sort();
    let mut want: Vec<String> = ["__construct", "Lib\\Model", "App\\User", "App\\Post", "App\\Admin"]
        .iter()
        .map(|n| crate::model::file_analysis::name_match_key(n, model.names()))
        .collect();
    want.sort();
    want.dedup();
    assert_eq!(keys, want);
    // The stamp freezes the class each construction dispatches on, keyed by
    // that class's identity.
    let fa = ws.idx.workspace_file_for_test(&std::path::PathBuf::from("/fake/php/App/Makes.php")).unwrap();
    let stamped: Vec<(&str, Option<&str>)> = fa
        .refs()
        .iter()
        .filter(|r| r.constructs())
        .map(|r| (r.target_name.as_str(), r.method_target().map(|e| e.invocant_class())))
        .collect();
    assert_eq!(
        stamped,
        [
            ("App\\User", Some("App\\User")),
            ("App\\Post", Some("App\\Post")),
            ("App\\Admin", Some("App\\Admin")),
            ("Lib\\Model", Some("Lib\\Model")),
        ]
    );
}

/// A class constructs whether or not it declares a constructor, so a
/// construction is never an unresolved method, whatever its class token
/// spells.
#[cfg(feature = "php")]
#[test]
fn a_construction_is_never_an_unresolved_method() {
    let fa = php("<?php\nclass Plain { public function go() {} }\n$p = new Plain();\nPlain::gone();\n");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let messages: Vec<String> = crate::lsp::symbols::collect_diagnostics(&fa, &idx, Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    // `gone` shows the lane runs on this class.
    assert_eq!(messages, ["'gone' is not defined in Plain"]);
}

/// An anonymous class is a class of its own, named by where it is written:
/// its members, its parents and `$this` are its, and its construction
/// dispatches on it.
#[cfg(feature = "php")]
#[test]
fn an_anonymous_class_is_a_class_of_its_own() {
    let fa = php(NEW);
    let anon = "App\\class@anonymous:20:18";
    let sym = fa.symbols().iter().find(|s| s.name == anon).expect("the anonymous class");
    assert_eq!((sym.kind, sym.selection_span.start.row, sym.selection_span.start.column), (SymKind::Class, 19, 17));
    assert!(sym.attributes.iter().any(|a| a == "anonymous"));
    assert_eq!(fa.declared_parents(anon), ["App\\Base"]);
    let hi = fa.symbols().iter().find(|s| s.name == "hi").unwrap();
    assert_eq!(hi.package.as_deref(), Some(anon));
    let ws = Workspace::new(&[("/fake/php/App/Box.php", NEW)]);
    assert_eq!(ws.defs("Box.php", "hi();", 0), vec![site("Box.php", 20, 28)]);
    // two anonymous classes are two classes
    let two = php("<?php\n$a = new class {};\n$b = new class {};\n");
    let classes: Vec<String> =
        two.symbols().iter().filter(|s| s.kind == SymKind::Class).map(|s| s.name.clone()).collect();
    assert_eq!(classes, ["class@anonymous:2:10", "class@anonymous:3:10"]);
}

/// A bare call in a method calls a function, never a method of the class:
/// PHP calls a method only through a receiver (`$this->foo()`,
/// `self::foo()`), so the class body's names are not in force in its
/// methods — a named class's or an anonymous one's. An anonymous class's
/// constructor arguments are code of the scope it is written in.
#[cfg(feature = "php")]
#[test]
fn a_bare_call_in_a_method_never_names_a_method() {
    use crate::model::file_analysis::RefKind;
    let src = "<?php
class A {
    public function foo() {}
    public function g() { foo(); $this->foo(); }
}
function make($a) {
    return new class($a) {
        public function bar() {}
        public function h() { bar(); }
    };
}
";
    let fa = php(src);
    let locals: Vec<(String, bool)> = call_invokes(&fa).into_iter().map(|(n, _, local, _)| (n, local)).collect();
    assert_eq!(locals, [("foo".to_string(), false), ("bar".to_string(), false)]);
    let ws = Workspace::new(&[("/fake/php/A.php", src)]);
    assert_eq!(ws.defs("A.php", "foo(); $this", 0), Sites::new(), "no function `foo`");
    assert_eq!(ws.defs("A.php", "foo(); }", 0), vec![site("A.php", 2, 20)], "the receiver's method");
    assert_eq!(ws.defs("A.php", "bar(); }", 0), Sites::new(), "no function `bar`");
    let seen = |needle: &str| -> Vec<String> {
        let scope = fa.scope_at(at(src, needle, 0)).unwrap();
        fa.bindings_seen_from(scope).iter().map(|s| s.name.clone()).collect()
    };
    assert!(!seen("foo(); $this").iter().any(|n| n == "foo" || n == "g"), "{:?}", seen("foo(); $this"));
    assert!(!seen("bar(); }").iter().any(|n| n == "bar" || n == "h"), "{:?}", seen("bar(); }"));
    let a = fa.symbols().iter().find(|s| s.kind == SymKind::Variable && s.name == "a").unwrap();
    let arg = fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::Variable) && r.target_name == "a" && r.span.start.row == 6)
        .unwrap();
    assert_eq!(arg.resolved_symbol(), Some(a.id), "the argument reads the function's parameter");
}

/// An anonymous class has no spelling another file could write, so it is
/// not part of the file's cross-file surface: two files with one at the
/// same place keep two classes.
#[cfg(feature = "php")]
#[test]
fn an_anonymous_class_is_not_linkage_visible() {
    let fa = php(NEW);
    let anon = fa.symbols().iter().find(|s| s.name == "App\\class@anonymous:20:18").unwrap();
    assert!(!fa.is_linkage_visible(anon));
    let base = fa.symbols().iter().find(|s| s.name == "App\\Base").unwrap();
    assert!(fa.is_linkage_visible(base));
    let ws = Workspace::new(&[("/fake/php/App/Box.php", NEW), ("/fake/php/App/Crate.php", NEW)]);
    assert_eq!(ws.defs("Box.php", "hi();", 0), vec![site("Box.php", 20, 28)]);
    assert_eq!(ws.defs("Crate.php", "hi();", 0), vec![site("Crate.php", 20, 28)]);
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
            " arguments: (arguments) @arity.args) @expr.call\n",
            "(arguments (_) @arity.arg) @arity.args\n",
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
    use crate::model::file_analysis::{CrossFileLookup, ScopedLookup};
    let fa = idx.workspace_file_for_test(std::path::Path::new(path)).unwrap();
    let base: &dyn CrossFileLookup = idx;
    let vis = LanguageRegistry::pack_visibility(&fa.language);
    let scoped = ScopedLookup::for_origin(base, &fa, None, vis);
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

const PHP_LIB_FNS: (&str, &str) =
    ("/fake/php/Lib/lib.php", "<?php\nnamespace Lib;\nfunction helper() { return 1; }\nfunction only() {}\n");
const PHP_OTHER_FNS: (&str, &str) = ("/fake/php/Other/other.php", "<?php\nnamespace Other;\nfunction helper() { return 'x'; }\n");

/// A function is its namespace-qualified identity across files, as a class
/// is: a `use function` row binds its leaf to the function it names, a
/// qualified call names one function, and a same-leaf function of another
/// namespace — even one that sorts first — never answers for it. A call
/// whose identity nothing indexed declares widens to its leaf out loud.
#[cfg(feature = "php")]
#[test]
fn a_function_is_its_identity_across_files() {
    use tree_sitter::Point;
    let a = (
        "/fake/php/App/a.php",
        "<?php\nnamespace App;\nuse function Other\\helper;\nfunction caller() {\n    helper();\n    \\Lib\\helper();\n    only();\n}\n",
    );
    let idx = php_indexed(&[PHP_LIB_FNS, PHP_OTHER_FNS, a]);
    let lands = |at: Point| php_landing(&idx, a.0, at);
    let other = (vec![("Other/other.php".to_string(), 2, None)], Some("Other/other.php".to_string()));
    assert_eq!(lands(Point::new(4, 4)), other, "a bare call through its `use function` row");
    assert_eq!(lands(Point::new(2, 19)), other, "the row's own leaf");
    assert_eq!(lands(Point::new(5, 9)).0, [("Lib/lib.php".to_string(), 2, None)], "a qualified call");
    assert_eq!(
        lands(Point::new(6, 4)).0,
        [("Lib/lib.php".to_string(), 3, Some("widened: `Lib\\only` stands in for `App\\only`".to_string()))]
    );
    let msg = "nothing indexed declares `App\\only`; resolved by its leaf to `Lib\\only`".to_string();
    assert_eq!(php_widening(&idx, a.0), [(6, 4, msg)]);

    // rename and references keep to the function: the row's leaf and the
    // bare call go with `Other\helper`, the qualified call with `Lib\helper`
    let ws = Workspace::new(&[PHP_LIB_FNS, PHP_OTHER_FNS, a]);
    assert_eq!(
        ws.renames("other.php", "helper", 0),
        [site("a.php", 2, 19), site("a.php", 4, 4), site("other.php", 2, 9)]
    );
    assert_eq!(ws.renames("lib.php", "helper", 0), [site("a.php", 5, 9), site("lib.php", 2, 9)]);
}

/// An imported function's alias is its own spelling: a rename of the
/// function rewrites the row's leaf and leaves the alias and the calls
/// spelled with it alone, and a call through the alias still lands.
#[cfg(feature = "php")]
#[test]
fn an_aliased_function_import_keeps_its_alias_on_rename() {
    let a = ("/fake/php/App/a.php", "<?php\nnamespace App;\nuse function Lib\\helper as h;\nh();\n");
    let idx = php_indexed(&[PHP_LIB_FNS, PHP_OTHER_FNS, a]);
    assert_eq!(php_landing(&idx, a.0, tree_sitter::Point::new(3, 0)).0, [("Lib/lib.php".to_string(), 2, None)]);
    let ws = Workspace::new(&[PHP_LIB_FNS, PHP_OTHER_FNS, a]);
    assert_eq!(ws.renames("lib.php", "helper", 0), [site("a.php", 2, 17), site("lib.php", 2, 9)]);
}

/// A bare call looks up the namespace in force's function first and the
/// global one only when the namespace declares none — PHP's own order,
/// across files.
#[cfg(feature = "php")]
#[test]
fn a_bare_call_tries_its_namespace_then_the_global_function() {
    use tree_sitter::Point;
    let global = ("/fake/php/0/global.php", "<?php\nfunction twice() {}\nfunction gone() {}\n");
    let own = ("/fake/php/App/own.php", "<?php\nnamespace App;\nfunction twice() {}\n");
    let b = ("/fake/php/App/b.php", "<?php\nnamespace App;\ntwice();\ngone();\n");
    let idx = php_indexed(&[global, own, b]);
    assert_eq!(php_landing(&idx, b.0, Point::new(2, 0)).0, [("App/own.php".to_string(), 2, None)]);
    assert_eq!(php_landing(&idx, b.0, Point::new(3, 0)).0, [("0/global.php".to_string(), 2, None)]);
    assert!(php_widening(&idx, b.0).is_empty());
}

/// `$this` is a reference to the object its method runs on: hover names
/// the enclosing class and goto-def lands on it, in a closure of the
/// method too, and it is no name a rename may rewrite.
#[cfg(feature = "php")]
#[test]
fn this_hovers_as_its_enclosing_class() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let src = "<?php\nnamespace App;\nclass Box {\n    public function me() { return $this; }\n    public function later() { return fn() => $this; }\n}\n";
    let path = "/fake/php/App/Box.php";
    let idx = php_indexed(&[(path, src)]);
    let fa = idx.workspace_file_for_test(std::path::Path::new(path)).unwrap();
    let docs = OpenDocs::new();
    for needle in ["this; }\n    public", "this; }\n}"] {
        let point = at(src, needle, 0);
        let key = FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(std::path::PathBuf::from(path)));
        let cs = resolve(&docs, &fa, key, point, Some(&idx), OverrideScope::default()).with_source(src);
        let hover = crate::lsp::symbols::pack_hover_markdown(&cs, "php").unwrap_or_default();
        assert!(hover.contains("class Box"), "{needle}: {hover}");
        assert_eq!(php_landing(&idx, path, point).0, [("App/Box.php".to_string(), 2, None)], "{needle}");
        assert_eq!(cs.renameable(), Ok(false), "{needle}");
    }
}

/// C++'s `this` is the same reference: hover names the enclosing class and
/// goto-def lands on it, in an out-of-line body (whose class is its
/// qualifier's) and in a lambda that captures it, and no rename rewrites
/// it.
#[cfg(feature = "cpp")]
#[test]
fn cpp_this_hovers_as_its_enclosing_class() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let src = "class W {\n  int n;\n  int get() { return this->n; }\n  void later() { auto f = [this] { return this->n; }; }\n  void set(int v);\n};\nvoid W::set(int v) { this->n = v; }\n";
    let path = std::path::PathBuf::from("/fake/cpp/w.cpp");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let driver = crate::build::language_driver::LanguageRegistry::compiled();
    idx.insert_workspace_for_test(path.clone(), driver.for_path(&path).unwrap().analyze_with_path(src, Some(&path)));
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let docs = OpenDocs::new();
    for needle in ["this->n; }\n  void later", "this->n; }; }", "this->n = v"] {
        let point = at(src, needle, 0);
        let key = FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone()));
        let cs = resolve(&docs, &fa, key, point, Some(&idx), OverrideScope::default()).with_source(src);
        let hover = crate::lsp::symbols::pack_hover_markdown(&cs, "cpp").unwrap_or_default();
        assert!(hover.contains("class W"), "{needle}: {hover}");
        let defs: Vec<usize> = cs.definitions().iter().map(|d| d.span.start.row).collect();
        assert_eq!(defs, [0], "{needle}");
        assert_eq!(cs.renameable(), Ok(false), "{needle}");
    }
}

/// A `static` member function runs on no object. Every site that refers
/// to one comes up empty in its body: `this` has no ref, no value and no
/// member place, and a bare instance member is no `this->n`. What names the
/// class still answers there: a bare static member, a bare static sibling.
#[cfg(feature = "cpp")]
#[test]
fn cpp_a_static_member_function_runs_on_no_object() {
    use crate::model::file_analysis::{Binding, InferredType, Span};
    let src = "class W {\n  int n;\n  static int c;\n  int get() { return this->n; }\n  static int helper() { return 1; }\n  static int make() { int a = std::move(this->n); int b = std::move(n); return a + b + c + helper(); }\n};\n";
    let path = std::path::PathBuf::from("/fake/cpp/w.cpp");
    let fa = crate::build::language_driver::LanguageRegistry::compiled().for_path(&path).unwrap().analyze_with_path(src, Some(&path));
    let sym = |name: &str| fa.symbols().iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name}"));
    let this_at = |needle| {
        let start = at(src, needle, 0);
        Span { start, end: tree_sitter::Point { row: start.row, column: start.column + 4 } }
    };
    // the object a method runs on, for contrast
    let get = this_at("this->n; }\n  static int helper");
    assert!(fa.ref_at(get.start).is_some());
    assert_eq!(fa.expr_type_at_span(get, None), Some(InferredType::ClassName("W".into())));
    let make = this_at("this->n); int b");
    assert!(fa.ref_at(make.start).is_none(), "{:?}", fa.ref_at(make.start));
    assert_eq!(fa.expr_type_at_span(make, None), None, "`this` has no value");
    let on_make: Vec<_> =
        fa.pack.moved_from.iter().filter(|m| m.place.root == Binding::Receiver(sym("make").id)).map(|m| &m.place).collect();
    assert!(on_make.is_empty(), "no member place of `this`, spelled or elided: {on_make:?}");
    let bound = |needle| fa.ref_at(at(src, needle, 0)).and_then(|r| r.resolved_symbol());
    assert_eq!(bound("n); return"), None, "a bare instance member names nothing");
    assert_eq!(bound("c + helper"), Some(sym("c").id), "a bare static member is the class's");
    assert!(sym("c").flags.contains(crate::model::file_analysis::SymbolFlags::STATIC));
    let helper = fa.ref_at(at(src, "helper(); }", 0)).expect("the sibling call");
    assert!(
        matches!(&helper.binding, Some(crate::model::file_analysis::RefBinding::Function { package }) if package == "W"),
        "a bare static sibling is the class's: {:?}",
        helper.binding
    );
}

/// php's `$this` in a `static function` follows the same rule: no ref, no
/// value, no member place; `self::` still names the class.
#[cfg(feature = "php")]
#[test]
fn php_a_static_method_runs_on_no_object() {
    use crate::model::file_analysis::{Binding, InferredType, PathStep, Place};
    let src = "<?php
class W {
    private ?array $items;
    private static ?W $inst = null;
    public function get() { $mine = $this; return $mine; }
    public static function make() {
        $x = $this;
        if (!isset($this->items)) { $first = 1; }
        if (self::$inst === null) { $second = 1; }
        return $x;
    }
}
";
    let fa = php(src);
    let make = fa.symbols().iter().find(|s| s.name == "make" && s.kind == SymKind::Method).unwrap().id;
    assert!(fa.ref_at(at(src, "$this;\n        if", 1)).is_none());
    let var = |name, needle| fa.inferred_type_via_bag(name, at(src, needle, 0));
    assert!(var("mine", "return $mine").is_some(), "the object a method runs on, for contrast");
    assert_eq!(var("x", "return $x"), None, "`$this` has no value");
    let member = |name: &str| Place { root: Binding::Receiver(make), path: vec![PathStep::Member(name.into())] };
    assert_eq!(fa.place_type_at(&member("items"), crate::model::witnesses::ReadPoint::position(at(src, "$first", 0)), None), None, "`$this->items` has no place");
    let self_at = at(src, "self::$inst", 0);
    let self_span = crate::model::file_analysis::Span { start: self_at, end: tree_sitter::Point { row: self_at.row, column: self_at.column + 4 } };
    assert_eq!(fa.expr_type_at_span(self_span, None), Some(InferredType::ClassName("W".into())), "`self` names the class");
}

/// A declaration's hover shows the value its own binding holds: an
/// enumerator its enum, a data member its declared type.
#[cfg(feature = "cpp")]
#[test]
fn cpp_declaration_hovers_show_their_own_binding_type() {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let src = "enum Color { RED, GREEN };\ntypedef enum { ON, OFF } Mode;\nstruct Box {\n  Color tint;\n};\n";
    let path = std::path::PathBuf::from("/fake/cpp/decls.cpp");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let driver = crate::build::language_driver::LanguageRegistry::compiled();
    idx.insert_workspace_for_test(path.clone(), driver.for_path(&path).unwrap().analyze_with_path(src, Some(&path)));
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let docs = OpenDocs::new();
    for (needle, want, kind) in [("RED", "RED: Color", "*enumerator*"), ("OFF", "OFF: Mode", "*enumerator*"), ("tint", "tint: Color", "*field*")] {
        let key = FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone()));
        let cs = resolve(&docs, &fa, key, at(src, needle, 0), Some(&idx), OverrideScope::default()).with_source(src);
        let hover = crate::lsp::symbols::pack_hover_markdown(&cs, "cpp").unwrap_or_default();
        assert!(hover.contains(want) && hover.contains(kind), "{needle}: {hover}");
    }
}

/// (namespace, start row, end row) of every lane section, in write order.
fn lane(fa: &FileAnalysis) -> Vec<(&str, usize, usize)> {
    fa.namespace_ranges.iter().map(|r| (r.namespace.as_str(), r.span.start.row, r.span.end.row)).collect()
}

#[cfg(feature = "php")]
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

#[cfg(feature = "php")]
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
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(p.clone())), call, Some(&idx), OverrideScope::default());
    let edits: Vec<std::path::PathBuf> = cs
        .rename_edits("renamed")
        .unwrap()
        .into_iter()
        .map(|(l, _)| match l.key {
            FileKey::Path(p) => p.into_path_buf(),
            FileKey::Url(_, p) => p.into_path_buf(),
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
#[cfg(feature = "php")]
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
/// named lookup. A forwarding call (`self::m()`) is the one that names its
/// lookup's class, the class its token names, since its receiver is the
/// method's own; the token's value is still that class.
#[cfg(feature = "php")]
#[test]
fn a_class_receiver_dispatches_on_its_value() {
    use crate::model::witnesses::{Callee, Lookup, WitnessAttachment as WA, WitnessPayload as WP};
    let fa = php("<?php\nclass A {\n  static function m() { return 1; }\n  function f() { return self::m(); }\n  function g() { return A::m(); }\n}\n");
    let calls = |call: Span| -> Vec<_> {
        fa.witnesses
            .for_attachment(&WA::Expr(call))
            .into_iter()
            .filter_map(|w| match &w.payload {
                WP::Invoke { callee: Callee::Method { lookup, .. }, receiver, .. } => Some((lookup.clone(), receiver.clone())),
                _ => None,
            })
            .collect()
    };
    let named = calls(Span { start: Point::new(4, 24), end: Point::new(4, 30) });
    assert_eq!(named, [(Lookup::Receiver, Some(WA::Expr(Span { start: Point::new(4, 24), end: Point::new(4, 25) })))]);
    let forwarded = calls(Span { start: Point::new(3, 24), end: Point::new(3, 33) });
    assert!(
        matches!(forwarded.as_slice(), [(Lookup::Named(c), Some(WA::Place(p)))] if c == "A" && p.is_variable()),
        "{forwarded:?}"
    );
    let recv = Span { start: Point::new(3, 24), end: Point::new(3, 28) };
    assert_eq!(fa.expr_type_at_span(recv, None), Some(InferredType::ClassName("A".into())));
}

#[cfg(feature = "php")]
#[test]
fn this_is_the_receiver_of_its_method() {
    let fa = php("<?php\nclass A {\n  function f() { return $this; }\n}\nfunction g() { return $this; }\n");
    let at = |row, col| fa.expr_type_at_span(Span { start: Point::new(row, col), end: Point::new(row, col + 5) }, None);
    assert_eq!(at(2, 24), Some(InferredType::ClassName("A".into())));
    assert_eq!(at(4, 22), None);
}

const READONLY: &str = "<?php
namespace App;

class Point {
    public readonly int $x;
    public int $plain;
    public function __construct(public readonly int $y) {
        $this->x = 1;
        $f = function () { $this->x = 2; };
    }
    public function copy(Point $other) {
        $other->x = 3;
        $f = function () { $this->x = 4; };
        $this->plain = 5;
    }
}

class Point3 extends Point {
    public function __construct() { $this->x = 6; }
    public function init() { $this->x = 7; }
}

readonly class Pair {
    public int $a;
    public function __construct(public int $b) {}
}

function outside(Point $p, Pair $q, $unknown) {
    $p->x = 8;
    $p->y += 1;
    $p->x++;
    $p->plain = 9;
    $unknown->x = 10;
    $q->a = 11;
    unset($q->b);
}

class Lazy {
    public readonly array $cache;
    public function get() { return $this->cache ??= []; }
}
function prime(Lazy $l) { $l->cache ??= []; }
";

/// `readonly` on a property or a promoted parameter, and every property a
/// `readonly class` declares, is the member's write-once flag.
#[cfg(feature = "php")]
#[test]
fn readonly_properties_carry_the_flag() {
    use crate::model::file_analysis::SymbolFlags as F;
    let fa = php(READONLY);
    let flags = |class: &str, name: &str| {
        fa.symbols()
            .iter()
            .find(|s| s.kind == SymKind::Field && s.name == name && s.package.as_deref() == Some(class))
            .unwrap()
            .flags
    };
    assert_eq!(flags("App\\Point", "x"), F::READONLY);
    assert_eq!(flags("App\\Point", "y"), F::READONLY);
    assert_eq!(flags("App\\Point", "plain"), F::empty());
    assert_eq!(flags("App\\Pair", "a"), F::READONLY);
    assert_eq!(flags("App\\Pair", "b"), F::READONLY);
}

/// A readonly member written from outside its class family fails; inside
/// it, only a constructor (its own closures and a subclass's included) or a
/// `??=` lazy init sets it in good form. A receiver nothing types, or a
/// writable member, is no finding.
#[cfg(feature = "php")]
#[test]
fn readonly_writes_outside_the_class_or_its_constructors_are_reported() {
    use crate::model::file_analysis::MisplacedWriteKind::{OutsideConstructor as InClass, OutsideFamily as Outside};
    let fa = php(READONLY);
    let found: Vec<_> = fa
        .misplaced_member_writes(None)
        .into_iter()
        .map(|w| (w.span.start.row, w.member, w.classes, w.kind))
        .collect();
    let at = |row: usize, member: &str, class: &str, kind| (row, member.to_string(), vec![class.to_string()], kind);
    assert_eq!(
        found,
        [
            at(11, "x", "App\\Point", InClass),
            at(12, "x", "App\\Point", InClass),
            at(19, "x", "App\\Point", InClass),
            at(28, "x", "App\\Point", Outside),
            at(29, "y", "App\\Point", Outside),
            at(30, "x", "App\\Point", Outside),
            at(33, "a", "App\\Pair", Outside),
            at(34, "b", "App\\Pair", Outside),
            at(41, "cache", "App\\Lazy", Outside),
        ]
    );
}

/// The lane answers through the index: a write in one file to a readonly
/// member another file declares names the member and its class, a warning
/// from outside the class and a hint from a subclass method.
#[cfg(feature = "php")]
#[test]
fn a_readonly_write_across_files_is_reported() {
    use crate::lsp::symbols::DiagnosticOptions;
    let point = ("/fake/php/App/Point.php", "<?php\nnamespace App;\nclass Point { public readonly int $x; }\n");
    let user = (
        "/fake/php/App/Use.php",
        "<?php\nnamespace App;\nclass Other { function f(Point $p) { $p->x = 1; } }\nclass Sub extends Point { function g() { $this->x = 2; } }\n",
    );
    let idx = php_indexed(&[point, user]);
    let fa = idx.workspace_file_for_test(std::path::Path::new(user.0)).unwrap();
    let diags: Vec<_> = crate::lsp::symbols::pack_diagnostics(&fa, &idx, DiagnosticOptions::default())
        .into_iter()
        .filter(|d| d.code == Some(tower_lsp::lsp_types::NumberOrString::String("readonly-write".into())))
        .map(|d| (d.range.start.line, d.severity, d.message))
        .collect();
    use tower_lsp::lsp_types::DiagnosticSeverity as S;
    assert_eq!(
        diags,
        [
            (
                2,
                Some(S::WARNING),
"`x` of App\\Point cannot be set from outside App\\Point or a subclass".to_string()
            ),
            (3, Some(S::HINT), "readonly property `x` of App\\Point is set outside the constructor".to_string()),
        ]
    );
}

const SET_SCOPES: &str = "<?php
namespace App;

class Point {
    public public(set) readonly int $open;
    public private(set) readonly int $own;
    public protected(set) int $kin;
    public private(set) int $mine;
    public function __construct(private(set) int $p) {
        $this->own = 1;
    }
    public function move() {
        $this->mine = 2;
        $this->own = 3;
    }
}

class Point3 extends Point {
    public function __construct() {
        $this->own = 4;
        $this->kin = 5;
        $this->mine = 6;
        $this->p = 7;
    }
}

function outside(Point $q) {
    $q->open = 8;
    $q->kin = 9;
    $q->mine = 10;
    $q->p = 11;
}
";

/// A `(set)` visibility is the member's set scope, on a property and on a
/// promoted parameter alike; a readonly member stating none keeps its
/// class family, and get visibility alone states no set scope.
#[cfg(feature = "php")]
#[test]
fn set_visibility_is_the_members_set_scope() {
    use crate::model::file_analysis::{AccessScope, SymbolFlags as F};
    let fa = php(SET_SCOPES);
    let flags = |name: &str| {
        fa.symbols()
            .iter()
            .find(|s| s.kind == SymKind::Field && s.name == name && s.package.as_deref() == Some("App\\Point"))
            .unwrap()
            .flags
    };
    assert_eq!(flags("open"), F::READONLY | F::SET_PUBLIC);
    assert_eq!(flags("own"), F::READONLY | F::SET_PRIVATE);
    assert_eq!(flags("kin"), F::SET_PROTECTED);
    assert_eq!(flags("mine"), F::SET_PRIVATE);
    assert_eq!(flags("p"), F::SET_PRIVATE);
    let scope = |name: &str| flags(name).set_scope();
    assert_eq!(
        ["open", "own", "kin", "mine", "p"].map(scope),
        [AccessScope::Anywhere, AccessScope::Class, AccessScope::Family, AccessScope::Class, AccessScope::Class]
    );
    assert_eq!(php(READONLY).symbols().iter().find(|s| s.name == "x").unwrap().flags.set_scope(), AccessScope::Family);
    assert_eq!(php(READONLY).symbols().iter().find(|s| s.name == "plain").unwrap().flags.set_scope(), AccessScope::Anywhere);
}

/// A write outside the member's set scope fails, whether or not the member
/// is write-once: `private(set)` shuts out a subclass, `protected(set)`
/// admits it, `public(set)` admits anyone. Only a write-once member has a
/// constructor to be set in.
#[cfg(feature = "php")]
#[test]
fn writes_outside_the_set_scope_are_reported() {
    use crate::model::file_analysis::MisplacedWriteKind::{OutsideClass, OutsideConstructor, OutsideFamily};
    let fa = php(SET_SCOPES);
    let found: Vec<_> = fa
        .misplaced_member_writes(None)
        .into_iter()
        .map(|w| (w.span.start.row, w.member, w.kind))
        .collect();
    let at = |row: usize, member: &str, kind| (row, member.to_string(), kind);
    assert_eq!(
        found,
        [
            at(13, "own", OutsideConstructor),
            at(19, "own", OutsideClass),
            at(21, "mine", OutsideClass),
            at(22, "p", OutsideClass),
            at(28, "kin", OutsideFamily),
            at(29, "mine", OutsideClass),
            at(30, "p", OutsideClass),
        ]
    );
}

/// The warning names the set scope it is outside: the class alone, or the
/// class and its subclasses.
#[cfg(feature = "php")]
#[test]
fn a_set_scope_warning_names_its_scope() {
    use crate::lsp::symbols::DiagnosticOptions;
    let point = (
        "/fake/php/App/Point.php",
        "<?php\nnamespace App;\nclass Point { public private(set) int $x; public protected(set) int $y; }\n",
    );
    let user = ("/fake/php/App/Use.php", "<?php\nnamespace App;\nfunction f(Point $p) { $p->x = 1; $p->y = 2; }\n");
    let idx = php_indexed(&[point, user]);
    let fa = idx.workspace_file_for_test(std::path::Path::new(user.0)).unwrap();
    let messages: Vec<_> = crate::lsp::symbols::pack_diagnostics(&fa, &idx, DiagnosticOptions::default())
        .into_iter()
        .filter(|d| d.code == Some(tower_lsp::lsp_types::NumberOrString::String("readonly-write".into())))
        .map(|d| d.message)
        .collect();
    assert_eq!(
        messages,
        [
            "`x` of App\\Point cannot be set from outside App\\Point",
            "`y` of App\\Point cannot be set from outside App\\Point or a subclass",
        ]
    );
}

/// A trait's `private(set)` member is the composing class's own, so the
/// composer's methods may set it.
#[cfg(feature = "php")]
#[test]
fn a_traits_private_set_member_is_its_composers() {
    let fa = php("<?php
trait HasId { public private(set) int $id; }
class User {
    use HasId;
    public function assign() { $this->id = 1; }
}
function f(User $u) { $u->id = 2; }
");
    let found: Vec<_> = fa.misplaced_member_writes(None).into_iter().map(|w| w.span.start.row).collect();
    assert_eq!(found, [6]);
}

/// PHP's protected reaches up the lineage as well as down: a parent's
/// method sets a `protected(set)` member its child declares, and a write-once
/// one in good form only from a constructor. A sibling class shares neither
/// lineage, and `private(set)` stays its declaring class's alone.
#[cfg(feature = "php")]
#[test]
fn a_parent_may_set_its_childs_protected_set_member() {
    use crate::model::file_analysis::MisplacedWriteKind::{OutsideClass, OutsideConstructor, OutsideFamily};
    let fa = php("<?php
namespace App;
class Base {
    public function __construct(Kid $k) { $k->once = 1; }
    public function reset(Kid $k) { $k->kin = 1; $k->own = 2; $k->once = 3; }
}
class Kid extends Base {
    public protected(set) int $kin;
    public private(set) int $own;
    public readonly int $once;
}
class Sibling extends Base {
    public function poke(Kid $k) { $k->kin = 4; }
}
function outside(Kid $k) { $k->kin = 5; }
");
    let found: Vec<_> = fa
        .misplaced_member_writes(None)
        .into_iter()
        .map(|w| (w.span.start.row, w.member, w.kind))
        .collect();
    let at = |row: usize, member: &str, kind| (row, member.to_string(), kind);
    assert_eq!(
        found,
        [
            at(4, "own", OutsideClass),
            at(4, "once", OutsideConstructor),
            at(12, "kin", OutsideFamily),
            at(14, "kin", OutsideFamily),
        ]
    );
}

/// Each method-call receiver `m` is read on, by its row, as the class it
/// dispatches on.
#[cfg(feature = "php")]
fn dispatch_rows(fa: &FileAnalysis, method: &str) -> Vec<(usize, Option<String>)> {
    let mut out: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == method && matches!(r.kind, crate::model::file_analysis::RefKind::MethodCall { .. }))
        .map(|r| (r.span.start.row, fa.method_call_invocant_type(r, None).and_then(|t| t.class_name().map(str::to_string))))
        .collect();
    out.sort();
    out
}

/// `instanceof` proves its subject is the class named, through the
/// use-map, where it holds: in the arm it enters, and after an `if` that
/// leaves the block when it fails. A failed `instanceof` proves nothing.
#[cfg(feature = "php")]
#[test]
fn instanceof_narrows_where_it_holds() {
    let src = "<?php
namespace App;
use Lib\\Foo;
function f($x, $y) {
    if ($x instanceof Foo) {
        $x->run();
    } else {
        $x->other();
    }
    $x->tail();
    if (!$y instanceof Foo) {
        return;
    }
    $y->run();
}
";
    let fa = php(src);
    let foo = Some(InferredType::ClassName("Lib\\Foo".into()));
    assert_eq!(fa.inferred_type_via_bag("x", at(src, "$x->run", 0)), foo, "the arm it enters");
    assert_eq!(fa.inferred_type_via_bag("x", at(src, "$x->other", 0)), None, "a failed test proves nothing");
    assert_eq!(fa.inferred_type_via_bag("x", at(src, "$x->tail", 0)), None, "nothing after the `if`");
    assert_eq!(fa.inferred_type_via_bag("y", at(src, "$y->run", 0)), foo, "the rest of the block after an exit");
    assert_eq!(fa.inferred_type_via_bag("y", at(src, "return", 0)), None, "not the exit's own arm");
}

/// An `if` with no later clause whose body leaves the block leaves the
/// rest of the block to run where its condition fails: `die` in either
/// spelling, `exit`, and an exit after work or a comment, or before more
/// work, and so does an `else` that falls through. A body that may fall
/// through, or a chain whose other clause falls through too, says nothing
/// past the `if`.
#[cfg(feature = "php")]
#[test]
fn an_if_whose_body_ends_in_an_exit_narrows_the_rest_of_the_block() {
    let foo = Some(InferredType::ClassName("Lib\\Foo".into()));
    let exits = [
        ("die(\"x\");", "die()"),
        ("{ die; }", "a bare die"),
        ("{ exit; }", "exit"),
        ("exit(1);", "exit()"),
        ("{\n        // why\n        return;\n    }", "a comment before the exit"),
        ("{ log_it(); return; }", "work, then the exit"),
        ("{ if ($z) { log_it(); } throw new \\Exception(); }", "a nested block, then the exit"),
        ("{ return; log_it(); }", "an exit that is not the last statement"),
        ("{ return; } else { log_it(); }", "an else clause that falls through"),
    ];
    for (body, why) in exits {
        let src = format!("<?php\nnamespace App;\nuse Lib\\Foo;\nfunction f($y, $z) {{\n    if (!$y instanceof Foo) {body}\n    $y->run();\n}}\n");
        let fa = php(&src);
        assert_eq!(fa.inferred_type_via_bag("y", at(&src, "$y->run", 0)), foo, "{why}");
    }
    let falls_through = [
        ("{ log_it(); }", "no exit"),
        ("{ if ($z) { return; } }", "an exit only in a nested block"),
        ("{ return; } elseif ($z) { log_it(); }", "an elseif clause"),
        ("{ diet(); }", "a call that is not die"),
    ];
    for (body, why) in falls_through {
        let src = format!("<?php\nnamespace App;\nuse Lib\\Foo;\nfunction f($y, $z) {{\n    if (!$y instanceof Foo) {body}\n    $y->run();\n}}\n");
        let fa = php(&src);
        assert_eq!(fa.inferred_type_via_bag("y", at(&src, "$y->run", 0)), None, "{why}");
    }
}

/// After a chain whose one arm falls through, the rest of the block runs
/// where that arm does: the then-arm where its condition holds, a later
/// clause where its own holds and every earlier one failed, and a missing
/// `else` where every condition failed. Every arm leaving, or two falling
/// through, narrows nothing after; a loop's body is left by `break` to what
/// follows the loop.
#[cfg(feature = "php")]
#[test]
fn the_one_arm_a_chain_falls_through_narrows_the_rest_of_the_block() {
    let foo = Some(InferredType::ClassName("Lib\\Foo".into()));
    let src_of = |chain: &str| {
        format!("<?php\nnamespace App;\nuse Lib\\Foo;\nfunction f($y, $z) {{\n    {chain}\n    $y->run();\n}}\n")
    };
    let narrows = [
        ("if (!$y instanceof Foo) { return; log_it(); }", "an exit not in last position"),
        ("if ($y instanceof Foo) { log_it(); } else { return; }", "an else-arm exit"),
        ("if ($y instanceof Foo) { log_it(); } else return;", "a braceless else-arm exit"),
        ("if (!$y instanceof Foo) { return; } elseif ($z) { die(); } else { log_it(); }", "only the else falls"),
        ("if ($z) { return; } elseif ($y instanceof Foo) { log_it(); } else { exit; }", "only the elseif falls"),
        ("if ($y instanceof Foo) { log_it(); } elseif ($z) { return; } else { throw new \\Exception(); }", "only the then falls"),
        ("if (!$y instanceof Foo) { return; } elseif ($z) { return; }", "a missing else, every condition failed"),
        ("if (!$y instanceof Foo) { if ($z) { return; } else { die; } }", "a nested chain whose every arm leaves"),
        ("if (!$y instanceof Foo) {\n        return; // why\n    }", "a comment after the exit"),
        ("if (!$y instanceof Foo):\n        return;\n    endif;", "a colon block"),
    ];
    for (chain, why) in narrows {
        let src = src_of(chain);
        assert_eq!(php(&src).inferred_type_via_bag("y", at(&src, "$y->run", 0)), foo, "{why}");
    }
    let nothing = [
        ("if ($y instanceof Foo) { return; } else { die; }", "every arm leaves"),
        ("if ($y instanceof Foo) { log_it(); } elseif ($z) { log_it(); } else { return; }", "two arms fall through"),
        ("while (!$y instanceof Foo) { break; }", "a loop left by break"),
        ("if (!$y instanceof Foo) while ($z) return;", "a loop's body is no statement of the arm"),
    ];
    for (chain, why) in nothing {
        let src = src_of(chain);
        assert_eq!(php(&src).inferred_type_via_bag("y", at(&src, "$y->run", 0)), None, "{why}");
    }
}

/// A property of `$this` is a place of the method's object: a guard on
/// it narrows the member's receiver for dispatch, in the arm and after an
/// early exit, until something may change it.
#[cfg(feature = "php")]
#[test]
fn instanceof_narrows_a_property_of_this_for_dispatch() {
    let src = "<?php
class Base { public function run() {} }
class Foo extends Base { public function run() {} }
class Holder {
    private Base $b;
    public function f() {
        if ($this->b instanceof Foo) {
            $this->b->run();
        }
        $this->b->run();
    }
    public function g() {
        if (!($this->b instanceof Foo)) throw new \\Exception();
        $this->b->run();
        $this->b = new Base();
        $this->b->run();
    }
}
";
    let fa = php(src);
    let class = |c: &str| Some(c.to_string());
    assert_eq!(
        dispatch_rows(&fa, "run"),
        [(7, class("Foo")), (9, class("Base")), (13, class("Foo")), (15, class("Base"))]
    );
}

/// `isset`, `!== null`, `=== null` and `is_null` decide whether the
/// subject holds a value: where it holds none, it is the null value.
#[cfg(feature = "php")]
#[test]
fn a_failed_isset_or_a_passing_null_test_is_the_null_value() {
    let src = "<?php
class Cache {
    private ?array $items;
    public function f($a, $b, $c, $d, $e) {
        if (!isset($this->items)) {
            $first = 1;
        }
        if (isset($a)) {
            $one = 1;
        } else {
            $two = 2;
        }
        if ($b === null) { $three = 3; }
        if (null !== $c) { $four = 4; } else { $five = 5; }
        $z = is_null($d) ? 6 : 7;
        if (!isset($e, $a)) { $eight = 8; }
    }
}
";
    let fa = php(src);
    let method = fa.symbols().iter().find(|s| s.name == "f" && s.kind == SymKind::Method).unwrap().id;
    let items = crate::model::file_analysis::Place {
        root: crate::model::file_analysis::Binding::Receiver(method),
        path: vec![crate::model::file_analysis::PathStep::Member("items".into())],
    };
    let undef = Some(InferredType::Undef);
    assert_eq!(fa.place_type_at(&items, crate::model::witnesses::ReadPoint::position(at(src, "$first", 0)), None), undef, "a property not set");
    let var = |name, needle| fa.inferred_type_via_bag(name, at(src, needle, 0));
    assert_eq!(var("a", "$one"), None, "a passing isset strips nothing yet");
    assert_eq!(var("a", "$two"), undef);
    assert_eq!(var("b", "$three"), undef);
    assert_eq!(var("c", "$four"), None);
    assert_eq!(var("c", "$five"), undef);
    assert_eq!(var("d", "6 :"), undef);
    assert_eq!(var("d", "7;"), None);
    assert_eq!(var("e", "$eight"), None, "a multi-argument isset is no atom");
}

/// A condition decides its atoms through its connectives: `&&` holding
/// and `||` failing fix every side, a negation flips it, and an atom that
/// is only an argument of the condition is none of it.
#[cfg(feature = "php")]
#[test]
fn connectives_decide_the_atoms_they_hold() {
    let src = "<?php
function f($a, $b) {
    if ($a instanceof Foo && $b instanceof Bar) { $both = 1; }
    if (!$a instanceof Foo || !($b instanceof Bar)) { $either = 1; } else { $neither = 1; }
    if ($a instanceof Foo || $b instanceof Bar) { $some = 1; }
    if (!($a === null)) { $set = 1; } elseif ($b) { $later = 1; } else { $last = 1; }
    if (check($a instanceof Foo)) { $arg = 1; }
    $a instanceof Foo and $a->run();
    $y = is_null($b) || $b->run();
}
";
    let fa = php(src);
    let var = |name, needle| fa.inferred_type_via_bag(name, at(src, needle, 0));
    let class = |c: &str| Some(InferredType::ClassName(c.into()));
    let undef = Some(InferredType::Undef);
    assert_eq!((var("a", "$both"), var("b", "$both")), (class("Foo"), class("Bar")));
    assert_eq!((var("a", "$either"), var("b", "$either")), (None, None));
    assert_eq!((var("a", "$neither"), var("b", "$neither")), (class("Foo"), class("Bar")));
    assert_eq!((var("a", "$some"), var("b", "$some")), (None, None));
    assert_eq!(var("a", "$set"), None);
    assert_eq!(var("a", "$later"), undef, "every later clause of the chain");
    assert_eq!(var("a", "$last"), undef);
    assert_eq!(var("a", "$arg"), None);
    assert_eq!(var("a", "$a->run"), class("Foo"), "the right of `and`");
    assert_eq!(var("b", "$b->run"), None, "the right of `||` holds the failed test: set");
}

/// A property of a local is a place of that local, as one of `$this` is
/// of the method's object: a guard on it narrows the member's receiver
/// for dispatch, in the arm and after an early exit, until a write of the
/// property or of the local, or the local handed on whole, may change it.
#[cfg(feature = "php")]
#[test]
fn instanceof_narrows_a_property_of_a_local_for_dispatch() {
    let src = "<?php
class Base { public function run() {} }
class Foo extends Base { public function run() {} }
class Holder { public Base $b; }
function f(Holder $h) {
    if ($h->b instanceof Foo) {
        $h->b->run();
    }
    $h->b->run();
    if (!($h->b instanceof Foo)) return;
    $h->b->run();
    $h = new Holder();
    $h->b->run();
}
function g(Holder $h) {
    if ($h->b instanceof Foo) {
        $h->b->run();
        $h->b = new Base();
        $h->b->run();
    }
    if ($h->b instanceof Foo) {
        touch($h);
        $h->b->run();
    }
}
";
    let fa = php(src);
    let class = |c: &str| Some(c.to_string());
    assert_eq!(
        dispatch_rows(&fa, "run"),
        [
            (6, class("Foo")),
            (8, class("Base")),
            (10, class("Foo")),
            (12, class("Base")),
            (16, class("Foo")),
            (18, class("Base")),
            (22, class("Base")),
        ]
    );
}

/// `isset` and the null tests decide whether a local's property holds a
/// value, as they do a property of `$this`.
#[cfg(feature = "php")]
#[test]
fn a_null_test_on_a_property_of_a_local_is_the_null_value() {
    let src = "<?php
class Cache { public ?array $items; }
function g(Cache $c) {
    if (!isset($c->items)) { $first = 1; }
    if ($c->items === null) { $second = 1; }
    if ($c->items !== null) { $third = 1; } else { $fourth = 1; }
    if (is_null($c->items)) { $fifth = 1; }
    $last = 1;
}
";
    let fa = php(src);
    let c = fa.symbols().iter().find(|s| s.name == "c" && s.kind == SymKind::Variable).unwrap().id;
    let items = crate::model::file_analysis::Place {
        root: crate::model::file_analysis::Binding::Lexical(c),
        path: vec![crate::model::file_analysis::PathStep::Member("items".into())],
    };
    let ty = |needle| fa.place_type_at(&items, crate::model::witnesses::ReadPoint::position(at(src, needle, 0)), None);
    let undef = Some(InferredType::Undef);
    assert_eq!(ty("$first"), undef, "a property not set");
    assert_eq!(ty("$second"), undef);
    assert_eq!(ty("$third"), None, "a passing test proves nothing of an untyped property");
    assert_eq!(ty("$fourth"), undef);
    assert_eq!(ty("$fifth"), undef);
    assert_eq!(ty("$last"), None, "nothing after the `if`");
}

/// A passing `isset`, `!== null` or `!is_null`, and a failed `=== null`,
/// leave the subject what it held coming in without its null: a nullable
/// local, parameter or property is its class there, a nullable union its
/// other arms. Outside the test, and on a subject that was never nullable,
/// nothing changes.
#[cfg(feature = "php")]
#[test]
fn a_passing_null_test_strips_the_null() {
    let src = "<?php
class Foo { public function run() {} }
class A { public function run() {} }
class B { public function run() {} }
function maybe(): ?Foo { return null; }
class Holder {
    public ?Foo $p;
    public function m(?Foo $w, Foo $plain, A|B|null $u) {
        $v = maybe();
        if (isset($w)) { $w->run(); }
        $w->run();
        if ($v !== null) { $v->run(); }
        if ($this->p === null) { $x = 1; } else { $this->p->run(); }
        if (!is_null($this->p)) { $this->p->run(); }
        $this->p->run();
        if (isset($plain)) { $plain->run(); }
        if ($u !== null) { $u->run(); }
        $u->run();
    }
}
function g(Holder $h) {
    if (isset($h->p)) { $h->p->run(); }
    $h->p->run();
}
";
    let fa = php(src);
    let mut rows: Vec<(usize, Option<InferredType>)> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "run" && matches!(r.kind, crate::model::file_analysis::RefKind::MethodCall { .. }))
        .map(|r| (r.span.start.row, fa.method_call_invocant_type(r, None)))
        .collect();
    rows.sort_by_key(|(row, _)| *row);
    let foo = || Some(InferredType::ClassName("Foo".into()));
    let nullable = |t: Option<InferredType>| t.map(|t| InferredType::Optional(Box::new(t)));
    let ab = || crate::model::file_analysis::join_all(
        &[InferredType::ClassName("A".into()), InferredType::ClassName("B".into())],
        &crate::model::file_analysis::LatticeRules::default(),
    );
    assert_eq!(
        rows,
        [
            (9, foo()),
            (10, nullable(foo())),
            (11, foo()),
            (12, foo()),
            (13, foo()),
            (14, nullable(foo())),
            (15, foo()),
            (16, ab()),
            (17, nullable(ab())),
            (21, foo()),
            (22, nullable(foo())),
        ]
    );
    // The variable itself, as hover reads it.
    assert_eq!(fa.inferred_type_via_bag("w", at(src, "$w->run(); }", 0)), foo());
    assert_eq!(fa.inferred_type_via_bag("w", at(src, "$w->run();\n", 0)), nullable(foo()));
}

/// `self`, `static` and `parent` on the right of `instanceof` name a class
/// by where the guard is written: the class around it, the class around it
/// again (the class the call is made on is that class or one beneath it),
/// and the class it extends. Each is an identity, never respelled through
/// the use-map. Outside a class they name none.
#[cfg(feature = "php")]
#[test]
fn instanceof_self_static_and_parent_narrow_to_the_class_they_name() {
    let src = "<?php
namespace App;
class Base { public function run() {} }
class Mid extends Base {
    private Base $b;
    public function f($x, $y, $z) {
        if ($x instanceof self) { $x->run(); }
        if ($y instanceof static) { $y->run(); }
        if ($z instanceof parent) { $z->run(); }
        if ($this->b instanceof self) { $this->b->run(); }
    }
}
function loose($w) {
    if ($w instanceof self) { $w->run(); }
}
";
    let fa = php(src);
    let var = |name, needle| fa.inferred_type_via_bag(name, at(src, needle, 0));
    let class = |c: &str| Some(InferredType::ClassName(c.into()));
    assert_eq!(var("x", "$x->run"), class("App\\Mid"));
    assert_eq!(var("y", "$y->run"), class("App\\Mid"));
    assert_eq!(var("z", "$z->run"), class("App\\Base"));
    assert_eq!(var("w", "$w->run"), None);
    let dispatch: Vec<_> = dispatch_rows(&fa, "run").into_iter().filter(|(row, _)| *row == 9).collect();
    assert_eq!(dispatch, [(9, Some("App\\Mid".to_string()))]);
}

/// A local's reference and its rename sit on its name, after the `$`,
/// however its properties are read and guarded.
#[cfg(feature = "php")]
#[test]
fn a_local_read_through_its_properties_is_referenced_by_its_name() {
    use crate::model::file_analysis::RefKind;
    let src = "<?php
function f(Holder $h) {
    if ($h->b instanceof Foo) { $h->b->run(); }
    $h = new Holder();
    return $h;
}
";
    let fa = php(src);
    let mut vars: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Variable))
        .map(|r| (r.span.start.row, r.span.start.column, r.span.end.column))
        .collect();
    vars.sort();
    assert_eq!(vars, [(1, 19, 20), (2, 9, 10), (2, 33, 34), (3, 5, 6), (4, 12, 13)]);
    let ws = Workspace::new(&[("locals.php", src)]);
    assert_eq!(
        ws.renames("locals.php", "$h)", 1),
        [
            site("locals.php", 1, 19),
            site("locals.php", 2, 9),
            site("locals.php", 2, 33),
            site("locals.php", 3, 5),
            site("locals.php", 4, 12),
        ]
    );
}

/// A write a guard proves lands where the member holds nothing is a lazy
/// init, as `??=` is: `!isset`, `=== null` and the rest of a block after
/// an `isset` exit all say so. The write itself ends what the guard
/// proved, so a second write in the same arm is no init. A write from
/// outside the class is outside its set scope whatever guards it.
#[cfg(feature = "php")]
#[test]
fn a_write_where_a_guard_proves_the_member_unset_is_a_lazy_init() {
    use crate::model::file_analysis::MisplacedWriteKind::{OutsideConstructor as InClass, OutsideFamily as Outside};
    let src = "<?php
class Lazy {
    public readonly array $a;
    public readonly ?array $b;
    public readonly array $c;
    public readonly array $d;
    public readonly array $e;
    public function get() {
        if (!isset($this->a)) { $this->a = []; }
        if ($this->b === null) {
            $this->b = [];
        }
        if (isset($this->c)) {
            return $this->c;
        }
        $this->c = [];
        if (!isset($this->d)) { $this->d = []; $this->d = [1]; }
        $this->e = [];
    }
}
function prime(Lazy $l) {
    if (!isset($l->a)) { $l->a = []; }
}
";
    let fa = php(src);
    let found: Vec<_> = fa
        .misplaced_member_writes(None)
        .into_iter()
        .map(|w| (w.span.start.row, w.member, w.kind))
        .collect();
    let at = |row: usize, member: &str, kind| (row, member.to_string(), kind);
    assert_eq!(found, [at(16, "d", InClass), at(17, "e", InClass), at(21, "a", Outside)]);
}

const SPACES: &str = "<?php
namespace App;

class Base {
    public static $shared = 1;
}

class Card extends Base {
    const X = 1;
    public $X = 2;
    public static $count = 0;
    public function X() {}
    public function run(Card $c) {
        $a = Card::X;
        $b = $c->X;
        $d = $c->X();
        $e = self::X;
        $f = Card::$count;
        $g = $c->count;
        $h = Card::$X;
        $i = static::$shared;
    }
}
";

/// `Card::X`, `$c->X` and `$c->X()` reach the constant, the property and
/// the method of one name, each in its own namespace.
#[cfg(feature = "php")]
#[test]
fn a_constant_property_and_method_of_one_name_are_three_members() {
    let ws = Workspace::new(&[("spaces.php", SPACES)]);
    assert_eq!(ws.defs("spaces.php", "Card::X;", 6), [site("spaces.php", 8, 10)]);
    assert_eq!(ws.defs("spaces.php", "self::X;", 6), [site("spaces.php", 8, 10)]);
    assert_eq!(ws.defs("spaces.php", "$c->X;", 4), [site("spaces.php", 9, 12)]);
    assert_eq!(ws.defs("spaces.php", "$c->X();", 4), [site("spaces.php", 11, 20)]);
}

/// A static property is read through `::`; `->` finds it only as a
/// fallback, and that landing is illegal. An instance property read
/// through `::` is the mirror case. A static one inherited from a parent
/// resolves through `static::`.
#[cfg(feature = "php")]
#[test]
fn a_static_property_is_reached_through_its_own_side_and_illegally_through_the_other() {
    use crate::model::file_analysis::{MemberKind, MethodResolution, RefKind};
    let ws = Workspace::new(&[("spaces.php", SPACES)]);
    assert_eq!(ws.defs("spaces.php", "Card::$count", 7), [site("spaces.php", 10, 19)]);
    assert_eq!(ws.defs("spaces.php", "static::$shared", 9), [site("spaces.php", 4, 19)]);
    assert_eq!(ws.defs("spaces.php", "$c->count", 4), [site("spaces.php", 10, 19)], "a fallback still resolves");
    assert_eq!(ws.defs("spaces.php", "Card::$X", 7), [site("spaces.php", 9, 12)], "a fallback still resolves");
    let fa = php(SPACES);
    let illegal: Vec<(usize, &str)> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::FieldAccess { .. }))
        .filter_map(|r| {
            let want = MemberKind::of_ref(&r.kind)?;
            let Some(MethodResolution::Local { sym_id, .. }) = fa.resolve_member_in_ancestors("App\\Card", &r.target_name, None, want) else {
                return None;
            };
            want.reaches_illegally(fa.symbol(sym_id), fa.member_spaces()).then_some((r.span.start.row, r.target_name.as_str()))
        })
        .collect();
    assert_eq!(illegal, [(18, "count"), (19, "X")]);
}

const MEMBER_VALUES: &str = "<?php
class Widget {}
class Gadget {}
class Gizmo {}

class Box {
    const X = 1;
    public Widget $X;
    public static Gadget $S;
    public function X(): Gizmo {}
    public function run(Box $o) {
        $a = Box::X;
        $b = $o->X;
        $c = $o->X();
        $d = Box::$S;
        $e = $o->S;
        return;
    }
}
";

/// A member's value is keyed by its identity, owner + namespace + name, not
/// its spelling: the property `X` holds a Widget, its method returns a
/// Gizmo, the constant `X` holds the number it is declared with, and the
/// static `$S` holds its
/// Gadget whether reached legally or through the instance side.
#[cfg(feature = "php")]
#[test]
fn a_members_value_is_its_own_namespaces() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(MEMBER_VALUES);
    let end = at(MEMBER_VALUES, "return;", 0);
    let ty = |v: &str| fa.inferred_type_via_bag(v, end);
    assert_eq!(ty("b"), Some(T::ClassName("Widget".into())), "the property");
    assert_eq!(ty("c"), Some(T::ClassName("Gizmo".into())), "the method's return");
    assert_eq!(ty("a"), Some(T::Numeric), "the constant holds what it is declared with");
    assert_eq!(ty("d"), Some(T::ClassName("Gadget".into())), "the static property");
    assert_eq!(ty("e"), Some(T::ClassName("Gadget".into())), "the static property, reached illegally");
}

const UNTYPED_LEGAL: &str = "<?php
class Gadget {}
class Base {
    public static Gadget $v;
}
class Card extends Base {
    public $v;
    public function run(Card $c) {
        $a = $c->v;
        return;
    }
}
";

/// The member a read reaches is the first namespace that HOLDS it, typed
/// or not: `$c->v` reaches the untyped property, so it reads `Unknown` and
/// the parent's static `$v` (an illegal reach from `->`) never answers in
/// its place.
#[cfg(feature = "php")]
#[test]
fn an_untyped_legal_member_is_still_the_one_a_read_reaches() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(UNTYPED_LEGAL);
    assert_eq!(fa.inferred_type_via_bag("a", at(UNTYPED_LEGAL, "return;", 0)), Some(T::Unknown));
}

const UNTYPED_REDECLARED: &str = "<?php
class Gadget {}
class Base {
    protected Gadget $aliasOf;
}
class Alias extends Base {
    protected $aliasOf;
    public function run() {
        $a = $this->aliasOf;
        return;
    }
}
";

/// A child that redeclares a member without a type still has the member, but
/// its type is the nearest typed declaration's up the MRO in the same
/// namespace: the parent's `Gadget`.
#[cfg(feature = "php")]
#[test]
fn an_untyped_redeclaration_takes_the_parents_type() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(UNTYPED_REDECLARED);
    assert_eq!(
        fa.inferred_type_via_bag("a", at(UNTYPED_REDECLARED, "return;", 0)),
        Some(T::ClassName("Gadget".into()))
    );
}

const LITERALS: &str = "<?php
class Conf {
    public $name = 'app';
    public $port = 8080;
    public function run() {
        $a = 1.5;
        $b = \"x{$a}\";
        $c = true;
        $d = $this->name;
        $e = $this->port;
        return;
    }
}
";

/// A literal is a value of its own kind, and an untyped property holds the
/// value it is initialized with.
#[cfg(feature = "php")]
#[test]
fn a_literal_types_and_an_initializer_flows_into_its_property() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(LITERALS);
    let end = at(LITERALS, "return;", 0);
    let ty = |v: &str| fa.inferred_type_via_bag(v, end);
    assert_eq!(ty("a"), Some(T::Numeric));
    assert_eq!(ty("b"), Some(T::String));
    assert_eq!(ty("c"), Some(T::Bool));
    assert_eq!(ty("d"), Some(T::String), "the initializer of an untyped property");
    assert_eq!(ty("e"), Some(T::Numeric));
}

const FLOWING_LITERALS: &str = "<?php
class K {
    const C = 11;
    public $p = 12;
    public function f($q = 13) {
        static $s = 14;
        $a = 15;
        $a += 16;
        g(17);
        $b = $a + 18;
        $c = -19;
        $d = [20 => 21];
        switch ($a) { case 22: break; }
        return 23;
    }
}
";

/// A literal states its kind only where its value flows on: a write, a
/// return, an argument, a declared initializer, an operand. One nothing
/// reads that way (an array key or element, a `case` label, a parameter's
/// default) states nothing.
#[cfg(feature = "php")]
#[test]
fn a_literal_states_its_kind_only_where_its_value_flows() {
    use crate::model::file_analysis::{InferredType as T, Span};
    let fa = php(FLOWING_LITERALS);
    let lit = |needle: &str| {
        let start = at(FLOWING_LITERALS, needle, 0);
        let end = tree_sitter::Point { column: start.column + needle.len(), ..start };
        fa.expr_type_at_span(Span { start, end }, None)
    };
    for flowing in ["11", "12", "14", "15", "16", "17", "18", "19", "23"] {
        assert_eq!(lit(flowing), Some(T::Numeric), "`{flowing}` flows on");
    }
    for idle in ["13", "20", "21", "22"] {
        assert_eq!(lit(idle), None, "`{idle}` flows nowhere");
    }
}

/// Each member call's name and the one class its receiver resolves to, in
/// source order.
#[cfg(feature = "php")]
fn call_receivers(fa: &FileAnalysis) -> Vec<(String, Option<String>)> {
    use crate::model::file_analysis::{ReceiverPolicy, RefKind};
    fa.refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { constructs: false, .. }))
        .map(|r| (r.target_name.clone(), fa.receiver_classes(r, None, ReceiverPolicy::Navigate).single().map(str::to_string)))
        .collect()
}

#[cfg(feature = "php")]
fn calls_on(pairs: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
    pairs.iter().map(|(m, c)| (m.to_string(), Some(c.to_string()))).collect()
}

const FLUENT_LSB: &str = "<?php
class Base {
    public static function make(): static { return new static(); }
    public function with(): static { return $this; }
}
class Kid extends Base {
    public function only() {}
}
Kid::make()->with()->only();
Base::make()->with();
$cls = Kid::class;
$cls::make()->only();
";

/// A fluent builder's `static` follows the class each call is made on, the
/// way Perl's `$self` does: `Kid::make()->with()` is a `Kid`, so `only`
/// resolves on `Kid`, through a class held in a variable too.
#[cfg(feature = "php")]
#[test]
fn a_fluent_chain_follows_the_class_the_call_is_made_on() {
    let fa = php(FLUENT_LSB);
    assert_eq!(
        call_receivers(&fa),
        calls_on(&[("make", "Kid"), ("with", "Kid"), ("only", "Kid"), ("make", "Base"), ("with", "Base"), ("make", "Kid"), ("only", "Kid")])
    );
    let ws = Workspace::new(&[("/fake/php/fluent.php", FLUENT_LSB)]);
    assert_eq!(ws.defs("fluent.php", "only();\nBase", 0), vec![site("fluent.php", 6, 20)]);
    assert_eq!(ws.defs("fluent.php", "only();\n", 0), vec![site("fluent.php", 6, 20)]);

    // the builder, the subclass and the chain each in a file of their own,
    // the bodies too: `return new static` and `return $this`
    let base = "<?php\nnamespace Lib;\n\nclass Base {\n    public static function make() { return new static(); }\n    public function with() { return $this; }\n}\n";
    let kid = "<?php\nnamespace App;\n\nuse Lib\\Base;\n\nclass Kid extends Base {\n    public function only() {}\n}\n";
    let run = "<?php\nnamespace App;\n\nKid::make()->with()->only();\n";
    let ws = Workspace::new(&[("/fake/php/Lib/Base.php", base), ("/fake/php/App/Kid.php", kid), ("/fake/php/App/run.php", run)]);
    assert_eq!(ws.defs("run.php", "only()", 0), vec![site("Kid.php", 6, 20)]);
    assert_eq!(ws.defs("run.php", "with()", 0), vec![site("Base.php", 5, 20)]);
}

const NEW_STATIC: &str = "<?php
class Base {
    public static function fresh() { return new static(); }
    public static function plain() { return new self(); }
    public function me() { return $this; }
    public function base() {}
}
class Kid extends Base {
    public function kid() {}
}
Kid::fresh()->kid();
Kid::plain()->base();
(new Kid())->me()->kid();
Base::fresh()->base();
";

/// `new static` is an instance of the class the call is made on and `new
/// self` of the class it is written in; `return $this` returns the object
/// the call is made on.
#[cfg(feature = "php")]
#[test]
fn new_static_follows_the_receiver_and_new_self_stays() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(NEW_STATIC);
    assert_eq!(
        call_receivers(&fa),
        calls_on(&[
            ("fresh", "Kid"),
            ("kid", "Kid"),
            ("plain", "Kid"),
            ("base", "Base"),
            ("me", "Kid"),
            ("kid", "Kid"),
            ("fresh", "Base"),
            ("base", "Base"),
        ])
    );
    // asked of the class itself, with no call site, each answers the class
    // it is written in
    let ret = |m: &str| fa.find_method_return_type("Base", m, None, None);
    assert_eq!(ret("fresh"), Some(T::ClassName("Base".into())));
    assert_eq!(ret("me"), Some(T::ClassName("Base".into())));
}

const FORWARD_PARENT: &str = "<?php
class Base {
    public static function make() { return new static(); }
    public function base() {}
}
class Kid extends Base {
    public static function viaParent() { return parent::make(); }
    public function inst() { return parent::make(); }
    public function kid() {}
}
class Grand extends Kid {
    public function grand() {}
}
Kid::viaParent()->kid();
Grand::viaParent()->grand();
(new Grand())->inst()->grand();
";

/// `parent::m()` forwards the called class: the `static` in the method it
/// reaches is still the class the outer call was made on, here and through
/// an inherited caller, from a static method and an instance one alike.
/// The `parent` token itself still names the parent.
#[cfg(feature = "php")]
#[test]
fn a_parent_call_forwards_the_called_class() {
    let fa = php(FORWARD_PARENT);
    assert_eq!(
        call_receivers(&fa),
        calls_on(&[
            ("make", "Base"),
            ("make", "Base"),
            ("viaParent", "Kid"),
            ("kid", "Kid"),
            ("viaParent", "Grand"),
            ("grand", "Grand"),
            ("inst", "Grand"),
            ("grand", "Grand"),
        ])
    );
    let ws = Workspace::new(&[("/fake/php/fwd.php", FORWARD_PARENT)]);
    assert_eq!(ws.defs("fwd.php", "make(); }\n    public function inst", 0), vec![site("fwd.php", 2, 27)]);
}

/// The object a forwarding call runs on is read where the call is written,
/// whatever point the question about it is asked from: here a local typed
/// above the classes it calls.
#[cfg(feature = "php")]
#[test]
fn a_forwarded_receiver_is_read_at_the_call() {
    use crate::model::file_analysis::InferredType as T;
    let src = "<?php
function first() {
    $k = Kid::viaParent();
    return $k;
}
class Base {
    public static function make() { return new static(); }
}
class Kid extends Base {
    public static function viaParent() { return parent::make(); }
}
";
    let fa = php(src);
    assert_eq!(fa.inferred_type_via_bag("k", at(src, "return $k", 0)), Some(T::ClassName("Kid".into())));
}

const FORWARD_SELF: &str = "<?php
class Other {}
class Base {
    public static function make() { return new static(); }
    public static function viaSelf() { return self::make(); }
    public function base() {}
}
class Kid extends Base {
    public static function make() { return new Other(); }
    public function kid() {}
}
Kid::viaSelf()->kid();
Base::viaSelf()->base();
";

/// `self::m()` looks `m` up from the class it is written in, so a
/// subclass's override is not the method it runs, and it forwards the
/// called class, so that method's `static` is the subclass the outer call
/// was made on.
#[cfg(feature = "php")]
#[test]
fn a_self_call_forwards_the_called_class() {
    let fa = php(FORWARD_SELF);
    assert_eq!(
        call_receivers(&fa),
        calls_on(&[("make", "Base"), ("viaSelf", "Kid"), ("kid", "Kid"), ("viaSelf", "Base"), ("base", "Base")])
    );
    let ws = Workspace::new(&[("/fake/php/fwd.php", FORWARD_SELF)]);
    assert_eq!(ws.defs("fwd.php", "make(); }", 0), vec![site("fwd.php", 3, 27)]);
}

const STATIC_RETURN: &str = "<?php
class Query {
    public function where(): static {}
    public function maybe(): ?static {}
    public function again(): self {}
    public function query() {}
    public function orNull(): static|null {}
    public function nullOr(): null|static {}
}
class Users extends Query {
    public function active() {}
}
function run(Users $u) {
    $u->where()->active();
    $u->maybe()->active();
    $u->again()->query();
    $u->orNull()->active();
    $u->nullOr()->active();
}
";

/// A declared `static` return is the class the call is made on, nullable
/// (`?static`, `static|null`, `null|static`) or not, and `self` the class
/// the method is written in.
#[cfg(feature = "php")]
#[test]
fn a_static_return_is_the_class_the_call_is_made_on() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php(STATIC_RETURN);
    assert_eq!(
        call_receivers(&fa),
        calls_on(&[
            ("where", "Users"),
            ("active", "Users"),
            ("maybe", "Users"),
            ("active", "Users"),
            ("again", "Users"),
            ("query", "Query"),
            ("orNull", "Users"),
            ("active", "Users"),
            ("nullOr", "Users"),
            ("active", "Users"),
        ])
    );
    assert_eq!(fa.find_method_return_type("Query", "orNull", None, None), Some(T::ClassName("Query".into())));
    assert_eq!(fa.find_method_return_type("Users", "where", None, None), Some(T::ClassName("Users".into())));
    assert_eq!(fa.find_method_return_type("Query", "where", None, None), Some(T::ClassName("Query".into())));
    assert_eq!(fa.find_method_return_type("Users", "again", None, None), Some(T::ClassName("Query".into())));
}

const STATIC_CALL: &str = "<?php
class Thing { public function thing() {} }
class Special { public function special() {} }
class Base {
    public static function create() { return static::build(); }
    public static function pinned() { return self::build(); }
    public static function build(): Thing {}
}
class Kid extends Base {
    public static function build(): Special {}
}
Kid::create()->special();
Base::create()->thing();
Kid::pinned()->thing();
";

/// `static::m()` in an inherited method calls `m` of the class the call is
/// made on, so a subclass's override answers; `self::m()` stays with the
/// class the method is written in.
#[cfg(feature = "php")]
#[test]
fn static_calls_in_an_inherited_method_dispatch_on_the_subclass() {
    let fa = php(STATIC_CALL);
    let receivers: Vec<(String, Option<String>)> =
        call_receivers(&fa).into_iter().filter(|(m, _)| !matches!(m.as_str(), "build" | "create" | "pinned")).collect();
    assert_eq!(receivers, calls_on(&[("special", "Special"), ("thing", "Thing"), ("thing", "Thing")]));
    // in the body itself, with no call site, `static::` is the class the
    // method is written in
    let build: Vec<(String, Option<String>)> = call_receivers(&fa).into_iter().filter(|(m, _)| m == "build").collect();
    assert_eq!(build, calls_on(&[("build", "Base"), ("build", "Base")]));
}


/// An override answers for its class: a parent declared in the same file
/// never stands in for the child's own method.
#[cfg(feature = "php")]
#[test]
fn an_override_answers_for_its_class() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php("<?php
class Thing {}
class Special {}
class Base {
    public function build(): Thing {}
    public function keep(): Thing {}
}
class Kid extends Base {
    public function build(): Special {}
}
");
    let ret = |c: &str, m: &str| fa.find_method_return_type(c, m, None, None);
    assert_eq!(ret("Kid", "build"), Some(T::ClassName("Special".into())));
    assert_eq!(ret("Base", "build"), Some(T::ClassName("Thing".into())));
    assert_eq!(ret("Kid", "keep"), Some(T::ClassName("Thing".into())), "an inherited method");
}

/// A callable that declares no return returns what its body returns; a
/// closure's `return` is the closure's, never the callable's around it.
#[cfg(feature = "php")]
#[test]
fn an_undeclared_return_is_what_the_body_returns() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php("<?php
class Clock { public function tick() {} }
class Box {
    public function clock() { return new Clock(); }
    public function sorted(array $xs) {
        usort($xs, function ($a, $b) { return 1; });
        $f = function () { return new Box(); };
        return new Clock();
    }
    public function maker() { return function () { return new Clock(); }; }
    public function declared(): Box { return new Clock(); }
}
function clock() { return new Clock(); }
");
    let clock = Some(T::ClassName("Clock".into()));
    let ret = |m: &str| fa.find_method_return_type("Box", m, None, None);
    assert_eq!(ret("clock"), clock);
    assert_eq!(ret("sorted"), clock, "the closures' returns are their own");
    assert_eq!(ret("maker"), None, "a closure is no `Clock`");
    assert_eq!(ret("declared"), Some(T::ClassName("Box".into())), "a declared return is the value");
    assert_eq!(fa.sub_return_type_at_arity("clock", Some(0)), clock, "a function");
}

/// A callable that yields returns a `Generator`, whatever its `return`
/// says; a closure's or an arrow function's `yield` is its own, and a
/// declared return still answers first.
#[cfg(feature = "php")]
#[test]
fn a_callable_that_yields_returns_a_generator() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php("<?php
namespace App;
class Clock {}
class Box {
    public function gen() { yield 1; return new Clock(); }
    public function delegates() { yield from [1, 2]; }
    public function outer() {
        $f = function () { yield 1; };
        $g = fn() => yield 2;
        return new Clock();
    }
    public function declared(): \\Iterator { yield 1; }
    public function spelled(): \\Generator { return make(); }
}
function produce() { yield 1; }
");
    let generator = Some(T::ClassName("Generator".into()));
    let ret = |m: &str| fa.find_method_return_type("App\\Box", m, None, None);
    assert_eq!(ret("gen"), generator, "a `return` beside a `yield` types nothing");
    assert_eq!(ret("delegates"), generator, "`yield from`");
    assert_eq!(ret("outer"), Some(T::ClassName("App\\Clock".into())), "the closures yield, not the method");
    assert_eq!(ret("declared"), Some(T::ClassName("Iterator".into())), "a declared return answers first");
    assert_eq!(ret("spelled"), generator, "the class a written `\\Generator` names");
    assert_eq!(fa.sub_return_type_at_arity("produce", Some(0)), generator, "a function");
}

/// The completion items at `|` in `src`, asked as the editor asks: the file
/// indexed, the cursor's buffer the one written.
#[cfg(feature = "php")]
fn completions(src: &str) -> Vec<(String, Option<tower_lsp::lsp_types::CompletionItemKind>)> {
    completion_items(src).into_iter().map(|i| (i.label, i.kind)).collect()
}

#[cfg(feature = "php")]
fn completion_items(src: &str) -> Vec<tower_lsp::lsp_types::CompletionItem> {
    completion_items_in("complete.php", "php", src)
}

#[cfg(any(feature = "php", feature = "cpp"))]
fn completion_items_in(path: &str, lang: &str, src: &str) -> Vec<tower_lsp::lsp_types::CompletionItem> {
    let at = src.find('|').expect("a cursor");
    let src = format!("{}{}", &src[..at], &src[at + 1..]);
    let row = src[..at].matches('\n').count();
    let point = tree_sitter::Point { row, column: at - src[..at].rfind('\n').map_or(0, |n| n + 1) };
    let path = std::path::PathBuf::from(path);
    let driver = crate::build::language_driver::LanguageRegistry::compiled();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test(path.clone(), driver.for_path(&path).unwrap().analyze(&src));
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let tree = driver.for_path(&path).unwrap().make_parser().parse(&src, None).unwrap();
    let store = crate::index::open_docs::OpenDocs::new();
    let canon = crate::util::canonical_path::CanonicalPath::fixture(path);
    let (items, _) = crate::lsp::backend::pack_completion(&store, &fa, &src, &tree, point, lang, Some(&canon), &idx);
    items
}

/// Every pack's member item replaces the member as written up to the
/// cursor, not only php's: a C++ `w.wi|` item's edit covers `wi`.
#[cfg(feature = "cpp")]
#[test]
fn a_cpp_member_item_replaces_the_written_prefix() {
    use tower_lsp::lsp_types::{CompletionTextEdit, Position, Range};
    let items = completion_items_in("complete.cpp", "cpp", "struct W { int width; int height; };\nint f(W w) { return w.wi|; }\n");
    let item = items.iter().find(|i| i.label == "width").expect("the member");
    let range = |c0, c1| Range { start: Position { line: 1, character: c0 }, end: Position { line: 1, character: c1 } };
    match &item.text_edit {
        Some(CompletionTextEdit::Edit(e)) => assert_eq!((e.new_text.as_str(), e.range), ("width", range(22, 24))),
        other => panic!("{other:?}"),
    }
}

#[cfg(feature = "php")]
fn labels(src: &str) -> Vec<String> {
    let mut out: Vec<String> = completions(src).into_iter().map(|(l, _)| l).collect();
    out.sort();
    out
}

#[cfg(feature = "php")]
const SHAPES: &str = "<?php
namespace App;
class Base {
    public $shared;
    public static $count = 0;
    const LIMIT = 3;
    public function greet() {}
    public static function make(): static {}
}
class Kid extends Base {
    public ?Kid $next;
    public function run(Base $b) {
        CURSOR
    }
}
";

/// After `->` or `?->` a member is one of the object's methods or
/// properties; after `::` one of the class's methods, constants or static
/// properties (written `$x`), or `class`.
#[cfg(feature = "php")]
#[test]
fn member_completion_offers_what_the_operator_reaches() {
    let at = |hole: &str| labels(&SHAPES.replace("CURSOR", hole));
    let instance = ["greet", "make", "shared"];
    assert_eq!(at("$b->|"), instance, "a typed parameter");
    assert_eq!(at("$b?->|"), instance, "the nullsafe operator");
    assert_eq!(at("$this->|"), ["greet", "make", "next", "run", "shared"], "the class the method is written in");
    assert_eq!(at("$this->next->|"), ["greet", "make", "next", "run", "shared"], "a chain");
    let statics = ["$count", "LIMIT", "class", "greet", "make"];
    assert_eq!(at("Base::|"), statics, "a class name");
    assert_eq!(at("parent::|"), statics, "the parent");
    assert_eq!(at("self::|"), ["$count", "LIMIT", "class", "greet", "make", "run"], "the enclosing class");
    assert_eq!(at("static::|"), ["$count", "LIMIT", "class", "greet", "make", "run"], "the called class");
    assert_eq!(at("Base::make()->|"), instance, "a call on the class");
    assert_eq!(at("$b->gr|eet();"), instance, "completing in place of a member");
}

/// A typed `$` writes the member as declared, so the access reaches only
/// the namespaces whose members `::` writes so: static properties, never a
/// method, a constant or `class`.
#[cfg(feature = "php")]
#[test]
fn a_typed_dollar_offers_only_static_properties() {
    let at = |hole: &str| labels(&SHAPES.replace("CURSOR", hole));
    assert_eq!(at("self::$|"), ["$count"]);
    assert_eq!(at("Base::$co|"), ["$count"]);
    assert_eq!(at("self::|"), ["$count", "LIMIT", "class", "greet", "make", "run"], "nothing typed");
}

/// A `spelled` namespace lists every member it offers as declared, methods
/// as well as fields, here and across files: the listing is the
/// namespace's, not the member kind's.
#[cfg(feature = "php")]
#[test]
fn a_spelled_namespace_lists_its_methods_as_declared() {
    use crate::model::file_analysis::{MemberReach, SpaceSet};
    let driver = crate::build::language_driver::LanguageRegistry::compiled().for_path(std::path::Path::new("x.php")).unwrap();
    let declared = |src: &str, path: &str| {
        let mut fa = driver.analyze_with_path(src, Some(std::path::Path::new(path)));
        for sym in fa.symbols_mut() {
            if matches!(sym.kind, SymKind::Method) {
                sym.presentation.label = Some(format!("{}()", sym.name));
            }
        }
        fa
    };
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_workspace_for_test("/w/Base.php".into(), declared("<?php\nclass Base { public static function make() {} }\n", "/w/Base.php"));
    let fa = declared("<?php\nclass Kid extends Base { public static function build() {} }\n", "/w/Kid.php");
    let method = fa.member_spaces().id_of("method").unwrap();
    let reach = |spelled| MemberReach { spaces: SpaceSet::of(method), forward: SpaceSet::default(), spelled };
    let labels = |spelled| {
        let mut out: Vec<String> =
            fa.complete_members_for_class("Kid", Some(&idx), None, false, Some(reach(spelled))).into_iter().map(|c| c.label).collect();
        out.sort();
        out
    };
    assert_eq!(labels(SpaceSet::of(method)), ["build()", "make()"], "local and cross-file");
    assert_eq!(labels(SpaceSet::default()), ["build", "make"], "a namespace not spelled lists names");
}

/// A static property completes as written, `$` and all, whether or not the
/// `$` is typed: the item replaces a typed `$`, never doubling it.
#[cfg(feature = "php")]
#[test]
fn a_static_property_replaces_a_typed_dollar() {
    use tower_lsp::lsp_types::{CompletionTextEdit, Position};
    let edit = |hole: &str| {
        let item = completion_items(&SHAPES.replace("CURSOR", hole)).into_iter().find(|i| i.label == "$count");
        match item.and_then(|i| i.text_edit) {
            Some(CompletionTextEdit::Edit(e)) => Some((e.new_text, e.range.start, e.range.end)),
            _ => None,
        }
    };
    let at = |col| Position { line: 12, character: col };
    assert_eq!(edit("self::|"), Some(("$count".into(), at(14), at(14))), "nothing typed");
    assert_eq!(edit("self::$|"), Some(("$count".into(), at(14), at(15))), "the `$` typed");
    assert_eq!(edit("self::$co|"), Some(("$count".into(), at(14), at(17))), "a prefix typed");
}

/// `::` reaches an instance method only where the call forwards the object
/// the calling code runs on: from an instance method (or a closure in one)
/// of the class named or one it extends. Elsewhere only static methods are
/// offered; constants, static properties and `class` everywhere.
#[cfg(feature = "php")]
#[test]
fn an_instance_method_is_offered_after_double_colon_only_where_the_call_forwards() {
    let src = "<?php
namespace App;
class Base {
    const LIMIT = 3;
    public static $count = 0;
    public function greet() {}
    public static function make() {}
    public function look() { BASE }
}
class Kid extends Base {
    public function run() { KID }
    public static function build() { STATIC }
}
class Stranger {
    public function run() { STRANGER }
}
function free() { FREE }
";
    let at = |hole: &str, cursor: &str| {
        let mut s = src.to_string();
        for h in ["BASE", "KID", "STATIC", "STRANGER", "FREE"] {
            s = s.replace(h, if h == hole { cursor } else { "" });
        }
        labels(&s)
    };
    let forwarded = ["$count", "LIMIT", "class", "greet", "look", "make"];
    let statics = ["$count", "LIMIT", "class", "make"];
    assert_eq!(at("KID", "parent::|"), forwarded, "the parent, from an instance method");
    assert_eq!(at("KID", "Base::|"), forwarded, "an ancestor by name");
    assert_eq!(at("KID", "$f = function () { parent::| };"), forwarded, "a closure runs on the method's object");
    assert_eq!(at("STATIC", "parent::|"), statics, "a static method runs on no object");
    assert_eq!(at("STRANGER", "Base::|"), statics, "a class outside the lineage");
    assert_eq!(at("BASE", "Kid::|"), ["$count", "LIMIT", "build", "class", "make"], "a descendant");
    assert_eq!(at("FREE", "Base::|"), statics, "free code");
    assert_eq!(at("FREE", "Stranger::|"), ["class"], "a class with nothing reachable still offers `class`");
}

#[cfg(feature = "php")]
const ACCESS: &str = "<?php
namespace App;
class Base {
    public $open;
    protected $kin;
    private $mine;
    const OPEN = 1;
    protected const KIN = 2;
    private const MINE = 3;
    public function greet() {}
    protected function normalize() {}
    private function secret() {}
    public function __construct(protected int $made, private int $own) {}
    public function here() { HERE }
}
class Kid extends Base {
    public function run(Base $b) { KID }
}
class Stranger {
    public function run(Base $b) { STRANGER }
}
";

/// A private member is named only from its own class, a protected one
/// from its class and the classes extending it, a public one anywhere.
#[cfg(feature = "php")]
#[test]
fn a_protected_member_is_offered_in_its_class_and_descendants() {
    let at = |hole: &str, cursor: &str| {
        let mut src = ACCESS.to_string();
        for h in ["HERE", "KID", "STRANGER"] {
            src = src.replace(h, if h == hole { cursor } else { "" });
        }
        labels(&src)
    };
    assert_eq!(
        at("HERE", "$this->|"),
        ["__construct", "greet", "here", "kin", "made", "mine", "normalize", "open", "own", "secret"],
        "its own class names every member"
    );
    assert_eq!(
        at("KID", "$this->|"),
        ["__construct", "greet", "here", "kin", "made", "normalize", "open", "run"],
        "a subclass names the protected, not the private"
    );
    assert_eq!(
        at("KID", "$b->|"),
        ["__construct", "greet", "here", "kin", "made", "normalize", "open"],
        "on any receiver of the family"
    );
    assert_eq!(
        at("KID", "parent::|"),
        ["KIN", "OPEN", "__construct", "class", "greet", "here", "normalize"],
        "the parent's protected methods and constants"
    );
    assert_eq!(at("STRANGER", "$b->|"), ["__construct", "greet", "here", "open"], "outside the family");
    assert_eq!(at("STRANGER", "Base::|"), ["OPEN", "class"], "outside the family, instance methods unreached");
}

/// A parent names its child's protected members on a receiver typed as
/// the child, never its private ones; a class outside the lineage names
/// neither.
#[cfg(feature = "php")]
#[test]
fn a_parent_is_offered_its_childs_protected_members() {
    let src = "<?php
namespace App;
class Base {
    public function look(Kid $k) { HERE }
}
class Kid extends Base {
    public $open;
    protected $kin;
    private $mine;
    protected function normalize() {}
}
class Stranger {
    public function look(Kid $k) { THERE }
}
";
    let at = |hole: &str| {
        let other = if hole == "HERE" { "THERE" } else { "HERE" };
        labels(&src.replace(hole, "$k->|").replace(other, ""))
    };
    assert_eq!(at("HERE"), ["kin", "look", "normalize", "open"], "the parent");
    assert_eq!(at("THERE"), ["look", "open"], "outside the lineage");
}

/// A member and a constant or property of one name are two members, each
/// offered where its operator reaches it.
#[cfg(feature = "php")]
#[test]
fn a_method_and_a_property_of_one_name_are_both_offered() {
    use tower_lsp::lsp_types::CompletionItemKind as K;
    let src = "<?php
class Box {
    public $size;
    const size = 1;
    public function size() {}
    public function go() { $this->| }
}
";
    let mut items = completions(src);
    items.sort_by_key(|(l, k)| (l.clone(), format!("{k:?}")));
    assert_eq!(items, [("go".to_string(), Some(K::METHOD)), ("size".into(), Some(K::FIELD)), ("size".into(), Some(K::METHOD))]);
}

/// A local is offered below its first write in the body that writes it,
/// as Perl offers a `my` below its declaration, though it binds from the
/// top of the body.
#[cfg(feature = "php")]
#[test]
fn a_local_is_offered_below_its_first_write() {
    let src = "<?php
function f($p) {
    $early = 1;

    $late = 2;

    $late = 3;
}
";
    let fa = php(src);
    let offered = |row: usize| -> Vec<String> {
        let mut out: Vec<String> = crate::lsp::symbols::in_scope_completion(&fa, tree_sitter::Point::new(row, 4))
            .into_iter()
            .map(|c| c.label)
            .filter(|l| ["p", "early", "late"].contains(&l.as_str()))
            .collect();
        out.sort();
        out
    };
    assert_eq!(offered(3), ["early", "p"], "above the first write");
    assert_eq!(offered(5), ["early", "late", "p"], "below it");
    assert!(fa.resolve_variable("late", tree_sitter::Point::new(3, 4)).is_some(), "it still binds from the top");
}

/// A receiver the written buffer leaves untyped is typed on the buffer the
/// sentinel finished only where the written one failed to parse the
/// access: `Foo::` at the end of a block is an ERROR, so the finished
/// buffer types `Foo`; `$u->go()` parsed, so its untyped `$u` stays
/// untyped and the finished buffer is never analyzed.
#[cfg(feature = "php")]
#[test]
fn the_finished_buffer_is_analyzed_only_where_the_written_one_failed_to_parse() {
    let driver = crate::build::language_driver::LanguageRegistry::compiled().for_path(std::path::Path::new("t.php")).unwrap();
    let ask = |src: &str, cursor_after: &str| {
        let cursor = src.find(cursor_after).unwrap() + cursor_after.len();
        let fa = driver.analyze(src);
        let mut parser = driver.make_parser();
        let tree = parser.parse(src, None).unwrap();
        let analyzed = std::cell::Cell::new(0);
        let completed = |patched: &str| {
            analyzed.set(analyzed.get() + 1);
            driver.analyze(patched)
        };
        let ctx = crate::build::cursor_sentinel::member_completion_ctx_incremental(
            &mut parser, &php_pack(), src, &tree, cursor, &fa, None, &completed,
        )
        .unwrap();
        (ctx.receiver_type.and_then(|t| t.class_name().map(str::to_string)), analyzed.get())
    };
    let error = "<?php\nclass Foo { const X = 1; }\nfunction f() {\n  Foo::\n}\n";
    assert_eq!(ask(error, "  Foo::"), (Some("Foo".to_string()), 1));
    let parsed = "<?php\nfunction f($u) {\n  $u->go();\n}\n";
    assert_eq!(ask(parsed, "$u->"), (None, 0));
}

const HOVERS: &str = "<?php
namespace App;

/**
 * A box of things.
 */
class Foo {
    /** How many. */
    public int $n;

    /**
     * Make a Foo.
     *
     * @param int $n the count
     */
    public function __construct(int $n) { $this->n = $n; }

    /** Twice the count. */
    public function twice(): int { return $this->n * 2; }

    // Not a docblock.
    public function me() { return $this; }

    /** @return static */
    public function tagged() { return $this; }
}

class Bar {}

/** Build one. */
function make(int $k): Foo { return new Foo($k); }

function run() {
    $f = new Foo(1);
    $g = $f->twice();
    $f = new Bar();
    return [$f, $g];
}
";

/// The hover at `needle` (+`offset`) in the indexed file `path`.
#[cfg(feature = "php")]
fn php_hover(idx: &crate::index::module_index::ModuleIndex, path: &str, src: &str, needle: &str, offset: usize) -> String {
    use crate::index::open_docs::{FileKey, OpenDocs};
    use crate::index::resolve::{resolve, OverrideScope};
    let fa = idx.workspace_file_for_test(std::path::Path::new(path)).unwrap();
    let key = FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(std::path::PathBuf::from(path)));
    let docs = OpenDocs::new();
    let cs = resolve(&docs, &fa, key, at(src, needle, offset), Some(idx), OverrideScope::default()).with_source(src);
    crate::lsp::symbols::pack_hover_markdown(&cs, "php").unwrap_or_default()
}

/// Every local is listed — in hover and the outline — by the `$x` it is
/// written as, whichever site declares it, and the spelling costs none of
/// the facts the site's other captures state.
#[cfg(feature = "php")]
#[test]
fn a_local_is_listed_by_its_written_spelling_at_every_declaring_site() {
    let src = "<?php
function all(int $p, ...$v) {
    $a = 1;
    $r = &$a;
    $c ??= 2;
    [$l1, $l2] = [1, 2];
    ['k' => $kv] = ['k' => 1];
    foreach ([1] as $e) {}
    foreach ([1] as &$br) {}
    foreach ([1] as $fk => $fv) {}
    foreach ([1] as $gk => &$gv) {}
    try {} catch (\\Exception $ex) {}
    global $gl;
    static $st = 1;
    $fn = function () use (&$hoisted) {};
}
class P { public function __construct(private int $promo) {} }
$top = 1;
";
    let fa = php(src);
    let names = [
        "p", "v", "a", "r", "c", "l1", "l2", "kv", "e", "br", "fk", "fv", "gk", "gv", "ex", "gl", "st", "fn", "hoisted", "promo", "top",
    ];
    for name in names {
        let syms: Vec<_> = fa.symbols().iter().filter(|s| s.name == name).collect();
        assert!(!syms.is_empty(), "`${name}` is declared");
        for s in syms {
            assert_eq!(s.listed_name(), format!("${name}"), "{:?}", s.kind);
        }
    }
    let ty = |name: &str, needle: &str| fa.inferred_type_via_bag(name, at(src, needle, 0)).map(|t| fa.render_type(&t));
    // the value each write's `@flow.target` states still lands
    assert_eq!(ty("a", "$fn = "), Some("int|float".to_string()));
    assert_eq!(ty("st", "$fn = "), Some("int|float".to_string()));
    assert_eq!(ty("ex", "{}\n    global"), Some("Exception".to_string()));
    assert_eq!(ty("p", "$fn = "), Some("int|float".to_string()));
    assert_eq!(ty("promo", "{} }"), Some("int|float".to_string()));
    // a by-reference capture still declares in the function around the closure
    let hoisted = fa.symbols().iter().find(|s| s.name == "hoisted").unwrap();
    let all = fa.symbols().iter().find(|s| s.name == "all").unwrap();
    assert!(crate::model::file_analysis::contains_point(&all.span, fa.scope(hoisted.scope).span.start));
    assert_ne!(fa.scope(hoisted.scope).span, fa.scope(fa.scope_at(at(src, "use (&", 0)).unwrap()).span);
    // a script's top-level local is in the outline as written
    assert!(fa.document_symbols().iter().any(|o| o.name == "$top"));
}

/// A typed local hovers by its `$x`, and at a write it hovers the value
/// that write gives — the write lands after its right side is read, so
/// the cursor on its left side is before it.
#[cfg(feature = "php")]
#[test]
fn a_write_hovers_the_value_it_writes() {
    let path = "/fake/php/App/hovers.php";
    let idx = php_indexed(&[(path, HOVERS)]);
    let hover = |needle: &str| php_hover(&idx, path, HOVERS, needle, 1);
    assert!(hover("$f = new Foo").contains("\n$f: App\\Foo\n"), "{}", hover("$f = new Foo"));
    assert!(hover("$g = $f").contains("\n$g: int|float\n"), "{}", hover("$g = $f"));
    assert!(hover("$f = new Bar").contains("\n$f: App\\Bar\n"), "{}", hover("$f = new Bar"));
    // a read between the writes holds the first
    assert!(hover("$f->twice").contains("\n$f: App\\Foo\n"), "{}", hover("$f->twice"));
    assert!(hover("$f, $g]").contains("\n$f: App\\Bar\n"), "{}", hover("$f, $g]"));
}

/// A docblock directly before a declaration is its documentation: its
/// description, without the frame or the tags. Any other comment, or a
/// docblock of tags alone, documents nothing.
#[cfg(feature = "php")]
#[test]
fn a_docblock_documents_the_declaration_it_precedes() {
    let fa = php(HOVERS);
    let doc = |name: &str| fa.symbols().iter().find(|s| s.name == name).and_then(|s| s.doc().map(str::to_string));
    assert_eq!(doc("App\\Foo").as_deref(), Some("A box of things."));
    assert_eq!(doc("n").as_deref(), Some("How many."));
    assert_eq!(doc("__construct").as_deref(), Some("Make a Foo."));
    assert_eq!(doc("twice").as_deref(), Some("Twice the count."));
    assert_eq!(doc("make").as_deref(), Some("Build one."));
    assert_eq!(doc("me"), None);
    assert_eq!(doc("tagged"), None);
    assert_eq!(doc("App\\Bar"), None);
}

/// A callable hovers as Perl's does: its declaration, where it lives, what
/// it returns (declared or inferred) and its docblock — at its
/// declaration and at a call alike.
#[cfg(feature = "php")]
#[test]
fn a_callable_hovers_its_owner_return_and_docblock() {
    let path = "/fake/php/App/hovers.php";
    let idx = php_indexed(&[(path, HOVERS)]);
    let hover = |needle: &str, offset: usize| php_hover(&idx, path, HOVERS, needle, offset);
    let twice = hover("twice(): int", 0);
    assert!(twice.contains("public function twice(): int"), "{twice}");
    assert!(twice.contains("*class App\\Foo*\n\n*returns: int|float*\n\nTwice the count."), "{twice}");
    assert_eq!(hover("twice();", 0), twice, "a call shows what the declaration does");
    let me = hover("me()", 0);
    assert!(me.contains("*class App\\Foo*\n\n*returns: App\\Foo*"), "an inferred return: {me}");
    assert!(!me.contains("Not a docblock"), "{me}");
    let make = hover("make(int", 0);
    assert!(make.contains("*function*\n\n*namespace App*\n\n*returns: App\\Foo*\n\nBuild one."), "{make}");
    let n = hover("$n;", 1);
    assert!(n.contains("$n: int|float") && n.ends_with("How many."), "{n}");
}

/// A construction's class token is the class — what goto-def lands on —
/// and the call of the class's constructor, shown under it as a member
/// call to it is: its declaration, class and docblock. In another file
/// too.
#[cfg(feature = "php")]
#[test]
fn a_construction_hovers_its_class_and_constructor() {
    let path = "/fake/php/App/hovers.php";
    let idx = php_indexed(&[(path, HOVERS)]);
    let hover = php_hover(&idx, path, HOVERS, "Foo(1)", 0);
    let (class, ctor) = hover.split_once("\n\n---\n\n").unwrap_or_else(|| panic!("{hover}"));
    assert!(class.starts_with("```php\nclass Foo\n```\n\n*class*") && class.ends_with("A box of things."), "{class}");
    assert!(ctor.contains("public function __construct(int $n)"), "{ctor}");
    assert!(ctor.contains("*class App\\Foo*") && ctor.ends_with("Make a Foo."), "{ctor}");
    // a class with no constructor is the class alone
    let bar = php_hover(&idx, path, HOVERS, "Bar()", 0);
    assert!(!bar.contains("---"), "{bar}");

    let lib = "<?php\nnamespace Lib;\n\nclass Handler {\n    /** Wrap a level. */\n    public function __construct(int $level) {}\n}\n";
    let app = "<?php\nnamespace App;\n\nuse Lib\\Handler;\n\nfunction go() { return new Handler(1); }\n";
    let root = std::env::temp_dir().join(format!("php-ctor-hover-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let files: Vec<(String, &str)> =
        [("Handler.php", lib), ("go.php", app)].iter().map(|(n, s)| (root.join(n).to_string_lossy().to_string(), *s)).collect();
    for (p, s) in &files {
        std::fs::write(p, s).unwrap();
    }
    let idx = php_indexed(&files.iter().map(|(p, s)| (p.as_str(), *s)).collect::<Vec<_>>());
    let hover = php_hover(&idx, &files[1].0, app, "Handler(1)", 0);
    std::fs::remove_dir_all(&root).ok();
    let (_, ctor) = hover.split_once("\n\n---\n\n").unwrap_or_else(|| panic!("{hover}"));
    assert!(ctor.contains("public function __construct(int $level)"), "{ctor}");
    assert!(ctor.contains("*class Lib\\Handler*") && ctor.contains("Wrap a level.") && ctor.contains("Handler.php"), "{ctor}");
}

/// A free constant is a def in both of its spellings: a namespace-level
/// `const` files under the namespace in force, `define` under the global
/// namespace wherever it is written. Each is a constant, never a variable,
/// and is filed for linkage under its namespaced identity.
#[cfg(feature = "php")]
#[test]
fn a_free_constant_is_a_def_in_both_spellings() {
    use crate::model::file_analysis::SymbolFlags;
    let fa = php("<?php\nnamespace App;\nconst LIMIT = 3;\ndefine('MAX', 'x');\nclass C { const INNER = 1; }\n");
    let constants: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.flags.contains(SymbolFlags::CONSTANT))
        .map(|s| (s.name.clone(), s.package.clone(), fa.linkage_identity(s).into_owned()))
        .collect();
    assert_eq!(
        constants,
        [
            ("LIMIT".to_string(), Some("App".to_string()), "App\\LIMIT".to_string()),
            ("MAX".to_string(), None, "MAX".to_string()),
        ]
    );
    assert!(fa.symbols().iter().filter(|s| s.flags.contains(SymbolFlags::CONSTANT)).all(|s| s.variable_decl().is_none()));
}

/// Within a file a constant's uses — bare and `\`-qualified — land on it, hover with its value's type, and
/// rename together; `$LIMIT` is another name, and a function of the same
/// name is another table.
#[cfg(feature = "php")]
#[test]
fn a_free_constant_resolves_within_its_file() {
    let src: &'static str = "<?php\nnamespace App;\nconst LIMIT = 3;\nfunction LIMIT() {}\n$LIMIT = 'x';\necho LIMIT;\necho \\App\\LIMIT;\nLIMIT();\necho $LIMIT;\n";
    let ws = Workspace::new(&[("/fake/php/App/c.php", src)]);
    for (needle, offset) in [("LIMIT;", 0), ("\\App\\LIMIT", 5)] {
        assert_eq!(ws.defs("c.php", needle, offset), [site("c.php", 2, 6)], "{needle}");
    }
    assert_eq!(ws.defs("c.php", "LIMIT();\necho", 0), [site("c.php", 3, 9)], "the call is the function's");
    assert_eq!(
        ws.renames("c.php", "LIMIT = 3", 0),
        [site("c.php", 2, 6), site("c.php", 5, 5), site("c.php", 6, 10)]
    );
    let idx = php_indexed(&[("/fake/php/App/c.php", src)]);
    let hover = php_hover(&idx, "/fake/php/App/c.php", src, "LIMIT;", 0);
    assert!(hover.contains("constant") && hover.contains("int"), "{hover}");
}

const PHP_LIB_CONSTS: (&str, &str) = ("/fake/php/Lib/consts.php", "<?php\nnamespace Lib;\nconst LIMIT = 3;\nconst ONLY = 1;\n");
const PHP_OTHER_CONSTS: (&str, &str) = ("/fake/php/Other/consts.php", "<?php\nnamespace Other;\nconst LIMIT = 'x';\n");

/// A constant is its namespace-qualified identity across files, as a
/// function is: a `use const` row binds its leaf, a qualified spelling
/// names one constant, a same-leaf constant of another namespace never
/// answers for it, and one nothing indexed declares widens out loud.
#[cfg(feature = "php")]
#[test]
fn a_constant_is_its_identity_across_files() {
    use tree_sitter::Point;
    let a = (
        "/fake/php/App/a.php",
        "<?php\nnamespace App;\nuse const Other\\LIMIT;\nfunction caller() {\n    return LIMIT . \\Lib\\LIMIT . ONLY;\n}\n",
    );
    let idx = php_indexed(&[PHP_LIB_CONSTS, PHP_OTHER_CONSTS, a]);
    let lands = |at: Point| php_landing(&idx, a.0, at).0;
    let other = [("Other/consts.php".to_string(), 2, None)];
    assert_eq!(lands(Point::new(4, 11)), other, "a bare use through its `use const` row");
    assert_eq!(lands(Point::new(2, 16)), other, "the row's own leaf");
    assert_eq!(lands(Point::new(4, 24)), [("Lib/consts.php".to_string(), 2, None)], "a qualified use");
    assert_eq!(
        lands(Point::new(4, 32)),
        [("Lib/consts.php".to_string(), 3, Some("widened: `Lib\\ONLY` stands in for `App\\ONLY`".to_string()))]
    );
    let msg = "nothing indexed declares `App\\ONLY`; resolved by its leaf to `Lib\\ONLY`".to_string();
    assert_eq!(php_widening(&idx, a.0), [(4, 32, msg)]);

    let ws = Workspace::new(&[PHP_LIB_CONSTS, PHP_OTHER_CONSTS, a]);
    assert_eq!(
        ws.renames("Other/consts.php", "LIMIT", 0),
        [site("a.php", 2, 16), site("a.php", 4, 11), site("consts.php", 2, 6)]
    );
    assert_eq!(ws.renames("Lib/consts.php", "LIMIT", 0), [site("a.php", 4, 24), site("consts.php", 2, 6)]);
}

/// A bare constant looks up the namespace in force's constant first and
/// the global one — `const` or `define` — only when the namespace
/// declares none, across files.
#[cfg(feature = "php")]
#[test]
fn a_bare_constant_tries_its_namespace_then_the_global_constant() {
    use tree_sitter::Point;
    let global = ("/fake/php/0/global.php", "<?php\nconst TWICE = 1;\ndefine('GONE', 2);\n");
    let own = ("/fake/php/App/own.php", "<?php\nnamespace App;\nconst TWICE = 3;\n");
    let b = ("/fake/php/App/b.php", "<?php\nnamespace App;\necho TWICE;\necho GONE;\necho \\TWICE;\n");
    let g = ("/fake/php/0/g.php", "<?php\necho App\\TWICE;\n");
    let idx = php_indexed(&[global, own, b, g]);
    assert_eq!(php_landing(&idx, b.0, Point::new(2, 5)).0, [("App/own.php".to_string(), 2, None)]);
    assert_eq!(php_landing(&idx, b.0, Point::new(3, 5)).0, [("0/global.php".to_string(), 2, None)]);
    assert_eq!(php_landing(&idx, b.0, Point::new(4, 6)).0, [("0/global.php".to_string(), 1, None)]);
    assert!(php_widening(&idx, b.0).is_empty());
    assert_eq!(php_landing(&idx, g.0, Point::new(1, 9)).0, [("App/own.php".to_string(), 2, None)], "namespace-qualified");
    let ws = Workspace::new(&[global, own, b, g]);
    assert_eq!(ws.renames("own.php", "TWICE", 0), [site("b.php", 2, 5), site("g.php", 1, 9), site("own.php", 2, 6)]);
    assert_eq!(ws.renames("global.php", "TWICE", 0), [site("b.php", 4, 6), site("global.php", 1, 6)]);
}

/// `Foo::X` is the class's member, never a free constant of the same
/// leaf; a builtin constant or literal names nothing indexed, so it has no
/// definition, no rename and no diagnostic.
#[cfg(feature = "php")]
#[test]
fn a_class_constant_and_a_builtin_are_no_free_constant() {
    let src: &'static str = "<?php\nnamespace App;\nclass Foo { const X = 1; }\nconst X = 2;\necho Foo::X;\necho X . PHP_EOL;\n$t = true;\n$n = null;\n";
    let ws = Workspace::new(&[("/fake/php/App/x.php", src)]);
    assert_eq!(ws.defs("x.php", "X;\necho X", 0), [site("x.php", 2, 18)]);
    assert_eq!(ws.defs("x.php", "X . PHP", 0), [site("x.php", 3, 6)]);
    assert!(ws.defs("x.php", "PHP_EOL", 0).is_empty());
    assert_eq!(ws.ask("x.php", "PHP_EOL", 0, |cs| cs.renameable()), Ok(false));
    let idx = php_indexed(&[("/fake/php/App/x.php", src)]);
    let fa = idx.workspace_file_for_test(std::path::Path::new("/fake/php/App/x.php")).unwrap();
    let messages: Vec<String> = crate::lsp::symbols::collect_diagnostics(&fa, &idx, Default::default())
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert!(messages.is_empty(), "{messages:?}");
}

/// `\f()` names the global function, whatever the namespace in force
/// declares: the leading `\` skips the namespace rung.
#[cfg(feature = "php")]
#[test]
fn a_global_qualified_call_reaches_the_global_function() {
    use tree_sitter::Point;
    let global = ("/fake/php/0/global.php", "<?php\nfunction f() {}\n");
    let own = ("/fake/php/App/own.php", "<?php\nnamespace App;\nfunction f() {}\n");
    let b = ("/fake/php/App/b.php", "<?php\nnamespace App;\n\\f();\nf();\n");
    let idx = php_indexed(&[global, own, b]);
    assert_eq!(php_landing(&idx, b.0, Point::new(2, 1)).0, [("0/global.php".to_string(), 1, None)]);
    assert_eq!(php_landing(&idx, b.0, Point::new(3, 0)).0, [("App/own.php".to_string(), 2, None)]);
    let ws = Workspace::new(&[global, own, b]);
    assert_eq!(ws.renames("global.php", "f()", 0), [site("b.php", 2, 1), site("global.php", 1, 9)]);
}

/// The narrowing builtins answer in their global-qualified spelling too —
/// `\is_null`, `\die`, `\exit` — and a spelling qualified by any other
/// namespace is somebody else's function.
#[cfg(feature = "php")]
#[test]
fn a_global_qualified_builtin_narrows() {
    let foo = Some(InferredType::ClassName("Lib\\Foo".into()));
    let src = "<?php\nnamespace App;\nuse Lib\\Foo;\nfunction f(?Foo $y) {\n    if (!\\is_null($y)) { $y->run(); }\n    if (!App\\is_null($y)) { $y->run(); }\n    if (!namespace\\is_null($y)) { $y->run(); }\n}\n";
    let fa = php(src);
    let mut rows: Vec<(usize, Option<InferredType>)> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "run" && matches!(r.kind, crate::model::file_analysis::RefKind::MethodCall { .. }))
        .map(|r| (r.span.start.row, fa.method_call_invocant_type(r, None)))
        .collect();
    rows.sort_by_key(|(row, _)| *row);
    let nullable = foo.clone().map(|t| InferredType::Optional(Box::new(t)));
    assert_eq!(rows, [(4, foo.clone()), (5, nullable.clone()), (6, nullable)]);

    for (body, exits) in [
        ("\\die(\"x\");", true),
        ("\\exit(1);", true),
        ("Lib\\die(\"x\");", false),
        ("namespace\\exit(1);", false),
    ] {
        let src = format!("<?php\nnamespace App;\nuse Lib\\Foo;\nfunction f($y) {{\n    if (!$y instanceof Foo) {body}\n    $y->run();\n}}\n");
        let fa = php(&src);
        let want = if exits { foo.clone() } else { None };
        assert_eq!(fa.inferred_type_via_bag("y", at(&src, "$y->run", 0)), want, "{body}");
    }
}

/// A constant and a variable spelled alike in one scope are two bindings:
/// each holds its own value, at its declaration's hover and at every read
/// — a read written above the constant's declaration included.
#[cfg(feature = "php")]
#[test]
fn a_constant_and_a_variable_of_one_name_hold_their_own_values() {
    let src: &'static str = "<?php\nnamespace App;\nfunction early() { return LIMIT; }\nconst LIMIT = 3;\n$LIMIT = 'x';\n$a = LIMIT;\n$b = $LIMIT;\n$c = \\App\\LIMIT;\n$d = $LIMIT;\necho $d;\n";
    let fa = php(src);
    let int = fa.inferred_type_via_bag("a", at(src, "$b", 0));
    let string = fa.inferred_type_via_bag("b", at(src, "$c", 0));
    assert!(int.as_ref().is_some_and(InferredType::is_known) && string.as_ref().is_some_and(InferredType::is_known));
    assert_ne!(int, string);
    assert_eq!(fa.inferred_type_via_bag("c", at(src, "$d", 0)), int, "a qualified read");
    assert_eq!(fa.inferred_type_via_bag("d", at(src, "echo $d", 0)), string, "the variable after the constant's reads");
    assert_eq!(fa.sub_return_type_at_arity("early", None), int, "a read above the declaration");

    let idx = php_indexed(&[("/fake/php/App/v.php", src)]);
    let constant = php_hover(&idx, "/fake/php/App/v.php", src, "LIMIT = 3", 0);
    let variable = php_hover(&idx, "/fake/php/App/v.php", src, "$LIMIT = 'x'", 1);
    assert!(constant.contains("int") && !constant.contains("string"), "{constant}");
    assert!(variable.contains("string") && !variable.contains("int"), "{variable}");
    let read = php_hover(&idx, "/fake/php/App/v.php", src, "$LIMIT;\n$c", 1);
    assert!(read.contains("string") && !read.contains("int"), "{read}");

    // `define` binds its value the same way, from inside a function too.
    let src = "<?php\nnamespace App;\nfunction boot() { define('MAX', 'x'); }\n$MAX = 1;\n$m = MAX;\necho $m;\n";
    let fa = php(src);
    assert_eq!(fa.inferred_type_via_bag("m", at(src, "echo $m", 0)), string);
}

/// A read of a constant another file declares holds that declaration's
/// value: `\Lib\LIMIT`, a `use const` row's leaf and a bare read in the
/// constant's namespace each, and a bare read its namespace declares
/// nothing for holds the global `define`'s.
#[cfg(feature = "php")]
#[test]
fn a_constant_read_holds_the_value_another_file_declares() {
    let lib = ("/fake/php/Lib/limits.php", "<?php\nnamespace Lib;\nconst LIMIT = 3;\n");
    let global = ("/fake/php/0/global.php", "<?php\ndefine('MAX', 'x');\n");
    let qualified = ("/fake/php/App/q.php", "<?php\nnamespace App;\n$a = \\Lib\\LIMIT;\n$m = MAX;\necho $a;\n");
    let imported = ("/fake/php/App/u.php", "<?php\nnamespace App;\nuse const Lib\\LIMIT;\n$a = LIMIT;\necho $a;\n");
    let bare = ("/fake/php/Lib/b.php", "<?php\nnamespace Lib;\n$a = LIMIT;\necho $a;\n");
    let idx = php_indexed(&[lib, global, qualified, imported, bare]);
    let ty = |(path, src): (&str, &str), var: &str| {
        use crate::build::language_driver::LanguageRegistry;
        use crate::model::file_analysis::{CrossFileLookup, ScopedLookup};
        let fa = idx.workspace_file_for_test(std::path::Path::new(path)).unwrap();
        let base: &dyn CrossFileLookup = &idx;
        let scoped = ScopedLookup::for_origin(base, &fa, None, LanguageRegistry::pack_visibility(&fa.language));
        fa.inferred_type_via_bag_ctx(var, crate::model::witnesses::ReadPoint::position(at(src, "echo", 0)), Some(&scoped))
    };
    let numeric = Some(InferredType::Numeric);
    assert_eq!(ty(qualified, "a"), numeric, "a qualified read");
    assert_eq!(ty(imported, "a"), numeric, "a read through `use const`");
    assert_eq!(ty(bare, "a"), numeric, "a bare read in the constant's namespace");
    assert_eq!(ty(qualified, "m"), Some(InferredType::String), "the global fallback");
    let hover = php_hover(&idx, qualified.0, qualified.1, "$a;", 1);
    assert!(hover.contains("int"), "hover reads the same value: {hover}");
    // the value is the declaring file's: this file alone states none
    assert_eq!(php(qualified.1).inferred_type_via_bag("a", at(qualified.1, "echo", 0)), None);
}

/// A parameter's declared type is checked when the call binds it, so it
/// bounds the local on entry only: a later write retypes it, and a guard
/// narrows within it. A property's declared type bounds the member for its
/// life.
#[cfg(feature = "php")]
#[test]
fn a_parameter_is_bounded_on_entry_and_a_property_for_life() {
    use crate::model::file_analysis::InferredType as T;
    let fa = php("<?php
class Logger { function log() {} }
class FileLogger extends Logger { function rotate() {} }
class Svc {
    private ?Logger $l = null;
    function __construct() { $this->l = new FileLogger(); }
    function f() { $x = $this->l; return $x; }
}
function g(int $n) { $n = \"s\"; return $n; }
function h(Logger $q) { $q = mk(); return $q; }
function k(Logger $q) { return $q; }
function m(Logger $q) { if ($q instanceof FileLogger) { return $q; } return $q; }
");
    let at = |v: &str, row, column| fa.inferred_type_via_bag(v, tree_sitter::Point { row, column });
    let logger = || T::ClassName("Logger".into());
    assert_eq!(at("x", 6, 42), Some(T::Optional(Box::new(logger()))), "a member reads as its declaration");
    assert_eq!(at("n", 8, 39), Some(T::String), "a write retires an entry bound");
    assert_ne!(at("q", 9, 43), Some(logger()), "even an untyped one");
    assert_eq!(at("q", 10, 32), Some(logger()), "unwritten, the parameter holds its declared type");
    assert_eq!(at("q", 11, 64), Some(T::ClassName("FileLogger".into())), "a guard narrows within the bound");
}

/// What a parameter takes is its declaration, asked where the call binds it:
/// a rebinding in the body moves what the body holds, not what callers pass.
/// The pack mints no `ParamInfo` yet (`TODO(pack-params)`), so the test
/// states the one a signature would carry: the parameter's token.
#[cfg(feature = "php")]
#[test]
fn a_parameter_takes_its_declared_type_whatever_the_body_rebinds() {
    use crate::model::file_analysis::{InferredType as T, ParamInfo, SymKind};
    let src = "<?php
function load(string $p) { $p = realpath($p); return file($p); }
";
    let fa = php(src);
    let token = at(src, "$p)", 1);
    let decl = fa.symbols().iter().find(|s| s.kind == SymKind::Variable && s.selection_span.start == token);
    assert!(decl.is_some(), "the parameter's declaration sits at its token");
    let param = ParamInfo { name: "p".into(), default: None, is_slurpy: false, is_invocant: false, binding_site: Some(token) };
    assert_eq!(fa.param_declared_type(&param, None), Some(T::String));
    assert_ne!(fa.inferred_type_via_bag("p", at(src, "return", 0)), Some(T::String), "the body holds the rebinding");
}

/// A guard on a class the declared type decidedly does not cover proves a
/// value that is both, so both classes' members dispatch inside it.
#[cfg(feature = "php")]
#[test]
fn a_guard_outside_the_declared_type_reads_as_both() {
    let src = "<?php
interface Sized { function count(); }
interface Logger { function info($m); }
function f(Sized $c) {
    if ($c instanceof Logger) { $c->count(); $c->info(''); }
}
";
    let fa = php(src);
    let t = fa.inferred_type_via_bag("c", at(src, "$c->count", 0)).expect("a type inside the guard");
    let classes: Vec<&str> = t.receiver_arms().iter().filter_map(|a| a.class_name()).collect();
    assert!(classes.contains(&"Sized") && classes.contains(&"Logger"), "{t:?}");
}

/// A guard naming a class nothing here declares is undecided, whether the
/// bound is a class, a nullable one or a union: the class may well
/// implement it, so the guard's class stands.
#[test]
fn a_guard_on_an_unseen_class_stands_under_a_nullable_bound() {
    let src = "<?php
interface Shape { function area(); }
function f(?Shape $c) {
    if ($c instanceof Square) { $c->side(); }
}
";
    let fa = php(src);
    let t = fa.inferred_type_via_bag("c", at(src, "$c->side", 0)).expect("a type inside the guard");
    assert_eq!(t, crate::model::file_analysis::InferredType::ClassName("Square".into()));
}
