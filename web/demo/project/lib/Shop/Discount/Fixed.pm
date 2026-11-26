package Shop::Discount::Fixed;
use strict;
use warnings;
use parent 'Shop::Discount';

sub amount_off {
    my ( $self, $subtotal ) = @_;
    my $amount = $self->{amount};
    return $amount > $subtotal ? $subtotal : $amount;
}

1;
