use super::*;

// ---- Inheritance extraction tests ----

#[test]
fn test_use_parent_single() {
    let fa = build_fa(
        "
            package Child;
            use parent 'Parent';
            sub child_method { }
        ",
    );
    assert_eq!(
        fa.declared_parents("Child"),
        &vec!["Parent".to_string()]
    );
}

#[test]
fn test_use_parent_multiple() {
    let fa = build_fa(
        "
            package Multi;
            use parent qw(Foo Bar);
        ",
    );
    assert_eq!(
        fa.declared_parents("Multi"),
        &vec!["Foo".to_string(), "Bar".to_string()]
    );
}

#[test]
fn test_use_parent_emits_package_refs() {
    let fa = build_fa(
        "
            package Child;
            use parent 'Parent';
        ",
    );
    let refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef) && r.target_name == "Parent")
        .collect();
    assert_eq!(refs.len(), 1, "should emit PackageRef for parent class");
}

#[test]
fn test_use_parent_qw_emits_package_refs() {
    let fa = build_fa(
        "
            package Multi;
            use parent qw(Foo Bar);
        ",
    );
    let foo_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef) && r.target_name == "Foo")
        .collect();
    let bar_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::PackageRef) && r.target_name == "Bar")
        .collect();
    assert_eq!(foo_refs.len(), 1, "should emit PackageRef for Foo");
    assert_eq!(bar_refs.len(), 1, "should emit PackageRef for Bar");
}

#[test]
fn test_use_parent_norequire() {
    let fa = build_fa(
        "
            package Local;
            use parent -norequire, 'My::Base';
        ",
    );
    assert_eq!(
        fa.declared_parents("Local"),
        &vec!["My::Base".to_string()]
    );
}

#[test]
fn test_use_base() {
    let fa = build_fa(
        "
            package Old;
            use base 'Legacy::Base';
        ",
    );
    assert_eq!(
        fa.declared_parents("Old"),
        &vec!["Legacy::Base".to_string()]
    );
}

#[test]
fn test_isa_assignment() {
    let fa = build_fa(
        "
            package Direct;
            our @ISA = ('Alpha', 'Beta');
        ",
    );
    assert_eq!(
        fa.declared_parents("Direct"),
        &vec!["Alpha".to_string(), "Beta".to_string()]
    );
}

#[test]
fn test_class_isa_populates_package_parents() {
    let fa = build_fa(
        "
            class Child :isa(Parent) { }
        ",
    );
    assert_eq!(
        fa.declared_parents("Child"),
        &vec!["Parent".to_string()]
    );
}

#[test]
fn test_class_does_populates_package_parents() {
    let fa = build_fa(
        "
            class MyClass :does(Printable) :does(Serializable) { }
        ",
    );
    let parents = fa.declared_parents("MyClass");
    assert!(parents.contains(&"Printable".to_string()));
    assert!(parents.contains(&"Serializable".to_string()));
}

#[test]
fn test_class_isa_and_does_combined() {
    let fa = build_fa(
        "
            class Child :isa(Parent) :does(Role) { }
        ",
    );
    let parents = fa.declared_parents("Child");
    assert_eq!(parents, &vec!["Parent".to_string(), "Role".to_string()]);
}

#[test]
fn test_with_role_populates_package_parents() {
    let fa = build_fa(
        "
            package MyApp;
            use Moo;
            with 'My::Role::Logging';
        ",
    );
    let parents = fa.declared_parents("MyApp");
    assert!(parents.contains(&"My::Role::Logging".to_string()));
}

#[test]
fn test_with_multiple_roles() {
    let fa = build_fa(
        "
            package MyApp;
            use Moose;
            with 'Role::A', 'Role::B';
        ",
    );
    let parents = fa.declared_parents("MyApp");
    assert!(parents.contains(&"Role::A".to_string()));
    assert!(parents.contains(&"Role::B".to_string()));
}

// --- E4a: MooX::Options `option` ---

/// `option 'name' => (is => 'ro', ...)` is a `has` with extra option-parsing
/// keys; it synthesizes the same accessor Method symbol + constructor HashKeyDef.
#[test]
fn test_moox_option_synthesizes_accessor_and_ctor_key() {
    let fa = build_fa(
        "
            package MyApp;
            use Moo;
            use MooX::Options;
            option 'verbose' => (is => 'ro', format => 'i', doc => 'noisy');
            option name => (is => 'rw', isa => 'Str');
        ",
    );
    // Accessor Method symbols.
    let methods: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == crate::model::file_analysis::SymKind::Method)
        .map(|s| s.name.as_str())
        .collect();
    assert!(methods.contains(&"verbose"), "ro accessor: {methods:?}");
    assert!(methods.contains(&"name"), "rw accessor: {methods:?}");

    // Constructor HashKeyDefs (`MyApp->new(verbose => ...)`).
    let ctor_keys: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| {
            matches!(
                &s.detail,
                crate::model::file_analysis::SymbolDetail::HashKeyDef {
                    owner: crate::model::file_analysis::HashKeyOwner::Sub { name, .. },
                    ..
                } if name == "new"
            )
        })
        .map(|s| s.name.as_str())
        .collect();
    assert!(ctor_keys.contains(&"verbose"), "ctor key verbose: {ctor_keys:?}");
    assert!(ctor_keys.contains(&"name"), "ctor key name: {ctor_keys:?}");
}

/// `option` outside a MooX::Options package must NOT synthesize accessors — an
/// unrelated `option(...)` sub call isn't an attribute declaration.
#[test]
fn test_option_without_moox_is_not_an_accessor() {
    let fa = build_fa(
        "
            package MyApp;
            use Moo;
            option 'verbose' => (is => 'ro');
        ",
    );
    let methods: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == crate::model::file_analysis::SymKind::Method)
        .map(|s| s.name.as_str())
        .collect();
    assert!(!methods.contains(&"verbose"), "no synthesis without MooX::Options: {methods:?}");
}

/// Regression: plain `has` in a package that also `use`s MooX::Options is
/// unaffected (still synthesizes via the shared path).
#[test]
fn test_moox_options_plain_has_still_works() {
    let fa = build_fa(
        "
            package MyApp;
            use Moo;
            use MooX::Options;
            has plain => (is => 'ro', isa => 'Int');
        ",
    );
    let methods: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == crate::model::file_analysis::SymKind::Method)
        .map(|s| s.name.as_str())
        .collect();
    assert!(methods.contains(&"plain"), "plain has accessor: {methods:?}");
}

// --- E4b: `with 'Role'` role-provided methods resolve cross-file ---

/// A class `with 'SomeRole'` should resolve `$self->m` to the role's `sub m`
/// cross-file. `with` already unifies into `package_parents`, and the
/// ancestor walk + cross-file module_index do the rest — this is a lock test.
#[test]
fn test_with_role_method_resolves_cross_file() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);

    // The role provides `log_it`.
    idx.insert_cache(
        "My::Role::Logging",
        Some(fake_cached_for_class(
            "My::Role::Logging",
            &PathBuf::from("/fake/My/Role/Logging.pm"),
            &["log_it"],
            &[],
        )),
    );

    let fa = build_fa(
        "
            package MyApp;
            use Moo;
            with 'My::Role::Logging';
            sub run { my $self = shift; }
        ",
    );

    // Completion surfaces the role method.
    let methods = fa.complete_methods_for_class("MyApp", Some(&idx));
    let names: Vec<&str> = methods.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"log_it"), "role method in completion: {names:?}");

    // resolve_method_in_ancestors finds it cross-file.
    let res = fa.resolve_method_in_ancestors("MyApp", "log_it", Some(&idx));
    assert!(
        matches!(res, Some(crate::model::file_analysis::MethodResolution::CrossFile { ref class, .. }) if class == "My::Role::Logging"),
        "expected CrossFile to the role, got {res:?}"
    );
}

#[test]
fn test_load_components_bare() {
    let fa = build_fa(
        "
            package MySchema::Result::User;
            use base 'DBIx::Class::Core';
            __PACKAGE__->load_components('InflateColumn::DateTime', 'TimeStamp');
        ",
    );
    let parents = fa.declared_parents("MySchema::Result::User");
    assert!(parents.contains(&"DBIx::Class::Core".to_string()));
    assert!(parents.contains(&"DBIx::Class::InflateColumn::DateTime".to_string()));
    assert!(parents.contains(&"DBIx::Class::TimeStamp".to_string()));
}

#[test]
fn test_load_components_plus_prefix() {
    let fa = build_fa(
        "
            package MySchema::Result::User;
            use base 'DBIx::Class::Core';
            __PACKAGE__->load_components('+My::Custom::Component');
        ",
    );
    let parents = fa.declared_parents("MySchema::Result::User");
    assert!(parents.contains(&"My::Custom::Component".to_string()));
}

#[test]
fn test_load_components_qw() {
    let fa = build_fa(
        "
            package MySchema::ResultSet::User;
            use base 'DBIx::Class::Core';
            __PACKAGE__->load_components(qw(Helper::ResultSet::Shortcut Helper::ResultSet::Me));
        ",
    );
    let parents = fa.declared_parents("MySchema::ResultSet::User");
    assert!(parents.contains(&"DBIx::Class::Helper::ResultSet::Shortcut".to_string()));
    assert!(parents.contains(&"DBIx::Class::Helper::ResultSet::Me".to_string()));
}

#[test]
fn test_load_own_components_prefixes_current_package() {
    // DBIC's `load_own_components` resolves bare names against the CURRENT
    // package's namespace, not `DBIx::Class::` — so `Relationship`'s
    // `load_own_components('CascadeActions')` pulls in
    // `DBIx::Class::Relationship::CascadeActions`. Without this the composed
    // mixin is invisible to method resolution / implementations (H7-7).
    let fa = build_fa(
        "
            package DBIx::Class::Relationship;
            use base 'DBIx::Class';
            __PACKAGE__->load_own_components(qw(Helpers CascadeActions Base));
        ",
    );
    let parents = fa.declared_parents("DBIx::Class::Relationship");
    assert!(parents.contains(&"DBIx::Class::Relationship::CascadeActions".to_string()));
    assert!(parents.contains(&"DBIx::Class::Relationship::Helpers".to_string()));
    assert!(parents.contains(&"DBIx::Class::Relationship::Base".to_string()));
}

#[test]
fn test_load_own_components_plus_prefix_is_fully_qualified() {
    let fa = build_fa(
        "
            package My::Component;
            __PACKAGE__->load_own_components('+Other::Ns::Thing', 'Local');
        ",
    );
    let parents = fa.declared_parents("My::Component");
    assert!(parents.contains(&"Other::Ns::Thing".to_string()));
    assert!(parents.contains(&"My::Component::Local".to_string()));
}

// ---- Inheritance method resolution tests ----

#[test]
fn test_inherited_method_completion() {
    let fa = build_fa(
        "
            package Animal;
            sub speak { }
            sub eat { }

            package Dog;
            use parent 'Animal';
            sub fetch { }
        ",
    );
    let methods = fa.complete_methods_for_class("Dog", None);
    let names: Vec<&str> = methods.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"fetch"), "own method");
    assert!(names.contains(&"speak"), "inherited from Animal");
    assert!(names.contains(&"eat"), "inherited from Animal");
}

#[test]
fn resolved_class_completion_excludes_anon_subs() {
    // `*__HM_DEDUP = sub () { 0 }` (DBIx::Class::ResultSet.pm) mints an
    // anonymous-sub symbol inside the package; a method-completion list on
    // the RESOLVED class must not offer it — no call can spell `(anon)`.
    let fa = build_fa(
        "
            package Widget;
            BEGIN { *__HM_DEDUP = sub () { 0 }; }
            sub spin { }
        ",
    );
    let methods = fa.complete_methods_for_class("Widget", None);
    let names: Vec<&str> = methods.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"spin"), "real method offered");
    assert!(
        !names.iter().any(|n| n.contains("(anon)")),
        "anonymous sub leaked into resolved-class completion: {names:?}"
    );
}

#[test]
fn test_child_method_overrides_parent() {
    let fa = build_fa(
        "
            package Base;
            sub greet { }

            package Override;
            use parent 'Base';
            sub greet { }
        ",
    );
    let methods = fa.complete_methods_for_class("Override", None);
    let greet_count = methods.iter().filter(|c| c.label == "greet").count();
    assert_eq!(greet_count, 1, "child override should shadow parent");
}

#[test]
fn test_find_method_in_parent() {
    let fa = build_fa(
        "
            package Base;
            sub base_method { }

            package Child;
            use parent 'Base';
        ",
    );
    let span = fa.find_method_in_class("Child", "base_method");
    assert!(span.is_some(), "should find inherited method");
}

#[test]
fn test_inherited_return_type() {
    let fa = build_fa(
        "
            package Factory;
            sub create { Factory->new(@_) }

            package SpecialFactory;
            use parent 'Factory';
        ",
    );
    let rt = fa.find_method_return_type("SpecialFactory", "create", None, None);
    assert!(rt.is_some(), "should find return type from parent");
}

#[test]
fn test_multi_level_inheritance() {
    let fa = build_fa(
        "
            package A;
            sub from_a { }

            package B;
            use parent 'A';
            sub from_b { }

            package C;
            use parent 'B';
            sub from_c { }
        ",
    );
    let methods = fa.complete_methods_for_class("C", None);
    let names: Vec<&str> = methods.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"from_a"));
    assert!(names.contains(&"from_b"));
    assert!(names.contains(&"from_c"));
}

#[test]
fn test_class_isa_inherits_methods() {
    let fa = build_fa(
        "
            class Parent {
                method greet() { }
            }
            class Child :isa(Parent) {
                method wave() { }
            }
        ",
    );
    let methods = fa.complete_methods_for_class("Child", None);
    let names: Vec<&str> = methods.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"wave"), "own method");
    assert!(names.contains(&"greet"), "inherited from Parent");
}

// ---- Cross-file inheritance tests ----

/// Build a CachedModule from a synthesized Perl source listing the given subs
/// (each as an `sub name { $self }` method) plus optional parent packages.
pub(super) fn fake_cached_for_class(
    package_name: &str,
    path: &std::path::Path,
    subs: &[&str],
    parents: &[&str],
) -> std::sync::Arc<crate::index::module_index::CachedModule> {
    let mut source = format!("package {};\n", package_name);
    if !parents.is_empty() {
        source.push_str(&format!("use parent '{}';\n", parents.join("', '")));
    }
    for sub in subs {
        source.push_str(&format!("sub {} {{ my $self = shift; }}\n", sub));
    }
    source.push_str("1;\n");
    let fa = build_fa(&source);
    std::sync::Arc::new(crate::index::module_index::CachedModule::new(
        path.to_path_buf(),
        std::sync::Arc::new(fa),
    ))
}

#[test]
fn test_cross_file_inherited_method_completion() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);

    // Grandparent: DBI has `connect`
    idx.insert_cache(
        "DBI",
        Some(fake_cached_for_class(
            "DBI",
            &PathBuf::from("/fake/DBI.pm"),
            &["connect"],
            &[],
        )),
    );

    // Parent: DBI::db inherits from DBI, has `prepare`
    idx.insert_cache(
        "DBI::db",
        Some(fake_cached_for_class(
            "DBI::db",
            &PathBuf::from("/fake/DBI/db.pm"),
            &["prepare"],
            &["DBI"],
        )),
    );

    // Local code inherits from DBI::db
    let fa = build_fa(
        "
            package MyDB;
            use parent 'DBI::db';
            sub custom_query { }
        ",
    );

    let methods = fa.complete_methods_for_class("MyDB", Some(&idx));
    let names: Vec<&str> = methods.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"custom_query"), "own method");
    assert!(names.contains(&"prepare"), "from DBI::db");
    assert!(names.contains(&"connect"), "from DBI (grandparent)");
}

#[test]
fn test_cross_file_method_override() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);

    // Parent has `process`
    idx.insert_cache(
        "Base::Worker",
        Some(fake_cached_for_class(
            "Base::Worker",
            &PathBuf::from("/fake/Base/Worker.pm"),
            &["process"],
            &[],
        )),
    );

    // Local child overrides `process`
    let fa = build_fa(
        "
            package MyWorker;
            use parent 'Base::Worker';
            sub process { }
        ",
    );

    let methods = fa.complete_methods_for_class("MyWorker", Some(&idx));
    let process_count = methods.iter().filter(|c| c.label == "process").count();
    assert_eq!(process_count, 1, "local override should shadow parent");
}

#[test]
fn test_cross_file_return_type_through_inheritance() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);

    // Parent module whose `fetch` returns a hashref with known keys.
    let source = r#"
package Fetcher;
sub fetch {
    my $self = shift;
    return { status => 1, body => 'ok' };
}
1;
"#;
    let fa_parent = build_fa(source);
    idx.insert_cache(
        "Fetcher",
        Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
            PathBuf::from("/fake/Fetcher.pm"),
            std::sync::Arc::new(fa_parent),
        ))),
    );

    let fa = build_fa(
        "
            package MyFetcher;
            use parent 'Fetcher';
        ",
    );

    let rt = fa.find_method_return_type("MyFetcher", "fetch", Some(&idx), None);
    assert!(rt.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_parents_cached() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);

    idx.insert_cache(
        "Child::Mod",
        Some(fake_cached_for_class(
            "Child::Mod",
            &PathBuf::from("/fake/Child/Mod.pm"),
            &[],
            &["Parent::Mod", "Mixin::Role"],
        )),
    );

    let parents = idx.parents_cached("Child::Mod");
    assert_eq!(parents, vec!["Parent::Mod", "Mixin::Role"]);
    assert!(idx.parents_cached("Unknown::Mod").is_empty());
}

#[test]
fn test_cross_bag_inheritance_cycle_does_not_overflow() {
    // A → B and B → A across separate cached bags. Each bag's
    // package_parents only knows the local-side edge of the loop,
    // so the cycle only closes once the inheritance fallback in
    // `query_rec` crosses bags. A per-bag-only visited set lets the
    // walk re-enter A's bag for `PackageSymbol{A, _}` after going
    // through B, then re-enter B for `PackageSymbol{B, _}`, ad
    // infinitum until the stack overflows. Visited must compose
    // (bag, attachment) so the loop closes.
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);

    idx.insert_cache(
        "Cycle::A",
        Some(fake_cached_for_class(
            "Cycle::A",
            &PathBuf::from("/fake/Cycle/A.pm"),
            &[],
            &["Cycle::B"],
        )),
    );
    idx.insert_cache(
        "Cycle::B",
        Some(fake_cached_for_class(
            "Cycle::B",
            &PathBuf::from("/fake/Cycle/B.pm"),
            &[],
            &["Cycle::A"],
        )),
    );

    let fa = build_fa("package main; 1;");

    assert_eq!(
        fa.find_method_return_type("Cycle::A", "no_such_method", Some(&idx), None),
        None,
    );
    assert_eq!(
        fa.find_method_return_type("Cycle::B", "no_such_method", Some(&idx), None),
        None,
    );
}

// ---- Method call return type propagation tests ----

#[test]
fn test_method_call_return_type_propagates() {
    let fa = build_fa(
        "
package Foo;
sub new { bless {}, shift }
sub get_config {
    return { host => 'localhost', port => 5432 };
}
package main;
my $f = Foo->new();
my $cfg = $f->get_config();
$cfg;
",
    );
    let ty = fa.inferred_type_via_bag("$cfg", Point::new(9, 0));
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_method_call_chain_propagation() {
    let fa = build_fa(
        "
package Foo;
sub new { bless {}, shift }
sub get_bar { return Bar->new() }
package Bar;
sub new { bless {}, shift }
sub get_name { return { name => 'test' } }
package main;
my $f = Foo->new();
my $bar = $f->get_bar();
my $name = $bar->get_name();
$name;
",
    );
    let bar_ty = fa.inferred_type_via_bag("$bar", Point::new(10, 0));
    assert_eq!(bar_ty, Some(InferredType::ClassName("Bar".into())));
    let name_ty = fa.inferred_type_via_bag("$name", Point::new(11, 0));
    assert!(name_ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_self_method_call_return_type() {
    let fa = build_fa(
        "
package Foo;
sub new { bless {}, shift }
sub get_config { return { host => 1 } }
sub run {
    my ($self) = @_;
    my $cfg = $self->get_config();
    $cfg;
}
",
    );
    let ty = fa.inferred_type_via_bag("$cfg", Point::new(7, 4));
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}
