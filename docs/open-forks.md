# Open architectural forks — for discussion

Convention (standing order, 2026-07-03): when autonomous work hits a genuine
architectural fork, we (a) pick the LOOSELY-COUPLED option — reversible,
behind a seam, no serialized-format lock-in where avoidable — (b) implement
it, and (c) log the fork here with the options, what was picked, why, and
what undoing it would cost. The user reviews this ledger; entries get
resolved (ratified or reversed) explicitly.

This file holds ONLY the open forks. Resolved/ratified/closed entries move
to `docs/forks-resolved.md` (ledger of record); deferred work items with
designs live in `docs/prompt-storage-residuals.md`.

**Awaiting review:**

| Fork | Since | The question |
| --- | --- | --- |
| [Answer honesty under index/enrichment windows](#answer-honesty-under-indexenrichment-windows--2026-07-14--open-claude) | 07-14 | which verbs block for honest answers vs stay fast-best-effort — now that honest cold references costs ~27 s on abseil? |
| [Decl→def ranking on QUALIFIED / member goto-def](#decldef-ranking-on-qualified--member-goto-def--2026-07-15--open-claude) | 07-15 | should qualified goto-def rank def-over-decl, via the shared seam (B) or a local patch (A)? |
| [Cross-file gated-emission visibility](#cross-file-gated-emission-visibility--2026-07-17--open-claude) | 07-17 | how do cross-file readers see a DBIC result class's deferred accessors — index-time materialize (picked) vs a per-query enriched overlay? |
| [DBIC source-moniker disambiguation without a typed `$schema`](#dbic-source-moniker-disambiguation-without-a-typed-schema--2026-07-17--open-claude) | 07-17 | is the largest-source-family heuristic acceptable as the interim, or should moniker resolution wait for schema-value provenance? |
| [GraphView node identity is leaf-keyed](#graphview-node-identity-is-leaf-keyed--2026-09-01--open-claude) | 09-01 | should `Node::Class` carry the namespace so a same-leaf aliased parent stops needing a per-consumer bypass (two exist)? |
| [Union types in the lattice](#union-types-in-the-lattice--2026-09-02--open-claude) | 09-02 | `list<A|B>` / `A|B` returns: add a `Union` variant, pick an arm, or stay dark? |
| [Dead-code queue vs library public API](#dead-code-queue-vs-library-public-api--2026-09-02--open-claude) | 09-02 | should `--heatmap` learn a library mode that never flags public members whose callers live out of tree? |
| [Use-map pin with no indexed declaration answers empty](#use-map-pin-with-no-indexed-declaration-answers-empty--2026-09-02--open-claude) | 09-02 | when a php file `use`s a class no indexed file declares (vendor not indexed), should gd/hover answer nothing, or fall back to a same-leaf candidate from another namespace? |

Format per entry:

## <fork name> — <date> — <status: OPEN / ratified / reversed>
- **Context:** where it came up (slice, finding).
- **Options:** A / B (/ C), one line each.
- **Picked:** which, and the loose-coupling story (how it stays undoable).
- **Undo cost:** what reversing takes.
- **Discussion needed:** the question for the user.

---

## Answer honesty under index/enrichment windows — 2026-07-14 — OPEN (Claude)
- **Context:** edit-bench rounds 1–4 (bench/RESULTS.md). Verbs answer
  PARTIAL or NULL inside two windows and the response looks complete:
  cold index build (curl cold references 866 B vs 34 KB warm; bugzilla
  cold completion 233 B vs 5.5 KB) and per-file build/enrichment waits
  (bugzilla WARM outline sometimes null, WARM hover sometimes null —
  the ~400 ms bounded waits `await_open_ready`/`await_index_ready`
  expire and the verb serves whatever is there). Editor-tier sibling of
  absence-as-answer.
- **Options:** A — per-verb wait policy on one seam: bulk/identity verbs
  (references, rename, implementations) wait for index-ready without the
  400 ms cap (with LSP progress); per-file verbs (outline, hover,
  completion) wait for THIS file's build (bounded by build time, not a
  fixed cap); latency-critical interactive verbs keep best-effort.
  B — always best-effort + server-initiated refresh nudges (works for
  semanticTokens/inlayHint; LSP has NO refresh channel for
  references/hover/outline responses — can't heal those).
  C — label partial answers (LSP has no partiality flag on these verbs;
  would need client cooperation).
- **Picked (to implement):** A — it's the only shape that can't lie on
  verbs whose answers are act-on-able (rename edits!), and the policy
  lives on ONE seam (the existing await_* helpers grow a per-verb
  policy parameter) so redirecting any verb's policy later is a
  one-line change. B's nudge pattern stays for the verbs that have
  refresh channels.
- **Undo cost:** trivial per verb — the policy table is data.
- **Discussion needed:** which verbs the user wants blocking-honest vs
  fast-best-effort; whether rename should hard-refuse (error) instead
  of wait when the index is cold. Concrete price now measured: abseil
  COLD references blocks ~27 s for the honest answer (was 402 ms
  partial). LSP progress for blocking waits is landed
  (`Backend::bounded_wait_with_progress` — silent under 500 ms, so
  Interactive waits never mint a token), so the block is visible in the
  editor rather than reading as a hung request.
- **The curl server-context case — RESOLVED 2026-07-16:** server
  references answered 4 sites where the CLI answers 155. Root cause was
  the DEGRADED-OPEN window, not target minting: `did_open` builds pack
  docs with the cached-only gather (a fresh server's gather cache is
  process-local and empty even when modules.db is warm), the background
  heal replaces the analysis, but `await_open_ready` only waits for AN
  analysis to exist — a references fired between open and heal read the
  partial closure (repro: immediate ask 826 B, same ask 15 s later
  32,665 B). Fixed as this fork's per-file half: `degraded_open` marks
  the window (set at cached-only open/first-change builds, cleared by
  the heal), and `await_open_full` — called by references / rename /
  implementations only — bounded-waits it out (280 ms warm on curl;
  cold pays the gather, visible via work-done progress). Per-file verbs
  (outline/hover/completion) deliberately don't wait: their answers
  don't read the cross-file closure, and blocking them behind a gather
  they don't need would regress open→outline latency.


## Decl→def ranking on QUALIFIED / member goto-def — 2026-07-15 — OPEN (Claude)
- **Context:** the C-tier bench finding "C goto-def stops at the header
  prototype" (bench/RESULTS.md). Fixed for UNqualified free-function calls
  (redis `lookupKeyReadOrReply`/`addReplyBulk`, curl
  `Curl_conn_cf_discard_all`): `CandidateSet::preferred_definitions` now
  admits a def-candidate whose TU includes the DECL's header, so a third TU
  calling through a shared prototype reaches the bodied definition (ranked
  first, decl kept). But the QUALIFIED / namespaced spelling
  (`pkg::Combine` in the multitu fixture) routes through
  `member_def_location` (the owner-anchored `qualifier_at_point` path at the
  top of `definitions()`), which returns a SINGLE location, applies the same
  origin-only connectivity gate (excluding the defining TU), and does NO
  decl→def ranking — so it still lands on the prototype.
- **Options:** A — teach `member_def_location` the same decl-connectivity
  clause AND a bodied-over-bodiless preference, returning the def (or def
  ranked first). B — route qualified member/namespaced-function calls
  through `preferred_definitions` (the free-function lane already fixed) so
  one mechanism serves both spellings; `member_def_location` stays the
  member-RESOLUTION seam, ranking becomes a projection concern. C — leave
  qualified member goto-def landing on the decl and expose the def via
  `textDocument/declaration` vs `definition` split.
- **Picked:** none yet — the free-function fix is landed and scoped to the
  bench finding; the qualified-member case is a strictly-additional surface
  (the bench did not flag it, no regression introduced). Documented so the
  maintainer can pick B (the loosely-coupled unification — one decl→def
  mechanism, member_def_location keeps resolving, ranking is inherited) vs A
  (local patch, faster but re-derives the ranking in a second place, the
  asymmetry the resolution-CandidateSet ADR warns against).
- **Undo cost:** low — the landed change is one added `||` clause in
  `preferred_definitions`; picking any option above is net-new work, not a
  reversal.
- **Discussion needed:** should member/qualified goto-def rank def-over-decl
  at all, and if so via the shared `preferred_definitions` seam (B) or a
  local `member_def_location` patch (A)? B is the rule-#10-consistent pick.

---

## Cross-file gated-emission visibility — 2026-07-17 — OPEN (Claude)
- **Context:** H7-5. A `ClassIsa` plugin trigger
  (DBIC `has_many`/`add_columns` synthesis) can't fire at build for a result
  class whose `isa DBIx::Class` route runs through a cross-file intermediate
  base (rule #1: index-free builder). The build records the emission as a
  `GatedEmission` and enrichment re-fires it (`class_isa_prefix` +
  `apply_gated_emissions`). That makes the OPEN focus file + `--dump-package`
  (enriched overlay) correct. The residual: cross-file goto-def / references
  read the TARGET class's *cached* copy via `whole_present`, which doesn't
  carry enrichment-only symbols.
- **Options:**
  - **A — index-time materialize (picked).** After indexing completes,
    `ModuleIndex::materialize_gated_emissions` applies each cached copy's
    deferred emissions in place (gate resolved cross-file), so `whole_present`
    carries them. Deterministic, no per-query cost.
  - **B — per-query enriched overlay.** A fallback-on-miss in
    `method_resolution_on_class` consults `enriched_snapshot` for a gated
    class. Tried and REVERTED: it re-enters full enrichment per inheritance
    hop (enrichment's own `stamp_method_call_targets` resolves methods →
    overlay → nested enrich), overflowing the stack on the DBIx::Class
    substrate; the enriched-overlay memoization key also churned, returning
    Some then None for the same file within one query.
- **Picked:** A. Loosely coupled: `materialize_gated_emissions` is one
  post-index pass keyed only on `gated_emissions` being non-empty (a normal
  class pays nothing); it mutates the cache in place and is idempotent
  (`apply_gated_emissions` dedups). It is called only from `cli_full_startup`
  (CLI + `--batch`), so the warm-server residency budget is untouched.
- **Undo cost:** delete the one call in `cli_full_startup` + the
  `ModuleIndex`/`FileAnalysis` methods; the build-time recording +
  enrichment application (open-doc + overlay) stand alone.
- **Discussion needed:** three residuals to ratify or close.
  (1) **Real LSP server (not CLI):** `materialize` runs only in the CLI/batch
  startup, so a live-editor goto-def into a CLOSED dependency's gated accessor
  misses (open files enrich; the warm server evicts symbols, so materializing
  in-place there would re-pin them — a residency call). Is the CLI-only scope
  acceptable, or should the server get a residency-bounded variant?
  (2) **The `$schema->resultset('X')->...->first` chain** (separate defect,
  H7-8-adjacent) types the row as the SHORT source name `"Artist"`, not the FQ
  result class `DBICTest::Schema::Artist`, so on the real DBIx-Class corpus the
  `->cds` call-site invocants never match the FQ anchor: `--references`/
  `--definition` at those coordinates return 0 despite correct synthesis + a
  working cross-file goto-def path (proven on a directly-typed invocant). The
  source-name→result-class mapping is the missing piece and is out of H7-5's
  ClassIsa scope.
  (3) **Gated content is invisible to the Surface freshness firewall**
  (surfaced by the round-7 sweep). `Surface::project` reads `fa.symbols` +
  `fa.packages`, never `fa.gated_emissions`, and
  `materialize_gated_emissions` swaps the cached `Arc` WITHOUT re-recording
  the Surface / `FreshnessIndex`. So a change confined to a result class's
  gated content (its `add_columns`/`has_many` list, whose synthesized
  accessors live ONLY in `gated_emissions` until the cross-file gate
  resolves) projects an IDENTICAL Surface → `SurfaceVerdict::Unchanged` →
  consumers are not dirtied. Latent today, not a live bug: CLI is one-shot
  (no incremental dirty-tracking), and the warm server never materializes
  into the Surface — it answers gated classes through the per-query
  enriched overlay. But it is a PRECONDITION of resolving residual (1) the
  in-place way: if the server ever gets a residency-bounded in-place
  materialize, `Surface::project` must also learn to reflect gated methods
  (or `FreshnessIndex` must fingerprint `gated_emissions`) so incremental
  edits to gated content dirty consumers — otherwise a server-side
  materialize goes stale on the next edit. The question: should gated
  emissions participate in the Surface / freshness firewall, and if so does
  that ride with the residual-(1) server-materialize decision or land first
  as its own equality-net arm? (No R1 violation exists today —
  `gated_emissions` is deliberately NOT a projected Surface field, so nothing
  smuggled a span past the equality net; this is a design question about
  whether it SHOULD be one.)

---

## DBIC source-moniker disambiguation without a typed `$schema` — 2026-07-17 — OPEN (Claude)
- **Context:** H7-15. `$schema->resultset('Artist')`
  names a row by its DBIC SOURCE MONIKER (`Artist`), not the FQ result class
  (`DBICTest::Schema::Artist`). `resolve_dbic_source_moniker`
  (`file_analysis.rs`) resolves it at query time: a candidate is any indexed
  class that is a DBIC result (transitively `isa DBIx::Class::Core`/`::Row`)
  whose basename or `source_name` equals the moniker. The CORRECT scoping is
  the receiver's schema: DBIC registers `moniker → class` per schema
  (`load_classes`/`load_namespaces`/`source_name`), so the schema value picks
  the source unambiguously.
- **The fork:** in the real DBIx-Class corpus the `$schema` value is
  UNTYPED at the resultset call (`$schema = DBICTest->init_schema()` returns
  through `compose_namespace`/`connect`; `$s = DBICTest::Schema->connect(...)`
  types only to the generic `DBIx::Class::Schema` base). With no concrete
  schema, the moniker is genuinely ambiguous — `Artist` matches three indexed
  result classes (`DBICTest::Schema::Artist`, `ViewDeps::Result::Artist`,
  `ViewDepsBad::Result::Artist`). Resolving it correctly needs the
  long-distance value-provenance tier (`prompt-type-inference-residual.md`)
  to type `$schema` to its concrete schema class — the same parked tier the
  instance-brands work waits on.
- **Options:**
  - **A — largest-source-family heuristic (picked, reversible).** When the
    schema is unknown and >1 candidate matches, pick the candidate whose
    parent namespace holds the most indexed classes (a proxy for the
    workspace's primary schema), lexicographic tie-break. Deterministic;
    picks `DBICTest::Schema::Artist` on the corpus. `schema_hint` (threaded
    as `None` today) already scopes correctly the moment `$schema` types to a
    concrete schema, so this degrades to exact resolution for free.
  - **B — schema-value provenance first.** Block moniker resolution on typing
    `$schema` (model `connect`/`compose_namespace`/`init_schema` returns as
    the concrete invocant class, cross-file). Correct, but a large separate
    inference effort (the parked value-provenance tier).
  - **C — resolve only when unambiguous.** Resolve a moniker only when
    exactly one DBIC result matches; keep the short moniker otherwise. Honest
    (never wrong) but leaves the corpus's `Artist` sites dark — fails the
    goto-def/references acceptance.
- **Picked:** A (reversible). The heuristic lives entirely in
  `resolve_dbic_source_moniker`; deleting the family-size sort and returning
  `None` on ambiguity reverts to C. The `schema_hint` parameter is the seam
  where B plugs in — when `$schema` becomes typeable, thread the concrete
  schema class and the heuristic never fires.
- **Discussion needed:** is the largest-family heuristic acceptable as the
  interim, or should moniker resolution wait for schema-value provenance (B)?
  And should the moniker→class CONVENTION (basename / `source_name` under a
  schema's result namespaces) move from core (`resolve_dbic_source_moniker`,
  where it sits with `extract_resultset_parametric`) into the DBIC plugin
  manifest when the DBIC-as-plugin port lands?

---

## GraphView node identity is leaf-keyed — 2026-09-01 — OPEN (Claude)
- **Context:** the php round-4 `parent::` fix (H8). `GraphView`'s ancestor
  edges are keyed by LEAF class name (`docs/adr/graph-walking.md`), so a
  same-leaf parent in another namespace (`use Support\Collection as
  BaseCollection; class Collection extends BaseCollection`) collapses onto
  the child's own node and `walk(Node::Class(child))` cannot tell them
  apart. Two consumers now carry their own bypass: `resolve_super_method`
  (ancestry.rs — a `parent_namespaces`-row pre-pass before the graph walk)
  and `CandidateSet::super_def_locations` (definitions.rs — walks
  `declared_parents` directly with per-row namespace routing, never
  entering the graph). The round-close sweep flagged the duplication.
- **Options:** A — keep the two bypasses (status quo; a third same-leaf
  consumer will grow a third). B — one shared `same_leaf_parent` helper
  both call (mechanical dedupe, identity stays leaf-keyed). C — give
  `Node::Class` a namespace-qualified identity for pack languages (the
  edge derivation reads `parent_namespaces` rows), so the graph itself
  distinguishes the parent and every walker inherits it — the rule-#10
  answer, but it touches every `Node::Class` constructor and the
  descendant/family walks' dedup keys.
- **Picked:** A for now (nothing further changed in the sweep; the two
  interface predicates were deduped onto `FileAnalysis::declares_interface`).
- **Undo cost:** B is an afternoon; C is a slice with its own gold rows
  (every leaf-keyed consumer re-examined) and a cache bump.
- **Discussion needed:** is C worth a slice now, or does it wait until a
  third consumer appears? Perl is absolute-named and never hits this.
- **Update 2026-09-02 (round-5 R5-1):** the use-map visibility axis is a
  THIRD consumer of leaf identity, and it is where `use X as Alias`
  stops: the alias spelling can't be resolved by translating it to the
  real leaf at extraction (H8's file means two different classes by
  `Collection` and `BaseCollection`), and can't be pinned without a
  namespace-qualified class identity. So alias-spelled hints/`new`/
  receivers resolve nothing today (never a wrong class). C is now the
  only path to alias support — a concrete reason to schedule it.

---

## Union types in the lattice — 2026-09-02 — OPEN (Claude)
- **Context:** php round 5 (composer): `@return list<CompletePackage|CompleteAliasPackage>` — a union INSIDE a generic — leaves the foreach var dark on every verb (hover/gd/refs/rename/completion), isolated against a working `list<Single>` control. `InferredType` has no union; `phpdoc_type` rejects a two-armed spelling ("a two-armed claim is not a type answer") and `php_annot_type` returns `None` for `A|B`, so the whole element type drops.
- **Options:** A — stay dark (status quo; honest, but composer's package-loading core path is exactly this shape). B — a `Union(Vec<InferredType>)` variant: dispatch = the INTERSECTION of the arms' member sets, hover renders `A|B`, `element_at`/projections map over the arms; a lattice change (bincode append, cache bump) touching every reducer that matches on `InferredType`. C — "first class arm wins" as a display-only heuristic: wrong for members the second arm lacks, cheap.
- **Picked:** A for now (nothing changed).
- **Undo cost:** B is a slice with its own gold rows; C is an afternoon and a documented lie.
- **Discussion needed:** is B worth its blast radius? `?T` (`Optional`) already exists as a one-armed union; the general case is the question.

## Dead-code queue vs library public API — 2026-09-02 — OPEN (Claude)
- **Context:** composer/phpMyAdmin heatmap sampling: after the ctor fix the remaining false positives are public Plugin/Event-class API, PSR interface implementations and framework-invoked overrides (Symfony Console `getLongVersion`) — callers live OUT of the indexed tree. Framework overrides are `entry.json` data; the general "this is a library's public surface" fact is not.
- **Options:** A — status quo (the queue is honest about "no caller found in this index", the doc says so). B — a `--library` heatmap mode: never flag `public` members of non-final classes / interface implementations. C — infer library-ness from `composer.json` (`"type": "library"` + PSR-4 autoload roots) and apply B automatically.
- **Picked:** A (nothing changed); Symfony Console overrides can go into `symfony.entry.json` as data regardless.
- **Undo cost:** B/C are small and reversible.
- **Discussion needed:** which of B/C, and whether "public" should mean the PHP visibility keyword or the autoload roots.

---

## Use-map pin with no indexed declaration answers empty — 2026-09-02 — OPEN (Claude)
- **Context:** round-5 R5-1 (`VisibilityAxis::UseMap`). A php origin's
  `use Symfony\...\Request;` pins the leaf `Request` to that namespace.
  When no indexed file declares it (the vendor tree is not in the
  workspace, or composer's tier is off), `visible_def_candidates` answers
  EMPTY — gd/hover/completion on that class go dark. Before the axis they
  answered a same-leaf stranger (`Http\Client\Request`) — wrong, but
  something. Laravel's `Auth/SessionGuard.php:150` is the live case.
- **Options:** A — empty (picked): the file said what it means, a
  stranger is a lie. B — degrade to the full same-leaf table, ranked by
  the own namespace, when the pinned filter is empty (SearchPath's
  degrade rule). C — empty for navigation, stranger for TYPE chases only
  (member completion would at least show something).
- **Picked:** A, per the smartmatch principle (predictable over clever)
  and rename safety — B would re-admit the stranger's members into the
  references walk through the dispatch chain. One arm in
  `ScopedLookup::visible_def_candidates` (the `pinned` branch); B is a
  three-line change there, C a shape flag on the query.
- **Undo cost:** trivial (one match arm); the pin test would need its
  "un-indexed pinned class answers empty" expectation flipped.
- **Discussion needed:** is dark-but-honest the right default for an
  editor surface, or should the composer vendor tier be the answer
  (index what `use` names, then the question never arises)?
