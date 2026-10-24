#!/usr/bin/env bash
# Pack e2e: builds a --features cpp,python release and drives it as a C++
# server and as a Python server (`--lang`, set by each suite's nvim config) in
# headless nvim, reusing the same lua harness as the Perl suites. Separate
# from run.sh because it needs the pack-feature binary.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Reap our own servers on exit — see e2e/run.sh for the rationale. A hung/killed
# nvim orphans its perl-lsp child (reparented to init, same process group). The
# parent-liveness monitor self-exits it within ~10s; this trap is immediate.
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

echo "building --features cpp,python release..."
cargo build --release --features cpp,python >/dev/null 2>&1

bin="$PWD/target/release/perl-lsp"
# Every suite names its binary explicitly. `dev_lsp.lua` falls back to this same
# path when the variable is unset, so an omission is invisible until someone
# changes the fallback — then it is a silent test of the wrong binary.
export PERL_LSP_BIN="$bin"

# Suite -> its nvim config. The pack suites and the python one share the harness
# but not the LSP config, so the pairing lives here rather than in a parallel list.
suites=(
  "cpp.lua              init_cpp.lua"
  "cpp_members.lua      init_cpp.lua"
  "cpp_locals.lua       init_cpp.lua"
  "cpp_macro_calls.lua  init_cpp.lua"
  "cpp_labels.lua       init_cpp.lua"
  "cpp_member_op.lua    init_cpp.lua"
  "cpp_header_edit.lua  init_cpp.lua"
  "python_members.lua   init_python.lua"
)

# Aggregate rather than abort. `set -e` on the first failing suite reports ONE
# failure and silently skips the rest, so a run that breaks three suites looks
# identical to one that breaks the first — and CI names the wrong culprit.
total_passed=0
total_failed=0
failed_suites=()
for entry in "${suites[@]}"; do
  read -r test cfg <<<"$entry"
  echo "── $test ──"
  if output=$(nvim --headless --clean -u "e2e/$cfg" -l "e2e/$test" 2>&1); then rc=0; else rc=$?; fi
  echo "$output"
  p=$(printf '%s' "$output" | grep -oE '[0-9]+ passed' | tail -1 | grep -oE '[0-9]+' || true)
  f=$(printf '%s' "$output" | grep -oE '[0-9]+ failed' | tail -1 | grep -oE '[0-9]+' || true)
  total_passed=$(( total_passed + ${p:-0} ))
  total_failed=$(( total_failed + ${f:-0} ))
  # A suite that dies before printing a tally (crash, missing config, nvim
  # startup failure) reports zero of both — count the run itself as failed so
  # it cannot pass by producing no output.
  if [ "$rc" -ne 0 ] || [ -z "$p" ]; then
    failed_suites+=("$test")
    if [ -z "$p" ] && [ "${f:-0}" -eq 0 ]; then
      total_failed=$(( total_failed + 1 ))
    fi
  fi
done

echo
echo "════════════════════════════════════════════"
echo "TOTAL: $total_passed passed, $total_failed failed across ${#suites[@]} suites"
if [ ${#failed_suites[@]} -gt 0 ]; then
  echo "failed: ${failed_suites[*]}"
  exit 1
fi
