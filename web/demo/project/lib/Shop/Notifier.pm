package Shop::Notifier;
use Mojo::Base 'Mojo::EventEmitter', -signatures;

# Events are registered and emitted by name.
sub new ( $class, @args ) {
    my $self = $class->SUPER::new(@args);
    $self->on( shipped => sub ( $emitter, $order ) { print "order $order shipped\n" } );
    return $self;
}

sub ship ( $self, $order ) {
    $self->emit( shipped => $order );
    return $self;
}

1;
