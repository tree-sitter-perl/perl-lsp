package Shop::Util;
use strict;
use warnings;
use Exporter 'import';

our @EXPORT_OK = qw(format_price round_cents split_tax parse_quantity);

=head2 format_price

    format_price( 12.5, 'EUR' );    # "12.50 EUR"

Formats an amount with two decimals and its currency.

=cut

sub format_price {
    my ( $amount, $currency ) = @_;
    return sprintf '%.2f %s', $amount, $currency // 'USD';
}

sub round_cents {
    my ($amount) = @_;
    return int( $amount * 100 + 0.5 ) / 100;
}

# Callers see the keys of the hash this returns.
sub split_tax {
    my ( $gross, $rate ) = @_;
    my $net = round_cents( $gross / ( 1 + $rate ) );
    return { net => $net, tax => round_cents( $gross - $net ), gross => $gross };
}

# The answer depends on what came in: a list of numbers, or nothing.
sub parse_quantity {
    my ($input) = @_;
    return unless defined $input;
    if ( ref $input eq 'ARRAY' ) {
        return scalar @$input;
    }
    return $input =~ /^(\d+)$/ ? $1 + 0 : 1;
}

1;
