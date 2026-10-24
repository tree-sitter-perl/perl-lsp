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
