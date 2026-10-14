# Epic 14 — The per-file stall: C/C++ beta → GA

> **Status:** scheduled (14th) but **high priority by impact** — this is
> the one place the product is measurably unusable rather than merely
> incomplete.
> **Design owner-docs:** `docs/cpp-status.md` §"The scaling limit:
> measured" and §"What would lift the scaling caveat",
> `docs/prompt-macro-salvage-scaling.md` (the ranked fixes),
> `docs/prompt-vendored-dirs.md` (the role-remap lever).

## Mission

`cpp-status.md` states the gate plainly:

> **What beta does not promise is scaling.** … yes for small-to-medium
> projects, not yet at Godot's size. A performance limit, not a
> correctness one.

And it names the next step, unambiguously:

> The per-file stall, specifically — an aggregate number will not settle
> it. The gate is a large generated header (`d3d12.h`,
> `vulkan_handles.hpp`) analysed in interactive time, **and no
> measurement of total wall across a corpus substitutes for that. Nobody
> has profiled where those 30–66 seconds go; that is the next step and
> it has not been taken.**

This epic takes that step, then fixes what it finds.

## The measurement, as it stands (dated — re-take before trusting)

From `cpp-status.md`:

| project | C++ files | result |
|---|---:|---|
| fmt | 80 indexed | 4.2 s, 0.50 GB — fine |
| Godot | 7,041 | **did not complete in 4 minutes**; killed |

Godot's **memory behaviour is good** — RSS plateaus flat at ~2 GB across
7,041 files, better per-file than the Perl side manages. The problem is
wall time, concentrated in individual files:

```
[stall] 66s  thirdparty/vulkan/include/vulkan/vulkan_handles.hpp
[stall] 34s  thirdparty/vulkan/include/vulkan/vulkan_raii.hpp
[stall] 32s  thirdparty/directx_headers/include/directx/d3d12.h
[stall] 31s  thirdparty/ufbx/ufbx.c
```

**Every stall is a large generated or vendored header.** `d3d12.h` alone
is 1.5 MB. A per-file cost of 30–66 seconds is unusable interactively no
matter what the aggregate looks like.

Note the failure mode is the **opposite** of the Perl side's: C++ is
memory-healthy and wall-pathological; Perl's FHEM shape is
memory-pathological. They share no mechanism.

## Read first

1. `docs/cpp-status.md` — whole; it is short and it is the charter.
2. `docs/prompt-macro-salvage-scaling.md` — the machinery, the two
   sibling defects, and the **ranked** fixes with a recommended order.
   The ranking is not advisory; it was reasoned about and endorsed.
3. `docs/prompt-vendored-dirs.md` — the role-remap design, its two
   levers, and the survey evidence behind them.
4. `docs/adr/macro-handling.md`, `docs/adr/reparse-stratification.md`,
   `docs/adr/cpp-templates.md` — the landed machinery this profiles.
5. `docs/adr/instrument-blindness.md` — **before you add a single
   timer.** The instrument distorting the measurement is a mistake this
   project has already made and paid for.

## Phase breakdown

### Phase A — profile the stall (no fixes)

**Ship nothing but a number.** The owner doc is explicit that nobody has
attributed these seconds, and every fix below is a guess until someone
does.

1. Reduce to a repro: one file, one open, wall-clocked. `d3d12.h` and
   `vulkan_handles.hpp` are the named specimens; put them in the C++
   corpus tree (`~/personal/cpp-bench` is where the C/C++ scale work
   lives) so the repro is reproducible on another box.
2. Attribute with the existing instrumentation, not new `eprintln!`s:
   - `PERL_LSP_PHASE_TIMING` + `timings::phase` / `tphase!` for the
     named phases. `PackDriver::analyze_with_path`'s pipeline is **seven
     named phases** (gather context → transform+parse → extract → remap
     spans → enrich skeleton → `into_file_analysis` → register post-build
     hooks) — the attribution should land on one
     of them before it lands anywhere finer.
   - `bphase!` → `ghost_stats::timed` for per-file regions, and
     `ghost_stats::count_by` for per-file quantities. **A printed line
     per entry only suits a region entered once per run**; a per-file
     region printed per entry cost one run 3.2M lines and 43 minutes and
     measured a run that no longer resembled the one being measured.
   - `PERL_LSP_GHOST_JSON[_DIR]`, `PERL_LSP_TIMINGS_JSON[_DIR]`,
     `PERL_LSP_HEAP_JSON[_DIR]` for the machine-readable sinks; the
     `_DIR` variants write one file per process.
3. **Hypotheses to discriminate between**, in the order the docs make
   likely — but let the profile decide, not this list:
   - **Macro-expansion salvage bisection.** The prime suspect. Per
     `prompt-macro-salvage-scaling.md`, the salvage machinery does
     *blind* bisection over macro names, each probe a fresh full parse
     of a 16k-line file. On a 1.5 MB generated header the arithmetic is
     brutal and the doc already documents the shape on `op.c`.
   - **Reparse stratification / splice mapping** over a file with tens
     of thousands of expansion sites.
   - **Template extraction** — `vulkan_raii.hpp` and
     `vulkan_handles.hpp` are template-dense; the spec-selection ladder
     and `substitute_type_params` run per instantiation.
   - **Query extraction itself** — one combined query over a 1.5 MB
     tree, or a pathological pattern.
4. **Acceptance:** a written attribution — "N of the 66 seconds are in
   phase X" — in the PR and in `cpp-status.md`. Three runs, dated. **No
   code fix in this phase.** If the profile contradicts the ranked fixes
   below, the ranking loses and the profile wins.

### Phase B — macro-salvage fixes, in the doc's recommended order

Only the ones the profile justifies.

1. **#2 — per-name expansion-verdict cache** (the doc's recommended
   first move: smallest diff, biggest payoff, user-endorsed). A macro
   name's expansion safety is *stable*: classify each name once as
   `{clean-expand | blank | drop}` and persist the verdict keyed by
   header-set/toolchain — **it rides the SQLite blob machinery
   already.** `pTHX_` gets classified blankable on first open and is
   never bisected again, so the probe budget goes only to genuinely
   ambiguous names.
2. **#1 — damage localization instead of blind bisection** (the
   principled follow-up, and the one that also covers
   position-dependent names). The first full-expansion parse already
   surfaces the ERROR node's byte range; map it back through the splice
   map to the covering macro-name group → the culprit is known in O(1),
   no search. Turns O(names) blind probes into O(bad-groups) targeted
   ones.
3. **#3 — incremental reparse per probe** (the general lever, most
   invasive). tree-sitter can reparse just the edited subtree given the
   prior tree; this attacks cost-per-probe rather than probe-count, and
   the doc estimates the budget could rise 10–100× nearly free. Levers
   #3/#4 are for "if #1 and #2 prove insufficient" — #4
   (`is_context_free_safe`) has already landed.
4. **Acceptance per fix:** the specimen file's wall, three runs, dated,
   before and after; plus the C/C++ gold suite unmoved. A salvage
   change that speeds up the file and loses symbols is a regression —
   assert the symbol count too.

### Phase C — the interactive escape hatch

Even a fixed salvage path will meet a file that is genuinely too big.
The server must degrade, not stall.

1. A per-file analysis budget with a declared degradation: past the
   budget, fall back to the cheapest honest analysis (skeleton
   extraction with macros blanked rather than salvaged) and **mark the
   analysis degraded** — the `degraded` flag already exists on
   `FileAnalysis` (it is `serde(skip)`, which is why the enrichment
   overlay clones rather than round-trips: a round-trip silently reset
   it and an enriched copy of a degraded analysis claimed to be whole).
2. Degradation must be **visible**: the status/`--languages` surface or
   a diagnostic should be able to say "this file was analysed in
   degraded mode", so a user with a mysteriously incomplete answer can
   find out why. A silent budget is worse than a slow file.
3. The existing precedents to follow rather than reinvent: the 1 MB cap
   (`scaling-limits.md` §6 — note "why the same file measures 0.39 s or
   29 s"), the depth gate and queued descent from the P0 stack-overflow
   fix, and `util/watchdog.rs`.
4. **Acceptance:** the specimen file opens in interactive time
   (single-digit seconds) or degrades visibly; a test pinning that a
   degraded analysis is never cached as whole.

### Phase D — the vendored-dirs lever

The workaround `cpp-status.md` documents today is *"excluding
`thirdparty/` from the workspace avoids most of it, since that is where
the giant headers live in every project we measured."* Make that a
supported, discoverable feature rather than folklore.

Implement `prompt-vendored-dirs.md`'s design as written — it is
survey-backed and the decisions are made:

1. **Vendored = the DEPENDENCY role, in-tree.** A remap to the existing
   `RoleMask` DEPENDENCY tier, **not a new flag consumers check.**
   Everything follows from the role: no sweep/`--check` diagnostics, no
   dead-export queue entries, demoted workspace-symbol ranking — while
   goto-def INTO the vendor keeps working. This is rust-analyzer's shape
   exactly.
2. **Two levers, not one:** `vendor` (analysed, navigable, silent) and
   `exclude` (not indexed at all — for build OUTPUT like `blib/`, which
   is a copy of `lib/` and mints duplicate definitions if indexed).
3. **Rename across the role boundary REFUSES, loudly.** A partial edit
   set breaks builds invisibly; a vendored fork can call workspace code.
4. **Dead-export asymmetry:** vendor stops PRODUCING dead-code entries
   but keeps COUNTING as a referencer.
5. **Detection is layered and PRINTABLE.** Convention defaults on by
   default, enumerable via a dump verb, overridable; precedence user >
   inner project > outer. golangci-lint deleted its invisible built-in
   list as a support burden — that is the lesson.
6. **Acceptance:** Godot indexes with `thirdparty/` auto-detected as
   vendor; goto-def into it still works; `--check` is silent on it;
   the dump verb prints why each directory was classified.

### Phase E — re-measure and re-tier

Re-run the Godot measurement. Update `cpp-status.md` — the table, the
stall list, the "what would lift the caveat" section, **all with
dates.** If the gate is met, bump `Maturity` and say what the new tier
promises.

## Non-goals

- Rewriting the macro model. `adr/macro-handling.md` and the
  config-variant model are landed and correct; this is a cost problem.
- Chasing aggregate corpus wall. The owner doc explicitly rejects it as
  a substitute for the per-file number.
- The parked template residue (deduction/dependent-type rungs,
  template-template params, `extern template` ERROR parse) — correctness
  work on its own brief.

## Language-pack beat

C/C++-specific by subject; **cross-language in three of its four
mechanisms**, and each should be built to be inherited:

1. **The per-file budget + degraded fallback (Phase C) is engine-level.**
   Any language can meet a pathological file — Perl already has the 1 MB
   cap and the depth gate. Build one budget mechanism with a
   per-language policy, not a C++ one.
2. **Vendored-dirs (Phase D) is explicitly cross-language.** The
   convention defaults `prompt-vendored-dirs.md` lists are Perl's
   (`cpan-lib`, `local/lib/perl5`, `extlib/`, `inc/`) plus linguist's
   generic regexes — the design was written from the Perl side and C++'s
   `thirdparty/` is the same shape. `scaling-limits.md` §2 ("Vendored
   dependency piles — MISLEADING, not slow") is the Perl-side evidence.
   **One implementation, per-language default lists**, and the dump verb
   prints all of them.
3. **The profiling method (Phase A) is the reusable product.** If
   attributing a pack language's per-file cost requires ad-hoc work,
   the next language pays it again. Whatever phase instrumentation
   Phase A needs, land it in `util/timings.rs` and
   `PackDriver::analyze_with_path`'s named phases so every pack language
   inherits the attribution.
4. Only the macro-salvage fixes (Phase B) are genuinely C-family. Keep
   them there.

## Scaling beat

This epic **is** a scaling beat, so the section states the discipline
instead:

1. **Per-file, not aggregate.** The gate is one large generated header
   analysed in interactive time. Report the specimen's wall, not the
   corpus's.
2. **Three runs, dated, every number.** `cpp-status.md`'s existing
   numbers are dated; keep it that way. A number without a date rots
   silently — abseil's warm RSS sat recorded at 34 MB and 47 MB in two
   ADRs, both ~2× low seven weeks later.
3. **Watch RSS while fixing wall.** Godot's memory behaviour is
   currently *good* (~2 GB flat across 7,041 files, better per-file than
   Perl manages). A verdict cache (Phase B #1) and an incremental-reparse
   lane (#3) both hold state. Report peak RSS beside every wall number
   so a wall win that costs 4 GB is visible immediately.
4. **The C++ scale tree is its own** — `~/personal/cpp-bench`, with
   abseil at `~/personal/cpp-bench/abseil-cpp`. Godot goes there too.
   `$PERL_CORPORA` is the Perl root and stays separate.
5. **Do not let a long measurement run inside an agent worktree** —
   unchanged agent worktrees get swept mid-run. Detach long measurement
   work outside them.

## Verification gate

`cargo test --features cpp` · gold built `--features cpp` with
**`lang-skip 0`**, 0 FAIL / 0 XPASS · the pack e2e lane · Perl substrate
audit at exact parity (Phases C and D touch shared machinery) ·
**the specimen-file wall + peak RSS, three runs, dated, in the PR and in
`cpp-status.md`** · the Godot run, or an explicit statement of what
still blocks it.

## Sizing

Medium — but front-loaded with genuine unknowns. Phase A may take as
long as B and is the only phase that cannot be skipped or reordered.
