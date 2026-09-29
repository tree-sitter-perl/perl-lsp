//! The assignment operator decides what the target holds and what the
//! expression itself is worth (issue #201).

use super::*;

const PRELUDE: &str = "package Bar; sub new { bless {}, shift }\npackage Baz; sub new { bless {}, shift }\npackage Foo;\nsub cache { {} }\n";

fn returns(body: &str) -> Option<InferredType> {
    let src = format!("{PRELUDE}sub probe {{ {body} }}\n1;\n");
    build_fa(&src).sub_return_type_at_arity("probe", None)
}

fn class(t: Option<InferredType>) -> Option<String> {
    t.and_then(|t| t.class_name().map(str::to_string))
}

#[test]
fn an_assignment_is_worth_the_value_it_stores() {
    for body in [
        "my $s; return $s = Bar->new;",
        "my $s; return $s ||= Bar->new;",
        "my $s; return $s //= Bar->new;",
        "my $s = 1; return $s &&= Bar->new;",
        "my $h = shift; return $h->{k} = Bar->new;",
        "return cache->{k} ||= Bar->new;",
        "return cache->{k} //= Bar->new;",
        "cache->{k} ||= Bar->new",
        "my $s; $s = Bar->new",
    ] {
        assert_eq!(class(returns(body)).as_deref(), Some("Bar"), "{body}");
    }
    for (op, want) in [
        (".=", InferredType::String),
        ("x=", InferredType::String),
        ("+=", InferredType::Numeric),
        ("-=", InferredType::Numeric),
        ("*=", InferredType::Numeric),
        ("/=", InferredType::Numeric),
        ("%=", InferredType::Numeric),
        ("**=", InferredType::Numeric),
        ("|=", InferredType::Numeric),
        ("&=", InferredType::Numeric),
        ("^=", InferredType::Numeric),
        ("<<=", InferredType::Numeric),
        (">>=", InferredType::Numeric),
    ] {
        let body = format!("return cache->{{k}} {op} Bar->new;");
        assert_eq!(returns(&body), Some(want), "{body}");
    }
}

#[test]
fn a_compound_operator_types_its_target_by_its_result() {
    for (body, want) in [
        (r#"my $s = "a"; $s .= 2; return $s"#, InferredType::String),
        (r#"my $s = "a"; $s .= Bar->new; return $s"#, InferredType::String),
        (r#"my $s = 1; $s += Bar->new; return $s"#, InferredType::Numeric),
        (r#"my $s = "a"; $s x= 3; return $s"#, InferredType::String),
        (r#"my $s = 1; $s .= "x"; return $s"#, InferredType::String),
    ] {
        assert_eq!(returns(body), Some(want), "{body}");
    }
    assert_eq!(class(returns("my $s; $s ||= Baz->new; return $s")).as_deref(), Some("Baz"));
    assert_eq!(class(returns("my $s = Bar->new; $s &&= Baz->new; return $s")).as_deref(), Some("Baz"));
}

#[test]
fn a_fallback_assignment_is_its_short_circuit_spelling() {
    for op in ["||", "//"] {
        let compound = format!("my $s = Bar->new; $s {op}= Baz->new; return $s");
        let spelled = format!("my $s = Bar->new; $s = $s {op} Baz->new; return $s");
        assert_eq!(returns(&compound), returns(&spelled), "{compound}");
        // The old value is read before the write, so an untypeable fallback
        // keeps it.
        let untyped = format!("my $s = Bar->new; $s {op}= nothing(); return $s");
        assert_eq!(class(returns(&untyped)).as_deref(), Some("Bar"), "{untyped}");
    }
}

#[test]
fn append_extends_the_constant_fold() {
    let method_targets = |src: &str| -> Vec<String> {
        build_fa(src)
            .refs()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }))
            .map(|r| r.target_name.clone())
            .collect()
    };
    let got = method_targets("package Foo;\nsub f { my $m = 'pro'; $m .= 'cess'; Foo->new->$m() }\n");
    assert!(got.iter().any(|t| t == "process"), "{got:?}");
    assert!(!got.iter().any(|t| t == "cess" || t == "pro"), "{got:?}");

    let got = method_targets("package Foo;\nsub f { my $m = 'go'; $m x= 2; Foo->new->$m() }\n");
    assert!(!got.iter().any(|t| t == "go" || t == "gogo"), "a fold it can't name is dropped: {got:?}");
}

