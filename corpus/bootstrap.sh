#!/usr/bin/env bash
# Reconstruct the scaling corpus on a fresh box (cloud agent, CI, new laptop).
#
#   corpus/bootstrap.sh [dest]        # default: $PERL_CORPORA/bulk
#   corpus/bootstrap.sh dest FHEM     # one repo
#
# `$PERL_CORPORA` (default ~/perl-corpora) is the ONE root, shared with
# kick.sh: an explicit [dest] that disagrees with it produces a corpus kick.sh
# cannot find.
#
# Needs: git, perl >= 5.20 with Module::CoreList (core), cpm
#   curl -sSL https://raw.githubusercontent.com/skaji/cpm/main/cpm -o /tmp/cpm && chmod +x /tmp/cpm
#
# Each repo is pinned to the commit the recorded measurements used, fetched
# shallow. Idempotent: an existing clone is left alone (a warning names it if it
# sits off its pin), existing local/ trees are topped up, and house plugins are
# re-copied every run.
set -u
DEST="${1:-${PERL_CORPORA:-$HOME/perl-corpora}/bulk}"
DEPS="$(dirname "$DEST")/deps"
ONLY="${2:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CPM="$(command -v cpm || echo /tmp/cpm)"

# The nine keepers. Each earned its slot; see README.md for the measurement.
# A pin moves only with a re-measure: every recorded number is against it, and
# the house plugins (house/<name>/) are copied out of the source at it.
#   name|url|commit|why
REPOS=(
"FHEM|https://github.com/fhem/fhem-mirror|f0ad3f52c0a21788b62257bf9ee0c083c5464f0d|main-monoculture: 534 providers of one package name"
"Foswiki|https://github.com/foswiki/distro|c6491a777541fb5288d9487fac0a9a6dceb3791a|high fan-out, 'other'-driven — a second mechanism"
"Evergreen|https://github.com/evergreen-library-system/Evergreen|96e49f91ed05cd4dc4218be578ff30a13996be13|high fan-out, 262/262 properly packaged"
"WeBWorK|https://github.com/openwebwork/webwork2|e692d816b87e202f77a8897fc9bde791d03bfa3c|worst per-file cost, not fan-out"
"Znuny|https://github.com/Znuny/Znuny|234947443653d709a2e916f8bfe10d4c784b8db1|largest corpus, LOWEST fan-out — size/fan-out are independent"
"Webmin|https://github.com/webmin/webmin|60f980d83245191e26a67e63cfb86c7652cb613b|path-based require, 101 package decls, lowest hit rate"
"BMO|https://github.com/mozilla-bteam/bmo|635a95c106cfe0cbf71809ffc244ae431100a4d1|healthy reference: 206 att/file at 99.90% hit"
"openfoodfacts|https://github.com/openfoodfacts/openfoodfacts-server|978319802a53816791de579d6c967c44ed67bc0a|densest static graph, near-zero fan-out"
"Conch|https://github.com/joyent/conch|fc8a0e3d7a0ec15bc216ee0843153225cccd1e8e|DBIC schema on a cross-file base: every result class's accessors are gated"
)

mkdir -p "$DEST" || exit 1
for ENTRY in "${REPOS[@]}"; do
  IFS='|' read -r NAME URL PIN WHY <<< "$ENTRY"
  [ -n "$ONLY" ] && [ "$ONLY" != "$NAME" ] && continue
  echo "══ $NAME — $WHY"
  if [ -d "$DEST/$NAME/.git" ]; then
    AT="$(git -C "$DEST/$NAME" rev-parse HEAD 2>/dev/null)"
    if [ "$AT" = "$PIN" ]; then
      echo "   clone: present at pin"
    else
      echo "   clone: present but at ${AT:-?}, NOT the pin ${PIN:0:12} — numbers won't compare"
      echo "          git -C $DEST/$NAME fetch --depth 1 origin $PIN && git -C $DEST/$NAME checkout FETCH_HEAD"
    fi
  else
    # GitHub serves a commit by id, so a pinned clone stays shallow.
    { git init -q "$DEST/$NAME" &&
      git -C "$DEST/$NAME" remote add origin "$URL" &&
      git -C "$DEST/$NAME" fetch -q --depth 1 origin "$PIN" &&
      git -C "$DEST/$NAME" checkout -q FETCH_HEAD; } ||
      { echo "   CLONE FAILED"; rm -rf "$DEST/$NAME"; continue; }
    echo "   clone: ok at ${PIN:0:12}"
  fi

  # House plugins: what a team would ship for its own runtime magic (exports
  # built at runtime, globals another file declares). The server loads
  # <root>/.perl-lsp/*.rhai, so they reach every run on this root with no env.
  if [ -d "$HERE/house/$NAME" ]; then
    mkdir -p "$DEST/$NAME/.perl-lsp"
    rm -f "$DEST/$NAME/.perl-lsp"/house-*.rhai
    for f in "$HERE/house/$NAME"/*.rhai; do cp "$f" "$DEST/$NAME/.perl-lsp/house-$(basename "$f")"; done
    grep -qx '/.perl-lsp/' "$DEST/$NAME/.git/info/exclude" 2>/dev/null ||
      echo '/.perl-lsp/' >> "$DEST/$NAME/.git/info/exclude"
    echo "   house plugins: $(ls "$HERE/house/$NAME" | tr '\n' ' ')"
  fi

  # Dependency manifest: use the repo's own if it has one, else derive.
  # Six of the nine ship none — they are applications, not CPAN dists.
  CF=""
  for c in cpanfile Makefile.PL Build.PL; do [ -f "$DEST/$NAME/$c" ] && CF="$c" && break; done
  if [ -z "$CF" ]; then
    perl "$HERE/derive-cpanfile.pl" "$DEST/$NAME" > "$DEST/$NAME/cpanfile.derived" 2>/dev/null
    CF=cpanfile.derived
    echo "   manifest: derived ($(grep -c '^requires' "$DEST/$NAME/$CF") deps)"
  else
    echo "   manifest: $CF (shipped)"
  fi

  # Install into the repo's own local/. Failures are EXPECTED and non-fatal:
  # XS modules wanting system headers we may not have. A partial local/ is
  # strictly better than none — the point is to make imports RESOLVABLE, and an
  # unresolvable import costs the resolver nothing, which is the bias that makes
  # a corpus measure the wrong thing.
  # Install OUTSIDE the repo, into $DEPS/$NAME. This is load-bearing, not tidiness:
  # the workspace walker indexes everything under the root, so a local/ inside the
  # repo joins the WORKSPACE tier. Measured: FHEM went 929 -> 33,912 indexed files,
  # 97% of them Paws (the AWS SDK ships one .pm per API call). The corpus would
  # have been measuring Paws. Deps belong on the @INC/dependency tier, which is
  # also how a real editor session sees them.
  mkdir -p "$DEPS"
  ( cd "$DEST/$NAME" || exit
    ARGS=(install -L "$DEPS/$NAME" --no-test)
    [ "$CF" = cpanfile.derived ] && ARGS+=(--cpanfile cpanfile.derived)
    timeout 2400 "$CPM" "${ARGS[@]}" > "$DEPS/$NAME.cpm.log" 2>&1
    N=$(find "$DEPS/$NAME" -name '*.pm' 2>/dev/null | wc -l)
    R=$(grep -c '^FAIL resolve' "$DEPS/$NAME.cpm.log" 2>/dev/null || echo 0)
    I=$(grep -c '^FAIL install' "$DEPS/$NAME.cpm.log" 2>/dev/null || echo 0)
    echo "   deps: $N modules -> $DEPS/$NAME  ($R unresolvable, $I build-failed)"
  )
done
echo
echo "Workspace corpora: $DEST"
echo "Dependencies:      $DEPS   (per-repo logs: $DEPS/<name>.cpm.log)"
echo
echo "Measure with deps on @INC, NOT in the workspace:"
echo "  PERL5LIB=$DEPS/FHEM/lib/perl5 perl-lsp --check $DEST/FHEM"
