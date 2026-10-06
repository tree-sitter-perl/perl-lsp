//! A call's value is an `Invoke`: the callee looked up on the receiver,
//! with the receiver's full type and the call's own args substituted.

use super::*;

const PRELUDE: &str = "\
package Base;
sub new { my $class = shift; return bless {}, $class }
sub me { my $self = shift; return $self }
sub first { return $_[0] }
sub peer { return Base->new }
sub name {
    my $self = shift;
    return $self->{name} unless @_;
    $self->{name} = shift;
    return $self;
}
package Kid;
our @ISA = ('Base');
sub new { my $class = shift; my $self = $class->SUPER::new(@_); return $self }
package Foo;
";

fn returns(body: &str) -> Option<InferredType> {
    let src = format!("{PRELUDE}sub probe {{ {body} }}\n1;\n");
    build_fa(&src).sub_return_type_at_arity("probe", None)
}

fn class(t: Option<InferredType>) -> Option<String> {
    t.and_then(|t| t.class_name().map(str::to_string))
}

#[test]
fn the_receiver_binds_through_the_argument_window() {
    // `return $_[0]` reads the receiver out of `@_` at the call.
    assert_eq!(class(returns("my $k = Kid->new; return $k->first;")).as_deref(), Some("Kid"));
}

#[test]
fn super_looks_up_on_the_writers_parents_with_the_callers_receiver() {
    // `Kid::new` delegates to `Base::new` through SUPER, but blesses the Kid.
    assert_eq!(class(returns("return Kid->new;")).as_deref(), Some("Kid"));
}

#[test]
fn a_coderef_call_takes_its_first_arg_as_the_receiver() {
    let body = "my $cb = \\&Base::first; my $k = Kid->new; return $cb->($k);";
    assert_eq!(class(returns(body)).as_deref(), Some("Kid"));
}

#[test]
fn the_receiver_is_whatever_the_invocant_holds() {
    let src = format!(
        "{PRELUDE}package Kid;\n\
         sub viapkg {{ __PACKAGE__->first }}\n\
         sub viac {{ my $c = Kid->new; $c->first }}\n\
         sub viafold {{ my $k = 'Kid'; $k->first }}\n1;\n"
    );
    let fa = build_fa(&src);
    for sub in ["viapkg", "viac", "viafold"] {
        assert_eq!(class(fa.sub_return_type_at_arity(sub, None)).as_deref(), Some("Kid"), "{sub}");
    }
}

#[test]
fn a_postfix_deref_argument_spreads() {
    // `first` reads `$_[0]`, the receiver, only while the window is exact.
    let src = format!("{PRELUDE}sub probe {{ my $r = []; Base->first($r->@*) }}\n1;\n");
    let fa = build_fa(&src);
    assert_eq!(class(fa.sub_return_type_at_arity("probe", None)).as_deref(), Some("Base"));
}

