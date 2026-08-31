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

Two fresh agents re-probed every round-1 fix (all hold — several now
grep-exact: `$handlers` 11/11, `addRecord` 16/16, `ClientInterface::
request` 61 with all 52 test-side call sites) and probed the untested
verbs. Rename passes all three shapes with exact blast radius (method
rename excludes unrelated same-named interface methods; local rename
respects closure shadowing; property rename 5/5). Semantic-tokens and
call-hierarchy (187 incoming) serve PHP. Fixed same-round:
`self::`/`static::` dispatch (canonicalizes to the current-package
invocant token) and foreach loop-variable declarations (the `"as" .`
anchor binds `$item`/`$k => $v`/`&$ref` without re-declaring the
iterated source) — refs/hover/highlight/rename on loop vars all light
up. Ledgered with evidence: `--implementations` misses the direct
interface implementer and short-name collisions pollute
type-hierarchy/references (`Repository` × 3 namespaces — CLOSED by
build-out item 2's FQ-identity slice); foreach ELEMENT typing is the sequence-types
engine residual; `toArray()` decl-vs-trait-impl ranking; semantic-tokens
wants absolute paths.

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

1. **Composer visibility.** LANDED — `LanguageDriver::dependency_roots`
   (the pack's analog of `@INC`): the php driver reads `composer.json`
   (the gate) + `vendor/composer/installed.json` install paths, the
   bulk indexer walks those roots ignore-rules-off (vendor/ is
   gitignored by design), and `is_dependency_path` attributes each
   candidate to the DEPENDENCY tier per path — so gd/references reach
   vendor code while rename's EDITABLE mask (OPEN|WORKSPACE) refuses to
   rewrite it. `autoload.psr-4` dirs remain unread — the name-keyed
   candidate relation covers them via the workspace walk.
2. **FQ identity.** LANDED for the inheritance-edge axis — symbols stay
   leaf-keyed (cpp parity, the engine's identity), with namespace
   claims layered on: the extract-time use-map (alias/group aware)
   resolves a parent's WRITTEN spelling to its real leaf + namespace
   (`use X\Y as Z` edges were dead under `Z`), each edge records
   `(child, parent leaf, parent ns)` in the `parent_namespaces` pack
   lane, and `implementations_of` validates leaf-keyed chain hops
   against those rows (three-outcome BFS: agreeing/unrecorded ns →
   keep, reached only through a recorded mismatch → prune, unreachable
   → keep) so Laravel's three same-leaf `Repository`s stop conflating.
   The aliased-contract idiom (`class Repository implements
   CacheContract`) resolves to a SELF-LOOP in leaf space — the direct
   implementer carries the contract's own leaf, which both arms'
   contract-side exclusions used to eat; the namespace rows re-admit
   it (a foreign-ns declaration recording the edge back to the
   contract's ns), which is exactly the round-2 pull/put probe fix
   (verified on laravel/framework: pull → Cache/Repository.php:228;
   put → :367 + RedisTaggedCache.php:52; the interface name → those
   plus TaggedCache, never the Config/Log strangers).
   Residual: `refs_to`/rename still leaf-keyed (over-approximate, never
   wrong-file for gd since goto-def ranks); full FQ symbol identity
   waits for a real need.
3. **Stdlib tier.** phpstorm-stubs (Apache-2.0) is the builtin surface —
   consumable the way `builtins.pod` feeds the Perl BUILTIN tier.
4. **Receiver-substituting returns.** LANDED — the `rettype_receiver`
   pack predicate publishes `ReturnExpr::Receiver` for
   `static`/`$this`/`self` (declared AND docblock spellings); fluent
   chains substitute through both the member arm and the MCB default
   receiver. (`self` = defining-class nuance is an accepted
   over-approximation for inherited methods.)
5. **Docblocks.** LANDED for `@param`/`@return`/`@var` (the `doc_types`
   pack predicate + positional join; declared types win; generics
   stripped, `X|null` collapsed). Still ahead: PHPStan array-shapes →
   `HashWithKeys`, `@template` → the parametric seam, and rendering the
   doc PROSE on hover.
6. **Framework plugins.** Laravel's first tier LANDED, in two pieces
   that deliberately need NO new hook machinery:
   - **Facades** ride a generic phpdoc lane: `@method [static] T
     name(args)` rows on a CLASS docblock synthesize real method
     symbols (each spanning its own `@method` line), so `Cache::get()`
     dispatches/types/completes through the normal scoped-call hop —
     verified on BookStack (gd on `Cache::get` lands on the vendor
     facade's `@method get` row). This also covers every library using
     `@method` (`__call` documentation) — not Laravel-specific.
   - **Eloquent relations** are the first tenant of the framework
     QUERY overlay: `queries/php/frameworks/laravel.scm`, concatenated
     into the pack's query, expressed entirely in the standard capture
     vocabulary + `#any-of?` text predicates (the engine carries no
     Laravel names — the doctrine holds). A relation method mints the
     same-named PROPERTY Eloquent's `__get` serves; to-one relations
     carry the related class (`$page->book->name` chains), to-many
     navigate untyped (Collection element typing = the generics
     residual). Verified on BookStack: gd on `$book->pages` lands on
     the `pages()` relation. The extract dedup now keys field-ness so
     the method+property pair at one name token survives.
   The rhai capture-event hook (a DYNAMICALLY loaded overlay + host
   predicates, `$PERL_LSP_PLUGIN_DIR`-style) remains the follow-on
   seam; the overlay file is exactly the artifact it would load.
   WordPress hooks (round-3 R8) are the second tenant and need emit
   actions richer than captures (a ref into arg #2's string), i.e. the
   real hook design.
7. **Calibration.** The gold-corpus sibling: a packagist-pinned substrate
   (top-N packages via composer), the same exact-assertion fixture
   format, corpus entries for a Laravel app + WordPress core in the
   `bench/` stack. Ship gate, budgeted as half the work.

Known residuals: ~~`parent::` dispatch~~ LANDED round 3 — the pack's
`super_receiver` predicate mints `parent::m()` as the model's SUPER
method token (`SUPER::m`, current-package invocant), so
gd/references/rename ride the existing SUPER lane (return TYPING of a
`parent::` call stays a residual — a hop would find the child
override; `self::`/`static::` landed round 2, canonicalizing to the
`__PACKAGE__` invocant token); `require`/`include` path imports;
~~class-constant access~~ LANDED — `User::VERSION` / `self::LIMIT` /
`Level::Debug` mint member-lane refs (gd/references/hover connect,
double-anchored patterns), a true enum case's value types as its enum
through the same hop lane (Enumerators joined the PackageSymbol
writeback-lite), while a class const's VALUE stays untyped (typing it
as the class would be wrong — thread the value span to fix);
heredoc/encapsed interpolation refs exist but interpolated member
completion doesn't; `list()`/array destructuring; global functions are
namespace-blind. **Array-element flow through `foreach`** (round-2
probe: `@var list<HandlerInterface>` on `$this->handlers` doesn't type
`$handler` in `foreach ($this->handlers as $handler)`) is the engine's
declared sequence-types residual — `Extraction::Rebind`'s own doc marks
the foreach element as undetermined for Perl too; the lane is
`docs/prompt-sequence-types.md`, and when it lands, `phpdoc_type` should
map `X[]`/`list<X>` to the parametric array-of-X instead of bare
`array`.

**The registry member-chain lane: LANDED** (was the top all-pack engine
residual). `$x = $a->b()->c();` / `auto x = w.get().spin();` now type:
each member-call site mints `Expr(whole call span) → Projected{base,
MethodHop{member, arity}}` (`@hop.call` in the php patterns; a dedicated
called-member pattern with `@hop.member` on the cpp side, whose field
ref pattern is call-blind), where the base is the receiver's `Variable`
(simple receiver), the enclosing class (`$this->`/`self::` via the
pack's `hop.recv` shaping — a companion `ClassName` witness on the
receiver span, minted where extraction has the class in hand), or its
`Expr` span — which, for a nested call, is exactly the inner call's own
hop witness. The registry materializes the
hop lazily: resolve the base, dispatch `member` on its class via
`PackageSymbol{class, member}` at the call's own arity, with the base
type passed as the dynamic receiver so `: static` fluent chains keep the
concrete class through every hop. The chase stays in the registry (refs
never enter it); Perl keeps its own build-time fold lane untouched.
