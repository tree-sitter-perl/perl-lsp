package Shop::Schema::Result::Review;
use strict;
use warnings;
use base 'DBIx::Class::Core';

__PACKAGE__->table('reviews');
__PACKAGE__->add_columns(
    id         => { data_type => 'integer', is_auto_increment => 1 },
    product_id => { data_type => 'integer' },
    stars      => { data_type => 'integer' },
    body       => { data_type => 'text' },
);
__PACKAGE__->set_primary_key('id');
__PACKAGE__->belongs_to( product => 'Shop::Schema::Result::Product', 'product_id' );

1;
