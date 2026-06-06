# diagnostics

Generated from `fixtures/diagnostics.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl diagnostics`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| diag-07 | simple | classic | `x86_64-linux/Net/SSLeay.pm:24:8` | all: [] / none: "file":"SSLeay.pm","line":24; unresolved-function | gold | [] |
| diag-08 | tricky | codegen | `x86_64-linux/Net/SSLeay.pm:1023:1` | all: "code":"unresolved-function"; "file":"SSLeay.pm","line":1023; bootstrap | xfail | [] |
| diag-09 | tricky | codegen | `Log/Log4perl/Logger.pm:879:6` | all: [] / none: "file":"Logger.pm","line":879; is_warn | gold | [] |
| diag-10 | tricky | codegen | `Log/Log4perl/Logger.pm:883:4` | all: [] / none: "file":"Logger.pm","line":883; 'warn' is not defined | gold | [] |
| diag-13 | simple | codegen | `CGI.pm:1205:4` | all: [] / none: "file":"CGI.pm","line":1205; "file":"CGI.pm","line":299 | gold | [] |

## Dropped (non-lib, absent from installed tree)

- diag-01 — JSON/PP.pm is not installed in the cpm tree (only JSON/MaybeXS.pm present); the `require B` row cannot be reproduced.
- diag-02 — JSON/PP.pm absent (only JSON/MaybeXS.pm); `my $class = shift` row not portable.
- diag-03 — JSON/PP.pm absent; the `use constant P_INDENT` hash-slot row not portable.
- diag-04 — JSON/PP.pm absent; the eval-string codegen `indent` unresolved-method TP not portable.
- diag-05 — Time/HiRes.pm is not installed (only Time/Zone.pm present); the XS `constant` row not portable. (Net/SSLeay has an analogous XS constant() at 1002, but it's a different dist/entity, so not substituted.)
- diag-06 — Time/HiRes.pm absent; the own-export `gettimeofday` FP row not portable.
- diag-11 — source cursor was t/no_tabindex.t, a .t test file under the CGI dist, absent from the installed lib tree (only CGI.pm is installed); not portable.
- diag-12 — source cursor was t/query_string.t, a .t test file absent from the installed lib tree; not portable.
- diag-14 — source cursor was xt/900_pod.t, an author/extended-test file absent from the installed lib tree; not portable.
