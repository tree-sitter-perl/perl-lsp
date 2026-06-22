use strict;
use warnings;
use lib 'lib';
use Account;

my $acct = Account->new( owner => 'Ada', balance => 100 );

$acct->deposit(50);

print $acct->describe, "\n";
