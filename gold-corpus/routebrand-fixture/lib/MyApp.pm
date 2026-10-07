package MyApp;
use Mojo::Base 'Mojolicious';

sub startup {
    my $self = shift;
    my $r = $self->routes;
    my $alerts = $r->any('/alerts')->to('alerts#', section => 'admin');
    $alerts->get('/')->to('#list');
    my $crud = $alerts->under('/:type')->to('#show');
    $crud->get('/settings')->to('#settings');
    my $other = $r->any('/other')->to('other#');
    $other->get('/x')->to('#thing');
}

1;
