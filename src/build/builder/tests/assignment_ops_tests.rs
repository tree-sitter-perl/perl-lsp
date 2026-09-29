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
        // An object is never false, so the fallback is unreachable.
        assert_eq!(class(returns(&compound)).as_deref(), Some("Bar"), "{compound}");
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


#[test]
fn a_plain_write_reads_the_value_it_replaces() {
    let mut bad = Vec::new();
    for body in [
        "my $s = Bar->new; $s = $s; return $s",
        "my $s = Bar->new; $s = $s || nothing(); return $s",
        "my $s = Bar->new; $s = $s->me; return $s",
        "my $s = Bar->new; $s = me($s); return $s",
        "my $s = Bar->new; $s = $s ? $s : Bar->new; return $s",
    ] {
        let src = format!(
            "package Bar; sub new {{ bless {{}}, shift }} sub me {{ Bar->new }}\npackage Foo;\nsub me {{ Bar->new }}\nsub probe {{ {body} }}\n1;\n"
        );
        let got = build_fa(&src).sub_return_type_at_arity("probe", None);
        if class(got.clone()).as_deref() != Some("Bar") {
            bad.push(format!("{body} => {got:?}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}


#[test]
fn compound_writes_compose_with_guards() {
    let src = "package Bar; sub new { bless {}, shift } sub me { Baz->new }\npackage Baz; sub new { bless {}, shift }\npackage Foo;\n\
sub maybe { return undef if $_[0]; Bar->new }\n\
sub g1 { my $x = shift; if ($x->isa('Bar')) { $x ||= Baz->new; return $x } }\n\
sub g2 { my $x = shift; if ($x->isa('Bar')) { $x = $x->me; return $x } }\n\
sub g3 { my $x = shift; if ($x->isa('Bar')) { $x .= 's'; return $x } }\n\
sub g4 { my $x; $x ||= Bar->new; return $x }\n\
sub g5 { my $x = maybe(1); $x //= Bar->new; return $x }\n\
sub g6 { my $x = maybe(1); return unless defined $x; $x ||= Baz->new; return $x }\n\
1;\n";
    let fa = build_fa(src);
    let ret = |s: &str| fa.sub_return_type_at_arity(s, None);
    let bar = || InferredType::ClassName("Bar".into());
    // The implicit read sees the guard's narrowing; a narrowed object is
    // never false, so the fallback never runs.
    assert_eq!(ret("g1"), Some(bar()));
    // The RHS reads the narrowed value; the write then ends the region.
    assert_eq!(ret("g2"), Some(InferredType::ClassName("Baz".into())));
    assert_eq!(ret("g3"), Some(InferredType::String));
    // `my $x;` is undef, so `||=` always takes the RHS.
    assert_eq!(ret("g4"), Some(bar()));
    // An optional LHS falls back to the RHS floor.
    assert_eq!(ret("g5"), Some(bar()));
    // `defined` strips the Optional up to the write, so `$x` stays Bar;
    // the sub is Optional only because of its bare `return`.
    assert_eq!(ret("g6"), Some(InferredType::Optional(Box::new(bar()))));
}


#[test]
fn element_writes_extend_an_empty_hash() {
    for (body, want) in [
        ("my $h = {}; $h->{k} = Bar->new; return $h->{k};", Some("Bar")),
        ("my %h; $h{k} = Bar->new; return $h{k};", Some("Bar")),
        ("my %h = (); $h{k} = Bar->new; return $h{k};", Some("Bar")),
        ("my $h = {}; $h->{k} ||= Bar->new; return $h->{k};", Some("Bar")),
        ("my $h = {}; $h->{k} //= Bar->new; return $h->{k};", Some("Bar")),
        ("my $h = {}; $h->{k} = Bar->new; $h->{k} ||= Baz->new; return $h->{k};", Some("Bar")),
        // A write the walk can't pin to one key leaves the key untyped.
        ("my $h = {}; $h->{$_} = Bar->new for 1; return $h->{k};", None),
    ] {
        assert_eq!(class(returns(body)).as_deref(), want, "{body}");
    }
    assert_eq!(returns("my $h = {}; $h->{k} .= 'x'; return $h->{k};"), Some(InferredType::String));
    // The memoized-accessor idiom on a file-level cache.
    let src = format!("{PRELUDE}my $cache = {{}};\nsub user {{ $cache->{{user}} ||= Bar->new }}\n1;\n");
    let got = build_fa(&src).sub_return_type_at_arity("user", None);
    assert_eq!(class(got).as_deref(), Some("Bar"));
}

#[test]
fn an_empty_shape_reports_no_key_typos() {
    let src = "package Foo;\nsub f { my %state; while (1) { last if $state{seen}; $state{seen} = 1 } my $h = {}; $h->{x} }\nsub g { {} }\nsub h { g()->{x} }\n1;\n";
    let fa = build_fa(src);
    assert!(fa.closed_shape_key_typos(None).is_empty());
    assert!(fa.projected_key_typos(None).is_empty());
}
