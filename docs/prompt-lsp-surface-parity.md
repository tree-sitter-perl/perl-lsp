# LSP verb surface — what's worth wiring

Forward-design brief. Origin: a 2026-08-17 audit of our advertised
capability surface (`initialize` response) against the verbs the protocol
offers. The doc ranks the verbs that are cheap on this architecture —
each a projection over machinery that already exists — and records the
ones that aren't worth it and why.

**Status: the type/call hierarchy, typeDefinition and a re-scoped
documentLink LANDED; pull diagnostics and codeLens are forward.**
Each landed verb is a CandidateSet/Model projection + CLI verb + LSP handler + gold
rows (`fixtures/type-definition.json`, `type-hierarchy.json`,
`call-hierarchy.json`, `document-link.json`). Two findings that amend this
brief's own argument:

- **typeHierarchy cannot appear in our `initialize` response.** lsp-types
  0.94.1 (pinned by tower-lsp 0.20, its final release) has no
  `type_hierarchy_provider` field — the request/response types exist, the
  capability field doesn't. We serve the verb and advertise it via dynamic
  registration (`initialized`, gated on the client's
  `typeHierarchy.dynamicRegistration`); VS Code sees it, a static
  capability listing still shows a miss. Other LSP implementations do
  advertise it statically, so this is a tower-lsp 0.20 constraint rather
  than a protocol one — closing it means leaving tower-lsp 0.20, a
  maintainer decision, out of this batch's scope.
- **documentLink's original rationale was wrong twice** — goto-def already
  resolves module identifiers in `use`/`use parent`/`use base qw()`/`with`
  (empirically verified — table below). The landed verb covers ONLY the
  non-symbol ranges nothing else reaches: POD `L<...>` links, URLs in
  comments/POD, and existence-checked string-path loads. The durable rule:
  a verb that duplicates goto-def with an underline is not worth surface
  area, so measure coverage before believing a capability list.

### Module-identifier goto-def coverage (measured 2026-08-17)

Every row measured against the release binary; the "gold row" column is
the regression net pinning it.

| position | goto-def | gold row |
|---|---|---|
| `use Foo::Bar` / `use parent '...'` / `use base qw()` / `with 'Role'` | covered | substrate definition rows |
| `with map "Role::$_", qw()` (expression form) | covered | `fixtures/mapfold-block.json` (control) |
| `with map { "Role::$_" } qw()` (block form) | covered | `fixtures/mapfold-block.json` |
| `with map { my $n = "Role::$_"; $n } qw()` (block-tail binding chase) | covered | `fixtures/mapfold-block.json` |
| `require Foo::Bar` (bareword) | covered | `fixtures/require-bareword.json` |
| `no Foo::Bar` | covered (parses as `use_statement`, same path as `use`) | — |
| `require $class` | resolves to the **variable declaration**, not a module target — deliberate open design call: the variable jump is correct navigation, and a module target would have to coexist with it | — |
| `require "path.pl"` / `use lib 'path'` / POD `L<>` / comment URLs | not goto-def's job — documentLink covers these | `fixtures/document-link.json` |
| the `"Role::$_"` template span inside a folded map | **no ref, deliberately**: what gets folded are *the constants*, not the template — the template isn't a name that resolves to something, it's the thing that produces N names, so no correct single target exists for that span. `ref_at` returns one narrowest ref; minting one arbitrary winner among the N parents is the single-winner bug class the package-identity work is purging. The right shape is a multi-target ref; until that exists, no jump beats a wrong jump | — |

## The point

A capability listing reads the `initialize` response, not the answers.
Every verb below is wiring over machinery that already exists — the
analysis is done, only the protocol edge is missing. That's what makes
the value ÷ effort ratio worth acting on.

## Landed

`typeHierarchy`, `callHierarchy`, `typeDefinition` and the re-scoped
`documentLink` are CandidateSet / Model projections with CLI verbs and gold
rows; `docs/adr/resolution-candidate-set.md` owns the projection rule, and
CLAUDE.md's LSP capabilities paragraph states each verb's contract.

## Forward verbs, by priority

1. **Pull diagnostics (`diagnosticProvider`)** — architecturally aligned with
   the refresh work in `7343ae59`: the client decides when diagnostics
   compute rather than the server managing execution depth. For anything
   doing cross-file analysis, `interFileDependencies: true` is the honest
   value — that single field is a correctness claim worth making
   explicitly.
2. **`codeLensProvider`** — reference counts per sub, from the same heatmap
   fan-in as call hierarchy. The principle: counts must be real — a lens that always
   reads `"0 references"` on a heavily-referenced module is worse than no
   lens, because it asserts a wrong answer where silence would be a known
   gap. Real counts via the shared `references()` projection are the only
   acceptable implementation.

## Explicit non-goals

Do not implement these; each has a reason, and re-deciding them is waste.

- `colorProvider` — Perl has no color literals.
- `notebookDocumentSync` — no notebook story.
- `monikerProvider` — LSIF-era, niche.
- `inlineValueProvider` — needs a debug adapter we don't have.
- `documentOnTypeFormattingProvider` — widely disliked; fights perltidy.
- `declarationProvider` — the declaration/definition split barely exists in
  Perl; it would duplicate goto-def.
- `linkedEditingRangeProvider` — **deliberately off** (#117). Clients with
  linked edits on by default replay keystrokes into every returned range, so
  a mid-typed `$abel` live-renames a declaration whose `$a` prefix matched.
  Keep it off. The co-edit projection stays CLI-queryable via
  `--linked-editing`.

## Invariants for whoever implements this

- **Every verb is a CandidateSet projection or a Model query — never a new
  analysis path.** If a verb seems to need its own walk, that is the signal
  to stop: the answer already exists somewhere and the projection is missing.
  `docs/adr/resolution-candidate-set.md` owns that rule.
- **Counts must agree with `--heatmap` by construction**, i.e. by using the
  same `references()` projection rather than a parallel count. Two reference
  counts that can disagree is the parallel-store bug in a new costume.
- Advertise honestly: a verb we cannot answer well is worse than one we do
  not advertise, because the first is a wrong answer and the second is a
  known gap. The maturity labels in `--languages` carry the same rule.
