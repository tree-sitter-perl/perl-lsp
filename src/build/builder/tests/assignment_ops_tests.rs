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
        (r#"my $s = "a"; $s |= 1; return $s"#, InferredType::Numeric),
        (r#"my $s = 1; $s ^= "b"; return $s"#, InferredType::Numeric),
    ] {
        assert_eq!(returns(body), Some(want), "{body}");
    }
    // Two strings are a per-character string op, which nothing types.
    for op in ["|=", "&=", "^="] {
        let body = format!(r#"my $s = "a"; $s {op} "b"; return $s"#);
        assert_eq!(returns(&body), Some(InferredType::Unknown), "{body}");
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
        // Writing the fallback says the author expects it to run, so the
        // arms join; with no union yet, disagreeing arms answer the floor.
        assert_eq!(class(returns(&compound)).as_deref(), Some("Baz"), "{compound}");
        // An untypeable fallback leaves the join unknown.
        let untyped = format!("my $s = Bar->new; $s {op}= nothing(); return $s");
        assert_eq!(class(returns(&untyped)), None, "{untyped}");
    }
}

// `FalseObj` is false, so the real value is a `Foo`: the fallback is taken
// at its word rather than ruled out by the object's truthiness.
#[test]
fn an_overloaded_bool_reaches_the_fallback() {
    let src = "package FalseObj; use overload 'bool' => sub { 0 }; sub new { bless {}, shift }\n\
package Foo; sub new { bless {}, shift }\n\
sub probe { my $f = FalseObj->new; return $f || Foo->new }\n1;\n";
    let got = build_fa(src).sub_return_type_at_arity("probe", None);
    assert_eq!(class(got).as_deref(), Some("Foo"));
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
    // The implicit read sees the guard's narrowing, and the arms join to
    // the floor.
    assert_eq!(ret("g1"), Some(InferredType::ClassName("Baz".into())));
    // The RHS reads the narrowed value; the write then ends the region.
    assert_eq!(ret("g2"), Some(InferredType::ClassName("Baz".into())));
    assert_eq!(ret("g3"), Some(InferredType::String));
    // `my $x;` is undef, so `||=` always takes the RHS.
    assert_eq!(ret("g4"), Some(bar()));
    // An optional LHS falls back to the RHS floor.
    assert_eq!(ret("g5"), Some(bar()));
    // `defined` strips the Optional up to the write, then the `||=` arms
    // join to the floor; the sub is Optional only because of its bare
    // `return`.
    assert_eq!(ret("g6"), Some(InferredType::Optional(Box::new(InferredType::ClassName("Baz".into())))));
}


#[test]
fn element_writes_extend_an_empty_hash() {
    for (body, want) in [
        ("my $h = {}; $h->{k} = Bar->new; return $h->{k};", Some("Bar")),
        ("my %h; $h{k} = Bar->new; return $h{k};", Some("Bar")),
        ("my %h = (); $h{k} = Bar->new; return $h{k};", Some("Bar")),
        ("my $h = {}; $h->{k} ||= Bar->new; return $h->{k};", Some("Bar")),
        ("my $h = {}; $h->{k} //= Bar->new; return $h->{k};", Some("Bar")),
        ("my $h = {}; $h->{k} = Bar->new; $h->{k} ||= Baz->new; return $h->{k};", Some("Baz")),
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

// A shape nothing wrote has no keys, so every read misses one.
#[test]
fn an_empty_shape_reports_every_read_key() {
    let src = "package Foo;\nsub f { my %state; $state{seen} }\nsub g { {} }\nsub h { g()->{x} }\n1;\n";
    let fa = build_fa(src);
    let closed = fa.closed_shape_key_typos(None);
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0].key, "seen");
    assert!(closed[0].known_keys.is_empty());
    let projected = fa.projected_key_typos(None);
    assert_eq!(projected.len(), 1, "{projected:?}");
    assert_eq!(projected[0].key, "x");
}

// An exit fallback leaves the sub, so it never becomes the value: the
// variable holds the LHS made true. Webmin's MIME::Body, where an abstract
// `open` returns undef, read `$io` as undef.
#[test]
fn an_exit_fallback_contributes_no_value() {
    let src = "package Bar; sub new { bless {}, shift }\n\
package Foo;\n\
sub nothing { undef }\n\
sub maybe { return undef if $_[0]; Bar->new }\n\
sub a { my $x = nothing() || return (); $x }\n\
sub b { my $x = maybe(1) // die 'none'; $x }\n\
sub c { my $x = maybe(1) || return; $x }\n\
1;\n";
    let fa = build_fa(src);
    let ret = |s: &str| fa.sub_return_type_at_arity(s, None);
    let bar = || Some(InferredType::ClassName("Bar".into()));
    assert!(!ret("a").is_some_and(|t| t.is_undef()), "{:?}", ret("a"));
    assert_eq!(ret("b"), bar());
    // `c` also returns from the bare `return`.
    assert_eq!(ret("c"), Some(InferredType::Optional(Box::new(InferredType::ClassName("Bar".into())))));
}

// Every declarator names its variable through the CST, so `state` and
// `local` fold like `my` and `our`.
#[test]
fn every_declarator_folds_its_constant() {
    for decl in ["my", "our", "state", "local"] {
        let src = format!("package Foo;\nsub f {{ {decl} $m = 'process'; Foo->new->$m() }}\n");
        let fa = build_fa(&src);
        let got: Vec<String> = fa
            .refs()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }))
            .map(|r| r.target_name.clone())
            .collect();
        assert!(got.iter().any(|t| t == "process"), "{decl}: {got:?}");
    }
}

