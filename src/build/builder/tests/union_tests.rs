//! A value that is one of several types: every consumer of its value reads
//! each arm and joins what the arms answer.

use super::*;

const PRELUDE: &str = "\
package Cat;
sub new { my $class = shift; return bless {}, $class }
sub me { my $self = shift; return $self }
sub sound { return 'meow' }
sub legs { return 4 }
sub purr { return 'purr' }
package Bird;
sub new { my $class = shift; return bless {}, $class }
sub me { my $self = shift; return $self }
sub sound { return 'tweet' }
sub legs { return [1, 2] }
package main;
";

fn returns(body: &str) -> Option<InferredType> {
    let src = format!("{PRELUDE}sub probe {{ my $pet = rand() ? Cat->new : Bird->new; {body} }}\n1;\n");
    build_fa(&src).sub_return_type_at_arity("probe", None)
}

fn arms(t: &InferredType) -> Vec<String> {
    match t {
        InferredType::Union(arms) => arms.iter().map(format_inferred_type).collect(),
        t => vec![format_inferred_type(t)],
    }
}

#[test]
fn the_receiver_is_a_union_of_its_branches() {
    let t = returns("return $pet;").expect("typed");
    assert_eq!(arms(&t), ["Bird", "Cat"]);
}

#[test]
fn a_call_on_a_union_joins_what_each_arm_returns() {
    // Both arms agree on the value's type.
    assert_eq!(returns("return $pet->sound;"), Some(InferredType::String));
    // They disagree, so the call is either.
    let t = returns("return $pet->legs;").expect("typed");
    assert!(matches!(&t, InferredType::Union(a) if a.len() == 2), "got {t:?}");
}

#[test]
fn each_arm_is_the_receiver_its_method_sees() {
    let t = returns("return $pet->me;").expect("typed");
    assert_eq!(arms(&t), ["Bird", "Cat"]);
    // And the chain keeps dispatching on both.
    assert_eq!(returns("return $pet->me->sound;"), Some(InferredType::String));
}

#[test]
fn an_arm_without_the_method_adds_nothing() {
    // `purr` is Cat's alone: a Bird there is a runtime error, not a type.
    assert_eq!(returns("return $pet->purr;"), Some(InferredType::String));
}

#[test]
fn a_drill_into_a_union_reads_every_arm_it_can() {
    let src = "sub k { my $v = rand() ? { k => 'x' } : [1]; return $v->{k} }\n\
               sub i { my $v = rand() ? { k => 'x' } : [1]; return $v->[0] }\n1;\n";
    let fa = build_fa(src);
    assert_eq!(fa.sub_return_type_at_arity("k", None), Some(InferredType::String));
    assert_eq!(fa.sub_return_type_at_arity("i", None), Some(InferredType::Numeric));
}

#[test]
fn a_maybe_union_receiver_distributes_like_the_union() {
    // `A|B|undef` is `Optional(Union[A, B])`: every member read sees through
    // the undef half to the same arms the bare union has.
    let src = format!(
        "{PRELUDE}sub pick {{ my $n = shift; return Cat->new if $n == 1; return Bird->new if $n == 2; return undef }}\n\
         sub probe {{ my $pet = pick(1); return $pet->legs }}\n\
         sub one {{ my $c = rand() ? Cat->new : undef; return $c->purr }}\n1;\n"
    );
    let fa = build_fa(&src);
    let pet = fa.sub_return_type_at_arity("pick", None).expect("typed");
    assert!(matches!(&pet, InferredType::Optional(inner) if matches!(**inner, InferredType::Union(_))), "got {pet:?}");
    let t = fa.sub_return_type_at_arity("probe", None).expect("typed");
    assert!(matches!(&t, InferredType::Union(a) if a.len() == 2), "got {t:?}");
    assert_eq!(fa.sub_return_type_at_arity("one", None), Some(InferredType::String));
}
