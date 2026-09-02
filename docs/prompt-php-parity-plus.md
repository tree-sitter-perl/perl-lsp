# PHP: beyond parity — the other tools' axes

Round-1 head-to-head (`bench/RESULTS.md`, 2026-09-02): navigation,
references, rename and completion are at parity with Intelephense (free)
and ahead of phpactor on recall, at a third of Intelephense's memory. The
axes where they still lead, in the order they are worth taking:

1. **Hover docblock text.** Ours renders the signature and the inferred
   type; both others render the description. The doc lane already parses
   the comment for `@return`/`@param`/`@var` — the description is the
   same comment's first paragraph.
2. **Cold cross-file references.** First references walk on guzzle
   1,057 ms vs 110 ms. Attributed: both per-walk rehydration memos are
   defeated on the walk path (`docs/hitlist-php-round5.md`, R5-4).
3. **Diagnostics.** Intelephense reports undefined symbols, undefined
   methods/properties on known classes, argument-count mismatches,
   undefined variables. Ours: unresolved function/method calls. Each new
   lane needs a SOUND gate (a receiver whose class is unknown never
   reports) — precision beats recall on a linter surface.
4. **Vendor-heavy projects.** Composer trees installed for the three
   corpora; the battery gains probes that resolve through `vendor/`
   (PSR interfaces, `AbstractController::render`).
5. **Editing features.** Signature help with docblock parameter text,
   import-class code action, type definition, implementations.
6. **Type-inference depth.** Union types (the open lattice fork —
   `list<A|B>` element typing), generics beyond the class-level
   `@template`, framework DI (Symfony services, Laravel facades).

Method: every axis is measured before and after with
`bench/compare/` on the same probe battery; the scoreboard lives in
`bench/RESULTS.md`. Tightening rounds (`tighten-loop`) between arcs.
Discussion questions go to `docs/open-forks.md`.
