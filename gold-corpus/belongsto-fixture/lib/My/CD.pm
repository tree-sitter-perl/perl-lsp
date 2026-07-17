package My::CD;
use base 'My::Core';

__PACKAGE__->link_to( artist => 'My::Artist' );

1;
