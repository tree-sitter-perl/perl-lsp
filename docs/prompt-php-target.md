# PHP as the next language target — the scouting brief

Market research (2026-08-30) + the architecture mapping that made PHP the
pick over Ruby / Python / Lua / Elixir / R / Bash. The `--features php`
pack skeleton landed with this brief; "What's still open" under "The
build-out" and "Known residuals" are the live forward work.

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

## Dogfood round 1

Two probe agents over monolog/guzzle/WordPress/laravel-framework: zero
crashes, zero misparses across ~90 probes (PHP 8.1 syntax included); every
finding was resolution-side (cross-file visibility, `new X()` structural
typing, sigil-less property fields, duplicate-def honest families) and
landed same-round.

## The Laravel app corpus

BookStack (`github.com/BookStackApp/BookStack`, ~520 app files) is the
real-app corpus entry: clone it beside the other php corpora, then
materialize `vendor/` (gitignored, and packagist dists are
proxy-blocked in the sandbox) by copying the laravel/framework clone's
`src/` to `vendor/laravel/framework/src/` and writing
`vendor/composer/installed.json` with one `{"name":
"laravel/framework", "install-path": "../laravel/framework"}` package
row — the dependency-roots tier reads exactly that. Cold index ~8s;
verifies the vendor tier (gd on `$this->hasMany` lands inside
vendor/), facades, and relation properties against real code.

## Dogfood round 2 (verification + the fresh-verb sweep)

Two fresh agents re-probed every round-1 fix (all held) and probed the
untested verbs (rename, semantic-tokens, call-hierarchy). Fixed
same-round: `self::`/`static::` dispatch (canonicalizes to the
current-package invocant token) and foreach loop-variable declarations
(the `"as" .` anchor binds `$item`/`$k => $v`/`&$ref` without
re-declaring the iterated source) — refs/hover/highlight/rename on loop
vars all light up. Open residuals it ledgered: `toArray()`
decl-vs-trait-impl ranking; semantic-tokens wants absolute paths. (The
short-name-collision / interface-implementer finding closed under the
build-out's FQ-identity slice, below; foreach ELEMENT typing is the
sequence-types engine residual, tracked in `prompt-sequence-types.md`.)

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

Landed: composer visibility (`LanguageDriver::dependency_roots` reads
`composer.json` + `vendor/composer/installed.json`, attributing vendor
code to the DEPENDENCY tier so rename's EDITABLE mask refuses to rewrite
it; `autoload.psr-4` dirs are covered via the name-keyed workspace walk
instead); receiver-substituting returns (`static`/`$this`/`self` publish
`ReturnExpr::Receiver`, declared and docblock spellings alike);
`@param`/`@return`/`@var` docblocks (declared types win; generics
stripped, `X|null` collapsed); Laravel's first framework-plugin tier
(facades via the generic `@method` phpdoc lane — not Laravel-specific,
any `__call`-documented library gets it; Eloquent relations as the first
tenant of the query-overlay lane, `docs/prompt-pack-plugins.md`); and the
registry member-chain lane (`$a->b()->c()` types across hops via
`Expr(span) → Projected{base, MethodHop}`, shared with cpp).

What's still open:

1. **FQ identity residual.** Inheritance edges resolve through
   namespace-validated leaf identity (`parent_namespaces`, the
   `implementations_of` three-outcome BFS) — Laravel's three same-leaf
   `Repository`s no longer conflate. `refs_to`/rename stay leaf-keyed
   (over-approximate, never wrong-file for gd since goto-def ranks);
   full FQ symbol identity waits for a real need.
2. **Stdlib tier.** phpstorm-stubs (Apache-2.0) as the builtin surface —
   consumable the way `builtins.pod` feeds the Perl BUILTIN tier. Not
   started.
3. **Docblock residuals.** PHPStan array-shapes → `HashWithKeys`,
   `@template` → the parametric seam, and rendering the doc PROSE on
   hover.
4. **Framework-plugin tier 2** waits for a tenant needing name surgery
   (Laravel scopes) — tracked in `docs/prompt-pack-plugins.md`.
5. **Calibration.** The gold-corpus sibling: a packagist-pinned substrate
   (top-N packages via composer), the same exact-assertion fixture
   format, corpus entries for a Laravel app + WordPress core in the
   `bench/` stack. Ship gate, budgeted as half the work. Not started —
   no PHP fixtures exist in `gold-corpus/` yet.

Known residuals: `require`/`include` path imports; a class const's VALUE
stays untyped (typing it as the class would be wrong — thread the value
span to fix; a true enum case's value already types as its enum through
the same hop lane); heredoc/encapsed interpolation refs exist but
interpolated member completion doesn't; `list()`/array destructuring;
global functions are namespace-blind. **Array-element flow
through `foreach`** (`@var list<HandlerInterface>` on `$this->handlers`
doesn't type `$handler` in `foreach ($this->handlers as $handler)`) is
the engine's declared sequence-types residual — `Extraction::Rebind`'s
own doc marks the foreach element as undetermined for Perl too; the lane
is `docs/prompt-sequence-types.md`, and when it lands, `phpdoc_type`
should map `X[]`/`list<X>` to the parametric array-of-X instead of bare
`array`.
