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

## LANDED at close (2026-09-02, one slice)
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

## CRITICAL

### R6-1 — an aliased import's FQ row pins the real leaf
A: `Composer\EventDispatcher\EventDispatcher.php` (namespace
`Composer\EventDispatcher`, `use Composer\Script\Event as ScriptEvent;`)
spells the bare `Event` 11 times meaning its OWN namespace's class. The
use-map reads the FQ row `Composer\Script\Event` and pins leaf `Event` →
`Composer\Script`, so gd/hover on `new Event(...)` land on `Script\Event`,
the class's references miss all 11 sites (24 found / 33 real) and admit the
stranger's declaration, and a rename would rewrite the wrong class. The
alias spelling is not persisted (`include_directives` carries only the FQ
text). Fix: persist `(alias, namespace, leaf)` from the `@use.alias`
captures; an aliased row pins the ALIAS spelling, never the real leaf.
Also: refs on a `use A\Event as BaseEvent;` row inside a file whose own
`Event` is another class are legitimate references to `A\Event` — the
per-file gate must admit import-row refs by their own FQ namespace.

### R6-2 — a namespace-relative qualified spelling counts as a bare one
A: `Factory.php` (namespace `Composer`): `new Downloader\DownloadManager(...)`
— the ctor ref is named by its leaf, the use-map counts `DownloadManager`
as spelled bare → own-namespace claim `Composer` ≠ `Composer\Downloader`
→ the references walk skips the file (1/5 `setSourceFallback` sites),
heatmap false positives follow. Fix: capture the written qualifier on
call/ctor/type refs and pin the leaf to `own_ns\prefix` (or the absolute
prefix when written with a leading `\`).

### R6-3 — gd on a CALL lands on the sibling property (hover/refs/rename right)
A: `Composer\Util\Svn`: `$this->hasAuth()` at 161/242 → gd lands on
`$hasAuth` (35:16) not `hasAuth()` (283:31). The ancestor walk takes the
shape (`resolve_member_in_ancestors`), so some earlier goto-def lane
answers first name-keyed — find which (the frozen-edge arm, the
`member_def_location` class-keyed BFS, or the parametric-receiver ladder)
and thread `MemberShape` through it.

### R6-4 — an array-literal string key reads as a method reference
B: `ChapterController.php:186` `'book' => $chapter->book,` inside a
`view(...)` array: hover/gd on the KEY `'book'` answer `BookChild::book()`
and a rename of `book()` rewrites the literal key. Only one of seven
same-shaped sites in the file; the difference is key ORDER (`'chapter'`
before `'book'`). Suspect the two-element instance-array-callable pattern
(`[$obj, 'm']`, anchored `.`) matching a sub-sequence of a longer
`array_creation_expression` when the preceding element is a bare-variable
pair — reproduce on a minimal fixture first.

### R6-5 — a chain through an INHERITED relation property is dark
B: `$chapter->book->getUrl()`, `$page->book->defaultTemplate()`,
`$entity->book->getDirectVisibleChildren()` — hover AND gd dark, while
`$entity->chapter->getVisiblePages()` (relation declared on the class
itself) resolves. One-hop `$chapter->book` hovers fine; the return-type
edge for the NEXT hop does not survive the inheritance walk when `book()`
lives on the abstract parent (`BookChild`). Everyday Laravel idiom.

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

### R6-9 — DI constructors in the dead queue
B: symfony/demo: 15/42 dead candidates are `__construct` with fan-in 0
(container-injected). Framework-agnostic idiom; a "constructor of a class
referenced anywhere (`Foo::class`, a hint, a config) is live" guard, or a
library mode (`docs/open-forks.md`).

### R6-10 — union darkness → heatmap false positives
A: `array<SecurityAdvisory|PartialSecurityAdvisory>` element access is
honestly dark, so `toIgnoredAdvisory`'s only caller is invisible and the
method is flagged dead. Same fork as union types.

### Heatmap sample (composer root, 243 candidates, 15 sampled)
6 tool false positives (R6-2, R6-6, R6-7, R6-10, the alias gap), 2 truly
dead, 2 framework-invoked, 4 inconclusive library/plugin API surface.
