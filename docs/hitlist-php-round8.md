# PHP round 8 — the other tools' axes (day-2 arc, 2026-09-02)

Round 1 of the head-to-head (`bench/RESULTS.md`) put navigation,
references, rename and completion at parity with Intelephense (free) and
ahead of phpactor. This round takes the axes where they led, measured
with `bench/compare/` on the same fixture and corpora.

| axis | before | after |
|---|---|---|
| hover docblock text | signature + inferred type only | the docblock summary (or a property's `@var` trailer) under the signature |
| signature help | dark (the pack claimed the capability but ran the Perl cursor path) | the declaration's parameter list, active parameter, return annotation, docblock — local and cross-file (`cursor_sentinel::call_at` on pack-declared `call_shapes`; `signature_from_source`) |
| document outline | flat (the php class scope spans the whole declaration, so the body-scope finder rejected it) | methods and properties nest under the class |
| diagnostics | member-operator lanes only (0 on the fixture; Intelephense 8) | `unresolved-method`, `undefined-property`, `non-public-access`, `arity-mismatch`, `undefined-variable`, `undefined-type` — the fixture's 8 findings exactly (`docs/adr/php-diagnostics.md`) |
| chained receivers | `$this->mailer->m()` untyped in the lanes | the class body registers `$this` as a witnessed variable; every chain off it resolves |

The seam that made the lanes possible and every verb better: the
extractor registers a class body with the class as the scope's package
and a `$this` witness, so a receiver chain resolves through the registry
like a typed variable.

## Precision (corpus `--check`, no PHPUnit/Symfony vendored)

Counts after the silence rules landed; every remaining row was read:

- guzzle: `undefined-type` 1,331 — 1,091 are `GuzzleHttp\Server\Server`
  (no such class in the checkout; a true finding Intelephense shares),
  208 `PHPUnit\Framework\TestCase`; `undefined-variable` 0;
  `non-public-access` 0; `unresolved-method` 2 (a `createMock` receiver
  and a promise typed through an unresolved handler); `arity-mismatch` 2.
- monolog: `undefined-type` 158 (PHPUnit attributes and mocks, optional
  transports — Elastica, Gelf, MongoDB); `unresolved-method` 9 (PHPUnit
  `createMock` receivers, `#39`; `indentStackTraces` behind an
  `instanceof`, `#38`); everything else 0.
- symfony/demo: `undefined-type` 358, every one a Symfony/Doctrine class
  without vendor — Intelephense reports the same 35 on
  `BlogController.php` alone.

## Found on the way

- **Inference bug**: `$r = $handler(new Request(), [])` typed `$r` as
  `Request` — the assignment narrowing descended into any literal inside
  the right-hand side. Wrapper-only now (parentheses and whitespace);
  pinned by `php_callable_variable_call_does_not_take_its_arguments_type`.
- The skeleton minted variable READS for a static property's `$name`,
  a property declaration's own token, and left `catch ($e)` / `use
  (&$x)` undeclared; `\Throwable` lost its absolute prefix; `f(...)`
  counted one argument; `use function A\b;` parses as a class row.

## Open

- `#38` `instanceof` narrowing — interface-typed receivers stay silent.
- `#39` PHPUnit mock typing overlay.
- Intelephense's remaining lanes: unused symbols, deprecations,
  documented-vs-declared type checks, argument type checks.
- `typeDefinition` on a member read (`$this->mailer`) — the cursor value
  type reads `Expr(span)` at the member token; the member's own type
  lives on the class.

## Cold references — attributed on the editor path (2026-09-02, late evening)

The batch CLI re-analyzes the origin per request, so it cannot stand in
for the editor; measured over stdio instead (`bench/compare`,
`spec-coldrefs.json`: `__construct` in guzzle's `Client.php`, 304
references, workspace persisted, server restarted, box under a
verification net's load):

| | ms |
|---|---|
| first references | 286 |
| second / third | 25 / 27 |
| `rehydrate.loader` (871 lookups) | 172 |
| `bagcache.decode` (104 decodes: SQL 8, zstd 42+23, bincode 48+44, post 19) | 168 |
| `refs.matcher_view` (54 candidates, 45 rows-view + 10 whole upgrades) | 139 |
| `mroc.total` (ancestor walk, 824 calls) | 25 |
| `refs.collect` (the matcher itself) | 18 |
| `refs.visibility_axis` | 7 |

Decode is ~60% of the first answer and is serial: one `matcher_view` per
candidate inside the walk loop. A rayon prefetch of the candidate set's
rows views into the LRU before the sequential match measured FLAT (three
runs each, same box: prefetch on 245 / 267 / 287 ms cold, off 224 / 245 /
271 ms; warm unchanged) and was not kept. So the decodes do not run in
parallel as written: the rehydration loader runs `load_one_diag` — the
SELECT and the zstd + bincode decode — inside `RetainedReader::with`,
i.e. under the one retained connection's mutex (`blob.rs`,
`open_and_load_diag_retained`), so every concurrent decode queues on it.
The lever is to hold the lock for the byte fetch only and decode outside
it — done, and the prefetch then measured 174 / 194 / 189 ms cold against
229 / 209 / 237 without it (`bench/RESULTS.md`). Fewer bytes per
decode (the 10 rows→whole upgrades, baked match verdicts) is the other
half. `module_declaring_method_in_package` runs 382 times to
conclude nothing each time — cheap here (0.2 ms), a memo candidate at
scale.

## Residual — a value read of a method-only name (2026-09-03)

`$this->session->store` where the class declares only `session()`: the
diagnostics lane now reports the undeclared property (php's syntax
decides the kind — `member_shapes_are_strict`), but goto-def still
surfaces the method (the name-keyed candidate set mints a shape-keyed
target only when the class overloads the name across kinds) and hover
types the read through the method's return. Both want the pack's strict
rule at the projection (`TargetRef::member_shape` minted whenever the
pack is strict, `member_value_type` skipping the method arm for a value
read on a strict pack).

## LANDED — an untyped reassignment keeps the earlier type (2026-09-03)

Landed: a failed REASSIGNMENT edge materializes to `InferredType::Unknown`,
a temporal reset in the framework fold that poisons the arms and copies
reading it (`docs/adr/flow-narrowing.md`).

`$user = new WP_Error(...)` in one branch, then `$user = wp_signon(...)`
(a `WP_User|WP_Error` return the lattice cannot hold): the second
assignment pushes no witness, so the `WP_Error` witness stands and
`$user->ID` reports an undefined property (WordPress, ~150 rows). The
narrowing cutoff (`earliest_rebind_in`) ends a GUARD region at a rebind;
an assignment witness needs the same rule — a rebind whose value does
not type should end the earlier value's region, not leave it standing.
One with the union fork.


## PARKED — `self::VOID` reads report an undefined property (2026-09-03)

`class-wp-block-processor.php` declares `const VOID = 'void'` and reads it
twice; both reads report `undefined-property 'VOID'`. `--parse` on a
reduced file shows tree-sitter-php lexing the constant NAME `VOID` as the
`void` type keyword: `const VOID = "v";` is an `ERROR` node, so the
declaration never enters the tree and the lane honestly finds no member.
Any keyword-spelled constant name (`VOID`, `STRING`, `ARRAY` — PHP keywords
are case-insensitive) hits it. A grammar fix upstream; recovering the
declaration out of the `ERROR` node would be a rule-#1 scan of error
children for `const NAME =`, deferred until a corpus shows more than two
rows.
