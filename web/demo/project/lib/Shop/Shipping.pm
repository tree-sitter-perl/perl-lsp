package Shop::Shipping;
use strict;
use warnings;

sub new {
    my ( $class, %args ) = @_;
    return bless {
        carrier => $args{carrier} // 'post',
        express => $args{express},
        insured => $args{insured},
    }, $class;
}

# Returns a list: the price, then a line describing the route.
sub quote {
    my ( $self, %opt ) = @_;
    my $weight = $opt{weight} // 1;
    my $zone   = $opt{zone}   // 'EU';
    my $price  = $weight * ( $self->{express} ? 9 : 4 );
    return ( $price, "$zone via $self->{carrier}" );
}

1;
