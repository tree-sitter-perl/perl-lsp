# PHP dogfood round 3 — hitlist

Two fresh agents, ~105 probes: monolog + laravel/framework (agent 1),
WordPress core + guzzle (agent 2). Binary: release all-langs at the
member-chain commit. Every round-2 fix re-verified holding (chain
typing, FQ implementations, alias parent edges, self::/static::
METHOD dispatch, trait attribution, typed member completion with zero
garbage on resolvable receivers). No crashes; guzzle heatmap clean.

Items marked LANDED were fixed same-round (the round-3 close slice);
the rest are the current open ledger, ranked.

## R1 (TOP) — `parent::method()` dark; rename CORRUPTED code — LANDED

Was: gd/hover nothing, references from the decl side missed every
`parent::` site (312 in laravel src, 92 in monolog), so renaming
`JsonFormatter::normalizeRecord` produced broken code. Fix: the pack
declares its SUPER receiver spelling (`super_receiver` — php
`parent`), and the ref is minted as the model's SUPER method token
(`SUPER::normalizeRecord`, current-package invocant, name-token span)
so gd/references/rename all ride the existing SUPER lane. Re-verified
on the agent's repro: gd on GoogleCloudLoggingFormatter.php:29 →
JsonFormatter.php:106 (the immediate parent's override, not the
grandparent's), and references on the decl now list all 4 previously
missed `parent::` sites. The hop lane deliberately skips SUPER
receivers (return typing of `parent::` calls is a residual — a hop
would find the CHILD override).

## R2 — `$this->prop->method()` never dispatched — LANDED

Was ~3,003 sites in laravel src. Property ACCESS now mints the same
chain-hop witness a call does (`@hop.call` on
`member_access_expression`), and fields answer through the registry
(`PackageSymbol{class, field} → Edge(Variable)`, pack-gated:
`field_registry_edges` — php only, because cpp's field answers go
through the instantiation-aware `member_value_type` lane and the raw
registry answer regressed template substitution until gated).
Re-verified on the agent's repro: gd on
`BufferHandler.php:97:25` (`$this->handler->handleBatch`) →
`HandlerInterface.php:57`. Static factory chains
(`Registry::instance()->register()`) ride the same slice via
`@hop.call` on `scoped_call_expression` + the bareword-receiver rule
(a bareword dispatches as the class).

## R3 — constructor-promoted properties: navigation dark — LANDED

Was: `public readonly Level $level` minted the Field (workspace-symbol
found it, typing worked) but gd/hover/references on the ACCESS token
(`$record->level`) were dark — 201 accesses in monolog. Two fixes:
the class-content gate now exempts FIELDS from the method-scope
refusal (`symbol_is_class_content` — a Field inside its own class's
method scope is a promoted ctor property, never a local; Variables
keep the refusal, so sub-body locals stay out), and the identity is a
GROUP: the one `$level` token declares BOTH the field and the ctor
param, so `promoted_param_use_spans` folds the param's sigil-narrowed
body uses into the member's group and rename rewrites every spelling
(decl + accesses + `$level` body reads — leaving any behind breaks
code). Both cursor sides resolve to the same group
(`promoted_field_twin` re-targets the decl cursor off the Variable).
Re-verified on monolog: gd on `$record->level`
(AbstractHandler.php:47) lands on LogRecord's promoted decl, hover
types it `Level`, references from the decl answer 48 sites.

## R4 — references on class consts / enum cases — LANDED (one residual)

The const-access member refs closed the main gap: `Level::Debug` from
the decl now answers 198 refs (59 in src — the agent's grep found 55
real usages; the agents probed a binary predating the access
patterns). The over-match residual is CLOSED: the decoy-import repro
(`use PhpConsole\Dispatcher\Debug as DebugTool;` beside an enum case
/ class const named `Debug`) no longer reproduces — an intervening
slice (the FQ use-map work) fixed it — and
`php_member_rename_never_rewrites_import_leaves` pins it for both
member shapes.

## R5 — local variable refs fragmented per assignment — LANDED

Was: `$orderby` in WP `get_bookmarks` (10 occurrences, one function)
came back as three disjoint ref islands, and a rename from any island
rewrote a fragment. Fix: the pack declares `function_scoped_vars`
(php) — the FIRST assignment per (name, enclosing sub scope) is THE
declaration, re-anchored to the sub scope so every block's uses bind
it through the chain; later assignments demote to WRITE references.
Typing stays per-witness (unchanged). Re-verified on the agent's
repro: all three probe points now answer the identical full 10-line
set. Python shares the semantics and can flip the same fact when its
pack matures.

## R6 — phpdoc residuals — LANDED (@global + generics; conditionals remain)

`@global wpdb $wpdb` rows now type the `global $wpdb;` binding the
def below declares (the global statement is a real declaration, and
the doc row joins like a @param) — on real WP core,
`$wpdb->get_results` hovers (`: array`, through wpdb's own doc
returns) and `$wpdb->` completes real members; the widest WP gap.
Generics landed as the Builder lane (above). Still unread: Laravel
12's CONDITIONAL return types, PHPStan array-shapes, `@template` at
METHOD level, and rendering doc PROSE on hover.
The static-factory arm of the agent's evidence is FIXED by the
scoped-call hop (landed after the agents ran; re-verified:
`WP_Block_Type_Registry::get_instance()->register` at blocks.php:817
now hovers the real signature and gd lands at
class-wp-block-type-registry.php:48 — the hop composes with the
doc-`@return` on `get_instance`).

## R7 — member completion on an UNRESOLVABLE receiver — LANDED

Was: `$p->` where `$p: PromiseInterface` (vendor absent) → 10 items,
all garbage. The member slot now answers EMPTY when the receiver's
class is known to NOTHING (no local decl, no index candidate) — the
deliberate typed-receiver fall-through survives only for a class the
analysis knows (cpp's self-access-sees-private gold case). Verified:
the guzzle probe answers zero items; a resolvable chain
(`User::query()->firstWhere(...)->`) completes real User members.

## R8 — WP hook string callbacks — LANDED (tier-1 pack plugins)

Was: `add_action('init', 'wp_cron')` — the string never refs the
function, both directions dark (993 string-callback + 161
`array($this,'m')` sites in WP), poisoning heatmap fan-in for every
hook-driven function. Fix (docs/prompt-pack-plugins.md tier 1): the
string-named reference captures (`@ref.call.named` /
`@ref.method.named` — span = the content between the quotes) + the
bundled WordPress overlay (`queries/php/frameworks/wordpress.scm`,
pure query). Re-verified on real WP core: refs on `wp_cron`'s decl
now include both registration sites; `wptexturize` answers 22 refs
including all 11 default-filters registrations; rename of `wp_cron`
rewrites exactly the 7 characters inside the quotes; the array form
(`array($this, 'wp_loaded')`) refs the method through the `$this`
receiver. The loader shipped in the same slice: overlays load from
`<plugin-dir>/<name>/queries/<lang>.scm` with per-overlay compile
isolation, content-hash query caching, fingerprint invalidation, and
a `--plugin-check` arm for `.scm` files. Hook-NAME identity LANDED as
its own slice: `@def.handler.named` (registration first-arg → a
stacked `HandlerOwner::Global` Handler symbol) + `@ref.dispatch.named`
/ `@dispatch.via` (firing first-arg → a DispatchCall labeled by the
firing function) — the model's Handler rail, extended with the
`Global` owner variant (flat program-wide hook namespace, no
receiver; receiver-gated machinery skips it by construction).
Re-verified on real WP core: references on `'init'` from the
`do_action` site answer 190 sites across 127 files — exactly the grep
count of hook-function first-arg spellings — and rename rewrites the
name inside the quotes at every one.

## R9 — `new Foo()` ↔ `__construct` — LANDED

The pack declares its constructor convention
(`constructor_names`, php `__construct` — a `PackFacts` lane), and
`TargetRef::method` — the ONE speller every Method-target builder
routes through — marks such a target `ctor_of` its class. The
backward matcher then admits the class's construction sites (the
ctor `FunctionCall` refs, which carry the CLASS name) as
NON-rewritable references: renaming `__construct` never touches
`new Client(`. On guzzle: references on `Client::__construct` went
1 → 193, and the heatmap dead-queue's `__construct` rows halved
(58 → 29 — the rest are genuinely never constructed in-repo).

## R10 — foreach ELEMENT typing — LANDED

Was: `$handler` in `foreach ($this->handlers as $handler)` untyped even
with `@var list<HandlerInterface>`. Three pieces, all on the sequence
rail the tuple spike built (`docs/adr/sequence-types.md`):
- phpdoc sequence spellings (`list<X>` / `array<X>` / `array<K,V>` /
  `iterable<X>` / `X[]`) parse to a one-slot `Sequence` carrying the
  element (previously the ClassName fallback minted a BOGUS class
  `list<X>` — the fix removes a wrong answer too).
- A refining doc row now beats a bare declared container:
  `protected array $h` + `@var list<X>` is the canonical php
  refinement (the syntax cannot spell the element), so the doc witness
  REPLACES the redundant `array` annot on that slot; unrelated
  declared types still win.
- The foreach binder mints `Variable → Projected{base, Element}` —
  a new uniform-element `ProjectionStep` (all elements agree → that
  type; heterogeneous/untyped → None) — base = the collection's
  Variable (simple) or its Expr span (member access, riding the hop).
Re-verified on monolog GroupHandler: `$handler` hovers
`HandlerInterface`; gd on `->isHandling` / `->handle` off the loop var
lands in HandlerInterface.php.

## R10b — BookStack (Laravel-app) follow-on probes

From the framework-tier round on the real app + vendor corpus:
- **Inherited statics** (`Widget::query()` with `query` on a parent in
  another file) — LANDED: `member_def_location` now walks the
  leaf-keyed parent edges (the instance-receiver path always had this
  via the invocant ladder; the bareword-scoped lane didn't). Pinned in
  `language_scope.rs`. Verified on BookStack through the TWO-level
  hierarchy: `View::query()` → app `BookStack\App\Model` (whose parent
  is the ALIASED `Model as EloquentModel` import) → vendor Eloquent
  `Model::query` at line 1839. (An earlier "still dark" reading was a
  probe on the `::` token — the round-1 coordinate trap, again: always
  hover first to confirm the landed token.)
- **Builder-chain receivers** — LANDED for the clean-docblock tier:
  `@template T` class rows feed the SAME per-class param axis cpp
  templates use; `@return Base<static>` publishes
  `Operator(InstanceOf{base, [Receiver]})` (base leafed — dispatch is
  leaf-keyed); `@return TModel|null` methods project through the
  existing `ParamOf` writeback. On real BookStack:
  `User::query()` types `Builder<User>`, `->firstWhere(...)` types
  `User`, and gd off the chain result lands in the app's base class.
  Residuals: Laravel 12's CONDITIONAL generic returns
  (`($id is ... ? Collection<...> : TModel|null)` on `find`) are
  beyond the parser (correctly rejected, stays untyped); method-level
  `@template TValue` on the BuildsQueries trait (`first()`) is a
  separate binding the class-keyed axis doesn't model.
- `new self()` receivers — LANDED: the ctor witness routes the
  current-class spellings through the pack's `hop.recv` shaping, so
  `new self()` carries the enclosing class. Fixing it surfaced (and
  fixed) a broader pre-existing bug: `$x = (new W())->c()` typed as W,
  because the flow edge's literal-narrowing grabbed the ctor inside
  the rhs — it now stands down whenever the rhs span carries its own
  witness (the chain hop). End-to-end on BookStack: `$record =
  (new self())->forceFill([...]); $record->save();` — gd on `save`
  lands on vendor `Model::save` (self ctor → fluent `@return $this`
  docblock → inherited vendor method).

## R11 — trust/cosmetic tail

- **Warm flicker (trust) — INVESTIGATED, not reproducible.** self::FORMAT
  gd answered "nothing" early in one cache's life, healed permanently
  ~30 runs later; fresh caches fine. The diagnostic ran the suspected
  race both ways: a 61-file php project (chunked persist) and real
  monolog (`SyslogFormatter.php:34 self::FORMAT`), cold + repeated warm
  probes on a fresh cache — 14/14 correct, no flicker. Leading
  explanation: the observed cache was written by a MID-DEVELOPMENT
  binary under the same EXTRACT_VERSION (dev iterations change
  extraction without bumping it, so stale rows look valid), which fits
  "healed permanently" (a later rewrite replaced the rows) and "fresh
  caches fine". Watch-only: if it recurs on a cache whose whole life is
  one binary, re-open with the stub-vs-blob diff (`stubs` rows are
  symbols-evicted BY DESIGN — the check is that symbol-needing readers
  rehydrate, not that stubs carry symbols).
- `self::CONST` in CLASS-LEVEL initializers — LANDED: the class-body
  scope opens under the OUTER package context, so the invocant
  ladder's scope-chain walk found no enclosing class for a ref
  sitting directly in the body; `enclosing_class_for_scope` now falls
  back structurally (narrowest containing Class symbol). gd/refs on a
  property-default `self::FORMAT` answer like the method-body form.
- gd on exactly-typed `(new Collection)->contains` also returns the
  subclass override (belongs to implementations, not gd).
- Completion ignores visibility (private members offered externally);
  completion annotates a declared `: int` as `int|float` (bag beats
  decl in the completion lane only); multi-line sigs truncate.
- **Attribute hover + the heatmap dead queue — LANDED** (the
  framework-entry arc): php `#[Attr]` annotations now ride the
  `@sym.attr` lane onto `Symbol.attributes`, and the hover signature
  is the line carrying the NAME token (no more
  `#[AllowDynamicProperties]` rendered as the class signature; cpp's
  `template<...>`-first-line hovers improved the same way). The dead
  queue gained two value-declared guards: `framework-entry`
  (`entry.json` rules — bundled PHPUnit + Laravel documents, plugin
  dirs extend — matched by attribute names, method name/prefix, and a
  leaf-keyed isa gate through the ancestry walk) and `runtime-invoked`
  (`LangPack::runtime_invoked_methods`, php's magic methods — the
  method-shaped sibling of `entrypoint_symbols`). Class-array
  callables (`[UserController::class, 'index']` — Laravel routes /
  event maps) mint REAL method refs (base skeleton — a language
  convention, not framework vocabulary). Measured on guzzle (the
  ledgered corpus): dead queue 2,195 → 352 (1,778 framework-entry +
  40 runtime-invoked), remainder honest (unconstructed ctors, PSR
  factory API unused in-repo). Monolog: 586 test methods shielded,
  181 remain.
- `global $x` refs are 0 everywhere; hover on `$wpdb` answers the
  CLASS by name coincidence.
