# PHP round-6 hitlist (dogfood 2026-09-02: composer, Slim, BookStack + real vendor, symfony/demo)

Raw findings: scratchpad `round6-findings-A.md` (composer/Slim) and
`round6-findings-B.md` (BookStack/demo). Every reference count below was
grep-checked by the agent. Round-5 fixes HELD on re-probe: same-leaf
`Request`/`Collection` disambiguation in both directions (app + vendor),
property-vs-method separation (`User::$permissions` / `permissions()`,
`Svn::$hasAuth` for hover/refs/rename), class-declaration references
(use rows, hints, `new`, `::class`, static receivers — 15/15 on
`HtmlErrorRenderer`), `Book::query()` through vendor + inheritance,
Doctrine `@method` rows, `use X as Alias` and union types dark-not-wrong.

## LANDED (2026-09-02)
- R6-1: `use_aliases` persisted; an aliased row pins the alias spelling,
  never the real leaf; a `use` row's leaf references only the class its
  namespace names (per-row verdict in the matcher, so a file whose own
  `Event` is another class still contributes its `use A\Event as
  BaseEvent` row to `A\Event`'s references/rename). RESIDUAL: gd on a
  member of the bare-spelled class (`$e->name()` with `$e = new Event()`)
  still lists the same-leaf stranger's method too — the decl→def fan-out
  lane is leaf-keyed; references/rename are exact.
- R6-2: `qualified_spellings` persisted (the written prefix of call /
  ctor / type / parent spellings); the leaf pins to `own_ns\prefix`, or
  to the absolute prefix after a leading `\`. The explicit speller
  `leaf_namespace` now reads the same pins (own-namespace and qualified
  claims included), so every class-keyed family filter agrees with the
  axis.
- R6-3: the build-time method-call stamp (`stamp_method_targets`) froze
  the first same-named symbol; it now walks with the written shape, so
  a same-file `$this->hasAuth()` goes to the method and `$this->hasAuth`
  to the property.
- R6-4: the `[$obj, 'method']` / `[Class::class, 'method']` callable
  patterns matched a keyed two-pair array (`['chapter' => $c, 'book' =>
  $c->book]` — each pair CONTAINS a variable / a string); the elements
  must now be bare. WordPress's overlay carried the same shape.
- R6-5: the Eloquent relation overlay matched only a bare
  `return $this->belongsTo(X::class)`; BookStack's `->withTrashed()`
  modifier dropped the property, so the chain typed as `BelongsTo`. One
  chained modifier is accepted (to-one and to-many). Deeper chains and
  the `@return BelongsTo<Book, $this>` generic (which would type the
  property from the docblock alone) stay open.
- R6-1 residual re-read under the goto-def policy (surface every relevant
  candidate): `$e->name()` with `$e = new Event()` listing `B\Event::name`
  too is the override family (B's `Event` extends A's) — by policy, not
  a bug.

- R6-6: `'A\\B\\X::method'` string callables are Callable member refs on
  `X` (the class part a qualified spelling, pinned like any other), so
  the method's references and rename reach them (composer's
  `EventDispatcherTest::someMethod` site).
- R6-7: `new self(...)` / `new static(...)` are Callable member refs of
  the pack's constructor on the current-package token — the ctor's
  references, fan-in, hover and goto-def all see the site (composer
  `ProxyManager::__construct` 1 → 2), and a class rename never touches
  the `self` token. A constructor-convention name (`__construct`) is not
  renameable at all (its `new self` sites carry no token spelling it).
- R6-8: word-keyed goto-def fallbacks stand down inside an import row, so
  `Http` in `use Illuminate\Http\Request;` no longer jumps to
  `Support\Facades\Http`.
- R6-11: bundled overlays are compiled ALONE and dropped with a stderr
  diagnostic (the same posture as plugin-dir overlays); a unit test
  compiles every bundled php document as a tripwire.

## CRITICAL

### R6-11 — one broken bundled overlay takes the whole php query dark (LANDED, see above)
Found while editing `laravel.scm`: the pack's bundled `.scm` documents
concatenate into ONE tree-sitter query, so a syntax error in any of them
fails the compile and every php verb answers nothing (no error surfaced
by the verb; `--plugin-check <file>` finds it). Plugin-dir overlays are
dropped individually; bundled ones are not. Fix: compile each bundled
document separately first (drop + stderr diagnostic, like plugin-dir
overlays), or a build-time test that lints every bundled overlay.

## MAJOR

### R6-6 — forward-only string callables
A: `'Composer\Test\...::method'` string callables resolve forward (gd/hover
reach the method) but the method's references never list the site — the
backward matcher has no arm for the FQCN-string shape. 3/15 heatmap false
positives in the composer sample; `composer.json` script hooks use it.

### R6-7 — `new self(...)` dark
A: `ForgejoUrl::__construct` reached only via `new self(...)`: hover/gd on
the `self` token dark, ctor fan-in 0. The ctor `expr.ctor` arm maps
`self`/`static` to the enclosing class for TYPING; the ctor REF still
carries the literal `self`.

### R6-8 — middle segment of a `use` FQN hovers to an unrelated class
B: `use Illuminate\Http\Request;` — hover/gd on `Http` jump to
`Support\Facades\Http`. No ref is minted for middle segments, so the
cursor falls to a bare-word lookup. Rename correctly refuses.

## MINOR / design

### R6-9 — DI constructors in the dead queue (LANDED)
B: symfony/demo: 15/42 dead candidates were `__construct` with fan-in 0
(container-injected). Now a pack constructor whose CLASS has a reference
row anywhere (a type hint, `Foo::class`, a `use` row) carries the
`class-referenced` guard — a container or factory instantiates it. Reads
the row store (an over-approximation, the sound side); without rows the
constructor is judged by its `new` sites alone, as before. A class named
nowhere keeps its constructor in the queue. symfony/demo (2026-09-02):
dead 42 → 36, dead constructors 15 → 9; the nine left are classes no
indexed PHP names (wired by config/autowiring) — the out-of-tree-callers
fork (`docs/open-forks.md`, dead-code queue vs library public API).

### R6-10 — union darkness → heatmap false positives
A: `array<SecurityAdvisory|PartialSecurityAdvisory>` element access is
honestly dark, so `toIgnoredAdvisory`'s only caller is invisible and the
method is flagged dead. Same fork as union types.

### Heatmap sample (composer root, 243 candidates, 15 sampled)
6 tool false positives (R6-2, R6-6, R6-7, R6-10, the alias gap), 2 truly
dead, 2 framework-invoked, 4 inconclusive library/plugin API surface, 1
left unclassified in the agent's table.
