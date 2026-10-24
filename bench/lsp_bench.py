#!/usr/bin/env python3
"""lsp_bench.py — realistic-editing LSP benchmark driver for perl-lsp.

Speaks LSP over stdio to a freshly-spawned server, plays a scenario file,
and records per-request latency, diagnostics-push latency after edits,
RSS checkpoints (VmRSS/VmHWM from /proc), and startup timings.

Usage:
  lsp_bench.py --bin <perl-lsp> --root <project root> --scenario <json> \
               --out <metrics.json> [--label cold|warm]
               [--jsonl <rows.jsonl> --corpus <name> --rep <n> [--run-id <id>]]

  --jsonl appends harness-schema rows ({t:"m", kind, name, value, unit, ...})
  so editor-surface KPIs land in the same DuckDB store as the batch sweep.
  Without --run-id a run line is minted (one per file per run id). Every
  request is its own row — aggregation belongs to reports, never collectors.

The harness protocol around this driver lives in
.claude/skills/edit-bench/SKILL.md; committed scenarios in
bench/scenarios/; the running results ledger in bench/RESULTS.md.

This driver must behave like a REAL editor, because a server can legally
block on client behaviour: it answers server->client requests (a dropped
`window/workDoneProgress/create` reply gates workspace indexing, and the
server then answers every query empty — indistinguishable from a hang), and
it omits `params` rather than sending null (tower-lsp rejects `"params":
null` with -32602, so a param-less `shutdown` never lands). Both were real
bugs here that cost measurable debugging time; keep the fidelity.

Scenario JSON shape (all positions are 0-based LSP coordinates — note the
binary's POSITIONAL CLI mirrors are also 0-based, so validated CLI coords
transfer verbatim):
{
  "readiness": {"file": "rel/path.pm", "line": 10, "character": 5},
     # a DEFINITION probe that answers non-empty only once the workspace
     # index is functionally ready; polled every 250ms after initialize.
  "steps": [
    {"action": "open",       "file": "rel/path.pm"},
    {"action": "hover",      "file": "...", "line": N, "character": C, "name": "..."},
    {"action": "definition", "file": "...", "line": N, "character": C, "name": "..."},
    {"action": "references", "file": "...", "line": N, "character": C, "name": "..."},
    {"action": "completion", "file": "...", "line": N, "character": C, "name": "..."},
    {"action": "documentSymbol", "file": "...", "name": "..."},
    {"action": "insert_line", "file": "...", "line": N, "text": "...", "name": "...",
     "await_diagnostics": true},
     # inserts a line into the driver's buffer copy, sends FULL-text
     # didChange, optionally times until the next publishDiagnostics.
    {"action": "revert",     "file": "...", "name": "...", "await_diagnostics": true},
    {"action": "save",       "file": "..."},
    {"action": "rss",        "name": "checkpoint-name"},
    {"action": "sleep",      "ms": 500}
  ]
}

Every timed step emits {name, action, ms, result_size}. result_size is
the JSON-encoded response length — the honesty signal: a fast answer
that returned little is visible as suspicious (cold indexes can serve
PARTIAL results that look complete).
"""
import argparse, json, os, subprocess, sys, threading, time, queue, pathlib

def now_ms():
    return time.monotonic() * 1000.0

class Lsp:
    def __init__(self, bin_path, root, stderr_path, served_lang):
        self.root = os.path.abspath(root)
        self.stderr_f = open(stderr_path, "wb")
        # One language per server process: a C/C++ scenario runs a cpp server.
        self.proc = subprocess.Popen(
            [bin_path, "--lang", served_lang], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr_f, cwd=self.root)
        self.next_id = 1
        self.pending = {}
        self.notif_q = queue.Queue()
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    def rss(self):
        try:
            st = open(f"/proc/{self.proc.pid}/status").read()
            g = lambda k: int([l for l in st.splitlines() if l.startswith(k)][0].split()[1])
            return {"vmrss_kb": g("VmRSS"), "vmhwm_kb": g("VmHWM")}
        except Exception:
            return {"vmrss_kb": None, "vmhwm_kb": None}

    def _read_loop(self):
        out = self.proc.stdout
        while True:
            headers = {}
            line = out.readline()
            if not line:
                return
            while line and line.strip():
                k, _, v = line.decode("utf8", "replace").partition(":")
                headers[k.strip().lower()] = v.strip()
                line = out.readline()
            n = int(headers.get("content-length", 0))
            if n == 0:
                continue
            body = out.read(n)
            try:
                msg = json.loads(body)
            except Exception:
                continue
            if "id" in msg and ("result" in msg or "error" in msg):
                ent = self.pending.pop(msg["id"], None)
                if ent:
                    ent[0].put((msg, now_ms()))
            elif "id" in msg and "method" in msg:
                # A server->client REQUEST. A real editor answers these; a
                # driver that drops them wedges any server that awaits the
                # reply before doing work — `window/workDoneProgress/create`
                # gates workspace indexing, so ignoring it looks exactly like
                # a hung server that answers every query with an empty result.
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
            else:
                self.notif_q.put((msg, now_ms()))

    def _send(self, obj):
        b = json.dumps(obj).encode("utf8")
        self.proc.stdin.write(f"Content-Length: {len(b)}\r\n\r\n".encode() + b)
        self.proc.stdin.flush()

    def request(self, method, params, timeout=30.0):
        rid = self.next_id
        self.next_id += 1
        q = queue.Queue()
        t0 = now_ms()
        self.pending[rid] = (q, t0)
        self._send(self._envelope({"jsonrpc": "2.0", "id": rid, "method": method}, params))
        try:
            msg, t1 = q.get(timeout=timeout)
        except queue.Empty:
            return None, timeout * 1000.0
        return msg, t1 - t0

    def notify(self, method, params):
        self._send(self._envelope({"jsonrpc": "2.0", "method": method}, params))

    @staticmethod
    def _envelope(msg, params):
        # JSON-RPC 2.0 allows params to be an object or array — never null.
        # tower-lsp rejects `"params": null` with -32602, so a param-less
        # `shutdown` sent that way never reaches the server and the process
        # is left to be killed instead of shutting down cleanly.
        if params is not None:
            msg["params"] = params
        return msg

    def await_diagnostics(self, uri, timeout=30.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                msg, t = self.notif_q.get(timeout=deadline - time.monotonic())
            except queue.Empty:
                return None
            if msg.get("method") == "textDocument/publishDiagnostics" \
               and msg.get("params", {}).get("uri") == uri:
                return t
        return None

    def drain_notifs(self):
        try:
            while True:
                self.notif_q.get_nowait()
        except queue.Empty:
            pass

    def shutdown(self):
        try:
            self.request("shutdown", None, timeout=10)
            self.notify("exit", None)
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()
        self.stderr_f.close()



def emit_jsonl(args, metrics, root):
    """Translate the metrics dict into the tall harness schema.

    Same row shape as bench/measure.sh so bench/load.sql ingests both:
    the editor surface and the batch surface become one queryable store.
    Raw rows only — every request is a sample, phases ride `phase`
    (the cold/warm label), and nothing is aggregated here.
    """
    import platform, subprocess
    corpus = args.corpus or os.path.basename(root.rstrip("/"))
    phase = args.label
    run_id = args.run_id
    rows = []
    if not run_id:
        run_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-lspbench-{os.getpid()}"
        sha, dirty = "unknown", None
        try:
            here = os.path.dirname(os.path.abspath(__file__))
            sha = subprocess.check_output(
                ["git", "-C", here, "rev-parse", "--short=8", "HEAD"],
                stderr=subprocess.DEVNULL).decode().strip()
            dirty = subprocess.call(["git", "-C", here, "diff", "--quiet"]) != 0
        except Exception:
            pass
        try:
            load = os.getloadavg()[0]
        except OSError:
            load = None
        rows.append({"t": "run", "run_id": run_id,
                     "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                     "sha": sha, "dirty": dirty, "source": "lsp_bench",
                     "host": platform.node(), "nproc": os.cpu_count(),
                     "loadavg_at_start": load,
                     "bin": os.path.abspath(args.bin)})

    def m(kind, name, value, unit, tag=None):
        if value is None:
            return
        r = {"t": "m", "run_id": run_id, "corpus": corpus, "rep": args.rep,
             "phase": phase, "kind": kind, "name": name, "value": value, "unit": unit}
        if tag:
            r["tag"] = tag
        rows.append(r)

    m("startup", "initialize", metrics.get("initialize_ms"), "ms")
    m("startup", "ready_from_spawn", metrics.get("ready_ms_from_spawn"), "ms")
    for step in metrics["steps"]:
        act, name = step.get("action"), step.get("name")
        if "ms" in step and act in ("hover", "definition", "references",
                                    "completion", "documentSymbol"):
            m("verb_ms", act, step["ms"], "ms", tag=name)
            m("result_bytes", act, step.get("result_size"), "bytes", tag=name)
        elif act == "open" and "ms" in step:
            m("open_ms", name, step["ms"], "ms")
        if step.get("diagnostics_ms") is not None:
            m("diag_push_ms", name, step["diagnostics_ms"], "ms")
    for cp in metrics["rss"]:
        if cp.get("vmrss_kb"):
            m("server_rss", cp["at"], cp["vmrss_kb"] / 1024.0, "MB")
        if cp.get("vmhwm_kb"):
            m("server_rss_peak", cp["at"], cp["vmhwm_kb"] / 1024.0, "MB")
    with open(args.jsonl, "a") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("--scenario", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--label", default="run")
    ap.add_argument("--jsonl", help="append harness-schema JSONL rows here")
    ap.add_argument("--corpus", help="corpus name for JSONL rows (default: root basename)")
    ap.add_argument("--rep", type=int, default=1)
    ap.add_argument("--run-id", dest="run_id", help="join key; minted (with a run line) if absent")
    args = ap.parse_args()

    scen = json.load(open(args.scenario))
    root = os.path.abspath(args.root)
    uri_of = lambda rel: pathlib.Path(os.path.join(root, rel)).as_uri()

    def lang_of(rel):
        e = rel.rsplit(".", 1)[-1].lower()
        return {"c": "c", "h": "c", "cc": "cpp", "cpp": "cpp", "cxx": "cpp",
                "hpp": "cpp", "hh": "cpp"}.get(e, "perl")

    metrics = {"label": args.label, "root": root, "steps": [], "rss": []}
    t_spawn = now_ms()
    served_lang = "perl" if lang_of(scen["readiness"]["file"]) == "perl" else "cpp"
    lsp = Lsp(os.path.abspath(args.bin), root, args.out + f".{args.label}.stderr", served_lang)

    _, init_ms = lsp.request("initialize", {
        "processId": os.getpid(),
        "rootUri": pathlib.Path(root).as_uri(),
        "capabilities": {"textDocument": {"publishDiagnostics": {}}},
    })
    lsp.notify("initialized", {})
    metrics["initialize_ms"] = init_ms
    metrics["rss"].append({"at": "post-initialize", **lsp.rss()})

    r = scen["readiness"]
    r_uri = uri_of(r["file"])
    r_text = open(os.path.join(root, r["file"]), encoding="utf8", errors="replace").read()
    lsp.notify("textDocument/didOpen", {"textDocument": {
        "uri": r_uri, "languageId": lang_of(r["file"]), "version": 1, "text": r_text}})
    ready_ms = None
    t0 = now_ms()
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        msg, _ = lsp.request("textDocument/definition", {
            "textDocument": {"uri": r_uri},
            "position": {"line": r["line"], "character": r["character"]}}, timeout=60)
        res = (msg or {}).get("result")
        if res:
            ready_ms = now_ms() - t0
            break
        time.sleep(0.25)
    metrics["ready_ms_from_spawn"] = (now_ms() - t_spawn) if ready_ms is not None else None
    metrics["rss"].append({"at": "post-ready", **lsp.rss()})

    buffers = {r["file"]: (r_text, 1)}

    for step in scen["steps"]:
        act = step["action"]
        name = step.get("name", act)
        if act == "open":
            rel = step["file"]
            text = open(os.path.join(root, rel), encoding="utf8", errors="replace").read()
            buffers[rel] = (text, 1)
            t0 = now_ms()
            lsp.notify("textDocument/didOpen", {"textDocument": {
                "uri": uri_of(rel), "languageId": lang_of(rel), "version": 1, "text": text}})
            msg, ms = lsp.request("textDocument/documentSymbol",
                                  {"textDocument": {"uri": uri_of(rel)}}, timeout=120)
            metrics["steps"].append({"name": f"open:{rel}", "action": "open",
                                     "ms": now_ms() - t0,
                                     "result_size": len(json.dumps((msg or {}).get("result")))})
        elif act in ("hover", "definition", "references", "completion"):
            method = {"hover": "textDocument/hover",
                      "definition": "textDocument/definition",
                      "references": "textDocument/references",
                      "completion": "textDocument/completion"}[act]
            params = {"textDocument": {"uri": uri_of(step["file"])},
                      "position": {"line": step["line"], "character": step["character"]}}
            if act == "references":
                params["context"] = {"includeDeclaration": True}
            msg, ms = lsp.request(method, params, timeout=float(os.environ.get("BENCH_REQ_TIMEOUT_SECONDS","120")))
            metrics["steps"].append({"name": name, "action": act, "ms": ms,
                                     "result_size": len(json.dumps((msg or {}).get("result")))})
        elif act == "documentSymbol":
            msg, ms = lsp.request("textDocument/documentSymbol",
                                  {"textDocument": {"uri": uri_of(step["file"])}}, timeout=120)
            metrics["steps"].append({"name": name, "action": act, "ms": ms,
                                     "result_size": len(json.dumps((msg or {}).get("result")))})
        elif act in ("insert_line", "revert"):
            rel = step["file"]
            text, ver = buffers[rel]
            if act == "insert_line":
                lines = text.split("\n")
                lines.insert(step["line"], step["text"])
                new_text = "\n".join(lines)
            else:
                new_text = open(os.path.join(root, rel), encoding="utf8", errors="replace").read()
            ver += 1
            buffers[rel] = (new_text, ver)
            lsp.drain_notifs()
            t0 = now_ms()
            lsp.notify("textDocument/didChange", {
                "textDocument": {"uri": uri_of(rel), "version": ver},
                "contentChanges": [{"text": new_text}]})
            entry = {"name": name, "action": act}
            if step.get("await_diagnostics"):
                t_d = lsp.await_diagnostics(uri_of(rel), timeout=60)
                entry["diagnostics_ms"] = (t_d - t0) if t_d else None
            metrics["steps"].append(entry)
        elif act == "save":
            lsp.notify("textDocument/didSave",
                       {"textDocument": {"uri": uri_of(step["file"])}})
            metrics["steps"].append({"name": name, "action": act})
        elif act == "rss":
            metrics["rss"].append({"at": name, **lsp.rss()})
        elif act == "sleep":
            time.sleep(step["ms"] / 1000.0)

    metrics["rss"].append({"at": "end", **lsp.rss()})
    lsp.shutdown()
    json.dump(metrics, open(args.out, "w"), indent=1)
    if args.jsonl:
        emit_jsonl(args, metrics, root)
    print(f"[{args.label}] ready={metrics['ready_ms_from_spawn']:.0f}ms "
          f"peak_rss={metrics['rss'][-1]['vmhwm_kb']}kB steps={len(metrics['steps'])}")

if __name__ == "__main__":
    main()
