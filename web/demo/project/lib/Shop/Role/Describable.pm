package Shop::Role::Describable;
use Moo::Role;

requires 'label';

# A role's methods land on every class that consumes it.
sub describe {
    my ($self) = @_;
    return sprintf '<%s>', $self->label;
}

1;
