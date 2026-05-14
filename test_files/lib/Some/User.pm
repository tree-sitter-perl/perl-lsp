package Some::User;

# Cross-file class the spike test resolves to. `name` is a Mojo::Base
# accessor (synthesizes both a Method and a HashKeyDef on this class),
# and the two regular methods stand in for "things the user can call
# on a User instance."

use Mojo::Base -base;

has 'name';

sub greet {
    my $self = shift;
    "hi $self->{name}";
}

sub email {
    my $self = shift;
    "$self->{name}\@example.com";
}

1;
