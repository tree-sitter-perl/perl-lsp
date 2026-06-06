package RexAll;
use Exporter 'import';
use RexBase ();
use RexMore ();
# Form 1 (static splice) + form 2 (loop-push) re-export.
our @EXPORT = ('all_fn', @RexBase::EXPORT);
for my $m (qw(RexMore)) { no strict 'refs'; push @EXPORT, @{"${m}::EXPORT"} }
sub all_fn { 'all' }
1;
