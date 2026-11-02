#!/usr/bin/env bash
# Whole-analysis equivalence: build BASE and the working tree, dump every
# analysis each builds over the same inputs, and diff. See README.md.
#
#   scripts/equiv/run.sh [BASE_REV]        default: merge-base with origin/main
#
# Env:
#   EQUIV_WORK      scratch root (worktrees, target dirs, dumps)
#                   default ${XDG_CACHE_HOME:-~/.cache}/perl-lsp-equiv
#   EQUIV_FEATURES  cargo features for both sides (default cpp,php,python)
#   EQUIV_TESTS     1 = also dump every build `cargo test` makes (default 1)
#   EQUIV_EXTRA     extra Perl paths to dump (space-separated)
#   TSP_DIR         tree-sitter-perl checkout whose test/corpus is split into
#                   inputs (default ../tree-sitter-perl, skipped if absent)
#   EQUIV_KEEP      1 = keep the dumps of a passing run (default: deleted;
#                   a failing run always keeps them)
set -uo pipefail

repo=$(git rev-parse --show-toplevel)
cd "$repo"
base_rev=${1:-$(git merge-base HEAD origin/main)}
base_sha=$(git rev-parse --verify "$base_rev^{commit}") || { echo "equiv: no such rev $base_rev" >&2; exit 2; }
work=${EQUIV_WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/perl-lsp-equiv}
features=${EQUIV_FEATURES:-cpp,php,python}
run_tests=${EQUIV_TESTS:-1}
tsp=${TSP_DIR:-$repo/../tree-sitter-perl}
stamp=$(date -u +%Y%m%dT%H%M%SZ)
out=$work/runs/$stamp
mkdir -p "$out"

# The base side is a detached worktree, reused across runs at the same sha
# so its target dir stays warm.
base_tree=$work/base-$base_sha
if [ ! -d "$base_tree" ]; then
    git worktree add --detach "$base_tree" "$base_sha" >/dev/null || exit 2
fi

# Grammar corpus entries become one input file each (split once).
tsp_inputs=$work/tsp-corpus
if [ -d "$tsp/test/corpus" ] && [ ! -d "$tsp_inputs" ]; then
    mkdir -p "$tsp_inputs.tmp"
    python3 "$repo/scripts/equiv/split_corpus.py" "$tsp/test/corpus" "$tsp_inputs.tmp" >/dev/null && mv "$tsp_inputs.tmp" "$tsp_inputs"
fi

# Inputs are read from the HEAD tree for both sides, so a fixture the change
# edits is the same bytes on each side.
perl_inputs=("$repo/test_files" "$repo/gold-corpus")
[ -d "$tsp_inputs" ] && perl_inputs+=("$tsp_inputs")
# shellcheck disable=SC2206
[ -n "${EQUIV_EXTRA:-}" ] && perl_inputs+=($EQUIV_EXTRA)
[ -d "$repo/gold-corpus/local" ] || echo "equiv: no gold-corpus/local in $repo; the substrate is not in this run (pass it in EQUIV_EXTRA)" >&2
cpp_inputs=("$repo/test_files/cpp" "$repo/gold-corpus/cpp-fixture" "$repo/gold-corpus/cpp-diag-fixture")

side() {
    local label=$1 tree=$2
    local target=$work/target-$label
    local dirty=""
    git -C "$tree" diff --quiet HEAD || dirty="+uncommitted"
    echo "equiv: [$label] building $(git -C "$tree" rev-parse --short HEAD)$dirty" >&2
    (cd "$tree" && CARGO_TARGET_DIR=$target cargo build --release --features "$features" -q >"$out/$label.build.log" 2>&1) \
        || { echo "equiv: [$label] build failed, see $out/$label.build.log" >&2; return 1; }
    local bin=$target/release/perl-lsp
    if ! "$bin" --help 2>&1 | grep -q -- --equiv-dump; then
        echo "equiv: [$label] binary has no --equiv-dump (base predates the net)" >&2
        return 1
    fi
    local t0=$SECONDS
    "$bin" --equiv-dump "$out/$label/cli" "${perl_inputs[@]}" 2>>"$out/$label.log"
    "$bin" --lang cpp --equiv-dump "$out/$label/cli" "${cpp_inputs[@]}" 2>>"$out/$label.log"
    echo "equiv: [$label] cli dump $((SECONDS - t0)) s" >&2
    if [ "$run_tests" = 1 ]; then
        t0=$SECONDS
        (cd "$tree" && CARGO_TARGET_DIR=$work/target-$label-test PERL_LSP_EQUIV_DUMP_DIR=$out/$label/tests \
            cargo test --features "$features" -q >"$out/$label.tests.log" 2>&1) \
            || echo "equiv: [$label] cargo test failed (dumps kept; see $out/$label.tests.log)" >&2
        echo "equiv: [$label] test dump $((SECONDS - t0)) s" >&2
    fi
}

side base "$base_tree" || exit 2
side head "$repo" || exit 2

rc=0
echo "== cli inputs (fixtures, gold substrate, grammar corpus) =="
roots=(--root "$repo" --root "$base_tree")
python3 "$repo/scripts/equiv/diff.py" "${roots[@]}" "$out/base/cli" "$out/head/cli" || rc=1
if [ "$run_tests" = 1 ]; then
    echo "== cargo test inputs =="
    python3 "$repo/scripts/equiv/diff.py" "${roots[@]}" "$out/base/tests" "$out/head/tests" || rc=1
fi
# A run's dumps are about a gigabyte; only a failure needs them read.
if [ $rc = 0 ] && [ "${EQUIV_KEEP:-0}" != 1 ]; then
    rm -rf "$out/base" "$out/head"
    echo "equiv: base $base_sha, passed (dumps deleted; logs in $out)"
else
    echo "equiv: base $base_sha, dumps in $out"
fi
exit $rc
