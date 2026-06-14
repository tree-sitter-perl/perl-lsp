package BrandedShared::Plugin;
use Mojo::Base 'Mojolicious::Plugin';

# A helper from a shared plugin — NOT a self-contained Mojo::Lite app, so
# its namespace stays UNBRANDED (global) and EVERY app sees it. The
# branded-edges e2e uses it as the "cross-file resolution is live + brands
# are additive" sentinel. See docs/adr/branded-edges.md.
sub register {
    my ($self, $app) = @_;
    $app->helper(shared_global_helper => sub { 'shared' });
}

1;
