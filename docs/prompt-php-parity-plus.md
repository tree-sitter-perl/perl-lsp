# PHP: beyond parity — the other tools' axes

Landed: hover docblock text, diagnostics (`docs/adr/php-diagnostics.md`),
signature help, document outline, import-class code action. Cold
cross-file references improved via the retained-connection lock split
(`bench/RESULTS.md`); the remaining rehydration-memo cost on a
heatmap-scale walk is tracked in `docs/PARKED.md`.

Still open:

1. **`typeDefinition` on a member read** (`$this->mailer`) — the cursor
   value type reads `Expr(span)` at the member token; the member's own
   type lives on the class.
2. **Type-inference depth** — union types (the open lattice fork,
   `docs/open-forks.md`), generics beyond the class-level `@template`,
   and framework DI (Symfony services, Laravel facades) — the docblock
   residuals list in `docs/prompt-php-target.md`.

Method: every axis is measured before and after with `bench/compare/`
on the same probe battery; the scoreboard lives in `bench/RESULTS.md`.
Discussion questions go to `docs/open-forks.md`.
