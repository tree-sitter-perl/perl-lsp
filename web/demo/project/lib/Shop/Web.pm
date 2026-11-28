package Shop::Web;
use Mojo::Base 'Mojolicious', -signatures;

use Shop::Cart;

# A Mojolicious application. The framework sources ship with this page, so
# everything here resolves into real Mojolicious code.
sub startup ($self) {

    # A helper is callable on every controller; its return type follows it.
    $self->helper( current_cart => sub ($c) { Shop::Cart->new( currency => 'EUR' ) } );

    my $r = $self->routes;
    $r->get('/cart')->to('cart#show');
    $r->post('/cart/add')->to('cart#add');
}

1;
