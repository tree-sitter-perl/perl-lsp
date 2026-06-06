use lib 'lib';
use RexAll;
my $a = base_fn();   # re-exported from RexBase via RexAll (static splice)
my $b = more_fn();   # re-exported from RexMore via RexAll (loop-push)
my $c = all_fn();    # RexAll's own
