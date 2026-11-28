#!/usr/bin/env python3
"""The demo page's scenes, asked of the native binary over raw LSP.

    python3 e2e/demo_scenes.py <perl-lsp> <perl5-dir>

`web/demo/scenes.json` is the one list of scenes and of what each must
answer. The page test (`web/test/demo.mjs`) plays it in a browser against the
wasm build; this plays it against the native build, so a scene red in both is
the engine and a scene red only there is the browser runtime.

<perl5-dir> is where the demo project's cpanfile is installed
(`cpm install -L deps --cpanfile web/demo/project/cpanfile` gives
`deps/lib/perl5`). The project is copied to a scratch root with the
dependencies under `local/lib/perl5`, where the page puts them: the plugin
scenes edit a project file, and a jump into a dependency is checked by its
path under the root.

Each scene is retried until it answers or its deadline passes, as on the
page: cross-file answers arrive once the resolver has landed the other
files.
"""
import json, os, shutil, subprocess, sys, tempfile, threading, time, queue

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.join(HERE, "..", "web", "demo", "project")
SCENES = os.path.join(HERE, "..", "web", "demo", "scenes.json")
DEADLINE = 30
PERL = (".pm", ".pl", ".t")

HIERARCHY = {
    "incomingCalls": ("textDocument/prepareCallHierarchy", "callHierarchy/incomingCalls", "from"),
    "outgoingCalls": ("textDocument/prepareCallHierarchy", "callHierarchy/outgoingCalls", "to"),
    "subtypes": ("textDocument/prepareTypeHierarchy", "typeHierarchy/subtypes", None),
}


class Server:
    def __init__(self, binary, root, init_options):
        self.root = root
        self.proc = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, cwd=root)
        self.pending, self.next_id = {}, 0
        self.diagnostics = {}
        self.wlock = threading.Lock()
        threading.Thread(target=self._read_loop, daemon=True).start()
        caps = {"textDocument": {
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "definition": {"linkSupport": True},
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
            "codeAction": {"codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix"]}}},
            "rename": {"prepareSupport": True},
        }}
        if self.ask("initialize", {"processId": os.getpid(), "rootUri": self.uri(""),
                                   "initializationOptions": init_options,
                                   "capabilities": caps}) is None:
            raise RuntimeError("no initialize response")
        self.notify("initialized", {})
        for rel in project_files(root):
            if rel.endswith(PERL):
                self.notify("textDocument/didOpen", {"textDocument": {
                    "uri": self.uri(rel), "languageId": "perl", "version": 1,
                    "text": read(root, rel)}})

    def uri(self, rel):
        return "file://" + os.path.join(self.root, rel)

    def rel(self, uri):
        return os.path.relpath(uri[len("file://"):], self.root)

    def _send(self, m):
        b = json.dumps(m).encode()
        with self.wlock:
            self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
            self.proc.stdin.flush()

    def _read_loop(self):
        try:
            while True:
                length = 0
                line = self.proc.stdout.readline()
                if not line:
                    return
                while line.strip():
                    k, _, v = line.strip().partition(b":")
                    if k.lower() == b"content-length":
                        length = int(v)
                    line = self.proc.stdout.readline()
                m = json.loads(self.proc.stdout.read(length))
                if "method" in m and "id" in m:
                    self._send({"jsonrpc": "2.0", "id": m["id"], "result": None})
                elif m.get("method") == "textDocument/publishDiagnostics":
                    self.diagnostics[m["params"]["uri"]] = m["params"]["diagnostics"]
                elif m.get("id") in self.pending:
                    self.pending[m["id"]].put(m)
        except Exception:
            pass

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def ask(self, method, params, timeout=20):
        """The result, or None on an error, a null answer or a timeout: a
        scene that gets none of the three is simply not answered yet."""
        self.next_id += 1
        rid, q = self.next_id, queue.Queue()
        self.pending[rid] = q
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        try:
            return q.get(timeout=timeout).get("result")
        except queue.Empty:
            return None

    def stop(self):
        # `shutdown` takes no params key: tower-lsp rejects `"params": null`.
        self.next_id += 1
        q = queue.Queue()
        self.pending[self.next_id] = q
        self._send({"jsonrpc": "2.0", "id": self.next_id, "method": "shutdown"})
        try:
            q.get(timeout=10)
        except queue.Empty:
            pass
        try:
            self._send({"jsonrpc": "2.0", "method": "exit"})
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


def project_files(root):
    for base, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d != "local"]
        for f in files:
            yield os.path.relpath(os.path.join(base, f), root)


def read(root, rel):
    with open(os.path.join(root, rel), encoding="utf-8") as f:
        return f.read()


def position(text, scene):
    """Monaco's `findMatches(on)[nth]`, then `offset` columns in (default
    one, so the cursor is inside the token whatever its sigil)."""
    at = -1
    for _ in range(scene.get("nth", 0) + 1):
        at = text.find(scene["on"], at + 1)
        if at < 0:
            return None
    at += scene.get("offset", 1)
    line = text.count("\n", 0, at)
    # Columns are UTF-16 units in LSP's default encoding.
    col = len(text[text.rfind("\n", 0, at) + 1:at].encode("utf-16-le")) // 2
    return {"line": line, "character": col}


def flatten(value):
    """Every string in an answer, joined: a hover's markdown, a completion's
    labels, a location's uri. `text` expectations are substring checks, as
    they are against the rendered page."""
    if isinstance(value, str):
        return value + "\n"
    if isinstance(value, dict):
        return "".join(flatten(v) for v in value.values())
    if isinstance(value, list):
        return "".join(flatten(v) for v in value)
    return ""


def covers(rng, pos):
    start, end = (rng["start"]["line"], rng["start"]["character"]), (rng["end"]["line"], rng["end"]["character"])
    return start <= (pos["line"], pos["character"]) <= end


def answer(server, scene, pos):
    """What the editor would show for the scene's action, as one text."""
    doc = {"uri": server.uri(scene["file"])}
    at = {"textDocument": doc, "position": pos}
    action = scene["action"]
    diagnostics = server.diagnostics.get(doc["uri"], [])
    if action == "hover":
        # An editor's hover card carries the diagnostics under the cursor.
        here = [d["message"] for d in diagnostics if covers(d["range"], pos)]
        # ...and renders the markdown: a code span shows without its backticks.
        return flatten(server.ask("textDocument/hover", at)).replace("`", "") + flatten(here)
    if action in ("definition", "references", "implementation"):
        params = dict(at, context={"includeDeclaration": True}) if action == "references" else at
        return server.ask("textDocument/" + action, params)
    if action == "complete":
        return flatten(server.ask("textDocument/completion", at))
    if action == "signature":
        return flatten(server.ask("textDocument/signatureHelp", at))
    if action == "outline":
        return flatten(server.ask("textDocument/documentSymbol", {"textDocument": doc}))
    if action == "rename":
        return server.ask("textDocument/prepareRename", at)
    if action == "diagnostic":
        # "Next problem" from the cursor.
        later = [d["message"] for d in diagnostics if d["range"]["start"]["line"] >= pos["line"]]
        return flatten(later)
    if action == "codeAction":
        here = [d for d in diagnostics if d["range"]["start"]["line"] == pos["line"]]
        return flatten(server.ask("textDocument/codeAction", {
            "textDocument": doc, "range": {"start": pos, "end": pos},
            "context": {"diagnostics": here, "only": ["quickfix"]}}))
    if action == "inlay":
        lines = read(server.root, scene["file"]).count("\n")
        return flatten(server.ask("textDocument/inlayHint", {
            "textDocument": doc,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": lines, "character": 0}}}))
    if action in HIERARCHY:
        prepare, method, pick = HIERARCHY[action]
        roots = server.ask(prepare, at)
        found = server.ask(method, {"item": roots[0]}) if roots else None
        return flatten([r[pick] if pick else r for r in found or []])
    raise ValueError(f"scene {scene['id']}: no native request for action {action!r}")


def locations(found):
    for loc in found if isinstance(found, list) else [found] if found else []:
        yield loc.get("uri") or loc["targetUri"], loc.get("range") or loc["targetSelectionRange"]


def holds(server, scene, pos):
    expect = scene["expect"]
    if "lands" in expect:
        file, line = expect["lands"]
        if "action" not in scene:
            # The scene only opens the file at `on`.
            return scene["file"] == file and line in read(server.root, file)
        for uri, rng in locations(answer(server, scene, pos)):
            rel = server.rel(uri)
            if rel == file and line in read(server.root, rel).split("\n")[rng["start"]["line"]]:
                return True
        return False
    found = answer(server, scene, pos)
    if scene["action"] == "rename":
        return found is not None
    if not isinstance(found, str):
        found = flatten([uri for uri, _ in locations(found)])
    return expect["text"] in found


def main():
    binary, perl5 = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    if not os.path.isfile(os.path.join(perl5, "Mojolicious.pm")):
        sys.exit(f"{perl5} does not hold the demo's dependencies")
    with open(SCENES, encoding="utf-8") as f:
        demo = json.load(f)
    scenes = [s for group in demo["groups"] for s in group["scenes"]]
    start = lambda root: Server(binary, root, demo["initializationOptions"])
    passed, failed = 0, []
    with tempfile.TemporaryDirectory(prefix="perl-lsp-demo-") as tmp:
        # macOS hands out a symlinked temp dir; the server answers with
        # canonical paths.
        root = os.path.join(os.path.realpath(tmp), "work")
        shutil.copytree(PROJECT, root)
        shutil.copytree(perl5, os.path.join(root, "local", "lib", "perl5"))
        env = dict(os.environ, XDG_CACHE_HOME=os.path.join(os.path.realpath(tmp), "cache"))
        os.environ.update(env)
        server = start(root)
        for scene in scenes:
            ok, detail = False, "no answer held within the deadline"
            try:
                if "report" in scene:
                    args = [root if a == "/work" else a for a in scene["report"]]
                    out = subprocess.run([binary, *args], capture_output=True, text=True, timeout=300, cwd=root)
                    ok = out.returncode == 0 and scene["expect"]["report"] in out.stdout
                    detail = f"exit {out.returncode}, report text missing"
                elif "replace" in scene:
                    # The page restarts the server when a plugin is edited;
                    # the scene after this one only answers on the new one.
                    text = read(root, scene["file"])
                    if scene["replace"] not in text:
                        text = text.replace(scene["on"], scene["replace"], 1)
                    with open(os.path.join(root, scene["file"]), "w", encoding="utf-8") as f:
                        f.write(text)
                    server.stop()
                    server = start(root)
                    ok = scene["replace"] in read(root, scene["file"])
                else:
                    pos = position(read(root, scene["file"]), scene)
                    if pos is None:
                        detail = f"{scene['on']!r} is not in {scene['file']}"
                    else:
                        deadline = time.monotonic() + DEADLINE
                        while not (ok := holds(server, scene, pos)) and time.monotonic() < deadline:
                            time.sleep(0.25)
            except Exception as e:  # a malformed answer is a failed scene, not a dead run
                detail = repr(e)
            if ok:
                passed += 1
                print(f"  ✓ {scene['id']}")
            else:
                failed.append(scene["id"])
                print(f"  ✗ {scene['id']}: {detail}")
        server.stop()
    print(f"\n{passed} passed, {len(failed)} failed")
    sys.exit(1 if failed else 0)


main()
