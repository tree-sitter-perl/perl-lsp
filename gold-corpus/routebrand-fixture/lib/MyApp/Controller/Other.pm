package MyApp::Controller::Other;
use Mojo::Base 'Mojolicious::Controller';

sub thing { my $c = shift; $c->render(text => 'thing') }

1;
