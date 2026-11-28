package Shop::Jobs;
use Mojo::Base 'Mojolicious::Plugin', -signatures;

# Background jobs with Minion.
sub register ( $self, $app, $conf ) {
    $app->plugin( Minion => { SQLite => 'shop.db' } );

    # A task is registered by name.
    $app->minion->add_task(
        send_receipt => sub ( $job, %args ) {
            my $to    = $args{to};
            my $total = $args{total};
            $job->finish("receipt for $total sent to $to");
        }
    );
}

# Enqueued by the same name, with the arguments the task reads.
sub receipt_for ( $self, $app, $order ) {
    return $app->minion->enqueue( send_receipt => [ to => 'me@example.com', total => 12 ], { priority => 5 } );
}

1;
