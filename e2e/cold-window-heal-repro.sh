#!/usr/bin/env bash
# Cold-open degraded-window HEAL + coalesce repro (hitlist-4 Family B).
#
# Two phases, both reading the PERL_LSP_DEBUG log (dev_lsp.lua routes RUST_LOG
# there when PERL_LSP_DEBUG=1):
#
#   Phase 1 — HEAL WINDOW.  Open a C file in a BIG tree (perl5) whose pack
#     workspace index takes many seconds to attach; query references inside the
#     first-open window (degraded → def only), then poll until it heals to the
#     full cross-file set. Reports the window width and confirms the server-side
#     completion-signal heal fired (`cold-window heal: index landed`).
#
#   Phase 2 — COALESCE.  Open a Perl file whose `use`s trigger a burst of module
#     resolutions; count `diag-refresh fired` (one per resolved module = the
#     pre-coalesce work) vs `diag-refresh executing` (the debounced runs). A
#     tight burst must collapse to ~one execution.
#
# Usage:  e2e/cold-window-heal-repro.sh
#   LOAD=0 disables the phase-1 parallel CPU load.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BIN="${PERL_LSP_BIN:-$PWD/target/release/perl-lsp}"
if [[ ! -x "$BIN" ]]; then
  echo "missing binary $BIN — run: cargo build --release --features cpp" >&2
  exit 2
fi
export PERL_LSP_BIN="$BIN"
export PERL_LSP_DEBUG=1
LOG=/tmp/perl-lsp.log
HEAL_FILE="${HEAL_FILE:-/home/veesh/personal/perl5/op.c}"
HEAL_ROOT="${HEAL_ROOT:-/home/veesh/personal/perl5}"

# ---------------------------------------------------------------- phase 1
echo "=== phase 1: heal window (${HEAL_FILE}) ==="
export XDG_CACHE_HOME="$(mktemp -d)"
"$BIN" --clear-cache "$HEAL_ROOT" >/dev/null 2>&1 || true
: > "$LOG"

LOADPIDS=()
if [[ "${LOAD:-1}" != "0" ]]; then
  for _ in $(seq 1 "$(nproc 2>/dev/null || echo 4)"); do yes >/dev/null & LOADPIDS+=($!); done
fi
cleanup() { for p in "${LOADPIDS[@]:-}"; do kill "$p" 2>/dev/null; done; }
trap cleanup EXIT

HEAL_FILE="$HEAL_FILE" timeout 120 nvim --headless --clean \
  -u e2e/init_cpp.lua -l e2e/cold_window_heal.lua 2>&1 \
  | grep -E "^(in_window_refs|settled_refs|healed_at_ms|window_ms|RESULT)"
cleanup; LOADPIDS=()

heal_fired=$(grep -c "cold-window heal: index landed" "$LOG" 2>/dev/null || echo 0)
echo "server_heal_events=$heal_fired   # completion-signal heals (ensure_workspace_indexed done)"

# ---------------------------------------------------------------- phase 2
echo ""
echo "=== phase 2: coalesce (Perl resolver storm) ==="
WS="$(mktemp -d)"
( cd "$WS" && git init -q )
cat > "$WS/storm.pl" <<'EOF'
use strict; use warnings;
use File::Spec; use File::Basename; use List::Util qw(first max sum);
use Scalar::Util qw(blessed reftype); use Data::Dumper; use Carp qw(croak);
use POSIX qw(floor ceil); use Cwd qw(getcwd abs_path); use Storable qw(dclone);
use Time::HiRes qw(time); use Getopt::Long; use Encode qw(encode decode);
use MIME::Base64;
my $x = first { $_ > 2 } (1,2,3); print Dumper($x);
EOF
export XDG_CACHE_HOME="$(mktemp -d)"
"$BIN" --clear-cache "$WS" >/dev/null 2>&1 || true
: > "$LOG"
STORM_FILE="$WS/storm.pl" STORM_WAITMS="${STORM_WAITMS:-14000}" timeout 40 nvim --headless --clean \
  -u e2e/init.lua -l e2e/coalesce_probe.lua >/dev/null 2>&1

fired=$(grep -c "diag-refresh fired" "$LOG" 2>/dev/null || echo 0)
exec_n=$(grep -c "diag-refresh executing" "$LOG" 2>/dev/null || echo 0)
resolved=$(grep -c "Resolving module" "$LOG" 2>/dev/null || echo 0)
echo "resolved_modules=$resolved"
echo "refresh_fired=$fired      # pre-coalesce work (one publish per resolved module)"
echo "refresh_executed=$exec_n  # post-coalesce work (debounced)"
rm -rf "$WS"

# Green iff phase-2 coalesced a real burst (fired>1) to a small number.
[[ "$fired" -gt 1 && "$exec_n" -le 3 ]]
