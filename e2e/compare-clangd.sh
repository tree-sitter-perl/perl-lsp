#!/usr/bin/env bash
# Differential test harness: run the C++ e2e suite against BOTH perl-lsp and
# clangd (the e2e is LSP-agnostic — the LSP binary is chosen by PERL_LSP_BIN,
# and the assertions are protocol-level: goto-def line, references set, rename
# edits, symbols). Categorizes every assertion:
#
#   PARITY   both pass         → standard behavior; also a regression guard
#                               (if WE later fail a PARITY row, we broke something
#                               clangd gets right)
#   OURS     we pass, clangd ✗ → a perl-lsp value-add (macro see-through, our DX
#                               diagnostics) OR a strict-format assertion
#   GAP      clangd passes, we ✗→ a real gap to close (clangd does it, we don't)
#   BOTH✗    neither passes    → fixture needs setup (e.g. clangd wants a
#                               compile_commands.json) or a shared limitation
#
# NOTE clangd is fallback-parsing our small self-contained fixtures (no compile
# DB); multi-file fixtures may not attach for it. That asymmetry is itself a
# data point (build-independence is our lane), not a clangd defect — read GAP
# rows with that in mind.
#
# Usage: ./e2e/compare-clangd.sh            (auto-detects clangd)
#        CLANGD=/path/to/clangd ./e2e/compare-clangd.sh
set -u
cd "$(dirname "$0")/.." || exit 1

OURS="$PWD/target/release/perl-lsp"
CLANGD="${CLANGD:-}"
if [ -z "$CLANGD" ]; then
  CLANGD="$(command -v clangd 2>/dev/null || true)"
  [ -z "$CLANGD" ] && CLANGD="$(ls -d "$HOME"/.local/share/nvim/mason/packages/clangd/*/bin/clangd 2>/dev/null | head -1)"
fi
[ -x "$OURS" ] || { echo "build the release binary first: cargo build --release --features cpp"; exit 1; }
[ -n "$CLANGD" ] && [ -x "$CLANGD" ] || { echo "clangd not found — set CLANGD=/path/to/clangd"; exit 1; }
command -v nvim >/dev/null || { echo "nvim required"; exit 1; }

# cpp e2e files whose assertions are protocol-level (skip pure repro scripts).
FILES="cpp cpp_locals cpp_members cpp_member_op cpp_macro_calls cross_file"

# Run one lua file under one LSP binary; emit "PASS <name>" / "FAIL <name>".
run() { # $1=bin $2=luafile
  PERL_LSP_BIN="$1" timeout 120 nvim --headless --clean \
    -u e2e/init_cpp.lua -l "e2e/$2.lua" 2>&1 \
  | sed -nE 's/.*\xE2\x9C\x93 *(.+)$/PASS \1/p; s/.*\xE2\x9C\x97 *(.+)$/FAIL \1/p'
}

declare -A parity ours gap bothx
tot_parity=0 tot_ours=0 tot_gap=0 tot_bothx=0
printf "\n%-42s %-8s %-8s %s\n" "assertion" "perl-lsp" "clangd" "verdict"
printf '%.0s-' {1..74}; echo
for f in $FILES; do
  declare -A o_res=() c_res=()
  while read -r st name; do [ -n "${name:-}" ] && o_res["$name"]="$st"; done < <(run "$OURS" "$f")
  while read -r st name; do [ -n "${name:-}" ] && c_res["$name"]="$st"; done < <(run "$CLANGD" "$f")
  # union of assertion names
  for name in "${!o_res[@]}" "${!c_res[@]}"; do echo "$name"; done | sort -u | while read -r name; do
    o="${o_res[$name]:-—}"; c="${c_res[$name]:-—}"
    if   [ "$o" = PASS ] && [ "$c" = PASS ]; then v="PARITY"
    elif [ "$o" = PASS ] && [ "$c" != PASS ]; then v="OURS"
    elif [ "$o" != PASS ] && [ "$c" = PASS ]; then v="GAP*"
    else v="BOTH✗"; fi
    printf "%-42s %-8s %-8s %s\n" "${name:0:42}" "$o" "$c" "$v"
  done
  unset o_res c_res
done
echo
echo "verdicts: PARITY=both pass  OURS=our value-add/format  GAP*=clangd passes we don't (INVESTIGATE)  BOTH✗=fixture/shared"
echo "GAP* rows are the ones to look at — a real capability clangd has and we lack."
