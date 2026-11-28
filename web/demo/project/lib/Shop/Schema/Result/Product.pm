package Shop::Schema::Result::Product;
use strict;
use warnings;
use base 'DBIx::Class::Core';

__PACKAGE__->table('products');

# Columns and relationships become methods on the row.
__PACKAGE__->add_columns(
    id    => { data_type => 'integer', is_auto_increment => 1 },
    name  => { data_type => 'text' },
    price => { data_type => 'numeric' },
);
__PACKAGE__->set_primary_key('id');
__PACKAGE__->has_many( reviews => 'Shop::Schema::Result::Review', 'product_id' );

sub display_name {
    my ($self) = @_;
    return sprintf '%s (%s)', $self->name, $self->price;
}

1;
