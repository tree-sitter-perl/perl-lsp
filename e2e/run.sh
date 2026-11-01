#!/usr/bin/env bash
set -euo pipefail

# Run from the repo root: fixtures (test_files/), the lua harness (lua/test/
# via rtp), and the release binary are all root-relative, and the SQLite cache
# is keyed on the canonical root. Lets `./e2e/run.sh` work from any CWD.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# roles.lua needs Moo on @INC too (its fixtures inherit Moo::Object); CI
# installs it and passes PERL5LIB explicitly.
export PERL5LIB="${PERL5LIB:-$PWD/test_files/lib}"

# Fail on an old nvim with the reason, not a traceback. The harness calls
# `vim.lsp.get_clients` (0.10+); on 0.9.x that surfaces as
# "attempt to call field 'get_clients' (a nil value)" inside lua/test/lsp.lua,
# which reads as a broken harness rather than a stale editor — an hour lost to
# it here, and Ubuntu 24.04 still ships 0.9.5.
if ! command -v nvim >/dev/null 2>&1; then
  echo "e2e: nvim not found. The harness needs nvim 0.10+ (CI pins v0.11.0)." >&2
  exit 1
fi
nvim_ver=$(nvim --version | head -1 | sed -E 's/^NVIM v?//')
nvim_major=${nvim_ver%%.*}
nvim_minor=${nvim_ver#*.}; nvim_minor=${nvim_minor%%.*}
if (( nvim_major == 0 && nvim_minor < 10 )); then
  cat >&2 <<EOF
e2e: nvim $nvim_ver is too old — the harness needs 0.10+ (it calls
     vim.lsp.get_clients). CI pins v0.11.0. Distro packages lag: Ubuntu
     24.04 ships 0.9.5. Use the release tarball:

  curl -sSL -o nvim.tar.gz \\
    https://github.com/neovim/neovim/releases/download/v0.11.0/nvim-linux-x86_64.tar.gz
  tar xzf nvim.tar.gz && export PATH="\$PWD/nvim-linux-x86_64/bin:\$PATH"
EOF
  exit 1
fi

bin="${PERL_LSP_BIN:-./target/release/perl-lsp}"

# Reap our own servers. nvim spawns perl-lsp and cleanly shuts it down when nvim
# exits normally — but a hung suite that CI `timeout`-kills orphans the server
# (it reparents to init but keeps our process group). The parent-liveness
# monitor self-exits it within ~10s regardless; this trap is the immediate
# belt-and-suspenders. Only touches perl-lsp started DURING this run and sharing
# our process group, so an unrelated editor's server (different pgid) is safe.
our_pgid=$(ps -o pgid= -p $$ | tr -d ' ')
declare -A _preexisting_lsp=()
for _p in $(pgrep -x perl-lsp 2>/dev/null || true); do _preexisting_lsp[$_p]=1; done
reap_servers() {
  local pid pgid
  for pid in $(pgrep -x perl-lsp 2>/dev/null || true); do
    [[ -n "${_preexisting_lsp[$pid]:-}" ]] && continue
    pgid=$(ps -o pgid= -p "$pid" 2>/dev/null | tr -d ' ')
    [[ "$pgid" == "$our_pgid" ]] && kill -TERM "$pid" 2>/dev/null || true
  done
}
trap reap_servers EXIT

# Warm the fixture cache synchronously before the suite loop. The cross-file
# suites poll a fixed 10s for the workspace index; with a cold cache the async
# resolver loses that race in isolation, and the full run only passes because
# earlier suites incidentally warm it. `--check` runs `cli_full_startup` (the
# same workspace-index + SQLite warm an LSP launch does), populating the cache
# so every suite below starts warm and the poll is deterministic. The cache is
# keyed on the canonicalized workspace root, so warm "$PWD" — the same root the
# nvim test harness resolves via root_markers (`.git`), not `test_files`. Clear
# first so the warm reflects the current build, not a stale blob.
"$bin" --clear-cache "$PWD" >/dev/null 2>&1 || true
"$bin" --check "$PWD" --severity warning >/dev/null 2>&1 || true

suites=(
  core.lua
  types.lua
  cross_file.lua
  inheritance.lua
  frameworks.lua
  array_hop.lua
  mojo_plugins.lua
  mojo_events.lua
  dbic_parametric.lua
  roles.lua
  saved_dep_edit.lua
)

total_passed=0
total_failed=0
failed_suites=()

for test in "${suites[@]}"; do
  echo "── $test ──"
  # The SQLite cache is warm (above), but each suite spawns a FRESH nvim+LSP
  # that re-resolves the workspace asynchronously; cross-file suites poll a
  # fixed window for the index, and under CI load that readiness race can lose
  # intermittently. Retry a failed suite ONCE — a flake passes on the second
  # attempt, a real regression fails both. (Capture output so we can sum the
  # per-suite tallies; echo it back so per-test ✓/✗ stays visible.)
  p=0; f=0; rc=0
  for attempt in 1 2; do
    if output=$(nvim --headless --clean -u e2e/init.lua -l "e2e/$test" 2>&1); then rc=0; else rc=$?; fi
    # Per-suite summary lines look like `N passed, M failed` (with ANSI codes).
    summary=$(echo "$output" | sed 's/\x1b\[[0-9;]*m//g' | grep -E '^[0-9]+ passed, [0-9]+ failed' | tail -1 || true)
    p=0; f=0
    if [[ -n "$summary" ]]; then
      p=$(echo "$summary" | sed -E 's/^([0-9]+) passed.*/\1/')
      f=$(echo "$summary" | sed -E 's/.* ([0-9]+) failed/\1/')
    fi
    if [[ $rc -eq 0 && $f -eq 0 ]]; then break; fi
    if [[ $attempt -eq 1 ]]; then
      echo "  ⟳ $test failed (rc=$rc, ${f} failed) — retrying once (e2e index-readiness is flaky under load)…"
    fi
  done
  echo "$output"
  total_passed=$((total_passed + p))
  total_failed=$((total_failed + f))
  if [[ $rc -ne 0 || $f -ne 0 ]]; then
    failed_suites+=("$test")
  fi
  echo
done

# Raw-LSP suite: the not-ready-vs-no-result net (message ORDERING relative to
# the in-flight build is the assertion, which nvim's own client would mask by
# gating on readiness — so this one speaks stdio directly, via python).
echo "── not_ready.py (raw LSP) ──"
if command -v python3 >/dev/null 2>&1; then
  if output=$(python3 e2e/not_ready.py "$bin" 2>&1); then rc=0; else rc=$?; fi
  echo "$output"
  summary=$(echo "$output" | grep -E '^[0-9]+ passed, [0-9]+ failed' | tail -1 || true)
  p=0; f=0
  if [[ -n "$summary" ]]; then
    p=$(echo "$summary" | sed -E 's/^([0-9]+) passed.*/\1/')
    f=$(echo "$summary" | sed -E 's/.* ([0-9]+) failed/\1/')
  fi
  total_passed=$((total_passed + p))
  total_failed=$((total_failed + f))
  [[ $rc -ne 0 || $f -ne 0 ]] && failed_suites+=("not_ready.py")
else
  # Loud skip, not silence — a silently missing suite reads as coverage.
  echo "  ⚠ SKIPPED: python3 not found (CI has it; install locally to run)"
fi
echo

echo "════════════════════════════════════════════"
if [[ ${#failed_suites[@]} -eq 0 ]] && [[ $total_failed -eq 0 ]]; then
  printf '\033[32mTOTAL: %d passed, 0 failed\033[0m across %d suites\n' \
    "$total_passed" "${#suites[@]}"
  exit 0
else
  printf '\033[31mTOTAL: %d passed, %d failed\033[0m across %d suites\n' \
    "$total_passed" "$total_failed" "${#suites[@]}"
  if [[ ${#failed_suites[@]} -gt 0 ]]; then
    printf '\033[31mFailing suites:\033[0m %s\n' "${failed_suites[*]}"
  fi
  exit 1
fi
