use super::*;

// ---- Ref tests ----

#[test]
fn test_variable_ref() {
    let fa = build_fa("my $x = 1;\nprint $x;");
    let var_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "$x" && matches!(r.kind, RefKind::Variable))
        .collect();
    // One declaration ref + one read ref
    assert!(var_refs.len() >= 2, "got {} refs for $x", var_refs.len());
    assert!(var_refs.iter().any(|r| r.access == AccessKind::Declaration));
    assert!(var_refs.iter().any(|r| r.access == AccessKind::Read));
}

#[test]
fn test_function_call_ref() {
    let fa = build_fa("sub foo { }\nfoo();");
    let call_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "foo" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .collect();
    assert_eq!(call_refs.len(), 1);
}

/// Rule #7: a call that appears as an *operand* of a larger expression
/// (string concatenation, ternary) must still emit its FunctionCall ref.
/// AWStats shape `print "<td>".Format_Number($x)."</td>"` parses the call
/// as a `function_call_expression` nested inside a `binary_expression`
/// (the concat) which is itself the `print` verb's argument. The generic
/// `_ => visit_children` traversal in `visit_node` reaches it; this test
/// pins that so a future grammar/traversal change can't silently regress
/// references to statement-level calls only.
#[test]
fn call_ref_in_concatenation_operand() {
    let fa = build_fa("sub Format_Number { }\nprint \"<td>\".Format_Number($x).\"</td>\";\n");
    let call_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "Format_Number" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .collect();
    assert_eq!(
        call_refs.len(),
        1,
        "a call inside `.`-concatenation must emit exactly one FunctionCall ref"
    );
    // The ref must pin the *call name*, not the surrounding concat/print.
    assert!(call_refs[0].span.start.row == 1);
}

/// Rule #7: calls in both arms of a ternary are operands too.
#[test]
fn call_ref_in_ternary_operands() {
    let fa = build_fa("sub foo { }\nsub bar { }\nmy $y = $cond ? foo() : bar();\n");
    let foo_refs = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "foo" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .count();
    let bar_refs = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "bar" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .count();
    assert_eq!(foo_refs, 1, "ternary consequent call must emit a ref");
    assert_eq!(bar_refs, 1, "ternary alternative call must emit a ref");
}

/// Method calls nested in expression operands must also emit a MethodCall ref.
#[test]
fn method_call_ref_in_concatenation_operand() {
    let fa = build_fa("my $s = \"x\" . $obj->fmt($n) . \"y\";\n");
    let m_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "fmt" && matches!(r.kind, RefKind::MethodCall { .. }))
        .collect();
    assert_eq!(m_refs.len(), 1, "method call inside concat must emit one MethodCall ref");
}

/// AWStats-shaped fixture: a def plus N call sites, every call embedded in a
/// concatenation operand. Mirrors the real-world undercount (def→6 of 172).
/// Asserts the call-ref count equals the textual occurrence count and that a
/// bareword that is NOT a call (`Format_Number` as a hash key) is not counted.
#[test]
fn call_refs_count_across_expression_positions() {
    let src = "\
sub Format_Number { my $n = shift; return $n; }
print \"<td>\".Format_Number($a).\"</td>\";
print \"<td>\".Format_Number($b).\"</td><td>x</td>\";
my $r = \"a\" . Format_Number($c) . \"b\" . Format_Number($d);
my $t = $cond ? Format_Number($e) : 0;
my %h = (Format_Number => 1);
";
    let fa = build_fa(src);
    let call_refs = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "Format_Number" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .count();
    // 5 genuine call sites (two single-call prints, two in one concat, one ternary).
    assert_eq!(
        call_refs, 5,
        "every call-position occurrence must emit a FunctionCall ref; the hash-key bareword must not"
    );
}

/// Regression guard: a plain statement-level call still emits exactly one
/// ref (no double-emission from the operand-traversal path).
#[test]
fn statement_level_call_emits_single_ref() {
    let fa = build_fa("sub debug { }\ndebug(\"hello\");\n");
    let call_refs = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "debug" && matches!(r.kind, RefKind::FunctionCall { .. }))
        .count();
    assert_eq!(call_refs, 1, "statement-level call must emit exactly one ref");
}

#[test]
fn test_method_call_ref() {
    let fa = build_fa("$obj->method();");
    let method_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "method" && matches!(r.kind, RefKind::MethodCall { .. }))
        .collect();
    assert_eq!(method_refs.len(), 1);
    if let RefKind::MethodCall { ref invocant, .. } = method_refs[0].kind {
        assert_eq!(invocant.text(), "$obj");
    }
}

#[test]
fn test_hash_key_ref() {
    let fa = build_fa("my %h;\n$h{foo};");
    let key_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "foo" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert_eq!(key_refs.len(), 1);
}

// ---- Query tests ----

#[test]
fn test_scope_at() {
    let fa = build_fa("sub foo {\n    my $x = 1;\n}");
    // Point inside the sub body
    let scope = fa.scope_at(Point::new(1, 8)).unwrap();
    let s = fa.scope(scope);
    assert!(matches!(&s.kind, ScopeKind::Sub { name } if name == "foo"));
}

#[test]
fn test_resolve_variable() {
    let fa = build_fa("my $x = 1;\nsub foo {\n    my $x = 2;\n    print $x;\n}");
    // Inside the sub, $x should resolve to the inner declaration
    let sym = fa.resolve_variable("$x", Point::new(3, 10)).unwrap();
    // Inner $x is at line 2
    assert_eq!(sym.selection_span.start.row, 2);
}

#[test]
fn test_resolve_variable_outer() {
    let fa = build_fa("my $x = 1;\nsub foo {\n    print $x;\n}");
    // Inside the sub with no inner $x, should resolve to outer
    let sym = fa.resolve_variable("$x", Point::new(2, 10)).unwrap();
    assert_eq!(sym.selection_span.start.row, 0);
}

#[test]
fn test_type_inference_constructor() {
    let fa = build_fa("use v5.38;\nclass Point { }\nmy $p = Point->new();");
    let ty = fa.inferred_type_via_bag("$p", Point::new(2, 20));
    assert!(ty.is_some(), "should infer type for $p");
    if let Some(InferredType::ClassName(cn)) = ty {
        assert_eq!(cn, "Point");
    } else {
        panic!("expected ClassName, got {:?}", ty);
    }
}

#[test]
fn test_type_inference_first_param() {
    // The walk pushes a `FirstParam { package: "Calculator" }`
    // type-constraint, but the bag-aware query normalises it to
    // `ClassName("Calculator")` via the FrameworkAwareTypeFold
    // (FirstParam is an internal observation; consumers see the
    // class identity). That's the canonical answer the LSP
    // serves at any cursor position on `$self`.
    let fa = build_fa("package Calculator;\nsub new {\n    my ($self) = @_;\n}");
    let ty = fa.inferred_type_via_bag("$self", Point::new(2, 10));
    assert_eq!(ty, Some(InferredType::ClassName("Calculator".into())));
}

#[test]
fn test_bless_promotes_var_to_class() {
    // `my $self = {}; bless $self, $class;` — after the bless, $self is an
    // instance of the enclosing class, not a bare HashRef.
    let src = "package Point;\nsub new {\n  my $class = shift;\n  my $self = {};\n  bless $self, $class;\n  return $self;\n}\n";
    let fa = build_fa(src);
    // Query $self at the `return $self` line (after the bless).
    let ty = fa.inferred_type_via_bag("$self", Point::new(5, 9));
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Point".into())),
        "post-bless $self should be ClassName(Point), got {:?}",
        ty
    );
}

#[test]
fn test_bless_fat_arrow_and_package() {
    // `bless $self => __PACKAGE__` form.
    let src = "package Widget;\nsub build {\n  my $self = {};\n  bless $self => __PACKAGE__;\n  return $self;\n}\n";
    let fa = build_fa(src);
    let ty = fa.inferred_type_via_bag("$self", Point::new(4, 9));
    assert_eq!(ty, Some(InferredType::ClassName("Widget".into())));
}

#[test]
fn test_bless_literal_class() {
    // `bless $self, "Other"` — explicit literal class wins.
    let src = "package Factory;\nsub mk {\n  my $self = {};\n  bless $self, \"Other\";\n  return $self;\n}\n";
    let fa = build_fa(src);
    let ty = fa.inferred_type_via_bag("$self", Point::new(4, 9));
    assert_eq!(ty, Some(InferredType::ClassName("Other".into())));
}

#[test]
fn test_return_bless_anon_hash_class() {
    // `return bless {}, $class` — the sub returns a ClassName instance even
    // though there's no variable to promote.
    let src = "package Maker;\nsub new {\n  my $class = shift;\n  return bless {}, $class;\n}\n";
    let fa = build_fa(src);
    let ty = fa.sub_return_type_at_arity("new", Some(0));
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Maker".into())),
        "return bless should type the sub return, got {:?}",
        ty
    );
}

#[test]
fn test_self_hosting_fluent_computed_receiver_not_paren() {
    // DBIC self-hosting edge: a fluent verb analyzed inside its OWN package
    // does `(ref $self)->new(...)`. That computed receiver is not a literal
    // class — before the fix it froze as `ClassName("(")`, so `search` /
    // `search_rs` reported `raw_return_type: "("`.
    let src = "\
package My::ResultSet;
sub search_rs {
  my $self = shift;
  my $rs = (ref $self)->new($self->{attrs});
  return $rs;
}
sub search {
  my $self = shift;
  my $rs = $self->search_rs(@_);
  return $rs;
}
";
    let fa = build_fa(src);
    for sub in ["search", "search_rs"] {
        for arity in [None, Some(0u32), Some(1), Some(2)] {
            let ty = fa.sub_return_type_at_arity(sub, arity);
            // A `(` is never a type — a plain class name or None is fine.
            if let Some(InferredType::ClassName(ref c)) = ty {
                assert!(
                    crate::model::conventions::is_bareword_class_name(c),
                    "{sub} at arity {arity:?} returned garbage ClassName({c:?})"
                );
            }
        }
    }
}

#[test]
fn test_statement_bless_receiver_types_sub_return() {
    // The Bugzilla::Object idiom: bless in STATEMENT position with a
    // receiver-derived class, returned as a separate statement. The sub's
    // return must type receiver-polymorphically — enclosing class as the
    // no-receiver fallback.
    let src = "package Base;\nsub new {\n  my $invocant = shift;\n  my $class = ref($invocant) || $invocant;\n  my $object = $class->_init(@_);\n  bless($object, $class) if $object;\n  return $object;\n}\nsub _init { return {} }\n";
    let fa = build_fa(src);
    let ty = fa.sub_return_type_at_arity("new", Some(0));
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Base".into())),
        "statement bless with receiver class should type the ctor return, got {:?}",
        ty
    );
}

#[test]
fn test_inherited_statement_bless_ctor_types_to_subclass() {
    // `$class->new` through an inherited statement-bless ctor: the call
    // site's receiver substitutes, so `$self` types as the SUBCLASS.
    let src = "package Base;\nsub new {\n  my $class = shift;\n  my $object = {};\n  bless($object, $class);\n  return $object;\n}\n\npackage Kid;\nuse base qw(Base);\nsub check {\n  my ($class) = @_;\n  my $self = $class->new;\n  return $self;\n}\n";
    let fa = build_fa(src);
    let ty = fa.inferred_type_via_bag("$self", Point::new(13, 9));
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Kid".into())),
        "inherited statement-bless ctor should type to the calling subclass, got {:?}",
        ty
    );
}

#[test]
fn test_inherited_assignment_bless_ctor_types_to_subclass() {
    // Same polymorphism through the assignment form
    // (`my $self = bless {}, $class; return $self`).
    let src = "package Base;\nsub new {\n  my $class = shift;\n  my $self = bless {}, $class;\n  return $self;\n}\n\npackage Kid;\nuse base qw(Base);\nsub check {\n  my ($class) = @_;\n  my $self = $class->new;\n  return $self;\n}\n";
    let fa = build_fa(src);
    let ty = fa.inferred_type_via_bag("$self", Point::new(12, 9));
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Kid".into())),
        "inherited assignment-bless ctor should type to the calling subclass, got {:?}",
        ty
    );
}

#[test]
fn test_statement_bless_receiver_pre_bless_query_keeps_rep() {
    // Temporal honesty: before the bless statement the variable is still
    // the hashref `_init` returned; only queries PAST the bless see the
    // class.
    let src = "package Base;\nsub new {\n  my $class = shift;\n  my $object = {};\n  bless($object, $class);\n  return $object;\n}\n";
    let fa = build_fa(src);
    // Point on the `bless` line's variable read is fine — the witness is
    // at the bless span itself; probe the line BEFORE it.
    let pre = fa.inferred_type_via_bag("$object", Point::new(3, 14));
    assert_eq!(
        pre,
        Some(InferredType::HashRef),
        "pre-bless query must keep the rep type, got {:?}",
        pre
    );
}

#[test]
fn test_bless_into_ref_invocant_types_clone_return() {
    // `bless { ... }, ref $_[0]` (the clone idiom) blesses into the invocant's
    // class, so the implicit-return value types as the enclosing class.
    let src = "package DateTime;\nsub clone { bless { %{ $_[0] } }, ref $_[0] }\n";
    let fa = build_fa(src);
    let ty = fa.sub_return_type_at_arity("clone", Some(1));
    assert_eq!(
        ty,
        Some(InferredType::ClassName("DateTime".into())),
        "bless ..., ref $_[0] should type the clone return, got {:?}",
        ty
    );
}

#[test]
fn test_forward_declaration_does_not_duplicate_symbol() {
    // `sub foo;` is a forward declaration, not a definition: only the bodied
    // `sub foo { ... }` should produce a symbol (no outline dup / goto-def shadow).
    let fa = build_fa("package P;\nsub foo;\nsub foo { my ($self, $x) = @_; $x }\n");
    let foos: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method) && s.name == "foo")
        .collect();
    assert_eq!(
        foos.len(),
        1,
        "expected one `foo` symbol (the definition), got {:?}",
        foos.iter().map(|s| s.span.start.row).collect::<Vec<_>>()
    );
    assert_eq!(foos[0].span.start.row, 2, "the symbol should be the bodied def on line 2");
}

#[test]
fn test_receiver_polymorphic_ctor_types_to_subclass() {
    // An inherited `bless {}, ref $class || $class` constructor returns whatever
    // class it was CALLED ON — Child->new is a Child, not a Base. The bless arm
    // emits ReturnExpr::ReceiverOr; the call's receiver substitutes.
    let fa = build_fa(
        "package Base;\nsub new { my $class = shift; bless {}, ref $class || $class }\npackage Child;\nuse parent -norequire, 'Base';\n",
    );
    assert_eq!(
        fa.find_method_return_type("Child", "new", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "Child->new (inherited ctor) must type as Child, not Base"
    );
    assert_eq!(
        fa.find_method_return_type("Base", "new", None, Some(0)),
        Some(InferredType::ClassName("Base".into())),
        "Base->new still types as Base"
    );
}

#[test]
fn test_non_bless_hashref_stays_hashref() {
    // Regression: a hashref that's never blessed keeps its HashRef type.
    let src = "sub mk {\n  my $h = {};\n  return $h;\n}\n";
    let fa = build_fa(src);
    let ty = fa.inferred_type_via_bag("$h", Point::new(2, 9));
    assert_eq!(ty, Some(InferredType::HashRef), "unblessed hashref stays HashRef");
}

// ---- Literal constructor extraction tests (via build_fa) ----

#[test]
fn test_extract_hashref_literal() {
    let fa = build_fa("my $href = {};");
    let ty = fa.inferred_type_via_bag("$href", Point::new(0, 14));
    assert_eq!(ty, Some(InferredType::HashRef), "empty hash ref literal");

    let fa = build_fa("my $href = { a => 1, b => 2 };");
    let ty = fa.inferred_type_via_bag("$href", Point::new(0, 30));
    assert!(
        ty.is_some_and(|t| t.is_hash_shaped()),
        "populated hash ref literal",
    );
}

#[test]
fn test_extract_arrayref_literal() {
    let fa = build_fa("my $aref = [];");
    let ty = fa.inferred_type_via_bag("$aref", Point::new(0, 14));
    assert_eq!(ty, Some(InferredType::ArrayRef), "empty array ref literal");

    let fa = build_fa("my $aref = [1, 2, 3];");
    let ty = fa.inferred_type_via_bag("$aref", Point::new(0, 21));
    assert!(
        ty.is_some_and(|t| t.is_array_shaped()),
        "populated array ref literal",
    );
}

#[test]
fn test_extract_coderef_literal() {
    let fa = build_fa("my $cref = sub { 42 };");
    let ty = fa.inferred_type_via_bag("$cref", Point::new(0, 22));
    // Sub-literal CodeRef carries `return_edge: Some(_)` — the
    // body's last-expression span. Survives the `my $cref = ...`
    // binding so downstream callable-shape consumers can edge-chase
    // into the body's type. Opaque coderef tests below use `None`.
    assert!(
        matches!(ty, Some(InferredType::CodeRef { return_edge: Some(_) })),
        "anonymous sub: got {:?}",
        ty
    );
}

#[test]
fn test_extract_regexp_literal() {
    let fa = build_fa("my $re = qr/pattern/;");
    let ty = fa.inferred_type_via_bag("$re", Point::new(0, 21));
    assert_eq!(ty, Some(InferredType::Regexp), "qr// literal");
}

#[test]
fn test_extract_reassignment_type_change() {
    let fa = build_fa("my $x = {};\n$x = [];");
    // After line 0 → HashRef
    let ty = fa.inferred_type_via_bag("$x", Point::new(0, 11));
    assert_eq!(ty, Some(InferredType::HashRef), "initial hashref");
    // After line 1 → ArrayRef
    let ty = fa.inferred_type_via_bag("$x", Point::new(1, 8));
    assert_eq!(ty, Some(InferredType::ArrayRef), "reassigned to arrayref");
}

#[test]
fn test_extract_constructor_still_works() {
    // Existing constructor detection should still work
    let fa = build_fa("my $obj = Foo->new();");
    let ty = fa.inferred_type_via_bag("$obj", Point::new(0, 21));
    assert_eq!(ty, Some(InferredType::ClassName("Foo".into())));
}

// ---- Operator-based type inference tests (Step 3) ----

#[test]
fn test_arrow_hash_deref_infers_hashref() {
    let fa = build_fa("my $x;\n$x->{key};");
    let ty = fa.inferred_type_via_bag("$x", Point::new(1, 10));
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_arrow_array_deref_infers_arrayref() {
    let fa = build_fa("my $x;\n$x->[0];");
    let ty = fa.inferred_type_via_bag("$x", Point::new(1, 8));
    assert!(ty.is_some_and(|t| t.is_array_shaped()), "array-shaped");
}

#[test]
fn test_arrow_code_deref_infers_coderef() {
    let fa = build_fa("my $x;\n$x->(1, 2);");
    let ty = fa.inferred_type_via_bag("$x", Point::new(1, 10));
    // Deref-context inference: `$x->(...)` says `$x` is a coderef
    // but reveals nothing about its body (the binding is opaque).
    assert_eq!(ty, Some(InferredType::CodeRef { return_edge: None }));
}

#[test]
fn test_coderef_call_propagates_return_type() {
    // `my $cb = sub { [1,2] }; my $r = $cb->();` — the literal's
    // `return_edge` (Expr(body_last)) rides through the binding,
    // and `$cb->()` should chase it: $r types as ArrayRef.
    // Anonymous-sub literal whose body's last expression is an
    // anonymous_array_expression — closed-under-syntax, so the
    // body span resolves to ArrayRef without name lookup.
    let fa = build_fa("my $cb = sub { [1,2] };\nmy $r = $cb->();\nmy $z;");
    let ty = fa.inferred_type_via_bag("$r", Point::new(2, 0));
    assert!(
        ty.as_ref().is_some_and(|t| t.is_array_shaped()),
        "coderef call must inherit the callable's return type via return_edge: got {:?}",
        ty,
    );
}

#[test]
fn test_postfix_array_deref_infers_arrayref() {
    let fa = build_fa("my $x;\nmy @a = $x->@*;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert!(ty.is_some_and(|t| t.is_array_shaped()), "array-shaped");
}

#[test]
fn test_postfix_hash_deref_infers_hashref() {
    let fa = build_fa("my $y;\nmy %h = $y->%*;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$y", Point::new(2, 0));
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_binary_numeric_ops_infer_numeric() {
    let fa = build_fa("my $x;\nmy $a = $x + 1;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::Numeric), "+ operator");

    let fa = build_fa("my $x;\nmy $a = $x * 2;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::Numeric), "* operator");
}

#[test]
fn test_assignment_from_binary_numeric_infers_result() {
    let fa = build_fa("my $a = 1;\nmy $b = 2;\nmy $result = $a + $b;\n$result;");
    let ty = fa.inferred_type_via_bag("$result", Point::new(3, 0));
    assert_eq!(
        ty,
        Some(InferredType::Numeric),
        "$result = $a + $b should be Numeric"
    );
}

#[test]
fn test_assignment_from_string_concat_infers_result() {
    let fa = build_fa("my $a = 'x';\nmy $b = 'y';\nmy $s = $a . $b;\n$s;");
    let ty = fa.inferred_type_via_bag("$s", Point::new(3, 0));
    assert_eq!(
        ty,
        Some(InferredType::String),
        "$s = $a . $b should be String"
    );
}

#[test]
fn test_string_concat_infers_string() {
    let fa = build_fa("my $s;\nmy $a = $s . \"x\";\nmy $z;");
    let ty = fa.inferred_type_via_bag("$s", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::String), ". operator");
}

#[test]
fn test_string_repeat_infers_string() {
    let fa = build_fa("my $s;\n$s x 3;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$s", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::String), "x operator");
}

#[test]
fn test_numeric_comparison_infers_numeric() {
    let fa = build_fa("my $x;\nmy $y;\n$x == $y;\nmy $z;");
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(3, 0)),
        Some(InferredType::Numeric)
    );
    assert_eq!(
        fa.inferred_type_via_bag("$y", Point::new(3, 0)),
        Some(InferredType::Numeric)
    );
}

#[test]
fn test_string_comparison_infers_string() {
    let fa = build_fa("my $x;\nmy $y;\n$x eq $y;\nmy $z;");
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(3, 0)),
        Some(InferredType::String)
    );
    assert_eq!(
        fa.inferred_type_via_bag("$y", Point::new(3, 0)),
        Some(InferredType::String)
    );
}

#[test]
fn test_increment_infers_numeric() {
    let fa = build_fa("my $x;\n$x++;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::Numeric));
}

#[test]
fn test_regex_match_infers_string() {
    let fa = build_fa("my $s;\n$s =~ /pattern/;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$s", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::String));
}

#[test]
fn test_preinc_infers_numeric() {
    let fa = build_fa("my $x;\n++$x;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::Numeric));
}

#[test]
fn test_block_array_deref_infers_arrayref() {
    let fa = build_fa("my $x;\nmy @items = @{$x};\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert!(ty.is_some_and(|t| t.is_array_shaped()), "array-shaped");
}

#[test]
fn test_block_hash_deref_infers_hashref() {
    let fa = build_fa("my $y;\nmy %t = %{$y};\nmy $z;");
    let ty = fa.inferred_type_via_bag("$y", Point::new(2, 0));
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_block_code_deref_infers_coderef() {
    let fa = build_fa("my $z;\n&{$z}();\nmy $w;");
    let ty = fa.inferred_type_via_bag("$z", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::CodeRef { return_edge: None }));
}

#[test]
fn test_no_numeric_on_array_variable() {
    // @arr + 1 should NOT push Numeric on @arr
    let fa = build_fa("my @arr;\nmy $n = @arr + 1;\nmy $z;");
    let ty = fa.inferred_type_via_bag("@arr", Point::new(2, 0));
    assert_eq!(ty, None, "@arr should not get Numeric constraint");
}

// ---- Builtin type inference tests ----

#[test]
fn test_builtin_push_infers_arrayref() {
    // push @{$aref} triggers array_deref_expression which already infers ArrayRef
    let fa = build_fa("my $aref;\npush @{$aref}, 1;\nmy $z;");
    let ty = fa.inferred_type_via_bag("$aref", Point::new(2, 0));
    assert!(
        ty.is_some_and(|t| t.is_array_shaped()),
        "push deref should infer ArrayRef",
    );
}

#[test]
fn test_builtin_length_infers_string_arg() {
    let fa = build_fa("my $s;\nmy $n = length($s);\nmy $z;");
    let ty = fa.inferred_type_via_bag("$s", Point::new(2, 0));
    assert_eq!(
        ty,
        Some(InferredType::String),
        "length arg should be String"
    );
}

#[test]
fn test_builtin_abs_infers_numeric_arg() {
    let fa = build_fa("my $x;\nmy $n = abs($x);\nmy $z;");
    let ty = fa.inferred_type_via_bag("$x", Point::new(2, 0));
    assert_eq!(ty, Some(InferredType::Numeric), "abs arg should be Numeric");
}

#[test]
fn test_builtin_return_type_propagates() {
    let fa = build_fa("my $t = time();\n$t;");
    let ty = fa.inferred_type_via_bag("$t", Point::new(1, 0));
    assert_eq!(
        ty,
        Some(InferredType::Numeric),
        "time() should return Numeric"
    );
}

#[test]
fn test_builtin_join_return_type() {
    let fa = build_fa("my $s = join(',', @arr);\n$s;");
    let ty = fa.inferred_type_via_bag("$s", Point::new(1, 0));
    assert_eq!(
        ty,
        Some(InferredType::String),
        "join() should return String"
    );
}

#[test]
fn test_builtin_length_return_type() {
    let fa = build_fa("my $n = length('hello');\n$n;");
    let ty = fa.inferred_type_via_bag("$n", Point::new(1, 0));
    assert_eq!(
        ty,
        Some(InferredType::Numeric),
        "length() should return Numeric"
    );
}

// ---- Return type inference tests (Step 4) ----

#[test]
fn test_return_type_hashref() {
    let fa = build_fa("sub get_config {\n    return { host => \"localhost\" };\n}");
    assert!(
        fa.sub_return_type_at_arity("get_config", None).is_some_and(|t| t.is_hash_shaped()),
        "hash-shaped",
    );
}

#[test]
fn test_return_type_arrayref() {
    let fa = build_fa("sub get_tags {\n    return [1, 2, 3];\n}");
    assert!(
        fa.sub_return_type_at_arity("get_tags", None).is_some_and(|t| t.is_array_shaped()),
        "array-shaped",
    );
}

#[test]
fn test_return_type_coderef() {
    let fa = build_fa("sub get_handler {\n    return sub { 1 };\n}");
    let ty = fa.sub_return_type_at_arity("get_handler", None);
    // `return sub { 1 }` is a sub-literal — the returned CodeRef
    // carries `return_edge` to the body's last expression.
    assert!(
        matches!(ty, Some(InferredType::CodeRef { return_edge: Some(_) })),
        "got {:?}",
        ty
    );
}

#[test]
fn test_return_type_implicit_last_expr() {
    // No explicit return — last expression is the implicit return
    let fa = build_fa("sub get_data {\n    { key => \"val\" };\n}");
    assert!(
        fa.sub_return_type_at_arity("get_data", None).is_some_and(|t| t.is_hash_shaped()),
        "hash-shaped",
    );
}

#[test]
fn test_return_type_conflicting_returns_unknown() {
    // Two returns with different types → None (unknown)
    let fa = build_fa("sub ambiguous {\n    if (1) { return {} }\n    return [];\n}");
    assert_eq!(fa.sub_return_type_at_arity("ambiguous", None), None);
}

#[test]
fn test_return_type_consistent_returns() {
    // Multiple returns all hashref → HashRef
    let fa =
        build_fa("sub consistent {\n    if (1) { return { a => 1 } }\n    return { b => 2 };\n}");
    assert!(
        fa.sub_return_type_at_arity("consistent", None).is_some_and(|t| t.is_hash_shaped()),
        "hash-shaped",
    );
}

#[test]
fn test_return_type_propagation_to_call_site() {
    let fa =
        build_fa("sub get_config {\n    return { host => 1 };\n}\nmy $cfg = get_config();\nmy $z;");
    assert!(
        fa.sub_return_type_at_arity("get_config", None).is_some_and(|t| t.is_hash_shaped()),
        "hash-shaped",
    );
    let ty = fa.inferred_type_via_bag("$cfg", Point::new(4, 0));
    assert!(
        ty.is_some_and(|t| t.is_hash_shaped()),
        "call site should get return type",
    );
}

#[test]
fn test_return_type_propagation_method_call() {
    let src = "package Calculator;\nsub new { bless {}, shift }\nsub add {\n    my ($self, $a, $b) = @_;\n    my $result = $a + $b;\n    return $result;\n}\npackage main;\nmy $calc = Calculator->new();\nmy $sum = $calc->add(2, 3);\n$sum;";
    let fa = build_fa(src);
    assert_eq!(
        fa.sub_return_type_at_arity("add", None),
        Some(InferredType::Numeric),
        "add should return Numeric"
    );
    let ty = fa.inferred_type_via_bag("$sum", Point::new(10, 0));
    assert_eq!(
        ty,
        Some(InferredType::Numeric),
        "$sum should be Numeric via method call binding"
    );
}

#[test]
fn test_return_type_constructor() {
    let fa = build_fa("package User;\nsub new { bless {}, shift }\npackage main;\nsub get_user {\n    return User->new();\n}");
    assert_eq!(
        fa.sub_return_type_at_arity("get_user", None),
        Some(InferredType::ClassName("User".into()))
    );
}

#[test]
fn test_return_type_self_variable() {
    // `return $self` resolves through the witness bag to the canonical
    // class type. `FirstParam` (the body-internal observation) is
    // normalised to `ClassName` at the FrameworkAwareTypeFold boundary
    // — callers chaining off the return get the concrete class.
    let fa = build_fa("package Foo;\nsub new { bless {}, shift }\nsub clone {\n    my ($self) = @_;\n    return $self;\n}");
    assert_eq!(
        fa.sub_return_type_at_arity("clone", None),
        Some(InferredType::ClassName("Foo".into())),
    );
}

#[test]
fn test_return_type_bare_return_optional() {
    // Bare `return;` + typed return → Optional<typed>: `return unless …`
    // means the sub can yield undef (docs/adr/optional-types.md).
    let fa = build_fa("sub get_config {\n    return unless 1;\n    return { host => 1 };\n}");
    let t = fa.sub_return_type_at_arity("get_config", None);
    assert!(
        matches!(&t, Some(InferredType::Optional(inner)) if inner.is_hash_shaped()),
        "Optional<hash-shaped>, got {t:?}",
    );
}

#[test]
fn test_return_type_all_bare_returns() {
    // Every way out is a provable undef → the definitive `Undef`, not the
    // silent `None`. `None` means "no idea"; this sub is not unknown, it is
    // known to yield undef, and the method-on-undef and always-false-guard
    // diagnostics can only act on the difference.
    let fa = build_fa("sub noop {\n    return;\n}");
    assert_eq!(
        fa.sub_return_type_at_arity("noop", None),
        Some(InferredType::Undef)
    );
}

#[test]
fn test_return_type_all_undef_returns() {
    // `return undef` — the explicit spelling of the same fact.
    let fa = build_fa("sub nothing {\n    return undef;\n}");
    assert_eq!(
        fa.sub_return_type_at_arity("nothing", None),
        Some(InferredType::Undef)
    );
}

#[test]
fn test_return_type_all_empty_list_returns() {
    // `return ()` is the third spelling: it coerces to undef in scalar
    // context, and the return side must agree with the ternary side about
    // that (both read `stub_expression` as an undef arm).
    let fa = build_fa("sub empty {\n    return ();\n}");
    assert_eq!(
        fa.sub_return_type_at_arity("empty", None),
        Some(InferredType::Undef)
    );
}

#[test]
fn test_return_type_guard_clause_constructor_is_optional() {
    // The standard Perl constructor: a guard clause that bails with `return
    // undef`, then an implicit final `$self`. Both are ways OUT, so the sub
    // is `Optional<Class>`.
    //
    // This is the shape the old model could not type at all. A sub with any
    // explicit return was excluded from the implicit-return path, so its
    // fallthrough tail was never an arm and the fold saw only the bail-out.
    let fa = build_fa(
        "package Widget;\nsub new {\n    my $class = shift;\n    my $ok = check() or return undef;\n    my $self = bless {}, $class;\n    $self;\n}",
    );
    let t = fa.sub_return_type_at_arity("new", None);
    assert!(
        matches!(&t, Some(InferredType::Optional(inner)) if matches!(&**inner, InferredType::ClassName(c) if c == "Widget")),
        "Optional<Widget>, got {t:?}",
    );
}

#[test]
fn test_return_type_undef_tail_is_an_undef_arm() {
    // A tail can itself be undef. It is an undef ARM, classified by the same
    // vocabulary the return side uses — not a value arm that happens to type
    // as Undef. `sub f { undef }` and `sub f { return undef }` state one fact
    // and must not answer differently.
    for src in ["sub t { undef }", "sub t { () }"] {
        let fa = build_fa(src);
        assert_eq!(
            fa.sub_return_type_at_arity("t", None),
            Some(InferredType::Undef),
            "{src}",
        );
    }
}

#[test]
fn test_undef_arm_records_which_spelling_produced_it() {
    // All three coerce to undef in SCALAR context — which is why they share
    // one Fact family and one verdict today. They diverge in LIST context,
    // and that is only recoverable at the emission site, so the spelling
    // rides the Fact's key as a dormant payload.
    //
    // The grouping is the Perl one, and it is NOT by syntax: a bare
    // `return;` is the EMPTY list like `()`, not a one-element list like
    // `return undef`. That is exactly why `return;` is the recommended
    // spelling.
    use crate::model::witnesses::{tags, tags::UndefArm, WitnessPayload};
    let spellings = |src: &str| -> Vec<Option<UndefArm>> {
        build_fa(src)
            .witnesses
            .all()
            .iter()
            .filter_map(|w| match &w.payload {
                WitnessPayload::Fact { family, value, .. }
                    if family == tags::FACT_UNDEF_ARM =>
                {
                    Some(UndefArm::from_fact_value(value))
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(spellings("sub f { return undef }"), vec![Some(UndefArm::Scalar)]);
    assert_eq!(spellings("sub f { return () }"), vec![Some(UndefArm::EmptyList)]);
    assert_eq!(spellings("sub f { return; }"), vec![Some(UndefArm::EmptyList)]);
    // The tail spellings classify identically to the return spellings.
    assert_eq!(spellings("sub f { undef }"), vec![Some(UndefArm::Scalar)]);
    assert_eq!(spellings("sub f { () }"), vec![Some(UndefArm::EmptyList)]);
    // …and the verdict is unchanged by the distinction: still just Undef.
    for src in ["sub f { return undef }", "sub f { return () }", "sub f { return; }"] {
        assert_eq!(
            build_fa(src).sub_return_type_at_arity("f", None),
            Some(InferredType::Undef),
            "{src}",
        );
    }
}

#[test]
fn test_return_type_undef_arm_with_fallthrough_tail_not_undef() {
    // `return undef if $x; compute()` — the fallthrough tail is a way out
    // that the per-`return` walk never visits. The all-undef gate must not
    // fire: this sub returns compute()'s value whenever the guard fails,
    // and claiming `Undef` would be a confident lie rather than a miss.
    let fa = build_fa("sub gated {\n    my ($x) = @_;\n    return undef if $x;\n    compute();\n}");
    assert_ne!(
        fa.sub_return_type_at_arity("gated", None),
        Some(InferredType::Undef)
    );
}

#[test]
fn test_return_type_undef_arm_before_explicit_return_still_undef() {
    // The mirror image, and the reason the tail needs a real signal rather
    // than "is there a last expression": `compute()` here is a discarded
    // statement, NOT a way out — the sub always leaves through `return
    // undef`. Reading any trailing expression as a value arm would lose
    // this verdict.
    let fa = build_fa("sub always {\n    compute();\n    return undef;\n}");
    assert_eq!(
        fa.sub_return_type_at_arity("always", None),
        Some(InferredType::Undef)
    );
}

#[test]
fn test_return_type_empty_list_return_optional() {
    // `return ()` beside a typed arm lifts to Optional<T>, same as bare
    // `return;` — the `{T, undef}` join, not the all-undef verdict.
    let fa = build_fa("sub find {\n    return () unless 1;\n    return { row => 1 };\n}");
    let t = fa.sub_return_type_at_arity("find", None);
    assert!(
        matches!(&t, Some(InferredType::Optional(inner)) if inner.is_hash_shaped()),
        "Optional<hash-shaped>, got {t:?}",
    );
}

#[test]
fn test_return_type_undef_optional() {
    // return undef + typed return → Optional<typed>: the sub CAN return undef,
    // so its value is optional (docs/adr/optional-types.md). Bare `return;`
    // is still filtered (Phase 2) — see test_return_type_bare_return_filtered.
    let fa = build_fa("sub maybe {\n    return undef unless 1;\n    return { a => 1 };\n}");
    let t = fa.sub_return_type_at_arity("maybe", None);
    assert!(
        matches!(&t, Some(InferredType::Optional(inner)) if inner.is_hash_shaped()),
        "Optional<hash-shaped>, got {t:?}",
    );
}

// ---- resolve_expression_type tests ----

/// Find the first node of given kind at/after a point (searches all children).
fn find_node_at<'a>(
    node: tree_sitter::Node<'a>,
    point: Point,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    if node.kind() == kind && node.start_position() >= point {
        return Some(node);
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(found) = find_node_at(child, point, kind) {
                return Some(found);
            }
        }
    }
    None
}

#[test]
fn test_resolve_expr_type_function_call() {
    let src = "sub get_config {\n    return { host => 1 };\n}\nget_config();\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // Find the function_call_expression on line 3
    let call_node = find_node_at(
        tree.root_node(),
        Point::new(3, 0),
        "function_call_expression",
    )
    .expect("should find function_call_expression");
    let ty = crate::lsp::cursor_context::resolve_expression_type(&fa, call_node, src.as_bytes(), None);
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_resolve_expr_type_method_call_return() {
    let src = "package Foo;\nsub new { bless {}, shift }\nsub get_bar {\n    return Bar->new();\n}\npackage Bar;\nsub new { bless {}, shift }\nsub do_thing { }\npackage main;\nmy $f = Foo->new();\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // $f->get_bar() should resolve to Object(Bar)
    // First verify get_bar has the right return type
    assert_eq!(
        fa.sub_return_type_at_arity("get_bar", None),
        Some(InferredType::ClassName("Bar".into()))
    );
}

#[test]
fn test_resolve_expr_type_scalar_variable() {
    let src = "my $x = {};\n$x;\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // Find the scalar $x on line 1
    let scalar_node =
        find_node_at(tree.root_node(), Point::new(1, 0), "scalar").expect("should find scalar");
    let ty = crate::lsp::cursor_context::resolve_expression_type(&fa, scalar_node, src.as_bytes(), None);
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_resolve_expr_type_chained_method() {
    let src = "package Foo;\nsub new { bless {}, shift }\nsub get_bar {\n    return Bar->new();\n}\npackage Bar;\nsub new { bless {}, shift }\nsub get_name {\n    return { name => 'test' };\n}\npackage main;\nmy $f = Foo->new();\n$f->get_bar()->get_name();\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    // Line 12: $f->get_bar()->get_name();
    // The outermost method_call_expression starts at column 0
    // Use descendant_for_point_range to find the node at the start of that line
    let node = tree
        .root_node()
        .descendant_for_point_range(Point::new(12, 0), Point::new(12, 25))
        .expect("should find node");
    // Walk up to find the outermost method_call_expression
    let mut n = node;
    while n.kind() != "method_call_expression"
        || n.parent()
            .map_or(false, |p| p.kind() == "method_call_expression")
    {
        n = match n.parent() {
            Some(p) => p,
            None => panic!("should find outermost method_call_expression"),
        };
    }
    assert_eq!(n.kind(), "method_call_expression");
    let ty = crate::lsp::cursor_context::resolve_expression_type(&fa, n, src.as_bytes(), None);
    assert!(ty.is_some_and(|t| t.is_hash_shaped()), "hash-shaped");
}

#[test]
fn test_resolve_expr_type_constructor() {
    let src = "package Foo;\nsub new { bless {}, shift }\npackage main;\nFoo->new();\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    let call = find_node_at(tree.root_node(), Point::new(3, 0), "method_call_expression")
        .expect("should find method_call_expression");
    let ty = crate::lsp::cursor_context::resolve_expression_type(&fa, call, src.as_bytes(), None);
    assert_eq!(ty, Some(InferredType::ClassName("Foo".into())));
}

#[test]
fn test_resolve_expr_type_triple_chain() {
    // $calc->get_self->get_config->{host} — no parens on method calls
    let src = "\
package Calculator;
sub new { bless {}, shift }
sub get_self {
    my ($self) = @_;
    return $self;
}
sub get_config {
    return { host => 'localhost', port => 5432 };
}
package main;
my $calc = Calculator->new();
$calc->get_self->get_config->{host};
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());

    // Verify get_self returns an object type for Calculator
    let get_self_rt = fa.sub_return_type_at_arity("get_self", None);
    assert_eq!(
        get_self_rt.as_ref().and_then(|t| t.class_name()),
        Some("Calculator"),
        "get_self should return Calculator"
    );

    // Verify get_config returns HashRef
    let get_config_rt = fa.sub_return_type_at_arity("get_config", None);
    assert!(
        get_config_rt.is_some_and(|t| t.is_hash_shaped()),
        "get_config should return HashRef",
    );

    // The outermost expression is hash_element_expression wrapping the chain
    // Find the method_call_expression for get_config (inner chain)
    // Line 11: $calc->get_self->get_config->{host}
    let node = tree
        .root_node()
        .descendant_for_point_range(Point::new(11, 0), Point::new(11, 0))
        .expect("should find node");
    let mut n = node;
    // Walk up to find hash_element_expression
    loop {
        if n.kind() == "hash_element_expression" {
            break;
        }
        n = n.parent().expect("should find hash_element_expression");
    }
    // The base of hash_element_expression is the method chain
    let base = n.named_child(0).expect("should have base");
    assert_eq!(base.kind(), "method_call_expression");
    let ty = crate::lsp::cursor_context::resolve_expression_type(&fa, base, src.as_bytes(), None);
    assert!(
        ty.is_some_and(|t| t.is_hash_shaped()),
        "the chain $calc->get_self->get_config should resolve to HashRef",
    );
}

/// Acceptance for the unified, tree-free expression-type chase
/// (`docs/adr/bag-canonical.md`): the ref-keyed,
/// `tree: None` invocant-class path and the node-keyed
/// `resolve_expression_type` path must produce identical answers for
/// every invocant shape — scalar, chain, array-element, function-call,
/// and hash-element. Both now route through `expr_type_at_span`.
#[test]
fn invocant_class_and_resolve_expression_type_agree_tree_free() {
    let src = "\
package Foo;
sub new { bless {}, shift }
sub kid { return Foo->new(); }
sub cfg { return { host => 'x' }; }
package main;
sub mk { return Foo->new(); }
my $f = Foo->new();
my @arr;
push @arr, Foo->new();
my %h = (it => $f);
$f->kid();
$f->kid()->kid();
$arr[0]->kid();
mk()->kid();
$h{it}->kid();
";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());

    // For every MethodCall ref with an invocant span, the two paths
    // must agree. The invocant-class path takes no tree; the
    // expression-type path is fed the actual invocant CST node.
    let mut checked_shapes = 0;
    for r in fa.refs() {
        let RefKind::MethodCall { invocant_span: Some(sp), .. } = &r.kind else {
            continue;
        };
        let invocant_node = tree
            .root_node()
            .descendant_for_point_range(sp.start, sp.end)
            .expect("invocant span maps to a node");
        // Skip if the descendant doesn't exactly cover the invocant
        // (parser quirk for some shapes) — we only compare where both
        // paths see the same node.
        if invocant_node.start_position() != sp.start
            || invocant_node.end_position() != sp.end
        {
            continue;
        }
        let via_ref = fa.method_call_invocant_class(r, None);
        let via_node =
            crate::lsp::cursor_context::resolve_expression_type(&fa, invocant_node, src.as_bytes(), None)
                .and_then(|t| t.class_name().map(|s| s.to_string()));
        assert_eq!(
            via_ref, via_node,
            "invocant-class (tree-free) vs resolve_expression_type disagree \
             for invocant `{}` (kind {})",
            invocant_node.utf8_text(src.as_bytes()).unwrap_or("?"),
            invocant_node.kind(),
        );
        checked_shapes += 1;
    }
    // Sanity: the source exercises scalar / chain / array-element /
    // function-call / hash-element invocants, so we should have
    // compared several.
    assert!(
        checked_shapes >= 5,
        "expected to compare at least the 5 invocant shapes, got {}",
        checked_shapes,
    );

    // Spot-check the concrete answers so a mutual `None` regression
    // can't pass the agreement assert vacuously.
    let kid_on_scalar = fa.refs().iter().find(|r| {
        matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.text() == "$f")
            && r.target_name == "kid"
    });
    assert_eq!(
        kid_on_scalar.and_then(|r| fa.method_call_invocant_class(r, None)).as_deref(),
        Some("Foo"),
        "scalar invocant `$f->kid` should type as Foo, tree-free",
    );
    let kid_on_array = fa.refs().iter().find(|r| {
        matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.text().starts_with("$arr"))
            && r.target_name == "kid"
    });
    assert_eq!(
        kid_on_array.and_then(|r| fa.method_call_invocant_class(r, None)).as_deref(),
        Some("Foo"),
        "array-element invocant `$arr[0]->kid` should type as Foo, tree-free",
    );
}

#[test]
fn test_package_at() {
    let fa = build_fa("package Foo;\nsub bar { }");
    let pkg = fa.package_at(Point::new(1, 5));
    assert_eq!(pkg, Some("Foo"));
}

#[test]
fn test_variable_resolves_to() {
    let fa = build_fa("my $x = 1;\nprint $x;");
    let read_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "$x" && r.access == AccessKind::Read)
        .collect();
    assert!(!read_refs.is_empty());
    assert!(
        read_refs[0].resolved_symbol().is_some(),
        "read ref should resolve to declaration"
    );
}

#[test]
fn test_fold_ranges() {
    let fa = build_fa("sub foo {\n    my $x = 1;\n}\nsub bar {\n    my $y = 2;\n}");
    assert!(
        fa.fold_ranges.len() >= 2,
        "should have fold ranges for sub blocks, got {}",
        fa.fold_ranges.len()
    );
}

#[test]
fn test_visible_symbols() {
    let fa = build_fa("my $outer = 1;\nsub foo {\n    my $inner = 2;\n}");
    // Inside the sub, both $outer and $inner should be visible
    let visible = fa.visible_symbols(Point::new(2, 10));
    let names: Vec<&str> = visible.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"$inner"),
        "should see $inner, got: {:?}",
        names
    );
    assert!(
        names.contains(&"$outer"),
        "should see $outer, got: {:?}",
        names
    );
}

#[test]
fn test_two_packages_scoped() {
    let fa = build_fa("package Foo;\nsub alpha { }\npackage Bar;\nsub beta { }");
    // At the beta sub, package should be "Bar"
    let pkg = fa.package_at(Point::new(3, 5));
    assert_eq!(pkg, Some("Bar"));
    // At the alpha sub, package should be "Foo"
    let pkg = fa.package_at(Point::new(1, 5));
    assert_eq!(pkg, Some("Foo"));
}

#[test]
fn test_block_scoped_package_reverts() {
    // A `package Inner;` inside a bare `{ }` block must NOT leak past the
    // block close — `sub o` after the block belongs to Outer.
    let src = "package Outer;\n{\n  package Inner;\n  sub i { }\n}\nsub o { }\n";
    let fa = build_fa(src);

    let o = fa.symbols().iter().find(|s| s.name == "o").expect("sub o");
    assert_eq!(o.package.as_deref(), Some("Outer"), "sub o must be in Outer, not Inner");

    let i = fa.symbols().iter().find(|s| s.name == "i").expect("sub i");
    assert_eq!(i.package.as_deref(), Some("Inner"), "sub i must be in Inner");

    // package_at must also revert: line 5 (`sub o`) is Outer.
    assert_eq!(fa.package_at(Point::new(5, 4)), Some("Outer"));
    // line 3 (`sub i`) is Inner.
    assert_eq!(fa.package_at(Point::new(3, 6)), Some("Inner"));
}

#[test]
fn test_non_block_package_unaffected() {
    // Regression: a normal statement-form `package Bar;` (no block) still
    // flows to end of file.
    let fa = build_fa("package Foo;\nsub a { }\npackage Bar;\nsub b { }\nsub c { }\n");
    let b = fa.symbols().iter().find(|s| s.name == "b").expect("sub b");
    let c = fa.symbols().iter().find(|s| s.name == "c").expect("sub c");
    assert_eq!(b.package.as_deref(), Some("Bar"));
    assert_eq!(c.package.as_deref(), Some("Bar"));
}

// ---- Qualified call names ----
//
// A qualifier is not decoration. `Foo::bar()` and `bar()` name different subs,
// and `CORE::stat()` names the builtin *precisely because* something else
// called `stat` is in scope. Dropping it made every qualified call bind to any
// same-named local sub, which inverts what the author wrote the qualifier to
// say — and the wrong type then propagated into the caller's return.

/// The declaring package's own name still binds: this is the case the strip
/// existed to serve, and it has to keep working.
#[test]
fn qualified_call_to_own_package_binds_locally() {
    let fa = build_fa(
        "package T;\nsub helper { return 'x' }\nsub probe { my $s = T::helper(); return $s; }\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$s", Point::new(2, 20)),
        Some(InferredType::String),
    );
}

#[test]
fn unqualified_call_binds_locally() {
    let fa = build_fa(
        "package T;\nsub helper { return 'x' }\nsub probe { my $s = helper(); return $s; }\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$s", Point::new(2, 20)),
        Some(InferredType::String),
    );
}

/// A qualifier naming a package this file does not declare must NOT reach a
/// same-named local sub — the bug this pair of tests pins.
#[test]
fn qualified_call_to_foreign_package_does_not_bind_to_local_sub() {
    let fa = build_fa(
        "package T;\nsub stat { return 'x' }\nsub probe { my $s = Data::Dumper::stat('/tmp'); return $s; }\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$s", Point::new(2, 20)),
        None,
        "Data::Dumper::stat must not resolve to T::stat",
    );
}

/// `CORE::` needs no special case: it names no package the file declares, so
/// the general rule already excludes it.
#[test]
fn core_qualified_call_does_not_bind_to_local_sub() {
    let fa = build_fa(
        "package T;\nsub stat { return 'x' }\nsub probe { my $s = CORE::stat('/tmp'); return $s; }\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$s", Point::new(2, 20)),
        None,
        "CORE::stat names the builtin, never the local sub it is written to bypass",
    );
}

/// A file that declares no package is `main`, so the `main::` spelling of its
/// own subs still binds.
#[test]
fn main_qualified_call_binds_in_a_package_less_file() {
    let fa = build_fa("sub helper { return 'x' }\nsub probe { my $s = main::helper(); return $s; }\n");
    assert_eq!(
        fa.inferred_type_via_bag("$s", Point::new(1, 20)),
        Some(InferredType::String),
    );
}
