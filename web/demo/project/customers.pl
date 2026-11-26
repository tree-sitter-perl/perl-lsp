#!/usr/bin/env perl
use strict;
use warnings;
use lib 'lib';

use Shop::Cart;
use Shop::Customer;

my $customer = Shop::Customer->new( name => 'Ada', cart => Shop::Cart->new );

my $name   = $customer->name;
my $points = $customer->points;
my $cart   = $customer->cart;

# `handles` delegates to the cart.
my $totals = $customer->totals;
$customer->add( undef, 2 );

$customer->forget_address if $customer->has_address;
