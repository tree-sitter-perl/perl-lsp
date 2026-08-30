# PHP as the next language target — the scouting brief

Market research (2026-08-30) + the architecture mapping that made PHP the
pick over Ruby / Python / Lua / Elixir / R / Bash. The `--features php`
pack skeleton landed with this brief; everything below "The build-out"
is forward work.

## The market gap, verified

PHP is the rare case of a top-five-by-deployment language whose de-facto
LSP standard is one open-source editors *wish didn't exist*:

- **Intelephense** is closed-source freemium: the EULA forbids
  modification, and rename, go-to-implementation, and code actions sit
  behind the paid tier, with upgrade nags on every editor restart
  (vscode-intelephense#631). The Helix community keeps an open thread
  about recommending a proprietary LSP without disclosure (helix#8518).
  A de-facto standard that the FLOSS ecosystem actively resents is a
  standing invitation.
- **Phpactor**, the FLOSS alternative, is written in PHP itself and
  carries the architecture ceiling that implies: documented indexing
  memory leaks (phpactor#2377), 100%-CPU episodes (phpactor#2538), weak
  Windows support. Community practice is to run Intelephense *and*
  phpactor together — Intelephense for speed, phpactor only for
  refactorings. **Serenata** (the "libre competitor" hopeful) is dormant.
- **Nobody credible is coming.** The one Rust+tree-sitter PHP LSP found
  (hightemp/php-lsp) is a zero-star single-maintainer project. Laravel's
  official VS Code extension does Blade/route/config sugar and leans on
  Intelephense for the PHP itself — the framework vendors decorate the
  incumbent rather than replace it.
- **The workaround industry is the demand signal.** `laravel-ide-helper`
  exists to *generate fake PHP files* so Intelephense can see through
  Eloquent's magic. A server whose plugin layer understands the
  framework natively deletes that whole category.

Rejected rivals, briefly: Ruby (Shopify's ruby-lsp won the
consolidation; Zed and others migrated), Python (a Rust type-checker
arms race is in progress — pyright/basedpyright, Astral's ty, Meta's
pyrefly; do not enter), Lua (lua-language-server is excellent), Elixir
(consolidating into an official LSP), R (real gap, but Posit's Ark is
filling it with corporate backing and the market is small — the `r`
skeleton stays a skeleton), Bash (mediocre incumbent but a shallow
ceiling: no types, no frameworks, nothing for the witness bag to do),
Tcl/Raku (no incumbent, no market).

## Why PHP fits THIS engine

PHP reads like Perl with the sigils sanded down, and the fit is
structural, not cosmetic:

- **String-keyed arrays are PHP's blood type.** The hash-key machinery
  (`HashKeyAccess`/`HashKeyDef`/`HashKeyOwner`, structural shapes, key
  completion) has no equivalent in any PHP LSP — array-shape awareness
  exists only as PHPStan docblock annotations. `$config['timeout']`
  provenance is the same trick that already works for Perl.
- **The `.rhai` plugin system is the differentiator.** Eloquent's
  magic accessors/relations are a direct analog of the DBIC plugin;
  WordPress hooks and Symfony containers fit the same emit-hook shape.
- **Operator orientation survives.** PHP kept Perl's split between
  string concatenation (`.`, `.=`) and arithmetic — usage sites leak
  operand types the way Perl's `eq`/`==` split does. The pack's `@obs`
  arms are live from day one; Python has no such arms to write.
- **Gradual typing seeds the bag.** Type hints since PHP 7 are direct
  `annot_type` witnesses; the witness bag covers the huge untyped
  legacy tier (WordPress-era code) that Intelephense's free tier serves
  worst. `$this` rides the existing conventional-invocant lane
  ("this" is already in `conventions.rs`); `User::method()` dispatches
  as a bareword invocant exactly like Perl's `User->method`.
- **The mechanics are pre-paid.** tree-sitter-php is an official
  tree-sitter-org grammar with a deliberate `php` (HTML-mixed) /
  `php_only` split; composer's PSR-4 is a `SearchPath` variant of
  `VisibilityAxis` (cleaner than `@INC` — the map is declared in
  composer.json instead of computed at runtime).

## Honest risks

Intelephense's free tier is genuinely fast and good — this is a harder
fight than Perl (where there was nothing) or C++ (where clangd has
known structural weaknesses). The bar: match its speed, make
rename/references/implementations free, and beat it on framework magic
plus array-key provenance. Modern PHP's type vocabulary lives in
PHPStan/Psalm docblocks (`@template`, array shapes, generics) — parity
there is a long road; `docs/adr/parametric-types.md` is the seam.
Blade/Twig embedded templating is real extra work (also paywalled in
Intelephense — an opportunity, but not v1).

## Dogfood round 1 (landed on top of the spike)

Two probe agents over monolog/guzzle/WordPress/laravel-framework
(`docs/hitlist-php-round1.md`): zero crashes, zero misparses; every
finding was resolution-side and seven of nine rows landed same-round —
cross-file visibility (the `PackVisibility` routing fact), structural
ctor typing (`@expr.ctor` → TypeName edge), sigil-less property fields,
the pack `type_display` vocabulary, trait/qualified parents,
duplicate-def honest families with arity ranking, constants in outline.
Measured on WordPress after: `esc_attr` references 7 → 1345,
`have_posts` 0 → 23, `Logger::addRecord` grep-exact.

## What landed with this brief

`--features php` (in `all-langs`): grammar dep, `queries/php/skeleton.scm`,
`php_pack()`, `php_driver()` (Alpha, `.php`/`.phtml`), and the pack test
block in `query_extract_tests.rs`. Working end-to-end through the
production engine, zero engine special-cases:

- outline (namespace/class/interface/trait/enum/method/field/const/var),
  enum cases as `Enumerator`s typed by their enum
- `extends` / `implements` / trait-`use` all as `@parent` edges (PHP
  trait flattening ≈ role composition ≈ ancestor walk)
- typed params/properties/returns as witnesses; `$u = new User()` via
  call-site→Class resolution; `$n = $u->name()` through the generic
  MCB→bag bridge (the skeleton now derives `method_call_bindings` from
  flow×member-ref joins — a lane every pack language inherits)
- `instanceof` narrowing with the flow-edge cutoff; `.`/`.=` string and
  arithmetic numeric observations
- keyed array literals as `HashWithKeys` (the array-shape lane)
- cross-file function refs through the production `refs_to`

## The build-out (sequenced like cpp's arc)

1. **Composer visibility.** Parse composer.json (`autoload.psr-4`,
   `autoload.files`) into a `SearchPath`-flavored `VisibilityAxis`
   variant; `vendor/` is the DEPENDENCY tier (role-masked, like `@INC`).
   Until then `module_paths` carries the namespace-mirrors-directories
   guess.
2. **FQ identity.** The pack keys classes by unqualified leaf (cpp
   parity). PHP namespaces + `use` aliasing need the qualified name on
   the symbol with leaf-keyed dispatch — decide when composer roots land.
3. **Stdlib tier.** phpstorm-stubs (Apache-2.0) is the builtin surface —
   consumable the way `builtins.pod` feeds the Perl BUILTIN tier.
4. **Receiver-substituting returns.** `static`/`self`/`$this` returns
   are `ReturnExpr::Receiver` (the reducer exists; the pack needs a
   rettype path that mints it) — Laravel's fluent everything depends on
   this.
5. **Docblocks.** `@param`/`@return`/`@var` as witnesses (tree-sitter
   comment reparse or a light scanner); then PHPStan array-shapes →
   `HashWithKeys`, `@template` → the parametric seam.
6. **Framework plugins.** Laravel first (Eloquent accessors/relations,
   facades — the DBIC playbook), WordPress hooks second. Needs the
   capture-event rhai hook design from `docs/prompt-multi-language.md`'s
   open round.
7. **Calibration.** The gold-corpus sibling: a packagist-pinned substrate
   (top-N packages via composer), the same exact-assertion fixture
   format, corpus entries for a Laravel app + WordPress core in the
   `bench/` stack. Ship gate, budgeted as half the work.

Known residuals (deliberate, v1): `self::`/`parent::`/`static::`
receiver substitution; `require`/`include` path imports; class-constant
access (`User::VERSION` as a scoped ref); heredoc/encapsed interpolation
refs exist but interpolated member completion doesn't; `list()`/array
destructuring; global functions are namespace-blind.

**Engine residual (all packs, not PHP-specific): the registry has no
member-chain lane.** `$x = $a->b()->c();` leaves `$x` untyped — the
Variable's flow edge lands on `Expr(chain span)`, which carries no
witness, and only `expr_type_at_span`'s ref-reading member arm (which
the reducer registry cannot reach) can resolve it. Single hops type via
the MCB bridge; C++ has the identical gap for `auto x = w.get().spin()`.
Candidate shape: a method-hop `ProjectionStep` on the `Projected`
payload (receiver attachment + member + arity, materialized through the
receiver's dispatch class), minted per hop at extract time — that keeps
the chase in the registry and the refs out of it.
