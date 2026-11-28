#!/usr/bin/env perl
use strict;
use warnings;
use lib 'lib';

use Shop::Cart;
use Shop::Item;
use Shop::Discount::Percent;
use Shop::Order;
use Shop::Util qw(format_price split_tax);

# No annotations anywhere: every type below is inferred.
my $cart = Shop::Cart->new( currency => 'EUR' );

my $book = Shop::Item->new( name => 'Programming Perl', price => 39.90 );
my $mug  = Shop::Item->new( name => 'Camel mug', price => 12, tags => ['kitchen'] );

# Methods that return $self chain, and the chain keeps its type.
$cart->add( $book, 2 )->add($mug)->apply( Shop::Discount::Percent->new( rate => 10 ) );

my $totals = $cart->totals;
print format_price( $totals->{total}, $cart->currency ), "\n";

# split_tax() returns a hash. Its keys complete here, and go to definition
# on one lands on the key inside the sub.
my $parts = split_tax( $totals->{total}, 0.2 );
print "net $parts->{net}, tax $parts->{tax}\n";

# describe() comes from a role that Shop::Item consumes.
print $book->describe, "\n";

# parse() can return undef, so $parsed is Maybe<Shop::Item>: the first call
# is flagged, the guarded one is not.
my $parsed = Shop::Item->parse('Tea 4.50');
print $parsed->name, "\n";
print $parsed->name, "\n" if defined $parsed;

# A method name built at runtime still resolves.
for my $kind (qw(shipping tax)) {
    my $method = "add_$kind";
    $cart->$method;
}

# A Perl 5.38 class, with a field reader.
my $order = Shop::Order->new( cart => $cart, customer => 'ada@example.com' );
print $order->summary, "\n";

# A typo in a method name, and a function nobody imported (it has a quick fix).
$cart->totls;
print round_cents(3.14159), "\n";
