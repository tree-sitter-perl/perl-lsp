#!/usr/bin/env perl
# Spike fixture: the array hop. End-to-end chain through the
# bag-canonical typing arch — every hop is exercised on its own
# infrastructure, the new code on the spike branch is purely the
# array contribution + projection.
#
# ┌─ const fold ─────────────────────────────────────────────┐
# │  use constant DEFAULT_NAME => 'alice'                     │
# └──────────────────┬────────────────────────────────────────┘
#                    │ folds to 'alice' at call sites
# ┌─ Mojo helper ────▼────────────────────────────────────────┐
# │  $app->helper(make_user => sub {                          │
# │      return Some::User->new(name => $name);               │
# │  });                                                      │
# │  ↓ plugin synth (mojo-helpers): Method on                 │
# │    Mojolicious::Controller, return_via_edge → anon body   │
# └──────────────────┬────────────────────────────────────────┘
#                    │
# ┌─ coderef return ─▼────────────────────────────────────────┐
# │  anon body last expr is `Some::User->new(...)`            │
# │  → ClassName("Some::User") via constructor pattern        │
# └──────────────────┬────────────────────────────────────────┘
#                    │
# ┌─ array contribution (NEW) ─▼──────────────────────────────┐
# │  push @users, $c->make_user(DEFAULT_NAME);                │
# │  push @users, $c->make_user('bob');                       │
# │  → Variable{"@users", scope} + Sequence([User, User])     │
# └──────────────────┬────────────────────────────────────────┘
#                    │
# ┌─ array projection (NEW) ───▼──────────────────────────────┐
# │  $users[0]                                                │
# │  → Sequence.element_at(0) → ClassName("Some::User")       │
# └──────────────────┬────────────────────────────────────────┘
#                    │
# ┌─ cross-file completion ─▼─────────────────────────────────┐
# │  $users[0]->greet()    ← Some::User method                │
# │  $users[0]->email()    ← Some::User method                │
# │  $users[0]->name()     ← Mojo::Base accessor              │
# └────────────────────────────────────────────────────────────┘

use Mojolicious::Lite;
use Some::User;
use constant DEFAULT_NAME => 'alice';

my $app = Mojolicious->new;

$app->helper(make_user => sub {
    my ($c, $name) = @_;
    return Some::User->new(name => $name);
});

sub action {
    my $c = Mojolicious::Controller->new;
    my @users;
    push @users, $c->make_user(DEFAULT_NAME);   # const fold + plugin helper
    push @users, $c->make_user('bob');

    # Put your cursor right after the `->` and trigger completion.
    # Methods offered: greet, email, name (the Mojo::Base accessor).
    $users[0]->greet();
    $users[0]->name();
}
