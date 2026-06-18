#!/usr/bin/env bash
# Usage: ./scripts/release-notes.sh v0.5.0 [CHANGELOG.md]
#
# Prints the CHANGELOG.md section for a release tag — the exact body the
# GitHub Release uses (see .github/workflows/release.yml). Run it to preview
# what a `git push origin v<x.y.z>` will publish.
set -euo pipefail

TAG="${1:?Usage: $0 <tag> [changelog]}"
CHANGELOG="${2:-CHANGELOG.md}"

# Section = the lines under `## <tag>` up to the next `## ` header. Headers
# are dated (`## v0.5.0 — 2026-06-18`), so match the tag as a PREFIX — an
# exact-line match misses the date suffix and silently yields nothing.
section=$(awk -v hdr="## ${TAG}" '
  $0 == hdr || index($0, hdr " ") == 1 { found = 1; next }
  /^## / && found { exit }
  found
' "$CHANGELOG" | awk 'NF { seen = 1 } seen')   # drop leading blank lines

if [ -z "$section" ]; then
  echo "Release ${TAG}."
  echo "::warning::No ${CHANGELOG} section for ${TAG}; using a stub body." >&2
  exit 0
fi

printf '%s\n' "$section"
