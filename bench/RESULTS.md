# edit-bench results ledger

Append-only: one section per round, newest last. Protocol and honest-reading
traps: `.claude/skills/edit-bench/SKILL.md`. Driver: `bench/lsp_bench.py`.

## Round 1 — 2026-07-14 — commit 2ad34e8 (v0.7.0 spike tip) — 4 cores / 15 GB

| project | files | cold ready | warm ready | cold peak RSS | warm settled RSS |
|---|---|---|---|---|---|
| bugzilla (Perl) | 194 .pm | 2.5 s | 1.2 s | 274 MB | 183 MB |
| abseil (C++) | 873 .cc/.h | 1.2 s¹ | 1.1 s | 216 MB | 265 MB |
| redis (C) | 216 TUs | 12.8 s | 1.1 s | 490 MB | 315 MB |

¹ cpp/c "ready" = first-file interactivity; the bulk index continues after.

Warm navigation (steady state): hover 0.6 ms (perl, after first) / 2.2 ms
(cpp) / 23 ms (c); cross-file goto-def 0.7 / 4.4 / 31 ms; references
worst-case 96 ms (92 refs) / 1.6 s (54 sites) / 640 ms (~250 sites);
member completion 20 / 8 / 20 ms.

Edit→diagnostics push: body edit ~620 ms (perl, 5.1k-line file) / ~197 ms
(cpp) / ~635 ms (c). Contract/header edit ~790 / ~260 / ~140 ms — redis's
server.h (included by ~every TU) diagnoses in 140 ms and post-invalidation
hover stays 4–6 ms: the Surface freshness gate holding at its worst case.

### Findings

- **NEW P1: cpp first-edit-after-cold-open → 26.2 s to diagnostics.**
  didOpen builds cached-only; the first didChange pays the full cross-file
  gather synchronously (warm: 197 ms). The worst number in the matrix.
- **NEW P1: cold answers silently partial while the index builds.**
  abseil cold references: 3.6 KB result vs 12.5 KB warm at the same
  position — looks complete, isn't. Absence-as-answer's little sibling.
- **NEW P2: documentSymbol null on big files at open** — Bug.pm (5.1k
  lines) even WARM; redis server.h both runs. The 400 ms bounded wait
  expires and the response carries null (editors heal via refresh nudge).
- **NEW P2: C goto-def stops at the header prototype** — never reaches
  the defining TU (lookupKeyReadOrReply → server.h, not db.c). Macro
  goto-def and struct-member hover/completion are correct.
- **NEW P2: `$self->` completion nearly empty on `use base` classes**
  (1 item in Bugzilla::Bug) while bareword `Bugzilla->` completes 46.
- **NEW P3: abseil member-resolution bugs** — private inline static
  (`IsInlined`) → no definition; `ToString` at status.cc:175 → wrong
  definition (lands in the enclosing operator<< signature).
- **NEW P3: perl body-edit diagnostics ~620 ms on a 5.1k-line file** —
  the synchronous per-change rebuild is the typing-responsiveness ceiling
  on big modules.
- Note: perl first-hover cold 3.0 s (on-demand enrichment of the
  receiver's module chain), then sub-ms.

## Rounds 2–4 — 2026-07-14 — commit 2ad34e8 (+bench harness) — 4 cores / 15 GB

Corpus doubled: + mojo (Perl/framework, root=lib, 112 files), fmt (C++
templates, 72 files), curl (C, root=lib, 380 TUs). Three full rounds
(cold+warm × 6 projects); medians below over rounds 2–4.

### Startup + RSS (cold ready min–max across rounds; warm settled median)

| project | cold ready | warm ready | cold peak RSS | warm settled RSS |
|---|---|---|---|---|
| bugzilla | 2.4–6.8 s | ~1.1 s | 248–297 MB | ~188 MB |
| mojo | 1.4–2.7 s | ~0.9 s | 109–121 MB | ~90 MB |
| abseil | ~1.1 s¹ | ~1.0 s | 206–217 MB | ~274 MB |
| fmt | 5.5–7.6 s | ~0.8 s | 292–331 MB | ~266 MB |
| redis | 11.6–12.8 s | ~1.0 s | 472–492 MB | 322–352 MB |
| curl | 14.0–14.2 s | ~1.1 s | 321–408 MB | ~171 MB |

### Warm navigation medians (steady state)

| verb | bugzilla | mojo | abseil | fmt | redis | curl |
|---|---|---|---|---|---|---|
| hover | 333 ms² | 0.3 ms | 2.3 ms | 1.9 ms | 19 ms | 15 ms |
| goto-def x-file | 0.5 ms | 0.4 ms | 4.3 ms | 17 ms | 31 ms | 2.9 ms |
| references | 91 ms | 19 ms | 1.62 s | 202 ms | 634 ms | 112 ms |
| member completion | 16 ms | 14–24 ms | 9 ms | 1.2 ms | 22 ms | 4.8 ms |
| body edit → diags | ~530 ms | ~50 ms | ~193 ms | ~203 ms | ~605 ms | 112–404 ms³ |
| contract/header edit | ~660 ms | ~64 ms | ~253 ms | ~271 ms | ~91 ms | ~217 ms |

¹ first-file interactivity; bulk index continues.
² Bug.pm `Bugzilla->dbh` hover only — on-demand enrichment; mojo's typed
  `has`-accessor hovers are 0.3 ms. See finding below.
³ curl body edits bimodal (first ~110 ms, subsequent ~400 ms).

### Finding updates

- **CONFIRMED P1 (stable): cpp first-edit-after-cold-open ≈ 24 s.**
  abseil body-edit-1 cold: 23.9/24.0/24.7 s across rounds — deterministic,
  not load noise. Warm: 193 ms.
- **CONFIRMED+WIDENED P1: partial/absent answers around index-build and
  enrichment windows.** New instances via the SIZE-VARIES column:
  bugzilla COLD hover 4 B (null!) vs 163 B across rounds; cold completion
  233 B vs 5.5 KB; curl cold references 866 B vs 34 KB; and — notably —
  bugzilla WARM open outline 4 B vs 53 KB and WARM hover 4 B vs 163 B.
  Not exclusively a cold problem.
- **NEW: abseil warm references 1.6 s stable** (54 sites) — vs redis 0.63 s
  for ~250 sites and curl 0.11 s for 155. The cpp references sweep cost is
  not proportional to result count; worth a profile in the fixing round.
- **Framework Perl is the speed king**: mojo `has`-accessor hover/def
  0.3–0.4 ms, `$self->` completion 77 typed items (14 ms), `has` contract
  edit 64 ms. The `$self->` weakness is SPECIFIC to `use base` classes
  (Bugzilla: 1 item) — invocant typing, not completion machinery.
- **NEW: untyped-invocant asymmetry (mojo)** — `$c->render` goto-def
  resolves by name-match but hover on `$c->app` returns nothing.
- **NEW minor: fmt warm header-REVERT 651 ms vs cold 147 ms**; curl
  goto-def→prototype replicated (C-tier pattern; fmt C++ lands on
  definitions, so it's the C path specifically); fmt explicit-instantiation
  template probe (dragonbox) answers empty — knownweak, tracked.

## Rounds 5–8 — 2026-07-15 — post-fixing-round (tip 2bdf57e) — 4 cores / 15 GB

Four rounds on the fixed binary. FIXED-BY verdicts (medians r5–8 vs r2–4):

| finding | before | after | status |
|---|---|---|---|
| cpp first-edit-after-cold-open | 24.0 s | **195 ms** | FIXED-BY 622361b (cached-only change path + background heal; fork: fast-degraded-now, option B ledgered) |
| abseil warm references | 1.62 s | **45 ms** | FIXED-BY aad409d+2bdf57e (row-narrowed sweeps; PERL_LSP_REFS_NARROW=0 kill-switch; answers byte-identical) |
| bugzilla warm outline null (WaitPolicy) | 403 ms + null | **730 ms + full 53 KB outline, every round** | FIXED-BY f988b52 (Complete wait; honesty costs ~330 ms) |
| rename missing index wait | partial edits possible | Complete wait | FIXED-BY f988b52 |
| C goto-def stops at prototype | header only | **defining TU first + prototype** (redis/curl; +2 gold rows) | FIXED-BY 498d2da (qualified-path residual forked) |
| `$self->` on `use base` | 1 item | **full method surface** (1 → 1574 in `update`) | FIXED-BY e904e7d (identity-over-rep; 2 ctor-gap sites forked) |
| bugzilla warm refs-check | 91 ms | 15 ms | rode the row narrowing |
| curl/mojo warm references | 112 / 19 ms | 10 / 2.4 ms | rode the row narrowing |
| warm settled RSS | 188/274/171 MB (bz/absl/curl) | **159/105/122 MB** | narrowing removed sweep rehydration storms |

### Costs of honesty (designed, fork-reviewable)
- abseil COLD references now ~27 s: `WaitPolicy::Complete` blocks until the
  873-file index lands instead of serving the old 402 ms PARTIAL (3.6 KB)
  answer. The fork's "Discussion needed" now has its concrete price; LSP
  progress reporting for the wait is the obvious follow-up.
- bugzilla open→outline 730 ms warm (was fast-null).

### New characterization: the curl server-context under-answer
Server-mode references on curl answer **4 sites where the CLI answers
155** — warm-deterministic, and it PREDATES the fixing round (rounds 2–4
warm was constant at the same 866 B; only cold occasionally hit the full
34 KB). Eliminated today: NOT row narrowing (identical with it off), NOT
candidate retrieval (17 candidates, byte-same as CLI), NOT rehydration
(strict-residency clean), NOT the relational block's view (whole_present).
Remaining suspect: the OPEN doc's cached-only build mints a weaker pack
target (identity/def_paths) than the CLI's fully-gathered staging, so the
matcher rejects most candidates. Evidence attached to the answer-honesty
fork entry; `PERL_LSP_REFS_DEBUG=1` prints the per-query key/candidate
counts for the next session's repro.

### Residual watch-list
dragonbox template knownweak (unchanged, tracked); fmt warm header-revert
~650 ms asymmetry; redis warm goto-def returns def-only while cold returns
def+prototype (CLI shows both, correct order — wobble, not defect);
bugzilla warm hover still occasionally null under Interactive policy (by
design — the fork's per-verb table is the redirect point).

## Spot check — 2026-07-15 — big-header outline post-WaitPolicy (tip 0485ef9)

Targeted re-verification of the rounds-1–4 "outline null on big headers"
finding, redis `server.h` (fresh shallow clone, quiet box): outline
returns the FULL 752 KB symbol tree in ~30 ms on the first pull, cold
(ready 11.8 s) and warm (ready 1.1 s) — `WaitPolicy::Complete` on
documentSymbol closed the window (bugzilla `Bug.pm` showed the same in
rounds 5–8: 52,882 B every round). Blocking Complete waits now also
surface as LSP work-done progress once they exceed 500 ms
(`bounded_wait_with_progress`), so the honest block is visible in-editor
instead of reading as a hang.

## Residual closed — 2026-07-15 — the ctor-gap 2/60 (tip 900b335)

The invocant fork's residual (`my $self = $class->new(...)` through a
cross-file base ctor → `$self` untyped) was a bug, not a fork: the
receiver-polymorphic ctor machinery existed but the statement/assignment
bless forms never reached it. Fixed (`push_receiver_bless_witness` +
receiver threading through the Variable hop, EXTRACT_VERSION 166).
Verified on real Bugzilla: goto-def on `$self->id` right after
`my $self = $class->new($param)` in `Bug::check` resolves to
`Bugzilla::Object::id` over five same-named decoy `sub id`s, cross-file
through `new` → `new_from_hash` → statement bless. Gold 436/17/0/0/0
(two new substrate rows lock the post-bless hover typing).

## Residual closed — 2026-07-16 — curl server-vs-CLI references (degraded-open window)

The warm-deterministic 4-vs-155 references undercount was the
DEGRADED-OPEN window: did_open's cached-only pack build answers until
the background full-gather heal lands, and the bench's back-to-back
open→references always asked inside it (immediate ask 826 B; the same
ask 15 s later 32,665 B — the full warm answer). Fixed: `degraded_open`
marks the window, `await_open_full` holds references/rename/
implementations (Complete policy) until the heal lands — 280 ms warm on
curl for the byte-identical full answer; cold pays the gather with
work-done progress visible. Outline/hover/completion stay fast-path
(no cross-file read). Server and CLI now agree.

## Round 9 — 2026-08-05 — commit 74de442f (post layered-restructure + 24-slice rework) — 4 cores / 15 GB

First round since 2bdf57e; the intervening span (94 commits of spike work +
the restructure + the rework arc) was unbenched. Scenarios re-anchored to
fresh upstream HEADs first (d672d525) — abseil/curl/fmt coordinates moved,
so single-probe deltas against Rounds 5–8 carry that caveat where noted.

### Startup + RSS

| project | cold ready | warm ready | cold peak RSS | warm peak RSS |
|---|---|---|---|---|
| bugzilla | 2.5 s | 1.2 s | 253 MB | 161 MB |
| mojo | 1.5 s | 1.0 s | 125 MB | 86 MB |
| abseil | 1.2 s¹ | 1.1 s | 214 MB | **101 MB** |
| fmt | 6.7 s | 0.9 s | 267 MB | 158 MB |
| curl | 13.8 s | 1.1 s | 305 MB | 162 MB |
| redis | 12.4 s | 1.0 s | 482 MB | 293 MB |

All within or better than the Rounds 5–8 bands; abseil warm RSS is the
standout (**274 → 101 MB**, −63% — the residency/eviction discipline of the
storage arc paying off in steady state; bugzilla warm 188 → 161 MB and fmt
warm 266 → 158 MB rhyme with it).

### Warm navigation medians

| verb | bugzilla | mojo | abseil | fmt | redis | curl |
|---|---|---|---|---|---|---|
| hover | **0.8 ms** (was 333²) | 0.3 ms | 3.4 ms | 3.3 ms | 21 ms | 16 ms |
| goto-def x-file | 0.7 ms | 0.4 ms | 6.0 ms | 18 ms | 32 ms | 2.7 ms |
| references | **16 ms** (was 91) | **1.6 ms** (was 19) | **85 ms** (was 1.62 s) | 146 ms | 1.73 s³ | 703 ms⁴ |
| member completion | 20 ms | 11–17 ms | 2.9 ms | 2.8 ms | 9.9 ms | 4.5 ms |
| body edit → diags | ~715 ms⁵ | ~70–86 ms | ~195 ms | ~206 ms | ~650 ms³ | ~395 ms |
| contract/header edit | ~890 ms⁵ | ~83 ms | ~275 ms | ~950 ms⁶ | ~730 ms³ | ~224 ms |

¹ first-file interactivity; bulk index continues (references waits it out:
  cold refs 25.8 s honest-full vs 85 ms warm).
² the Bug.pm `Bugzilla->dbh` on-demand-enrichment hover: the open-doc
  enrichment artifact (D1) killed the 333 ms in-publish enrichment stall.
³ redis warm-slower-than-cold inversion — see finding below.
⁴ curl `Curl_conn_meta_get` refs re-anchored (upstream moved the call
  sites; result 33.6 KB) — not comparable to the old 112 ms row.
⁵ bugzilla diag latency drifted up from ~530/660 ms — see finding.
⁶ fmt warm header edit+revert both ~950 ms where cold is 129/652 ms; the
  old ledger's "~650 ms revert asymmetry" grew a sibling — watch.

### Findings

- **FIXED-BY 74de442f (this round's catch): C++ serving path wedged by the
  gather single-flight × rayon pool deadlock.** Introduced by H9-3
  (d0ec2acb, unbenched span): an open-doc gather claims a file's flight and
  injects level par_iter work into the global rayon pool while a bulk-index
  pool worker parks on the same flight's condvar — a parked worker buries
  its stolen continuations, the pool wedges, `index_pack_languages` never
  returns, the pack ReadyGate never opens. Symptom: fmt never ready,
  hover/def null at the 400 ms Interactive cap, references null after the
  full 120 s Complete cap, os.cc diagnostics silent; abseil same disease
  milder (warm refs 120 s→null, no body diags). Fix: a rayon worker never
  parks on a foreign flight — it computes a private duplicate and does not
  publish (claimant owns population). Pinned by
  `gather_cache_rayon_worker_never_blocks_on_a_foreign_flight` (hangs
  pre-fix). After: fmt cold ready 6.7 s / refs 71 ms / 1.3 KB; abseil warm
  refs 85 ms / 12.7 KB — at or above the Rounds 5–8 baselines. Neither
  cargo test, gold, nor the CLI mirrors can see this failure mode — only
  server-mode rounds exercise open-doc gather × bulk index concurrency.
- **NEW: redis warm runs slower than cold on the index-heavy verbs** —
  refs 383 ms cold → 1.73 s warm, contract diag 72 → 730 ms, body diags
  137 → ~650 ms. Warm serves from SQLite rows/rehydration where cold has
  everything resident post-index; the inversion says the warm lane pays
  rehydration on the hot path for redis-sized files. Candidate for a
  storage-arc look; not user-catastrophic (still sub-2 s) but the trend is
  wrong.
- **NEW: bugzilla edit→diagnostics drifted up** (~530/660 → ~715/890 ms).
  Enrichment now derives an artifact per publish (D1) where it previously
  mutated in place; suspect the derive-clone on Bug.pm-sized analyses.
  Watch; a per-size threshold or reuse would claw it back.
- **KNOWN (A4 ledger note): CLI hover lane gap** — `Bugzilla->dbh` answers
  163 B in server mode but empty via `--hover` CLI (CLI renders only the
  model hover; the set's binding lanes are server-side). Tracked in the
  rework hitlist's A3/A4 LANDED notes as the renderer-placement residual.
- **KNOWN (still present): redis warm goto-def def-only wobble** — cold
  returns def+prototype (440 B), warm def-only (220 B); CLI shows both.
  Unchanged from the Rounds 5–8 watch-list.
- dragonbox template knownweak: both probes now answer small non-null
  payloads (212/133 B) — the lazy template projection edge moved; probes
  left tagged until asserted meaningful.

## Targeted A/B — enrichment copy: bincode round-trip → clone

Not a full round; one scenario, run as the answer-identity net for the copy
change (`docs/adr/enrichment-build-cost.md`). mojo (`lib`, server path via
`lsp_bench.py`), five paired cold runs, `0b3e2fc0` vs branch.

- **Answer identity: IDENTICAL across 5 pairs × 16 steps** (every step's
  `result_size` byte-equal). That is the local stand-in for the Koha
  `references`-on-`store` 284,617 net, which needs a corpus this box lacks.
- Step total: before median 231 ms (145–236), after median 150 ms (109–254).
  **The medians favour the branch and the ranges overlap, so this run does
  not establish a wall-clock win** — mojo builds only 86 enriched copies
  (99.1% overlay hit rate), so the per-build saving has almost nothing to
  multiply. The measured win is per-build (27.8%) and shows up where builds
  are numerous, which is the 138k case, not this one.
- Ready: before median 1,484 ms, after 1,464 ms — unchanged, as expected;
  enrichment is not on the startup path.

Worth keeping as a caution: a scenario whose overlay hit rate is ~99% cannot
measure a change to what a MISS costs. Reach for a corpus where the overlay
thrashes, or count builds first and stop if the count is small.

## 2026-08-30 — the harness measures itself in: first KPI baselines (sha ce16a564)

The JSONL harness (bench/MEASURE.md) landed with per-file exclusive-time
lanes, and paid for itself before merging:

- **`finalize_post_walk` was the interactive path's biggest build phase** —
  41% of e2e build-family time, invisible to every `--check` measurement.
  The exclusive-time split put 98.3% on `seal_unrowed_attachment_names`
  (O(attachments × symbols), a String allocation per comparison). Fixed:
  **10.99 → 0.049 ms/call (~220×)**, both arms on buildable binaries.
- **The instrument was held to its own standard**: armed gold ran +15% over
  bare; an A/B against the pre-lane binary pinned it to the file lane's
  per-drop String+lock — sitting inside parents' exclusive times. Per-thread
  staging brought armed runs inside noise (28.7 s vs 30.3 s bare).
- **First KPI baselines** seeded to `bench/baselines.jsonl` (reproducible-8,
  clean tree, n=3 each). Headlines: Znuny `--check` 52.0 s / 9.31 GB cold,
  24.2 s / 9.23 GB warm; FHEM 65.0 s / 2.26 GB cold, 53.5 s / 2.17 GB warm;
  BMO 9.7 s / 0.68 GB cold, 2.0 s / 0.71 GB warm.
- **FHEM's memory variance collapsed**: warm peak was 3.9 GB with a 61%
  spread on 2026-08-27; it is 2.17 GB at ~5% now — the seal fix plus the
  consult pre-filter, not a measurement artifact (same harness, same box).
  Its warm/cold wall ratio moved 0.90 → 0.82; the `package main` consult
  sweep remains the dominant term and the open lever.
- Editor-surface KPIs now flow through `lsp_bench.py --jsonl` into the same
  store (Bugzilla spot-run: ready 624 ms cold, first didChange→diagnostics
  2.24 s cold then 260–650 ms, server RSS 406 MB). **Editor baselines are
  deliberately not seeded yet** — they must come from a quiet box, and the
  batch sweep owned the box today.

## 2026-08-30 — SharedKeys: the N×S clone product deleted at the type (sha pending)

Znuny's 9.3 GB `--check` root-caused to one representation choice: by-value
transport of `HashWithKeys` meant every consumer querying a variable typed
by a big generated literal took delivery of the whole key list — N sites ×
O(S), in three consumers (the sweep's deref lane: 7.3 GB + 17.6 s; the
build's owner-upgrade pass: 16.1 s to compute 9,724 `None`s; the finalize
seal, fixed earlier). `SharedKeys` (Arc'd key list, ptr-eq equality fast
path, copy-on-write for the one mutation site) deletes the product with
zero consumer changes and rule #10's rich-type contract intact.

Measured (single runs; deltas clear baseline spreads by orders of
magnitude): Znuny cold 52.0 s / 9,314 MB → **19.0 s / 1,973 MB**; warm
24.2 s / 9,232 MB → **6.3 s / 1,900 MB**. Glyphs.pm standalone 34.4 s /
7.58 GB → **15.1 s / 95 MB**. FHEM warm 53.5 → 48.1 s, memory flat (its
mechanism is consult volume, not shapes). Gold 503/0 warm-clean, e2e
121/0, unit 1687/0 — answers unchanged by the exact-assertion net, not
just by argument. No cache invalidation: SharedKeys serializes via
delegating impls, byte-identical to the Vec it replaced.

Known residual, named: Glyphs standalone still spends 7.9 s (build) +
6.7 s (sweep) re-FOLDING the ~9.7k witnesses on one attachment per query —
N queries × W witnesses, clone-free but not fold-free, temporal semantics
make naive memoization wrong. Separate design question; the checked-in
baselines will hold the line meanwhile.

## 2026-09-02 — PHP answers vs Intelephense (free) and phpactor (sha ee285f2)

Three servers over stdio (`bench/compare/`), same probe battery, same
checkouts, no `vendor/` in any root (so vendor-defined symbols are dark for
all three equally): guzzle (10 probes), monolog (8), symfony/demo (3).
Intelephense 1.x free tier via npm, phpactor 2026.06.23.0 phar with
`index:build` run first, ours = the r69 binary (`--features cpp,php`),
cold cache. One run each; latencies are first-call numbers on a shared box.

| | ready (guzzle / monolog / demo) | RSS at end (guzzle / monolog / demo) |
|---|---|---|
| ours | 3.5 s / 1.3 s / 1.1 s | 213 / 73 / 63 MB |
| intelephense | 0.8 / 0.7 / 1.7 s | 316 / 203 / 197 MB |
| phpactor | 7.2 / 0.8 / 0.3 s (+ index:build 14 / 9 / 10 s) | 133 / 96 / 80 MB |

Answers. Every goto-definition probe (13: `$this->method()`, `Class::static()`,
`new Foo()`, trait method, property, `parent::__construct`, a `use` leaf, a
typed parameter) lands on the same symbol in all three tools (Intelephense
anchors the range at the docblock, the others at the declaration). Reference
counts against grep truth:

| probe | grep | ours | intelephense | phpactor |
|---|---|---|---|---|
| guzzle `Client::sendAsync` | 10 sites + decl | 12 (incl. interface decl) | 12 | 10 (misses `ClientInterface` decl, `Pool.php` site) |
| guzzle trait `request` | 6 trait sites | 61 | 60 (no interface decl) | 59 (no `Client::request` impl) |
| guzzle `new Client(` | 304 | 304 | 304 | 304 |
| guzzle `CookieJar::count` | decl | 1 | 2 — the second is `MockHandler::count()`, another class's same-named method | 1 |
| monolog `Logger::$handlers` | 10 + decl | 11 | 11 | 11 |
| monolog `pushHandler` | 50 sites + decl | 42 | 42 | 42 |
| monolog `addRecord` | 15 + decl | 16 | 16 | 16 |
| demo `Post::getTitle` | 4 + decl | 5 | 5 | 5 |

(`pushHandler`: all three agree at 42; the grep's extra 8 are `->pushHandler(`
on receivers no tool types — the count is the tools' shared ceiling, not a
gap of ours.) Rename: ours = phpactor on both probes (a private method: 3
edits; a protected property read by a subclass: 16 edits across
`StreamHandler` + `RotatingFileHandler`); Intelephense free returns none.
Completion after `$this->`: identical member sets (64 / 43 items) in all
three. Hover: ours shows the signature and the inferred type
(`handlers: list<HandlerInterface>` where phpactor shows the docblock's
`array<int,HandlerInterface>`), but NOT the docblock description text both
others render — the one visible gap in this battery. Latency: ours is
single-digit to tens of ms warm like the others, except the first
cross-file references walk on guzzle (1,057 ms vs Intelephense 110 ms) —
the cold rehydration cost the R5-4 attribution names.

## 2026-09-02 — the other tools' axes: signature help, docblocks, outline, diagnostics (sha pending net)

Same three servers, same driver (`bench/compare/lspq.py` grew
`signatureHelp`, `codeAction`, `implementation`, `typeDefinition`,
`documentSymbol` and a `publishDiagnostics` capture). A two-file fixture
(`Service` calling a `Mailer` with every mistake an editor should catch)
plus the round-1 corpora, guzzle and monolog hand-vendored with their PSR
dependencies (composer's dist downloads are refused by the sandbox proxy).

| axis (fixture) | ours before | ours after | Intelephense free | phpactor |
|---|---|---|---|---|
| signature help in `$this->mailer->send($who, │)` | none | `send(string $to, string $subject, string $body = '') : bool`, active 1, docblock | 3 params, active 0/1 | 3 params |
| hover on a method | signature + type | + docblock summary | docblock | docblock |
| document outline of a class | 2 flat items | class with its members | 9 (params too) | 3 |
| diagnostics on `Service.php` | 0 | 8: not enough / too many arguments, undefined method ×2, non-public access, undefined variable, undefined type ×2 | 8 real + 2 "declared but not used" | 0 (phpactor lints docblocks) |
| type definition on `$this->mailer` | none | none (open) | none (licensed) | `Mailer` |
| implementations of an interface method | (works) | (works) | none (licensed) | works |

Diagnostics on the corpora (`--check`, every remaining row read —
`docs/hitlist-php-round8.md`): guzzle `undefined-type` 1,331 of which
1,091 are one missing test class and 208 the unvendored PHPUnit;
`unresolved-method` 2; `undefined-variable` 0; `non-public-access` 0.
monolog `undefined-type` 158 (PHPUnit attributes, optional transports),
`unresolved-method` 9 (PHPUnit `createMock` receivers), everything else
0. symfony/demo without vendor: 358 undefined types, the same storm
Intelephense reports (35 on `BlogController.php` alone). On the
battery's own opened files Intelephense's remaining rows are lanes we
do not have yet: unused symbols, deprecations, documented-vs-declared
type checks, argument type checks.

Where the answers differ, ours read as the more honest one twice:
Intelephense counts a same-named `count()` on another class as a
reference (round 1), and its free tier answers neither implementations
nor type definitions. Where they lead: the unused/deprecated/type-check
lanes, and vendor stubs for the global namespace (we carry none, so
`\Exception` and friends are simply silent).

### `instanceof` narrowing, round 2 (2026-09-02, evening)

Fixture: an interface-typed parameter (`Shape $s`) with the eight guard
shapes, hover on the receiver at the member call (`bench/compare`,
`spec-narrow.json`; `ours` = the round-2 build).

| shape | ours | Intelephense | phpactor |
|---|---|---|---|
| `if (!$s instanceof Circle) { return; }` then `$s->` | Circle | Circle | Circle |
| `if (!($s instanceof Circle)) throw …;` then `$s->` | Circle | Circle | Circle |
| `assert($s instanceof Square);` then `$s->` | Square | Square | Square |
| `$s instanceof Circle && $s->…` | Circle | Circle | Circle |
| `$s instanceof Square ? $s->… : 0` | Square | Square | Square |
| `match (true) { $s instanceof Circle => $s->… }` | Circle | Circle | Circle |
| `foreach … { if (!$i instanceof Square) continue; $i->… }` | Square | Square | **Shape** |
| after the loop, `$s->` | Shape | Shape | Shape |
| negated guard whose body does NOT exit, then `$s->` | Shape | Shape | Shape |

Before round 2 ours answered `Shape` on the first seven rows. The
diagnostics counts on guzzle / monolog / demo are unchanged by design:
the interface silence rule stays (member subjects, method guards and
`is_a()` leave the interface standing), so narrowing pays off on hover,
completion and goto-def over interface receivers, not on the linter
surface.

Real sites (monolog, `spec-narrow-monolog.json`):

| site | ours | Intelephense | phpactor |
|---|---|---|---|
| `MandrillHandler::__construct`: `$message` (`callable\|Swift_Message` param) after `if (!$message instanceof Swift_Message) { throw … }` | Swift_Message | Swift_Message | Swift_Message |
| same, before the guard | untyped | mixed | untyped |
| `Logger::log`: `$level` after `if (!$level instanceof Level) { … $level = static::toMonologLevel($level); }` | Level | Level | mixed\|Level |

Verdict: parity with Intelephense on every narrowing shape probed; ahead
of phpactor on the loop `continue` form and the reassigning non-exit
guard.
