#!/usr/bin/env python3
"""Split tree-sitter corpus files into one source file per entry.

Usage: split_corpus.py CORPUS_DIR OUT_DIR [EXT]   (EXT default .pl)

A corpus entry is `===\\nname\\n===\\n<code>\\n---\\n<expected tree>`; each
entry's code becomes OUT_DIR/<corpus file>-<n><EXT>, named stably so the
same grammar checkout always yields the same inputs.
"""
import os
import re
import sys

src, out = sys.argv[1], sys.argv[2]
ext = sys.argv[3] if len(sys.argv) > 3 else ".pl"
os.makedirs(out, exist_ok=True)
n = 0
for f in sorted(os.path.join(d, x) for d, _, xs in os.walk(src) for x in xs if not x.startswith(".")):
    text = open(f, encoding="utf-8", errors="replace").read()
    parts = re.split(r"^={3,}[^\n]*\n.*?\n={3,}[^\n]*\n", text, flags=re.M | re.S)
    stem = os.path.splitext(os.path.relpath(f, src).replace(os.sep, "_"))[0]
    for i, p in enumerate(parts[1:]):
        code = re.split(r"^-{3,}\s*$", p, maxsplit=1, flags=re.M)[0]
        open(os.path.join(out, f"{stem}-{i:04d}{ext}"), "w").write(code)
        n += 1
print(n)
