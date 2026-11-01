#!/usr/bin/env bash
# Answer census, base binary vs head binary, over the real-project corpora.
# Writes one ledger per corpus plus ledger.md (all of them). See README.md.
#
#   scripts/census/census.sh BASE_BIN HEAD_BIN [CORPUS...]   default: every corpus
#
# Env: PERL_CORPORA (default ~/perl-corpora), CENSUS_OUT (default
# ./census-<stamp>), CENSUS_FILES (files sampled per corpus, default 15),
# CENSUS_PER_FILE (positions per file, default 40), CENSUS_VERBS (default
# hover,definition).
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
base_bin=$1 head_bin=$2; shift 2
corpora=${PERL_CORPORA:-$HOME/perl-corpora}
out=${CENSUS_OUT:-$PWD/census-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$out"
if [ $# -gt 0 ]; then names=("$@"); else
    names=(); for d in "$corpora"/bulk/*/; do names+=("$(basename "$d")"); done
fi
rc=0
: >"$out/ledger.md"
for c in "${names[@]}"; do
    root=$corpora/bulk/$c
    [ -d "$root" ] || { echo "census: skip $c (missing)" >&2; continue; }
    pl=""; [ -d "$corpora/deps/$c/lib/perl5" ] && pl=$corpora/deps/$c/lib/perl5
    python3 "$here/probes.py" "$root" --files "${CENSUS_FILES:-15}" --per-file "${CENSUS_PER_FILE:-40}" --verbs "${CENSUS_VERBS:-hover,definition}" >"$out/$c.probes.jsonl"
    for side in base head; do
        bin=$base_bin; [ $side = head ] && bin=$head_bin
        echo "census: $c [$side]" >&2
        PERL5LIB=$pl "$here/run.sh" "$bin" "$root" "$out/$c.probes.jsonl" "$out/$c/$side"
    done
    python3 "$here/ledger.py" --label "$c" "$out/$c/base" "$out/$c/head" --json "$out/$c.ledger.json" >>"$out/ledger.md" || rc=1
    echo >>"$out/ledger.md"
done
cat "$out/ledger.md"
echo "census: outputs in $out" >&2
exit $rc
