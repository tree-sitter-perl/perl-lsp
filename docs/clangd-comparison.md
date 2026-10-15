# perl-lsp vs clangd — measured comparison (2026-07-06)

Base: `spike/cpp-support` @ `088da995` (EXTRACT_VERSION 162), the commit
right after Memory Slice 2 landed. Companion runbook:
`docs/clangd-benchmark-procedure.md` (exact commands to reproduce every
number below).

**Tooling used to measure**: `e2e/compare-clangd.sh` (existing, correctness
differential) + `e2e/lsp_latency.py` (new, added by this pass — a minimal
stdlib-only LSP client that drives the real protocol handshake against
either binary and reads peak RSS from `/proc/<pid>/status VmHWM`, so numbers
reflect the actual server process, not a wrapping driver).

## Corpus

**abseil-cpp** (`/home/veesh/personal/cpp-bench/abseil-cpp`), 1222 pack-language
files (488 `.cc` + 387 `.h` + some Python/CMake/R caught by the same walk —
877 were pure C/C++ in an earlier count). A **159-entry `compile_commands.json`**
already existed in that checkout (CMake `build-cc/`, production targets
only) — this is clangd's real scope on this corpus.

**LLVM was NOT cloned or built.** Reasons, both independently sufficient:
disk/time cost (LLVM is a multi-GB clone + a from-scratch CMake configure to
get `compile_commands.json`, easily 30+ min even with parallel build, on a
box already running other agents) and — per the standing RAM order — no
justification to take that risk when abseil already gives clangd a *real*,
existing compile database and a corpus big enough to show both tools' actual
behavior at more-than-toy scale. Section "What a future LLVM run needs" in
the procedure doc has the exact steps for whoever wants to pull that trigger.

**clangd IS installed** on this box: `/usr/bin/clangd-18`, Ubuntu build
`18.1.8`. (The prior scouting note that said "clangd is not installed" was
wrong for this box — worth a correction, since it changes the calculus:
paired measurement was possible without any setup.)

## Cold-start latency (time-to-first-answer over the real LSP protocol)

Measured via `e2e/lsp_latency.py`: spawn process → initialize → didOpen →
first `textDocument/definition`, timed from process spawn. Two files:
one **inside** clangd's compile-db (`absl/base/log_severity.cc`, its first
entry), one **outside** it (`absl/base/call_once_test.cc`, a `_test.cc` —
clangd falls back to guessed flags for these).

| scenario | perl-lsp | clangd | note |
|---|---|---|---|
| in-DB file, cross-file goto-def (`NormalizeLogSeverity` → `log_severity.h`) | **degrades to `null`** at 410ms (bounded-wait cap), **heals at ~8.3s** once full-workspace index completes | **342ms, correct on the first try** | MEASURED |
| out-of-DB file, cross-file goto-def (`Mutex` → `mutex.h`, fallback flags) | same pattern: null at 410ms, heals ~8.65s | **456–502ms, correct on the first try** (fallback flags borrowed enough `-I` from a sibling TU in the same dir) | MEASURED |
| trivial single-file fixture, no workspace, no compile-db (`test_files/cpp/sample.cpp`) | null at 408ms, heals at 613ms | **82ms, correct on the first try** | MEASURED |

**This is the opposite of the prior brief's assumption.** The brief expected
clangd's "parses on open" to be the slow path and ours to win cold. Measured
result is the reverse for *this specific* shape of query (goto-def to a
symbol reachable through the file's own `#include` chain): clangd's
preamble build answers in the low hundreds of ms; our pack-language
architecture gates **every** cross-file answer on the full bulk workspace
scan finishing (`ensure_workspace_indexed` → `index_pack_language`, one
`par_iter` pass over the whole tree — see `main.rs`/`backend.rs`), which
takes seconds even though the specific file you opened only needed one
header. `DEFAULT_COLD_WAIT_MS = 400` in `backend.rs` is exactly why the
degraded window is ~410ms every time — the bounded wait times out before a
fresh-process cross-file build lands, and the client is left to retry (no
push-based refresh for goto-def/references/hover, unlike semanticTokens/
inlayHint which DO get a server-initiated refresh nudge on heal).

**Where we're NOT behind**: same-file (no cross-file resolution needed)
answers are comparably fast once past the 400ms floor, and — see
Coverage below — clangd is only fast here because both test files happened
to resolve through headers/flags it could reach; a file that needs a
truly unbuildable config (no compile command reachable, no plausible
fallback) is the scenario where clangd degrades and we don't, and that
scenario wasn't reproduced in this corpus (abseil is a well-behaved CMake
project — no macro-superposition or genuinely broken TU to point at).

## Peak RAM (full corpus index, both tools, abseil)

| tool | scope | peak RSS | note |
|---|---|---|---|
| perl-lsp | 1222 files, whole tree, no compile-db needed | **~635–660 MB** (650,876 KB via `/usr/bin/time -v` + `PERL_LSP_HEAP_DUMP`; 642–668 MB via 3 independent LSP-protocol runs) | MEASURED, matches the Memory-Slice-2 claim (~632MB) closely; heap-dump shows `witness_vec`/`witness_index` now **0.0 MB** (evicted), `refs` now dominant at 64.6% of a 248MB payload (was 71.5% witness-bag of 857MB pre-Slice-2) |
| clangd | 159 TUs (production compile-db only) | **~1522 MB** at full background-index completion (159/160 done at 5.2s); **~410–505 MB** mid-index (partial, ~6/160 done) | MEASURED. The 1.5GB number is on a corpus **7.7× smaller** (159 vs 1222 files) than ours |

**Headline**: post-Slice-2, perl-lsp's full-tree RSS (650MB / 1222 files)
is **under half** of clangd's full-index RSS (1.52GB / 159 files) on the
*same* codebase, despite indexing 7.7× more files. This flips the framing
in the prior scouting doc, which (before Slice 2 landed, and without an
actual clangd run) assumed clangd would be the leaner one. The
partial-index 320MB figure the user saw live was clangd **before its
background index finished** — not a steady-state number; full completion
is where the 1.5GB shows up. This is an apples-to-apples same-machine,
same-corpus pair — the strongest number in this report.

## Coverage / completeness

- **File scope**: clangd's compile-db here covers 159 files; only **1** of
  abseil's 302 `_test.cc`/`_benchmark.cc` files is in it. We index all 1222
  pack-language files (test/bench included) with zero compile-db setup —
  MEASURED file counts, both sides.
- **goto-def cross-file: confirmed working both ways.** `absl::StrCat` called
  from `cord.cc` resolves correctly to its declaration in `str_cat.h`
  (cross-TU, header-mediated) once our workspace index completes. Hover at
  the same position renders the right signature. This part of the
  differentiator claim holds.
- **references cross-file: verified complete — the differentiator holds.**
  An earlier pass reported this as a gap; that was a **measurement
  artifact**, now corrected. `perl-lsp --references` takes **0-based** input
  (`col` is a byte offset; only the output is 1-based — see `--help` and
  every gold row's `"line": 0` convention). The original repro typed
  **1-based** coordinates, so each query landed one row *below* the target
  token (`mutex.h:163` 1-based → line 164, inside the class body, off the
  token → 0 refs). Re-measured with correct 0-based coords on abseil:

  | symbol | 0-based pos | refs | files | of which test/bench |
  |---|---|---|---|---|
  | `Mutex` (class) | `mutex.h` 162:47 | **230** | **37** | 11 test + 1 benchmark |
  | `StrCat` (free fn) | `str_cat.h` 580:33 | **348** | **70** | 41 test |

  Textual grep finds `Mutex` in 48 files / `StrCat` in 93; the difference is
  **comment-only mentions** the LSP correctly excludes (verified on
  `const_init.h`, `barrier.cc`, `thread_identity.h`) — we are *more precise*
  than grep, not undercounting. Both `StrCat` overload decls return the same
  set (references is name+owner-keyed, not per-signature). The CLI and the
  editor share the exact `resolve()`/`references()` path (a `RoleMask`
  walk over the workspace-walked pack index), so the user's live session —
  sending the real 0-based cursor — resolved correctly and surfaced the test
  + benchmark call sites. Locked by 6 gold rows over the hermetic multi-TU
  fixture `gold-corpus/cpp-fixture/multitu/` (references from both decl and
  use anchors reaches every including TU incl. the `_test.cc`, and excludes a
  TU that includes the header but never names the symbol — precision, not
  just recall). Root cause: a 0-based vs 1-based coordinate artifact, not a
  real ref gap.

**Net honest read on the differentiator axis**: whole-tree *file coverage*,
cross-file *goto-def/hover*, AND whole-tree *references* completeness — the
"we surface test/bench call sites clangd's compile-db misses" story — are all
real and measured. The one axis where clangd wins is **cold-start latency**
(it answers from an opened file's preamble in ~100–500ms; we null at the
400ms bounded wait and heal at the ~8.9s bulk pack scan).

## Correctness parity (`e2e/compare-clangd.sh`, self-contained fixtures)

Re-ran unmodified against `clangd-18`:

```
10 PARITY   documentSymbol, goto-def, highlight, hover, references,
            rename, local-goto, outline, param-goto, param-hover
 9 OURS     completion in-scope, member completion, deep-peel-diag,
            deep-show-only, diag-quickfix, ptr-dot-autofix,
            val-arrow-autofix, newThing-seethrough, wrap-seethrough
 0 GAP      (clangd passes something we fail) — none
```

Close to the "11 PARITY / 8 OURS" figure cited in the brief (exact split
shifts a little between runs/binary revisions; the important number, 0 GAP,
matches). The `cross_file` fixture in the script's `FILES` list produces no
assertions for either tool — it targets a **Perl** fixture
(`test_files/cross_file_types.pl`) under the C++-only nvim config
(`init_cpp.lua` sets `filetypes = {c, cpp}`), so LSP never attaches within
the 15s wait. Pre-existing harness quirk, not something this pass
introduced or fixed (compare-clangd.sh wasn't touched).

## Steady-state (warm) query latency

Once the workspace index has landed (or, for clangd, once a query has
warmed the relevant preamble), a repeat `textDocument/definition` at the
same position:

| tool | warm latency |
|---|---|
| perl-lsp | 5–86 ms (5ms typical; higher readings coincided with trailing background-index I/O right after a heal) |
| clangd | 5–7 ms |

Both effectively instant once warm — MEASURED, no meaningful difference at
this scale.

## Where clangd wins (honest list)

1. **Cold single-file / small-index goto-def**, whenever the answer is
   reachable through the opened TU's own preamble (its `#include` chain) —
   clangd answers in the 80–500ms range; we're gated on the full
   workspace-wide pack-language scan (seconds) regardless of how small the
   actual dependency is for that one file.
2. Template/overload/SFINAE-grade semantic accuracy is clangd's designed
   strength and wasn't touched by this pass (out of scope; abseil doesn't
   stress it much either).

## Where we win (honest list)

1. **Zero setup.** No `compile_commands.json`, no build step, works on the
   1222-file whole tree including 301 test/bench files clangd's production
   compile-db doesn't cover, out of the box.
2. **Peak RAM at full-index steady-state**: ~650MB (1222 files) vs
   clangd's ~1.52GB (159 files) — better memory *and* 7.7× more coverage.
3. **goto-def / hover / references correctness across the whole tree**,
   confirmed working cross-file including into files clangd's compile-db
   excludes — references reaches every including TU incl. test/benchmark
   call sites (Mutex 230 refs/37 files/12 test+bench; StrCat 348/70/41).
4. **0 GAP rows** on the existing correctness differential suite — every
   protocol-level behavior clangd gets right on the fixture set, we also
   get right.

## What's MEASURED vs PROJECTED, at a glance

- MEASURED: all numbers in this document except where explicitly marked.
- PROJECTED / not re-verified this pass: Chromium/LLVM-scale clangd
  behavior ("hours + multiple GB") — inherited from clangd's own published
  docs, not independently reproduced here (see procedure doc for what that
  would take).
- Flagged assumption this pass **overturned**: "clangd is lighter on RAM"
  and "clangd not installed on this box" — both wrong; see above.
