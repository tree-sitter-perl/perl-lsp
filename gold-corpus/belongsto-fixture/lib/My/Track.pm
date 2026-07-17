package My::Track;
use base 'My::Core';

__PACKAGE__->link_to( cd => 'My::CD' );

1;
