# ADR: The resolution CandidateSet — one semantic core, features as projections

The one resolution entry point is `resolve.rs::CandidateSet`; every
feature is a projection of it.

## Context: the recurring asymmetry disease

Five instances of the same bug wearing different costumes (verified on the
spike):

1. gd resolved kinds that gr couldn't mirror (the refs-symmetry audit's whole
   matrix: macros, enum variants, members, globals, typedefs).
2. Resolution was cross-file while completion *gathering* was same-file-only
   (the `OP_NULL` editor find).
3. The symmetry audit fixed gr on the **name** key but missed the
   **visibility** key — gd was closure-gated, gr wasn't (arc-review C1,
   CRITICAL: 85% noise on real queries).
4. Rename didn't follow what refs knew (arc-review C2, CRITICAL: silent
   partial edits).
5. Reachability ranking was computed but goto-def never consulted it (the
   win32-wins residual).

Root cause: **each LSP feature owns its own resolution path.** gd, gr,
rename, completion, hover, implementations share *data* (`FileAnalysis`,
`ModuleIndex`) but each independently re-composes the pipeline:
identify → gather → visibility-filter → rank → project. Every new **axis**
(include-closure visibility, language boundaries, delegation edges, macro
variants, family walks) is wired into the feature that motivated it, and
every other feature silently misses it. `ScopedLookup` is the smell made
visible: a *decorator* each entry point must remember to apply — C1 is
"the gr entry points didn't wrap." Symmetry is maintained by per-feature
diligence, N times per axis. Diligence always misses one.

## The proof the fix works: the tiers that already flow

The **witness bag** is single-sourced by decree ("production is push,
consumption is query through the registry, there is no second source") —
and the type tier has never had a cross-feature asymmetry. Same for
`parents_of` (one parent-enumeration seam: the app-surface edge injected
once, visible everywhere) and `cst.rs` (each grammar trap encoded once).
Single-seam tiers flow; N-path tiers leak. The resolution tier never got
its bag. `resolve_symbol`/`refs_to` (docs/adr/file-store-and-resolve.md)
was the right instinct — "the one entry point" — but it only unified the
identify step of references+rename. gd, completion gathering, and the
cross-cutting axes remained per-feature.

## Decision

One semantic core: **`resolve(files, origin, key, point, index, scope) →
CandidateSet`** — the canonical answer to "what does this name mean, from
here." The CandidateSet owns, computed once at the set level:

- **identity** — what the cursor resolves to (`resolve_symbol_scoped`'s
  Target / Group / Local verdict), minted exactly once,
- **visibility** — the RoleMask verdict (`references_mask_for`) memoized on
  the set, with a construction-time override (`with_visibility`) that every
  projection inherits — the plug point for future axes (closure/import
  gating, language boundaries), never an entry-point decorator,
- **edges** — the override family / dispatch chain on `TargetRef.method_classes`,
  group members with per-member rename rules, and the descendants walk
  (`implementations_of` over `GraphView`); each projection declares which
  edges it follows,
- **declaration** — a projection group carries `decl_spans` (the `has` /
  column token, the `field` decl) beside its spellings, each tagged with
  the file it was minted from; goto-def projects it rather than re-deriving
  where an inherited attr was declared,
- **per-site policy** — `RefLocation.rewritable`, `MemberRename` texts:
  policy rides the candidates, handlers never re-derive it.

Every feature is a **projection** of the same CandidateSet:

| feature | projection |
|---|---|
| references | `references()` — the backward image of the set |
| rename | `rename_edits(new)` — references + rewritability policy; an edit outside the references image is unrepresentable |
| prepareRename | `renameable()` — mirrors `rename_edits`' arms |
| implementations | `implementations()` — the family/descendants walk; every location it returns is a Declaration-access site (`projection_consistency_tests` I7 holds this at every corpus cursor — the sentence was measured before it was promised) |
| goto-def | `definitions()` — forward-best projection, backstopped by the identity's declaration axis: when every forward lane misses and the cursor resolved to a group, the group's `decl_spans` answer, so goto-def cannot come up empty where `references()` names a declaration |
| completion gathering | `complete(prefix, import_slot)` — prefix-enumeration of the visible identifier universe (Perl: in-scope + explicit imports on OPEN, export surfaces + auto-import firehose on DEPENDENCY; pack: the origin's include-closure universe); `complete_modules(prefix)` — the loadable-module half (DEPENDENCY). `import_slot` is the slot's import affordance: `false` = the slot offers no import-sourced names (an import candidate without a place for its edit completes to broken code); candidates carry `ImportFact`, the adapter composes the edit |
| hover | the set resolves for BOTH languages; each keeps its presenter. Pack: `hover_candidate()` — the top-ranked `definitions()` candidate, presented. Perl: the model hover primitives render local identity, and the cross-file call lanes present `function_binding()` — the same import-classification / FQ-package lanes `definitions()` jumps through — so hover and goto-def cannot disagree at a position |
| documentHighlight | `highlights()` — the origin-file-narrowed image of `references()` (same identity, same matcher via `refs_to_in_file`, walk never leaves the cursor's document), carrying `RefLocation.access` for highlight kinds |
| linkedEditingRange | `linked_editing_spans()` — the co-edit set: `highlights()` restricted to sites a rename writes the typed text at VERBATIM (`rewritable`, bare-text group members only — affix-derived accessors join references but can't co-edit one text), so it equals the rename image's site set by construction |

Symmetry becomes **by construction**: an axis added to CandidateSet
construction is inherited by every projection — the test
`candidate_set_visibility_axis_flows_to_every_projection` demonstrates the
one-knob property, asserting references, rename, highlights, linked
editing, AND completion gathering narrow together. C1 ("gd gated, gr not") and C2 ("rename edits a subset
of refs") become unrepresentable states, and disease #2's class
("resolution cross-file, completion gathering same-file") is closed: the
identifier candidates come from the same masked universe the navigation
verbs walk. The audit's gold *pairs* remain as the verification net —
pairs verify, the seam prevents.

## Completion: sources on the set, and the honest boundary

The CandidateSet owns the candidate **sources** — where names come
from — never the slot logic. Sources on the set: in-scope names
(`complete_general`, OPEN), explicitly imported names (origin's `use`
lists, OPEN — the dep cache only enriches detail), imported modules'
remaining export surfaces and the unimported auto-import firehose
(DEPENDENCY), and loadable module names (`complete_modules`, DEPENDENCY —
resolved cache + @INC availability behind
`CrossFileLookup::complete_module_names`). The qualified-path drill takes
both of its sub-package sources from the set.

Deliberately NOT on the set (the seam's edge, kept honest):

- **Cursor-context slot detection** (method position / hash key / variable
  sigil / import list / dispatch arg-0) — decides WHICH slot the cursor is
  in, never where names come from. Stays in `cursor_context`/the adapter.
- **Entity-content gathering**: methods on a resolved class
  (`complete_methods_for_class`), hash keys for a resolved owner, dispatch
  handler names, keyval/`:param` keys, the `use Foo qw(|)` import-list
  slot (one named module's export surface). These enumerate the content OF
  an already-identified entity and ride the method/dispatch resolution
  seams (`PackageSymbol`, `ReceiverGated`) — a different question than
  "what names are visible from here." Folding them in would re-derive
  those seams, not unify them.
- **Presentation**: labels, kinds, details, sort priorities, snippet
  items, and where the auto-import `use` edit lands (`auto_import_span` —
  needs the LSP-side stable outline). Policy still rides the candidates
  (`additional_edits`), placement is the adapter's.
- **Plugin query hooks** — cursor-time, imperative, plugin-owned.
- **Tiers with no source**: WORKSPACE contributes no completion names
  (true before the seam too — workspace-package names and workspace
  exporter surfaces were never gathered); when it grows a source it plugs
  into the same mask — that's the point of the seam. BUILTIN has one: the
  Perl builtin surface (`model/builtins.rs`, the single table diagnostics
  suppression and builtin hover also ask) supplies its `Function` rows as
  completion candidates in `complete()`, Perl origins only (the pack arm
  never reaches it).

## Structure

- The set lives in `resolve.rs`, extending the existing
  `resolve_symbol`/`refs_to` seam — not a parallel module. `refs_to`,
  `group_refs`, `references_mask_for` are now the set's internals; handlers
  and CLI mirrors construct the set and project.
- Completion constructs the same set (`completion_items` → `resolve` →
  `complete`/`complete_modules`); identity minting stays lazy, so slots
  that never consult `resolution()` don't pay the override-family walk.
  Completion has no resolved target for `references_mask_for` to judge, so
  its default visibility is the full VISIBLE universe; the construction
  override narrows it like every other projection.
- Projections only READ the stores (`FileStore::for_each_open`), so an LSP
  handler may hold its open-doc guard across a projection — the old
  `drop(doc)`-before-walking discipline (a deadlock trap) is gone.
- Each projection reproduces the exact composition of the verb it serves.
  One documented asymmetry holds: group rename does not consult `rewritable`
  while target rename does. `definitions()` returns the never-pruned ranked
  multi-set for macro-named words (the ranking axis below); other lanes keep
  first-winning-path.

## The cpp axes on the seam

The spike's axes live in CandidateSet construction:

- **closure visibility** — `resolve()` builds the per-origin
  `ScopedLookup` (origin path + `include_closure`) once; identity
  minting, goto-def, and implementations read through it, and the
  backward walk driver (`refs_to`) applies the target's `def_paths`
  connectivity gate per scanned file (`file_sees_target`) before the
  matcher runs — no entry point re-applies a decorator, and every
  projection that walks inherits the gate.
- **pack routing** — a construction fact: `resolve()` derives it from the
  origin's stamped `FileAnalysis.language` (`is_pack_language`), so no
  handler declares or can forget it; store selection has its own single
  speller (`ModuleIndex::lookup_for`, tripwired out of the LSP layer).
  The set owns the consequences: visibility widens to VISIBLE (pack
  workspace files ride the DEPENDENCY role), and `rename_edits` →
  `Result` REFUSES on alias-spelled sites (full-or-refuse) instead of
  silently skipping.
- **delegation / `Specializes` / domain edges** — consumers declare
  traversal: references walks delegation aliases (never the domain
  bridge); `implementations()` walks `Specializes` families and the
  enum-def → field-slot domain bridge; goto-def sees through direct
  delegation on the top-ranked macro variant.
- **macro variants / multi-def + ranking** — `with_source` feeds the
  raw-word lane; a pack `definitions()` answers a macro-named word with
  EVERY def site, reachability-ranked config-active first (the total
  order, per candidate); `RefLocation.label` carries the per-candidate
  fact (reachability verdict / see-through note) — the LSP adapter drops
  it, the CLI renders it.
- **decl→def ranking** — a pack `definitions()` answer landing on a
  bodiless declaration (a prototype, an `extern` variable decl) ranks the
  bodied definition(s) of the same identity first, decl kept (never
  pruned). Definition-ness is a symbol-borne fact (a callable's body mints
  a scope spanning it; a variable carries its `extern` storage class as an
  attribute), and cross-file identity rides the def-candidates table +
  closure connectivity — forward (origin reaches the defining file) and
  reverse (the defining TU includes the origin header). Hover inherits it:
  `hover_candidate()` presents the ranked winner.
- **owner-match ranking** — `overload_arity_definitions` ranks a
  candidate whose package genuinely agrees with the call's anchored owner
  (both sides carry a package, tails agree) above one admitted only by
  `pkg_agrees`' recall bias (a package-`None` free function). The family
  is never pruned — the free decl stays in the set, ranked below. This is
  what makes a sibling method call (`resolved_package` pinned to the
  class) win over a same-named free function without a member-vs-free
  branch; the owner match is the same key serving `dynamic::STRING` /
  `logger.info` / `level::info`.
- **Function-lane visibility gate** — a plain Sub target minted from a
  pack-routed set carries closure-keyed `def_paths` (minted in
  `resolution()`, on the routing fact — a Perl Sub cursor is the same
  `RenameKind` shape but is package-keyed, so it stays ungated). The
  backward gate has a textual-inclusion extension: a scanned file whose
  own closure misses every def path still passes when a direct seer
  includes it (redis's `ae.c → #include "ae_epoll.c"` fragments).
- **completion** — the pack instance of `complete(prefix)`: the origin's
  include closure is the identifier universe
  (`CrossFileLookup::visible_defs_with_prefix`, no global fallback).
- **name semantics** — the set's `bare_new_name` hook: Perl sigil rules
  live in `conventions.rs::strip_variable_sigils`; pack spellings
  canonicalize at extraction (the LangPack `shape_name` hook, cpp's
  `canonical_template_spelling`).
- **import facts, not edits** — import-sourced completion candidates
  carry `ImportFact` (`AddToQw`/`NewUse`); the adapter composes fact +
  slot affordance (`complete(prefix, import_slot: bool)`) into the edit.

The invariant test has a per-language instance:
`candidate_set_visibility_axis_flows_to_every_projection` (Perl,
RoleMask knob) and
`closure_visibility_axis_flows_to_every_cpp_projection` (cpp, the
closure fact) each turn ONE construction knob and assert gd, gr, rename,
and completion gathering move together.

## Consequences

- New-axis review question shrinks from "did every feature get it?" to
  "is it in CandidateSet construction?"
- The gold pairs + e2e are the verification net for changes to `resolve.rs`
  (a hot path); the full net must stay green.
