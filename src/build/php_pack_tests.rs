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
    let rows: Vec<&str> = fa.pack.include_directives.iter().map(|(_, raw)| raw.as_str()).collect();
    assert_eq!(
        rows,
        [
            "Illuminate\\Support\\Collection",
            "\\Vendor\\Pkg\\Speaker",
            "App\\Contracts\\Listener",
            "App\\Contracts\\Logger",
        ]
    );
    let aliases: Vec<(&str, &str, &str)> =
        fa.pack.use_aliases.iter().map(|(a, ns, leaf)| (a.as_str(), ns.as_str(), leaf.as_str())).collect();
    assert_eq!(aliases, [("Talks", "Vendor\\Pkg", "Speaker"), ("Log", "App\\Contracts", "Logger")]);

    // The model reads the same map the extractor resolved parents through.
    assert_eq!(fa.class_spelling_identity("Collection"), "Illuminate\\Support\\Collection");
    assert_eq!(fa.class_spelling_identity("Talks"), "Vendor\\Pkg\\Speaker");
    assert_eq!(fa.class_spelling_identity("Log"), "App\\Contracts\\Logger");
    assert_eq!(fa.class_spelling_identity("Collection\\Item"), "Illuminate\\Support\\Collection\\Item");
    // an aliased row binds its alias, never its own leaf
    assert_eq!(fa.class_spelling_identity("Speaker"), "App\\Speaker");
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

/// A class's uses in another file are found by the identity they name:
/// goto-def from a parent clause lands on the declaration, and rename
/// rewrites the declaration, the import's leaf and the parent's leaf.
#[test]
fn a_class_renames_across_files_through_its_identity() {
    use crate::index::file_store::{FileKey, FileStore};
    use crate::index::resolve::{resolve, OverrideScope};
    use std::path::PathBuf;
    use std::sync::Arc;
    let src_model = "<?php\nnamespace Lib;\nclass Model {}\n";
    // `Other\Model` shares the leaf and must never be renamed with it.
    let src_user = "<?php\nnamespace App;\nuse Lib\\Model;\nclass User extends Model {}\nclass Twin extends Other\\Model {}\n";
    let (p_model, p_user) = (PathBuf::from("/fake/php/Lib/Model.php"), PathBuf::from("/fake/php/App/User.php"));

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_symbols(p_model.clone(), Arc::new(php(src_model)));
    let store = FileStore::new();
    store.insert_workspace(p_model.clone(), php(src_model));
    store.insert_workspace(p_user.clone(), php(src_user));

    let user = store.workspace_raw().get(&p_user).unwrap().clone();
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
