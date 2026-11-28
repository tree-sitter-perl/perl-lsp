#!/usr/bin/env perl
use strict;
use warnings;
use lib 'lib';
use Shop::Flags qw(enabled disabled);

# The names below are plain strings to perl. The project's own plugin,
# .perl-lsp/flags.rhai, makes each one a link to its declaration.
my $checkout = enabled('new_checkout') ? 'one page' : 'three steps';
print "checkout: $checkout\n";

print "offering gift wrap\n" if enabled('gift_wrap');

# The plugin does not know this verb yet. Open .perl-lsp/flags.rhai, add
# "disabled" to the verbs its query matches, and this name becomes a link too.
print "classic search\n" if disabled('beta_search');
