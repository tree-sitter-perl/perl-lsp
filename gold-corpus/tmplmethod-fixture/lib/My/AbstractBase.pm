package My::AbstractBase;

# Shape B: the demand lives in a base and the definition in a DESCENDANT.
# Nothing records that the base has an obligation, so no edge connects the
# two at all — unlike the sibling-role shape, where the composer supplies
# the path. This is the population a `demands` lane would convert.
sub run {
    my $self = shift;
    return $self->fetch_raw;
}

1;
