#!/usr/bin/env python3
"""Diff two census outputs (base, head) into a delta ledger.

Usage: ledger.py [--label NAME] BASE_OUT HEAD_OUT [--json FILE]

Prints a Markdown ledger: diagnostics grouped by code (removed / added), and
probe answers grouped by verb (added / dropped / changed), each group with a
count, one example, and a `class` cell for the author to fill: fixed,
intended, or regression. A regression blocks the PR; each fixed group lands a
gold row. Exit 0 when nothing moved, 1 otherwise.
"""
import collections
import json
import sys


def diagnostics(d):
    try:
        rows = json.load(open(f"{d}/diagnostics.json"))
    except (OSError, ValueError):
        sys.exit(f"ledger: {d}/diagnostics.json missing or not JSON (see {d}/check.stderr)")
    return collections.Counter(
        (r["code"] or "?", r["file"], r["line"], r["col"], r["severity"], r["message"]) for r in rows)


def normalize(out):
    """A JSON answer whose top level is a list is compared as a set: order
    is not part of what a references/definition answer means."""
    try:
        v = json.loads(out)
    except (TypeError, ValueError):
        return out
    if isinstance(v, list):
        v = sorted(v, key=lambda x: json.dumps(x, sort_keys=True))
    return json.dumps(v, sort_keys=True)


def answers(d):
    out = {}
    for line in open(f"{d}/answers.jsonl"):
        r = json.loads(line)
        if "id" not in r:
            continue
        out[r["id"]] = normalize(r["out"]) if r.get("ok") else None
    return out


def empty(a):
    return a is None or a in ("", "null", "[]", "{}")


def short(s, n=160):
    s = "∅" if s is None else str(s).replace("\n", " ")
    return s if len(s) <= n else s[: n - 1] + "…"


def main():
    args = sys.argv[1:]
    label, jpath = None, None
    for flag in ("--label", "--json"):
        if flag in args:
            i = args.index(flag)
            if flag == "--label":
                label = args[i + 1]
            else:
                jpath = args[i + 1]
            del args[i:i + 2]
    if len(args) != 2:
        sys.exit(__doc__)
    base, head = args
    title = label or f"{base} → {head}"

    db, dh = diagnostics(base), diagnostics(head)
    removed, added = db - dh, dh - db
    by_code = collections.defaultdict(lambda: {"removed": [], "added": []})
    for k, n in removed.items():
        by_code[k[0]]["removed"] += [k] * n
    for k, n in added.items():
        by_code[k[0]]["added"] += [k] * n

    ab, ah = answers(base), answers(head)
    groups = collections.defaultdict(list)  # (verb, delta) -> [(id, base, head)]
    for pid in sorted(ab.keys() | ah.keys()):
        b, h = ab.get(pid), ah.get(pid)
        if b == h:
            continue
        verb = pid.split(":", 1)[0]
        delta = "added" if empty(b) else "dropped" if empty(h) else "changed"
        groups[(verb, delta)].append((pid, b, h))
    same = sum(1 for pid in ab.keys() & ah.keys() if ab[pid] == ah[pid])

    print(f"## Census: {title}\n")
    print(f"Diagnostics: {sum(db.values())} → {sum(dh.values())} "
          f"({sum(removed.values())} removed, {sum(added.values())} added). "
          f"Probes: {len(ab.keys() | ah.keys())}, {same} identical, "
          f"{sum(len(v) for v in groups.values())} moved.\n")
    if by_code or groups:
        print("| group | delta | n | example | class |")
        print("|---|---|---|---|---|")
        for code in sorted(by_code):
            for delta in ("removed", "added"):
                rows = by_code[code][delta]
                if rows:
                    _, f, line, col, sev, msg = sorted(rows)[0]
                    ex = f"`{f}:{line + 1}:{col + 1}` {sev}: {short(msg, 100)}"
                    print(f"| diag `{code}` | {delta} | {len(rows)} | {ex} | ? |")
        for (verb, delta) in sorted(groups):
            rows = groups[(verb, delta)]
            pid, b, h = rows[0]
            loc = pid.split(":", 2)[2]
            ex = f"`{loc}` {short(b, 70)} → {short(h, 70)}"
            print(f"| {verb} | {delta} | {len(rows)} | {ex} | ? |")
        print("\nclass: `fixed` (lands a gold row), `intended` (says why), or "
              "`regression` (blocks).")
    else:
        print("No deltas.")

    if jpath:
        json.dump({
            "title": title,
            "diagnostics": {"base": sum(db.values()), "head": sum(dh.values()),
                            "by_code": {c: {k: len(v) for k, v in d.items()} for c, d in by_code.items()}},
            "probes": {"total": len(ab.keys() | ah.keys()), "identical": same,
                       "groups": {f"{v}:{d}": [list(r) for r in rows] for (v, d), rows in groups.items()}},
        }, open(jpath, "w"), indent=1)
    sys.exit(1 if by_code or groups else 0)


if __name__ == "__main__":
    main()
