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

## Cold references — reattributed (2026-09-02 evening)

Editor-restart shape (workspace persisted, RAM caches empty), `__construct`
in guzzle's `Client.php`, 126 candidate files, 3 fresh-cache runs: cold
1,050 ms mean, warm 85 ms. Decode (SQLite + zstd + bincode, 151 ops,
21 double-decodes on the rows→whole upgrade) is ~220 ms — 21% of cold.
The remaining ~825 ms has no covering timer; `strace -c` puts syscalls
at 3% of wall (CPU-bound, not I/O), and the SQL prefilters are not it
(disabling them changes nothing). Candidates for the untimed share, in
order: `VisibilityAxis::for_origin` per candidate (`collect.rs`), the
post-decode index rebuild, `resolve_method_in_ancestors` per candidate
(`ancestry.rs`); `module_declaring_method_in_package` recomputes the same
`(name, class)` verdict 633 times per walk with no session memo. Next
step is instrumentation on those three (`ghost_stats::timed`), then the
fix the numbers name — a parallel decode prefetch of the candidate set is
the one bounded win already sized (~150–190 ms).

