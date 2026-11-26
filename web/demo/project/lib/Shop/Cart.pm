package Shop::Cart;
use strict;
use warnings;

use Shop::Util qw(round_cents);

=head1 NAME

Shop::Cart - a shopping cart

=head2 new

    my $cart = Shop::Cart->new( currency => 'EUR' );

Creates an empty cart. C<currency> defaults to C<USD>.

=cut

sub new {
    my ( $class, %args ) = @_;
    return bless {
        currency  => $args{currency} // 'USD',
        lines     => [],
        discounts => [],
        extras    => {},
    }, $class;
}

sub currency { $_[0]{currency} }

=head2 add

    $cart->add( $item, $quantity );

Adds C<$quantity> of an item (one by default) and returns the cart, so calls
chain.

=cut

sub add {
    my ( $self, $item, $quantity ) = @_;
    push @{ $self->{lines} }, { item => $item, quantity => $quantity // 1 };
    return $self;
}

sub apply {
    my ( $self, $discount ) = @_;
    push @{ $self->{discounts} }, $discount;
    return $self;
}

sub subtotal {
    my ($self) = @_;
    my $sum = 0;
    $sum += $_->{item}->price * $_->{quantity} for @{ $self->{lines} };
    return $sum;
}

sub add_shipping {
    my ($self) = @_;
    $self->{extras}{shipping} = $self->subtotal > 50 ? 0 : 4.95;
    return $self;
}

sub add_tax {
    my ($self) = @_;
    $self->{extras}{tax} = round_cents( $self->subtotal * 0.2 );
    return $self;
}

=head2 totals

Returns a hash reference with C<subtotal>, C<discount> and C<total>.

=cut

sub totals {
    my ($self) = @_;
    my $subtotal = $self->subtotal;
    my $discount = 0;
    $discount += $_->amount_off($subtotal) for @{ $self->{discounts} };
    return {
        subtotal => $subtotal,
        discount => $discount,
        total    => round_cents( $subtotal - $discount ),
    };
}

1;
