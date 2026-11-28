package Shop::Item;
use Moo;

with 'Shop::Role::Describable';

# Moo attributes become methods; `isa` gives them a type.
has name  => ( is => 'ro', required => 1 );
has price => ( is => 'rw', isa => sub { die 'not a number' unless $_[0] =~ /^[\d.]+$/ } );
has tags  => ( is => 'ro', default => sub { [] }, predicate => 'has_tags' );
has sku   => ( is => 'lazy', clearer => 'reset_sku' );

sub _build_sku {
    my ($self) = @_;
    return uc join '-', split /\s+/, $self->name;
}

# Required by Shop::Role::Describable.
sub label {
    my ($self) = @_;
    return $self->name;
}

# Parses "Tea 4.50". Returns nothing when the text has no price, so callers
# get a value that may be undef.
sub parse {
    my ( $class, $text ) = @_;
    return undef unless $text =~ /^(.+?)\s+([\d.]+)$/;
    return $class->new( name => $1, price => $2 );
}

sub on_sale {
    my ( $self, $percent ) = @_;
    $self->price( $self->price * ( 1 - $percent / 100 ) );
    return $self;
}

1;
