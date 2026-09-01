# ADR: Destructuring — positional binding off tuple sources

`my ($q, $a) = mk();` (Perl) and `[$q, $a] = $this->fakeQueue();` (php)
bind each LHS slot to ONE POSITION of the RHS value. Before this ADR the
Perl binder existed but its literal-list source never typed as a tuple,
and php had neither binder nor tuple sources — six of fourteen `getJobId`
call sites in laravel's own tests went dark behind one destructured
return. A GLOBAL concern: every language perl-lsp serves has the
statement shape, so there is one binder rule, and each language only
supplies its sources.

## Decisions worth keeping

1. **One binder, language-neutral.** A destructuring slot is a
   `FlowEdge { extraction: Extraction::Positional(n) }` (core_types.rs),
   lowered to `Variable{slot} → Projected { base: Expr(rhs), step:
   ArrayIndex(n) }`. The registry's `ArrayIndex` arm projects
   `element_at(n)` off whatever the RHS materializes to — at query time,
   index in hand, so a cross-file tuple return works. Perl mints the edge
   from the `@flow.lhs` capture (queries/perl/flow.scm; paren-list slots);
   php from `@flow.slot` on a `list_literal` (queries/php/skeleton.scm),
   with the slot's position counted over the list text's top-level commas
   so `[, $b] = …` lands on index 1. A slurpy tail (`my ($a, @rest) = …`)
   is `Extraction::Slurpy`: the whole-source edge, i.e. the tail carries
   the source's element lattice — an approximation, stated, not exactness.

2. **Sources are tuples: `InferredType::Sequence(Vec<T>)` with a type
   PER SLOT.** The same variant is also the homogeneous-sequence carrier
   (`array<X>` / `X[]` publish a ONE-slot `Sequence`, read by the `Element`
   peel). The two readings collide only at `element_at(i > 0)` on a
   one-slot value, which answers `None` — honest for a length-1 tuple,
   under-typed for a homogeneous sequence. Accepted: the safe answer is
   the conservative one; splitting tuple-vs-homogeneous is a variant
   change and a cache bump, deferred until a real corpus site needs it.

3. **Tuple sources stay EDGES until queried.** Perl's walker types a
   literal list eagerly (`list_literal_type`: every element's own Expr
   witness, `None` if any is unknown — a holey tuple mis-projects), so
   `return (Queue->new, Agent->new)` bakes `Sequence<Queue, Agent>` on the
   return arm. A pack extractor cannot type elements at extraction time
   (they are variable reads, calls, hops), so its key-less array literal
   publishes `WitnessPayload::Tuple(Vec<WitnessAttachment>)` — the
   element EDGES — and `materialize` chases each one, minting the
   `Sequence` only when every slot answers (the same all-or-nothing rule).
   A keyed element or a spread disqualifies the literal (it is not a
   tuple; the keyed-shape lane owns it). This is the "edges, not values"
   rule applied to a compound value.

4. **Doc shapes are sources too.** phpdoc `array{A, B}` / `list{A, B}` /
   `array{0: A, 1: B}` parse to a `Sequence` tuple; a STRING-keyed shape
   (`array{name: string}` / `object{jobs: array}`) parses to
   `HashWithKeys` — the structural-shape lane, not a tuple. Both refine a
   bare declared container the same way `doc_admits` lets `array<X>`
   refine `array $p`: a `@return array{…}` over `: array` wins, and an
   undeclared return with a tuple literal gets its return-arm chain at
   annotation priority so the tuple beats the bare `array` annot (latest
   wins at equal priority; `HashRef` never subsumes `Sequence`).

5. **The safe subset, and what is deliberately outside it.** IN: scalar
   slots at fixed positions; a slurpy tail (element lattice); nested
   `foreach ($rows as [$a, $b])` (the list peels the collection's
   `Element`, then indexes — two projections chained through the list's
   own `Expr` span). OUT, answering `None` rather than guessing: keyed
   destructuring (`['k' => $v] = …`, `Extraction::KeyOf` is minted for
   Perl hashes but has no projection lowering yet); nested list slots
   (`[[$a, $b], $c]` — the inner vars are not even declared today);
   Perl list-vs-scalar context of the RHS (`wantarray` subs answer by
   their list arm — the arm fold is context-blind); `return @arr` where
   the array's per-index types are unknown (`element_at` on a bare
   `ArrayRef` is `None`); positions past a one-slot homogeneous sequence
   (decision 2).

## Related decision recorded here (not destructuring): H1 rename scope

Round-4 asked whether a method rename should choose its override scope
from the CURSOR (concrete override → `dispatch`, contract decl →
`hierarchy`). Decided NO: `rename.overrideScope` stays a single global
setting (README, `initializationOptions.rename`), default `hierarchy`.
Predictable beats clever — a rename whose blast radius depends on which
token you happened to click is smartmatch-grade surprise. Users who want
the precise mode set it once.
