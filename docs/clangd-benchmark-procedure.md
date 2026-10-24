# clangd comparison — repeatable procedure

Companion to `docs/clangd-comparison.md` (the results). This is the runbook:
exact commands so the next person (or the next `cargo` release) can
reproduce every number, or push the same procedure to a bigger corpus.

## Prerequisites

- Release binary with C++ support: `cargo build --release --features cpp`.
  A process serves one language: the CLI verbs below pick C++ from their
  target file, and the live server needs `--lang cpp`.
- `clangd` — check what's on the box first: `which clangd-N` for various N
  (Ubuntu ships versioned binaries, e.g. `clangd-18`, not a bare `clangd`
  symlink). `clangd --version` / `clangd-18 --version` to confirm.
  `apt list --installed | grep -i clangd` if unsure it's there at all.
- `nvim` (for `e2e/compare-clangd.sh`, the correctness differential).
- `python3` stdlib only (for `e2e/lsp_latency.py`, the latency/RSS probe —
  no pip installs, no `psutil`).
- A corpus with a real `compile_commands.json` if you want to test clangd
  properly. `abseil-cpp` under `/home/veesh/personal/cpp-bench/` already
  has one checked in (`build-cc/compile_commands.json`, symlinked at the
  repo root) — don't regenerate it unless you need a different config.

## THE RAM CEILING DISCIPLINE (read this before running anything)

The user's standing order: never let a measurement run blow RAM through the
roof, and never run two heavy indexing jobs in parallel.

- Before any run: `free -h` — confirm several GB free and no other heavy
  process already running (`ps aux --sort=-%mem | head`).
- One indexer at a time. Never `perl-lsp` and `clangd` doing a cold full
  index simultaneously — they compete for CPU and the RSS numbers stop
  being clean per-tool measurements anyway.
- Watch peak RSS as you go: `/usr/bin/time -v <cmd>` for CLI invocations
  (its "Maximum resident set size" line), or read
  `/proc/<pid>/status`'s `VmHWM` line for a long-running LSP-protocol
  session (this is what `e2e/lsp_latency.py` does automatically — no
  polling thread, the kernel already tracks the historical peak).
- If a corpus/config looks like it would push past ~4GB projected (e.g.
  LLVM/Chromium-scale — clangd's own docs say Chromium-scale is
  "multiple GB, multiple hours"), **don't run it**. Measure a bounded
  subset and extrapolate, and say so explicitly in whatever you write up.
- Always kill what you spawned when done:
  `pgrep -af "release/perl-lsp|clangd"` then `pkill -f <pattern>` (or let
  `lsp_latency.py` exit cleanly via its own `shutdown`+`exit` handshake,
  which it does automatically).
- Clear caches between "cold" runs so you're not measuring a warm SQLite
  cache by accident: `perl-lsp --clear-cache <root>` (ours) and
  `rm -rf <root>/.cache/clangd` (clangd's on-disk background index).

## 1. Cold full-workspace index + peak RAM (perl-lsp)

The documented repro from the Memory-Slice-2 ADR (`docs/adr/memory-slice-2-lru.md`),
still the cleanest single command for our own number — includes the
per-bucket heap-composition breakdown:

```bash
perl-lsp --clear-cache <abseil-root>
PERL_LSP_HEAP_DUMP=1 /usr/bin/time -v perl-lsp --references <abseil-root> \
  <abseil-root>/absl/strings/string_view.h 41 15
```

Read `Maximum resident set size` from the `time -v` output (KB; divide by
1024 for MB) and the `[heap-dump]` block for the payload breakdown. Note:
this CLI path (`cli_full_startup`) is a fair proxy for cold **indexing**
wall-time and peak RAM. Its `--references`/`--definition` *query results*
match what the real async LSP server answers once indexing completes — the
CLI and the live session share the exact `resolve()`/`references()` path
(`docs/clangd-comparison.md`) — but the live server can still answer
`null`/degraded before that point (the bounded-wait window in step 3), so
don't compare an early live-session response against the CLI's
already-fully-indexed one.

## 2. Cold full-workspace index + peak RAM (clangd)

```bash
cd <abseil-root>
rm -rf .cache/clangd   # force a true cold background-index
python3 e2e/lsp_latency.py --bin /usr/bin/clangd-18 \
  --root <abseil-root> --file absl/base/call_once_test.cc \
  --line 32 --col 17 --query definition \
  --retry-until-non-null --retry-timeout-secs 10 \
  --wait-indexing-secs 90
```

`--wait-indexing-secs 90` gives clangd's background indexer time to finish
across the whole compile-db before the script reads `VmHWM` and exits; the
output's `progress_notifications` array shows clangd's own `$/progress`
events (title `"indexing"`, `message` like `"N/160"`) — the timestamp of
the final `kind: "end"` event is the wall-clock to full-index completion.
For abseil's 159-entry compile-db this took ~5.2s; RSS at that point was
~1.5GB (vs ~400-500MB at the halfway point) — **peak RAM is a function of
how much of the background index has completed**, not a fixed number, so
always let it run to the `"end"` event (or a fixed, stated cutoff) before
quoting a peak.

## 3. Cold time-to-first-answer over the real LSP protocol (both tools)

This is the number that matters for "does the editor feel responsive on
open" — NOT the CLI `--batch` path, which runs a full synchronous index
before answering and so never shows the live server's early
null/degraded window; `--references` is not a separate concern here —
the CLI and the live session share the exact `resolve()`/`references()`
path, see `docs/clangd-comparison.md`'s Coverage section.

```bash
python3 e2e/lsp_latency.py --bin ./target/release/perl-lsp --arg=--lang --arg=cpp \
  --root <root> --file <relative/or/abs/path> \
  --line <1-based> --col <1-based> \
  --query definition --warm-repeat 2 \
  --retry-until-non-null --retry-timeout-secs 90
```

Swap `--bin /usr/bin/clangd-18` for the clangd side (same root/file/position
so it's a true paired comparison — clangd auto-discovers
`compile_commands.json` by walking up from the opened file's directory).

Read: `timings_ms.spawn_to_first_query_response` (raw first answer, may be
degraded/null for us — see `raw_first_response_summary`),
`timings_ms.spawn_to_healed_non_null_ms` (when a retry first got a real
answer — only populated if `--retry-until-non-null` was passed and the raw
first response was empty/null), `peak_rss_mb`, `warm_response_summary` /
`timings_ms.warm_query_response_ms` for the steady-state number.

Use `--query references` for the completeness axis (below) and
`--query hover` as a cheap sanity check that a position resolves to the
symbol you think it does (compare `raw_first_response_summary`'s content
against what you expect before trusting a references/definition count).

Use `--dump-full-response <path>` to get the complete result list (the
default summary only samples the first 3 entries) — needed for any
file-breakdown / completeness analysis:

```bash
python3 -c "
import json, collections
d = json.load(open('<path>'))
print('total', len(d))
files = collections.Counter(r['uri'].split('/')[-1] for r in d)
for f, c in files.most_common(30): print(c, f)
"
```

**Before trusting a references() count as a completeness signal**: confirm
the position resolves to the symbol you intend via `--query hover` first,
and confirm cross-file identity independently via `--query definition`
*from a known caller* (does goto-def find its way to the same declaration
site?) — this is exactly how the cross-TU references gap in
`docs/clangd-comparison.md` was isolated: hover confirmed the right symbol,
goto-def confirmed cross-file identity works, and only the reverse
(definition → all usages) direction came up short.

## 4. Correctness parity

```bash
CLANGD=/usr/bin/clangd-18 ./e2e/compare-clangd.sh
```

Already exists, already documented at the top of the script itself. Rerun
whenever either binary changes to keep the PARITY/OURS/GAP counts current.
Note the `cross_file` entry in the script's `FILES` list currently produces
no assertions (it targets a Perl fixture under the C++-only nvim config) —
a pre-existing harness quirk, not something introduced by this pass.

## 5. Picking a query position

Any position works, but for a fair completeness comparison pick a symbol
that:
- Is declared in a header and defined/called across multiple `.cc` files
  (exercises cross-TU resolution, not just same-file).
- Has at least one call site in a file OUTSIDE clangd's compile-db scope
  (a `_test.cc`/`_benchmark.cc` not in the 159-entry list) — this is
  exactly the "test/bench clangd misses" scenario, when it holds.
- To check whether a specific `.cc` file is in clangd's compile-db:
  ```bash
  python3 -c "
  import json
  d = json.load(open('<root>/compile_commands.json'))
  files = {e['file'] for e in d}
  print('<candidate file>' in files)
  "
  ```

Symbols used in this pass (abseil): `absl::Mutex` (class, `mutex.h:163:48`),
`absl::StrCat` (free function, `str_cat.cc:58:13` / `str_cat.h:574:34`),
`ABSL_GUARDED_BY` (function-like macro, `thread_annotations.h:58:9`),
`Mutex::Lock` (method, `mutex.h:195:15`). All four are a reminder to get
the coordinate convention right: `--references` takes 0-based input, and a
1-based position lands one row off the target token and reads as a
references gap that isn't one — see the comparison doc's Coverage section
for the corrected counts.

## What a future full-LLVM run would need

Not attempted this pass (RAM/time discipline — see the comparison doc's
"Corpus" section for why). If someone wants to pull the trigger:

1. **Disk**: LLVM monorepo is several GB shallow-cloned, more with history.
   Check `df -h` first — this box had 193GB free at last check, comfortably
   enough, but confirm again since other agents may have used space since.
2. **Clone**: `git clone --depth 1 https://github.com/llvm/llvm-project.git`
   (shallow — full history is much larger and unneeded).
3. **Configure for `compile_commands.json`**:
   ```bash
   cmake -S llvm -B build -G Ninja \
     -DCMAKE_EXPORT_COMPILE_COMMANDS=ON \
     -DLLVM_ENABLE_PROJECTS="clang" \
     -DCMAKE_BUILD_TYPE=Release
   ```
   Time this step — it's part of the "time to first value" story for
   clangd (we need zero equivalent step). Don't run the actual `ninja`
   build unless you specifically need compiled artifacts; clangd only
   needs `compile_commands.json` to exist with correct flags, and preambles
   build from source + flags without a prior full build in most cases
   (though missing generated headers can degrade specific TUs — note any
   such degradation honestly rather than silently skipping those files).
4. **RAM ceiling for this scale is genuinely unknown territory** — clangd's
   own docs say Chromium-scale (much bigger than LLVM) is "multiple GB,
   multiple hours." Budget for LLVM to land somewhere between abseil's
   1.5GB and Chromium's multi-GB; watch `/proc/<pid>/status VmHWM` on
   both tools continuously (poll every few seconds to a log file — for a
   run this long, don't rely on a single end-of-run read) and be ready to
   kill early if it's climbing toward the ceiling before finishing.
   Consider `ulimit -v <bytes>` as a hard backstop for the very first
   attempt, since an OOM on this box would affect other agents sharing it.
5. **Same measurement steps** as sections 1–4 above, just pointed at the
   LLVM checkout instead of abseil. `e2e/lsp_latency.py` and
   `e2e/compare-clangd.sh` are both corpus-agnostic already.
6. Expect wall-clock in the minutes-to tens-of-minutes range for clangd's
   background index at this scale (vs abseil's ~5s) — budget an
   interactive session accordingly, not a quick check-in-between-other-things
   run.
