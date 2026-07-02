# Adversarial review: the `spike/cpp-support` arc (b4aaf35..885ebc3)

Deep review of the cumulative branch diff (~25k insertions, 202 files), riskiest
seams first: resolve.rs refs symmetry, cpp_reparse.rs expansion/caching,
module_index.rs visibility, witnesses.rs domain typing, cpp_macro_model.rs.
Every finding below was **verified empirically** (micro-repro + CLI/LSP probe,
or exact code trace confirmed by a repro) on this worktree's
`--features all-langs` release build. Probes ran against scratch workspaces,
`/home/veesh/personal/perl5`, and `/home/veesh/personal/cpp-bench/*`.

**Regression nets are green**: gold corpus 257 PASS / 0 FAIL / 0 CRASH
(16 xfail), cpp e2e 21/21, perl e2e 113/113, `cargo test --release` 1186/0.
Every finding below lives *outside* the covered scenarios — multi-TU name
collisions, mixed-language workspaces, in-session edits, rename, config-flag
reachability. That is the review's core message: the happy paths hold; the
seams between them don't.

**Review-environment caveat**: mid-review, a `cargo test --release` (default
features) transiently replaced `target/release/perl-lsp` with a perl-only
build; probes in that window showed pack support "dead" (including a full gold
run that lang-skipped all 70 cpp rows because `--languages` reported perl
only). Every finding below was (re-)verified after rebuilding with
`all-langs`. One agent-observed "nondeterministic analysis generation" on
perl5 is likely confounded by that window and is reported at reduced severity
(H8) with its code-verified structural core intact.

---

## CRITICAL

### C1. `refs_to` ignores the visibility model goto-def uses — name-splat across TUs, closures, and *languages*

The arc's own ADR (docs/adr/macro-handling.md, "Resolution visibility = the
include-closure lie") and the gd↔gr symmetry commit (27d8c80: "every forward
resolution mirrors backward on the same key") promise closure-scoped
visibility. Forward resolution honors it (ScopedLookup); the **backward match
side of `collect_from_analysis` does not** — it matches by bare name across
every file in the store and the dep cache.

Verified instances (all quiet-system, current binary):

- **Cross-language pollution at scale**: `--references perl5/util.c 1923 0`
  (`Perl_croak_nocontext`) → **1727 locations, 1479 of them `.pm` files** —
  every Perl `Carp::croak(...)` call in the tree counted as a reference to the
  C function, via the delegation alias `croak` (embed.h). The alias match
  (src/resolve.rs:1431-1442) has no language, closure, or resolution gate:
  any `FunctionCall|Variable|PackageRef` ref whose *name* equals an alias
  matches. `delegation_aliases` itself (src/resolve.rs:1014-1049) collects
  edges from every file regardless of visibility. perl5 — the flagship
  dogfood corpus — is exactly the mixed workspace this breaks in: ~85% noise.
- **Unrelated `static` globals conflate**: two files each declaring
  `static int counter;` — refs on one returns the other TU's def *and* uses
  (`TargetKind::FileScopeValue` arms, src/resolve.rs:1551-1561; the gate
  `symbol_is_file_scope_value`, src/file_analysis.rs:8793-8800, checks
  file-scope only, never linkage or closure). Real instance: redis has
  same-named statics (`gc_count` ×2). A pure-Perl workspace is unaffected
  (sigils), but a Perl file's *unresolved* call `VERSION()` matched a C
  `#define VERSION` (verified) via the
  `(FileScopeValue, FunctionCall{resolved_package: None}) => true` arm.
- **`(Method{class}, RefKind::Variable)` with `resolves_to: None => true`
  (src/resolve.rs:1531-1546)**: any unresolved bare identifier read with the
  right name, in any file, matches a member/enum target. Verified: a stray
  undeclared `op_type` read matched `Method{S1}`; with two same-named enum
  constants under disjoint include closures (`enum OpA { OP_X }` /
  `enum OpB { OP_X }`), refs on OpA's `OP_X` includes OpB-closure uses while
  correctly *excluding* OpB's def — the output is internally inconsistent
  (uses without their def).
- **Same-named structs conflate wholesale**: `struct conn { int fd; }` in two
  unrelated headers — refs on one `fd` returns the other's def and uses
  (`symbol_defines_target`'s Method arm, src/resolve.rs:1155-1168, and the
  MethodCall class match are name-keyed with no closure gate).

gr(def) == gr(use) held in every probe — the machinery is symmetric, but
symmetric-in-wrongness: both directions splat identically.

**Fix direction**: apply the same closure gate to the *match* side that
resolution already uses — accept a cross-file name match only when the
target's defining file is reachable from the collecting file's
`include_closure` (with the same cold-closure fallback `get_cached_scoped`
uses); gate `delegation_aliases` collection and matching on visibility (at
minimum, never match aliases in files of another language).

### C2. Pack rename silently emits partial edits — applying one breaks the user's code

Verified in both CLI and LSP:

- **Struct member** (`struct S1 { int op_type; }` + `o->op_type` use in
  another file): rename at the def edits **only the def**; rename at the use
  edits **only that use**. References at the same cursor finds both.
- **Macro / file-scope value**: rename `#define DEBUG` → def-only edit; its
  uses in including files are left behind (`FileScopeValue` is not in
  `supports_cross_file_rename`, src/resolve.rs:122-133, so it falls to the
  single-file `rename_at`).
- Agent-verified LSP mirror: rename `get_box` at its header def → edit
  touches only the header while references returns 5 locations.

Two root causes, both structural:

1. Pack workspace files live only in the per-language sub-index, which
   `refs_to` sweeps under the **DEPENDENCY** role; rename hard-codes
   `RoleMask::EDITABLE` (src/resolve.rs:828 `rename_via_refs_to`;
   src/main.rs:1509). References knows this and special-cases pack → VISIBLE
   (src/backend.rs:1168-1174, src/main.rs:1183-1189); rename never got the
   same treatment.
2. The LSP rename/prepareRename/implementation handlers pass the raw Perl hub
   un-scoped (src/backend.rs:1196-1201, 1242-1245, 1075-1081) — no
   `pack_index(doc.language)` routing, no ScopedLookup — unlike references
   and goto-def.

A partial rename is worse than a refusal: the workspace edit applies cleanly
and the code is silently broken. (Enum-constant rename currently returns `{}`
— a safe no-op; members and macros are the dangerous ones.)

**Fix direction**: route pack rename through the same pack-index + ScopedLookup
preamble references uses and widen the mask for pack targets (their
"dependency" role is a storage artifact, not a policy); until the C1 closure
gate lands, decline cross-file pack rename outright rather than emit partial
edits.

---

## HIGH

### H1. In-session edits are invisible: frozen pack index + never-invalidated macro caches

End-to-end verified over LSP (compliant client, current binary): open
`main.c` (includes `hdr.h`); cross-file def works. Add `#define LIMIT2 7` and
`int helper2()` to hdr.h via didOpen+didChange+didSave *and* write it to disk;
touch main.c to force re-analysis. `definition` on `LIMIT2` and `helper2` from
main.c → **null for the rest of the session**, while a fresh one-shot CLI on
the same disk state resolves both. Agent-verified converse: shifting a
function 3 lines in an open header leaves goto-def pointing at the **old
position** all session.

Root causes (all code-verified):

- `register_symbols` runs only from `index_pack_languages`, once per session
  behind the `pack_indexed` latch (src/module_resolver.rs:812,837;
  src/backend.rs:277-280). **No re-register on didSave/didClose, no
  unregister path at all** — edits and deletes never reach `all_defs` /
  `all_files` / `cache`.
- The tier-1 macro table cache is keyed by (file path, hash of the file's own
  `#include` lines) with **no header-content validation and no eviction**
  (src/cpp_reparse.rs:851-876); tier-2's header-mtime check is only reached on
  a tier-1 miss, which never happens while the include list is unchanged. Same
  for `include_closure_cache` (src/cpp_reparse.rs:1312) and
  `pre_expanded_cache`. The comment "reopen to refresh"
  (src/cpp_reparse.rs:721) is false — reopening hits the same tier-1 entry.
- File watchers are registered for `**/*.pm`, `**/*.pl`, `**/*.t` only
  (src/backend.rs:876-883) — pack files aren't watched; and if they ever
  were, the watcher handler parses with the Perl parser
  (src/backend.rs:1622-1628).

**Fix direction**: watch pack extensions; on change, re-register the file's
symbols and evict the three per-file caches for every consumer whose
`include_closure` contains the changed path (or fold header mtimes into
tier-1 validity); add `unregister(path)` for deletes.

### H2. Type-name goto-def splices an unscoped file with a scoped range — wrong file, nonexistent position

src/symbols.rs:486-513 (`RefKind::PackageRef` arm): the target *file* comes
from `module_index.module_path_cached(&r.target_name)` — which ScopedLookup
passes through **unscoped** (src/file_analysis.rs:352-354) to the one-winner
cache slot — while the *range* comes from the scoped `get_cached`. With
`struct Box` defined differently in h1.h and h2.h and `b.c` including only
h2.h: `--definition b.c` on `Box` → **h1.h (wrong file) at h2.h's row** — a
position that doesn't exist in h1.h (6-line file, answer row 7). LSP mirror
confirmed. `get_box` on the same layout disambiguates correctly (it takes the
scoped path, src/symbols.rs:596).

**Fix direction**: resolve the CachedModule once through the scoped
`get_cached` and take path *and* range from it; make
`ScopedLookup::module_path_cached` scope-aware.

### H3. `strip_declarator_macros` corrupts valid C++11 brace-init declarations

src/cpp_reparse.rs:597-642 blanks `ID1` in any `struct/class ID1 ID2` followed
by `{`/`:`/`<` — which matches `struct Point p {1, 2};` and
`struct sockaddr_in addr {};` (ubiquitous aggregate-init). The blank runs
*before* the validate baseline (src/cpp_reparse.rs:683-688), so the damage
gate can never reject it. Verified: def on `p.x` → not found; refs on `Point`
lose the use; outline mints the local variable `p` as a phantom **Class**, and
`recovered` feeds a bogus `(class p, macro Point)` pair to the attribute-macro
lane. Control with `= {1, 2}` works.

**Fix direction**: validate the strip like every other repair (damage must not
increase), or require a type-position context for the head token.

### H4. Span remap misses four ref fields — member resolution dies after same-line length-changing splices

`remap_spans` (src/language_driver.rs:507-583) covers symbols/refs/scopes/
witnesses/flow_edges/label_refs/moved_from/var_reads but **not**
`SkelRef.invocant` (src/query_extract.rs:88; consumed via
`expr_type_at_span(invocant_span)`, src/file_analysis.rs:3827), `member_op`
(:93), `import_sites` (:104 → `#include` goto-def), or
`domain_sites.slot_span` (:135). Witness `Expr` spans *are* remapped, so the
un-remapped invocant span can never match. Verified: a long object-like macro
expansion on the same line before `w.size` → def on `size` fails and hover
misattributes to the receiver; identical code with the macro on another line
works. (The SpliceMap arithmetic itself was probed hard — multi-splice on one
line, UTF-8 adjacency — and is sound; the bug is the four unmapped fields.)

**Fix direction**: route the four remaining span-bearing fields through the
same `remap_span` closure.

### H5. Bodyless `#define FLAG` is invisible to reachability — ranking exactly inverted on the most common flag idiom

`MACRO_DEF_QUERY` (src/cpp_reparse.rs:154-160) requires `value:
(preproc_arg)`; the emit gate (:255-260) needs a body — so `#define
MY_FEATURE` (no value: include guards, feature toggles, `PERL_CORE`-style
flags) never enters `macro_defs`, hence neither `defined` nor `universe` in
`ranked_macro_variants` (src/symbols.rs:663-668); `defined_tri`
(src/cpp_macro_model.rs:200-208) returns False. Verified: with `#define
MY_FEATURE` in the same file, goto-def on a macro defined under
`#ifdef MY_FEATURE` ranks the `#else` arm PRIMARY and labels the actually-live
arm "(unreachable: MY_FEATURE undefined)".

**Fix direction**: add a valueless `preproc_def` pattern so bodyless defines
populate the config universe.

### H6. Domain vote counts only enum-resolving sites — confidently wrong domains (`op_targ: opcode` on perl5)

`field_domain_for_owner` (src/file_analysis.rs:5261-5274) `continue`s past
every site whose RHS doesn't resolve to an enumerator, so plain-int
assignments never reach `domain_coherence`'s `total` (src/witnesses.rs:1184).
A slot that is dominantly a plain integer with a minority enum idiom gets 100%
confidence. Verified on perl5: hover `o->op_targ` (a PADOFFSET pad index;
~15/111 uses in op.c are the op_null "stash old type" idiom) → `op_targ:
opcode`, and goto-def returns *only* `enum opcode` — the real decl is lost.

**Fix direction**: count non-resolving sites in the denominator so the enum
must be a majority of *all* uses (matching the documented "truly-mixed →
none" claim).

### H7. `Field{owner, name}` owner is decorative — votes pool across same-named fields of different structs

`DomainSite` (src/file_analysis.rs:2915-2919) records no receiver; the fold
filters on slot *name* only (src/file_analysis.rs:5262); the skeleton query
captures no receiver (queries/cpp/skeleton.scm:266-281). Verified:
`struct basket { int kind; }` compared to `enum fruit` contaminates
`struct crate { int kind; }` — hover on `c->kind` says `kind: fruit` and
goto-def offers the enum. The "canonical field_subject (declaring-class
owner)" identity (src/file_analysis.rs:5160-5172) is minted but never enforced
at vote time.

**Fix direction**: capture the receiver in the domain.slot query patterns and
filter sites by the queried subject's owner.

### H8. Persist tier can freeze a degraded analysis generation (structural; observed nondeterminism confounded)

Code-verified: `index_pack_languages` warms a pack FileAnalysis from
`modules-{lang}.db` validated only by the file's own (mtime, size)
(src/module_resolver.rs:800-817; src/module_cache.rs:344-351). None of the
analysis *inputs* are in the key: the external macro table, the include
closure, or the toolchain probe (`toolchain_info()`,
src/cpp_reparse.rs:1172-1180 — a OnceLock over a subprocess probe whose
failure silently empties system include roots and can flip the whole-file
validate gate to alias-only). `extract` Err persists a silently **empty**
FileAnalysis (src/language_driver.rs:232). Any degraded generation is
re-served forever (the source file never changes). A dark generation was
*observed* on perl5 (hover/def dead warm, fixed by `--clear-cache` + cold
reindex) but that observation is confounded by the mid-review binary swap;
the structural amplifier stands regardless.

**Fix direction**: include an external-table/toolchain fingerprint in the
row key; tag alias-only/empty-external analyses non-cacheable; fail loud on
extract Err instead of persisting a default FA.

---

## MEDIUM

- **M1. Whole-second mtime staleness (cross-session, deterministic repro)** —
  `mtime_secs` (src/cpp_reparse.rs:1113-1122), `load_persisted` (:795), and
  `modules-{lang}.db` (src/module_cache.rs:283-286, 344-351) all compare whole
  seconds; two same-length writes to a header within one second serve the
  stale table/blob. Realistic for generated headers and rapid saves. Fix:
  nanosecond mtime (available) or content hash.
- **M2. Indexed consumer FAs stale on any header change** — a header edit
  re-analyzes the header's own row, but every consumer `.c` row (baked
  splices, old type-alias witnesses, old closure) stays warm because its own
  (mtime,size) is unchanged. CLI probes hide it (probed file re-analyzed as a
  document); cross-file queries reading index copies serve the stale bake.
  Same fix as H8's keying.
- **M3. Domain fold is per-file; the doc says project-wide** —
  `WitnessAttachment::Field` doc (src/witnesses.rs:117) promises "project-wide
  gathering"; the fold reads only `self.domain_sites`
  (src/file_analysis.rs:5261). Verified: same shared field votes `Numeric` in
  one file and `alpha` in another while the project-wide vote is a strict
  majority. Fix: gather across the include-closure's cached files, or fix the
  doc — but per-file semantics makes H7 worse (name is then the only
  identity).
- **M4. Domain bridge masks failed member resolution, site-dependently** — at
  one `o->op_type` site goto-def returns enum-only (field decl lost); at an
  identical expression 60 lines later, field decl + enum. Stable across
  processes; the invocant class resolves differently per site and the
  owner-blind vote (H7) still fires where member resolution failed
  (src/symbols.rs:221, 301-323 vs 533-580). Fix: run shared member resolution
  first and *append* the enum offer.
- **M5. Predefined-macro asymmetry** — `cpp_reparse::known_config`
  (src/cpp_reparse.rs:2096-2100) seeds `toolchain_info().predefined_macros`;
  `ranked_macro_variants` (src/symbols.rs:641-699) does not, despite claiming
  to mirror it. Verified: on a GCC toolchain, goto-def labels the `__GNUC__`
  arm "(unreachable: __GNUC__ undefined)" while build-side variant selection
  ranks it Active — minting and navigation disagree. Fix: seed the predefined
  set in `ranked_macro_variants`.
- **M6. Cold-open goto-def/hover return None, then flip after warm** — on-open
  analyze is cached-only (src/backend.rs:903-905, empty closure per
  src/cpp_reparse.rs:1280-1282) and the pack index attaches only after the
  lazy background walk. Verified: def → None immediately after didOpen, same
  request correct ~4s later. Completion self-heals (`isIncomplete`); def/hover
  have no re-request signal. Design-accepted per comments; reported as the
  determinism gap it is.
- **M7. Pack indexing blocks forever on a client that doesn't answer
  `window/workDoneProgress/create`** — `ensure_workspace_indexed` sends the
  request unconditionally (no client-capability check — an LSP spec
  violation) and `block_on`s the response before indexing
  (src/backend.rs:294-296). Verified: a minimal client (empty capabilities,
  no reply) gets *no cross-file features for the entire session*; the same
  probe with replies works. Fix: gate on
  `window.workDoneProgress` capability / don't block indexing on the
  response.

---

## LOW

- **L1.** `#define S S` self-delegation offers a duplicate "delegates to S"
  location pointing at the definition itself (src/symbols.rs:887-897).
  Cosmetic; skip when `delegate == m.name`.
- **L2.** Enum-constant rename returns `{}` (safe no-op) while refs and def
  work — a gd↔gr↔rename asymmetry to close *after* C2's safety fix, not
  before.
- **L3.** The didChange fast path leaves `doc.analysis` stale against the new
  tree/text between keystroke and debounced rebuild (positions can
  misattribute mid-typing). Inherent to the debounce design; listed for
  awareness.

## Verified-sound (adversarial probes that came back clean)

- SpliceMap multi-splice arithmetic (two same-line different-length
  expansions, UTF-8 adjacency) — exact.
- Crash battery: `#define A A`, mutual macro recursion, `##`, `#define` at
  EOF without newline, unterminated `(((`, CRLF continuations, exponential
  body fanout — no panics, no hangs.
- `#elif` guard-trail negation, nested guards, `#ifndef` self-guards —
  correct. Goto-def and hover consume the same `ranked_macro_variants` —
  agreement by construction (modulo M5's config input).
- Delegation cycles: one-hop see-through + visited set + depth cap — no
  hang. The backward `delegation_aliases` chase is cycle-guarded.
- Determinism at fixed inputs: byte-identical refs across runs (perl5),
  10× fresh-process domain folds identical (BTreeMap + strict majority).
- EXTRACT_VERSION discipline: 112→130 across the arc, bumped with each
  serde-shape change, none after the last bump; MACRO_CACHE_VERSION 1→2
  exactly when `Macro` gained fields; old-shape blobs fail decode and
  re-analyze rather than garbage-serve.
- Perl regression: full unit suite, perl e2e, gold Perl rows all green;
  domain reducers claim disjoint attachment shapes (no `ClassName(enum)`
  leakage into Perl folds).
- Perf: no cliffs at current scale. perl5 refs ≈3.5s wall is ~3.3s CLI
  startup; warm per-query ≈0.13s (396 locations); domain fold ≈15ms/query;
  project-wide enum reverse sweep 3.17s including startup. Structure is
  O(sites×symbols) per query with a fresh registry per call — index it if
  domain shapes grow, not before.
- `all_defs` re-registration replace logic is correct — it's just never
  exercised in-session (H1).

## Suspicions (unverified, one line each)

- `clean_body` truncates at `//` inside string literals — `#define URL
  "https://x"` mangles the body and may flip the whole-file validate gate to
  alias-only (src/cpp_reparse.rs:503).
- `expand_text`/`substitute` push non-ASCII bytes via `as char` — mojibake
  that doubles per fixpoint iteration (spans stay consistent)
  (src/cpp_reparse.rs:562, 1507).
- `strip_declarator_macros` has no string/comment exclusion — can blank text
  inside literals and mint phantom `recovered` pairs.
- `ensure_workspace_indexed` burns its once-latch if `workspace_root()` is
  None or the spawn_blocking task panics — pack index permanently absent that
  session (src/backend.rs:277-287).
- `pack_xfile_word_at` does blocking `fs::read_to_string` inside an async
  handler (src/backend.rs:563).
- `resolve_enumerator_enum`'s name-keyed `get_cached` may pick either enum
  when two cached headers define the same enumerator name (DashMap tie).
- Any unconditional `#define` in ANY cached file counts as globally ON in the
  reachability `defined` set — a win32-only header's defines could satisfy
  WIN32 guards on Linux.
- Refs on one enumerator surfaces sibling-enumerator slot sites (documented
  enum-level design, but user-facing noise: OP_NULL refs include
  `op_type == OP_SCOPE` spans).
- `modules-{lang}.db` rows for deleted files are skipped on warm but never
  purged — unbounded growth.
- ScopedLookup compares `to_string_lossy()` in one place and `to_str()` in
  another — divergence only on non-UTF8 paths.
- The CLI's 5s global import-resolution budget (src/main.rs:1011) silently
  degrades answers under load — by-design eventual consistency, but nothing
  in the output marks an answer as partial (QA/CI trust hazard).
- A newly created header shadowing an existing include path doesn't
  invalidate the persisted macro table.

---

## Verdict

**The arc is not yet a sound base for the template arc.** The single-file and
warm-cache happy paths are solid — the parse-repair core held up under a
deliberate crash battery, the version discipline is clean, the regression nets
are genuinely green, and the perf story is fine at current scale. But the two
newest layers — refs symmetry and the cache/index lifecycle — both violate
their own stated invariants in ways that produce *confidently wrong* answers,
and the next arc would build directly on both.

Must fix before building on top, in order:

1. **C1** — put the include-closure/language gate on `collect_from_analysis`'s
   match side and on delegation aliases. Until then, references is unusable on
   the flagship perl5 corpus (85% noise on delegated names), and every future
   feature that consumes `refs_to` inherits the splat.
2. **C2** — make pack rename either correct (pack-index routing + widened
   mask) or refused. Partial-edit rename is active data corruption.
3. **H1 (+M1/M2/H8 as one "cache lifecycle" work item)** — in-session
   invalidation for the pack index and macro caches, and analysis-input
   fingerprints in the persist keys. Until then any long-lived editor session
   drifts arbitrarily far from the source, and the template arc's caches
   would sit on the same sand.
4. **H3 + H4** — the two parse-correctness holes (brace-init corruption,
   incomplete remap). Small, contained, high blast radius.
5. **H5–H7 (+M3–M5)** — the macro-reachability and domain-typing voting
   gaps. These generate wrong hovers/defs on perl5 today and the domain
   primitive is explicitly the arc's foundation for "source-agnostic field
   slots" — fix the denominator/owner/config-universe before deepening it.

H2 and M7 are cheap, isolated fixes worth taking in the same pass.
