# PHP dogfood round 4 — hitlist

Three fresh agents, ~91 grep-sanity-checked probes: WordPress+guzzle
(A), monolog+laravel/framework (B), BookStack+symfony/demo (C). Binary:
release cpp,php at be10a34 (post round-3 close + framework-entry +
visibility/foreach-keys arcs). Findings files:
`scratchpad/round4-findings-{A,B,C}.md` (session-local).

**Round-3 wins that HELD under fresh adversarial probing** (recorded so
the next round doesn't re-probe blind): hook-name/callback counting
grep-EXACT on WP (`init` 190/190, `wp_head` 69/69, `excerpt_more`
13/13, rename inside quotes byte-exact); same-name/same-value const
disambiguation exact (Multiplexing::NONE vs TransportSharing::NONE, 50
edits, zero cross-contamination); typed-receiver rename discrimination
strong (CurlMultiHandler::close vs 2 same-named classes across 113
sites; WP_Query::query vs 7 same-named classes across 188 sites);
route-arrays `[Ctrl::class,'m']` clean across 6 BookStack controllers
incl. multi-line; vendor never touched by rename; facades, prop chains,
static factory chains, plain `array<K,V>` foreach key+value typing,
protected-refs scoping, promoted-property access gd/hover, both
freeform traces resolved end-to-end.

The ledger, clustered by root cause, ranked:

## H1 (CRITICAL) — method rename fans across the whole interface family

B5.1: dry-rename `Fluent::toArray()` → 217 edits / 72 files, editing
MessageBag/Model/Enumerable/Request/Uri/Validator/… — their OWN
`toArray` decls and self-calls. All connect only through implementing
`Arrayable` (and friends). The Hierarchy override-family fan is
contract-theoretically defensible but catastrophic UX from a concrete
override on a mega-common interface method: 700 `->toArray(` sites in
the corpus, a third rewritten. Contrast: interface-method rename from
the INTERFACE decl (B5.3, `Job::getJobId`) fanned exactly right (8/8
decls, no leakage). Design question for the fix: initiating from a
CONCRETE override should scope to that class's subtree (+ its own call
sites), not lift to the contract root — lifting stays available from
the interface's own decl.

## H2 (CRITICAL) — trait-method call sites invisible through `use Trait` — LANDED (typed/chain receivers)

B5.2: references/rename on `EnumeratesValues::eachSpread()` (trait) →
2 decls only; 0 of 7 real `$collection->eachSpread(...)` sites through
`Collection`/`LazyCollection` (both `use EnumeratesValues`). The
matcher never admits a MethodCall whose invocant class reaches the
trait via the use-edge (downward direction). Mirror image of H1.

## H3 (CRITICAL) — gd picks a cross-namespace same-name class, ignoring the file's `use` — LANDED

B4.3: gd on `Cache::store('session')` in Session/Store.php (which
imports `Support\Facades\Cache`) lands on
`Container\Attributes\Cache::$store` — an unrelated promoted PROPERTY
on a never-imported class. Hover on the same token disambiguates
CORRECTLY (Repository), so the type side has the answer; gd's
candidate ranking doesn't consult the use-map. references() merges the
spurious decl in (201 = 199 calls + right decl + wrong decl).

## H4 (CRITICAL, collapses into H6) — promoted-property refs/rename undercount — LANDED (4/6 sites)

B1.1/1.3: `LogRecord::$level` refs/rename miss 6 real sites in 4
files. B root-caused every miss to the RECEIVER being untyped at the
site — each is one of H6's doc gaps (@inheritDoc, @phpstan-param,
static::$prop @var). No new mechanism; fixing H6 closes these and the
group machinery already handles the rest (49 sites correct).

## H5 — PHPUnit provider/test wiring (3 findings, one cluster) — LANDED

- C1 (CRITICAL): `#[DataProvider('providerMethod')]` attribute ARG is
  dark everywhere — gd/hover/refs/rename; a real rename silently
  breaks the suite. Needs a named-method ref whose receiver is the
  ENCLOSING class (a `self`-flavored named capture — the attribute arg
  has no receiver node).
- A3 (MAJOR): docblock `@dataProvider name` form (guzzle) — 192 of 194
  `*Provider` dead-queue rows are live provider targets (55% of the
  whole queue). Doc-fact lane: `@dataProvider` row → same
  named-method ref (or at least entry evidence).
- C2 (MAJOR): `test*` methods in classes extending INTERMEDIATE bases
  (WebTestCase/KernelTestCase — vendor absent, so the leaf-isa chain
  never reaches TestCase) aren't framework-entry. Pure entry.json
  data fix: add the common intermediate leaves (+ setUp lifecycle
  through them).

## H6 — doc-type coverage cluster (highest type-leverage per B) — LANDED

- B1.1b/1.1c/2.4 (MAJOR, systemic): `@inheritDoc` does not inherit the
  overridden method's `@param array<X>` element type — 13 monolog
  handleBatch overrides + every formatBatch blind; blanks BOTH foreach
  vars and cascades into H4's rename misses.
- B1.1a + A4 (MAJOR): `@phpstan-param` / `@phpstan-var` /
  `@phpstan-return` prefixes unread; `non-empty-array<X>` /
  `non-empty-list<X>` shapes unparsed.
- C5 + A-minor (MAJOR): inline `/** @var Type[] $localVar */` above an
  assignment doesn't type the local (property-level identical tag
  works) — the @var-on-assignment join ("var"-kind defs excluded from
  the doc join).
- B1.1d (MAJOR): `foreach (static::$prop as $x)` loses the prop's own
  `@var Type[]` while `$this->prop` works — the static-receiver
  spelling misses the field-registry hop.

## H7 — generic string-callables outside the hook idiom

A1 (MAJOR): `function_exists('name')` guard invisible → stale guard
post-rename. A2 (MAJOR): `array_map('fn', …)`,
`'sanitize_callback' => 'fn'`, `array('fn')` schema entries invisible
(6 real sanitize_title sites). Fix shape: a php-stdlib bundled overlay
(`#any-of?` over the callable-taking builtins: array_map, array_filter,
usort, call_user_func[_array], is_callable, function_exists, …) minting
`@ref.call.named` — the tier-1 vocabulary already does the rest.
`'sanitize_callback' => 'fn'` (arbitrary key positions) is harder —
park the key-position form unless a bounded pattern emerges.

## H8 — `parent::` resolves to an interface stub over the concrete parent — LANDED

B4.4/4.5 (MAJOR, ×2 methods): `parent::map()` from Eloquent\Collection
lands on `Enumerable.php` (interface abstract) instead of
`Collections/Collection.php:829` (concrete parent override). The SUPER
walk must prefer concrete class parents over implements-edges.

## H9 — Doctrine `@method` magic finders disconnected both ends

C3 (CRITICAL): `@method Post|null findOneByTitle(...)` — references on
the tag token answer []; gd/hover from the real call site dark. Two
sub-causes to verify: (a) the call-site receiver is untyped
(`getContainer()->get(X::class)` — container get is genuinely opaque;
partially H6/park), (b) references on the doc-method DECL answering
empty is OURS — the doc-method symbol should at least self-reference
and collect typed call sites (facades prove the machinery; find what
differs for instance-dispatch @method rows).

## H10 — `getSubscribedEvents()` string→method map — LANDED

C4 (MAJOR): `['event' => 'methodName']` returned from
getSubscribedEvents — same shape as route arrays. Symfony overlay
pattern gated on the enclosing method name, minting the self-flavored
named-method ref (shares H5's new capture).

## H11 — list-destructuring loses per-element return types

B5.3 (MINOR-MAJOR): `[$q, $a] = $this->fakeQueue()` unbinds element
types → 6/14 getJobId call sites dark. Needs array-shape returns
(`array{Queue, Agent}`) or positional peel off a Sequence return —
rides the sequence rail (ArrayIndex step exists). Park unless cheap.

## H12 (MINOR) — vendor-resolved method hover labeled "member", drops signature

C-minor: cross-file method hover through the vendor tier renders the
generic member arm instead of the method signature arm.

## Verification contradictions resolved during synthesis

- C2 vs the framework-entry proof: not a contradiction — the proof
  sampled ValidatorTest (extends TestCase DIRECTLY, claimed); C's
  misses all extend vendor-absent intermediates. Data gap, not code.
- A's heatmap sample re-confirmed test*/magic guards on guzzle 10/10.

## Fix-wave verification (round-4, post-hitlist)

- H3/H8: laravel probes land on the concrete parent / the imported class;
  pinned by `php_parent_call_through_same_leaf_aliased_parent`.
- H6 all four lanes verified on monolog: 1.1a (`@phpstan-param
  non-empty-array<LogRecord>`) types the foreach var; 1.1b/1.1c/2.4
  (`@inheritDoc`) inherit the ancestor's `@param array<X>` element type
  through multi-hop chains (publication `PackageSymbol{cls,"m#p#$v"}` +
  bare-container subscription edge at annot priority); 1.1d
  (`static::$prop`) picks up the prop's `@var Type[]`.
- H4 rename recount: LogRecord::$level 49 → 53 edits; MailHandler,
  ChromePHPHandler, BrowserConsoleHandler recovered. Residuals (2 sites,
  NEW root causes, not doc gaps): LogglyHandler:128 — closure param
  through `array_filter` callback (H7-adjacent, park with H11);
  MailHandler:71 `$highestRecord` — assignment flow into a null-guarded
  accumulator local.
- H5 verified: `#[DataProvider('m')]` gd string→decl, refs both ways,
  rename edits the string (symfony/demo); docblock `@dataProvider m`
  refs/gd/rename all reach the doc token (span = the name token,
  invocant = the class name — the class-body scope's package is the
  NAMESPACE, so `__PACKAGE__` mis-resolved); guzzle dead queue 352 → 148,
  provider-dead 196 → 2 (both grep-confirmed genuinely unused);
  demo test*-dead 0, all five named providers carry real fan-in.

## H2 + H10 wave (round-4, second fix slice)

- H2 root-cause split: the trait use-edge itself was SOUND (minimal case
  and typed receivers matched all along). The real bugs: (a) `@return
  static<int, static<...>>` — a union INSIDE generics — hit phpdoc_type's
  naive `|` split and dropped the whole fluent doc surface (depth-aware
  top-level split now); (b) `resolve_method_in_ancestors` answered the
  interface stub ahead of the trait's concrete method (php MRO interleaves
  `implements` before `use Trait`) — interface-deferral now shared with
  `resolve_super_method` via `hit_class_is_interface`; (c) the invocant
  ladder's bareword terminal minted a chain-receiver EXPRESSION text as a
  ClassName, freezing a garbage edge that read as a baked verdict — the
  matcher then never re-resolved with the index. Gated on
  `is_bareword_class_name` (now `\`-aware); the chained Sleep.php:440
  site is admitted and gd lands on EnumeratesValues.php:289. Laravel's
  remaining eachSpread sites are `new $collection` class-strings and
  closure params through test helpers — H7/H11 territory, recorded there.
- H10: bundled `symfony.scm` overlay — `getSubscribedEvents()` map
  strings (all three value shapes) mint `@ref.method.named.self`;
  refs/gd/rename round-trip on demo's subscribers
  (RedirectToPreferredLocaleSubscriber onKernelRequest 58:39 ✓).

