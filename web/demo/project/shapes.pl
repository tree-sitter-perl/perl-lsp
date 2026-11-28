#!/usr/bin/env perl
use strict;
use warnings;
use lib 'lib';

use Shop::Discount::Fixed;
use Shop::Discount::Percent;
use Shop::Shipping;

# One of two classes, depending on the code.
sub pick_discount {
    my ($code) = @_;
    return Shop::Discount::Percent->new( rate => 10 ) if $code =~ /%$/;
    return Shop::Discount::Fixed->new( amount => 5 );
}

my $discount = pick_discount('10%');
print $discount->describe, "\n";
print $discount->rate, "\n";

# A hash that is built up over several statements.
my $settings = { port => 8080 };
$settings->{host} = 'localhost';
push @{ $settings->{paths} }, '/static';
my $port = $settings->{port};

# Named arguments: the keys a sub reads are the keys a call may pass.
my $shipping = Shop::Shipping->new( carrier => 'dhl', );

# A list return types each variable it is unpacked into.
my ( $price, $route ) = $shipping->quote( weight => 2, zone => 'US' );
