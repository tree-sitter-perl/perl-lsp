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

## R3 — constructor-promoted properties: navigation dark — PARTIAL

`public readonly Level $level` in a ctor signature. The Field symbol
IS minted (workspace-symbol finds it) and this round's slices fixed
typing (`$rec->channel` types string through the field hop) and the
class-content completion gate (a Field is class content BY KIND — the
sub-body/param-region locals gates now apply to Variables only). Still
dark: gd/hover/references on the ACCESS token (`$record->level` — the
member-lookup join that works for regular fields misses promoted ones;
201 accesses in monolog). The differential fixture is
`php_property_receiver_and_static_factory_chains`-adjacent: regular
field gd works, promoted doesn't, same file.

## R4 — references on class consts / enum cases — LANDED (one residual)

The const-access member refs closed the main gap: `Level::Debug` from
the decl now answers 198 refs (59 in src — the agent's grep found 55
real usages; the agents probed a binary predating the access
patterns). ONE over-match remains, and it is a real (small) rename
hazard: renaming an enum case named `Debug` also rewrites the leaf of
`use PhpConsole\Dispatcher\Debug as ...` — an UNRELATED class's
import line. The import-leaf `@ref.type` (deliberate, so CLASS
renames rewrite use lines) is being accepted by the member target's
bare-name matching; the matcher arm that admits a PackageRef row for
a class-OWNED member (Enumerator/const) target should refuse it — a
class member never appears as a php import leaf. Repro: constproj +
a decoy `use Some\Other\Debug as DebugTool;` — rename of the case
edits the decoy line.

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

## R8 — WP hook string callbacks (framework plugin, build-out item 6)

`add_action('init', 'wp_cron')`: the string never refs the function,
both directions dark. 993 string-callback sites + 161
`array($this,'m')` in WP. Poisons heatmap fan-in for every
hook-driven function. Fix: WordPress plugin emitting a FunctionRef
for arg#2 of add_action/add_filter/remove_*; same emit-hook shape as
the Mojo plugin lane.

## R9 — `new Foo()` ↔ `__construct` unlinked

304 `new Client(` sites; references on `Client::__construct` → 1
(itself); all 58 guzzle constructors land in the heatmap dead queue.
The class-token ref exists — only the ctor edge is missing.

## R10 — foreach ELEMENT typing (known engine residual)

Confirmed unchanged (`docs/prompt-sequence-types.md`): `$handler` in
`foreach ($this->handlers as $handler)` untyped even with
`@var list<HandlerInterface>` — needs both the generic peel
(`list<X>` → element X) and the binder edge.

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

- **Warm flicker (trust):** self::FORMAT gd answered "nothing" early
  in one cache's life, healed permanently ~30 runs later; fresh caches
  fine. Suspect: warm pack stub / chunked-persist race registering a
  const-less stub. Check: diff `stubs` rows vs full-blob symbols for a
  PHP file with class consts.
- `self::CONST` in CLASS-LEVEL initializers (property defaults)
  deterministically dark; the method-body form works (same file).
- gd on exactly-typed `(new Collection)->contains` also returns the
  subclass override (belongs to implementations, not gd).
- Completion ignores visibility (private members offered externally);
  completion annotates a declared `: int` as `int|float` (bag beats
  decl in the completion lane only); class hover renders
  `#[AllowDynamicProperties]` as the signature; multi-line sigs
  truncate; heatmap dead queue needs a PHPUnit `test*` gate
  (2105/2195 flagged symbols are runner-invoked test methods).
- `global $x` refs are 0 everywhere; hover on `$wpdb` answers the
  CLASS by name coincidence.
