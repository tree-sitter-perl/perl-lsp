//! Red-pin tests for the `ReturnExpr` symbol-declarative
//! return-type machinery. Each test demonstrates a concrete gap
//! today's call-site-emission / Edge-without-receiver chase can't
//! cover, and goes green at a specific phase:
//!
//! - **A** (`coderef_call_arity_zero_resolves_mojo_getter`,
//!   `coderef_call_arity_one_resolves_mojo_writer`) — Mojo
//!   accessor invoked via `\&Class::name; $cb->($obj)` /
//!   `$cb->($obj, "v")`. Today `bag_query_attachment` builds a
//!   `ReducerQuery` with `arity_hint: None`, so the bag's
//!   `MethodOnClass{...}` chase falls through `FluentArityDispatch`
//!   and surfaces the writer's `Receiver` answer regardless of
//!   how many args the coderef received. Greens after Phase 3
//!   (Mojo synth migrates to `UnionOnArgs` ReturnExpr) once the
//!   chain typer's coderef arm threads `arity_hint` and `receiver`
//!   into the bag query (Phase 1 + Phase 2).
//!
//! - **B** (`coderef_to_resultset_find_returns_row_class`) —
//!   `$rs->find(...)` works because the call site emits
//!   `Parametric(RowOf(self))` directly on the `Expression(refidx)`
//!   in `builder.rs`. Via `\&...::find`, the chase reaches a
//!   `MethodOnClass{...}` whose stored answer is whatever
//!   inheritance + body inference produces — no projection. Greens
//!   after Phase 4 (DBIC find/single/etc. declare
//!   `Operator(RowOf(Receiver))` on the symbol).
//!
//! - **C** (`anon_closure_arity_dispatch_via_coderef_call`) —
//!   anon-sub closure with arity-discriminated arms (`return X
//!   unless @_; ...`) invoked via `$cb->()` / `$cb->("v")`. Today
//!   `coderef_return_edge_for` for `anonymous_subroutine_expression`
//!   emits `Expr(body_last_expr_span)`, and the bag's `Expr(_)`
//!   chase doesn't go through `FluentArityDispatch` (which only
//!   claims `Symbol(_)` / `MethodOnClass{...}`). Greens after
//!   Phase 2 (anon-sub `return_edge` becomes `Symbol(sub_id)` and
//!   the builder publishes a `UnionOnArgs` ReturnExpr on the
//!   symbol when the body has arity arms).
//!
//! Convention mirrors `parametric_resultset_tests`: each test is
//! self-contained (no fixtures), uses `inferred_type_via_bag` /
//! `find_definition` so it doesn't pin internal encoding. Ignored
//! today; the `#[ignore]` annotation comes off as each phase lands.

use super::*;
use tree_sitter::{Parser, Point};

fn parse(source: &str) -> FileAnalysis {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    crate::builder::build(&tree, source.as_bytes())
}

fn point_at(source: &str, needle: &str) -> Point {
    let byte = source
        .find(needle)
        .unwrap_or_else(|| panic!("needle {:?} not in source:\n{}", needle, source));
    let row = source[..byte].matches('\n').count();
    let col = byte - source[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    Point::new(row, col)
}

// ---- Test A: Mojo accessor via coderef carries arity ----

/// `\&Foo::name; $cb->($foo)` — coderef-call invokes a Mojo getter
/// (arity 0 from the method's perspective; 1 arg passed to the
/// coderef, the first being the receiver). Result: the getter's
/// scalar-default type (`String`).
///
/// **Today:** `bag_query_attachment` calls the registry with
/// `arity_hint: None`. Without a hint, `FluentArityDispatch`'s
/// `(Some(a), Some(h))` arm never fires, default arms are scanned,
/// and the writer's `ClassName` answer (the `arg_count: None`
/// branch on Mojo synth) wins. So `$x` types as `Foo`, not
/// `String` — wrong axis.
///
/// **After ReturnExpr:** Mojo synth declares
/// `UnionOnArgs { (Empty, Concrete(String)), (Any, Receiver) }`
/// on `MethodOnClass{Foo, name}`. The chain typer's coderef arm
/// passes `arity_hint = args.len() - 1 = 0` (subtracting the
/// receiver from the coderef's arg count) and `receiver = $foo`'s
/// type. ReturnExprReducer matches `Empty`, returns `Concrete(String)`.
#[test]
fn coderef_call_arity_zero_resolves_mojo_getter() {
    let src = "\
package Foo;
use Mojo::Base -base;
has 'name' => 'default';

package main;
my $foo = Foo->new;
my $cb = \\&Foo::name;
my $x = $cb->($foo);
my $sentinel;
";
    let fa = parse(src);
    let pt = point_at(src, "my $sentinel");
    let ty = fa.inferred_type_via_bag("$x", pt);
    assert_eq!(
        ty,
        Some(InferredType::String),
        "coderef call of Mojo getter (arity 0 after dropping receiver) must \
         resolve through ReturnExpr's UnionOnArgs::Empty arm to String"
    );
}

/// Same setup as the getter test, but `$cb->($foo, "v")` → arity 1
/// after dropping the receiver. Hits `UnionOnArgs::Any => Receiver`
/// → substitutes the call's receiver (`$foo`'s type) → `ClassName("Foo")`.
///
/// Today: arity_hint=None falls into the writer's default arm and
/// returns `ClassName("Foo")` *for the wrong reason* (the writer is
/// always picked regardless of arg count). The test passes today,
/// but the mechanism is broken — ReturnExpr replaces the accidental
/// pass with a principled one. Kept here to pin the right answer
/// once the gating is correct.
#[test]
fn coderef_call_arity_one_resolves_mojo_writer() {
    let src = "\
package Foo;
use Mojo::Base -base;
has 'name' => 'default';

package main;
my $foo = Foo->new;
my $cb = \\&Foo::name;
my $x = $cb->($foo, \"v\");
my $sentinel;
";
    let fa = parse(src);
    let pt = point_at(src, "my $sentinel");
    let ty = fa.inferred_type_via_bag("$x", pt);
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Foo".into())),
        "coderef call of Mojo writer (arity 1 after dropping receiver) must \
         resolve through ReturnExpr's UnionOnArgs::Any => Receiver arm"
    );
}

// ---- Test B: ResultSet find via coderef projects through RowOf ----

/// `my $cb = \&...::find; $cb->($rs, $id)->{column}` — coderef
/// invocation of `find`, with a Parametric receiver. After
/// substitution, the result types as the row class so `->{column}`
/// resolves to a row hash-key def.
///
/// **Today:** `find` doesn't have a body in our source (it lives
/// in DBIx::Class::ResultSet, not in scope). The call-site
/// projection in `builder.rs:5299–5320` emits
/// `Parametric(RowOf(self))` *only* when the receiver of a
/// `method_call_expression` is locally typed Parametric — the
/// coderef-call chase doesn't trigger it. So the chase resolves
/// `MethodOnClass{*, find}` via inheritance → no symbol with a
/// stored return → None. `$row->{name}` falls back to scope-walk
/// resolution and misses the column-def synthesized by
/// `add_columns`.
///
/// **After ReturnExpr (Phase 4):** the DBIC plugin / synth
/// declares `Operator(RowOf(Receiver))` on
/// `MethodOnClass{"DBIx::Class::ResultSet", "find"}`. The chain
/// typer's coderef arm chases with `receiver = $rs`'s
/// `Parametric(ResultSet { row, .. })`. ReturnExprReducer
/// substitutes Receiver, evaluates `RowOf(...)` →
/// `ClassName(row_class)`. `$row->{name}` resolves through the
/// row class's column defs.
#[test]
fn coderef_to_resultset_find_returns_row_class() {
    let src = "\
package Schema::Result::Users;
use base 'DBIx::Class::Core';
__PACKAGE__->add_columns(
    name => { data_type => 'varchar' },
);

package main;
my $schema;
my $rs = $schema->resultset('Schema::Result::Users');
my $cb = \\&DBIx::Class::ResultSet::find;
my $row = $cb->($rs, 1);
$row->{name};
";
    let fa = parse(src);

    // $row's TC carries the unevaluated `Parametric(RowOf(ResultSet))`
    // — the value-side projection. Consumers (`class_name()`,
    // `hash_key_class()`) evaluate it on demand. Pin the structural
    // shape so a regression to a flat / unrelated encoding trips
    // here before any user-visible feature breaks.
    let pt_row = point_at(src, "$row->{name}");
    let ty = fa.inferred_type_via_bag("$row", pt_row);
    let parametric = match ty {
        Some(InferredType::Parametric(p)) => p,
        other => panic!(
            "coderef call of `find` with a Parametric receiver must produce \
             a Parametric (RowOf<ResultSet>); got {:?}",
            other
        ),
    };
    assert_eq!(
        parametric.class_name(),
        Some("Schema::Result::Users"),
        "RowOf<ResultSet>'s class_name() evaluates to the row class for \
         downstream method dispatch + hash-key access"
    );
    assert_eq!(
        parametric.hash_key_class(),
        Some("Schema::Result::Users"),
        "RowOf<ResultSet>'s hash_key_class() evaluates to the row class \
         (delegates to inner ResultSet's hash_key_class)"
    );
}

// ---- Test C: anon-sub closure with arity dispatch via coderef ----

/// Anon-sub with arity-discriminated returns called via `$cb->()` /
/// `$cb->("v")`. Body returns a String literal in the no-args arm,
/// constructs a class instance otherwise — keeps the test focused
/// on the arity dispatch question and avoids the orthogonal
/// "does `$self->{x}` resolve through Mojo accessor return type"
/// question (which has its own resolution path).
///
/// **Today:** `coderef_return_edge_for` for
/// `anonymous_subroutine_expression` emits
/// `WitnessAttachment::Expr(body_last_expr_span)`. The bag's
/// `Expr(_)` chase hits `ExprReturn`, which doesn't claim arity
/// witnesses — `FluentArityDispatch` only fires on `Symbol(_)` /
/// `MethodOnClass{...}` shapes. So both calls return whatever
/// the body's last-expression span happens to type as. Arity is
/// lost at the chase boundary.
///
/// **After Phase 2 (anon uniformity):** the anon-sub gets a
/// `Symbol(sub_id)` and `coderef_return_edge_for` returns it
/// instead of `Expr(...)`. The bag chases via Symbol, the existing
/// `FluentArityDispatch` (or post-Phase-5, the `ReturnExprReducer`'s
/// UnionOnArgs) dispatches on the call's `arity_hint`, and each
/// invocation gets its arm-specific answer.
#[test]
fn anon_closure_arity_dispatch_via_coderef_call() {
    let src = "\
package Foo;
sub new { bless {}, shift }

package main;
my $cb = sub {
    return \"getter\" unless @_;
    return Foo->new;
};
my $g = $cb->();
my $w = $cb->(\"v\");
my $sentinel;
";
    let fa = parse(src);
    let pt = point_at(src, "my $sentinel");

    let g_ty = fa.inferred_type_via_bag("$g", pt);
    assert_eq!(
        g_ty,
        Some(InferredType::String),
        "anon-sub called with arity 0 must hit the `unless @_` arm \
         (returning a String literal); chain typer threads arity_hint=0 \
         through the coderef-call edge"
    );

    let w_ty = fa.inferred_type_via_bag("$w", pt);
    assert_eq!(
        w_ty,
        Some(InferredType::ClassName("Foo".into())),
        "anon-sub called with arity 1 falls through to the default arm \
         (returning Foo->new); chain typer threads arity_hint=1"
    );
}

// ---- Test F: dynamic method call ($obj->$cb()) routes through edge ----

/// `$obj->$cb()` is method-call syntax with a scalar-valued method
/// position — the parser emits `method_call_expression` with
/// `method` field shaped `(method (scalar))`. Semantically it's
/// just `$cb->($obj)`: the coderef in `$cb` runs with `$obj` as
/// the first arg.
///
/// **Today:** the chain typer's `method_call_expression` arm
/// resolves invocant types via the bag's `Expression(refidx)`
/// chase, but the dynamic-method case doesn't carry a method-name
/// keyed `MethodOnClass{...}` edge — there's no static name to
/// dispatch on. So `my $r = $obj->$cb()` falls through and `$r`
/// stays untyped.
///
/// **After Phase 2b:** the chain typer detects the scalar `method`
/// field, resolves the scalar's `CodeRef.return_edge` through the
/// bag, and treats the call like `$cb->($obj, ...)` — receiver =
/// `$obj`'s type, arity = call_args.len() (the coderef receives
/// args as-is; the method-call syntax is just a dispatch shorthand).
#[test]
fn dynamic_method_call_routes_through_coderef_edge() {
    let src = "\
package Foo;
sub new { bless {}, shift }

package main;
my $obj = Foo->new;
my $cb = sub { [1, 2] };
my $r = $obj->$cb();
my $sentinel;
";
    let fa = parse(src);
    let pt = point_at(src, "my $sentinel");
    let ty = fa.inferred_type_via_bag("$r", pt);
    assert_eq!(
        ty,
        Some(InferredType::ArrayRef),
        "dynamic-method call must chase $cb's CodeRef return_edge — \
         the closure returns an arrayref regardless of receiver, so \
         the chase resolves through the Symbol's stored return"
    );
}
