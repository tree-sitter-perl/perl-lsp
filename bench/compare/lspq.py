#!/usr/bin/env python3
"""Minimal stdio LSP client for cross-tool answer comparison.
usage: lspq.py --cmd '<shell cmd>' --root DIR --probes probes.json --out out.json [--ready-timeout S]
probes.json: {"readiness": {"file":..., "line":..., "character":...},
              "probes": [{"name":..., "file":..., "line":..., "character":..., "verbs": ["definition","references","hover","completion","rename","implementation"], "new_name": "x"}]}
Lines/characters are 0-based (LSP)."""
import argparse, json, os, subprocess, sys, threading, time, queue, pathlib, shlex, resource

class Lsp:
    def __init__(self, cmd, root, errpath):
        self.root = os.path.abspath(root)
        self.err = open(errpath, "wb")
        self.proc = subprocess.Popen(shlex.split(cmd), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err, cwd=self.root)
        self.nid = 1; self.pending = {}; self.lock = threading.Lock()
        self.notifs = []; self.diags = {}
        threading.Thread(target=self._read, daemon=True).start()
    def rss_mb(self):
        try:
            st = open(f"/proc/{self.proc.pid}/status").read()
            g = lambda k: int([l for l in st.splitlines() if l.startswith(k)][0].split()[1])
            # include child processes (phpactor/intelephense may fork)
            return {"rss": g("VmRSS")/1024, "peak": g("VmHWM")/1024}
        except Exception: return {}
    def _send(self, obj):
        b = json.dumps(obj).encode()
        with self.lock:
            self.proc.stdin.write(f"Content-Length: {len(b)}\r\n\r\n".encode() + b); self.proc.stdin.flush()
    def _read(self):
        out = self.proc.stdout
        while True:
            headers = {}; line = out.readline()
            if not line: return
            while line and line.strip():
                k, _, v = line.decode("utf8","replace").partition(":"); headers[k.strip().lower()] = v.strip(); line = out.readline()
            n = int(headers.get("content-length", 0))
            if not n: continue
            msg = json.loads(out.read(n))
            if "id" in msg and "method" in msg:   # server->client request
                m = msg["method"]; res = None
                if m == "workspace/configuration": res = [None]*len(msg.get("params",{}).get("items",[]))
                elif m == "workspace/workspaceFolders": res = [{"uri": pathlib.Path(self.root).as_uri(), "name": "root"}]
                elif m == "window/workDoneProgress/create": res = None
                elif m == "client/registerCapability": res = None
                elif m == "window/showMessageRequest": res = None
                self._send({"jsonrpc":"2.0","id":msg["id"],"result":res})
            elif "id" in msg:
                ev = self.pending.get(msg["id"])
                if ev: ev[1] = msg; ev[0].set()
            else:
                self.notifs.append(msg)
                if msg.get("method") == "textDocument/publishDiagnostics":
                    prm = msg.get("params", {}); self.diags[prm.get("uri")] = prm.get("diagnostics", [])
    def request(self, method, params, timeout=120):
        i = self.nid; self.nid += 1
        ev = [threading.Event(), None]; self.pending[i] = ev
        t0 = time.monotonic(); self._send({"jsonrpc":"2.0","id":i,"method":method,"params":params})
        ok = ev[0].wait(timeout); ms = (time.monotonic()-t0)*1000
        del self.pending[i]
        return (ev[1] if ok else {"error": "timeout"}), ms
    def notify(self, method, params): self._send({"jsonrpc":"2.0","method":method,"params":params})

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--cmd", required=True); ap.add_argument("--root", required=True)
    ap.add_argument("--probes", required=True); ap.add_argument("--out", required=True); ap.add_argument("--ready-timeout", type=float, default=600)
    ap.add_argument("--label", default="tool"); ap.add_argument("--settle", type=float, default=0)
    ap.add_argument("--diag-settle", type=float, default=3, help="seconds to wait for publishDiagnostics after the last probe")
    ap.add_argument("--open", action="append", default=[], help="extra files to open (diagnostics capture)")
    a = ap.parse_args()
    spec = json.load(open(a.probes)); root = os.path.abspath(a.root)
    uri = lambda rel: pathlib.Path(os.path.join(root, rel)).as_uri()
    t_spawn = time.monotonic()
    lsp = Lsp(a.cmd, root, a.out + "." + a.label + ".stderr")
    caps = {"textDocument": {"publishDiagnostics": {}, "completion": {"completionItem": {"snippetSupport": False}}, "hover": {"contentFormat": ["markdown","plaintext"]}}, "workspace": {"configuration": True, "workspaceFolders": True}}
    init, init_ms = lsp.request("initialize", {"processId": os.getpid(), "rootUri": pathlib.Path(root).as_uri(), "rootPath": root, "workspaceFolders": [{"uri": pathlib.Path(root).as_uri(), "name": "root"}], "capabilities": caps, "initializationOptions": {}})
    lsp.notify("initialized", {})
    lsp.notify("workspace/didChangeConfiguration", {"settings": {}})
    opened = set()
    def open_file(rel):
        if rel in opened: return
        text = open(os.path.join(root, rel), encoding="utf8", errors="replace").read()
        lsp.notify("textDocument/didOpen", {"textDocument": {"uri": uri(rel), "languageId": "php", "version": 1, "text": text}}); opened.add(rel)
    r = spec["readiness"]; open_file(r["file"])
    for rel in a.open + spec.get("open", []): open_file(rel)
    t0 = time.monotonic(); ready = None
    while time.monotonic() - t0 < a.ready_timeout:
        msg, _ = lsp.request("textDocument/definition", {"textDocument": {"uri": uri(r["file"])}, "position": {"line": r["line"], "character": r["character"]}}, timeout=60)
        if (msg or {}).get("result"): ready = time.monotonic() - t_spawn; break
        time.sleep(0.5)
    if a.settle: time.sleep(a.settle)
    out = {"label": a.label, "initialize_ms": init_ms, "ready_s": ready, "rss_after_ready": lsp.rss_mb(), "probes": []}
    def norm_loc(x):
        if x is None: return []
        if isinstance(x, dict): x = [x]
        res = []
        for l in x:
            u = l.get("uri") or l.get("targetUri"); rg = l.get("range") or l.get("targetSelectionRange") or l.get("targetRange")
            p = pathlib.Path(u[7:]).as_posix() if u and u.startswith("file://") else u
            try: p = os.path.relpath(p, root)
            except Exception: pass
            res.append({"file": p, "line": rg["start"]["line"] if rg else None, "char": rg["start"]["character"] if rg else None})
        return sorted(res, key=lambda d: (str(d["file"]), d["line"] or 0, d["char"] or 0))
    for p in spec["probes"]:
        open_file(p["file"]); td = {"textDocument": {"uri": uri(p["file"])}, "position": {"line": p["line"], "character": p["character"]}}
        rec = {"name": p["name"], "file": p["file"], "line": p["line"], "character": p["character"]}
        for verb in p.get("verbs", ["definition"]):
            if verb == "definition": m, ms = lsp.request("textDocument/definition", td); rec["definition"] = norm_loc((m or {}).get("result")); rec["definition_ms"] = ms
            elif verb == "implementation": m, ms = lsp.request("textDocument/implementation", td); rec["implementation"] = norm_loc((m or {}).get("result")) if "result" in (m or {}) else {"error": (m or {}).get("error")}; rec["implementation_ms"] = ms
            elif verb == "references":
                m, ms = lsp.request("textDocument/references", dict(td, context={"includeDeclaration": True})); rec["references"] = norm_loc((m or {}).get("result")); rec["references_ms"] = ms
            elif verb == "hover":
                m, ms = lsp.request("textDocument/hover", td); res = (m or {}).get("result"); c = (res or {}).get("contents")
                if isinstance(c, dict): c = c.get("value")
                elif isinstance(c, list): c = "\n".join(x if isinstance(x, str) else x.get("value","") for x in c)
                rec["hover"] = c; rec["hover_ms"] = ms
            elif verb == "completion":
                m, ms = lsp.request("textDocument/completion", td); res = (m or {}).get("result"); items = res.get("items") if isinstance(res, dict) else (res or [])
                rec["completion"] = sorted(i.get("label","") for i in items)[:400]; rec["completion_count"] = len(items); rec["completion_ms"] = ms
            elif verb == "signatureHelp":
                m, ms = lsp.request("textDocument/signatureHelp", td); res = (m or {}).get("result") or {}
                sigs = res.get("signatures") or []
                rec["signatureHelp"] = [{"label": x.get("label"), "doc": (x.get("documentation") if isinstance(x.get("documentation"), str) else (x.get("documentation") or {}).get("value")), "params": [(pp.get("label") if isinstance(pp.get("label"), str) else str(pp.get("label"))) for pp in (x.get("parameters") or [])]} for x in sigs]
                rec["signatureHelp_active"] = res.get("activeParameter"); rec["signatureHelp_ms"] = ms
            elif verb == "codeAction":
                rng = {"start": td["position"], "end": td["position"]}
                m, ms = lsp.request("textDocument/codeAction", {"textDocument": td["textDocument"], "range": rng, "context": {"diagnostics": [d for d in lsp.diags.get(uri(p["file"]), []) if d.get("range",{}).get("start",{}).get("line") == p["line"]]}})
                res = (m or {}).get("result") or []
                rec["codeAction"] = [{"title": x.get("title"), "kind": x.get("kind")} for x in res]; rec["codeAction_ms"] = ms
            elif verb == "typeDefinition":
                m, ms = lsp.request("textDocument/typeDefinition", td); rec["typeDefinition"] = norm_loc((m or {}).get("result")) if "result" in (m or {}) else {"error": str((m or {}).get("error"))[:80]}; rec["typeDefinition_ms"] = ms
            elif verb == "documentSymbol":
                m, ms = lsp.request("textDocument/documentSymbol", {"textDocument": td["textDocument"]}); res = (m or {}).get("result") or []
                def count(items):
                    return sum(1 + count(i.get("children") or []) for i in items)
                rec["documentSymbol"] = count(res); rec["documentSymbol_ms"] = ms
            elif verb == "rename":
                m, ms = lsp.request("textDocument/rename", dict(td, newName=p.get("new_name","zzRenamed"))); res = (m or {}).get("result"); edits = []
                if res:
                    for u, es in (res.get("changes") or {}).items():
                        for e in es: edits.append({"file": os.path.relpath(pathlib.Path(u[7:]).as_posix(), root), "line": e["range"]["start"]["line"], "char": e["range"]["start"]["character"]})
                    for dc in (res.get("documentChanges") or []):
                        u = (dc.get("textDocument") or {}).get("uri")
                        for e in dc.get("edits", []): edits.append({"file": os.path.relpath(pathlib.Path(u[7:]).as_posix(), root), "line": e["range"]["start"]["line"], "char": e["range"]["start"]["character"]})
                rec["rename"] = sorted(edits, key=lambda d: (d["file"], d["line"], d["char"])); rec["rename_error"] = (m or {}).get("error"); rec["rename_ms"] = ms
        out["probes"].append(rec)
    if a.diag_settle: time.sleep(a.diag_settle)
    out["diagnostics"] = {}
    for rel in sorted(opened):
        ds = lsp.diags.get(uri(rel), [])
        out["diagnostics"][rel] = [{"line": d.get("range",{}).get("start",{}).get("line"), "severity": d.get("severity"), "code": d.get("code"), "message": (d.get("message") or "")[:160]} for d in ds]
    out["rss_end"] = lsp.rss_mb()
    try: lsp.request("shutdown", None, timeout=10); lsp.notify("exit", None)
    except Exception: pass
    time.sleep(0.5); 
    try: lsp.proc.kill()
    except Exception: pass
    json.dump(out, open(a.out, "w"), indent=1)
    print(f"{a.label}: ready {ready and round(ready,1)} s, {len(out['probes'])} probes, rss {out['rss_end']}")
main()
