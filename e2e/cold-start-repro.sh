#!/usr/bin/env bash
# Cold-start determinism repro (the M6/L3 "LSP session determinism" flake).
#
# Loops a COLD-cache C++ e2e session N times and reports the failure rate.
# The flake it locks down was a DashMap shard-reentrancy DEADLOCK: a request
# handler held a `get_open` read guard across `resolve()`, which re-locks the
# open shards via `for_each_open`; a concurrent diagnostics-refresh
# `for_each_open_mut` writer queuing on that shard (parking_lot writer
# preference) wedged the handler's reentrant read behind the writer, behind the
# handler's own first read. The Perl cpanfile resolver fires a burst of ~45
# refresh callbacks right after `didOpen`, so a mixed repo hit the window
# intermittently; each wedged handler consumed a worker thread until the runtime
# starved and every request timed out (the "5 compute assertions miss, self-heal
# on rerun" symptom).
#
# A flaky deadlock cannot be a gold row (the assertion is stable, the timing is
# not) — this loop is the lock. Pre-fix it fails a fraction of runs (worse under
# load); post-fix it is 0/N.
#
# Usage:  N=20 e2e/cold-start-repro.sh          # N defaults to 20
#         Assumes a `--features cpp` release build already exists.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

N="${N:-20}"
BIN="${PERL_LSP_BIN:-$PWD/target/release/perl-lsp}"
if [[ ! -x "$BIN" ]]; then
  echo "missing binary $BIN — run: cargo build --release --features cpp" >&2
  exit 2
fi

# Isolate the cache so the loop is reproducibly COLD and never contends with a
# concurrently-running server's shared ~/.cache tree.
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$(mktemp -d)}"

fails=0
for i in $(seq 1 "$N"); do
  "$BIN" --clear-cache "$PWD" >/dev/null 2>&1
  out=$(PERL_LSP_BIN="$BIN" nvim --headless --clean -u e2e/init_cpp.lua -l e2e/cpp.lua 2>&1)
  if echo "$out" | grep -qE "✗|[1-9][0-9]* failed"; then
    fails=$((fails + 1))
    echo "run $i: FAIL"
    echo "$out" | grep -E "✗" | sed 's/^/    /'
  fi
done

echo "cold-start-repro: $fails/$N runs with failures"
[[ "$fails" -eq 0 ]]
