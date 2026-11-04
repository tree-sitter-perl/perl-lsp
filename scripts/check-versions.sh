#!/usr/bin/env bash
# Usage: scripts/check-versions.sh [--release]
#
# Fails unless every file that carries the release version agrees with
# Cargo.toml. CI runs it on every PR, so a bump that misses a file fails
# before it merges. `--release` also requires the `## v<version>` section in
# CHANGELOG.md (release.yml runs it before creating the tag).
#
# A new file that carries the version gets a line in SITES below.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

want=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$want" ] || { echo "::error::no version in Cargo.toml"; exit 1; }

# file | what | extracted value
SITES=(
  "Cargo.lock|perl-lsp package|$(awk '/^name = "perl-lsp"$/ { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock)"
  "editors/vscode/package.json|version|$(sed -n 's/^  "version": "\(.*\)",$/\1/p' editors/vscode/package.json | head -1)"
  "editors/vscode/package-lock.json|version|$(sed -n 's/^  "version": "\(.*\)",$/\1/p' editors/vscode/package-lock.json | head -1)"
  "editors/vscode/package-lock.json|packages[\"\"].version|$(awk '/^    "": \{$/ { root = 1 } root && /"version":/ { gsub(/.*"version": "|",?$/, ""); print; exit }' editors/vscode/package-lock.json)"
  "editors/vscode/src/extension.ts|VERSION (binary download)|$(sed -n 's/^const VERSION = "\(.*\)";$/\1/p' editors/vscode/src/extension.ts)"
)

bad=0
for site in "${SITES[@]}"; do
  IFS='|' read -r file what got <<<"$site"
  if [ "$got" != "$want" ]; then
    echo "::error file=$file::$what is '${got:-<not found>}', Cargo.toml is '$want'"
    bad=1
  fi
done

if [ "${1:-}" = --release ] && ! grep -q "^## v$want\( \|$\)" CHANGELOG.md; then
  echo "::error file=CHANGELOG.md::no '## v$want' section"
  bad=1
fi

[ "$bad" = 0 ] && echo "All version strings are $want."
exit "$bad"
