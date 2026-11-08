#!/bin/bash
# fetch1.sh <vendor_dir> <name> <url> <ref>: shallow-fetch one commit and export it the way a GitHub zipball (git archive) would.
set -e
V=$1; N=$2; U=$3; R=$4
T=$(mktemp -d "$V/../.fetch.XXXXXX")
git -C "$T" init -q
for i in 1 2 3; do git -C "$T" fetch -q --depth 1 "$U" "$R" && break; sleep 3; done
mkdir -p "$V/$N"
git -C "$T" archive FETCH_HEAD | tar -x -C "$V/$N"
rm -rf "$T"
echo "ok $N"
