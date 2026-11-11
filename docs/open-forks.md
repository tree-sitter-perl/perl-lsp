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
| [DBIC source-moniker disambiguation without a typed `$schema`](#dbic-source-moniker-disambiguation-without-a-typed-schema--2026-07-17--open-claude) | 07-17 | is the largest-source-family heuristic acceptable as the interim, or should moniker resolution wait for schema-value provenance? |

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
