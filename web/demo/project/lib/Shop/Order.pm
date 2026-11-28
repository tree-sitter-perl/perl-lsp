use v5.38;
use experimental 'class';

class Shop::Order;

use Shop::Util qw(format_price);

field $cart     :param :reader;
field $customer :param :reader;
field $placed   :reader = time;

method summary {
    my $totals = $cart->totals;
    return sprintf '%s owes %s', $customer, format_price( $totals->{total}, $cart->currency );
}

method is_free {
    return $cart->totals->{total} == 0;
}

1;
