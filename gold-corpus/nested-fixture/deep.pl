use strict;
use warnings;
use lib 'lib';
use DeepRow;

# The invocant is directly typed via the constructor pattern (DeepRow->new),
# so goto-def / references on the 2-hop-synthesized accessors must reach
# DeepRow.pm — proving cross-file ClassIsa synthesis is visible cross-file.
my $row = DeepRow->new;
my $w   = $row->widgets;
my $t   = $row->title;

# H7-8: an inline resultset->search->first chain (no intermediate var)
# projects the row type, so goto-def on the row's accessor reaches DeepRow.pm.
my $inline = $schema->resultset('DeepRow')->search({ title => 'x' })->first->widgets;

# H7-8: list-context row extraction binds each scalar to the row class.
my ($only) = $schema->resultset('DeepRow')->all;
my $lt = $only->title;
