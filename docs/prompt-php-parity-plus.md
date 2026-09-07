# PHP: beyond parity — the other tools' axes

The comparison axes live in `bench/RESULTS.md` (each battery names the
probe spec and the three tools' answers); the lanes and their silences in
`docs/adr/php-diagnostics.md`; the role-contract, completion and
narrowing rules in `docs/adr/role-contracts.md`,
`docs/adr/cursor-context-completion.md` and `docs/adr/flow-narrowing.md`.

Still open:

1. **Type-inference depth** — union and intersection types (the lattice
   forks in `docs/open-forks.md`), generics beyond the class-level
   `@template`, and framework DI (Symfony services, Laravel facades) — the
   docblock residuals list in `docs/prompt-php-target.md`.
2. **Case-insensitive method names** and the residuals with fixtures in
   `docs/PARKED.md`.
3. **The cold references walk** — the remaining gap to Intelephense's
   110 ms is the rows→whole upgrade and the matcher (`docs/PARKED.md`).

Method: every axis is measured before and after with `bench/compare/` on
the same probe battery; discussion questions go to `docs/open-forks.md`.
