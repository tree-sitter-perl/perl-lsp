package Shop::Discount::Percent;
use strict;
use warnings;
use parent 'Shop::Discount';

sub rate { $_[0]{rate} }

sub amount_off {
    my ( $self, $subtotal ) = @_;
    return $subtotal * $self->rate / 100;
}

sub describe {
    my ($self) = @_;
    return $self->rate . '% off: ' . $self->SUPER::describe();
}

1;
