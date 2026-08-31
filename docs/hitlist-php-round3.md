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

## R4 — references on class consts / enum cases: decl-only + a wrong-identity hit

gd usage→decl works (landed this round: `User::VERSION` /
`self::LIMIT` / `Level::Debug` mint member-lane refs; a true enum
case's VALUE types as its enum). But the references PROJECTION never
surfaces usage sites from the decl (Level::Debug: 55 real usages,
refs answer 2), and the one extra hit is `use PhpConsole\Dispatcher\
Debug` — an unrelated class, name-only fallback. Const rename would
miss every usage. Fix shape: the refs_to matcher needs to match
MethodCall refs against Enumerator decls (it matches them for
methods; the const access rides RefKind::MethodCall).

## R5 — local variable refs fragment per assignment (rename hazard)

`$orderby` in WP `get_bookmarks` (10 occurrences, one function) comes
back as three disjoint ref islands depending on the probe point —
each reassignment starts a new binding. An LSP rename from any site
renames a fragment and breaks the code. Single-assignment locals are
correct. Root cause: PHP has no `my`; assignment-is-declaration mints
a fresh decl per assignment and refs bind nearest-preceding. Fix
shape: same-scope re-assignment should REBIND, not re-declare (the
flow lane already has Rebind vocabulary).

## R6 — phpdoc residuals: `@global` + generics + the factory arm

WP core is docblock-typed (858 `@return` docblocks vs ~249 native in
wp-includes; 219 `@global wpdb $wpdb`). The doc lane reads
`@return`/`@param`/`@var` — but `@global wpdb $wpdb` is unread, so
`$wpdb->get_results` hover/completion is dark everywhere. Laravel's
generics leak raw (`$this->where()->first()` answers `TValue`).
The static-factory arm of the agent's evidence is FIXED by the
scoped-call hop (landed after the agents ran; re-verified:
`WP_Block_Type_Registry::get_instance()->register` at blocks.php:817
now hovers the real signature and gd lands at
class-wp-block-type-registry.php:48 — the hop composes with the
doc-`@return` on `get_instance`).

## R7 — member completion on an UNRESOLVABLE receiver dumps scope symbols

`$p->` where `$p: PromiseInterface` (vendor absent) → 10 items, all
garbage for a member slot (`$c2`, `GuzzleHttp`, `probeChain`...).
Slot=Member is already detected — suppress the identifier fallback
when the receiver is typed-but-unresolvable (empty list is honest).

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
