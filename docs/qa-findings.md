# QA findings — open worklist

Tracks what's **open**. Landed work is collapsed into the summary below (kept for the PR body).
QA corpus: ~45 real projects across eras/authors (`~/perl-qa-corpus` + earlier `~/personal` sweeps).

## ✅ Landed (PR #45 — sprint + 6 rounds + focused fixes, EXTRACT_VERSION 49)
**Zero crashes/panics across the entire corpus** throughout. Warning-severity channel is clean everywhere
(noise lived in hint/info, now largely cleared). Highlights:

- **Classic-Perl FPs:** bare-word filehandles (indirect-object), `use constant` (scalar; multi NAME-form),
  `my $x = shift` typing, `require Bareword`.
- **Frameworks/codegen:** `requires`/Role::Tiny, Class::Tiny, `use X -base`, `has` comma-form, DBIC ancestry
  walk, `mk_group_accessors`/`mk_classdata` (incl. statement-modifier `for`), typeglob codegen (`*name=sub`,
  `*$m=sub` over literal-returning locals, cross-package `*{'Pkg::'.$n}=…->can()`), AutoLoader/SelfLoader
  `__END__` subs, hashref-value typing.
- **NAV unification:** method-call refs carry a build-time resolved-target edge (`refs_to`/goto-def/hover
  single-sourced); precise **and** complete, validated 0% false-exclusion + 100% typed recall, **arbitrarily
  deep** chains; package-decl fallback dropped (honest miss); chained `->method->{key}` build-time owner.
- **Exporter producer surface:** `@EXPORT`/`@EXPORT_OK`/`%EXPORT_TAGS` (incl. `Readonly`-wrapped) + Sub::Exporter
  `-setup`/`setup_exporter` folded into one surface; export-member refs; tag-import goto-def.
- **Resolution:** qualified `Pkg::Bar::baz()` calls; imported-fn goto-def lands on the sub (not the `use`);
  `Class->method`→`package` ref; **multi-hop `@ISA` verified closed**; cross-`@INC` inheritance resolves when
  the parent is installed; incomplete-ISA chain → `unresolved-method` suppressed (honest).
- **Perf/infra:** cold-start 9 min → seconds (witness-bag memo); CLI `--references`/`--definition` position
  renderer fixed; `--timings`; committed e2e warmup; `ts-parser-perl` 1.0.1 → 1.0.3.
- **Parser handoff:** `docs/parser-shortcomings.md` G1–G7 + GR-1/GR-2 (+ G4 with a removable builder kludge).

---

## OPEN

### Exporter subsystem (Pillar 1) — the largest remaining FP lever
- **Consumer import-binding — IN FLIGHT.** bare `use M;`→`@EXPORT`, `:tag`/`:DEFAULT` expansion, `-as` rename,
  single-sourced for diagnostics+nav. Clears the dominant Bugzilla FP (1155 `unresolved-function`, e.g. bare
  `use Bugzilla::Util;`). Design: `qa-design-items.md` § B2/B3.
- **Deferred follow-ups:** re-export chains (Test::Most 253 / Test::Spec 129 — runtime `push @EXPORT =>
  @{"$m::EXPORT"}` idiom) · regex import args (`qw(/^foo/)`) / negation selectors (B7).

### Fat-comma audit — QUEUED (after exporter binding lands)
`=>` has no *code* semantics (it's a comma + bareword autoquote), so pair-walkers must pair **positionally**, not
match the `=>` node. Confirmed debt: `use constant { 'GAMMA', 3 }` (plain-comma block) registers nothing while
`{ A => 1 }` works. Audit/fix: use-constant block, `%EXPORT_TAGS` table, Sub::Exporter `exports`/`groups`,
export-member collector, the new exporter `imported_names`/`extract_as_renames`/`-as` parse, plus the
`for_each_fat_comma_pair`/`flatten_fat_comma` helpers. (Rule now in CLAUDE.md.)
- **Also rename** every helper/method whose name says `fat_comma` — the name describes the surface token, not
  the positional-pair semantics it implements (e.g. `for_each_fat_comma_pair` → `for_each_pair_in_list`,
  `flatten_fat_comma` → `flatten_pair_list`). `grep -rn fat_comma src/` for the set; rename in the same sweep.
- **Human-semantics note (future):** `=>` *does* carry human intent ("LHS is a key/label") — a legitimate
  **hint** we may lean on later (disambiguation / which-element-is-the-name), but only as a heuristic tie-breaker,
  never a hard gate. The correctness path stays separator-agnostic.

### Generic fully-qualified (FQ) symbol handling — DESIGN (cross-cutting cleanup)
We keep adding a per-construct qualifier-stripper: `Ref::unqualified_target_name()` for `Foo::Bar::baz()` calls
(R6), `Builder::export_var_basename` for `@Pkg::EXPORT`/`%Pkg::EXPORT_TAGS` globals (exporter round). Same
underlying fact each time: a name token may carry a `Pkg::` qualifier; resolution = `(qualifier ?? current_pkg,
basename)`. Generically un-handled today: FQ *variable* reads (`$Foo::Bar::x`, `@Pkg::arr`, `%Pkg::h`), `\&Pkg::sub`.
**Unify:** one `split_qualified(name) -> (Option<pkg>, basename)` that every symbol/ref consumer resolves through
(rule #10 — encode "is qualified" on the name/ref once), retiring the bespoke strippers. Deferred; documented here.

### Type inference — DESIGN (`qa-design-items.md`)
- **A4 — hash-extracted invocant** (Pillar 2). `my $x = $self->{field}; $x->method` → `HashRef`; also the
  `Bugzilla::Memcached` field-type case. Slot→type witnesses from observed writes; never "no evidence → HashRef".
- **NARROW-1** — `_INSTANCE($x,'Class')` / `ref $x eq 'Class'` narrowing guards (flow-sensitive, branch-scoped).
- **E2** — helper/callback `$c` typed by registration context (Mojo ~75).

### Inheritance / cross-file — DESIGN
- **MAIN-1** — `main::` aggregation across `require` of package-less scripts (legacy CGI, AWStats ~270).
- **@INC-dependency inheritance residue** — methods from *uninstalled* CPAN parents (TheSchwartz) resolve once
  installed; **DBI-XS** (runtime-typeglob-installed) and template-method (subclass-defined) cases are inherent.

### Minor / nav-quality
- unknown-receiver same-name goto-def fallback → prefer honest-miss (libwww). · **H1** duplicate-package
  resolution (path/role ranking; interacts with exporter @EXPORT). · **H2** block-scoped `package` reversion. ·
  **H4** bless-constructor type. · goto-def off-by-one. · **E4** MooX::Options `option` / `with` role-required /
  MooseX::Role::Parameterized.

### Forward / parked
- Exporter **recognition→plugin extraction** (the `ExportSurface`/`ExportDecl` seam; `exporters-core-vs-byo`).

## Grammar gaps — handed off (awaiting parser team)
`docs/parser-shortcomings.md`: G1 `$#_`-in-string · G2 top-level bare block · G3 `<<''` · G4 bareword filehandle
· G5/G6 (per doc) · G7 `"${@}"` block-interp bleed · GR-1 v-string · GR-2 bareword-`&&`. (`not` = ts-perl#230.)

## Reference — confirmed NOT bugs
XS-defined methods (DBI, PPI-on-untyped-param) · truly-dynamic `*{$runtime}=…` installs · methods from
not-installed dependencies · `--dump-package` faithfully mirrors the editor query path.
