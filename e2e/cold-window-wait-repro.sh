#!/usr/bin/env bash
# Cold-open BOUNDED-WAIT repro (hitlist-4 Family B — the ledgered pull-verb
# residual). Demonstrates the fix that closes the last transient cold-open
# window: a single gd/hover/references fired IN the window that the user never
# re-triggers.
#
# Self-contained: generates a synthetic C workspace (N files each calling a
# shared function) whose pack workspace index takes ~2s to attach — wide enough
# that a query at t=500ms races it, narrow enough that a demo-scale bounded wait
# covers it. Runs the SAME binary twice against a COLD cache:
#
#   OFF (coldWaitMs=0)      — the handler does NOT wait → the ONE in-window
#                             references answer is DEGRADED (local call sites
#                             only, cross-file uses absent).
#   ON  (coldWaitMs=large)  — the handler blocks briefly for the imminent index
#                             → the SAME single query resolves WARM (the full
#                             cross-file set), WITHOUT the probe re-issuing it.
#
# The ON run's second (warm) query proves the common path pays ~zero added wait
# (index already done → the bounded wait returns before awaiting).
#
# The PRODUCTION default cap is 400ms — bounded so it can never wedge; it covers
# the window on normal-sized projects and degrades safely on huge trees (perl5's
# pack index alone takes ~22s, far past any sane wait). This repro uses a
# generous cap (ON_WAITMS) to exercise the MECHANISM end to end.
#
# Usage:  e2e/cold-window-wait-repro.sh
#   FILES=N  fixture size (default 4000)   ON_WAITMS=ms  (default 8000)
#   LOAD=1   add parallel CPU load (default 0 — the index alone is slow enough)
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BIN="${PERL_LSP_BIN:-$PWD/target/release/perl-lsp}"
if [[ ! -x "$BIN" ]]; then
  echo "missing binary $BIN — run: cargo build --release --features cpp" >&2
  exit 2
fi
export PERL_LSP_BIN="$BIN"

FILES="${FILES:-4000}"
ON_WAITMS="${ON_WAITMS:-8000}"

# ---- generate the fixture ------------------------------------------------
WS="$(mktemp -d)"
mkdir -p "$WS/inc"
cat > "$WS/inc/common.h" <<'EOF'
#ifndef COMMON_H
#define COMMON_H
int widget_compute(int x);
#endif
EOF
for i in $(seq 0 $((FILES - 1))); do
  n="$(printf %04d "$i")"
  {
    printf '#include "common.h"\n'
    for j in 0 1 2 3; do
      printf 'int use_%s_%d(void) { return widget_compute(%d); }\n' "$n" "$j" "$i"
    done
  } > "$WS/f${n}.c"
done
( cd "$WS" && git init -q )
CALL_FILE="$WS/f0000.c"        # widget_compute call at row 1 (0-idx), col 30
echo "fixture: $FILES files in $WS  (expected warm refs = $((FILES * 4 + 1)))"

cleanup_all() {
  for p in "${LOADPIDS[@]:-}"; do kill "$p" 2>/dev/null; done
  rm -rf "$WS"
}
trap cleanup_all EXIT
LOADPIDS=()
start_load() {
  if [[ "${LOAD:-0}" != "0" ]]; then
    for _ in $(seq 1 "$(nproc 2>/dev/null || echo 4)"); do yes >/dev/null & LOADPIDS+=($!); done
  fi
}
stop_load() { for p in "${LOADPIDS[@]:-}"; do kill "$p" 2>/dev/null; done; LOADPIDS=(); }

run() {  # $1 = coldWaitMs
  export PERL_LSP_COLD_WAIT_MILLISECONDS="$1"
  export XDG_CACHE_HOME="$(mktemp -d)"
  "$BIN" --clear-cache "$WS" >/dev/null 2>&1 || true
  start_load
  HEAL_FILE="$CALL_FILE" HEAL_ROW=1 HEAL_COL=30 WARM_WAITMS="${WARM_WAITMS:-12000}" \
    timeout 90 nvim --headless --clean \
    -u e2e/init_cpp.lua -l e2e/cold_window_wait.lua 2>&1 \
    | grep -E "^(in_window_refs|in_window_ms|warm_refs|warm_ms|RESULT)"
  stop_load
}

echo ""
echo "=== OFF: coldWaitMs=0 (bounded wait disabled) ==="
off_out="$(run 0)"; echo "$off_out"
off_win=$(echo "$off_out" | sed -n 's/^in_window_refs=//p')

echo ""
echo "=== ON: coldWaitMs=${ON_WAITMS} (bounded wait) ==="
on_out="$(run "$ON_WAITMS")"; echo "$on_out"
on_win=$(echo "$on_out"  | sed -n 's/^in_window_refs=//p')
on_warm=$(echo "$on_out" | sed -n 's/^warm_refs=//p')
on_win_ms=$(echo "$on_out"  | sed -n 's/^in_window_ms=//p')
on_warm_ms=$(echo "$on_out" | sed -n 's/^warm_ms=//p')

echo ""
echo "=== summary ==="
echo "OFF in-window refs = ${off_win:-?}   (degraded — handler did not wait)"
echo "ON  in-window refs = ${on_win:-?}   in ${on_win_ms:-?}ms   (healed — handler waited for the index)"
echo "ON  warm    refs   = ${on_warm:-?}   in ${on_warm_ms:-?}ms   (zero added wait — index already done)"

# Green iff the wait healed the single in-window query to the FULL warm set
# (ON in-window == ON warm) AND the OFF run really was degraded (strictly fewer).
[[ -n "${off_win:-}" && -n "${on_win:-}" && -n "${on_warm:-}" \
   && "$on_win" -eq "$on_warm" && "$off_win" -lt "$on_win" ]]
