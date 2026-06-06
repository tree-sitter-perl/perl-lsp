# completion

Generated from `fixtures/completion.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl completion`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| completion-datetime-self-classic | simple | classic | `x86_64-linux/DateTime.pm:207:11` | all: new	DateTime; now	DateTime; today	DateTime; clone	DateTime; year	DateTime; month	DateTime; formatter	DateTime; _set_locale	DateTime | gold | new	DateTime → DateTime; now; today; clone; year; month; formatter; _set_locale |
| completion-catalyst-action-has-moose | tricky | moose | `Catalyst/Action.pm:66:20` | all: class	Catalyst::Action; instance	Catalyst::Action; has_instance	Catalyst::Action; namespace	Catalyst::Action; reverse	Catalyst::Action; attributes	Catalyst::Action; name	Catalyst::Action; code	Catalyst::Action; private_path	Catalyst::Action; number_of_args	Catalyst::Action | gold | class;instance;has_instance;namespace;reverse;attributes;name;code;private_path;number_of_args → Int\|Undef |
| completion-uri-http-multilevel-isa | tricky | oo-isa | `URI/http.pm:14:23` | all: default_port	URI::http; canonical	URI::http; host	URI::http (from URI::_server); port	URI::http (from URI::_server); userinfo	URI::http (from URI::_server); authority	URI::http (from URI::_generic); path	URI::http (from URI::_generic); scheme	URI::http (from URI); new	URI::http (from URI) | gold | default_port → Numeric; canonical → URI; userinfo/host/port (from URI::_server); authority/path (from URI::_generic); new/scheme (from URI) |
| completion-datetime-hashkey | tricky | type-inference | `x86_64-linux/DateTime.pm:315:30` | all: local_rd_days	DateTime->{local_rd_days}; local_rd_secs	DateTime->{local_rd_secs}; formatter	DateTime->{formatter}; locale	DateTime->{locale}; offset_modifier	DateTime->{offset_modifier}; rd_nanosecs	DateTime->{rd_nanosecs}; utc_year	DateTime->{utc_year} | xfail | utc_vals	DateTime->{utc_vals}; tz	DateTime->{tz} (only 2 of 13 keys) |
| completion-uri-escape-fq-crossfile | tricky | fq | `URI.pm:141:41` | all: uri_escape	URI::Escape; uri_unescape	URI::Escape; uri_escape_utf8	URI::Escape; escape_char	URI::Escape | gold | uri_escape → String; uri_escape_utf8 → String; uri_unescape; escape_char |
| completion-datetime-partial-off | simple | classic | `x86_64-linux/DateTime.pm:318:36` | all: offset	DateTime; _offset_for_local_datetime	DateTime | gold | _handle_offset_modifier; offset; _offset_for_local_datetime |
| completion-distzilla-self-moose-typed | tricky | moose | `Dist/Zilla.pm:109:23` | all: chrome	Dist::Zilla; name	Dist::Zilla; version	Dist::Zilla; abstract	Dist::Zilla; license	Dist::Zilla; authors	Dist::Zilla; plugins	Dist::Zilla; distmeta	Dist::Zilla; main_module	Dist::Zilla | gold | chrome; name → DistName; version → LaxVersionStr; abstract → String; main_module → Dist::Zilla::Role::File; license → License; authors; plugins → ArrayRef[...]; distmeta → HashRef |
| completion-typetiny-imported-blessed | tricky | exporter | `Type/Tiny.pm:165:15` | all: blessed	 | xfail | _croak; _swap; (anon); _install_overloads; new; DESTROY [140 local subs; 'blessed' absent] |

## Dropped (non-lib, absent from installed tree)

- completion-jsonpp-arrayidx-invocant — old file lib/JSON/PP.pm. JSON::PP is a Perl core module and is NOT installed in the cpm substrate (only JSON::MaybeXS.pm present; no JSON/PP.pm anywhere under local/lib/perl5). File absent → cannot port the $_[0]-> invocant-method completion row.
- completion-jsonpp-constants-bareword — old file lib/JSON/PP.pm. Same absence: JSON::PP not installed, so the use-constant P_* bareword completion has no source file in the installed tree.
- completion-jsonpp-decode-... (any further JSON-PP rows) — n/a: only the two JSON-PP rows above existed in completion.md; both dropped for the same JSON::PP-not-installed reason.
