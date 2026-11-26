#!/usr/bin/env python3
"""Emit a fixed sample of --batch probe requests for one corpus.

Usage: probes.py ROOT [--files N] [--per-file K] [--seed S] [--verbs hover,definition,...] [--lang perl|php|python] [--unclosed]

Picks N files (seeded, from the sorted list, so the same root always yields
the same sample) and, per line, the first `$var` and the first `->method`,
keeping at most --per-file positions per file (seeded too). --batch answers
about 7 probes a second (Conch, 2026-10-31), so the defaults keep a corpus
side to a few minutes.
Each position is asked every verb in --verbs (default hover,definition).
Positions are engine-native: 0-based line, byte column (the --batch input).

--unclosed asks about calls still being typed instead. Per call `name(args)`
written on one line it keeps the first k arguments (k seeded) and asks at the
cursor after them, in two buffers sent as the request's `text`: `eof`, the
file cut at the cursor, and `open`, the file with the call's `)` and the
arguments after the cursor deleted, so code follows the unclosed call.
"""
import json
import os
import random
import re
import sys

SKIP_DIRS = {".git", "local", "node_modules", "blib"}
# Per language: the files it serves, a comment line's lead, the first
# variable on a line and the first method call.
LANGS = {
    "perl": ((".pm", ".pl", ".t"), "#", re.compile(r"\$([A-Za-z_]\w*)"), re.compile(r"->([A-Za-z_]\w*)")),
    "php": ((".php",), "//", re.compile(r"\$([A-Za-z_]\w*)"), re.compile(r"->([A-Za-z_]\w*)")),
    "python": ((".py",), "#", re.compile(r"(?<![.\w])([a-z_]\w*)\b(?!\s*[(=])"), re.compile(r"\.([A-Za-z_]\w*)\s*\(")),
}
PY_KEYWORDS = {"if", "elif", "else", "for", "while", "in", "not", "and", "or", "is", "return", "def", "class",
               "import", "from", "as", "with", "try", "except", "finally", "raise", "pass", "break", "continue",
               "lambda", "yield", "assert", "del", "global", "nonlocal", "await", "async", "self", "cls"}


def main():
    args = sys.argv[1:]
    opts = {"--files": "15", "--per-file": "40", "--seed": "42", "--verbs": "hover,definition", "--lang": "perl"}
    unclosed = "--unclosed" in args
    if unclosed:
        args.remove("--unclosed")
    for k in list(opts):
        if k in args:
            i = args.index(k)
            opts[k] = args[i + 1]
            del args[i:i + 2]
    if len(args) != 1:
        sys.exit(__doc__)
    root = args[0]
    exts, comment, var, method = LANGS[opts["--lang"]]
    files = []
    # An unclosed call is asked in project code, the buffers people type in.
    skip = SKIP_DIRS | ({"vendor", "venv"} if unclosed else set())
    for d, dirs, fs in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x not in skip and not x.startswith("."))
        files += [os.path.join(d, f) for f in fs if f.endswith(exts)]
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
        if unclosed:
            n += emit_unclosed(path, rel, lines, comment, rng, int(opts["--per-file"]), verbs)
            continue
        positions = []
        for i, line in enumerate(lines):
            if line.lstrip().startswith(comment):
                continue
            for kind, rx in (("var", var), ("method", method)):
                m = next((m for m in rx.finditer(line) if m.group(1) not in PY_KEYWORDS), None)
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


CALL = re.compile(r"(?<![\w$])([A-Za-z_]\w*)\(")
# A declaration's parameter list is not a call.
DECLARES = re.compile(r"\b(?:def|function|fn|class)\s+$")


def call_args(line, open_at):
    """The top-level argument spans of the call whose `(` is at `open_at`,
    and its `)`'s index; None when the line does not close it. A string or
    a nested bracket is skipped whole, crudely (no escapes beyond `\\`)."""
    depth, quote, start, spans, i = 0, None, open_at + 1, [], open_at + 1
    while i < len(line):
        c = line[i]
        if quote:
            if c == "\\":
                i += 1
            elif c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                if c != ")":
                    return None
                if line[start:i].strip():
                    spans.append((start, i))
                return spans, i
            depth -= 1
        elif c == "," and depth == 0:
            spans.append((start, i))
            start = i + 1
        i += 1
    return None


def emit_unclosed(path, rel, lines, comment, rng, keep, verbs):
    sites = []
    for i, line in enumerate(lines):
        if line.lstrip().startswith((comment, "*", "/*")):
            continue
        for m in CALL.finditer(line):
            if m.group(1) in PY_KEYWORDS or DECLARES.search(line[:m.start(1)]):
                continue
            got = call_args(line, m.end() - 1)
            if got:
                sites.append((i, m.end() - 1, got))
    if len(sites) > keep:
        sites = sorted(rng.sample(sites, keep))
    n = 0
    for i, open_at, (spans, close_at) in sites:
        k = rng.randint(0, len(spans))
        line = lines[i]
        kept = line[open_at + 1:spans[k - 1][1]].strip() + ", " if k else ""
        head = line[:open_at + 1] + kept
        col = len(head.encode())
        before = "\n".join(lines[:i] + [head])
        buffers = {
            "eof": before,
            "open": "\n".join(lines[:i] + [head + line[close_at + 1:]] + lines[i + 1:]),
        }
        for variant, text in buffers.items():
            for verb in verbs:
                print(json.dumps({"id": f"{verb}:unclosed-{variant}:{rel}:{i}:{col}:k{k}", "q": verb,
                                  "file": path, "line": i, "col": col, "text": text}))
                n += 1
    return n


if __name__ == "__main__":
    main()
