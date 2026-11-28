package Shop::Web::Controller::Cart;
use Mojo::Base 'Mojolicious::Controller', -signatures;

sub show ($self) {
    my $cart = $self->current_cart;
    $self->render( json => $cart->totals );
}

sub add ($self) {
    my $name = $self->param('name');
    return $self->render( text => 'name is required', status => 400 ) unless $name;
    $self->redirect_to('/cart');
}

1;
