//! The language root surface (`crate::model::roots`): what every class of
//! a language answers without declaring it.

use super::*;
use crate::model::file_analysis::{MemberKey, MethodResolution, SymKind as FaSymKind};

fn pack_fa(lang: &str, path: &str, src: &str) -> FileAnalysis {
    crate::build::language_driver::LanguageRegistry::compiled()
        .for_id(lang)
        .unwrap()
        .analyze_with_path(src, Some(std::path::Path::new(path)))
}

fn unresolved_methods(analysis: &FileAnalysis, idx: &crate::index::module_index::ModuleIndex) -> Vec<String> {
    collect_diagnostics(analysis, idx, Default::default())
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-method"))
        .map(|d| d.message)
        .collect()
}

fn point_of(src: &str, needle: &str) -> tree_sitter::Point {
    let i = src.find(needle).unwrap_or_else(|| panic!("`{needle}`"));
    let row = src[..i].matches('\n').count();
    tree_sitter::Point { row, column: i - src[..i].rfind('\n').map_or(0, |n| n + 1) }
}

fn class_named<'a>(fa: &'a FileAnalysis, leaf: &str) -> &'a str {
    &fa.symbols()
        .iter()
        .find(|s| s.kind == FaSymKind::Class && s.name.ends_with(leaf))
        .unwrap_or_else(|| panic!("class {leaf}"))
        .name
}

const PERL_UNIVERSAL: &str = "\
package Foo;
sub new { bless {}, shift }
sub go {}
package main;
my $x = Foo->new;
$x->can('go');
$x->isa('Foo');
Foo->DOES('Role');
Foo->VERSION;
$x->nope;
1;
";

#[test]
fn perl_universal_members_are_answered() {
    let fa = parse_analysis(PERL_UNIVERSAL);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let um = unresolved_methods(&fa, &idx);
    assert_eq!(um.len(), 1, "only `nope` misses: {um:?}");
    assert!(um[0].contains("'nope'"), "{um:?}");
    for m in ["can", "isa", "DOES", "VERSION"] {
        match fa.resolve_method_in_ancestors("Foo", m, Some(&idx)) {
            Some(MethodResolution::Root { class, member }) => assert_eq!((class.as_str(), member.as_str()), ("Foo", m)),
            other => panic!("{m}: the root answers, got {other:?}"),
        }
    }
}

#[test]
fn perl_universal_member_hovers_its_document() {
    let fa = parse_analysis(PERL_UNIVERSAL);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let text = fa
        .hover_info(point_of(PERL_UNIVERSAL, "can("), PERL_UNIVERSAL, Some(&idx))
        .expect("hover on `can`");
    assert!(text.contains("can($method)"), "{text}");
    assert!(text.contains("from UNIVERSAL"), "{text}");
    assert!(text.contains("UNIVERSAL::can"), "{text}");
}

#[test]
fn perl_completion_ends_with_the_root() {
    let fa = parse_analysis(PERL_UNIVERSAL);
    let labels: Vec<_> = fa.complete_methods_for_class("Foo", None).into_iter().map(|c| (c.label, c.sort_priority)).collect();
    for m in ["can", "isa", "DOES", "VERSION", "import", "unimport"] {
        assert!(labels.iter().any(|(l, _)| l == m), "{m} offered: {labels:?}");
    }
    let go = labels.iter().find(|(l, _)| l == "go").unwrap().1;
    let can = labels.iter().find(|(l, _)| l == "can").unwrap().1;
    assert!(can > go, "a root member ranks below the class's own");
}

#[test]
fn perl_class_generates_its_own_new() {
    let src = "use v5.38;\nuse experimental 'class';\nclass Point {\n    field $x :param;\n    method go { $x }\n}\nmy $p = Point->new(x => 1);\n$p->go;\n";
    let fa = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    match fa.resolve_method_in_ancestors("Point", "new", Some(&idx)) {
        Some(MethodResolution::Local { sym_id, .. }) => {
            assert_eq!(fa.symbol(sym_id).selection_span.start.row, 2, "lands on the class declaration");
        }
        other => panic!("Corinna's `new` is minted per class, got {other:?}"),
    }
    assert!(fa.complete_methods_for_class("Point", None).iter().any(|c| c.label == "new"));
    assert!(unresolved_methods(&fa, &idx).is_empty());
}

const PHP_PLAIN: &str = "<?php
class Plain {
    public function go() {}
}
$o = new Plain();
$o->go();
";

#[test]
fn php_construction_without_a_constructor_is_answered() {
    let fa = pack_fa("php", "/ws/plain.php", PHP_PLAIN);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    assert!(unresolved_methods(&fa, &idx).is_empty(), "{:?}", unresolved_methods(&fa, &idx));
    let plain = class_named(&fa, "Plain");
    match fa.resolve_constructor_in_ancestors(plain, Some(&idx)) {
        Some(MethodResolution::Root { member, .. }) => assert_eq!(member, "__construct"),
        other => panic!("the implicit constructor answers, got {other:?}"),
    }
    assert!(fa.constructor_of(plain, Some(&idx)).is_none(), "an implicit constructor is no rename identity");
}

#[test]
fn php_implicit_constructor_hovers_and_signs_as_empty() {
    let fa = pack_fa("php", "/ws/plain.php", PHP_PLAIN);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let plain = class_named(&fa, "Plain").to_string();
    let text = fa.root_member_hover(&plain, "__construct").expect("root hover");
    assert!(text.contains("implicit constructor of `"), "{text}");
    assert!(text.contains("no parameters"), "{text}");
    let sig = fa.member_signatures(&plain, MemberKey::Constructor, Some(&idx)).expect("a signature");
    assert_eq!(sig.name, "__construct");
    assert!(sig.params.is_empty(), "`()`: {:?}", sig.params);
}

#[test]
fn php_named_construct_asks_still_miss() {
    let src = "<?php
class Base {
    public function go() {}
}
class Child extends Base {
    public function make() { parent::__construct(); }
}
$o = new Base();
$o->__construct();
";
    let fa = pack_fa("php", "/ws/base.php", src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let base = class_named(&fa, "Base").to_string();
    let child = class_named(&fa, "Child").to_string();
    assert!(fa.resolve_method_in_ancestors(&base, "__construct", Some(&idx)).is_none());
    assert!(fa.resolve_super_method(&child, "__construct", Some(&idx)).is_none(), "parent::__construct() with none declared");
    assert!(fa.resolve_constructor_in_ancestors(&child, Some(&idx)).is_some(), "while `new Child` constructs");
    let um = unresolved_methods(&fa, &idx);
    assert!(um.iter().any(|m| m.contains("'__construct'")), "{um:?}");
}

#[test]
fn python_construction_runs_object_init() {
    let src = "class Foo:\n    def go(self):\n        pass\n\n\nclass Bar(object):\n    def go(self):\n        pass\n\n\nf = Foo()\n";
    let fa = pack_fa("python", "/ws/m.py", src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let foo = class_named(&fa, "Foo").to_string();
    match fa.resolve_constructor_in_ancestors(&foo, Some(&idx)) {
        Some(MethodResolution::Root { member, .. }) => assert_eq!(member, "__init__"),
        other => panic!("object.__init__ answers, got {other:?}"),
    }
    assert!(matches!(
        fa.resolve_method_in_ancestors(&foo, "__repr__", Some(&idx)),
        Some(MethodResolution::Root { .. })
    ));
    let bar = class_named(&fa, "Bar").to_string();
    assert!(!fa.class_has_unresolved_ancestor(&bar, Some(&idx)), "an explicit `object` base is the root, not a missing class");
}

#[test]
fn cpp_has_no_root() {
    let src = "class Foo {\npublic:\n    void go();\n};\nvoid f() {\n    Foo* p = new Foo();\n    p->go();\n}\n";
    let fa = pack_fa("cpp", "/ws/foo.cpp", src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let foo = class_named(&fa, "Foo").to_string();
    assert!(fa.resolve_method_in_ancestors(&foo, "can", Some(&idx)).is_none());
    assert!(fa.resolve_constructor_in_ancestors(&foo, Some(&idx)).is_none(), "C++ constructors are not inherited: no tail");
    assert!(crate::model::roots::installed().surface("cpp").is_none());
    assert!(unresolved_methods(&fa, &idx).is_empty());
}

#[test]
fn moo_new_resolves_through_its_base() {
    let src = "package Foo;\nuse Moo;\nhas x => (is => 'ro');\nsub go {}\npackage main;\nmy $f = Foo->new(x => 1);\n$f->go;\nFoo->nope;\n1;\n";
    let fa = parse_analysis(src);
    // Moo installed: `new` is Moo::Object's, and a real miss still fires.
    let idx = moo_installed_index();
    assert!(matches!(
        fa.resolve_method_in_ancestors("Foo", "new", Some(&idx)),
        Some(MethodResolution::CrossFile { ref class, .. }) if class == "Moo::Object"
    ));
    let um = unresolved_methods(&fa, &idx);
    assert_eq!(um.len(), 1, "{um:?}");
    assert!(um[0].contains("'nope'"), "{um:?}");
    // Moo uninstalled: the chain is incomplete, so nothing is guessed.
    let bare = crate::index::module_index::ModuleIndex::new_for_test();
    assert!(unresolved_methods(&fa, &bare).is_empty());
}

#[test]
fn perl_new_is_no_longer_silenced() {
    // A plain package with methods and no constructor anywhere in its MRO.
    let src = "package Foo;\nsub go {}\npackage main;\nFoo->new;\n1;\n";
    let fa = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let um = unresolved_methods(&fa, &idx);
    assert_eq!(um.len(), 1, "{um:?}");
    assert!(um[0].contains("'new'"), "{um:?}");
}
