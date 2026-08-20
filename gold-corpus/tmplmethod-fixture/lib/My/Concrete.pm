package My::Concrete;
use parent -norequire, 'My::AbstractBase';

sub fetch_raw {
    return 7;
}

1;
