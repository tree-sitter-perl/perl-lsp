package Shop::Customer;
use Moo;
use Types::Standard qw(Str Int ArrayRef InstanceOf is_Int);

# Type::Tiny constraints type the accessors, and `handles` delegates.
has name    => ( is => 'ro', isa => Str, required => 1 );
has points  => ( is => 'rw', isa => Int, default => 0 );
has cart    => ( is => 'ro', handles => [qw(add totals)], isa => InstanceOf ['Shop::Cart'] );
has address => ( is => 'rw', predicate => 'has_address', clearer => 'forget_address' );

sub reward {
    my ( $self, $amount ) = @_;
    return $self unless is_Int($amount);    # a Type::Tiny check narrows
    $self->points( $self->points + $amount );
    return $self;
}

1;
