#!/usr/bin/env python3
"""Compare two equivalence-dump directories (base, head).

Usage: diff.py BASE_DIR HEAD_DIR [--show N] [--root PATH]...

A dump file is <input>.<content>.json (src/build/equiv_dump.rs). An input is
identical when both sides hold the same SET of content hashes for it, so the
check is a name comparison; contents are opened only to explain a change.

Some facts carry absolute paths (a pack's include closure, a test's temp
dir in its own source). Two checkouts, or two runs of one test, differ only
there, so an input whose dumps differ by name is re-compared after every
`--root PATH` (the two trees) becomes `<root>` and every `/tmp/<name>` becomes
`/tmp/<tmp>`; an input built only on one side is paired with one built only
on the other whose source normalizes the same. Only what survives that is a
change.

Exit 0: no input changed and none was dropped. Exit 1 otherwise. Inputs only
in HEAD (a test the change adds) are listed, never a failure.
"""
import difflib
import hashlib
import json
import os
import re
import sys

TMP = re.compile(r"/tmp/[^/\"\s']+")
ROOTS = []


def norm(text):
    for r in ROOTS:
        text = text.replace(r, "<root>")
    return TMP.sub("/tmp/<tmp>", text)


def digest(text):
    return hashlib.sha1(norm(text).encode()).hexdigest()


def normalized_variants(d, inp, variants):
    return {digest(open(os.path.join(d, f"{inp}.{v}.json")).read()) for v in variants}


def src_digest(d, inp):
    p = os.path.join(d, inp + ".src")
    return digest(open(p, errors="replace").read()) if os.path.exists(p) else None


def load(d):
    by_input = {}
    for name in os.listdir(d):
        if not name.endswith(".json"):
            continue
        inp, content, _ = name.split(".")
        by_input.setdefault(inp, set()).add(content)
    return by_input


def where(d, inp):
    for side in d:
        p = os.path.join(side, inp + ".where")
        if os.path.exists(p):
            return open(p).read().strip()
    src = next((os.path.join(s, inp + ".src") for s in d if os.path.exists(os.path.join(s, inp + ".src"))), None)
    if src:
        first = open(src, errors="replace").readline().strip()
        return f"(inline source, first line: {first[:60]!r}; full text in {src})"
    return "(unknown)"


def first_difference(a, b, path="$"):
    if type(a) is not type(b):
        return path
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if a.get(k) != b.get(k):
                return first_difference(a.get(k), b.get(k), f"{path}.{k}")
    elif isinstance(a, list):
        for i, (x, y) in enumerate(zip(a, b)):
            if x != y:
                return first_difference(x, y, f"{path}[{i}]")
        if len(a) != len(b):
            return f"{path} (len {len(a)} vs {len(b)})"
    return path


def explain(base, head, inp, b_variants, h_variants, out, hinp=None):
    hinp = hinp or inp
    out.append(f"  {inp}  {where((base, head), inp)}")
    if len(b_variants) == 1 and len(h_variants) == 1:
        bp = os.path.join(base, f"{inp}.{next(iter(b_variants))}.json")
        hp = os.path.join(head, f"{hinp}.{next(iter(h_variants))}.json")
        bj, hj = json.loads(norm(open(bp).read())), json.loads(norm(open(hp).read()))
        out.append(f"    first difference at {first_difference(bj, hj)}")
        pretty = lambda j: json.dumps(j, indent=1, sort_keys=True).splitlines()
        lines = list(difflib.unified_diff(pretty(bj), pretty(hj), "base", "head", n=2, lineterm=""))
        out.extend("    " + l for l in lines[:40])
        if len(lines) > 40:
            out.append(f"    ... {len(lines) - 40} more diff lines: diff {bp} {hp}")
    else:
        out.append(f"    built {len(b_variants)} way(s) in base, {len(h_variants)} in head; "
                   f"variants base={sorted(b_variants)} head={sorted(h_variants)}")


def main():
    args = sys.argv[1:]
    show = 5
    while "--show" in args or "--root" in args:
        i = args.index("--show") if "--show" in args else args.index("--root")
        if args[i] == "--show":
            show = int(args[i + 1])
        else:
            ROOTS.append(os.path.realpath(args[i + 1]))
        del args[i:i + 2]
    ROOTS.sort(key=len, reverse=True)
    if len(args) != 2:
        sys.exit(__doc__)
    base, head = args
    b, h = load(base), load(head)
    changed = sorted(k for k in b.keys() & h.keys()
                     if b[k] != h[k] and normalized_variants(base, k, b[k]) != normalized_variants(head, k, h[k]))
    dropped = sorted(b.keys() - h.keys())
    new = sorted(h.keys() - b.keys())
    # Pair one-sided inputs whose sources differ only by path.
    by_src = {}
    for k in new:
        by_src.setdefault(src_digest(head, k), []).append(k)
    paired = 0
    head_key = {}
    for k in list(dropped):
        cands = by_src.get(src_digest(base, k)) or []
        if not cands or src_digest(base, k) is None:
            continue
        hk = cands.pop(0)
        dropped.remove(k)
        new.remove(hk)
        paired += 1
        if normalized_variants(base, k, b[k]) != normalized_variants(head, hk, h[hk]):
            changed.append(k)
            head_key[k] = hk
    same = len(b.keys() & h.keys()) + paired - len(changed)
    print(f"equiv: {len(b)} inputs in base, {len(h)} in head: "
          f"{same} identical, {len(changed)} changed, {len(dropped)} dropped, {len(new)} new")
    out = []
    if changed:
        out.append(f"CHANGED ({len(changed)}):")
        for inp in changed[:show]:
            hk = head_key.get(inp, inp)
            explain(base, head, inp, b[inp], h[hk], out, hk)
        if len(changed) > show:
            out.append(f"  ... {len(changed) - show} more (--show N)")
    if dropped:
        out.append(f"DROPPED, built in base only ({len(dropped)}):")
        out.extend(f"  {inp}  {where((base,), inp)}" for inp in dropped[:show])
    if new:
        out.append(f"NEW, built in head only ({len(new)}):")
        out.extend(f"  {inp}  {where((head,), inp)}" for inp in new[:show])
    print("\n".join(out))
    sys.exit(1 if changed or dropped else 0)


if __name__ == "__main__":
    main()
