#!/usr/bin/env python3
"""Emit a fixed sample of --batch probe requests for one corpus.

Usage: probes.py ROOT [--files N] [--per-file K] [--seed S] [--verbs hover,definition,...]

Picks N files (seeded, from the sorted list, so the same root always yields
the same sample) and, per line, the first `$var` and the first `->method`,
keeping at most --per-file positions per file (seeded too). --batch answers
about 7 probes a second (Conch, 2026-10-31), so the defaults keep a corpus
side to a few minutes.
Each position is asked every verb in --verbs (default hover,definition).
Positions are engine-native: 0-based line, byte column (the --batch input).
"""
import json
import os
import random
import re
import sys

SKIP_DIRS = {".git", "local", "node_modules", "blib"}
VAR = re.compile(r"\$([A-Za-z_]\w*)")
METHOD = re.compile(r"->([A-Za-z_]\w*)")


def main():
    args = sys.argv[1:]
    opts = {"--files": "15", "--per-file": "40", "--seed": "42", "--verbs": "hover,definition"}
    for k in list(opts):
        if k in args:
            i = args.index(k)
            opts[k] = args[i + 1]
            del args[i:i + 2]
    if len(args) != 1:
        sys.exit(__doc__)
    root = args[0]
    files = []
    for d, dirs, fs in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x not in SKIP_DIRS and not x.startswith("."))
        files += [os.path.join(d, f) for f in fs if f.endswith((".pm", ".pl", ".t"))]
    files.sort()
    rng = random.Random(int(opts["--seed"]))
    sample = sorted(rng.sample(files, min(int(opts["--files"]), len(files))))
    verbs = opts["--verbs"].split(",")
    n = 0
    for path in sample:
        try:
            lines = open(path, encoding="utf-8").read().split("\n")
        except (OSError, UnicodeDecodeError):
            continue
        rel = os.path.relpath(path, root)
        positions = []
        for i, line in enumerate(lines):
            if line.lstrip().startswith("#"):
                continue
            for kind, rx in (("var", VAR), ("method", METHOD)):
                m = rx.search(line)
                if m:
                    positions.append((kind, i, len(line[: m.start(1)].encode())))
        keep = int(opts["--per-file"])
        if len(positions) > keep:
            positions = sorted(rng.sample(positions, keep), key=lambda p: (p[1], p[2]))
        for kind, i, col in positions:
            for verb in verbs:
                print(json.dumps({"id": f"{verb}:{kind}:{rel}:{i}:{col}", "q": verb,
                                  "file": path, "line": i, "col": col}))
                n += 1
    print(f"probes: {len(sample)} files, {n} requests", file=sys.stderr)


if __name__ == "__main__":
    main()
