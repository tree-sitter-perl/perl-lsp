package My::Caller;
use Moo::Role;

# The template half: generic machinery that dispatches to a step it does
# not provide and does not declare `requires` for. The provider is a
# SIBLING role, not an ancestor — reachable only through the composer.
sub run {
    my $self = shift;
    return $self->fetch_raw;
}

1;
