# QA findings — open worklist

Two real-world QA sweeps: (1) crm + Dancer2/metacpan-web/Moose; (2) **17 local Perl projects**
under `~/personal` (DBIx-Class family, SQL-Abstract, Mojolicious, Bugzilla, SpamAssassin,
perltidy, sql-translator, modern Mojo/Moo libs). This file tracks only what's **open**.

## ✅ Closed (landed in PR #45)
P0.1 moo phantom accessors · P1.2/GATE-5 `export_ok` bare-`use` (diagnostic + resolution) ·
P1.3 catalyst `param_types` over-application · P1.4 cross-file `$c` multi-hop ·
P2.8 dancer DSL keywords · `does`/`lazy_build`/`accessor` keyword · completion-path `$c` ·
**cold-start 9 min → ~8 s** (witness-bag exponential memoized) + review-flagged perf cleanups.

## ✅ Second sweep — stability/perf PASS
**Zero crashes/panics across all 17 projects** (incl. 5k-line files, dense DBIC, multi-package files).
**Cold-start fix confirmed on SQL::Abstract.pm itself (511s → 1.4s)** and everywhere (cold = seconds,
warm = sub-second). One non-witness-bag perf outlier: perltidy `Formatter.pm` outline 1.5s, caused by
509 parse-ERROR nodes (grammar gap — see G2).

---

## A. Classic-Perl false positives — framework-agnostic, highest volume
The second sweep's headline: the biggest FP sources are plain-Perl idioms, not framework gaps.

- **A1 — bare-word filehandles flagged as functions** *(9 projects; 458 FPs in perltidy alone, ~the #1 recurring FP)*. `print STDERR`/`STDOUT`/`DATA`/`RUN ...`, `STDOUT->autoflush`, `-t STDIN`. `print FH LIST` (indirect-object) parses the filehandle as a call. Fix: builtin filehandles + indirect-object filehandle handling. **High value, contained.**
- **A2 — `my $x = shift` not typed** *(sql-translator ~500 FPs; also Bugzilla/perltidy)*. Every method call on a `shift`-extracted invocant flags. We type `my ($self) = @_` but not the `shift` form. **Single biggest single-pattern FP source.**
- **A3 — `use constant` not registered** *(5+ projects)*. File-scope constants (`use constant NAME => val` scalar form, between-subs, AND `{ … }` block form — buggy across forms/contexts) flag at every callsite. SpamAssassin/SQL-Abstract/io-socket-ssl/Mojo/perltidy.
- **A4 — hash-extracted invocant loses type** *(SpamAssassin/perltidy 44/Mojo)*. `my $x = $self->{field}` → `HashRef`, so `$x->method` flags even when `_field` holds a typed object. Related to A2 + the untyped-boundary open problem.

## B. Exporter consumer-side semantics — validates "exporters are core's job"
Renames / bundles / re-exports are a massive FP source; this is the consumer half of the exporter decision (`exporters-core-vs-byo`).

- **B1 — re-exporting test frameworks not traced** *(Test::Most 253 FPs, Test::Spec 129, `use Test`)*. `use Test::Most` re-exports Test::More's `ok`/`is`/… → all flag "not imported." Ubiquitous in Perl testing.
- **B2 — tag/bundle imports** *(6+ projects)*. `use M qw(:tag)` / `:DEFAULT` / `:all` / `-V2` / `:log` not resolved (the **bundles** half). Includes P2.7 (Type::Library / ResultDDL `-V2` → 25 FPs/file).
- **B3 — import alias `-as` (renames)** — `use M foo => { -as => 'bar' }` not tracked (the **renames** half).
- **B4 — cross-file `@EXPORT` bare-`use` not suppressed at scale** *(Bugzilla ~899 FPs!)*. `use Bugzilla::Util;` should pull `@EXPORT`; every exported fn flags. **Surprising given GATE-5 — investigate** (possibly the H1 duplicate-package bug shadowing the exporter, or workspace-exporter resolution at scale).
- **B5 — imported-function CALL SITES don't resolve** *(Mojo/DBD::Pg/Bugzilla/perltidy)*. goto-def works from the `qw()` import list but not where the function is called; qualified `Pkg::func()` is one token with no ref.
- **B6 — warm-cache "exported by X" attribution lost** *(all projects)*. Cold reports `hint … 'weaken' is exported by Scalar::Util`; warm/cached downgrades to `info … not defined in this file` — the enrichment isn't persisted in the cache blob. Degrades auto-import quality on the common warm path. **Our own regression.**
- **B7 — `Exporter::Extensible` `export`/`exporter_*`** injected names not modeled (ResultDDL); **regex import args** `use Carp::Clan qw/^Foo/` treated as names (47 FPs, DBIC/schema-loader).

## C. DBIC / accessor codegen (was P2.5) — the dominant DBIC noise
- **C1 — `mk_group_accessors`/`mk_group_ro_accessors`/`mk_classdata`** (Class::Accessor::Grouped) not modeled *(DBIC 440 + schema-loader 541 FPs)*. Recognizable pattern → synthesize stub `Sub` symbols from the qw-list (like `has`/`add_columns`). **High value, contained.**
- **C2 — `is_dbic_class()` shallow parent check** (`builder.rs:7253-7260` checks DIRECT parents only) → 2-level DBIC inheritance (`Result → BaseResult → Core`) gets zero synthesis (~215 FPs). **One-function fix: walk ancestry via `package_parents`/`module_index`.** The P2.5 quick-win.
- DBIC inherited base-class API methods (`result_source`/`search`/…) — the remainder once C1/C2 land; some are cross-@INC (need indexed DBIC).

## D. Multi-hop classic inheritance
- **D1 — deep `use base`/`@ISA` method resolution flaky** *(Bugzilla ~304 FPs, schema-loader)*. Single-hop classic `@ISA` works (SpamAssassin: 244 refs); 3-hop chains don't resolve methods. Echoes the catalyst multi-hop `$c` shape (cross-file ancestry depth).

## E. Framework-specific (Mojo / Moo)
- **E1 — `use Mojo::Base -base` / `use AnyModule -base` doesn't register the parent in `parents[]`** *(Mojo + modern-libs)*. `tap`/`attr`/`new` (universal Mojo::Base methods) flag on every `-base` class. **High for our flagship.**
- **E2 — helper-sub `$c` param typing** *(Mojo ~75 FPs)*. `sub _helper($c, …)` types `$c` as the enclosing plugin class, not `Mojolicious::Controller` (cascades to cross-file refs misses).
- **E3 — `has 'name', is => 'ro', …` comma-form** not parsed as `has` → no accessors (Migration). Common Moo style.
- **E4 — `MooX::Options` `option` keyword** not treated like `has` (37 FPs); **`with 'Role'` required methods** unresolved cross-file (DH/Migration); **MooseX::Role::Parameterized** `parameter`/`role{}` opaque; multi-line `has => sub{…}` default return type missed.

## F. P2.6 — `requires` (Moo::Role / Role::Tiny) not in framework imports
37 crm FPs. Trivial: add `requires` to the role import set (`builder.rs:~3897`); handle `Role::Tiny`.

## G. Upstream grammar gaps (tree-sitter-perl — file like the `not` issue #230)
- **G1 — `$#_`** (last index of `@_`) → cascade parse error wrapping the file (Bugzilla `Chart.pm`); string literals bleed out as `unresolved-function 'SELECT'`.
- **G2 — top-level bare `{ … }` blocks** (perltidy non-indenting-brace idiom) → root ERROR wrapping the whole 39k-line file → `Perl::Tidy::Formatter` invisible (indexed as `main`), 31+ subs missing, `--dump-package` fails. High impact (whole file lost).
- (`not` operator already filed: tree-sitter-perl#230.)

## H. Minor / nav
- **H1 — duplicate-package resolution** — two files `package Foo;` → resolver picks the wrong one (Bugzilla `contrib/Bugzilla.pm` shadows root). Breaks the singleton's type inference.
- **H2 — block-scoped `package` reversion** — inner `{ package Inner; … }` doesn't revert to the outer package on block close (DBD::Pg → 0 subs for the outer).
- **H3 — goto-def on `use Module::Name` (with a flag arg)** lands on the next line (Mojo); **`require Bareword`** flagged as a call (DBD::Pg).
- **H4 — bless-constructor type** — `my $self = {…}; bless $self => $class` doesn't promote `HashRef`→ClassName (Bugzilla/schema-loader).
- minor: goto-def off-by-one (lands in sub body, not on `sub`); `} or next` parses `or` as a call; `\&{$expr}` glob deref parsed as a call; `<<''` heredoc SQL bleed.

## Reference — confirmed NOT bugs
- XS-defined methods (DBI::db/DBI::st — runtime `@ISA`, no Perl def) flag as unresolved — inherent, expected.
- `--dump-package` is a faithful mirror of the editor query path (no drift).
- Partial route `->to('#action')` (untyped `$conf->{root}` boundary), cross-file `ClassIsa` trigger — documented deferrals.
- Dynamic helper goto-def (`$app->helper('dotted.name')`), `monkey_patch`/`local *glob` installs — no static def by design.

## Diagnostic-noise note
Default `--check --severity warning` is clean (0 output) everywhere; noise lives in hint/info. Across the
corpus the dominant FP clusters are **A (classic-Perl: filehandles, `shift`, `use constant`)**, **B
(exporter consumer-semantics)**, and **C (DBIC codegen)** — all framework-agnostic. Clearing A+B+C is what
would make the hint channel trustworthy enough to surface at warning level (the real VS Code release gate).
