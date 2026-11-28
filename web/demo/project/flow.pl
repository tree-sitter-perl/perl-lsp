#!/usr/bin/env perl
use strict;
use warnings;
use feature 'try';
no warnings 'experimental::try';
use lib 'lib';

use Shop::Cart;
use Shop::Item;

# Shop::Item->parse returns an item, or nothing for a line it cannot read.
# Every sub below starts from that "maybe" and follows the control flow.

sub fail { die "import failed: @_\n" }

# An early return narrows everything after it.
sub label_of {
    my ($line) = @_;
    my $item = Shop::Item->parse($line);
    return 'unknown' unless defined $item;
    return $item->label;
}

# So does a call to a sub that never returns.
sub must_parse {
    my ($line) = @_;
    my $item = Shop::Item->parse($line);
    fail($line) unless defined $item;
    return $item->name;
}

# `// die` leaves no undef behind.
sub parse_or_die {
    my ($line) = @_;
    my $item = Shop::Item->parse($line) // die "bad line\n";
    return $item;
}

# In a loop, `next` narrows the rest of the body.
sub import_lines {
    my ( $cart, @lines ) = @_;
    for my $line (@lines) {
        my $item = Shop::Item->parse($line);
        next unless defined $item;
        $cart->add( $item, 1 );
    }
    return $cart;
}

# Assigning again forgets what the guard proved.
sub reparse {
    my ( $line, $other ) = @_;
    my $item = Shop::Item->parse($line);
    return unless defined $item;
    my $first = $item->name;
    $item = Shop::Item->parse($other);
    return $item->price;
}

# A guard on a value that is never undef.
sub total_of {
    my $cart = Shop::Cart->new( currency => 'EUR' );
    if ( defined $cart ) {
        return $cart->subtotal;
    }
    return 0;
}

# A closure runs later: by then $item may have been assigned again.
sub later {
    my ($line) = @_;
    my $item = Shop::Item->parse($line);
    return unless defined $item;
    my $show = sub { $item->name };
    $item = undef;
    return $show;
}

# try/catch: assigned on one path, untouched on the other.
sub safe_parse {
    my ($line) = @_;
    my $item;
    try {
        $item = parse_or_die($line);
    }
    catch ($e) {
        warn $e;
    }
    return $item;
}

# The guard proved an array; the next line treats it as a hash.
sub count_of {
    my ($input) = @_;
    if ( ref $input eq 'ARRAY' ) {
        return $input->{count};
    }
    return 0;
}
