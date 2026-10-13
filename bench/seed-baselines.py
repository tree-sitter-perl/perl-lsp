#!/usr/bin/env python3
"""Append KPI baseline rows to bench/baselines.jsonl from a harness run.

  bench/seed-baselines.py <run.jsonl> [--note "why this run is trusted"]

Baselines are CURATED: run this deliberately after a sweep you trust, review
the diff, commit it. Never wired into collection — an auto-appended baseline
is just a moving average wearing a baseline's clothes.

What qualifies as a KPI lives here, on purpose, in one place:
  batch : check.wall + peak RSS (cold and warm medians), per-file build
          p50/p99/max — the leading indicator that catches the next
          20-second file before a user does. The p99 row also names
          its `tail`: every file at or above it, so a move says which.
  editor: startup ready, per-verb p50/p99, diagnostics push, server RSS
          (from lsp_bench --jsonl rows, when the run contains them).
Counters and per-file lanes are DIAGNOSTICS, never baselined: they exist to
attribute a KPI move and legitimately shift with every refactor.

Every row carries (sha, dirty, date, host, n, spread): a baseline that can't
say how noisy it was can't support a regression claim, and one from a dirty
tree says so to your face.
"""
import json, statistics as st, sys
from collections import defaultdict

def main():
    path = sys.argv[1]
    note = ""
    if "--note" in sys.argv:
        note = sys.argv[sys.argv.index("--note") + 1]
    run = None
    vals = defaultdict(list)          # (corpus, phase, metric) -> samples
    files = defaultdict(list)         # (corpus, phase, rep) -> per-file build ms
    by_file = defaultdict(lambda: defaultdict(list))  # (corpus, phase) -> module -> ms per rep
    for line in open(path):
        r = json.loads(line)
        if r.get("t") == "run":
            run = r
            continue
        if r.get("t") != "m":
            continue
        k, c, p = r["kind"], r["corpus"], r["phase"]
        if k == "timing" and r["name"] == "check.wall":
            vals[(c, p, "check.wall_ms")].append(r["value"])
        elif k == "rss" and r["name"] == "peak":
            vals[(c, p, "check.peak_rss_mb")].append(r["value"])
        elif k == "file_build":
            files[(c, p, r["rep"])].append(r["value"])
            by_file[(c, p)][r["name"]].append(r["value"])
        elif k == "startup":
            vals[(c, p, f"editor.{r['name']}_ms")].append(r["value"])
        elif k == "verb_ms":
            # Keyed by the SCENARIO STEP (tag), not just the verb: the same
            # verb at two points in a scenario is two different promises
            # (first-definition-after-open vs steady-state), and a median
            # over a bimodal population describes neither.
            step = r.get("tag") or r["name"]
            vals[(c, p, f"editor.verb.{r['name']}.{step}_ms")].append(r["value"])
        elif k == "diag_push_ms":
            vals[(c, p, f"editor.diag_push.{r['name']}_ms")].append(r["value"])
        elif k == "server_rss" and r["name"] == "end":
            vals[(c, p, "editor.rss_end_mb")].append(r["value"])
    assert run, "no run line in input"

    out = []
    def row(corpus, phase, metric, samples):
        if not samples:
            return None
        out.append({
            "date": run["ts"][:10], "sha": run["sha"], "dirty": run.get("dirty"),
            "host": run.get("host"), "corpus": corpus, "phase": phase,
            "metric": metric,
            "value": round(st.median(samples), 2), "n": len(samples),
            "spread": round(max(samples) - min(samples), 2),
            "note": note,
        })
        return out[-1]
    for (c, p, mname), samples in sorted(vals.items()):
        row(c, p, mname, samples)
    # Each distribution stat is taken PER REP, and the baseline is the median
    # of those with the spread across reps — the same run-to-run noise every
    # other KPI carries. Pooled across reps, the spread would be the
    # slowest-minus-fastest FILE, which no regression could ever clear.
    # baseline-check.sql spells the same stats; change them together.
    stats = {"": st.median,
             ".p99": lambda xs: sorted(xs)[min(len(xs) - 1, int(len(xs) * 0.99))],
             ".max": max}
    per_rep = defaultdict(list)       # (corpus, phase, metric) -> one value per rep
    for (c, p, _rep), samples in sorted(files.items()):
        for suffix, f in stats.items():
            per_rep[(c, p, f"file_build_ms{suffix}")].append(f(samples))
    for (c, p, mname), samples in sorted(per_rep.items()):
        r = row(c, p, mname, samples)
        if mname == "file_build_ms.p99":
            # The files that ARE the tail: a p99 move is only actionable
            # when it names what got slower. Ranked by each file's median
            # across reps, so one noisy rep cannot put a file in or out.
            tail = [{"file": f, "ms": round(st.median(v), 2),
                     "spread": round(max(v) - min(v), 2), "reps": len(v)}
                    for f, v in by_file[(c, p)].items()
                    if st.median(v) >= r["value"]]
            r["tail"] = sorted(tail, key=lambda t: -t["ms"])

    with open("bench/baselines.jsonl", "a") as f:
        for r in out:
            f.write(json.dumps(r) + "\n")
    print(f"appended {len(out)} baseline rows from {run['run_id']} (sha {run['sha']}, dirty={run.get('dirty')})")

if __name__ == "__main__":
    main()
