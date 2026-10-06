package MyApp::Controller::Alerts;
use Mojo::Base 'Mojolicious::Controller';

sub list { my $c = shift; $c->render(text => 'list') }

sub show { my $c = shift; $c->render(text => 'show') }

sub settings { my $c = shift; $c->render(text => 'settings') }

1;
