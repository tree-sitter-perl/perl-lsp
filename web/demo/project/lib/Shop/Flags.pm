package Shop::Flags;
use strict;
use warnings;
use Exporter 'import';

our @EXPORT_OK = qw(feature_flag enabled disabled);

# A home-grown DSL: a flag is declared once, by name, and looked up by
# that name everywhere else. The server learns the link between the two
# from .perl-lsp/flags.rhai.
my %FLAGS;

sub feature_flag {
    my ($name, %spec) = @_;
    $FLAGS{$name} = {%spec};
    return;
}

sub enabled {
    my ($name) = @_;
    my $flag = $FLAGS{$name} or die "no such flag: $name";
    return $ENV{ 'SHOP_' . uc $name } // $flag->{default};
}

sub disabled { return !enabled(@_) }

feature_flag new_checkout => (default => 0, owner => 'payments');
feature_flag gift_wrap    => (default => 1, owner => 'fulfilment');
feature_flag beta_search  => (default => 0, owner => 'catalog');

1;
