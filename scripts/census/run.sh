#!/usr/bin/env bash
# Answer census for one binary over one corpus: every diagnostic (hint and
# up) plus the answer to every probe. See README.md.
#
#   scripts/census/run.sh BIN ROOT PROBES.jsonl OUT_DIR
#
# PERL5LIB is passed through (deps on @INC, never in the workspace). The
# probes run in --batch after --check, against the cache --check wrote:
# one startup for the census, the same for both sides.
set -uo pipefail
bin=$1 root=$2 probes=$3 out=$4
mkdir -p "$out"
cache=$(mktemp -d)
trap 'rm -rf "$cache"' EXIT
t0=$SECONDS
XDG_CACHE_HOME=$cache "$bin" --check "$root" --severity hint --format json >"$out/diagnostics.json" 2>"$out/check.stderr"
echo "census: --check rc=$? $((SECONDS - t0)) s" >&2
t0=$SECONDS
XDG_CACHE_HOME=$cache "$bin" --batch "$root" <"$probes" >"$out/answers.jsonl" 2>"$out/batch.stderr"
echo "census: --batch rc=$? $((SECONDS - t0)) s, $(wc -l <"$out/answers.jsonl") answers" >&2
