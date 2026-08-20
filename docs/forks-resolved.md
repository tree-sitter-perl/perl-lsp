# Resolved architectural forks — ledger of record

Historical record of forks logged in `docs/open-forks.md` and since
ratified, resolved, or closed. Entries move here VERBATIM when their
status leaves OPEN; the live discussion queue stays in
`docs/open-forks.md`, deferred work extracted from these entries lives
in `docs/prompt-storage-residuals.md`. Load-bearing decisions are also
stated in their owning ADRs — this file is provenance (who picked what,
when, why), not the contract.

---

## Invocant class vs inner-scope rep narrowing — 2026-07-15 — RESOLVED (veesh, 2026-07-15)
- **Context:** edit-bench finding (`bench/RESULTS.md`): `$self->` completion
  returned a lone `{key} hash dereference` inside `use base` classes
  (Bugzilla::Bug) instead of the class's methods. Root cause was NOT that
  the invocant fails to type — a conventional invocant already types as
  `ClassName`/`FirstParam` (via `detect_first_param_type`, independent of
  framework). The bug: `$self->{field}` accesses inside a nested block push
  rep witnesses (`HashRefAccess` observation + an `infer_deref_type`
  `HashRef` TC) on the *inner* scope, while the invocant's class lives on the
  sub scope. The scope-chain walk (`query_variable_with_visited`) returned
  the innermost typed scope first → `HashRef`, masking the class.
- **Options:** A — always assert every conventional invocant scalar
  (`$self`/`$class`) as `ClassName(current_package)` via a witness
  (aggressive; also masks a genuinely hashref-typed `$self` and needs a
  sub-Builder confidence tier to lose to real bindings). B — framework-gate
  the rep suppression (wrong — the bug is framework-independent). C —
  identity-over-rep across the scope walk: class identity anywhere in the
  chain dominates an inner scope whose answer is a *rep-observation-only*
  projection, and `subsumes_narrowing` stops `infer_deref_type` re-deriving
  a rep `HashRef` over an existing class identity at the access site.
- **Picked:** C. Two small, reversible seams: (1) `subsumes_narrowing`'s
  class-over-rep arm; (2) `query_variable_with_visited`'s weak-answer
  deferral (`scope_binds_variable` gate). Genuine inner-scope rebindings
  (`my $h = {…}`) stay authoritative because they BIND the variable.
- **Resolution:** the fix landed and the one open residual — `my $self =
  $class->new(...)` through a cross-file base ctor staying untyped — was
  autopsied and turned out to be a plain BUG, not a fork: the
  receiver-polymorphic machinery (`ReturnExpr::ReceiverOr`, the
  `PackageSymbol` chase, receiver threading) existed and worked end-to-end
  for value-position bless, but the statement/assignment bless forms
  (`bless($object, $class); return $object` — the Bugzilla::Object idiom)
  never minted the deferred witness, and the Variable hop dropped the
  receiver. Fixed at the mechanism (`push_receiver_bless_witness`, the
  `ReturnExprReducer` Variable claim with temporal gate, receiver threading
  through `query_variable_with_visited`); option A's blanket `$self`
  assertion was never needed. Verified: 4 unit tests, 2 substrate gold rows
  (RobotUA / MultiPartParser post-bless hover), and the real Bugzilla case —
  goto-def on `$self->id` after `my $self = $class->new($param)` in
  `Bug::check` picks `Bugzilla::Object::id` over five same-named decoys,
  through two polymorphic hops cross-file.

## Freshness engine: hand-rolled reverse-dep vs Salsa — 2026-07-06 — RATIFIED (veesh, 2026-07-07)
- **Context:** storage-engine mission phase 3 (docs/adr/storage-engine.md;
  eval on claude/salsa-incremental-eval-1bmv23). The Surface boundary makes
  the engine choice reversible.
- **Options:** A — hand-rolled `FreshnessIndex` (surface records +
  name-keyed reverse-dep + seen-set dirty walk, ~150 lines, no deps).
  B — Salsa 0.27 (revision machinery, durability, auto edges; `'db`
  virality, memory-tuning burden, pre-1.0). C — comemo (lighter, no
  revision machinery).
- **Picked:** A, per the eval's own recommendation: we already own the
  reverse-index discipline, and the engine sits entirely behind
  `Surface`/`SurfaceVerdict` — swapping in Salsa later touches the
  recording sites, not the consumers.
- **Undo cost:** moderate — reimplement `record`/`dirty_consumers` over
  salsa inputs/tracked fns; call sites unchanged.
- **Discussion needed:** when the query graph deepens (materialized
  workspace enrichment, phase 4 SQL views), revisit whether the dirty-set
  still suffices or Salsa's cancellation/durability earns its costs.

## Unregister inverse under symbol eviction: recorded name list vs self-healing candidates — 2026-07-06 — RATIFIED (veesh, 2026-07-07)
- **Context:** symbols-relational phase B. `unregister_file` walked
  `old.analysis.symbols` to remove name registrations; under symbol
  eviction that vec is empty, and rehydrating after an edit persists
  fetches the NEW generation's names (wrong inverse).
- **Options:** A — record the registered (name, is-class) pairs per path
  at registration time (a side map, ~pointers + names). B — make
  `all_defs`/`cache` self-healing at read (validate candidates against
  `all_files` membership/Arc identity; registration appends only). C —
  read the syms rows for the OLD generation before shredding the new one
  (couples unregister ordering to persistence).
- **Picked:** A (`ModuleIndex.registered_names`). Exact inverse by
  construction, no read-path cost, and it doubles as the class-rank
  source for the cache-slot tie-break (which also read evicted symbols).
  Cost: one Vec<(String, bool)> per registered file — ~tens of MB at
  chromium scale, bounded and measured into the floor.
- **Undo cost:** small — the map is private to `module_index.rs`; B can
  replace it without touching call sites outside registration/unregister.
- **Discussion needed:** if the floor budget tightens further, B removes
  the per-file name copies entirely at the price of lazy consistency
  (stale candidates until next read).

---

## include_closure representation: interned path pointers vs file-id arrays — 2026-07-06 — RATIFIED (veesh, 2026-07-07)
- **Context:** chromium warm heap dump post-refs/symbols eviction:
  `include_closure` is the largest resident bucket — 2,827 MB / 41% at
  132K files (16-byte `Arc<str>` per closure entry × deep header
  closures). It is read on hot paths (the refs_to visibility gate, per
  candidate) so relocation to rows would thrash; this is a REPRESENTATION
  problem.
- **Options:** A — global path table + per-file sorted `Arc<[u32]>`
  (4 bytes/entry, ~4× smaller; gate becomes a binary search over ids).
  B — dedupe identical closure SETS across files (headers within one
  subtree share suffixes; measure hit rate first). C — roaring bitmaps
  over the global file table (best compression, new dep).
- **Picked:** A, landed same day (`path_intern::ClosureList` — sorted
  `Arc<[u32]>` over a process-global path-id table; serde keeps the
  `Vec<String>` blob shape so no EXTRACT_VERSION bump). Membership became
  id binary-search (`contains`), set/save consumers go through
  `iter_strs`. One subtlety worth remembering: `closure_stamp` had to
  SORT before hashing — id order is global mint order, nondeterministic
  across sessions, and an order-sensitive hash would have silently
  invalidated every warm row every run. Measured on abseil: closure
  bucket 10.8 → 1.8 MB (residual is `include_directives` strings), whole
  payload 20.2 → 11.2 MB; table 1,123 paths / 0.1 MB. Chromium
  projection: 2,827 MB → roughly 300–500 MB + a ~150 MB one-time table.
- **Undo cost:** small — representation is private to `ClosureList`;
  swapping to dedup'd sets (B) or bitmaps (C) touches only the type.
- **Discussion needed:** whether `include_directives` (now the residual)
  should ride the same table; and whether B (closure-set dedup) stacks
  worthwhile savings on top at chromium depth.

## Implicit-`this` capability: one flag for fields AND calls — 2026-07-05 — RATIFIED + RENAME LANDED (veesh, 2026-07-05)
- **Context:** hitlist-3 Family A+I slice. The implicit-member pass is
  gated by the pack's `implicit_this_members` capability. The sibling-CALL
  half (a bare `foo()` inside a method body meaning `this->foo()`) needed a
  gate too — same fork the task flagged: reuse the flag, or add a sibling
  one.
- **Options:** A — reuse the one capability for both halves. B — add a
  parallel `implicit_method_calls` capability.
- **Picked:** A. "Can a bare name resolve through an implicit `this->`" is a
  SINGLE language fact — C/C++ elide the receiver for both members and
  methods; Python/R make it mandatory for both. There is no language where
  fields elide but methods don't (or vice-versa), so a second flag would be
  a distinction with no possible producer. The flag's NAME is now
  field-specific and slightly under-describes its scope; the rename to
  `implicit_this_members` (member-scoped, covers fields AND sibling calls)
  landed in the round-close sweep.
- **Undo cost:** trivial — split into two bools and thread the second
  through `emit_return_fuel`; the sibling-call pass already stands alone as
  its own block, so it just reads a different flag.
- **Ratification (veesh):** one flag for both, confirmed; the rename to a
  member-scoped name landed (`implicit_this_members`).

## Sibling-call vs. same-named free function ranking — 2026-07-05 — RESOLVED (Family Q, 2026-07-05)
- **Context:** same slice. When a method body calls `foo()` and BOTH a
  sibling method `Class::foo` and a free `foo()` exist, C++ name lookup says
  the member hides the free function. The model tier correctly MINTS the
  sibling link (pins the call's `resolved_package` to the class, so
  `find_definition` lands on the member). But goto-def's set projection runs
  through `overload_arity_definitions` in `resolve.rs`, whose `pkg_agrees`
  admits a package-less (free) function into a class-scoped overload family
  (`_ => true`), so the free decl still surfaces — and its earlier source
  row sorts it FIRST.
- **Options:** A — leave it: the sibling link is present, the ranking
  residual is a resolve.rs overload-family concern. B — teach
  `overload_arity` that a member call (pinned `resolved_package`, class
  origin) excludes package-less free functions from the family.
- **Picked:** B, landed by the Family Q slice that owns `resolve.rs`. This
  ranking residual is the exact symptom-2 of Family Q (owner/qualifier-blind
  forward resolution): `overload_arity_definitions` now ranks candidates by
  **owner match** first — a candidate whose package genuinely agrees with the
  call's anchored owner (both sides carry a package, tails agree) sorts above
  one admitted only by `pkg_agrees`' recall bias (a package-`None` free
  function). The family is never pruned: the free decl stays in the set,
  ranked below the sibling member. The pinned `resolved_package` = the class
  origin makes the sibling member owner-matched, so it wins. General rule (no
  member-vs-free branch): the owner match IS the key, shared with the
  `dynamic::STRING` / `logger.info` / `level::info` cases.
- **Outcome:** `cpp-sibling-call-shadows-free` promoted PROVISIONAL → gold,
  now asserting the sibling member ranks FIRST (`none: ["\nsibling_call.cpp:25:10"]`).

## Hover presentation payload — 2026-07-03 — RATIFIED (veesh, 2026-07-03)
- **Context:** hitlist-2 slice D (#14): hover became a CandidateSet
  projection (`hover_candidate()` = the top-ranked `definitions()`
  candidate; `symbols::pack_hover_markdown` presents it).
- **Options:** A — the projection returns a bare `RefLocation`; the adapter
  materializes (file → analysis → symbol at span) and renders (member
  drill-downs stay a cursor-side adapter lane over the same invocant
  resolution). B — candidates carry a presentation payload (symbol
  identity, kind, member facts) minted inside `definitions()`, so the
  adapter never re-looks-up.
- **Picked:** A. No widening of `RefLocation` for one consumer, zero new
  serialized shapes; identity/ranking stays single-sourced (the invariant
  that matters) while presentation lookups read through the same scoped
  index the set resolved with. The member drill-down (domain headline /
  storage leaf / template substitution) keeps its landed adapter-side home.
- **Undo cost:** small — introduce a `HoverCandidate` payload struct and
  move the adapter's symbol-at-span lookup into `definitions()`'s lanes;
  the adapter shrinks to a pure renderer.
- **Discussion needed:** if a second presentation consumer appears (e.g.
  CLI gd wanting signatures beside locations), promote to B then.

## Function-lane def_paths minted at the set, not identity minting — 2026-07-03 — RATIFIED (veesh, 2026-07-03)
- **Context:** slice D3 re-activated the def-candidates visibility gate for
  plain function (Sub) targets. Every other def_paths mint sits in
  `resolve_symbol_scoped` behind a structural pack-only fact (macro_defs,
  sigil-less class content); a Sub cursor is language-neutral (a Perl `sub`
  mints the same `RenameKind::Function`), so minting there would gate Perl
  subs — whose visibility is package-keyed, never closure-keyed — off their
  own workspaces (Perl closures are empty).
- **Options:** A — mint in `CandidateSet::resolution()` under the
  caller-declared `pack_routed()` fact. B — add a language/pack tag to
  `FileAnalysis` and gate inside `from_rename_kind`.
- **Picked:** A. The ADR already blesses pack routing as a set-level axis
  with set-owned consequences (VISIBLE widening, rename full-or-refuse);
  the visibility gate is precisely such a consequence, and A adds no
  persisted field.
- **Undo cost:** trivial — move ~10 lines if a `FileAnalysis` language tag
  ever lands for other reasons.
- **Discussion needed:** none urgent; fold into B if/when a language tag
  exists.
- **2026-08-04:** the language tag landed (rework C1): `pack` is derived
  from `FileAnalysis.language` at set construction and `pack_routed()` is
  gone; the Sub-lane def_paths mint stays at the set, now gated by the
  construction-derived fact.

## `Slot::ModulePath`'s `in_use` field — 2026-07-05 — RATIFIED + GENERALIZED (veesh, 2026-07-05)
- **Context:** cursor Slot taxonomy (`docs/adr/cursor-slots.md`), migrating
  completion's context match onto `Slot`. The ADR sketches `ModulePath {
  prefix: String }` covering BOTH `use |` (typing the module name —
  `complete_module_names`, loadable-module labels) and `Foo::|` (an
  in-file qualified-path drill — `qualified_path_completions`, sub +
  sub-package labels). The two behaviors are genuinely different renders
  over the same CandidateSet (full module name vs. bare suffix), and
  `prefix`'s text alone can't distinguish them — `Mojo::Ut` is a valid
  partial spelling under either. Folding them into one slot with only
  `prefix` and picking ONE render would silently change completion output
  for whichever case lost, breaking the migration's byte-identical
  requirement.
- **Options:** A — add `in_use: bool` to `ModulePath`, set at detection
  time from which `CursorContext` arm fired (`UseStatement` vs
  `QualifiedPath`); the consumer's `if in_use {..} else {..}` exactly
  reconstructs today's two code paths. B — split into two Slot variants
  (`ModulePath` for the drill, a new `UseModule` for the bare `use` case),
  matching the ADR's 7-variant count more loosely.
- **Picked:** A. Keeps the ADR's closed 7-variant vocabulary intact (the
  field is additive, not a new variant), stays a straight decode of which
  detector fired — no shape re-derivation from tree/text — and the two
  render functions (`complete_module_names` / `qualified_path_completions`)
  are untouched, called exactly as before.
- **Undo cost:** trivial — drop the field and hardcode one render, or
  promote to option B (split variant) if a future consumer wants to
  match on it structurally instead of a bool.
- **Discussion needed:** none urgent; the field is documented at its
  definition (`src/lsp/cursor_slot.rs`) and locked by
  `cursor_slot_tests.rs::detect_slot_perl_use_module_name_is_module_path`.
  (The two renders now live set-side as
  `CandidateSet::complete_module_candidates` / `complete_qualified_path`.)
- **Ratification (veesh):** keep, but GENERALIZE — done in the round-close
  sweep. `in_use` is gone; every detected slot now carries a
  `DetectorArm` (`cursor_slot.rs`) — the generic "which detector fired"
  fact. `detect_slot`/`detect_call_slot` return `DetectedSlot { slot, arm }`;
  the `ModulePath` consumer asks `arm == UseModule` (module-name render) vs
  `QualifiedPath` (drill), no per-variant bool. Any future folded-slot
  consumer reads the same arm.

## Ref-type deref snippets — candidate data vs projection policy — 2026-07-05 — RATIFIED (veesh, 2026-07-05)
- **Context:** the entity-content candidate-level migration (PARKED
  "Entity-content completion sources"). Every entity-content source now
  yields `CompletionCandidate` through one adapter projection
  (`candidate_to_completion_item`). Two Member-slot extras don't fit the
  candidate mould: the pack `.`→`->` operator-swap edit (`op_fix`) and the
  Perl ref-type deref snippets (`[index]` / `{key}` / `(args)` offered when
  the `->` receiver is an ArrayRef/HashRef/CodeRef).
- **Options:** A — keep them as they are: `op_fix` rides the existing
  `CompletionCandidate.additional_edits` (candidate DATA — the receiver's
  pointer depth is a fact about the member candidate), and the ref snippets
  stay adapter-appended `CompletionItem`s (projection POLICY — they are
  syntactic templates for a ref receiver, not members of any entity, and
  need `InsertTextFormat::SNIPPET` which the candidate vocabulary doesn't
  model). B — add a snippet-format field to `CompletionCandidate` so the
  ref snippets become candidates too, folding the last Member extra into
  the vocabulary.
- **Picked:** A. `op_fix` was already candidate data and stays there. The
  ref snippets are a fixed 1-item-per-ref-kind template with no gathering
  to unify — making them candidates would add a SNIPPET-only field to the
  struct that every other candidate carries as `None`, bloat for zero
  dedup/provenance benefit. They're the same shape as the import-list
  "still indexing" placeholder: a slot affordance, not a resolved entity,
  so the adapter builds them directly.
- **Undo cost:** trivial — add the snippet field + move
  `ref_type_snippet_completions` into a gatherer if a second snippet source
  ever appears; today there's exactly one.
- **Discussion needed:** none urgent; revisit only if type-constrained
  completion wants snippet candidates from a shared source.
- **Ratification (veesh):** punt is fine — snippets likely push into the
  candidate vocabulary eventually, but blast radius is low; the standing
  condition is that layering rules hold (snippets stay adapter-side, no
  analysis decisions leak into the projection).

## Type-constrained completion — carried expected type vs new Slot variant — 2026-07-05 — RATIFIED (veesh, 2026-07-05)
- **Context:** the type-constrained-completion slice needed a slot for the
  pack domain comparison (`o->op_type == |` → the field's enum DOMAIN). The
  `Slot::expected_type` seam already existed; the question was how the pack
  detector hands its EAGERLY-resolved domain type to that seam.
- **Options:** A — reuse `ArgPosition`, adding an `expected: Option<InferredType>`
  field the detector fills when it already knows the type (Perl call-arg
  slots leave it `None` and resolve the callee's param lazily). B — mint a
  new `Slot::Comparison { expected }` variant.
- **Picked:** A. The ADR already grouped `x == |` under `ArgPosition`
  ("wants sig-help AND type-constrained candidates. Carries the slot's
  EXPECTED TYPE when derivable"), so the field is the shape the doc
  reserved; a comparison and a call-arg answer the same `expected_type`
  question with the same consumer. A new variant would fork the vocabulary
  for one producer with no distinct consumer. `Slot` is ephemeral
  (no serde), so no EXTRACT_VERSION cost either way.
- **Undo cost:** trivial — the field defaults conceptually to `None`; drop
  it and re-inline if a comparison ever needs consumer behavior a call-arg
  doesn't share.
- **Also parked here:** switch-`case |:` domain completion (the ADR's
  "if cheap" half) — SKIPPED. It needs a distinct probe (climb to the
  `switch_statement`, resolve the CONDITION field's domain) rather than the
  `==`/`!=` binary the landed probe reads; not cheap enough to fold in now.
- **Perl ranking tier:** the ArgPosition consumer boosts type-matching
  scope vars by keeping them at `PRIORITY_LOCAL` and nudging the non-matching
  locals they lead to `PRIORITY_LOCAL + 1` (0 is the priority floor, so a
  sub-LOCAL tier isn't expressible; demoting the complement is the minimal
  sort_text-visible reorder). Revisit if a second sub-LOCAL ranking axis
  appears and the two need a shared ordering.
- **Ratification (veesh):** leave as is — "ArgPosition is a drop of a
  lie, but Slot::Comparison would be sprawl." A friendly comment on the
  variant acknowledges the stretch (landed in `src/lsp/cursor_slot.rs`).

## Warm stubs — separate table vs. blob column — 2026-07-06 — RATIFIED (veesh, 2026-07-07)

- **Context:** register-from-Surface warm start persists a per-file stub
  (registration feed + specialization edges + projected Surface + the
  stripped skeleton) so warm scans never decode full analysis blobs.
  Where does the stub live?
- **Options:** A — `ALTER TABLE modules ADD COLUMN stub BLOB` (the
  `deps_stamp` precedent). B — a separate `stubs(path PRIMARY KEY, stub)`
  table joined at warm.
- **Picked:** B. SQLite reads a record left-to-right; a column appended
  AFTER the `analysis` BLOB sits past its overflow-page chain, so every
  stub read would drag the full blob off disk — the exact cost the lane
  exists to skip. The separate table also gets its own `stub_version`
  meta wipe without touching blob validity.
- **Undo cost:** low — the stub is a pure cache derived from the blob in
  the same txn; dropping the table (or the version gate wiping it)
  degrades to the full-decode lane and backfills on the next warm.
- **Tradeoffs accepted:** (a) every modules-row rewrite must DELETE the
  path's stub or a stale skeleton pairs with a fresh stamp — enforced
  inside `save_to_db`/`save_blob_to_db_stamped` so writers can't forget;
  (b) `pack_file_changed` deletes but doesn't rewrite stubs, so edited
  files take the full lane on the next warm (bounded: only edited files);
  (c) NO_EVICT bypasses stubs entirely (skeletons are stripped by
  construction — a whole-copy session can't register them);
  (d) stub-lane files whose derived rows vanished (REF_ROWS_VERSION
  wipe) decline to the full lane because re-shredding needs the whole
  analysis.
- **Discussion needed:** whether the Perl workspace tier should get the
  same lane (its warm is milliseconds on bugzilla today; chromium-scale
  Perl trees don't exist in the corpus set).

## Mission-2 hardening round — 2026-07-06 — CLOSED (fixed record; residuals extracted)

Fixed in the round: free callables were absent from the
Surface (a C free-function signature change read as Unchanged — the
firewall's worst failure mode); the dirty walk missed consumers of
RENAMED-away packages (`stale_provided` now seeds one walk); the deleted
pack file's own reparse caches leaked (deletion was semantically
invisible to re-analyzed consumers); open-doc surfaces were recorded
POST-enrichment (verdict flapping against pre-enrichment indexer
records); the Unchanged gate skipped consumer deps-stamp refresh (the
restart cold storm); watcher re-registration dropped the verdict (stale
open consumers after git pull); stub-lane rows with an unrehydratable
blob (NULL/empty) no longer register; deferred stub backfill is
stamp-guarded against racing edits; `remove_surface` parent-canonicalizes
deleted paths; method dedup collapses only FULLY-equal duplicates.

Landed follow-ups from the round's deferred list:

- **Concurrent surface writers (buffer vs disk)** — LANDED 2026-07-12.
  Record provenance (`SurfaceWrite::{OpenDoc, Background}`) at the one
  freshness write (`record_surface_write`): while a doc is open (didOpen
  marks, didClose clears), background writes on its path are suppressed —
  consumers read the buffer, so the baseline must track the buffer.
  didClose reconciles: re-records the indexed DISK copy (whole view) and
  republishes whoever the flip dirtied, so a buffer that dies with unsaved
  contract changes can't leave consumers enriched against a ghost. didOpen
  now records too (catches open-after-external-change). Perl hub only —
  pack languages have no open-doc surface recorder yet; guarding their
  background writes would freeze records staleward (residual in
  `docs/prompt-storage-residuals.md`).
- **Verdict-policy seam** — LANDED 2026-07-12.
  `ModuleIndex::record_and_dirty(path, fa) -> SurfaceDirty {verdict, dirty}`
  binds record → verdict → dirty-consumers in one seam; the open-doc editor
  path and the watcher (via `register_workspace_resident`, which routes
  through it) both consume it, so a caller can't record a surface without
  the consumer answer. The ACT arms stay separate (open-doc republish vs
  watcher batch). `pack_file_changed` is NOT forced through it: the pack
  tier discovers consumers by include-closure, a genuinely different axis,
  so it keeps `record_surface` (verdict only) — the honest residual.
- **Declined micro-optimizations**: per-registration `fs::canonicalize`
  in `record_surface_value` stays (correctness guard; ~µs against per-file
  analysis costs); cold-path stub encoding stays (measured cold wall
  unchanged, and it buys the FIRST warm, not the second).

Still-deferred items (probe serialization, pack provided-names
vocabulary): `docs/prompt-storage-residuals.md`.

## Session review round — duplication + structural residency — 2026-07-07 — TRIAGED (veesh, 2026-07-07)

Landed: `evict_axes` / `prepare_pack_parts` / `prepare_workspace_parts` are
the only spellers of the reads-whole-before-evict strip; the warm scans
share `classify_row_generation` + `write_in_chunks` + a single-callback
`WarmPayload` API (RefCell dance gone; dead rows rejected pre-decode);
`register_symbols` feeds through `prepare_pack_feed`; the watcher and
editor paths share `republish_open_docs_in`; the writers' PANIC arms now
drop stale LRU pins like their commit-fail arms (live bug, both tiers);
the enrichment overlay is byte-capped (128 MiB + 64 entries, per-entry
`heap_estimate` stored); `whole_copy_registration_sites_are_allowlisted`
(layering_tests) makes every whole-copy registration call site declare its
residency bound; a post-bulk-index residency tripwire counts
fully-resident pack copies against the deliberate whole-copy sites and
`log::error` + debug-asserts on unexplained pins.

Landed follow-ups from the round's deferred list:

- **@INC/'import' tier stripping** — LANDED 2026-07-07 (see
  "@INC stripping arc — closed" below): cold-path strip via
  `strip_import_copy`, warm strip at insert inside `warm_cache` for
  long-lived processes. Residuals: CLI one-shot keeps whole warm copies
  (deliberate — wall over RAM there), registration-generation keys for
  the tier (landed 2026-07-12, see the R4 entry).
- **Writer fallback budget** — LANDED 2026-07-12. `FALLBACK_WHOLE_BYTE_CAP`
  (128 MiB, byte-accounted via `FileAnalysis::heap_estimate` like the
  enrichment overlay) bounds the whole copies each persist writer retains
  on commit-fail/panic. Past the cap the fallback DROPS the resident copy
  (does not register a stripped one — the chunk didn't commit, so a
  stripped copy's blob isn't on disk and could only rehydrate to
  wrong-empty): honest absence, re-indexed next run, never wrong data,
  never an unrehydratable evicted copy. Under-cap pack fallbacks stay
  tripwire-accounted (`expected_whole`). Residual: the budget is
  per-writer-thread (per pack language / the workspace writer), not a
  single index-wide atomic — bounds total to (writers × 128 MiB), fine at
  the 1-2 pack languages the corpus has.
- **Parts-token-only inner registration** — LANDED 2026-07-12.
  `register_symbols_inner` / `register_workspace_residency` now consume a
  `PackRegistrationParts` / `WorkspaceRegistrationParts` token whose fields
  are private and minted only by the choke points in module_index.rs
  (`prepare_pack_parts` / `prepare_workspace_parts`, plus
  `PackRegistrationParts::whole` for the deliberate whole-copy front door
  and `from_warm_stub` for a persisted token). Constructing the argument is
  the compile-time proof of reads-whole-before-evict; the writer channels
  carry the token instead of raw pieces. Allowlist counts unchanged (a
  type-level change, not a call-site collapse); the allowlist test still
  polices the whole-copy front doors.

Still-deferred items with designs (watcher re-registration re-strip,
rows-missing re-strip after backfill, writer-thread harness dedup,
stamp-capture helper): `docs/prompt-storage-residuals.md`.

## Triage (veesh, 2026-07-07)

- The four architecture picks above: **ratified as-is**, revisit triggers
  stand as written.
- Next arcs, in order: **R4 server consumers** (always-enriched closed
  files through the overlay), then **@INC tier stripping** — both full
  auto with hardening rounds.
- Backlog now: **writer-harness + stamp-capture dedup**. Staying
  laddered: parts-token inner APIs (allowlist test covers the gap),
  pack provided-names vocabulary (fix when a name-keyed pack dirty walk
  lands), writer fallback budget (tripwire keeps it visible), watcher
  persist+re-strip, buffer-vs-disk record provenance, probe
  serialization (measure first), phase-4 SQL views. Declined micro-opts
  stay declined.

## Phase-4 SQL views — CLOSED 2026-07-12 (Claude)

The one triaged-"build" view landed: unused-exports
(`unused_exported_syms` + `SymRowSeed::FLAG_EXPORTED`,
`REF_ROWS_VERSION` 5), wired into `--heatmap` as a dead-export queue
plus a sound pre-prune of the fan-in walk. The row-backed verdict
substitutes only for skipped walks (provably equal there); a running
projection always decides — candidate rows over-approximate, so a row
"maybe used" could mask a dead export whose every candidate the matcher
rejects. Parked/declined views and the full contract:
`docs/adr/relational-ref-index.md` ("Further relational views").

## R4 hardening round — 2026-07-07 — CLOSED 2026-07-12 (Claude)

Fixed: retries only ever use RETAINED overlay copies (an unretained copy
gave the seams fresh bag pointers per recursion level — unbounded mints
to the 512-depth cap on byte-cap giants, and memo entries keyed on freed
addresses); declines (giant/cycle-taint) are CACHED under the key, so
repeat queries skip the deep copy; the exporter loop is two-pass (raw
answers can never be shadowed by an earlier exporter's enriched answer;
symbol-less exporters never trigger a retry); QueryState pins enriched
Arcs for memo-address validity; the overlay is LRU (was FIFO);
enrichment keys use monotonic REGISTRATION GENERATIONS instead of Arc
pointers (ABA-proof; also covers body-dependent provider facts the
span-free fingerprint deliberately ignores); --dump-package warns on
overlay decline. Cycle policy: tainted builds decline deterministically
— cyclic files answer raw everywhere, never a query-order-dependent
half-enriched cache.

The unwired seams landed 2026-07-12: bridged-plugin-entity chase
(index-less by design — a ctx-ful leaf query would spawn a fresh cycle
guard per bridged hop, so mutual bridges could recurse unbounded; the
ENRICHING-guarded bake reaches the same transitive answer), SlotType
primary (dormant twin of the PackageSymbol retry — SlotType seeds are
build-gated on a resolvable RHS, so it goes live the moment slot
seeding emits an unconditional edge), and enrichment's own import scan
(enrichment is now transitive A→B→C; the cycle guard's first real
customer — mutual imports decline to raw deterministically, tainted
copies never cached). TypeName chase stays raw (pack aliases, no Perl
win). Gold: 432/17/0/0/0 cold+warm, warm RSS flat.

The @INC Arc-pointer freshness token also landed 2026-07-12: the
generation maps (`registration_gen` + `gen_counter`) are threaded into
`spawn_resolver` / `spawn_test_resolver` (like `long_lived` /
`bag_cache`); the resolver thread mints a generation for every @INC
provider it (re-)resolves and stamps warm-loaded providers after
`warm_cache` (the CLI main-thread path mirrors this in `insert_cache` +
a post-warm stamp). Every provider now has a real, monotonic
generation, so the Arc-pointer fallback arm in `enrichment_key` is
deleted (ABA-proof).

Still deferred (in-flight dedup): `docs/prompt-storage-residuals.md`.

## @INC stripping arc — closed — 2026-07-07 (Claude)

Landed across four commits: the cold-path strip (persist-first,
strip gated on the blob landing, memo unpinned), generation-coherence
fixes (NULL-blob rows report unpersisted; shred gated on persist), the
`mark_long_lived` gate (R4 enriched retries + the warm @INC strip run
only where a process amortizes them — the server; one-shot CLI skips
both), lifecycle fixes (resolver stale-pin clears, memo-None parity,
warm sentinel None-over-Some guard, `load_one` prefers the workspace
generation), and `idx_modules_path`.

The wall saga, for the record: warm gold 40s (NO_EVICT) vs 442-547s
(default) decomposed into (a) the R4 retries in one-shot processes
(bisected 374→790s; now gated off there), and (b) `load_one` full table
scans — modules had NO path index, so every rehydration since the
eviction axes landed scanned blob-bearing rows (sys-time storms). The
index took warm gold 547→162s (sys 227→7.6s). Residual 162s vs 40s =
per-row cold-LRU rehydration in one-shot processes — the accepted
profile; the long-lived server amortizes it.

Measured: warm-harness RSS 615→348MB (default vs NO_EVICT); warm
SERVER sessions additionally strip warm-loaded @INC copies (CLI keeps
them whole — RAM dies with the process; wall matters more there).

Deferred: @INC registration generations for the enrichment key — LANDED
2026-07-12 (threaded into the resolver thread; the Arc-pointer fallback is
gone, see the R4 hardening round's entry above). The 162s one-shot
rehydration profile and the import tier's symbols/refs reader routing:
`docs/prompt-storage-residuals.md`.

## Intermittent cold-start flake — 2026-07-07 — CLOSED as watch-with-tripwires (2026-07-12)

Twice this branch, a COLD gold run misbehaved and was clean on
immediate rerun: once 3 FAILs, once 372 PASS / 41 FAIL / 5 XPASS with an
impossibly fast wall (88s vs ~300-500s) — the "inputs vanished,
absence-as-answer" signature (diagnostics XPASS = typeless sweep). Both
occurrences ran immediately after (or concurrent with) heavy build/test
activity on the same box. Suspects at the time: the two-writer startup
window (resolver thread + workspace indexer on one modules.db) under
load, or a strip-before-persist window on the Perl tier.

The nets, all landed: `PERL_LSP_STRICT_RESIDENCY=1` (set by the gold
harness) makes a rehydration miss on an evicted copy PANIC with the
failing stage named — a compromised session dies as CRASH rows instead
of scoring wrong answers; `rehydration_miss_count` counts the same
degrades in live servers; the workspace tier got the residency tripwire
the pack tier had (shared `residency_tripwire` speller, strict-fatal in
release).

Probe result: 3 fully-cold gold runs under saturating CPU (4 busy
loops) + fsync-churn IO load — all 432/17/0/0/0, zero strict
violations, walls honestly degraded (~335s vs ~245s quiet). No repro.

First blood went to the net itself, immediately on arming: the strict
baseline caught a DETERMINISTIC absence-as-answer bug — the whole-view
workspace sweep handed PERL paths to the routed PACK sub-index, whose
loader (modules-{lang}.db) can never serve them, so cross-TU cpp
references silently dropped every Perl workspace file's matches. Fixed
at the mechanism: `rehydrate_or_resident` routes a foreign path to the
owning sibling tier (sub-index → the hub's rehydration cell, hub → the
registering pack sibling; one hop, cache-only, no recursion). That bug
postdates the original flake occurrences and does not explain them.

Standing state: any recurrence now fails LOUDLY in gold (named stage,
CRASH row) and is countable in servers. If it never fires again, the
original occurrences stay attributed to the pre-hygiene two-writer
window whose fixes have since landed.

## Stack graphs for name resolution — 2026-07 — CONCLUDED / DECLINED (Claude)

Full eval + spike kept as the paper trail: `docs/evals/stack-graphs.md`
(+ `stack-graphs-spike.rs`). Verdict: do not adopt. Stack graphs solve
name binding (reference→definition path-find), but the high-value
perl-lsp problem is type inference, and in Perl the two aren't separable
— the dominant nav case `$obj->method` needs `$obj`'s inferred class,
which stack graphs cannot derive and must be fed from the witness bag. A
running spike (`stack-graphs 0.14`, real `ForwardPartialPathStitcher`)
resolved method dispatch 0 on syntax alone, 1 only when the `$o : Obj`
type fact was hand-injected as edges; a gold-corpus census put reach at
~38% of rows (pure name-binding) with ~62% type/framework-gated.
Adoption would also re-encode each Perl exporter/`@ISA`/`use constant`
dialect in a second formalism, add ~36 crates + a parallel graph store,
and lose the type-carrying edges the witness bag already has (`graph.rs`
derives edges on demand). Net more bookkeeping, not less; the typed-edge
`GraphView` is the better generalization. Revisit only if stack graphs
gain value/type-flow semantics AND an upstream Perl TSG definition ships.

## Pack first-change diagnostics: fast-degraded-now vs correct-but-delayed — 2026-07-15 — RATIFIED 2026-07-17 (veesh)
- **Context:** edit-bench P1 (bench/RESULTS.md). The first didChange on a
  cold-opened C++ file published diagnostics in ~24 s (warm 193 ms). Root
  cause: `spawn_debounced_rebuild` ran the pack analyze with the cross-file
  GATHER enabled, so the first keystroke after a cold open paid the whole
  cold gather synchronously inside the debounce task — and did_open's
  background `spawn_pack_gather_refresh` couldn't warm it because that task
  bails once the buffer text changes.
- **Options:** A — first change rebuilds CACHED-ONLY (instant, degraded
  diagnostics), then a background gather refresh heals full-quality
  diagnostics when the cold gather lands (the same async-refresh did_open
  uses). B — share the in-flight open gather via a per-URI completion token
  and have the change path await it (correct diagnostics, but the first
  change still waits ~24 s and the token/registry is new shared state).
- **Picked:** A. Loosest-coupled: reuses the existing
  `set_gather_cached_only` thread-local and `spawn_pack_doc_refresh` heal;
  no new shared state, no cross-task handshake. The change path is symmetric
  with the open path. Cost: the first change's diagnostics are DEGRADED
  (cached-only macro table) for the ~24 s until the background gather warms
  the shared `pre_expanded_cache`; every rebuild after that is fast AND
  full-quality (cache hit). One redundant cold gather can run (did_open's G0
  bails, the change's heal G1 recomputes) — bounded, warm-cache-idempotent.
- **Undo cost:** trivial to revert to B's shape — drop the cached-only
  wrap + heal spawn, add a shared token; the seam is one function.
- **Discussion needed:** is stale-but-fast the right default for pack
  first-change, or should the first change block on correct diagnostics?
  If a shared-gather token is wanted anyway (to also kill the redundant
  double gather), that's the B upgrade — additive on top of A.
- **Ruling (veesh, 2026-07-17):** A stands — degraded immediate is the right
  default — WITH an LSP-visible notification that indexing/gather is still
  loading (work-done progress or equivalent; the degraded window must be
  announced, not silent). The shared-gather token (B's machinery) is NOT
  excluded: wanted as the additive upgrade that kills the redundant double
  gather and single-flights concurrent gathers — it just never blocks the
  change path.
- **Follow-ups commissioned:** (1) first-change degraded window announces
  itself via progress/notification; (2) per-URI single-flight gather token,
  additive on A; (3) Chromium-scale editing-during-cold-gather storm
  mitigations assessed (abandoned-heal churn, save-during-index invalidation
  cone, pre-expanded-cache cap thrash) — see the slice brief of 2026-07-17.

