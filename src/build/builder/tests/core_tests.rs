use super::*;

#[test]
fn debug_moo_name_refs() {
    let src = std::fs::read_to_string("test_files/frameworks.pl").unwrap();
    let fa = build_fa(&src);
    for r in fa.refs() {
        if r.target_name == "name" || r.target_name == "new" {
            eprintln!(
                "REF: target={} kind={:?} span={:?} resolves_to={:?}",
                r.target_name, r.kind, r.span, r.resolved_symbol()
            );
        }
    }
    for s in fa.symbols() {
        if s.name == "name"
            || (matches!(s.kind, SymKind::HashKeyDef)
                && s.span.start.row > 5
                && s.span.start.row < 25)
        {
            eprintln!(
                "SYM: name={} kind={:?} span={:?} sel_span={:?} detail={:?}",
                s.name, s.kind, s.span, s.selection_span, s.detail
            );
        }
    }
}

/// A Mojo plugin that mints helpers with dynamically-built names — an
/// interpolated loop name (`"get_$name"`), the bare loop var (`$name`),
/// a `.`-concat (`'find_' . $name`), and a lexical-literal name (both
/// bare and interpolated) — must synthesize each concrete helper as a
/// real Method on the app surface. The name enumeration is structural
/// (loop var over a literal `qw` list, lexical assigned a literal);
/// nothing is executed. Each synthesized helper's selection span is the
/// registration `$app->helper(...)` name argument, so goto-def lands on
/// the call site (provenance, rule #9).
#[test]
fn plugin_mojo_helpers_dynamic_loop_names_synthesize() {
    let src = r#"
package MyApp::Plugin::Dyn;
use Mojo::Base 'Mojolicious::Plugin';
sub register {
    my ($self, $app) = @_;
    for my $name (qw(user order invoice)) {
        $app->helper("get_$name" => sub { my ($c) = @_; return 1; });
        $app->helper($name => sub { my ($c) = @_; return 2; });
        $app->helper('find_' . $name => sub { my ($c) = @_; return 3; });
    }
    my $one = 'single';
    $app->helper($one => sub { my ($c) = @_; return 4; });
    $app->helper("mk_$one" => sub { my ($c) = @_; return 5; });
    $app->helper("${one}_x" => sub { my ($c) = @_; return 6; });
}
1;
"#;
    let fa = build_fa(src);
    let mut names: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-helpers"))
        .map(|s| s.name.as_str())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "find_invoice", "find_order", "find_user", "get_invoice", "get_order", "get_user",
            "invoice", "mk_single", "order", "single", "single_x", "user",
        ],
        "every statically enumerable helper name (loop-var interpolation bare \
         and braced, bare loop var, concat, lexical literal) minted as a Method",
    );

    // Each helper is a real Method on the app surface, resolvable from any
    // controller/app via the synthetic-parent edge — same as a literal helper.
    for consumer in ["Mojolicious::Controller", "Mojolicious"] {
        for helper in ["get_order", "find_user", "mk_single"] {
            match fa.resolve_method_in_ancestors(consumer, helper, None) {
                Some(crate::model::file_analysis::MethodResolution::Local { class, .. }) => {
                    assert_eq!(class, crate::model::file_analysis::APP_SURFACE_CLASS);
                }
                other => panic!("{consumer}->{helper} should resolve to the app surface, got {other:?}"),
            }
        }
    }

    // Provenance: the interpolated helper's selection span is the name
    // argument at its own registration site (row 6 = the `"get_$name"`
    // line), NOT some other loop iteration's line — so goto-def lands on
    // the exact `$app->helper("get_$name" => …)` call.
    let get_order = fa
        .symbols()
        .iter()
        .find(|s| s.name == "get_order")
        .expect("get_order synthesized");
    assert_eq!(get_order.selection_span.start.row, 6, "selection span at the registration name");
    assert_eq!(get_order.span.start.row, 6, "extent span at the registration call");
}

/// Nested loops over two literal lists: the cross-product falls out of the
/// same interpolation fold (both loop vars are live in the constant table
/// when the inner body is walked), so `"${verb}_$obj"` mints every
/// verb×obj combination. No dedicated nested-loop machinery.
#[test]
fn plugin_mojo_helpers_nested_loop_cross_product() {
    let src = r#"
package P;
use Mojo::Base 'Mojolicious::Plugin';
sub register {
    my ($self, $app) = @_;
    for my $verb (qw(get set)) {
        for my $obj (qw(user post)) {
            $app->helper("${verb}_$obj" => sub { my ($c) = @_; return 1; });
        }
    }
}
1;
"#;
    let fa = build_fa(src);
    let mut names: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-helpers"))
        .map(|s| s.name.as_str())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["get_post", "get_user", "set_post", "set_user"],
        "nested loops mint the full verb×obj cross-product",
    );
}

/// A variable typed in an outer scope (both a plain `my $x = Foo->new`
/// assignment and a plugin-synthesized route-handler param) keeps its
/// type when captured by a nested closure body — the scope-chain walk
/// ascends through the closure boundary. The plugin case additionally
/// exercises the Mojo::Lite route form with an intermediate
/// default-values hash between the path and the handler sub
/// (`any '/x' => {k => ''} => sub ($c) {...}`): the route-decl query's
/// `@handler` capture must bind the trailing anonymous sub, not the
/// intervening hash — otherwise the param list is empty and no
/// controller type is synthesized (H8-1).
#[test]
fn closure_captured_typed_variable_survives_into_nested_sub() {
    // Plain assignment shape: outer `my $x = Foo->new` captured by an
    // inner `sub { $x->method }`.
    let plain = r#"
package Foo;
sub new { bless {}, shift }
sub method { return 42 }

package main;
my $x = Foo->new;
my $cb = sub {
    $x->method;
};
"#;
    let fa = build_fa(plain);
    // `$x` inside the closure body (row 8) resolves to Foo, same as at
    // its declaration (row 6).
    for (row, col) in [(6usize, 3usize), (8, 4)] {
        let ty = fa.inferred_type_via_bag("$x", Point::new(row, col));
        assert_eq!(
            ty.and_then(|t| t.class_name().map(str::to_string)),
            Some("Foo".to_string()),
            "plain-assignment typed $x should resolve to Foo at {row}:{col}",
        );
    }

    // Plugin-synthesized route param, with an intermediate defaults hash
    // between path and handler — the H8-1 shape.
    let route = r#"
use Mojolicious::Lite -signatures;

any '/*whatever' => {whatever => ''} => sub ($c) {
    $c->render(text => 'ok');
    my $cb = sub ($err) {
        $c->render(data => $err, status => 400);
    };
};
"#;
    let fa = build_fa(route);
    // `$c` in the outer route body (row 4) and inside the nested
    // `->catch`-style closure body (row 6) both resolve to the
    // controller.
    for (row, col) in [(4usize, 4usize), (6, 8)] {
        let ty = fa.inferred_type_via_bag("$c", Point::new(row, col));
        assert_eq!(
            ty.and_then(|t| t.class_name().map(str::to_string)),
            Some("Mojolicious::Controller".to_string()),
            "route-param typed $c should resolve to Mojolicious::Controller at {row}:{col} \
             (intermediate defaults hash must not steal the @handler capture)",
        );
    }
}

/// An assignment whose value this tier cannot type still HAPPENED: the
/// variable's earlier class must not stand past it. Before the rebind the
/// class answers; after it, nothing does — the honest unknown, not a
/// stale `Foo`.
#[test]
fn untyped_reassignment_resets_the_earlier_type() {
    let src = r#"
package Foo;
sub new { bless {}, shift }

package main;
my $x = Foo->new;
$x->m;
$x = compute($x);
$x->n;
my $y = Foo->new;
$y = Foo->new;
$y->m;
sub g { my $o = Foo->new; $o = compute($o); return $o }
sub h { my $o = Foo->new; return $o }
my $z = Foo->new;
$z = {};
$z->{k};
"#;
    let fa = build_fa(src);
    let class_at = |var: &str, row: usize| {
        fa.inferred_type_via_bag(var, Point::new(row, 0))
            .and_then(|t| t.class_name().map(str::to_string))
    };
    assert_eq!(class_at("$x", 6), Some("Foo".into()), "typed before the rebind");
    assert_eq!(class_at("$x", 8), None, "an untypable rebind resets the value");
    assert_eq!(class_at("$y", 11), Some("Foo".into()), "a typed rebind keeps typing");
    // The reset is a VALUE inside the chase: a return arm reading the reset
    // variable makes the sub's return unknown rather than falling back to
    // whatever else resolved — and the boundary never renders it.
    let ret = |name: &str| {
        fa.sub_return_type_at_arity(name, None)
            .and_then(|t| t.class_name().map(str::to_string))
    };
    assert_eq!(ret("h"), Some("Foo".into()));
    assert_eq!(ret("g"), None, "a reset arm is unknown, not the earlier class");
    // A TYPED reassignment resets too: the class axis does not outlive
    // `$z = {}` just because a class beats a rep in any order.
    assert_eq!(class_at("$z", 16), None, "a hash reassignment ends the class");
    assert_eq!(
        fa.inferred_type_via_bag("$z", Point::new(16, 0)),
        Some(InferredType::HashRef)
    );
}

/// The honest boundary: a helper name that is NOT statically decidable —
/// a function call (`compute()`), or an interpolation over an unknown
/// variable (`"x_$unknown"`) — synthesizes NOTHING. No guess, no
/// fabricated symbol named `compute` / `x_$unknown`.
#[test]
fn plugin_mojo_helpers_undecidable_name_synthesizes_nothing() {
    let src = r#"
package MyApp::Plugin::Dyn;
use Mojo::Base 'Mojolicious::Plugin';
sub register {
    my ($self, $app) = @_;
    $app->helper(compute() => sub { my ($c) = @_; return 1; });
    $app->helper("x_$unknown" => sub { my ($c) = @_; return 2; });
    $app->helper($runtime => sub { my ($c) = @_; return 3; });
}
1;
"#;
    let fa = build_fa(src);
    let names: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-helpers"))
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        names.is_empty(),
        "undecidable helper names must synthesize nothing, got {names:?}",
    );
}

// ---- varname-based extraction ----

/// Adversarial: every flavor of `foo` access (plain, element, slice,
/// KV slice, arraylen) must canonicalize to the underlying
/// `$foo`/`@foo`/`%foo` Variable symbol — NOT to "$foo" across the
/// board. TSP exposes the container kind via distinct node types
/// (`container_variable`, `slice_container_variable`,
/// `keyval_container_variable`, `arraylen`) + a `hash:`/`array:`
/// field on the parent. Our job is to route each to the correct
/// declared symbol.
#[test]
fn sigil_disambiguation_across_access_forms() {
    let src = "\
my ($foo, @foo, %foo);
$foo;
$foo[0];
$foo{hi};
@foo[0..1];
@foo{qw/hi there/};
$#foo;
%foo[0..1];
%foo{a};
";
    let fa = build_fa(src);

    // Three distinct declarations.
    let decls: std::collections::HashMap<&str, _> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Variable
                && s.scope == ScopeId(0)
                && matches!(s.name.as_str(), "$foo" | "@foo" | "%foo")
        })
        .map(|s| (s.name.as_str(), s.id))
        .collect();
    assert!(decls.contains_key("$foo"), "missing scalar decl");
    assert!(decls.contains_key("@foo"), "missing array decl");
    assert!(decls.contains_key("%foo"), "missing hash decl");

    // Collect every Variable/ContainerAccess ref, keyed by the line
    // it sits on. Line 0 is the declaration — skip it.
    let mut refs_by_line: std::collections::HashMap<usize, Vec<&str>> = Default::default();
    for r in fa.refs() {
        if !matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess) {
            continue;
        }
        if r.access == AccessKind::Declaration {
            continue;
        }
        refs_by_line
            .entry(r.span.start.row)
            .or_default()
            .push(r.target_name.as_str());
    }

    let expected: &[(usize, &str, &str)] = &[
        (1, "$foo", "$foo"),               // plain scalar
        (2, "$foo[0]", "@foo"),            // array element access
        (3, "$foo{hi}", "%foo"),           // hash element access
        (4, "@foo[0..1]", "@foo"),         // array slice
        (5, "@foo{qw/hi there/}", "%foo"), // hash slice — Perl semantic
        (6, "$#foo", "@foo"),              // arraylen
        (7, "%foo[0..1]", "@foo"),         // KV slice of array (5.20+)
        (8, "%foo{a}", "%foo"),            // KV slice of hash
    ];

    let mut failures: Vec<String> = Vec::new();
    for (line, form, want) in expected {
        let got = refs_by_line.get(line).cloned().unwrap_or_default();
        if got.as_slice() != [*want] {
            failures.push(format!(
                "  line {} `{}` → want [{}], got {:?}",
                line, form, want, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "sigil disambiguation failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn braced_var_declaration_names_match_bare_form() {
    // `my ${foo}` is just `my $foo`. Before the varname refactor we
    // stored the declared name as the full node text `${foo}`, so a
    // later `$foo` reference couldn't resolve to it. Now both share
    // the canonical `$foo` name.
    let fa = build_fa("my ${foo} = 1;\n$foo;\n");
    let decls: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Variable && s.name == "$foo")
        .collect();
    assert_eq!(
        decls.len(),
        1,
        "expected one $foo symbol, got {:?}",
        fa.symbols()
            .iter()
            .filter(|s| s.kind == SymKind::Variable)
            .map(|s| &s.name)
            .collect::<Vec<_>>()
    );
}

// ---- parse_instance_of ----

#[test]
fn parse_instance_of_single_quoted() {
    assert_eq!(
        parse_instance_of("InstanceOf['Foo::Bar']").as_deref(),
        Some("Foo::Bar")
    );
}

#[test]
fn parse_instance_of_double_quoted() {
    assert_eq!(
        parse_instance_of("InstanceOf[\"Foo::Bar\"]").as_deref(),
        Some("Foo::Bar")
    );
}

#[test]
fn parse_instance_of_rejects_non_instance_of() {
    assert_eq!(parse_instance_of("Str"), None);
    assert_eq!(parse_instance_of("ArrayRef[Int]"), None);
    assert_eq!(parse_instance_of("My::Class"), None);
}

// ---- Scope tests ----

#[test]
fn test_file_scope() {
    let fa = build_fa("my $x = 1;");
    assert_eq!(fa.scopes.len(), 1);
    assert_eq!(fa.scopes[0].kind, ScopeKind::File);
}

#[test]
fn test_sub_creates_scope() {
    let fa = build_fa("sub foo { my $x = 1; }");
    let sub_scopes: Vec<_> = fa
        .scopes
        .iter()
        .filter(|s| matches!(&s.kind, ScopeKind::Sub { name } if name == "foo"))
        .collect();
    assert_eq!(sub_scopes.len(), 1);
    assert_eq!(sub_scopes[0].parent, Some(ScopeId(0))); // parent is file
}

#[test]
fn test_class_creates_scope() {
    let fa = build_fa("use v5.38;\nclass Point {\n    field $x :param;\n}");
    let class_scopes: Vec<_> = fa
        .scopes
        .iter()
        .filter(|s| matches!(&s.kind, ScopeKind::Class { name } if name == "Point"))
        .collect();
    assert_eq!(class_scopes.len(), 1);
    assert_eq!(class_scopes[0].package, Some("Point".to_string()));
}

#[test]
fn test_package_sets_scope_package() {
    let fa = build_fa("package Foo;\nsub bar { 1 }");
    // The sub scope should inherit package "Foo"
    let sub_scopes: Vec<_> = fa
        .scopes
        .iter()
        .filter(|s| matches!(&s.kind, ScopeKind::Sub { name } if name == "bar"))
        .collect();
    assert_eq!(sub_scopes.len(), 1);
    assert_eq!(sub_scopes[0].package, Some("Foo".to_string()));
}

#[test]
fn test_for_loop_scope() {
    let fa = build_fa("for my $i (1..10) { print $i; }");
    let for_scopes: Vec<_> = fa
        .scopes
        .iter()
        .filter(|s| matches!(&s.kind, ScopeKind::ForLoop { .. }))
        .collect();
    assert_eq!(for_scopes.len(), 1);
}

// ---- Symbol tests ----

#[test]
fn test_variable_symbol() {
    let fa = build_fa("my $x = 1;");
    let vars: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Variable && s.name == "$x")
        .collect();
    assert_eq!(vars.len(), 1);
    if let SymbolDetail::Variable { sigil, decl_kind } = &vars[0].detail {
        assert_eq!(*sigil, '$');
        assert_eq!(*decl_kind, DeclKind::My);
    } else {
        panic!("expected Variable detail");
    }
}

#[test]
fn test_sub_symbol_with_params() {
    let fa = build_fa("sub connect($self, %opts) { }");
    let subs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Sub && s.name == "connect")
        .collect();
    assert_eq!(subs.len(), 1);
    if let SymbolDetail::Sub {
        params, is_method, ..
    } = &subs[0].detail
    {
        assert!(!is_method);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "$self");
        assert_eq!(params[1].name, "%opts");
        assert!(params[1].is_slurpy);
    } else {
        panic!("expected Sub detail");
    }
}

#[test]
fn test_legacy_sub_params() {
    let fa = build_fa("sub new {\n    my ($class, %args) = @_;\n}");
    let subs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Sub && s.name == "new")
        .collect();
    assert_eq!(subs.len(), 1);
    if let SymbolDetail::Sub { params, .. } = &subs[0].detail {
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "$class");
        assert_eq!(params[1].name, "%args");
        assert!(params[1].is_slurpy);
    } else {
        panic!("expected Sub detail");
    }
}

#[test]
fn test_package_symbol() {
    let fa = build_fa("package Foo;");
    let pkgs: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Package && s.name == "Foo")
        .collect();
    assert_eq!(pkgs.len(), 1);
}

#[test]
fn test_class_symbol() {
    let fa = build_fa("use v5.38;\nclass Point {\n    field $x :param;\n    field $y :param;\n    method magnitude() { }\n}");
    let classes: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Class && s.name == "Point")
        .collect();
    assert_eq!(classes.len(), 1);
    if let SymbolDetail::Class { fields, parent, .. } = &classes[0].detail {
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "$x");
        assert_eq!(fields[1].name, "$y");
        assert!(fields[0].attributes.contains(&"param".to_string()));
        assert!(parent.is_none());
    } else {
        panic!("expected Class detail");
    }
}

#[test]
fn test_field_symbol() {
    let fa = build_fa("use v5.38;\nclass Point {\n    field $x :param;\n}");
    let fields: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Field)
        .collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "$x");
}

#[test]
fn test_field_reader_synthesizes_method() {
    let fa = build_fa(
        "use v5.38;\nclass Point {\n    field $x :param :reader;\n    field $y :param;\n}",
    );
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Method)
        .collect();
    assert_eq!(
        methods.len(),
        1,
        "got: {:?}",
        methods.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert_eq!(methods[0].name, "x");
}

#[test]
fn test_implicit_self_in_method() {
    // $self is implicitly available in Perl 5.38 method blocks
    let source = "use v5.38;\nclass Point {\n    field $x :param :reader;\n    method magnitude () {\n        $self->x;\n    }\n}\n";
    let fa = build_fa(source);

    // $self should be resolvable as a variable inside the method
    let resolved = fa.resolve_variable("$self", Point::new(4, 8));
    assert!(
        resolved.is_some(),
        "$self should resolve inside method body"
    );
}

#[test]
fn test_implicit_self_type_inference() {
    // $self should be type-inferred to the enclosing class
    let source = "use v5.38;\nclass Point {\n    field $x :param :reader;\n    method magnitude () {\n        $self->x;\n    }\n}\n";
    let fa = build_fa(source);

    // Type inference: $self → Point
    let inferred = fa.inferred_type_via_bag("$self", Point::new(4, 8));
    assert!(inferred.is_some(), "$self type should be inferred");
    match inferred.unwrap() {
        InferredType::ClassName(name) => assert_eq!(name, "Point"),
        InferredType::FirstParam { package } => assert_eq!(package, "Point"),
        other => panic!("expected ClassName or FirstParam, got {:?}", other),
    }
}

#[test]
fn test_invocant_class_survives_nested_hash_access() {
    // A conventional invocant accessed as `$self->{k}` inside a nested block
    // observes HashRef *there*, but the invocant's ClassName lives on the sub
    // scope. Identity must dominate the inner-scope rep projection — no
    // framework needed (Perl method-ness is conventional). Regression: this
    // returned HashRef, so `$self->` completed a lone hash-key item instead
    // of the class's methods.
    let source = "package Widget;\n\
                  sub new { my $class = shift; bless {}, $class }\n\
                  sub helper { my ($self) = @_; return 1; }\n\
                  sub run {\n\
                  \x20   my ($self) = @_;\n\
                  \x20   {\n\
                  \x20       my $x = $self->{flag};\n\
                  \x20       $self->helper;\n\
                  \x20   }\n\
                  }\n";
    let fa = build_fa(source);
    // `$self` at the `$self->helper` call (row 7), inside the nested block,
    // after the `$self->{flag}` read on the previous line.
    let inferred = fa.inferred_type_via_bag("$self", Point::new(7, 8));
    match inferred {
        Some(InferredType::ClassName(name)) => assert_eq!(name, "Widget"),
        Some(InferredType::FirstParam { package }) => assert_eq!(package, "Widget"),
        other => panic!("expected Widget class identity, got {:?}", other),
    }
}

#[test]
fn test_invocant_class_survives_nested_hash_access_use_base() {
    // Same rule for a `use base` class (framework None) — the case Bugzilla's
    // `use base qw(Bugzilla::Object Exporter)` modules hit.
    let source = "package Bug;\n\
                  use base qw(Obj);\n\
                  sub thing { my ($self) = @_; return 1; }\n\
                  sub run {\n\
                  \x20   my ($self) = @_;\n\
                  \x20   if ($self->{error}) {\n\
                  \x20       $self->thing;\n\
                  \x20   }\n\
                  }\n";
    let fa = build_fa(source);
    let inferred = fa.inferred_type_via_bag("$self", Point::new(6, 8));
    match inferred {
        Some(InferredType::ClassName(name)) => assert_eq!(name, "Bug"),
        Some(InferredType::FirstParam { package }) => assert_eq!(package, "Bug"),
        other => panic!("expected Bug class identity, got {:?}", other),
    }
}

#[test]
fn test_genuine_inner_scope_hashref_binding_still_wins() {
    // Guard: identity-over-rep defers only a *rep-observation-only* inner
    // scope. An inner scope that actually BINDS the variable to a hashref
    // (`my $h = { ... }` in a closure) stays authoritative — `scope_binds_
    // variable` returns it immediately rather than falling out to any outer
    // class identity.
    let source = "package P;\n\
                  sub run {\n\
                  \x20   my ($self) = @_;\n\
                  \x20   my $cb = sub {\n\
                  \x20       my $h = { a => 1 };\n\
                  \x20       return $h->{a};\n\
                  \x20   };\n\
                  }\n";
    let fa = build_fa(source);
    // `$h` at its use (row 5, `return $h->{a}`) is the hashref literal.
    let inferred = fa.inferred_type_via_bag("$h", Point::new(5, 14));
    assert!(
        inferred.as_ref().is_some_and(|t| t.is_hash_shaped()),
        "an explicit inner-scope hashref binding must stay hash-shaped, got {:?}",
        inferred
    );
}

#[test]
fn test_self_completion_walks_ancestors_in_fallback() {
    // Untyped `$self` (the fallback path, no bag type — e.g. assigned via
    // `$class->SUPER::new`) must still resolve to the enclosing class AND walk
    // its ancestors, so inherited methods are offered, not just own ones.
    let source = "package Base;\nsub inherited_m { 1 }\npackage Child;\nuse parent -norequire, 'Base';\nsub own_m {\n  my $self = $class->SUPER::new;\n  $self->\n}\n";
    let fa = build_fa(source);
    let names: Vec<String> = fa
        .complete_methods("$self", Point::new(6, 9), None)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(names.iter().any(|n| n == "own_m"), "own method missing: {names:?}");
    assert!(
        names.iter().any(|n| n == "inherited_m"),
        "inherited (ancestor) method missing from untyped-$self fallback: {names:?}"
    );
}

#[test]
fn test_self_completion_inside_method() {
    // $self-> inside a method should complete with sibling methods
    let source = "use v5.38;\nclass Point {\n    field $x :param :reader;\n    method magnitude () { }\n    method to_string () {\n        $self->;\n    }\n}\n";
    let fa = build_fa(source);

    let candidates = fa.complete_methods("$self", Point::new(5, 14), None);
    let names: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(
        names.contains(&"magnitude"),
        "missing magnitude, got: {:?}",
        names
    );
    assert!(
        names.contains(&"to_string"),
        "missing to_string, got: {:?}",
        names
    );
    assert!(names.contains(&"x"), "missing reader x, got: {:?}", names);
}

#[test]
fn test_field_writer_synthesizes_method() {
    let fa =
        build_fa("use v5.38;\nclass Point {\n    field $label :reader :writer = \"point\";\n}");
    let methods: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Method)
        .map(|s| s.name.clone())
        .collect();
    assert!(
        methods.contains(&"label".to_string()),
        "missing reader, got: {:?}",
        methods
    );
    assert!(
        methods.contains(&"set_label".to_string()),
        "missing writer, got: {:?}",
        methods
    );
}

#[test]
fn test_complete_methods_in_class() {
    let fa = build_fa("use v5.38;\nclass Point {\n    field $x :param :reader;\n    field $y :param;\n    method magnitude() { }\n    method to_string() { }\n}\nmy $p = Point->new(x => 1);\n$p->;\n");
    // $p-> is at line 8, col 4
    let candidates = fa.complete_methods("$p", Point::new(8, 4), None);
    let names: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"new"), "missing new, got: {:?}", names);
    assert!(
        names.contains(&"magnitude"),
        "missing magnitude, got: {:?}",
        names
    );
    assert!(
        names.contains(&"to_string"),
        "missing to_string, got: {:?}",
        names
    );
    assert!(names.contains(&"x"), "missing reader x, got: {:?}", names);
}

#[test]
fn test_complete_methods_sample_file_layout() {
    // Matches sample.pl: class defined after package main, $p usage at end
    let source = r#"use v5.38;
class Point {
    field $x :param :reader;
    field $y :param;
    method magnitude () { }
    method to_string () { }
}
my $p = Point->new(x => 3, y => 4);
$p->;
"#;
    let fa = build_fa(source);

    // Check type inference resolved $p → Point
    let inferred = fa.inferred_type_via_bag("$p", Point::new(8, 4));
    assert!(inferred.is_some(), "type inference for $p should resolve");

    let candidates = fa.complete_methods("$p", Point::new(10, 4), None);
    let names: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(
        names.contains(&"magnitude"),
        "missing magnitude, got: {:?}",
        names
    );
    assert!(
        names.contains(&"to_string"),
        "missing to_string, got: {:?}",
        names
    );
    assert!(names.contains(&"x"), "missing reader x, got: {:?}", names);
}

#[test]
fn test_complete_methods_class_after_package_main() {
    // Real-world: package main; ... class Point {} ... $p->
    let source = r#"package main;
my $calc = Calculator->new();
1;
use v5.38;
class Point {
    field $x :param :reader;
    field $y :param;
    method magnitude () { }
    method to_string () { }
}
my $p = Point->new(x => 3, y => 4);
$p->;
"#;
    let fa = build_fa(source);

    let candidates = fa.complete_methods("$p", Point::new(11, 4), None);
    let names: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"new"), "missing new, got: {:?}", names);
    assert!(
        names.contains(&"magnitude"),
        "missing magnitude, got: {:?}",
        names
    );
    assert!(
        names.contains(&"to_string"),
        "missing to_string, got: {:?}",
        names
    );
    assert!(names.contains(&"x"), "missing reader x, got: {:?}", names);
}

#[test]
fn test_complete_methods_flat_class() {
    // class Foo; (no block) — methods follow as siblings, like package
    let source = "use v5.38;\nclass Foo;\nmethod bar () { }\nmethod baz () { }\n";
    let fa = build_fa(source);
    let candidates = fa.complete_methods("Foo", Point::new(3, 0), None);
    let names: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(names.contains(&"bar"), "missing bar, got: {:?}", names);
    assert!(names.contains(&"baz"), "missing baz, got: {:?}", names);
}

#[test]
fn test_goto_def_method_after_package_main() {
    // go-to-def on $p->magnitude() should find the method, not the class
    let source = "package main;\n1;\nuse v5.38;\nclass Point {\n    field $x :param :reader;\n    method magnitude () { }\n}\nmy $p = Point->new(x => 3);\n$p->magnitude();\n";
    let fa = build_fa(source);
    // cursor on `magnitude` in `$p->magnitude()` — line 8, col 5
    let def = fa.find_definition(Point::new(8, 5), None);
    assert!(def.is_some(), "should find definition for magnitude");
    let span = def.unwrap();
    assert_eq!(
        span.start.row, 5,
        "should point to method declaration line, got row {}",
        span.start.row
    );
}

#[test]
fn test_field_reader_goto_def() {
    // go-to-def on $p->x should find the reader method, which points to the field
    let fa = build_fa("use v5.38;\nclass Point {\n    field $x :param :reader;\n    method mag() { }\n}\nmy $p = Point->new(x => 1);\n$p->x;");
    let def = fa.find_definition(Point::new(6, 5), None); // cursor on `x` in `$p->x`
    assert!(def.is_some(), "should find definition for reader method");
    // The reader method's selection_span points to the field declaration
    let span = def.unwrap();
    assert_eq!(span.start.row, 2, "should point to field declaration line");
}

/// NAV (a): goto-def on a method that does NOT exist on a known class
/// must be an honest miss (None), NEVER a confident jump to the
/// `package` decl. The `$self->{email}->method` shape used to over-type
/// the invocant to the enclosing class and then jump to its package
/// line when the method wasn't found — worse than a miss.
#[test]
fn test_goto_def_unknown_method_is_honest_miss_not_package_jump() {
    let source = "package Foo;\nsub new { bless { email => undef }, shift }\nsub to { my $self = shift; $self->{email}->totallyunknownmethod(1); }\n1;\n";
    let fa = build_fa(source);
    // `totallyunknownmethod` starts at row 2, col 43 (after the `->`).
    let def = fa.find_definition(Point::new(2, 48), None);
    assert!(
        def.is_none(),
        "unknown method must return None (honest miss), not jump to package decl; got {:?}",
        def
    );
}

/// NAV regression (iv): goto-def on a typed same-file method still
/// lands on the method declaration via the frozen dispatch edge.
#[test]
fn test_goto_def_typed_same_file_method_resolves() {
    let source = "package Widget;\nsub new { bless {}, shift }\nsub frobnicate { 1 }\nsub run { my $w = Widget->new; $w->frobnicate; }\n1;\n";
    let fa = build_fa(source);
    // `frobnicate` in `$w->frobnicate` — row 3. Find its column.
    let row = 3usize;
    let line = source.lines().nth(row).unwrap();
    let col = line.find("$w->frobnicate").unwrap() + "$w->".len() + 2;
    let def = fa.find_definition(Point::new(row, col), None);
    assert!(def.is_some(), "typed $w->frobnicate must resolve to the decl");
    assert_eq!(
        def.unwrap().start.row,
        2,
        "should land on `sub frobnicate` (row 2)"
    );
}

/// OVER-TYPING PIN: a hash element extracted to a scalar must NOT be
/// typed as the container's class. `my $h = $self->{helper}` carries
/// the value of `$self->{helper}`, whose type is independent of
/// `$self`'s class (Foo). The chain typer used to push a spurious
/// `TypeConstraint $h = Foo` because `$self->{helper}` resolved to
/// `$self`'s class. `$h` must be honest-UNTYPED (no real value type is
/// known), and `$h->do_thing` must be an honest miss — not a
/// confident-wrong jump to a Foo sub.
#[test]
fn test_hash_element_extracted_to_scalar_is_not_container_class() {
    let source = "package Foo;\nsub new { bless {}, shift }\nsub use_it { my $self = shift; $self->{helper} = Helper->new; my $h = $self->{helper}; $h->do_thing(); }\n1;\n";
    let fa = build_fa(source);

    // `$h` is declared on row 2. Probe just past its declaration.
    let line = source.lines().nth(2).unwrap();
    let h_decl_col = line.find("my $h").unwrap();
    let probe = tree_sitter::Point::new(2, h_decl_col + "my $h = $self->{helper}; ".len());

    let ty = fa.inferred_type("$h", probe);
    assert!(
        !matches!(ty, Some(InferredType::ClassName(c)) if c == "Foo"),
        "$h must NOT be typed as the container's class Foo; got {:?}",
        ty
    );

    // goto-def on `$h->do_thing` must be an honest miss, never a jump
    // to a Foo sub.
    let do_thing_col = line.rfind("do_thing").unwrap();
    let def = fa.find_definition(tree_sitter::Point::new(2, do_thing_col + 1), None);
    assert!(
        def.is_none(),
        "$h->do_thing must be an honest miss, not a confident jump to a Foo sub; got {:?}",
        def
    );
}

/// A4 end-to-end (Step 3 consume join): a typed write into a slot, extracted
/// to a scalar, types the scalar via `SlotType` — so a method call on it
/// resolves. Helper is defined here so resolution can complete (contrast the
/// honest-miss pin above, where it isn't).
#[test]
fn slot_type_write_then_extract_resolves_method() {
    let source = "package Helper;\nsub new { bless {}, shift }\nsub do_thing { 1 }\npackage Foo;\nsub new { bless {}, shift }\nsub use_it { my $self = shift; $self->{helper} = Helper->new; my $h = $self->{helper}; $h->do_thing(); }\n1;\n";
    let fa = build_fa(source);
    let line = source.lines().nth(5).unwrap();

    let probe = tree_sitter::Point::new(
        5,
        line.find("my $h").unwrap() + "my $h = $self->{helper}; ".len(),
    );
    let ty = fa.inferred_type("$h", probe);
    assert_eq!(
        ty.as_ref().and_then(|t| t.class_name()),
        Some("Helper"),
        "$h must type as Helper via the consumed SlotType; got {:?}",
        ty
    );

    let def = fa.find_definition(
        tree_sitter::Point::new(5, line.rfind("do_thing").unwrap() + 1),
        None);
    assert!(
        matches!(&def, Some(d) if d.start.row == 2),
        "$h->do_thing must resolve to Helper::do_thing on row 2; got {:?}",
        def
    );
}


#[test]
fn test_use_symbol() {
    let fa = build_fa("use Foo::Bar;");
    let modules: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.kind == SymKind::Module)
        .collect();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "Foo::Bar");
}

/// The depth gate: a CST past `MAX_CST_DEPTH` gets NO analysis and SAYS SO
/// via a `cst-too-deep` diagnostic. Ordinary files far below the cap are
/// untouched.
///
/// The gate no longer stands between the walk and a stack overflow — the walk
/// is iterative, and `deep_file_gets_a_real_analysis` pins that a file well
/// past the old overflow depth is analyzed for real. What is pinned here is
/// the honest-degradation contract for input past the sanity bound.
#[test]
fn depth_gate_degrades_honestly() {
    let over = crate::build::builder::pipeline::MAX_CST_DEPTH + 10;
    let deep = format!("my $x =\n{}1{};\n1;\n", "[".repeat(over), "]".repeat(over));
    let fa = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || build_fa(&deep))
        .expect("spawn")
        .join()
        .expect("the gate must screen this without overflowing");
    assert!(fa.symbols().is_empty(), "gated file must carry no symbols");
    assert!(fa.refs().is_empty(), "gated file must carry no refs");
    assert_eq!(fa.plugin.diagnostics.len(), 1);
    let d = &fa.plugin.diagnostics[0];
    assert_eq!(d.code, "cst-too-deep");
    assert!(
        d.message.contains("analysis skipped"),
        "diagnostic must state the skip: {}",
        d.message
    );

    // Control: same shape under the cap analyzes normally.
    let shallow = "my $x = [[1]];\nsub greet { 1 }\n";
    let fa = build_fa(shallow);
    assert!(fa.plugin.diagnostics.is_empty());
    assert!(fa.symbols().iter().any(|s| s.name == "greet"));
}

/// The point of the iterative walk: a file far deeper than the recursive
/// descent could survive gets a REAL analysis, not the degraded one.
///
/// 4,000 nested `[` is ~2.2× the depth at which the recursive walk aborted
/// (measured: real analysis at 1,803, fatal stack overflow by 2,503, release
/// build, 2 MiB stack). Base-verify by reverting to the recursive walk — the
/// abort is a process abort, not a test failure, so a green run here is only
/// meaningful because that abort is what it replaced.
///
/// Runs on a 2 MiB stack explicitly, matching a rayon worker: the harness's
/// own thread size is not something this test should depend on.
#[test]
fn deep_file_gets_a_real_analysis() {
    const NEST: usize = 4_000;
    let src = format!("my $deep =\n{}42{};\nsub marker {{ 7 }}\n", "[".repeat(NEST), "]".repeat(NEST));
    let fa = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || build_fa(&src))
        .expect("spawn")
        .join()
        .expect("the walk must not overflow the stack");

    assert!(
        fa.plugin.diagnostics.iter().all(|d| d.code != "cst-too-deep"),
        "a file this deep must be analyzed, not refused: {:?}",
        fa.plugin.diagnostics
    );
    // Real analysis means the walk reached BOTH ends of the file: the
    // declaration before the deep literal and the sub after it.
    assert!(
        fa.symbols().iter().any(|s| s.name == "marker"),
        "the sub after the deep literal must be reached"
    );
    assert!(
        fa.refs().iter().any(|r| r.target_name == "$deep"),
        "the declaration before the deep literal must emit its ref"
    );
}


/// The two walks agree, over every Perl file checked into the repo.
///
/// This is the landed half of the equivalence proof. The other half is the
/// whole-suite sweep — `PERL_LSP_WALK_EQUIV=1 cargo test` re-builds every file
/// any test touches both ways — which covers far more visitor arms but has to
/// be asked for. This one runs by default, so the recursive descent stays
/// exercised and cannot quietly rot into something that no longer agrees.
///
/// Compared as serde projections rather than bincode bytes: `HashMap`
/// iteration order differs between two builds in one thread, and would report
/// differences that are not there. See `walk::assert_walks_agree`.
#[test]
fn walk_equivalence_over_repo_fixtures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_perl_files(root, &mut files, 0);
    assert!(
        files.len() > 50,
        "expected the repo's fixture corpus, found {} files",
        files.len()
    );

    let mut compared = 0usize;
    for path in &files {
        let Ok(src) = std::fs::read(path) else { continue };
        let tree = {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
            match p.parse(&src, None) {
                Some(t) => t,
                None => continue,
            }
        };
        // The deep fixtures are past what the recursive walk can survive at
        // all — that is the point of them, and `deep_file_gets_a_real_analysis`
        // is where they are covered.
        if crate::build::builder::pipeline::cst_depth(&tree) > 256 {
            continue;
        }
        let plugins = crate::build::plugin::default_plugin_registry();
        let iterative =
            crate::build::builder::walk::with_walk_mode(false, || {
                build_with_plugins(&tree, &src, plugins.clone())
            });
        let recursive =
            crate::build::builder::walk::with_walk_mode(true, || {
                build_with_plugins(&tree, &src, plugins.clone())
            });
        crate::build::builder::walk::assert_walks_agree(&iterative, || recursive);
        compared += 1;
    }
    assert!(compared > 50, "only {} files actually compared", compared);
}

fn collect_perl_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // `target/` is build output and `gold-corpus/local/` is the CPAN
            // substrate — neither is this repo's source, and both are huge.
            if name == "target" || name == "local" || name == ".git" {
                continue;
            }
            collect_perl_files(&path, out, depth + 1);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("pm") | Some("pl") | Some("t")
        ) {
            out.push(path);
        }
    }
}

// ---- builtin call keyword refs (rule 7) ----

/// A builtin call's keyword gets its own ref, bound to CORE. Without it,
/// `ref_at` at an invocant-position builtin (`shift->SUPER::new`) fell
/// through to the ENCLOSING MethodCall: goto-def answered the method from
/// a token nobody minted while references answered nothing — the
/// projection disagreement the consistency net carried as its KNOWN pair.
/// CORE (the namespace Perl itself gives builtins) keeps the identity from
/// ever cross-linking to a user sub of the same name, and rename declines
/// it: the language owns the name.
#[test]
fn builtin_call_keyword_gets_a_core_bound_ref() {
    let src = "my @x = (1);\nmy $a = shift @x;\nshift(@x)->foo;\nmy $u = uc($a);\nmy $t = time;\n";
    let fa = build_fa(src);
    let core_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| {
            matches!(r.kind, RefKind::FunctionCall) && r.resolved_package() == Some("CORE")
        })
        .map(|r| (r.target_name.as_str(), r.span.start.row, r.span.start.column))
        .collect();
    for want in [("shift", 1, 8), ("shift", 2, 0), ("uc", 3, 8), ("time", 4, 8)] {
        assert!(
            core_refs.contains(&want),
            "missing CORE-bound builtin ref {want:?}; got {core_refs:?}",
        );
    }

    // Narrowest-span (rule 7's tiebreaker): the invocant-position cursor
    // now resolves the BUILTIN, not the enclosing method call.
    let r = fa.ref_at(Point { row: 2, column: 2 }).expect("ref at invocant");
    assert_eq!(r.target_name, "shift", "invocant cursor claims the builtin: {:?}", r.kind);

    // And a builtin is not renameable — prepareRename must refuse rather
    // than offer to rewrite the language's own vocabulary.
    assert!(
        fa.rename_kind_at(Point { row: 2, column: 2 }, None).is_none(),
        "a CORE-bound builtin call must not mint a rename kind",
    );
}
