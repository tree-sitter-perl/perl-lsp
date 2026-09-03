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
`docs/adr/php-diagnostics.md`): guzzle `undefined-type` 1,331 of which
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

### Import-class quick-fix (2026-09-02, evening)

Fixture: `Service.php` under `namespace App` calling `Helper::go()` with
`App\Util\Helper` declared in another file, one unused `use App\Mailer;`
row. `bench/compare` codeAction probe on the `Helper` token with the
published diagnostics in context (`spec-import.json`).

| tool | diagnostics on the file | code actions on `Helper` | latency |
|---|---|---|---|
| ours | `undefined-type` App\Helper | `Add 'use App\Util\Helper;'` | 1 ms |
| Intelephense (free) | P1009 undefined type; P1003 `Mailer` declared but not used | none | 2 ms |
| phpactor | unresolved name; unused import | `Import class "App\Util\Helper"`; `Remove unused imports` | 348 ms |

The undefined-type diagnostic now publishes in the editor once the pack
index has settled (it was CLI-only).

### Unused imports (2026-09-02, late evening)

The `unused-import` lane on the corpora (`--check --severity hint`):

| corpus | first cut | after minting the missing class refs | verified real |
|---|---|---|---|
| guzzle | 21 | 0 | — |
| monolog | 43 | 2 | 2 (`Aws\Sdk`, `Monolog\Utils`, imported and never mentioned) |
| symfony demo | 37 | 0 | — |

Every first-cut row was a class the walker had not minted a reference
for: `#[Attribute]` names, `instanceof` right operands, and namespace-
qualified static receivers (`Psr7\Utils::x()`). Fixed at the source, so
goto-def, references and rename now reach those tokens too — the price is
more `undefined-type` rows on corpora without their vendor tree (monolog
158 → 257, demo 358 → 512: `PHPUnit\Framework\Attributes\DataProvider`,
`Symfony\Component\Routing\Attribute\Route`, …), which is the honest
answer for an uninstalled attribute class, the same one Intelephense gives.

### PHPUnit mocks and typeDefinition (2026-09-02, night)

Mock fixture (`spec-mock.json`: a `TestCase` stub declaring
`createMock(string $c): MockObject`, `Foo` with `bar()`, a test doing
`$m = $this->createMock(Foo::class); $m->bar();`):

| tool | hover `$m` | typeDefinition `$m` | goto-def `bar` | completion after `$m->` |
|---|---|---|---|---|
| ours | `$m: Foo` | Foo.php | Foo.php:4 | 3 |
| Intelephense (free) | `mixed $m` | none | none | 0 |
| phpactor | `MockObject&Foo` | none | Foo.php:4 | 2 |

Ours reads the doubled class from the overlay rule alone; phpactor
reads PHPUnit's `@template` docblock (with the real PHPUnit installed
Intelephense would too). Neither of the others answers typeDefinition
on the mock.

typeDefinition on monolog (`spec-typedef-monolog.json`; ours measured
with an 8 s settle after open — the harness's readiness probe was a
same-file definition, which answers before the pack index attaches, and
the first cross-file probe raced it in the unsettled run):

| token | ours | Intelephense (free) | phpactor |
|---|---|---|---|
| `$handler` (param typed `HandlerInterface`) | HandlerInterface.php:20 | none | HandlerInterface.php:20 |
| `->getFormatter()` (returns `FormatterInterface`) | FormatterInterface.php:20 | none | FormatterInterface.php:20 |
| `$record->level` (promoted property `Level`) | Level.php:31 | none | Level.php:31 |

Parity with phpactor on every row; Intelephense's free tier answers no
typeDefinition at all. Latency 2–6 ms per answer.

### Deprecations and the cold references walk (2026-09-02, night)

Deprecation fixture (`spec-depr.json`: a class, two methods — one
`@deprecated`, one `#[Deprecated]` — and a function, all used from
another file; published diagnostics captured):

| tool | rows | attribute form (`#[Deprecated]`) | notice text |
|---|---|---|---|
| ours | 4 | yes | yes (`'Legacy' is deprecated: use Modern instead`) |
| Intelephense (free) | 3 | no | no |
| phpactor | 3 | no | yes |

Cold references, editor path (guzzle `Client::__construct`, 304
references, workspace persisted, server restarted; three runs each):

| build | first answer (ms) | warm (ms) |
|---|---|---|
| decode under the connection lock, no prefetch | 224 / 245 / 271 | ~25 |
| decode under the lock, rayon prefetch | 245 / 267 / 287 (flat) | ~25 |
| decode outside the lock, rayon prefetch | 174 / 194 / 189 | ~25 |
| decode outside the lock, no prefetch | 229 / 209 / 237 | ~25 |

The lock split is what let the prefetch pay: the rehydration loader ran
zstd + bincode inside the retained SQLite connection's mutex, so
parallel decodes queued. Intelephense's first references answer on the
same site was 110 ms in the round-1 ledger; the remaining gap is the
rows→whole upgrades (10 double-decodes) and the matcher itself.

### Scoreboard refresh with the day-2 final build (2026-09-03, 00:30)

The day-2 battery (`spec2-*.json`) replayed against the final build
(a289243); the other tools' rows are the day-2 runs. Answered / probed,
with the median latency per verb.

| corpus · verb | ours | Intelephense (free) | phpactor |
|---|---|---|---|
| guzzle · definition | 1/1 · 1 ms | 1/1 · 1 ms | 1/1 · 141 ms |
| guzzle · hover | 2/2 · 2 ms | 2/2 · 13 ms | 2/2 · 96 ms |
| guzzle · signatureHelp | 1/1 · 1 ms | 1/1 · 10 ms | 1/1 · 2,040 ms |
| guzzle · typeDefinition | 1/1 · 1 ms | 0/1 | 1/1 · 15 ms |
| guzzle · implementation | 1/1 · 6 ms | 0/1 | 1/1 · 75 ms |
| guzzle · documentSymbol | 1/1 · 1 ms | 1/1 · 12 ms | 1/1 · 107 ms |
| monolog · definition | 2/2 · 1 ms | 2/2 · 1 ms | 2/2 · 2 ms |
| monolog · signatureHelp | 1/1 · 1 ms | 1/1 · 8 ms | 1/1 · 24 ms |
| monolog · implementation | 2/2 · 16 ms | 0/2 | 2/2 · 133 ms |
| monolog · documentSymbol | 1/1 · 1 ms | 1/1 · 11 ms | 1/1 · 38 ms |
| demo · completion | 1/1 · 1 ms | 1/1 · 5 ms | 1/1 · 40 ms |
| demo · typeDefinition | 1/1 · 1 ms | 0/1 | 1/1 · 3 ms |
| demo · documentSymbol | 1/1 · 1 ms | 1/1 · 4 ms | 1/1 · 5 ms |

demo's definition (0/2 for all three) and hover probes target vendor
symbols the hand-vendored tree lacks; the diagnostics rows on demo are
the same undefined-vendor-type findings for ours and Intelephense
(phpactor reports none).

| corpus | ours ready · RSS | Intelephense | phpactor |
|---|---|---|---|
| guzzle | 1.9 s · 355 MB | 1.5 s · 232 MB | 0.7 s · 126 MB |
| monolog | 1.8 s · 85 MB | 1.1 s · 195 MB | 0.5 s · 119 MB |
| demo | 1.5 s · 69 MB | 1.1 s · 187 MB | 1.6 s · 117 MB |

guzzle's RSS (213 MB in the round-1 ledger) re-measured under identical
flags (a 6 s settle after open, so the workspace index has finished):
pre-day-2 build 355 MB, final 361 MB, final without the prefetch 363 MB.
The day-2 work did not move it; the round-1 number was taken before the
index (guzzle's hand-vendored tree included) had landed.

Lanes the others carry that we do not (from the same runs): Intelephense's
"declared but not used" variable hint and its documented-vs-declared
type mismatch; both are the next diagnostics axis.

### Lanes the others carry, judged (2026-09-03, 01:05)

- **"Declared but not used" variables** (Intelephense): mirrored as
  `unused-variable` (hint, unnecessary-tagged; parameters, captures and
  dynamically-materialized scopes silent).
- **"Documented type is not compatible with the declared type"**
  (Intelephense, severity 3): every row it reported on monolog
  (`Logger.php` 192/205/234, `LineFormatter.php` 55/69) is the
  fluent-builder `@return $this` / `@return static` against `: self` —
  an idiom, not a mismatch, and one the extractor already reduces to
  the same receiver bucket. Not mirrored; noise.

### Lane counts on the corpora, hint severity (2026-09-03, 01:15, build 17977ec)

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity |
|---|---|---|---|---|---|---|---|---|
| WordPress | 111 | 198 | 94 | 24 | 0 | 611 | 283 | 15 |
| laravel/framework | 1,521 | 518 | 7,039 | 51 | 6 | 741 | 34 | 14 |
| guzzle | 2 | 0 | 1,340 | 0 | 0 | 563 | 0 | 2 |
| monolog | 11 | 13 | 263 | 0 | 2 | 36 | 4 | 0 |
| symfony demo | 0 | 5 | 519 | 0 | 0 | 0 | 0 | 0 |

Sampled: the `unused-variable` rows on guzzle were by-reference closure
captures written inside the closure and read outside, a foreach key
read only as a subscript index, and a variable captured by a nested
closure; the `undefined-property` rows on laravel were a trait's
`$this->app` (the composing class provides it) and `$this->load(...)`
(a first-class callable read as a property); WordPress's `$user->ID`
after `$user = wp_signon()` keeps an earlier branch's `WP_Error` — an
untyped reassignment does not yet reset a variable's type. The first
three are fixed in the next build; the recount follows.

After the fixes (2026-09-03, 01:55, build eb320a4 — trait `$this`,
by-reference captures, foreach key subscripts, nested-closure captures,
`$this->load(...)` first-class callables):

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity |
|---|---|---|---|---|---|---|---|---|
| WordPress | 104 | 195 | 94 | 24 | 0 | 420 | 283 | 15 |
| laravel/framework | 993 | 101 | 7,039 | 51 | 6 | 257 | 34 | 14 |
| guzzle | 2 | 0 | 1,340 | 0 | 0 | 98 | 0 | 2 |
| monolog | 11 | 9 | 263 | 0 | 2 | 24 | 4 | 0 |
| symfony demo | 0 | 0 | 519 | 0 | 0 | 0 | 0 | 0 |

`unused-variable` fell 563 → 98 on guzzle and 741 → 257 on laravel;
`undefined-property` 518 → 101 on laravel (trait bodies) and
`unresolved-method` 1,521 → 993 (trait `$this` calls). WordPress's
`unused-variable` 611 → 420 and `undefined-property` 195 are the
untyped-reassignment residual (`docs/adr/flow-narrowing.md`), measured
next. `undefined-type` is unchanged by construction: those rows are
vendor classes with no `vendor/` tree installed.

With the untyped-reassignment reset (2026-09-03, 02:20 — a reassignment
whose value cannot be typed makes the variable unknown, and a return arm
reading it makes the arm fold a disagreement instead of collapsing to the
arms that resolved): WordPress `undefined-property` 195 → 132 and
`unresolved-method` 104 → 89 with no new rows (`get_term()`'s
`WP_Term|WP_Error` shape); laravel/framework 101 → 94 and 993 → 984.
The remaining WordPress `undefined-property` rows are mostly `ID` /
`term_id` / `post_status` / `object_id` reads (17 / 11 / 9 / 7 of 132),
not yet sampled for their receivers.

Two more slices on the same rows (2026-09-03, 03:30): a documented union
(`@return WP_Term|WP_Error`, `@var A|B $skin`, `@param A|B $x`, a declared
`A|B`) is honoured as "cannot be typed" instead of letting the body's arms
or one member speak for it, every reassignment (typed or not) ends the
earlier class, a documented property outranks the constructor's write
to it, and a method call spelled with a space before its parentheses is a
call. WordPress `undefined-property` 132 → 40 → 32, `unresolved-method`
89 → 77, `arity-mismatch` 15 → 11, no new rows; laravel 94 / 981.
What remains on WordPress is `isset($tax->helps)`-style existence probes
(the read IS the question), dynamic properties on `stdClass`/legacy
classes (`$cache->ERROR` with the declaration commented out), and
`is_wp_error()` exit guards whose `@phpstan-assert-if-true` the analyzer
does not read.

Two silence rules the rows then named (2026-09-03, 04:10): a `$this` call a
DESCENDANT declares is the template-method idiom (WordPress `ftp_base`
calling `$this->_exec()` that only `ftp_pure` / `ftp_sockets` implement),
and a parent's namespace is what the `extends` clause wrote — a namespaced
`class Exception extends \Exception`, or laravel's `use Carbon\Carbon as
BaseCarbon; class Carbon extends BaseCarbon`, resolved its parent to
ITSELF, so the vendor ancestor's members read as missing. WordPress
`unresolved-method` 77 → 13; laravel 981 → 160 (457 `Carbon::now()`
rows alone); no new rows anywhere.

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity |
|---|---|---|---|---|---|---|---|---|
| WordPress | 13 | 32 | 94 | 24 | 0 | 420 | 283 | 11 |
| laravel/framework | 160 | 91 | 7,039 | 51 | 6 | 257 | 34 | 14 |

### Scoreboard replay with the night's final build (2026-09-03, 04:35, build 11334c6)

The day-2 battery (`spec2-*.json`) replayed once more against the build
carrying the night's slices (the reassignment reset, unions as
known-untypable, the template-method and self-parent rules, existence
probes). Every answered/probed cell of the 00:30 table above is
unchanged: the same definitions, hovers, signatures, implementations,
typeDefinitions and outlines, at the same 0–16 ms; the other tools' rows
are the day-2 runs. Startup and resident memory, this replay:

| corpus | ours ready · RSS | Intelephense | phpactor |
|---|---|---|---|
| guzzle | 1.4 s · 365 MB | 1.5 s · 232 MB | 0.7 s · 126 MB |
| monolog | 1.2 s · 84 MB | 1.1 s · 195 MB | 0.5 s · 119 MB |
| demo | 1.1 s · 69 MB | 1.1 s · 187 MB | 1.6 s · 117 MB |

One row the replay surfaced in our own diagnostics: guzzle's
`foreach ($options['curl'] as $option => $_)` reports `$_` as assigned
but never used — the conventional throwaway name, flagged twice.

### Lane counts, the night's final build (2026-09-03, 04:55, build 8fdb042)

The same five corpora, hint severity, fresh cache, against the build
carrying every night slice. Read against the 01:15 table above (17977ec).

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity |
|---|---|---|---|---|---|---|---|---|
| WordPress | 13 | 26 | 94 | 24 | 0 | 420 | 283 | 11 |
| laravel/framework | 160 | 91 | 7,039 | 51 | 6 | 257 | 34 | 14 |
| guzzle | 1 | 0 | 1,340 | 0 | 0 | 89 | 0 | 2 |
| monolog | 11 | 9 | 263 | 0 | 2 | 24 | 4 | 0 |
| symfony demo | 0 | 0 | 519 | 0 | 0 | 0 | 0 | 0 |

WordPress `unresolved-method` 111 → 13 and `undefined-property`
198 → 26 over the night; laravel 1,521 → 160 and 518 → 91; guzzle's
`unused-variable` 563 → 89. Every step was a diff against the previous
build with zero new rows. What remains is named in the ADR's silence
rules and the open forks: mock objects behind typed getters (122 of
laravel's 160), `is_wp_error()` exit guards (nine WordPress rows), dynamic
properties on legacy classes, and `undefined-type` rows that are vendor
classes with no `vendor/` tree on disk.

### BookStack, vendor present (2026-09-03, 05:40)

A findings-only dogfood on BookStack — the one corpus here with a real
`vendor/` tree (laravel/framework installed; the other ~150 packages
absent, which is what its 2,520 `undefined-type` rows are) — judged the
lanes for false positives and named ten shapes. Three slices later, the
member and arity lanes on it:

| build | undefined-property | unresolved-method | arity-mismatch | undefined-variable | unused-import | unused-variable | deprecated |
|---|---|---|---|---|---|---|---|
| 8fdb042 (before) | 60 | 24 | 4 | 14 | 77 | 128 | 17 |
| after promotion / spread / tuples | 4 | 23 | 1 | 13 | 77 | 128 | 17 |
| after expression-scope static properties | 4 | 23 | 1 | 9 | 77 | 128 | 17 |

The dogfood's sample tallies: `unused-import` 8/8 true, `unused-variable`
8/8 true, `deprecated` 8/8 true; `undefined-property` 8/8 false before
the promotion fix (one root cause: untyped promoted properties, which
Laravel's own event classes use); `undefined-variable` 13/14 false before
the static-property fix; `arity-mismatch` 4/4 false before the spread
fix. Hover tracks a variable's type through reassignment at every read
site probed. Left parked with fixtures: `parent::` under a same-leaf
alias, method-name case, Laravel facade aliases, an anonymous-class
return typed by the declared class, an inline `$flags = 0` argument
reported unused (`docs/PARKED.md`).
### Member-completion battery, monolog, three tools (2026-09-03, 06:40)

Four member-completion probes on monolog (`bench/compare/lspq.py`,
readiness = goto-def at `Logger.php` 176:15, fresh cache each run):
`$this->` inside `Logger`, `$handler->` over the `HandlerInterface`
receiver, `$record->` in `AbstractProcessingHandler`, and `self::` at the
level-lookup site.

| probe | ours | Intelephense | phpactor |
|---|---|---|---|
| `$this->` in `Logger` | 43 · 6 ms | 43 · 7 ms | 42 · 96 ms |
| `$handler->` (interface) | 4 · 1 ms | 4 · 2 ms | 4 · 115 ms |
| `$record->` (LogRecord) | 14 · 3 ms | 14 · 2 ms | 13 · 45 ms |
| `self::` in `Logger` | 13 · 3 ms | 13 · 2 ms | 14 · 49 ms |

The three instance probes match Intelephense item for item; phpactor
omits `__construct`. `self::` matched only after the scoped operator
reached the member half: the php pack's member kinds named the `->` forms
only, so `self::` fell to the identifier universe and answered 67 items
(every local in the function plus the members). Now `::` offers the
class's constants, its `static` members (a `static` attribute the skeleton
stamps, `is_static` on every candidate) and the pack's class-name literal;
`->` offers the instance members and hides the constants. The set is
Intelephense's exactly; phpactor's extra item is a `level: ` named-argument
snippet, not a member.

### Editor-axes battery, monolog, three tools (2026-09-03, 07:20)

The verbs an editor fires without asking — highlights, call hierarchy,
workspace symbol search, outline, folding, selection ranges, semantic
tokens, inlay hints, rename preparation — over `Logger.php` and
`StreamHandler.php` (`$S/cmp/spec-axes2-monolog.json`, ten probes, the
same harness and readiness gate as the completion battery).

| probe | ours | Intelephense | phpactor |
|---|---|---|---|
| highlight `$this->handlers` (11 sites) | 11 · 4 ms, decl = Write | 11 · 6 ms | 11 · 17 ms, decl = Text |
| highlight `$handler` param | 2 · 0.5 ms | 2 · 3 ms | 14 · 19 ms (every `$handler` in the file) |
| prepareCallHierarchy `addRecord` | 1 · incoming 15 · outgoing 5 | unsupported | unsupported |
| prepareCallHierarchy `pushHandler` | 1 · incoming 29 | unsupported | unsupported |
| workspace/symbol `Logger` | 23 (substring) · 3 ms | 45 (fuzzy, variables too) · 7 ms | 10 (classes + constants) · 217 ms |
| workspace/symbol `pushHandler` | 1 · 2 ms | 4 (fuzzy: `PushoverHandler`) · 5 ms | 0 |
| documentSymbol `Logger.php` | 55 · 0.6 ms | 118 (params/locals nested) · 3 ms | 54 · 29 ms |
| foldingRange `Logger.php` / `StreamHandler.php` | 162 / 73 (blocks + docblocks) | 0 | 0 |
| selectionRange `$this->handlers` | 11 levels | none | 1 level |
| semanticTokens/full `Logger.php` | 446 tokens · 0.5 ms | none | none |
| inlayHint (lines 575–640) | 9 parameter hints (`level:`, `message:`) · 0.7 ms | licence required | unsupported |
| prepareRename `handlers` | placeholder `handlers` | null (free tier) | range |

Folding, selection range and the outgoing-calls list were the three gaps
the battery found: pack documents answered no folds and no selection
range (both verbs were Perl-only), and outgoing calls listed the
method's property reads (`handlers`, `fiberLogDepth`, `RFC_5424_LEVELS`)
beside its callees — 14 rows where the body makes 5 calls. Folding now
follows the skeleton's scopes plus the pack's fold-only captures (php
blocks that are not scopes, docblocks as comment folds), selection range
walks the tree's ancestors for every pack, and a value-shaped member read
is excluded by its own `MemberShape`. The `$handler` highlight is scope-exact (the
parameter's two sites); phpactor's 14 is name-blind. Inlay hints were
the one axis nobody answered: ours emitted type hints only for
inferred locals and none of the range's locals are untyped. Parameter
names now ride the signature-help ladder — every positional argument of
a call whose callee resolves gets `name:` (`addRecord(Level::Debug,
(string) $message, $context)` shows `level:` and `message:`; `$context`
is the parameter's own name and shows nothing) — 9 hints over the 65
lines, 0.7 ms.

### CLI cold-start floor (2026-09-03, 07:40)

Prompted by the question whether the day's rounds had slowed the tests:
the unit target went 11.2 s → 8.5 s over the day, but `tests/language_scope.rs`
sat at ~1 s per CLI test (24 tests / 24.9 s → 38 / 36 s), and the r89
binary from the night before paid the same, so it was a standing floor,
not a regression. `PERL_LSP_PHASE_TIMING` on a two-file php fixture
(cold cache, `--check`):

| phase | before | after |
|---|---|---|
| `registry::queries` (Perl plugin pattern warm) | 502 ms | not run (no Perl file) |
| `cli::index_workspace` (0 Perl files) | 544 ms | 9 ms |
| `pack.query_compile` (assembled php query) | 2 × 540 ms, one per worker | 1 × 541 ms |
| `cli::index_pack` (2 php files) | 888 ms | 726 ms |
| wall | 2.6 s | 0.75 s |
| `tests/language_scope.rs` (38 tests) | 36.0 s | 26.2 s |

The registry warm compiled Perl-only patterns inside the workspace
indexer whether or not the walk found a Perl file; it now runs only when a
Perl build follows. The pack query cache was check-then-compile outside
its lock, so every Rayon worker that reached the php query first compiled
its own copy (one wall, N CPUs); it is single-flight now. What remains is
the tree-sitter compile of the 1,000-line assembled php query itself
(~540 ms, `pack.query_compile`) — the floor a pack-only cold start pays
once per process; it cannot be persisted (a `Query` is not serializable)
and splitting the query does not reduce the pattern analysis it pays for.

### Unimplemented contracts, three tools (2026-09-03, 08:00)

A three-file fixture: `interface Greeter { hi(string $n): string; bye(): void }`,
`abstract class Base implements Greeter` (declares `hi`, adds
`abstract protected function tag(): string`), then `class En implements
Greeter` (declares `hi` only), `class Sub extends Base {}` and `class Dyn
implements Greeter { __call(...) }`.

| | ours | Intelephense | phpactor |
|---|---|---|---|
| `En` | `unimplemented-method`: `Greeter::bye()` | P1037 `does not implement method 'bye'` | `Missing methods "bye"` |
| `Sub` | `Base::tag()`, `Greeter::bye()` | `'tag', 'bye'` | `"bye", "tag"` |
| `Dyn` (`__call`) | `Greeter::hi()`, `Greeter::bye()` | `'hi', 'bye'` | `"hi", "bye"` |
| quick-fix | "Implement missing methods" (stubs from the contracts' declarators) | none (free tier) | "Implement contracts" |
| `abstract class Base` | silent | silent | silent |

Three tools, one verdict per class. The first cut silenced `Dyn` under
Perl's AUTOLOAD rule; both other tools report it and they are right —
php checks the contract when the class is declared. Two more corrections
came from the corpora before the lane shipped: the role lookup took the
first same-leaf candidate (laravel's `MySqlConnector extends Connector`
found the Redis `Connector` interface's `connectToCluster()`, 6 rows on
BookStack's vendor tree and 10 on laravel/framework), and provision was
Perl's "a def anywhere in the candidate file" (a sibling class in the same
file provided `hi` for `Dyn`). The parent is now the namespace-pinned
candidate and provision is package-attributed for a pack whose members
are package-bound. Rows on monolog, guzzle, BookStack, symfony/demo, Slim,
laravel/framework and WordPress after the corrections: 0 — code that runs
has no unimplemented contracts.

### Missing return types, vs phpactor (2026-09-03, 09:40)

phpactor's `worse.missing_return_type` names a method without a native
return type and the type it infers; Intelephense has no such lane. On the
axes fixture both name the same two methods with the same types
(`all()` → `array`, `name()` → `string`); ours adds the "Add return type"
quick-fix (phpactor's is a separate transform).

The first cut over the corpora reported 5,085 rows on laravel/framework,
3,680 on BookStack's vendor tree and 1,314 on WordPress — with the
file-convention gate already in place. Three shapes made most of them and
each was a guess: the fold drops a `null` arm and answers from the rest
(`string` over `return "a"; … return null;`), a fluent `return $this`
spelled the class where `static` is meant, and monolog's `@method`
docblock rows were reported as bodied methods. The lane now reads the
TOTAL return (every arm witnessed, none null), never spells the enclosing
class, and skips documentation rows and closures:

| corpus | first cut | now | spellings now |
|---|---|---|---|
| monolog | 92 | 36 | array 26, string 5, bool 2, `Logger` 1 |
| laravel/framework | 5,085 | 723 | array 418, string 137, bool 56, `Envelope` 11 |
| WordPress | 1,314 | 156 | array 149, `stdClass` 2, bool 2 |

Eight sampled rows checked against the source: data providers returning
literal arrays, `__toString` returning `''`, `Str::startsWith` returning
`false`/`true`, a test stub returning `'foo'`, `initLogger` returning
`new Logger(...)` — every one a return the annotation would state truly.

### Lane counts, the day's final build (2026-09-03, 09:40, build 98dfe19)

The five corpora at hint severity, fresh cache, against the build carrying
every slice of the day. Read against the 04:55 table above (8fdb042).

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity | unimplemented-method | missing-return-type |
|---|---|---|---|---|---|---|---|---|---|---|
| WordPress | 12 | 26 | 94 | 24 | 0 | 420 | 283 | 7 | 0 | 156 |
| laravel/framework | 152 | 11 | 7,039 | 14 | 6 | 257 | 34 | 11 | 0 | 723 |
| guzzle | 1 | 0 | 1,340 | 0 | 0 | 89 | 0 | 2 | 0 | 0 |
| monolog | 10 | 9 | 263 | 0 | 2 | 24 | 4 | 0 | 0 | 36 |
| symfony demo | 0 | 0 | 519 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

Since the night table: laravel `undefined-property` 91 → 11 and
`undefined-variable` 51 → 14 (the promoted-property and expression-scope
static property rules), `unresolved-method` 160 → 152 (the same-leaf
parent pin), WordPress `arity-mismatch` 11 → 7 (spread arguments). The
two new lanes: `unimplemented-method` is 0 everywhere — code that runs
has no unimplemented contracts — and `missing-return-type` reports only
where the file's own convention is native return types (guzzle and the
demo are docblock-typed and stay silent).

### Scoreboard replay with the day's final build (2026-09-03, 09:50, build 98dfe19)

The day-2 battery (`spec2-*.json`) replayed against the final build: every
answered cell of the 00:30 table is unchanged — the same definitions,
hovers, signatures, implementations, typeDefinitions and outlines, at the
same 1–22 ms; the other tools' rows are the day-2 runs. Startup and
resident memory this replay: guzzle 1.5 s · 376 MB, monolog 1.3 s · 84 MB,
demo 1.1 s · 68 MB (Intelephense 1.5 s · 232 MB, 1.1 s · 195 MB,
1.1 s · 187 MB; phpactor 0.7 s · 126 MB, 0.5 s · 119 MB, 1.6 s · 117 MB).

### Auto-import completion, three tools (2026-09-03, 10:00)

`$g = new Gre` in `App\Web\Home` with `App\Util\Greeter` declared in
another file and not imported:

| | ours | Intelephense | phpactor |
|---|---|---|---|
| items | 5, `Greeter` among them · 3 ms | 5, `Greeter` (+ `IntlGregorianCalendar`) | 1: `Greeter (App)` · 62 ms |
| on accept | inserts `use App\Util\Greeter;` after the last import | inserts the `use` row | inserts the `use` row |

Before the slice we offered four items and no `Greeter` at all: the
identifier universe of a pack was gated on an include closure, which a
name-keyed language never has.

### phpmyadmin and composer, first sweep (2026-09-03, 10:30, build 053bf62)

Two corpora the day had not swept, hint severity, fresh cache:

| corpus | files · cold wall | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity | missing-return-type |
|---|---|---|---|---|---|---|---|---|---|---|
| phpmyadmin | 1,232 · 9.5 s | 393 → 392 | 7 | 4,210 | 2 | 32 → 0 | 1 | 667 | 8 | 0 |
| composer | 622 · 4.9 s | 45 → 44 | 2 | 2,548 | 35 | 19 | 132 | 15 | 3 | 13 |
| Slim | — | 0 | 0 | 1,285 | 0 | 5 | 4 | 0 | 0 | 5 |

phpmyadmin's `deprecated` rows are true: 661 of them are its own
`DatabaseInterface::getInstance()`, marked `@deprecated` in the source.
Its `unresolved-method` rows are the mock residual (`expects` on a
`MockObject`, the open intersection fork). The two fixes the sweep paid
for: an import used only as a namespace head inside `Sql\Column::class`
was flagged unused (the class-literal path did not record its qualified
spelling), and an `instanceof` guard narrowed the second operand of an
`&&` chain but not the third (`$package instanceof CompletePackageInterface
&& !$package instanceof AliasPackage && $package->getFunding()` — the
chain nests left, so the guard sits two levels down). Left parked with
evidence: by-ref out-parameters read as undefined variables (2 of 4
sampled composer rows). Slim's five `unused-import` rows are all true
(`use function htmlentities` never called, an aliased `PHPUnitTestCase`
never spelled, `dirname`, `RuntimeException`, `stdClass` unused).

### Closing lane sweep, every corpus, final build (2026-09-03, 11:30, build eb310e0)

Hint severity, fresh cache per corpus, one run each:

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity | unimplemented-method | missing-return-type |
|---|---|---|---|---|---|---|---|---|---|---|
| WordPress | 11 | 26 | 94 | 24 | 0 | 420 | 283 | 7 | 0 | 156 |
| laravel/framework | 152 | 11 | 7,039 | 14 | 5 | 257 | 34 | 10 | 0 | 723 |
| guzzle | 1 | 0 | 1,340 | 0 | 0 | 89 | 0 | 2 | 0 | 0 |
| monolog | 10 | 9 | 258 | 0 | 2 | 24 | 4 | 0 | 0 | 36 |
| symfony demo | 0 | 0 | 519 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| phpmyadmin | 392 | 7 | 4,206 | 2 | 0 | 1 | 667 | 8 | 0 | 0 |
| composer | 44 | 2 | 2,548 | 35 | 19 | 132 | 15 | 3 | 0 | 13 |

Against the 09:50 table the four cells that moved are the day's last three
slices: WordPress `unresolved-method` 12 → 11 and laravel `arity-mismatch`
11 → 10 (the `parent::` alias reads the parent), laravel `unused-import`
6 → 5 and monolog `undefined-type` 263 → 258 (a `X\Y::class` literal counts
as a use of `X`, and its head resolves through the import). phpmyadmin's
`unused-import` 32 → 0 is the same class-literal rule. Every other cell is
byte-identical, so the day's completion, hint and quick-fix slices moved
no diagnostic lane. Wall unchanged: phpmyadmin 9.5 s, composer 4.9 s.

### By-reference out-parameters bind their argument (2026-09-03, 12:10, build under net r116)

The undefined-variable lane, before and after the callee-resolved
binding rule (`ParamArity::binds_arg` over the extractor's bare-variable
argument sites), the `$d = &expr` declaration and the variadic parameter
declaration; hint severity, fresh cache:

| corpus | undefined-variable before | after | unused-variable before | after |
|---|---|---|---|---|
| composer | 35 | 0 | 132 | 132 |
| WordPress | 24 | 4 | 420 | 420 |
| laravel/framework | 14 | 14 | 257 | 257 |
| monolog | 0 | 0 | 24 | 24 |

composer's 35 were `$process->execute($cmd, $output, $cwd)` against
`ProcessExecutor::execute($command, &$output = null, …)` (cross-file, a
receiver typed by its parameter), `\Composer\Autoload\Init::$files` read
as a local, and one `$degradedMode = &$this->degradedMode`. WordPress's
twenty that went silent split three ways: `preg_match` / `preg_match_all`
/ `fsockopen` / `socket_getsockname` out-parameters (`$matches`, `$out`,
`$toks`, `$errno`, `$errstr`, `$port`) — php's own functions, which the
lane cannot resolve and so no longer guesses about; `strpos($wp_version,
'-src')` twice, a global a `require` sets, silent for the same reason
(the honest cost of the rule, recorded on the builtin-stubs fork); and
`function query( ...$args )`, a variadic parameter the query never
declared. The four survivors: `$wp_version` / `$wp_local_package` read
outside any call (globals a `require` sets — Intelephense reports them
too), `unset($v_header_list)` (an `unset` of a never-assigned name), and
one `$schema` read in `update_item` that the method never assigns — a
real finding `empty()` hides at runtime. The alias rule keeps the
unused-variable lane exactly where it was: without it, composer's
`$headers = &$options['http']['header']; $headers[] = …` gained a false
row. With `isset` / `empty` / `unset` reads treated as the existence
question (the member lanes' probe silence, now on the variable lane too):
WordPress 4 → 2, both `$wp_version`-family globals.

### Final lane table, every corpus, build 8fe1bc1 (2026-09-03, 12:40)

Hint severity, fresh cache per corpus, one run each — the day's closing
numbers:

| corpus | unresolved-method | undefined-property | undefined-type | undefined-variable | unused-import | unused-variable | deprecated | arity | unimplemented-method | missing-return-type |
|---|---|---|---|---|---|---|---|---|---|---|
| WordPress | 11 | 26 | 94 | 2 | 0 | 420 | 283 | 7 | 0 | 156 |
| laravel/framework | 152 | 11 | 7,039 | 0 | 5 | 257 | 34 | 10 | 0 | 723 |
| guzzle | 1 | 0 | 1,340 | 0 | 0 | 89 | 0 | 2 | 0 | 0 |
| monolog | 10 | 9 | 258 | 0 | 2 | 24 | 4 | 0 | 0 | 36 |
| symfony demo | 0 | 0 | 519 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| phpmyadmin | 392 | 7 | 4,206 | 0 | 0 | 1 | 667 | 8 | 0 | 0 |
| composer | 44 | 2 | 2,548 | 0 | 19 | 132 | 15 | 3 | 0 | 13 |

Against the 11:30 table only the `undefined-variable` column moved
(WordPress 24 → 2, laravel 14 → 0, phpmyadmin 2 → 0, composer 35 → 0):
the by-reference binding, the reference-assignment and variadic
declarations, and the probe silence. Every other cell is byte-identical.
