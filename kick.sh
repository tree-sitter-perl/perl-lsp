#!/usr/bin/env bash
# Kick the tires on a real corpus without typing paths.
#
#   ./kick.sh                 pick a corpus from a menu
#   ./kick.sh bmo             open a representative file from BMO
#   ./kick.sh bmo Bug         ...the biggest BMO file matching /Bug/
#   ./kick.sh bmo --list      show the ten biggest files, pick by number
#   ./kick.sh --nodeps bmo    leave PERL5LIB unset (deps unresolvable)
#   ./kick.sh --dry bmo       print what it would open, launch nothing
#   ./kick.sh --debug bmo     debug log + phase timing + counters, paths printed
#
# Dependencies live OUTSIDE each workspace (corpus/README.md) so they land on
# the @INC tier instead of joining the indexed workspace. This script sets
# PERL5LIB for you, because with it unset half of what you want to click
# through — hover and goto-def into CPAN — simply is not there.
set -uo pipefail
cd "$(dirname "$0")"

BULK=${PERL_CORPORA:-$HOME/perl-corpora}/bulk
DEPS=${PERL_CORPORA:-$HOME/perl-corpora}/deps
NODEPS=""; DRY=""; DEBUG=""
while :; do case "${1:-}" in
  --nodeps) NODEPS=1; shift;;
  --dry)    DRY=1; shift;;
  --debug)  DEBUG=1; shift;;
  *) break;; esac; done

# name|dir|cold wall|note   — walls are measured, deps installed, this box
ROWS=(
"bmo|BMO|13.8s|healthy reference, dense graph — good default"
"evergreen|Evergreen|17.2s|high fan-out, properly packaged"
"foswiki|Foswiki|12.8s|high fan-out, plugin-heavy"
"webwork|WeBWorK|9.9s|small but slowest per file"
"openfoodfacts|openfoodfacts|12.8s|densest static graph, tiny fan-out"
"webmin|Webmin|8.8s|path-based require, few packages — odd shape"
"koha|../koha|10.2s|library ILS (separate tree)"
"znuny|Znuny|79s|HEAVY: 8.2 GB, 3k files"
"fhem|FHEM|slow start|editor OK; --check dies at 12+ GB (batch verb only)"
"conch|Conch|8.2s|DBIC on a cross-file base: gated accessors everywhere"
)

pick_menu() {
  echo "corpora (cold --check wall, measured with deps):" >&2
  local i=1
  for r in "${ROWS[@]}"; do IFS='|' read -r k d w n <<< "$r"
    printf '  %2d) %-14s %-6s %s\n' "$i" "$k" "$w" "$n" >&2; i=$((i+1)); done
  printf '  choose [1]: ' >&2; read -r c </dev/tty; c=${c:-1}
  IFS='|' read -r KEY DIR W N <<< "${ROWS[$((c-1))]}"
}

if [ $# -eq 0 ]; then pick_menu; else
  want=$1; shift
  for r in "${ROWS[@]}"; do IFS='|' read -r k d w n <<< "$r"
    case "$k" in "$want"*) KEY=$k DIR=$d W=$w N=$n; break;; esac; done
  [ -z "${KEY:-}" ] && { echo "no corpus matching '$want'. run with no args for the menu." >&2; exit 1; }
fi

ROOT="$BULK/$DIR"
[ -d "$ROOT" ] || { echo "missing: $ROOT — run corpus/bootstrap.sh" >&2; exit 1; }
case "$N" in *"batch verb only"*)
  printf '\n  note: FHEM startup is slow (534 files declare `package main`), but the\n        editor is fine — the 12 GB blowup is --check sweeping every file,\n        not the server, which only enriches what you open.\n\n' >&2;;
esac

# Biggest files first: a 20-line module tells you nothing about how it feels.
FILTER=${1:-}
mapfile -t FILES < <(find "$ROOT" -name '*.pm' -not -path '*/local/*' -printf '%s\t%p\n' 2>/dev/null \
  | { [ -n "$FILTER" ] && [ "$FILTER" != "--list" ] && grep -i -- "$FILTER" || cat; } \
  | sort -rn | head -10 | cut -f2)
[ ${#FILES[@]} -eq 0 ] && { echo "no .pm matching '${FILTER}' in $KEY" >&2; exit 1; }

if [ "${FILTER:-}" = "--list" ]; then
  echo "biggest files in $KEY:" >&2; i=1
  for f in "${FILES[@]}"; do printf '  %2d) %6s lines  %s\n' "$i" "$(wc -l <"$f")" "${f#$ROOT/}" >&2; i=$((i+1)); done
  printf '  choose [1]: ' >&2; read -r c </dev/tty; TARGET=${FILES[$(( ${c:-1} - 1 ))]}
else
  TARGET=${FILES[0]}
fi

DEPDIR="$DEPS/$(basename "$DIR")/lib/perl5"
if [ -z "$NODEPS" ] && [ -d "$DEPDIR" ]; then
  export PERL5LIB="$DEPDIR"
  DEPNOTE="deps ON ($(find "$DEPDIR" -name '*.pm' 2>/dev/null | wc -l) modules on @INC)"
else
  DEPNOTE="deps OFF — CPAN imports will not resolve"
fi

printf '\n  %s  ·  %s cold  ·  %s\n  %s (%s lines)\n\n' \
  "$KEY" "$W" "$DEPNOTE" "${TARGET#$ROOT/}" "$(wc -l <"$TARGET")" >&2
if [ -n "$DEBUG" ]; then
  STAMP=$(date +%H%M%S)
  RUN=/tmp/perl-lsp-kick/$KEY-$STAMP; mkdir -p "$RUN"
  export PERL_LSP_DEBUG=1                    # server log -> /tmp/perl-lsp.log (dev.sh's path)
  export PERL_LSP_PHASE_TIMING=1             # cli::*/phase attribution
  export PERL_LSP_GHOST_STATS="$RUN/ghost.txt"   # counters, flushed at shutdown
  : > /tmp/perl-lsp.log
  cat >&2 <<EOF
  debug on:
    tail -f /tmp/perl-lsp.log            server log (live)
    $RUN/ghost.txt
                                         counters — written at SHUTDOWN, so
                                         :q the editor before reading them.
                                         A killed server writes nothing.
EOF
fi
[ -n "$DRY" ] && { echo "  (--dry: not launching)" >&2; exit 0; }
exec ./dev.sh "$TARGET"
