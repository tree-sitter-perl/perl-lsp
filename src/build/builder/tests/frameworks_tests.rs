use super::*;

// ---- Framework accessor synthesis tests ----

#[test]
fn test_moo_has_ro() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'name' => (is => 'ro');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 1, "should synthesize one getter");
    if let SymbolDetail::Sub {
        ref params,
        is_method,
        ..
    } = methods[0].detail
    {
        assert!(is_method);
        assert!(params.is_empty(), "ro getter has no params");
    }
}

#[test]
fn test_moo_has_rw() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'name' => (is => 'rw');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    // rw produces getter (0 params) + setter (1 param)
    assert_eq!(methods.len(), 2, "should synthesize getter + setter");
}

#[test]
fn test_moo_has_isa_type() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'count' => (is => 'ro', isa => 'Int');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "count" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 1);
    let __r = fa.symbol_return_type_via_bag(methods[0].id, None);
    let return_type = __r.as_ref();
    if matches!(methods[0].detail, SymbolDetail::Sub { .. }) {
        assert_eq!(return_type, Some(&InferredType::Numeric));
    }
}

#[test]
fn test_moo_has_multiple_qw() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has [qw(foo bar)] => (is => 'ro');
",
    );
    let foo: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "foo" && s.kind == SymKind::Method)
        .collect();
    let bar: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "bar" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(foo.len(), 1, "should synthesize foo accessor");
    assert_eq!(bar.len(), 1, "should synthesize bar accessor");
}

#[test]
fn test_moo_has_constant_array_ref() {
    // `has \@names` (a ref to a constant array) folds the array's elements
    // through the same accessor synthesis as the literal-arrayref form. A
    // bare `has @names` SPLATS the array into the call (`has 'a','b', is=>…`)
    // — a different declaration — so it is NOT folded; nor is a non-constant
    // array.
    let fa = build_fa(
        "
package Foo;
use Moo;
my @attrs = qw(client_id client_secret);
my @more  = ('refresh_token', 'profile_id');
has \\@attrs, is => 'ro';
has @more,    is => 'ro';
has @runtime, is => 'ro';
",
    );
    let accessor = |name: &str| {
        fa.symbols()
            .iter()
            .filter(|s| s.name == name && s.kind == SymKind::Method)
            .count()
    };
    assert_eq!(accessor("client_id"), 1, "fold \\@attrs → client_id");
    assert_eq!(accessor("client_secret"), 1, "fold \\@attrs → client_secret");
    assert_eq!(accessor("refresh_token"), 0, "bare `has @more` splats — not a multi-attr decl, not folded");
    assert_eq!(accessor("profile_id"), 0, "bare `has @more` splats — not a multi-attr decl, not folded");
    assert_eq!(accessor("runtime"), 0, "non-constant array stays unclaimed");
}

#[test]
fn test_moo_has_bare_no_accessor() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'internal' => (is => 'bare');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "internal" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 0, "bare should not synthesize accessor");
}

#[test]
fn test_moo_no_accessor_without_is() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'internal';
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "internal" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 0, "no `is` should not synthesize accessor");
}

#[test]
fn test_moose_has_classname_isa() {
    let fa = build_fa(
        "
package Foo;
use Moose;
has 'db' => (is => 'ro', isa => 'DBI::db');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "db" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 1);
    let __r = fa.symbol_return_type_via_bag(methods[0].id, None);
    let return_type = __r.as_ref();
    if matches!(methods[0].detail, SymbolDetail::Sub { .. }) {
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("DBI::db".into()))
        );
    }
}

#[test]
fn test_moo_has_instanceof() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'logger' => (is => 'ro', isa => \"InstanceOf['Log::Any']\");
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "logger" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 1);
    let __r = fa.symbol_return_type_via_bag(methods[0].id, None);
    let return_type = __r.as_ref();
    if matches!(methods[0].detail, SymbolDetail::Sub { .. }) {
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("Log::Any".into()))
        );
    }
}

#[test]
fn test_moo_has_rwp() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'status' => (is => 'rwp');
",
    );
    let getter: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "status" && s.kind == SymKind::Method)
        .collect();
    let writer: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "_set_status" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(getter.len(), 1, "rwp should synthesize getter");
    assert_eq!(writer.len(), 1, "rwp should synthesize _set_name writer");
}

#[test]
fn test_moo_has_accessor_keyword() {
    // `accessor => 'get_set_x'` synthesizes a combined read/write method named
    // `get_set_x` — distinct from the attr-named default accessor.
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'x' => (is => 'rw', accessor => 'get_set_x');
",
    );
    let acc: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "get_set_x" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(acc.len(), 1, "accessor keyword should synthesize get_set_x method");
    // The default attr-named accessor ('x') still exists from `is => 'rw'`.
    let default_acc: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "x" && s.kind == SymKind::Method)
        .collect();
    assert!(!default_acc.is_empty(), "default attr accessor still synthesized");
}

#[test]
fn test_moo_has_ro_does_not_synthesize_ro_symbol() {
    // `is => 'ro'` must NOT synthesize a method named `ro` — the gate that
    // fixes the phantom-`ro` regression (names_a_method excludes `is`).
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'y' => (is => 'ro');
",
    );
    let ro_sym: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "ro")
        .collect();
    assert!(ro_sym.is_empty(), "`is => 'ro'` must not mint a symbol named `ro`");
}

#[test]
fn test_mojo_has_basic() {
    let fa = build_fa(
        "
package Foo;
use Mojo::Base -base;
has 'name';
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 2, "Mojo::Base synthesizes getter + setter");
    // Getter: no params, no return type
    let getter = methods
        .iter()
        .find(|m| {
            if let SymbolDetail::Sub { ref params, .. } = m.detail {
                params.is_empty()
            } else {
                false
            }
        })
        .expect("should have getter");
    let __r = fa.symbol_return_type_via_bag(getter.id, None);
    let return_type = __r.as_ref();
    if let SymbolDetail::Sub { is_method, .. } = getter.detail {
        assert!(is_method);
        assert!(return_type.is_none(), "getter has no return type");
    }
    // Setter: 1 param, fluent return
    let setter = methods
        .iter()
        .find(|m| {
            if let SymbolDetail::Sub { ref params, .. } = m.detail {
                params.len() == 1
            } else {
                false
            }
        })
        .expect("should have setter");
    let __r = fa.symbol_return_type_via_bag(setter.id, None);
    let return_type = __r.as_ref();
    if let SymbolDetail::Sub { is_method, .. } = setter.detail {
        assert!(is_method);
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("Foo".into()))
        );
    }
}

/// Regression test for QA finding B1 (export-attribution; the sweep history):
/// Mojo::Base writer accessors lost their fluent invocant return type
/// when queried through the bag at arity=1. The QA dump for
/// `Mojo::Log::level` showed:
///
///   getter (params=0): raw=String,    bag=String     ← OK
///   writer (params=1): raw=Mojo::Log, bag=String     ← BUG, getter's type
///
/// `query_sub_return_type` finds the *first* same-named symbol by name
/// (the getter, since synthesis adds it first), folds witnesses on that
/// id, and answers from the getter's stored `return_type`. The writer's
/// distinct fluent type was never visible at the bag level — every
/// `$ua->ca($f)->cert(...)` chain lost its receiver type at the second
/// hop.
///
/// Fix: framework synthesis publishes `ReturnExpr::UnionOnArgs`
/// arms on `Symbol(sym_id)` (per-symbol arity arm) and a multi-arm
/// `UnionOnArgs` on `PackageSymbol{package, name}` (cross-symbol arity
/// dispatch scoped to the declaring class). `ReturnExprReducer`
/// dispatches `q.arity_hint` against the union's `ArgGuard` branches
/// regardless of which sister sym `find()` returned. See
/// `docs/adr/return-expr.md`.
#[test]
fn test_mojo_base_writer_returns_invocant_via_bag() {
    use crate::model::file_analysis::TypeProvenance;

    let fa = build_fa(
        "
package MyLog;
use Mojo::Base -base;

has level => 'info';   # default value gives getter type = String
has app;               # no default; getter has no return type
",
    );

    // arity=1 → fluent writer must return the package class for chaining.
    // Pre-fix this returned String (the getter's type) — that's the bug.
    assert_eq!(
        fa.sub_return_type_at_arity("level", Some(1)),
        Some(InferredType::ClassName("MyLog".into())),
        "Mojo::Base writer at arity=1 must return the invocant class for fluent chaining"
    );
    assert_eq!(
        fa.sub_return_type_at_arity("app", Some(1)),
        Some(InferredType::ClassName("MyLog".into())),
        "Mojo::Base writer with no default still returns the invocant class"
    );

    // arity=0 → getter returns its scalar accessor type.
    assert_eq!(
        fa.sub_return_type_at_arity("level", Some(0)),
        Some(InferredType::String),
        "Mojo::Base getter at arity=0 returns the default-value type"
    );

    // Both sister symbols carry per-symbol witnesses now (was 0 across
    // every Mojo::* dump in the QA sweep).
    let getter = fa
        .symbols()
        .iter()
        .find(|s| s.name == "level" && matches!(&s.detail, SymbolDetail::Sub { params, .. } if params.is_empty()))
        .expect("getter symbol");
    let writer = fa
        .symbols()
        .iter()
        .find(|s| s.name == "level" && matches!(&s.detail, SymbolDetail::Sub { params, .. } if params.len() == 1))
        .expect("writer symbol");
    let getter_witnesses = fa
        .witnesses
        .for_attachment(&crate::model::witnesses::WitnessAttachment::Symbol(getter.id))
        .len();
    let writer_witnesses = fa
        .witnesses
        .for_attachment(&crate::model::witnesses::WitnessAttachment::Symbol(writer.id))
        .len();
    assert!(getter_witnesses > 0, "getter must have at least one bag witness");
    assert!(writer_witnesses > 0, "writer must have at least one bag witness");

    // Provenance: each accessor flushes a FrameworkSynthesis entry, not
    // PluginOverride (Mojo::Base synthesis is core, not a plugin).
    match fa.return_type_provenance(writer.id) {
        TypeProvenance::FrameworkSynthesis { framework, reason } => {
            assert_eq!(framework, "Mojo::Base");
            assert!(reason.contains("level"), "reason names the attribute");
            assert!(
                reason.contains("fluent") || reason.contains("writer"),
                "reason describes the writer role"
            );
        }
        other => panic!("writer provenance must be FrameworkSynthesis, got {other:?}"),
    }
    match fa.return_type_provenance(getter.id) {
        TypeProvenance::FrameworkSynthesis { framework, .. } => {
            assert_eq!(framework, "Mojo::Base");
        }
        other => panic!("getter provenance must be FrameworkSynthesis, got {other:?}"),
    }
}

/// H7-11: a Mojo `has`-default sub whose body is a `$ENV{X} || <literal>`
/// (or `//`) fallback must type the getter to the literal's type — the
/// fallback (RHS) is the guaranteed floor. Pre-fix the two-branch `||`
/// killed inference and the getter's arity-0 entry vanished entirely.
#[test]
fn test_mojo_has_default_or_fallback_types_to_literal() {
    let fa = build_fa(
        "
package My::UA;
use Mojo::Base -base;

has connect_timeout => sub { $ENV{MOJO_CONNECT_TIMEOUT} || 10 };
has max_redirects   => sub { $ENV{MOJO_MAX_REDIRECTS} // 5 };
has ioloop          => sub { My::IOLoop->new };
",
    );
    // `$ENV{X} || 10` → the literal floor is Numeric even though the LHS
    // env-hash access can't be typed.
    assert_eq!(
        fa.sub_return_type_at_arity("connect_timeout", Some(0)),
        Some(InferredType::Numeric),
        "|| fallback getter types to the literal floor"
    );
    assert_eq!(
        fa.sub_return_type_at_arity("max_redirects", Some(0)),
        Some(InferredType::Numeric),
        "// fallback getter types to the literal floor"
    );
    // A non-`||` default (a class constructor) still types to the class —
    // the fold only kicks in for the short-circuit operators.
    assert_eq!(
        fa.sub_return_type_at_arity("ioloop", Some(0)),
        Some(InferredType::ClassName("My::IOLoop".into())),
        "class-constructor default is unaffected by the || fold"
    );
}

/// H7-12: an arity-discriminated sub whose 1-arg branch is guarded by a
/// COMPOUND `unless @_ > 1 || ref $_[0]` (the Mojo::DOM::attr shape). Only
/// the `@_ > 1` disjunct is arity-decidable; the arm fires at arity ≤ 1, so
/// the fluent `return $self` must NOT claim arity 1. Pre-fix the compound
/// guard was unclassifiable, the 1-arg arm dropped, and the fluent `Any`
/// arm wrongly reported the invocant class at arity 1.
#[test]
fn test_compound_arity_guard_does_not_leak_fluent_to_arity_one() {
    let fa = build_fa(
        "
package My::DOM;
use Mojo::Base -base;

sub attr {
  my $self = shift;
  my $attrs = { title => 'x' };
  return $attrs unless @_;
  return $attrs->{$_[0]} unless @_ > 1 || ref $_[0];
  $attrs->{$_[0]} = $_[1];
  return $self;
}
",
    );
    // arity 0 → the whole hashref (structural shape, not the fluent class).
    assert!(
        fa.sub_return_type_at_arity("attr", Some(0))
            .is_some_and(|t| t.is_hash_shaped()),
        "0-arg attr returns the attrs hashref, got {:?}",
        fa.sub_return_type_at_arity("attr", Some(0))
    );
    // arity 1 → the hash VALUE (get), never the fluent invocant class. An
    // honest None is acceptable; ClassName(My::DOM) is the regression.
    assert_ne!(
        fa.sub_return_type_at_arity("attr", Some(1)),
        Some(InferredType::ClassName("My::DOM".into())),
        "1-arg attr is the getter branch, not the fluent $self branch"
    );
    // arity ≥ 2 → the fluent invocant (set).
    assert_eq!(
        fa.sub_return_type_at_arity("attr", Some(2)),
        Some(InferredType::ClassName("My::DOM".into())),
        "2-arg attr returns the invocant class for fluent chaining"
    );
}

/// H7-12 companion: an arity-discriminated sub whose branches all AGREE on
/// the return type (Path::Tiny::path — every branch yields the invocant
/// class) must still answer a NO-HINT query with the agreed type. The
/// arity-union retraction that keeps a genuinely-disagreeing gap honest
/// (attr) must NOT fire here, or hover on the declaration loses "returns: X".
#[test]
fn test_arity_discriminated_all_arms_agree_answers_no_hint() {
    // `if ( !@_ && ... )` makes this arity-discriminated (a Zero arm), but
    // both the early-return and the fall-through yield the same class.
    let fa = build_fa(
        "
package My::Path;
sub path {
  my $self = shift;
  return $self if !@_ && ref($self) eq __PACKAGE__;
  return bless {}, __PACKAGE__;
}
",
    );
    // No-hint query (what hover uses) must fold across the agreeing arms.
    assert_eq!(
        fa.sub_return_type_at_arity("path", None),
        Some(InferredType::ClassName("My::Path".into())),
        "all-arms-agree discriminated sub answers the agreed type at no-hint"
    );
    // And every concrete arity agrees too — no gap, no leak.
    for arity in [Some(0u32), Some(1)] {
        assert_eq!(
            fa.sub_return_type_at_arity("path", arity),
            Some(InferredType::ClassName("My::Path".into())),
            "agreeing arms answer the same type at arity {arity:?}"
        );
    }
}

/// H7-12 companion: the `||`/`//` fold must NOT type a `shift`/`$_[N]`-LHS
/// param-default idiom (`my $x = shift // ''`). The value is the unknown
/// parameter, not the fallback literal — typing it `String` poisoned the
/// arm-join of subs whose narrowed returns depend on the param (the
/// Mojolicious url_for regression).
#[test]
fn test_shift_default_does_not_poison_param_type() {
    let fa = build_fa(
        "
package My::C;
sub build {
  my ($self, $target) = (shift, shift // '');
  return $self;
}
",
    );
    // `$target` stays open (the param), not the literal's String — so a
    // downstream arm-join that expects the real arg type isn't poisoned.
    assert_ne!(
        fa.inferred_type_via_bag("$target", Point::new(4, 2)),
        Some(InferredType::String),
        "shift-default LHS must not brand the param as the fallback literal's type"
    );
}

/// Mirror of B1 for Moo `is => 'rw'` writers — same shape, isa-typed
/// rather than fluent. The writer reads back the isa type at arity=1.
#[test]
fn test_moo_rw_writer_returns_isa_type_via_bag() {
    use crate::model::file_analysis::TypeProvenance;

    let fa = build_fa(
        "
package Thing;
use Moo;
has size => (is => 'rw', isa => 'Int');
",
    );

    // Both arities resolve to the isa-derived type.
    assert_eq!(
        fa.sub_return_type_at_arity("size", Some(0)),
        Some(InferredType::Numeric),
        "Moo getter returns isa type"
    );
    assert_eq!(
        fa.sub_return_type_at_arity("size", Some(1)),
        Some(InferredType::Numeric),
        "Moo rw writer returns isa type"
    );

    let writer = fa
        .symbols()
        .iter()
        .find(|s| s.name == "size" && matches!(&s.detail, SymbolDetail::Sub { params, .. } if params.len() == 1))
        .expect("writer symbol");
    match fa.return_type_provenance(writer.id) {
        TypeProvenance::FrameworkSynthesis { framework, .. } => {
            assert_eq!(framework, "Moo");
        }
        other => panic!("Moo writer provenance must be FrameworkSynthesis, got {other:?}"),
    }
}

#[test]
fn test_mojo_base_parent_inheritance() {
    let fa = build_fa(
        "
package MyApp;
use Mojo::Base 'Mojolicious';
has 'config';
",
    );
    // Should register parent
    assert_eq!(
        Some(fa.declared_parents("MyApp")),
        Some(["Mojolicious".to_string()].as_slice())
    );
    // Should synthesize getter + setter accessors
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "config" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 2, "Mojo::Base synthesizes getter + setter");
}

#[test]
fn test_mojo_base_strict_no_accessor() {
    let fa = build_fa(
        "
package Foo;
use Mojo::Base -strict;
has 'name';
",
    );
    // -strict means no framework mode, has is just a regular function
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(
        methods.len(),
        0,
        "-strict should not trigger accessor synthesis"
    );
}

#[test]
fn test_no_accessor_without_framework() {
    let fa = build_fa(
        "
package Foo;
has 'name' => (is => 'ro');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 0, "no framework = no accessor synthesis");
}

#[test]
fn test_dbic_add_columns() {
    let fa = build_fa(
        "
package Schema::Result::User;
use base 'DBIx::Class::Core';
__PACKAGE__->add_columns(
    id    => { data_type => 'integer' },
    name  => { data_type => 'varchar' },
    email => { data_type => 'varchar' },
);
",
    );
    let id: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "id" && s.kind == SymKind::Method)
        .collect();
    let name: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    let email: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "email" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(id.len(), 1, "should synthesize id accessor");
    assert_eq!(name.len(), 1, "should synthesize name accessor");
    assert_eq!(email.len(), 1, "should synthesize email accessor");
}

#[test]
fn test_dbic_has_many() {
    let fa = build_fa(
        "
package Schema::Result::Post;
use base 'DBIx::Class::Core';
__PACKAGE__->has_many(comments => 'Schema::Result::Comment', 'post_id');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "comments" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 1);
    let __r = fa.symbol_return_type_via_bag(methods[0].id, None);
    let return_type = __r.as_ref();
    if matches!(methods[0].detail, SymbolDetail::Sub { .. }) {
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("DBIx::Class::ResultSet".into()))
        );
    }
}

#[test]
fn test_dbic_belongs_to() {
    let fa = build_fa(
        "
package Schema::Result::Comment;
use base 'DBIx::Class::Core';
__PACKAGE__->belongs_to(author => 'Schema::Result::User', 'author_id');
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "author" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 1);
    let __r = fa.symbol_return_type_via_bag(methods[0].id, None);
    let return_type = __r.as_ref();
    if matches!(methods[0].detail, SymbolDetail::Sub { .. }) {
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("Schema::Result::User".into()))
        );
    }
}

#[test]
fn test_dbic_instance_add_columns_does_not_synthesize() {
    // A runtime `$rs->add_columns('x','y')` (dynamic query building, e.g. crm's
    // ResultSet joins) is NOT a class declaration — the dbic plugin gates on
    // `receiver_is_package`, so no column accessors are minted from it. Only
    // `__PACKAGE__->add_columns` (class-level) declares columns.
    let fa = build_fa(
        "
package My::RS;
use base 'DBIx::Class::ResultSet';
__PACKAGE__->add_columns('decl_col');
sub widen {
    my $self = shift;
    $self->add_columns('runtime_col');
}
",
    );
    let decl: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "decl_col" && s.kind == SymKind::Method)
        .collect();
    let runtime: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "runtime_col" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(decl.len(), 1, "class-level __PACKAGE__->add_columns declares");
    assert_eq!(
        runtime.len(),
        0,
        "instance $rs->add_columns is a runtime op, not a declaration"
    );
}

#[test]
fn test_accessor_return_type_propagation() {
    let src = r#"
package Moo::Config;
use Moo;
has 'host' => (is => 'ro', isa => 'Str');
sub dsn { my ($self) = @_; return "x"; }

package Moo::Service;
use Moo;
has 'config' => (is => 'ro', isa => "InstanceOf['Moo::Config']");
sub run {
    my ($self) = @_;
    my $cfg = $self->config;
    my $dsn = $cfg->dsn;
}
"#;
    let fa = build_fa(src);

    // Verify the config accessor has the right return type
    let config_methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "config" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(config_methods.len(), 1, "should have 1 config accessor");
    assert_eq!(config_methods[0].package.as_deref(), Some("Moo::Service"));
    let __r = fa.symbol_return_type_via_bag(config_methods[0].id, None);
    let return_type = __r.as_ref();
    if matches!(config_methods[0].detail, SymbolDetail::Sub { .. }) {
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("Moo::Config".into())),
            "config accessor should return Moo::Config"
        );
    }

    // Verify method call binding exists (not a function call binding)
    let cfg_binding = fa
        .method_call_bindings
        .iter()
        .find(|b| b.variable == "$cfg");
    assert!(
        cfg_binding.is_some(),
        "should have method call binding for $cfg"
    );
    assert!(
        fa.call_bindings
            .iter()
            .find(|b| b.variable == "$cfg")
            .is_none(),
        "$cfg should NOT be a function call binding"
    );

    // Verify $cfg gets Moo::Config type (not Moo::Service)
    let cfg_type = fa.inferred_type_via_bag("$cfg", tree_sitter::Point::new(13, 0));
    assert_eq!(
        cfg_type,
        Some(InferredType::ClassName("Moo::Config".into())),
        "$cfg should be Moo::Config, not Moo::Service"
    );

    // Verify chained resolution: $dsn = $cfg->dsn → String
    let dsn_binding = fa
        .method_call_bindings
        .iter()
        .find(|b| b.variable == "$dsn");
    assert!(
        dsn_binding.is_some(),
        "should have method call binding for $dsn"
    );
}

#[test]
fn test_mojo_getter_setter_distinct() {
    let fa = build_fa(
        "
package Foo;
use Mojo::Base -base;
has 'name';
",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "name" && s.kind == SymKind::Method)
        .collect();
    assert_eq!(methods.len(), 2, "should synthesize getter + setter");

    let getter = methods.iter().find(|m| {
        if let SymbolDetail::Sub { ref params, .. } = m.detail {
            params.is_empty()
        } else {
            false
        }
    });
    let setter = methods.iter().find(|m| {
        if let SymbolDetail::Sub { ref params, .. } = m.detail {
            params.len() == 1
        } else {
            false
        }
    });
    assert!(getter.is_some(), "should have a 0-param getter");
    assert!(setter.is_some(), "should have a 1-param setter");

    // Getter: no return type (inferable from usage)
    let __r = fa.symbol_return_type_via_bag(getter.unwrap().id, None);
    let return_type = __r.as_ref();
    if let SymbolDetail::Sub { .. } = getter.unwrap().detail {
        assert!(return_type.is_none());
    }
    // Setter: fluent return
    let __r = fa.symbol_return_type_via_bag(setter.unwrap().id, None);
    let return_type = __r.as_ref();
    if let SymbolDetail::Sub { .. } = setter.unwrap().detail {
        assert_eq!(
            return_type,
            Some(&InferredType::ClassName("Foo".into()))
        );
    }
}

#[test]
fn test_mojo_fluent_chain_resolves() {
    let src = "
package Foo;
use Mojo::Base -base;
has 'name';
has 'age';
sub greet {
    my ($self) = @_;
    my $result = $self->name('Bob')->age;
    return $result;
}
";
    let fa = build_fa(src);
    // $self->name('Bob') has args → setter → returns Foo
    // ->age has no args → getter → returns None (unknown)
    // The chain should resolve: name('Bob') returns Foo, ->age is valid on Foo
    let method_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "age" && matches!(r.kind, RefKind::MethodCall { .. }))
        .collect();
    assert!(
        !method_refs.is_empty(),
        "should have method call ref for 'age'"
    );
}

#[test]
fn test_moo_rw_arity_resolution() {
    let fa = build_fa(
        "
package Foo;
use Moo;
has 'name' => (is => 'rw', isa => 'Str');
",
    );
    // Moo rw: both getter and setter have same return type (Str)
    // With arity, both 0 and 1 should return String since both symbols have the same type
    let rt_getter = fa.find_method_return_type("Foo", "name", None, Some(0));
    assert_eq!(rt_getter, Some(InferredType::String));
    let rt_setter = fa.find_method_return_type("Foo", "name", None, Some(1));
    assert_eq!(rt_setter, Some(InferredType::String));
    let rt_default = fa.find_method_return_type("Foo", "name", None, None);
    assert_eq!(rt_default, Some(InferredType::String));
}

/// Regression guard for bag-residual D1: same-named methods on
/// unrelated classes must resolve to their own per-class types, no
/// matter what name-keyed cache or "latest-wins" witness landed last.
///
/// Two unrelated classes (`Sweet`, `Sour`) ship a method `flavor` via
/// Mojo::Base `has`, with different defaults. Class-keyed dispatch is
/// required to disambiguate them — any code path that resolves
/// methods by name alone (a `return_types: HashMap<String, _>` mirror,
/// or the now-deleted `WitnessAttachment::NamedSub(name)` shape)
/// will silently shadow one class's getter with the other's whenever
/// the second declaration overwrites the first.
///
/// The arity=1 (fluent writer) assertions extend the same guarantee
/// to overload dispatch: `Sweet`'s writer returns `Sweet`, `Sour`'s
/// returns `Sour`, even though both subs share the name `flavor`.
///
/// D1 (lifted from the abandoned `refactor/bag-residual-d1-method-on-class`
/// branch — see commit c322178 for the original attempt). The redo
/// must keep this test passing while routing every method-type query
/// through the bag.
#[test]
fn method_on_class_disambiguates_same_name_across_classes() {
    let fa = build_fa(
        "
package Sweet;
use Mojo::Base -base;
has flavor => 'caramel';

package Sour;
use Mojo::Base -base;
has flavor => sub { [1, 2, 3] };
",
    );
    let sweet_getter_sym = fa
        .symbols()
        .iter()
        .find(|s| {
            s.name == "flavor"
                && s.package.as_deref() == Some("Sweet")
                && matches!(&s.detail, SymbolDetail::Sub { params, .. } if params.is_empty())
        })
        .map(|s| s.id);
    let sour_getter_sym = fa
        .symbols()
        .iter()
        .find(|s| {
            s.name == "flavor"
                && s.package.as_deref() == Some("Sour")
                && matches!(&s.detail, SymbolDetail::Sub { params, .. } if params.is_empty())
        })
        .map(|s| s.id);
    assert!(sweet_getter_sym.is_some(), "Sweet getter sym must exist");
    assert!(sour_getter_sym.is_some(), "Sour getter sym must exist");
    assert_ne!(sweet_getter_sym, sour_getter_sym);

    assert_eq!(
        fa.find_method_return_type("Sweet", "flavor", None, Some(0)),
        Some(InferredType::String),
        "Sweet::flavor getter returns String (from 'caramel' default), \
         not Sour's ArrayRef"
    );
    assert!(
        fa.find_method_return_type("Sour", "flavor", None, Some(0)).is_some_and(|t| t.is_array_shaped()),
        "Sour::flavor getter returns ArrayRef (from sub-returning-array \
         default), not Sweet's String",
    );
    assert_eq!(
        fa.find_method_return_type("Sweet", "flavor", None, Some(1)),
        Some(InferredType::ClassName("Sweet".into())),
    );
    assert_eq!(
        fa.find_method_return_type("Sour", "flavor", None, Some(1)),
        Some(InferredType::ClassName("Sour".into())),
    );
}

/// Regression for the DFS-MRO order fix in
/// `for_each_ancestor_class`. Perl's default MRO is left-to-right
/// depth-first: for `@ISA = (A, B)` where A and B both define `m`,
/// `C->m` resolves to A's. Pre-fix, the stack-based walker pushed
/// parents left-to-right and `pop()`'d, traversing in REVERSE
/// `@ISA` order — so B::m silently won. The fix pushes parents in
/// reverse order so LIFO pops them in `@ISA` order.
#[test]
fn for_each_ancestor_class_walks_left_to_right_isa_order() {
    let fa = build_fa(
        "
package A;
sub m { return 'a' }
package B;
sub m { return 1 }
package C;
our @ISA = ('A', 'B');
",
    );
    // A::m returns String ('a'), B::m returns Numeric (1).
    // C->m must resolve to A's String — A is first in @ISA.
    assert_eq!(
        fa.find_method_return_type("C", "m", None, None),
        Some(InferredType::String),
        "C->m must walk @ISA left-to-right and pick A::m, not B::m"
    );
}

#[test]
fn test_mojo_arity_resolution() {
    let fa = build_fa(
        "
package Bar;
use Mojo::Base -base;
has 'title';
",
    );
    // Getter (0 args): no return type
    let rt_getter = fa.find_method_return_type("Bar", "title", None, Some(0));
    assert!(rt_getter.is_none(), "getter should have no return type");
    // Setter (1 arg): fluent return (ClassName)
    let rt_setter = fa.find_method_return_type("Bar", "title", None, Some(1));
    assert_eq!(rt_setter, Some(InferredType::ClassName("Bar".into())));
    // Default (None): getter (primary, first symbol)
    let rt_default = fa.find_method_return_type("Bar", "title", None, None);
    assert!(rt_default.is_none(), "default should return getter type");
}

#[test]
fn test_mojo_default_string_infers_type() {
    let fa = build_fa(
        "
package App;
use Mojo::Base -base;
has name => 'default';
",
    );
    let rt = fa.find_method_return_type("App", "name", None, Some(0));
    assert_eq!(
        rt,
        Some(InferredType::String),
        "string default → String getter"
    );
}

#[test]
fn test_mojo_default_arrayref_infers_type() {
    let fa = build_fa(
        "
package App;
use Mojo::Base -base;
has items => sub { [] };
",
    );
    let rt = fa.find_method_return_type("App", "items", None, Some(0));
    assert!(
        rt.is_some_and(|t| t.is_array_shaped()),
        "sub {{ [] }} default → ArrayRef getter",
    );
}

#[test]
fn test_mojo_default_hashref_infers_type() {
    let fa = build_fa(
        "
package App;
use Mojo::Base -base;
has config => sub { {} };
",
    );
    let rt = fa.find_method_return_type("App", "config", None, Some(0));
    assert!(
        rt.is_some_and(|t| t.is_hash_shaped()),
        "sub {{{{ }}}} default → HashRef getter",
    );
}

#[test]
fn test_mojo_default_constructor_infers_type() {
    let fa = build_fa(
        "
package App;
use Mojo::Base -base;
has ua => sub { Mojo::UserAgent->new };
",
    );
    let rt = fa.find_method_return_type("App", "ua", None, Some(0));
    assert_eq!(
        rt,
        Some(InferredType::ClassName("Mojo::UserAgent".into())),
        "sub {{ Foo->new }} default → ClassName getter"
    );
}

#[test]
fn test_mojo_default_number_infers_type() {
    let fa = build_fa(
        "
package App;
use Mojo::Base -base;
has timeout => 30;
",
    );
    let rt = fa.find_method_return_type("App", "timeout", None, Some(0));
    assert_eq!(
        rt,
        Some(InferredType::Numeric),
        "number default → Numeric getter"
    );
}

#[test]
fn test_mojo_default_no_value_no_type() {
    let fa = build_fa(
        "
package App;
use Mojo::Base -base;
has 'name';
",
    );
    let rt = fa.find_method_return_type("App", "name", None, Some(0));
    assert!(rt.is_none(), "no default → no getter type");
}

// ---- Constant folding + export extraction tests ----

#[test]
fn test_builder_extracts_exports_qw() {
    let fa = build_fa(
        "
package Foo;
use Exporter 'import';
our @EXPORT = qw(delta);
our @EXPORT_OK = qw(alpha beta gamma);
",
    );
    assert_eq!(fa.export, vec!["delta"]);
    assert_eq!(fa.export_ok, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_builder_extracts_exports_paren() {
    let fa = build_fa(
        "
package Bar;
our @EXPORT_OK = ('foo', 'bar', 'baz');
",
    );
    assert_eq!(fa.export_ok, vec!["foo", "bar", "baz"]);
}

#[test]
fn test_push_exports() {
    let fa = build_fa(
        "
package Foo;
use Exporter 'import';
our @EXPORT_OK = qw(foo);
push @EXPORT_OK, 'bar', 'baz';
",
    );
    assert_eq!(fa.export_ok, vec!["foo", "bar", "baz"]);
}

#[test]
fn test_exporter_extensible_export_call() {
    // `export(qw( foo $bar @baz -tag ))` — only the plain sub name `foo`
    // is recorded; sigil'd vars and `-tag` group names are skipped.
    let fa = build_fa(
        "
package My::Ext;
use Exporter::Extensible -exporter_setup => 1;
export(qw( foo $bar @baz -tag ));
sub foo { 1 }
",
    );
    assert_eq!(fa.export_ok, vec!["foo"]);
}

#[test]
fn test_exporter_extensible_attribute() {
    // `sub foo :Export(...)` / `sub bar :Export` — the sub name is the export.
    let fa = build_fa(
        "
package My::Ext;
use Exporter::Extensible -exporter_setup => 1;
sub foo : Export(-tag) { 1 }
sub bar :Export { 2 }
sub plain { 3 }
",
    );
    assert!(fa.export_ok.contains(&"foo".to_string()));
    assert!(fa.export_ok.contains(&"bar".to_string()));
    assert!(!fa.export_ok.contains(&"plain".to_string()));
}

#[test]
fn test_exporter_declare_export_pair() {
    // `default_export NAME => sub {}` / `export NAME => sub {}` / `exports qw/../`.
    let fa = build_fa(
        "
package My::Decl;
use Exporter::Declare;
default_export foo => sub { 1 };
export bar => sub { 2 };
exports qw/a b/;
",
    );
    assert!(fa.export_ok.contains(&"foo".to_string()));
    assert!(fa.export_ok.contains(&"bar".to_string()));
    assert!(fa.export_ok.contains(&"a".to_string()));
    assert!(fa.export_ok.contains(&"b".to_string()));
}

#[test]
fn test_exporter_call_gated_on_use() {
    // False-positive guard: a `sub export {}` (or a stray `export(...)`)
    // in a package that didn't `use` an exporter-declare family module
    // must NOT register exports.
    let fa = build_fa(
        "
package Plain;
sub export { 1 }
export('not_an_export');
",
    );
    assert!(fa.export_ok.is_empty());
}

/// Gate 5: `find_sub_for_call` (resolution path) must check `export_ok`, not
/// just `export`. A name that Foo records only in `@EXPORT_OK` suppresses the
/// diagnostic in symbols.rs (already checks both), but before this fix
/// `signature_for_call` / goto-def would fall through because the resolution
/// path only tested `export`. Now both lists are checked.
#[test]
fn test_export_ok_resolves_cross_file() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;

    // Build a module that exports `fetch_data` via @EXPORT_OK only.
    let provider_fa = build_fa(
        "package Data::Fetcher;\nour @EXPORT_OK = qw(fetch_data);\nsub fetch_data { my ($url) = @_; }\n1;\n",
    );
    assert!(
        provider_fa.export_ok.contains(&"fetch_data".to_string()),
        "provider must record fetch_data in export_ok",
    );

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);
    idx.insert_cache(
        "Data::Fetcher",
        Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
            PathBuf::from("/fake/Data/Fetcher.pm"),
            std::sync::Arc::new(provider_fa),
        ))),
    );

    // Consumer: bare `use Data::Fetcher;` — calls `fetch_data(...)`.
    let consumer_fa = build_fa(
        "package My::App;\nuse Data::Fetcher;\nfetch_data('http://example.com');\n",
    );

    // signature_for_call exercises find_sub_for_call → bare-import path.
    let sig = consumer_fa.signature_for_call(
        "fetch_data",
        false,
        None,
        tree_sitter::Point::new(2, 0),
        Some(&idx),
    );
    assert!(
        sig.is_some(),
        "signature_for_call must resolve fetch_data via @EXPORT_OK, got None",
    );
}

#[test]
fn test_importer_consumer_retargets_source() {
    // `use Importer 'M' => qw/foo bar/` imports foo/bar FROM M. The Import
    // entry must point at M, not Importer, so goto-def crosses to M's subs.
    let fa = build_fa(
        "
package My::Consumer;
use Importer 'Some::Module' => qw/foo bar/;
",
    );
    let imp = fa
        .imports
        .iter()
        .find(|i| i.module_name == "Some::Module")
        .expect("Import re-targeted to Some::Module");
    let names: Vec<&str> = imp
        .imported_symbols
        .iter()
        .map(|s| s.local_name.as_str())
        .collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    // No Import should pin to Importer itself.
    assert!(fa.imports.iter().all(|i| i.module_name != "Importer"));
}

#[test]
fn test_importer_menu_advertised_names() {
    // IMPORTER_MENU advertises export lists; pull the `export`/`export_ok`
    // name arrays. `export_anon` (name → coderef) is unmodeled.
    let fa = build_fa(
        "
package My::Menu;
sub IMPORTER_MENU {
  return (
    export => [qw/foo bar/],
    export_ok => ['baz'],
    export_anon => { quux => sub { 1 } },
  );
}
sub foo { 1 }
",
    );
    assert!(fa.export_ok.contains(&"foo".to_string()));
    assert!(fa.export_ok.contains(&"bar".to_string()));
    assert!(fa.export_ok.contains(&"baz".to_string()));
    assert!(!fa.export_ok.contains(&"quux".to_string()));
}

#[test]
fn test_use_constant_string() {
    let fa = build_fa(
        "
package Foo;
use constant NAME => 'hello';
use parent NAME;
",
    );
    assert_eq!(
        fa.declared_parents("Foo"),
        &vec!["hello".to_string()]
    );
}

#[test]
fn test_constant_array_our() {
    let fa = build_fa(
        "
our @THINGS = qw(a b);
our @EXPORT_OK = (@THINGS, 'c');
",
    );
    assert_eq!(fa.export_ok, vec!["a", "b", "c"]);
}

#[test]
fn test_constant_array_my() {
    let fa = build_fa(
        "
my @THINGS = qw(a b);
our @EXPORT_OK = (@THINGS, 'c');
",
    );
    assert_eq!(fa.export_ok, vec!["a", "b", "c"]);
}

#[test]
fn test_constant_array_in_exports() {
    let fa = build_fa(
        "
package Foo;
use Exporter 'import';
my @COMMON = qw(alpha beta);
our @EXPORT_OK = (@COMMON, 'gamma');
",
    );
    assert_eq!(fa.export_ok, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_recursive_constant_resolution() {
    let fa = build_fa(
        "
package Foo;
use Exporter 'import';
use constant BASE => qw(a b);
use constant ALL => (BASE, 'c');
our @EXPORT_OK = (ALL);
",
    );
    assert_eq!(fa.export_ok, vec!["a", "b", "c"]);
}

#[test]
fn test_glob_export_literal_name() {
    // Data::Printer pattern: *{"${caller}::np"} = \&np
    let fa = build_fa(
        r#"
package Data::Printer;
sub np { }
sub p { }
sub import {
    my $class = shift;
    my $caller = caller;
    { no strict 'refs';
        *{"${caller}::p"} = \&p;
        *{"${caller}::np"} = \&np;
    }
}
"#,
    );
    assert!(
        fa.export.contains(&"p".to_string()),
        "should detect p export: {:?}",
        fa.export
    );
    assert!(
        fa.export.contains(&"np".to_string()),
        "should detect np export: {:?}",
        fa.export
    );
}

#[test]
fn test_glob_export_variable_name() {
    // Aliased export: my $imported = 'p'; *{"$caller\::$imported"} = \&p
    let fa = build_fa(
        r#"
package Data::Printer;
sub p { }
sub import {
    my $class = shift;
    my $caller = caller;
    my $imported = 'dump_it';
    { no strict 'refs';
        *{"$caller\::$imported"} = \&p;
    }
}
"#,
    );
    assert!(
        fa.export.contains(&"dump_it".to_string()),
        "should resolve aliased export: {:?}",
        fa.export
    );
}

#[test]
fn test_glob_export_loop_pattern() {
    // Try::Tiny pattern: loop over qw list
    let fa = build_fa(
        r#"
package Try::Tiny;
sub try { }
sub catch { }
sub finally { }
sub import {
    my $class = shift;
    my $caller = caller;
    for my $name (qw(try catch finally)) {
        no strict 'refs';
        *{"${caller}::${name}"} = \&$name;
    }
}
"#,
    );
    assert!(
        fa.export.contains(&"try".to_string()),
        "should detect try: {:?}",
        fa.export
    );
    assert!(
        fa.export.contains(&"catch".to_string()),
        "should detect catch: {:?}",
        fa.export
    );
    assert!(
        fa.export.contains(&"finally".to_string()),
        "should detect finally: {:?}",
        fa.export
    );
}

#[test]
fn test_glob_export_fallback_to_rhs() {
    // When glob name is fully dynamic, fall back to \&name on RHS
    let fa = build_fa(
        r#"
package Foo;
sub bar { }
sub import {
    my $caller = caller;
    *{$caller . '::bar'} = \&bar;
}
"#,
    );
    assert!(
        fa.export.contains(&"bar".to_string()),
        "should fall back to RHS name: {:?}",
        fa.export
    );
}

#[test]
fn test_glob_export_only_inside_import() {
    // Glob assigns outside sub import should NOT populate exports
    let fa = build_fa(
        r#"
package Foo;
sub setup {
    my $caller = caller;
    *{"${caller}::thing"} = \&thing;
}
"#,
    );
    assert!(
        fa.export.is_empty(),
        "should not export from non-import sub: {:?}",
        fa.export
    );
}

#[test]
fn test_mojo_has_accessor_writer_hidden_from_outline() {
    // Mojo `has 'x'` synthesizes a getter + a same-named fluent writer (for
    // arity-1 return typing). Only ONE should show in the outline; the writer
    // carries hide_in_outline so it doesn't duplicate the getter.
    let fa = build_fa("package Msg;\nuse Mojo::Base -base;\nhas 'content';\n");
    let visible: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "content" && s.kind == SymKind::Method)
        .filter(|s| !s.hidden_in_outline())
        .collect();
    assert_eq!(
        visible.len(),
        1,
        "exactly one visible `content` accessor expected, got {}",
        visible.len()
    );
    // Both symbols still exist (writer hidden, not deleted) for arity typing.
    let total = fa.symbols().iter().filter(|s| s.name == "content" && s.kind == SymKind::Method).count();
    assert_eq!(total, 2, "getter + (hidden) writer both retained");
}

#[test]
fn test_strict_mojo_base_shift_not_invocant() {
    // `use Mojo::Base -strict` is a non-OO module: a bare `my $x = shift` is
    // arg[0], NOT the invocant, so it must not type as the package (doing so
    // produced bogus unresolved-method diagnostics, e.g. $tx->res in
    // Mojo::WebSocket). A named invocant is still typed elsewhere.
    let fa = build_fa(
        "package MyStrict;\nuse Mojo::Base -strict;\nsub helper {\n  my $tx = shift;\n  return $tx->res;\n}\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$tx", Point::new(4, 9)),
        None,
        "shift in a -strict (non-OO) module must not type as the package"
    );
}

#[test]
fn test_mojo_base_base_and_strict_still_oo() {
    // `-base` (in any order, even alongside the redundant `-strict`) makes the
    // package a class, so a bare `shift` IS the invocant.
    for src in [
        "package C;\nuse Mojo::Base -base, -strict;\nsub greet {\n  my $x = shift;\n  return $x->name;\n}\n",
        "package C;\nuse Mojo::Base -strict, -base;\nsub greet {\n  my $x = shift;\n  return $x->name;\n}\n",
    ] {
        let fa = build_fa(src);
        assert_eq!(
            fa.inferred_type_via_bag("$x", Point::new(4, 9)),
            Some(InferredType::ClassName("C".into())),
            "-base makes the package OO regardless of a redundant -strict: {src}"
        );
    }
}

#[test]
fn test_list_shift_pair_params_extracted() {
    // Mojo idiom `my ($self, $name) = (shift, shift)` — each shift binds the
    // next @_ element, so the LHS vars are positional params. $self is the
    // invocant; $name is a real param.
    let fa = build_fa("package P;\nsub cookie {\n  my ($self, $name) = (shift, shift);\n}\n");
    let sub = fa.symbols().iter().find(|s| s.name == "cookie").expect("cookie sym");
    let params: Vec<&str> = match &sub.detail {
        SymbolDetail::Sub { params, .. } => params.iter().map(|p| p.name.as_str()).collect(),
        _ => panic!("cookie not a Sub"),
    };
    assert!(params.contains(&"$self") && params.contains(&"$name"),
        "expected $self + $name from (shift, shift), got {params:?}");
}

#[test]
fn test_goto_amp_fq_sub_emits_call_ref() {
    // `goto &Foo::bar` — the `&` code-ref sigil is stripped so the FQ call
    // resolves; a FunctionCall ref to Foo::bar is emitted (resolved_package Foo).
    let fa = build_fa("package Child;\nsub import { goto &Parent::Thing::setup; }\n");
    let r = fa.refs().iter().find(|r|
        matches!(r.kind, RefKind::FunctionCall { .. }) && r.target_name == "Parent::Thing::setup");
    assert!(r.is_some(), "expected FunctionCall ref to Parent::Thing::setup (& stripped), got {:?}",
        fa.refs().iter().filter(|r| matches!(r.kind, RefKind::FunctionCall{..})).map(|r| &r.target_name).collect::<Vec<_>>());
}

#[test]
fn test_glob_assigned_sub_ternary_rhs_registers() {
    // Try::Tiny `*_subname = $su ? \&Sub::Util::set_subname : sub {...}` /
    // Path::Tiny `*_same = IS_WIN32() ? sub{} : sub{}`: the glob holds a coderef
    // in every branch, so the name is a registered sub. Without this, same-file
    // calls were flagged unresolved-function (false positive).
    let fa = build_fa(
        r#"
package Demo;
sub bar { 1 }
*_subname = $su ? \&Sub::Util::set_subname : sub { $_[1] };
"#,
    );
    assert!(
        fa.symbols()
            .iter()
            .any(|s| s.kind == SymKind::Sub && s.name == "_subname"),
        "ternary glob-assign should register `_subname` as a sub: {:?}",
        fa.symbols().iter().filter(|s| s.kind == SymKind::Sub).map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[test]
fn test_loop_variable_constant_folding() {
    let fa = build_fa(
        "
package Foo;
sub test {
    my $self = shift;
    for my $attr (qw(name email)) {
        my $getter = \"get_$attr\";
        $self->$getter();
    }
}
",
    );
    let method_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }))
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(method_refs.contains(&"get_name"), "should resolve get_name");
    assert!(
        method_refs.contains(&"get_email"),
        "should resolve get_email"
    );
}

#[test]
fn test_dynamic_method_dispatch() {
    let fa = build_fa(
        "
package Foo;
my $method = 'get_name';
sub test { my $self = shift; $self->$method() }
",
    );
    let method_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "get_name")
        .collect();
    assert!(
        !method_refs.is_empty(),
        "dynamic method call should resolve to get_name"
    );
}

#[test]
fn test_hover_dynamic_dispatch_token_vs_chain_head() {
    // The `$self->$method()` dynamic-dispatch hover must fire on the METHOD
    // TOKEN (the `$method` variable) but NOT on a plain variable at the head
    // of a wide chain — a multi-line chain's MethodCall ref spans the whole
    // expression, so keying on the whole span returned the tail method's POD
    // for the head invocant (DBIC F2).
    let src = "\
package Foo;
sub get_name { my $self = shift; return $self->{name} }
my $method = 'get_name';
sub test { my $self = shift; $self->$method() }
my $obj = Foo->new;
my $chain = $obj->get_name->get_name;
";
    let fa = build_fa(src);

    // Genuine case: hover on the `$method` token in `$self->$method()`.
    // Line 3 (0-based): "sub test { my $self = shift; $self->$method() }"
    // `$method` begins at column 36.
    let line3 = src.lines().nth(3).unwrap();
    let mcol = line3.find("$method(").unwrap();
    let genuine = fa.hover_info(Point::new(3, mcol + 1), src, None);
    assert!(
        genuine.as_deref().is_some_and(|h| h.contains("resolved from")),
        "hover on the dynamic method token should resolve the dispatch, got {:?}",
        genuine
    );

    // Regression: hover on `$obj` at the head of the chain must NOT be
    // attributed to the chain's tail method.
    let line5 = src.lines().nth(5).unwrap();
    let ocol = line5.find("$obj->").unwrap();
    let head = fa.hover_info(Point::new(5, ocol + 1), src, None);
    assert!(
        !head.as_deref().unwrap_or("").contains("resolved from"),
        "hover on the chain-head variable must not borrow the tail method, got {:?}",
        head
    );
}
