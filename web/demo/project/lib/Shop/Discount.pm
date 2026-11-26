package Shop::Discount;
use strict;
use warnings;

sub new {
    my ( $class, %args ) = @_;
    return bless {%args}, $class;
}

# Subclasses override this.
sub amount_off {
    my ( $self, $subtotal ) = @_;
    return 0;
}

sub describe {
    my ($self) = @_;
    return ref($self) . ' saves ' . $self->amount_off(100) . ' per 100';
}

1;
