use super::*;

// ---- Framework synthesis/detection: requires / Role::Tiny / DBIC ancestry /
//      mk_group_accessors / Mojo -base parent / has comma-form ----

#[test]
fn test_moo_role_requires_is_framework_import() {
    let fa = build_fa(
        "
package My::Role;
use Moo::Role;
requires 'must_implement';
",
    );
    assert!(
        fa.framework_imports.contains("requires"),
        "Moo::Role exports `requires` — should register as a framework import"
    );
}

#[test]
fn test_moose_role_requires_is_framework_import() {
    let fa = build_fa(
        "
package My::Role;
use Moose::Role;
requires 'foo';
",
    );
    assert!(fa.framework_imports.contains("requires"));
}

#[test]
fn test_role_tiny_behaves_like_moo_role() {
    let fa = build_fa(
        "
package My::Role;
use Role::Tiny;
requires 'bar';
with 'Other::Role';
",
    );
    assert!(
        fa.framework_imports.contains("requires"),
        "Role::Tiny exports `requires`"
    );
    assert!(
        fa.framework_imports.contains("with"),
        "Role::Tiny exports `with`"
    );
}

#[test]
fn test_role_tiny_with_behaves_like_moo_role() {
    let fa = build_fa(
        "
package My::Class;
use Role::Tiny::With;
with 'Some::Role';
",
    );
    assert!(fa.framework_imports.contains("with"));
}

#[test]
fn test_dbic_two_level_ancestry_synthesizes_columns() {
    // Result → BaseResult → DBIx::Class::Core: the DBIC base is two hops up.
    // The shallow direct-parent check missed this; full-ancestry walk catches it.
    let fa = build_fa(
        "
package My::Schema::BaseResult;
use base 'DBIx::Class::Core';

package My::Schema::Result::User;
use base 'My::Schema::BaseResult';
__PACKAGE__->add_columns(qw/id username/);
",
    );
    let id: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "id" && s.kind == SymKind::Method)
        .collect();
    let username: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "username" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(id.len(), 1, "2-level DBIC inheritance should synthesize `id`");
    assert_eq!(username.len(), 1, "and `username`");
}

#[test]
fn test_mk_group_accessors_synthesizes_methods() {
    let fa = build_fa(
        "
package My::Thing;
use base 'Class::Accessor::Grouped';
__PACKAGE__->mk_group_accessors('simple', qw/alpha beta/);
__PACKAGE__->mk_group_ro_accessors('inflated', 'gamma', 'delta');
",
    );
    for name in ["alpha", "beta", "gamma", "delta"] {
        let hits: Vec<_> = fa
            .symbols()
            .iter()
            .filter(|s| s.name == name && s.kind == SymKind::Method)
            .collect();
        assert_eq!(hits.len(), 1, "mk_group accessor `{name}` should be synthesized");
    }
    // The group name itself is NOT an accessor.
    assert!(
        !fa.symbols().iter().any(|s| s.name == "simple" && s.kind == SymKind::Method),
        "the leading group name must not become an accessor"
    );
}

#[test]
fn test_mk_classdata_synthesizes_method() {
    let fa = build_fa(
        "
package My::Thing;
use base 'Class::Accessor::Grouped';
__PACKAGE__->mk_classdata('config');
",
    );
    let hits: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "config" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(hits.len(), 1, "mk_classdata should synthesize the named accessor");
}

#[test]
fn test_use_module_dash_base_registers_parent_and_mojo_behavior() {
    let fa = build_fa(
        "
package My::Emitter;
use Mojo::EventEmitter -base;
has 'value';
",
    );
    // The module imported with -base becomes a parent...
    assert!(
        fa.declared_parents("My::Emitter").iter().any(|p| p == "Mojo::EventEmitter"),
        "`use X -base` should register X as a parent"
    );
    // ...and Mojo::Base accessor synthesis (getter + setter) applies.
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "value" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 2, "`-base` pulls Mojo::Base has-synthesis");
}

#[test]
fn test_mojo_base_dash_base_carries_mojo_base_as_parent() {
    let fa = build_fa(
        "
package My::Class;
use Mojo::Base -base;
has 'x';
",
    );
    assert!(
        fa.declared_parents("My::Class").iter().any(|p| p == "Mojo::Base"),
        "`Mojo::Base -base` should carry Mojo::Base itself as a parent so tap/attr/new resolve"
    );
}

#[test]
fn test_moo_has_comma_form_synthesizes_accessor() {
    // The comma-separated option form (not the fat-arrow `name => (...)` form).
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'name', is => 'ro', default => sub { 1 };
has age => (is => 'rw');
",
    );
    let name_acc: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(
        name_acc.len(),
        1,
        "comma-form `has 'name', is => 'ro'` should synthesize a `name` accessor"
    );
    // The fat-arrow form on the next line still works (no regression).
    // `is => 'rw'` synthesizes a getter + a writer (2 symbols named `age`).
    let age_acc: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "age" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(age_acc.len(), 2, "fat-arrow rw form still synthesizes getter+setter");
    // `is`/`default` must not become phantom accessors.
    assert!(
        !fa.symbols().iter().any(|s| (s.name == "is" || s.name == "ro") && s.kind == SymKind::Method),
        "option keywords/values must not mint phantom methods in comma form"
    );
}

// ---- typeglob sub installation (CG-1) ----

fn has_sub(fa: &FileAnalysis, name: &str) -> bool {
    fa.symbols()
        .iter()
        .any(|s| s.name == name && s.kind == SymKind::Sub)
}

#[test]
fn glob_static_name_sub() {
    let fa = build_fa("*greet = sub { return 'hi' };\n");
    assert!(has_sub(&fa, "greet"), "static *name = sub {{...}} must mint a Sub symbol");
}

#[test]
fn glob_alias_to_existing_sub() {
    let fa = build_fa("*alias = \\&Other::func;\n*local_alias = \\&real;\n");
    assert!(has_sub(&fa, "alias"), "*name = \\&Other::func glob alias must mint a Sub symbol");
    assert!(has_sub(&fa, "local_alias"), "*name = \\&func glob alias must mint a Sub symbol");
}

#[test]
fn glob_qualified_name_installs_tail() {
    // `*Other::foo = sub {...}` installs `foo` into Other; the unqualified
    // tail is what local call sites / nav use.
    let fa = build_fa("*Other::foo = sub { 1 };\n");
    assert!(has_sub(&fa, "foo"), "qualified glob must register the unqualified tail");
    assert!(!has_sub(&fa, "Other::foo"), "must not register the fully-qualified string as a name");
}

#[test]
fn glob_loop_over_qw() {
    let src = "for my $m (qw/red green blue/) {\n  no strict 'refs';\n  *$m = sub { 1 };\n}\n";
    let fa = build_fa(src);
    for name in ["red", "green", "blue"] {
        assert!(has_sub(&fa, name), "loop-installed glob `{name}` must mint a Sub symbol");
    }
}

#[test]
fn glob_begin_constant_style() {
    let src = "BEGIN {\n  *_FORCE_WRITABLE = sub () { 1 };\n}\n";
    let fa = build_fa(src);
    assert!(
        has_sub(&fa, "_FORCE_WRITABLE"),
        "constant-style glob sub in BEGIN must mint a Sub symbol"
    );
}

#[test]
fn glob_literal_block_name() {
    let fa = build_fa("*{ 'is_thing' } = sub { 1 };\n");
    assert!(has_sub(&fa, "is_thing"), "`*{{ 'literal' }}` glob must mint a Sub symbol");
}

#[test]
fn glob_scalar_rhs_coderef() {
    let fa = build_fa("*handler = $coderef;\n");
    assert!(has_sub(&fa, "handler"), "*name = $coderef must mint a Sub symbol");
}

#[test]
fn glob_dynamic_name_skipped() {
    // Fully runtime name — no static derivation, must NOT fabricate a symbol.
    let fa = build_fa("*{ $runtime } = sub { 1 };\n");
    // The anon `sub {...}` RHS mints an `(anon)` Sub symbol; the glob install
    // itself must add no named Sub.
    assert!(
        !fa.symbols().iter().any(|s| s.kind == SymKind::Sub && s.name != "(anon)"),
        "fully-dynamic glob name must be skipped, not guessed"
    );
}

#[test]
fn glob_unfoldable_concat_skipped() {
    // `'is_' . $type` with an unknown $type is not derivable → skip.
    let fa = build_fa("*{ 'is_' . $type } = sub { 1 };\n");
    assert!(
        !fa.symbols().iter().any(|s| s.kind == SymKind::Sub && s.name.starts_with("is_")),
        "unfoldable concat name must be skipped, not guessed with a partial prefix"
    );
}

#[test]
fn glob_concat_with_loop_var_foldable() {
    // `'is_' . $kind` where $kind ranges over a qw list → derivable names.
    let src =
        "for my $kind (qw/foo bar/) {\n  *{ 'is_' . $kind } = sub { 1 };\n}\n";
    let fa = build_fa(src);
    assert!(has_sub(&fa, "is_foo"), "foldable concat over loop var must mint is_foo");
    assert!(has_sub(&fa, "is_bar"), "foldable concat over loop var must mint is_bar");
}

#[test]
fn normal_assignment_unaffected() {
    // Regression guard: a plain scalar assignment must not mint a Sub symbol,
    // and `my $x = sub {...}` is a lexical coderef, not a glob install.
    let fa = build_fa("my $x = 42;\nmy $cb = sub { 1 };\n");
    assert!(
        !fa.symbols().iter().any(|s| s.name == "x" && s.kind == SymKind::Sub),
        "plain scalar assignment must not mint a Sub symbol"
    );
    assert!(
        !fa.symbols().iter().any(|s| s.name == "cb" && s.kind == SymKind::Sub),
        "lexical `my $cb = sub {{...}}` must not be treated as a glob install"
    );
}

// ---- CG-3a: glob loop over a local literal-returning sub ----

#[test]
fn glob_loop_over_local_qw_sub() {
    // CGI.pm shape: the loop source is a same-file sub returning a qw list.
    // Each installed glob name must mint a Sub symbol.
    let src = "\
foreach my $tag (_all_html_tags()) {
  no strict 'refs';
  *$tag = sub { 1 };
}
sub _all_html_tags { return qw(div span br); }
";
    let fa = build_fa(src);
    for name in ["div", "span", "br"] {
        assert!(has_sub(&fa, name), "loop over local qw-returning sub must mint `{name}`");
    }
}

#[test]
fn glob_loop_over_local_list_sub() {
    // Same, but the local sub returns a bare parenthesized string list
    // (no `qw`, no explicit `return`).
    let src = "\
for my $m (_names()) {
  *$m = sub { 1 };
}
sub _names { ('alpha', 'beta') }
";
    let fa = build_fa(src);
    assert!(has_sub(&fa, "alpha"), "loop over local list-returning sub must mint alpha");
    assert!(has_sub(&fa, "beta"), "loop over local list-returning sub must mint beta");
}

#[test]
fn glob_loop_over_nonliteral_local_sub_skipped() {
    // The local sub's body is computed (not a literal list) → fold yields
    // nothing, loop var stays dynamic, no fabricated symbols.
    let src = "\
for my $m (_dynamic()) {
  *$m = sub { 1 };
}
sub _dynamic { return map { lc } @ARGV; }
";
    let fa = build_fa(src);
    assert!(
        !fa.symbols().iter().any(|s| s.kind == SymKind::Sub && s.name != "(anon)" && s.name != "_dynamic"),
        "non-literal local sub return must not synthesize glob names"
    );
}

#[test]
fn glob_loop_over_unknown_sub_skipped() {
    // Cross-file / undefined callee — no same-file body to fold. Skip.
    let src = "\
for my $m (Some::Other::tags()) {
  *$m = sub { 1 };
}
";
    let fa = build_fa(src);
    assert!(
        !fa.symbols().iter().any(|s| s.kind == SymKind::Sub && s.name != "(anon)"),
        "unresolvable loop-source sub must not synthesize glob names"
    );
}

// ---- CG-3b: cross-package glob injection via ->can ----

#[test]
fn glob_loop_can_rhs_synthesizes_under_current_pkg() {
    // DateTime::PP shape. `__PACKAGE__->can($sub)` is recognized as a
    // sub-producing RHS, and the symbol is minted under the unqualified TAIL
    // — never the fully-qualified string, which is what call sites and nav
    // look up. Which PACKAGE the tail is attributed to is a separate axis and
    // is pinned by `cross_package_glob_synthesizes_under_target_package`;
    // this test deliberately declares no package, so there is no target to
    // attribute to and only the tail is in question.
    let src = "\
for my $sub (qw/foo bar/) {
  *{ 'DateTime::' . $sub } = __PACKAGE__->can($sub);
}
";
    let fa = build_fa(src);
    assert!(has_sub(&fa, "foo"), "->can RHS over loop var must mint foo (tail)");
    assert!(has_sub(&fa, "bar"), "->can RHS over loop var must mint bar (tail)");
    assert!(!has_sub(&fa, "DateTime::foo"), "must not register the fully-qualified name");
}

#[test]
fn glob_can_on_package_invocant() {
    // `Pkg->can('name')` static target also qualifies as sub-producing.
    let fa = build_fa("*alias = Foo::Bar->can('helper');\n");
    assert!(has_sub(&fa, "alias"), "*name = Pkg->can(...) must mint a Sub symbol");
}

#[test]
fn glob_non_can_method_rhs_skipped() {
    // A method call that isn't `->can` is not known to yield a coderef → skip.
    let fa = build_fa("*thing = $obj->build_something();\n");
    assert!(!has_sub(&fa, "thing"), "non-can method RHS must not mint a Sub symbol");
}

// ---- mk_classdata in a statement-modifier loop ----

fn count_method(fa: &FileAnalysis, name: &str) -> usize {
    fa.symbols().iter().filter(|s| s.name == name && s.kind == SymKind::Method).count()
}

#[test]
fn mk_classdata_postfix_for_qw() {
    // Catalyst.pm:176 shape.
    let fa = build_fa(
        "\
package My::App;
use base 'Class::Accessor::Grouped';
__PACKAGE__->mk_classdata($_) for qw/setup_finished params/;
",
    );
    assert_eq!(count_method(&fa, "setup_finished"), 1, "loop mk_classdata must mint setup_finished once");
    assert_eq!(count_method(&fa, "params"), 1, "loop mk_classdata must mint params once");
}

#[test]
fn mk_classdata_postfix_for_list() {
    // `mk_classdata($_) for (LIST)` bare-call form (Controller.pm:123).
    let fa = build_fa(
        "\
package My::Controller;
use base 'Class::Accessor::Grouped';
mk_classdata($_) for ('action_namespace', 'path_prefix');
",
    );
    assert_eq!(count_method(&fa, "action_namespace"), 1, "bare-call loop must mint action_namespace");
    assert_eq!(count_method(&fa, "path_prefix"), 1, "bare-call loop must mint path_prefix");
}

#[test]
fn mk_classdata_postfix_for_nonliteral_skipped() {
    // Loop over an array variable → no literal names → no synthesis.
    let fa = build_fa(
        "\
package My::App;
use base 'Class::Accessor::Grouped';
__PACKAGE__->mk_classdata($_) for @dynamic_names;
",
    );
    assert!(
        !fa.symbols().iter().any(|s| s.kind == SymKind::Method),
        "non-literal loop list must not synthesize accessors"
    );
}

#[test]
fn postfix_for_non_accessor_call_synthesizes_nothing() {
    // Regression: a statement-modifier loop whose body is an unrelated call
    // must not mint any accessor symbol.
    let fa = build_fa(
        "\
package My::App;
print(\"$_\\n\") for qw/a b c/;
",
    );
    assert!(
        !fa.symbols().iter().any(|s| s.kind == SymKind::Method),
        "non-accessor postfix-for loop must not synthesize accessors"
    );
}

// ---- Class::Tiny accessor synthesis (CG-2) ----

#[test]
fn test_class_tiny_list_form_synthesizes_accessors() {
    let fa = build_fa(
        "
package Foo;
use Class::Tiny qw( resolvers cache );
",
    );
    for attr in ["resolvers", "cache"] {
        let acc: Vec<_> = fa
            .symbols()
            .iter()
            .filter(|s| s.name == attr && s.kind == SymKind::Method)
            .collect();
        assert_eq!(
            acc.len(),
            1,
            "Class::Tiny qw list should synthesize one rw accessor for `{attr}`"
        );
        // Constructor key so `Foo->new(resolvers => ...)` connects.
        let key_def: Vec<_> = fa
            .symbols()
            .iter()
            .filter(|s| s.name == attr && matches!(s.detail, SymbolDetail::HashKeyDef { .. }))
            .collect();
        assert!(
            !key_def.is_empty(),
            "Class::Tiny attr `{attr}` should mint a constructor HashKeyDef"
        );
        if let SymbolDetail::HashKeyDef { ref owner, .. } = key_def[0].detail {
            assert_eq!(
                owner,
                &HashKeyOwner::Sub {
                    package: Some("Foo".to_string()),
                    name: "new".to_string(),
                }
            );
        }
    }
}

#[test]
fn test_class_tiny_hashref_form_synthesizes_accessors_from_keys() {
    let fa = build_fa(
        "
package Foo;
use Class::Tiny {
  name => 'default',
  builder => sub { [] },
};
",
    );
    // Keys are accessors; default values (string / coderef) are NOT.
    for attr in ["name", "builder"] {
        let acc: Vec<_> = fa
            .symbols()
            .iter()
            .filter(|s| s.name == attr && s.kind == SymKind::Method)
            .collect();
        assert_eq!(
            acc.len(),
            1,
            "Class::Tiny hashref key `{attr}` should synthesize an accessor"
        );
    }
    // The default value `'default'` must not become an accessor.
    assert!(
        !fa.symbols()
            .iter()
            .any(|s| s.name == "default" && s.kind == SymKind::Method),
        "hashref default value must not mint a phantom accessor"
    );
}

#[test]
fn test_class_tiny_combined_list_and_hashref() {
    // `use Class::Tiny qw( ssn ), { name => undef };` — both shapes on one line.
    let fa = build_fa(
        "
package Foo;
use Class::Tiny qw( ssn ), { name => undef };
",
    );
    for attr in ["ssn", "name"] {
        assert!(
            fa.symbols()
                .iter()
                .any(|s| s.name == attr && s.kind == SymKind::Method),
            "combined qw+hashref form should synthesize accessor `{attr}`"
        );
    }
}

#[test]
fn test_non_class_tiny_use_unaffected() {
    // Regression: an unrelated `use X qw(...)` must NOT synthesize accessors.
    let fa = build_fa(
        "
package Foo;
use List::Util qw( max min );
",
    );
    assert!(
        !fa.symbols()
            .iter()
            .any(|s| (s.name == "max" || s.name == "min") && s.kind == SymKind::Method),
        "non-Class::Tiny use must not synthesize accessor methods"
    );
}

// ── Task A: rule #7 ref-emission for use-constant usages + export-list members ──

/// `use constant NAME => ...` usage sites (plain expr + call arg) each get a
/// FunctionCall ref back to the constant def, so goto-def and references work.
#[test]
fn const_usage_name_form_emits_function_call_ref() {
    let src = r#"
package QA::C;
use constant MAX_RETRIES => 5;
sub retry {
    my $limit = MAX_RETRIES;
    return _attempt($limit, MAX_RETRIES);
}
sub _attempt { return 1 }
"#;
    let fa = build_fa(src);
    let usages: Vec<&Ref> = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "MAX_RETRIES"
                && matches!(&r.kind, RefKind::FunctionCall)
                    && r.resolved_package() == Some("QA::C")
        })
        .collect();
    assert_eq!(
        usages.len(),
        2,
        "both MAX_RETRIES usages (plain + call-arg) should ref the const def; got {:?}",
        fa.refs()
            .iter()
            .filter(|r| r.target_name == "MAX_RETRIES")
            .collect::<Vec<_>>()
    );
}

/// Block form `use constant { TIMEOUT => 30, BACKOFF => 2 }` — usages of a
/// block-declared constant get the same FunctionCall ref.
#[test]
fn const_usage_block_form_emits_function_call_ref() {
    let src = r#"
package QA::C;
use constant {
    TIMEOUT => 30,
    BACKOFF => 2,
};
sub run {
    my $t = TIMEOUT;
    return $t + BACKOFF;
}
"#;
    let fa = build_fa(src);
    for name in ["TIMEOUT", "BACKOFF"] {
        let n = fa
            .refs()
            .iter()
            .filter(|r| {
                r.target_name == name
                    && matches!(&r.kind, RefKind::FunctionCall { .. })
            })
            .count();
        assert_eq!(n, 1, "{name} usage should ref the block-form const def");
    }
}

/// goto-def from a constant usage lands on the const def via `ref_at` +
/// `refs_to`; references on the const lists the def + every usage.
#[test]
fn const_usage_goto_def_and_references() {
    use crate::index::file_store::FileStore;
    use crate::index::resolve::{refs_to, RoleMask, TargetKind, TargetRef};
    use std::path::PathBuf;

    let src = r#"package QA::C;
use constant MAX_RETRIES => 5;
sub retry {
    my $limit = MAX_RETRIES;
    return MAX_RETRIES;
}
"#;
    let fa = build_fa(src);

    // ref_at the first usage (`my $limit = MAX_RETRIES;`) is a FunctionCall
    // ref naming the const — that's the goto-def routing token.
    let usage_pt = Point::new(3, 16); // inside MAX_RETRIES on the `my $limit` line
    let r = fa
        .ref_at(usage_pt)
        .expect("a ref should sit on the constant usage");
    assert_eq!(r.target_name, "MAX_RETRIES");
    assert!(matches!(r.kind, RefKind::FunctionCall { .. }));

    let store = FileStore::new();
    let path = PathBuf::from("/tmp/qa_const.pm");
    store.insert_workspace(path.clone(), fa);

    let results = refs_to(
        &store,
        None,
        &TargetRef {
            name: "MAX_RETRIES".to_string(),
            kind: TargetKind::Sub {
                package: Some("QA::C".to_string()),
            },
            method_classes: Vec::new(), scope: crate::index::resolve::OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
        },
        RoleMask::EDITABLE,
    );
    // def + 2 usages = 3 hits.
    assert_eq!(
        results.len(),
        3,
        "references on MAX_RETRIES should list the def and both usages; got {results:?}"
    );
}

/// Regression: a bareword that is NOT a declared constant gets no spurious
/// constant-usage ref.
#[test]
fn non_constant_bareword_gets_no_const_ref() {
    let src = r#"
package QA::C;
use constant MAX_RETRIES => 5;
sub run {
    my $x = SOME_OTHER;
    return $x;
}
"#;
    let fa = build_fa(src);
    assert!(
        !fa.refs()
            .iter()
            .any(|r| r.target_name == "SOME_OTHER"),
        "a non-constant bareword must not get a constant-usage ref"
    );
}

/// `@EXPORT` / `@EXPORT_OK` / `%EXPORT_TAGS` member tokens that name a local
/// sub each get a FunctionCall ref to that sub (forward-declared subs work).
#[test]
fn export_list_members_ref_local_subs() {
    let src = r#"
package QA::E;
use Exporter 'import';
our @EXPORT      = qw(always_on);
our @EXPORT_OK   = qw(opt_a opt_b opt_c);
our %EXPORT_TAGS = (
    group_one => [qw(opt_a opt_b)],
    group_two => [qw(opt_c)],
);
sub always_on { 1 }
sub opt_a { 'a' }
sub opt_b { 'b' }
sub opt_c { 'c' }
"#;
    let fa = build_fa(src);
    let count = |name: &str| {
        fa.refs()
            .iter()
            .filter(|r| {
                r.target_name == name
                    && matches!(&r.kind, RefKind::FunctionCall)
                    && r.resolved_package() == Some("QA::E")
            })
            .count()
    };
    // always_on: 1 (@EXPORT). opt_a: @EXPORT_OK + %EXPORT_TAGS group_one = 2.
    // opt_b: @EXPORT_OK + group_one = 2. opt_c: @EXPORT_OK + group_two = 2.
    assert_eq!(count("always_on"), 1, "@EXPORT member should ref its sub");
    assert_eq!(count("opt_a"), 2, "opt_a appears in @EXPORT_OK and a tag array");
    assert_eq!(count("opt_b"), 2, "opt_b appears in @EXPORT_OK and a tag array");
    assert_eq!(count("opt_c"), 2, "opt_c appears in @EXPORT_OK and a tag array");
}

/// goto-def / references on an export-list member resolve to the sub def.
#[test]
fn export_member_goto_def_and_references() {
    use crate::index::file_store::FileStore;
    use crate::index::resolve::{refs_to, RoleMask, TargetKind, TargetRef};
    use std::path::PathBuf;

    let src = r#"package QA::E;
use Exporter 'import';
our @EXPORT_OK = qw(opt_a opt_b);
sub opt_a { 'a' }
sub opt_b { 'b' }
"#;
    let fa = build_fa(src);

    // ref_at the `opt_a` token in the export list.
    let opt_a_def_span = fa
        .symbols()
        .iter()
        .find(|s| s.name == "opt_a")
        .map(|s| s.selection_span)
        .expect("opt_a sub symbol");
    // The export-list member ref must NOT be the def span itself.
    let export_ref = fa
        .refs()
        .iter()
        .find(|r| {
            r.target_name == "opt_a"
                && matches!(&r.kind, RefKind::FunctionCall { .. })
                && r.span != opt_a_def_span
        })
        .expect("an export-list FunctionCall ref for opt_a");
    let r = fa
        .ref_at(export_ref.span.start)
        .expect("ref_at the export member token");
    assert_eq!(r.target_name, "opt_a");

    let store = FileStore::new();
    let path = PathBuf::from("/tmp/qa_export.pm");
    store.insert_workspace(path.clone(), fa);
    let results = refs_to(
        &store,
        None,
        &TargetRef {
            name: "opt_a".to_string(),
            kind: TargetKind::Sub {
                package: Some("QA::E".to_string()),
            },
            method_classes: Vec::new(), scope: crate::index::resolve::OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
        },
        RoleMask::EDITABLE,
    );
    // def + 1 export-list mention = 2.
    assert_eq!(
        results.len(),
        2,
        "references on opt_a should list the def and its @EXPORT_OK mention; got {results:?}"
    );
}

/// Regression: a `%EXPORT_TAGS` tag-NAME key (`group_one`) is NOT a sub, so it
/// gets no ref even though it sits in the export table.
#[test]
fn export_tag_name_key_gets_no_ref() {
    let src = r#"
package QA::E;
use Exporter 'import';
our %EXPORT_TAGS = (
    group_one => [qw(opt_a)],
);
sub opt_a { 'a' }
sub group_one { 'not a tag' }
"#;
    let fa = build_fa(src);
    // The fixture defines a sub literally named `group_one` to make the test
    // sharp: if the tag-name key were (wrongly) recorded as a member, it would
    // resolve to this sub. The key must still get no ref — only the value-array
    // member `opt_a` does.
    let group_one_refs = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "group_one"
                && matches!(&r.kind, RefKind::FunctionCall { .. })
        })
        .count();
    assert_eq!(
        group_one_refs, 0,
        "a tag-name key must not be reffed even when a same-named sub exists"
    );
}

/// Package-qualified `@Pkg::EXPORT` / `@Pkg::EXPORT_OK` / `%Pkg::EXPORT_TAGS`
/// (Bugzilla's form) must populate the export surface exactly like the
/// `our @EXPORT` spelling. Without this, `use Bugzilla::Util;` resolves nothing
/// (the 1000+ Bugzilla FP cluster).
#[test]
fn qualified_export_globals_populate_surface() {
    let src = r#"
package Bugzilla::Util;
@Bugzilla::Util::EXPORT = qw(trick_taint detaint_natural);
@Bugzilla::Util::EXPORT_OK = qw(opt_util);
%Bugzilla::Util::EXPORT_TAGS = (all => [qw(trick_taint opt_util)]);
sub trick_taint { 1 }
sub detaint_natural { 2 }
sub opt_util { 3 }
"#;
    let fa = build_fa(src);
    assert!(
        fa.export.contains(&"trick_taint".to_string())
            && fa.export.contains(&"detaint_natural".to_string()),
        "qualified @Pkg::EXPORT must populate the default set; got export={:?}",
        fa.export,
    );
    assert!(
        fa.export_ok.contains(&"opt_util".to_string()),
        "qualified @Pkg::EXPORT_OK must populate the optional set; got export_ok={:?}",
        fa.export_ok,
    );
    // Tag membership is preserved per-tag for the `:tag` consumer selector.
    let surface = fa.export_surface();
    let all = surface.tag_members("all").expect("all tag present");
    assert!(
        all.contains(&"trick_taint") && all.contains(&"opt_util"),
        "qualified %Pkg::EXPORT_TAGS must record per-tag members; got {:?}",
        all,
    );
    // :DEFAULT is synthesized from @EXPORT.
    let default = surface.tag_members("DEFAULT").expect(":DEFAULT synthesized");
    assert!(
        default.contains(&"trick_taint") && default.contains(&"detaint_natural"),
        ":DEFAULT must equal @EXPORT; got {:?}",
        default,
    );
}

/// `%EXPORT_TAGS = ( all => [...] )` and the plain-comma `( 'all', [...] )`
/// fold identically — `=>` is just an autoquoting comma, so the tag key/value
/// pairing is positional. A `:all` import must bind the folded members in both
/// spellings.
#[test]
fn export_tags_plain_comma_folds_members() {
    for table in [
        "( all => [qw(foo bar)] )",
        "( 'all', [qw(foo bar)] )",
    ] {
        let src = format!(
            "package P;\nour %EXPORT_TAGS = {table};\nsub foo {{ 1 }}\nsub bar {{ 2 }}\n",
        );
        let fa = build_fa(&src);
        let surface = fa.export_surface();
        let all = surface
            .tag_members("all")
            .unwrap_or_else(|| panic!("`all` tag must fold for table `{table}`"));
        assert!(
            all.contains(&"foo") && all.contains(&"bar"),
            "table `{table}`: :all members foo+bar must fold; got {:?}",
            all,
        );
        assert!(
            fa.export_ok.contains(&"foo".to_string()),
            "table `{table}`: tag members join the export surface; got export_ok={:?}",
            fa.export_ok,
        );
    }
}

/// A constant invoked as a call (`MAX_RETRIES()`) is reffed once by the
/// function-call path; the bareword arm must not double-emit at that span.
#[test]
fn const_call_form_not_double_reffed() {
    let src = r#"
package QA::C;
use constant MAX_RETRIES => 5;
sub run { return MAX_RETRIES(); }
"#;
    let fa = build_fa(src);
    let n = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "MAX_RETRIES" && matches!(&r.kind, RefKind::FunctionCall { .. })
        })
        .count();
    assert_eq!(n, 1, "MAX_RETRIES() call must get exactly one FunctionCall ref");
}

/// AutoLoader-backed package: subs after `__END__` live in the opaque
/// `data_section`, but they are runtime-live via AUTOLOAD. They must surface
/// as navigable Sub symbols with file-offset spans, with POD between them
/// skipped, and goto-def from an in-package caller must reach them.
#[test]
fn autoloader_data_section_subs_synthesized() {
    let src = "package My::AL;\n\
               use AutoLoader qw(AUTOLOAD);\n\
               sub uses_them { want_read(); }\n\
               1;\n\
               __END__\n\
               sub want_read { return 42 }\n\
               sub get_https { do_httpx2(GET => 1, @_) }\n\
               =pod\n\
               junk\n\
               =cut\n\
               sub after_pod ($;$) { return 1 }\n";
    let fa = build_fa(src);

    let names: std::collections::HashSet<&str> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Sub)
        .map(|s| s.name.as_str())
        .collect();
    assert!(names.contains("want_read"), "want_read must be synthesized");
    assert!(names.contains("get_https"), "get_https must be synthesized");
    assert!(names.contains("after_pod"), "sub after POD must be synthesized");

    // Spans land in the data section (row 5 = first sub after __END__).
    let want_read = fa
        .symbols()
        .iter()
        .find(|s| s.name == "want_read" && s.kind == SymKind::Sub)
        .expect("want_read symbol");
    assert_eq!(want_read.selection_span.start.row, 5, "want_read at file row 5");
    assert_eq!(want_read.package.as_deref(), Some("My::AL"));

    // Goto-def from the in-package caller reaches the data-section def.
    let def = fa.find_definition(Point::new(2, 16), None);
    assert_eq!(
        def.map(|s| s.start.row),
        Some(5),
        "goto-def on want_read() should land on the data-section sub"
    );
}

/// The gate: a package that does NOT use AutoLoader/SelfLoader must not have
/// its trailing `__END__` / `__DATA__` payload mined for subs — that text is
/// genuine data or POD, not code.
#[test]
fn non_autoloader_data_section_synthesizes_nothing() {
    let src = "package My::Plain;\n\
               use strict;\n\
               sub real_sub { 1 }\n\
               1;\n\
               __END__\n\
               sub looks_like_a_sub { return 99 }\n\
               =pod\n\
               docs\n\
               =cut\n\
               plain documentation text\n";
    let fa = build_fa(src);
    assert!(
        fa.symbols()
            .iter()
            .all(|s| s.name != "looks_like_a_sub"),
        "data-section subs must NOT be synthesized without AutoLoader/SelfLoader"
    );
    assert!(
        fa.symbols().iter().any(|s| s.name == "real_sub"),
        "the real pre-__END__ sub is still present"
    );
}

/// Inheritance-form gate: `use base 'AutoLoader'` (parents, not a direct
/// `use AutoLoader`) also enables data-section synthesis.
#[test]
fn autoloader_via_use_base_enables_synthesis() {
    let src = "package My::Sub;\n\
               use base 'AutoLoader';\n\
               1;\n\
               __END__\n\
               sub inherited_loader_sub { return 1 }\n";
    let fa = build_fa(src);
    assert!(
        fa.symbols()
            .iter()
            .any(|s| s.name == "inherited_loader_sub" && s.kind == SymKind::Sub),
        "use base 'AutoLoader' must enable data-section synthesis"
    );
}

/// CHAINED hashref-key ref: `$obj->get_config->{host}` — get_config's
/// return type is a known class (here a blessed Config), so the `host`
/// key is knowable and must get its own narrow HashKeyAccess ref owned
/// by that class. Without it, references/goto-def on `host` are dropped.
#[test]
fn chained_method_call_hash_key_emits_owned_ref() {
    let src = "\
package Config;
sub new { bless { host => 'localhost', port => 5432 }, shift }
package Foo;
sub new { bless {}, shift }
sub get_config { return Config->new() }
package main;
my $obj = Foo->new();
$obj->get_config->{host};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());

    // The `host` token (line 7, after `->{`) gets a HashKeyAccess ref
    // owned by Config — the chain receiver's class.
    let host_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "host" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        !host_refs.is_empty(),
        "chained hash-key access should emit a HashKeyAccess ref for 'host'"
    );
    let owner = host_refs
        .iter()
        .find_map(|r| r.hash_key_owner().cloned())
        .expect("chained hash-key ref should carry a resolved owner");
    assert_eq!(
        owner,
        HashKeyOwner::Class("Config".to_string()),
        "owner should be the chain receiver's class, got {:?}",
        owner
    );

    // Goto-def from the `host` token reaches the bless'd key in Config::new.
    let key_ref = host_refs[0];
    let def = fa.find_definition(key_ref.span.start, None);
    assert!(
        def.is_some(),
        "goto-def on chained `->{{host}}` should resolve to Config's key def"
    );
    assert_eq!(def.unwrap().start.row, 1, "host def is the bless key on line 1");
}

/// Plain-comma blessed hash keys (`bless { 'host', $h }`) emit HashKeyDef
/// symbols exactly like the fat-comma spelling — `collect_pair_keys` pairs
/// positionally, so the key is the even-position element regardless of the
/// separator that follows it.
#[test]
fn blessed_hash_plain_comma_keys_emit_hash_key_defs() {
    let src = "\
package Config;
sub new { bless { 'host', 'localhost', port => 5432 }, shift }
package main;
my $c = Config->new();
$c->{host};
$c->{port};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // `bless { ... }` inside `sub new` owns its keys by the declaring sub
    // (Config::new) — the same owner the fat-comma spelling produces.
    let expected = HashKeyOwner::Sub { package: Some("Config".to_string()), name: "new".to_string() };
    for key in ["host", "port"] {
        assert!(
            fa.symbols().iter().any(|s| s.name == key
                && matches!(&s.detail, SymbolDetail::HashKeyDef { owner, .. } if *owner == expected)),
            "plain/fat-comma bless key `{key}` must emit a HashKeyDef owned by Config::new; got: {:?}",
            fa.symbols().iter()
                .filter(|s| matches!(s.detail, SymbolDetail::HashKeyDef { .. }))
                .map(|s| (s.name.clone(), s.detail.clone())).collect::<Vec<_>>(),
        );
    }
}

/// Regression: an untyped chain (`$obj->mystery->{host}` where `mystery`
/// has no resolvable return type) must emit NO key ref — honest about
/// ignorance rather than latching onto a wrong owner.
#[test]
fn untyped_chain_emits_no_hash_key_ref() {
    let src = "\
package main;
my $obj = bless {}, 'Foo';
$obj->totally_unknown_method->{host};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    let host_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "host" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        host_refs.is_empty(),
        "untyped chain must not emit a hash-key ref, got {:?}",
        host_refs
    );
}

/// CG-3b cross-package glob attribution: `*{ 'DateTime::' . $sub } = …`
/// inside `package DateTime::PP` synthesizes the tail (`_ymd2rd`) under
/// the *named* package (`DateTime`), not the file's own package.
#[test]
fn cross_package_glob_synthesizes_under_target_package() {
    let src = r#"package DateTime::PP;
sub _ymd2rd { 1 }
sub _rd2ymd { 2 }
my @subs = qw( _ymd2rd _rd2ymd );
for my $sub (@subs) {
    no strict 'refs';
    *{ 'DateTime::' . $sub } = __PACKAGE__->can($sub);
}
1;
"#;
    let fa = build_fa(src);
    for tail in ["_ymd2rd", "_rd2ymd"] {
        let under_datetime = fa.symbols().iter().any(|s| {
            s.name == tail
                && matches!(s.kind, SymKind::Sub)
                && s.package.as_deref() == Some("DateTime")
        });
        assert!(
            under_datetime,
            "glob-synthesized `{}` should be attributed to DateTime, symbols: {:?}",
            tail,
            fa.symbols()
                .iter()
                .filter(|s| s.name == tail)
                .map(|s| (&s.name, &s.package))
                .collect::<Vec<_>>()
        );
    }
    // The real definitions (under DateTime::PP) are untouched.
    assert!(
        fa.symbols().iter().any(|s| s.name == "_ymd2rd"
            && matches!(s.kind, SymKind::Sub)
            && s.package.as_deref() == Some("DateTime::PP")),
        "the original DateTime::PP::_ymd2rd sub must still exist"
    );
}

/// Pins ARBITRARY DEPTH for the chained hash-key owner: build-time owner
/// resolution must ride the recursive chain typer, so `host` carries the same
/// `Config` owner whether the chain is one hop or three.
#[test]
fn chained_method_call_hash_key_owned_at_arbitrary_depth() {
    let src = "\
package Config;
sub new { bless { host => 'localhost' }, shift }
package Foo;
sub new { bless {}, shift }
sub me { return $_[0] }
sub get_config { return Config->new() }
package main;
my $obj = Foo->new();
$obj->get_config->{host};
$obj->me->me->get_config->{host};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    let owners: Vec<_> = fa.refs().iter().filter_map(|r| match r.hash_key_owner() {
        Some(o) if r.target_name == "host" => Some(o.clone()),
        _ => None,
    }).collect();
    assert_eq!(owners.len(), 2, "both 1-hop and 3-hop chained ->{{host}} should emit an owned ref, got {:?}", owners);
    assert!(owners.iter().all(|o| *o == HashKeyOwner::Class("Config".to_string())), "every depth's owner must be Config, got {:?}", owners);
}

/// Mixed-depth chain: a method-call value, then a hash-key, then a method,
/// then a hash-key. `$obj->get_config->deep->cfg->{host}` — the final `host`
/// resolves through (method → typed value → method → key).
#[test]
fn chained_hash_key_mixed_depth_method_key() {
    let src = "\
package Inner;
sub new { bless { host => 'localhost' }, shift }
package Deep;
sub new { bless {}, shift }
sub cfg { return Inner->new() }
package Config;
sub new { bless {}, shift }
sub deep { return Deep->new() }
package Foo;
sub new { bless {}, shift }
sub get_config { return Config->new() }
package main;
my $obj = Foo->new();
$obj->get_config->deep->cfg->{host};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    let owners: Vec<_> = fa.refs().iter().filter_map(|r| match r.hash_key_owner() {
        Some(o) if r.target_name == "host" => Some(o.clone()),
        _ => None,
    }).collect();
    assert_eq!(owners, vec![HashKeyOwner::Class("Inner".to_string())],
        "mixed-depth chain must resolve host's owner to Inner, got {:?}", owners);
}

/// Regression: an untyped deep chain emits NO owner — honest-about-ignorance,
/// never a wrong-owner latch.
#[test]
fn chained_hash_key_untyped_deep_chain_no_owner() {
    let src = "\
package Foo;
sub new { bless {}, shift }
sub mystery { return $_[0]->some_unknown_thing() }
package main;
my $obj = Foo->new();
$obj->mystery->mystery->{host};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    let owned: Vec<_> = fa.refs().iter().filter(|r| r.hash_key_owner().is_some()
        && r.target_name == "host").collect();
    assert!(owned.is_empty(), "untyped deep chain must not latch a wrong owner, got {:?}",
        owned.iter().map(|r| &r.kind).collect::<Vec<_>>());
}

/// Regression: a same-package glob (`*name = sub {…}`, no `::` prefix)
/// still synthesizes under the current package.
#[test]
fn same_package_glob_synthesizes_under_current_package() {
    let src = r#"package Acme::Widget;
*frobnicate = sub { 42 };
1;
"#;
    let fa = build_fa(src);
    assert!(
        fa.symbols().iter().any(|s| s.name == "frobnicate"
            && matches!(s.kind, SymKind::Sub)
            && s.package.as_deref() == Some("Acme::Widget")),
        "same-package glob must stay under the current package, symbols: {:?}",
        fa.symbols()
            .iter()
            .filter(|s| s.name == "frobnicate")
            .map(|s| (&s.name, &s.package))
            .collect::<Vec<_>>()
    );
}
