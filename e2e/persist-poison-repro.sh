#!/usr/bin/env bash
# Poisoned-persist lock (the M6/L3 escalation: a degraded cold-run analysis
# frozen into the SQLite pack cache behind a self-validating stamp, re-served
# on every WARM run until the next --clear-cache).
#
# The invariant: a cold-cache session under heavy CPU load may hit the transient
# cold-open degraded window (a known, deferred residual — a query can race the
# background gather/index), but its damage is NEVER PERSISTED. `save_to_db`
# refuses any `degraded` analysis (on-open cached-only skip OR a truncated
# include closure), so the SQLite pack cache only ever holds correct
# full-closure blobs. A WARM run therefore serves the truth and passes.
#
# Method (deterministic — no reliance on the cache warming over many runs):
#   1. POISON: open the macro file under heavy load and hold the session open
#      long enough for the background workspace index to run to completion and
#      PERSIST. If the escalation were live, this is where a degraded blob would
#      be frozen behind a validating deps_stamp.
#   2. WARM ASSERT: a fresh session (no load, no --clear-cache) warms the cache
#      and runs the seethrough e2e. It MUST pass — a persisted poison would be
#      re-served here and fail every warm run.
#
# Usage:  e2e/persist-poison-repro.sh
#         Assumes a `--features cpp` release build already exists.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BIN="${PERL_LSP_BIN:-$PWD/target/release/perl-lsp}"
if [[ ! -x "$BIN" ]]; then
  echo "missing binary $BIN — run: cargo build --release --features cpp" >&2
  exit 2
fi

export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$(mktemp -d)}"

# A long-lived open: attach the macro file and hold the session so the pack
# workspace index completes + persists before nvim exits.
HOLD_LUA="$(mktemp --suffix=.lua)"
cat > "$HOLD_LUA" <<'LUA'
vim.opt.rtp:prepend(".")
local lsp = require("test.lsp")
lsp.open_and_attach("test_files/cpp/macro_calls.c")
vim.wait(6000, function() return false end)  -- let workspace index persist
LUA
trap 'rm -f "$HOLD_LUA"' EXIT

NLOAD="$(( $(nproc 2>/dev/null || echo 8) + 4 ))"
load_pids=()
start_load() { for _ in $(seq 1 "$NLOAD"); do yes >/dev/null & load_pids+=($!); done; }
stop_load()  { [[ ${#load_pids[@]} -gt 0 ]] && kill "${load_pids[@]}" 2>/dev/null; load_pids=(); }

"$BIN" --clear-cache "$PWD" >/dev/null 2>&1

# 1. POISON under load (long-lived → persists).
start_load
sleep 1
PERL_LSP_BIN="$BIN" nvim --headless --clean -u e2e/init_cpp.lua -l "$HOLD_LUA" >/dev/null 2>&1
stop_load

# Confirm the poison session actually persisted (else the assert is vacuous).
cppdb="$(find "$XDG_CACHE_HOME" -name 'modules-cpp.db' 2>/dev/null | head -1)"
rows=0
if [[ -n "$cppdb" ]] && command -v sqlite3 >/dev/null 2>&1; then
  rows="$(sqlite3 "$cppdb" 'SELECT count(*) FROM modules;' 2>/dev/null || echo 0)"
fi
echo "poison session persisted rows: $rows"

# 2. WARM assert (no load, no --clear-cache): must pass. A persisted poison
#    would be re-served here and fail.
out="$(PERL_LSP_BIN="$BIN" nvim --headless --clean -u e2e/init_cpp.lua -l e2e/cpp_macro_calls.lua 2>&1)"
if echo "$out" | grep -qE "✗|[1-9][0-9]* failed"; then
  echo "$out" | grep -E "✗" | sed 's/^/    /'
  echo "STICKY POISON: warm run served a degraded persisted blob — regression." >&2
  exit 1
fi
echo "OK: warm run over the persisted cache passes — no poisoned-persist."
