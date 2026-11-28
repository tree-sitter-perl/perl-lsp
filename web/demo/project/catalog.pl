#!/usr/bin/env perl
use strict;
use warnings;
use lib 'lib';

use Shop::Schema;

# DBIx::Class: the result class is found from the source name.
my $schema = Shop::Schema->connect('dbi:SQLite:shop.db');

my $products = $schema->resultset('Product')->search( { price => { '<' => 20 } } );

while ( my $product = $products->next ) {
    print $product->display_name, "\n";

    # Column accessors come from add_columns, relationships from has_many.
    print '  costs ', $product->price, "\n";
    my $reviews = $product->reviews;
}
