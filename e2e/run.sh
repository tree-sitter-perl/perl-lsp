#!/usr/bin/env bash
set -euo pipefail

# Run from the repo root: fixtures (test_files/), the lua harness (lua/test/
# via rtp), and the release binary are all root-relative, and the SQLite cache
# is keyed on the canonical root. Lets `./e2e/run.sh` work from any CWD.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

export PERL5LIB="${PERL5LIB:-$PWD/test_files/lib}"

bin="${PERL_LSP_BIN:-./target/release/perl-lsp}"

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
