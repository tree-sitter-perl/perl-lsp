package My::Core;
use base 'My::Rel';

# An intermediate base — the verb reaches descendants only through this
# extra cross-file hop (mirrors CD -> BaseResult -> ...Core -> ...BelongsTo).
1;
