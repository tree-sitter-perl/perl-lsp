package My::Rel;

# The relationship verb, defined in a component/mixin ancestor — the DBIC
# `belongs_to`/`has_many` shape. Descendants invoke it as a class method.
sub link_to {
    my ($class, $rel, $target) = @_;
    return 1;
}

1;
