#!/usr/bin/env python3
"""Regression net for the not-ready-vs-no-result distinction (raw LSP).

While a document's initial build runs, pull verbs must never answer a
fast `null` — LSP null means "nothing at this position", a FINAL answer
the client caches and never re-requests, which is how a giant file made
the editor look dead until a refresh nudge landed (scaling-limits §6).
The contract under test:

  1. default: a verb fired immediately after didOpen WAITS for the
     in-flight build (the 5 s Interactive floor in `await_open_ready`)
     and answers with content — never null, never an error.
  2. coldWaitMs: 0 (wait opted out): the same verb answers
     ContentModified (-32801) — "in flux, retry" — never null.

  3. the session ends at `exit` or at stdin EOF, whichever comes first,
     even while a handler waits on the client: a server->client request
     the client never answers must not keep the process alive.

Branch 2 is forced via the opt-out rather than a monster fixture: in
default mode -32801 only fires when a build outruns the 5 s floor,
which post the fold fixes means roughly >20k-line files — nothing a
checked-in fixture should be.

This test deliberately has NO readiness gate before the probed request:
firing before the server is ready IS the scenario. The only wait is for
the `initialize` RESPONSE — tower-lsp silently drops notifications sent
before the handshake completes, so a client that fires
initialize+initialized+didOpen back-to-back wedges itself, not the
server. Every request carries a hard deadline so a hang is a loud FAIL,
not a slow pass. `shutdown` is sent WITHOUT params — tower-lsp rejects
`"params": null` with -32602 and the clean exit under assertion never
happens.

Speaks raw LSP over stdio (not nvim) because the assertion is about
message ORDERING relative to the build, which a real client's own
readiness logic would mask. Emits the harness summary line
(`N passed, M failed`) so run.sh sums it like any suite.
"""
import json, os, subprocess, sys, tempfile, threading, time, queue

# Absolute: the server runs with cwd = the fixture tempdir.
BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "./target/release/perl-lsp")

passed = 0
failed = 0


def report(name, ok, detail=""):
    global passed, failed
    if ok:
        passed += 1
        print(f"  ✓ {name}")
    else:
        failed += 1
        print(f"  ✗ {name}: {detail}")


def make_fixture(root):
    """A file big enough that its build reliably outlasts the ~ms gap
    between didOpen and the probed request (the race the test exists to
    pin). ~6k small subs with call bindings ≈ tens of thousands of
    lines, sub-second-to-seconds build across boxes — either way both
    branches hold; only a build faster than the request ROUND-TRIP
    (~1 ms) could flake branch 2, and no box builds 30k lines in 1 ms."""
    path = os.path.join(root, "Big.pm")
    with open(path, "w") as f:
        f.write("package Big;\nuse strict;\nuse warnings;\n")
        for i in range(6000):
            f.write(
                f"sub f{i} {{ my ($self) = @_; my $x = helper{i % 50}();"
                f" return $x + {i}; }}\n"
            )
        for i in range(50):
            f.write(f"sub helper{i} {{ return {i}; }}\n")
        f.write("1;\n")
    return path


class Client:
    def __init__(self, root, init_options=None, answers=True):
        self.answers = answers
        self.proc = subprocess.Popen(
            [BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, cwd=root)
        self.pending = {}
        self.wlock = threading.Lock()
        threading.Thread(target=self._read_loop, daemon=True).start()
        params = {"processId": os.getpid(), "rootUri": "file://" + root,
                  "capabilities": {}}
        if init_options is not None:
            params["initializationOptions"] = init_options
        msg = self.request(1, "initialize", params, timeout=30)
        if msg is None:
            raise RuntimeError("no initialize response in 30s")
        self.notify("initialized", {})

    def _send(self, m):
        b = json.dumps(m).encode()
        with self.wlock:
            self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
            self.proc.stdin.flush()

    def _read_loop(self):
        try:
            while True:
                headers = {}
                line = self.proc.stdout.readline()
                if not line:
                    return
                while line.strip():
                    k, _, v = line.strip().partition(b":")
                    headers[k.lower()] = v.strip()
                    line = self.proc.stdout.readline()
                body = self.proc.stdout.read(int(headers.get(b"content-length", b"0")))
                try:
                    m = json.loads(body)
                except Exception:
                    continue
                if "method" in m and "id" in m:
                    # server->client request: answer, or the server stalls
                    if self.answers:
                        self._send({"jsonrpc": "2.0", "id": m["id"], "result": None})
                elif "id" in m and m["id"] in self.pending:
                    self.pending[m["id"]].put(m)
        except Exception:
            pass

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, rid, method, params, timeout=60):
        q = queue.Queue()
        self.pending[rid] = q
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        try:
            return q.get(timeout=timeout)
        except queue.Empty:
            return None

    def open(self, path):
        with open(path, errors="replace") as f:
            text = f.read()
        self.notify("textDocument/didOpen", {"textDocument": {
            "uri": "file://" + path, "languageId": "perl",
            "version": 1, "text": text}})

    def shutdown_clean(self, close_stdin=True):
        """shutdown (NO params key), exit, bounded wait for exit 0. `exit`
        alone must end the process, and so must stdin EOF (see
        `stdio_bridge::SessionEnd`)."""
        r = self.request(99, "shutdown", None, timeout=15)
        self._send({"jsonrpc": "2.0", "method": "exit"})
        if close_stdin:
            self.proc.stdin.close()
        try:
            return r is not None and "error" not in r and self.proc.wait(timeout=15) == 0
        except subprocess.TimeoutExpired:
            self.proc.kill()
            return False


def main():
    with tempfile.TemporaryDirectory(prefix="perl-lsp-not-ready-") as root:
        fixture = make_fixture(root)
        doc = {"textDocument": {"uri": "file://" + fixture}}

        # Branch 1: default — the verb waits out the build and answers.
        c = Client(root)
        c.open(fixture)
        t0 = time.monotonic()
        m = c.request(10, "textDocument/documentSymbol", doc, timeout=60)
        took = time.monotonic() - t0
        if m is None:
            report("verb after didOpen answers", False, "timed out (hang, not a slow pass)")
        elif "error" in m:
            report("verb after didOpen answers", False, f"error {m['error']}")
        elif not m.get("result"):
            report("verb after didOpen answers", False,
                   f"null/empty after {took:.2f}s — the cached-lie regression")
        else:
            report("verb after didOpen answers", True)
            # Nested DocumentSymbol: the subs sit under the package node.
            syms = sum(len(top.get("children", [])) or 1 for top in m["result"])
            report("answer carries the outline", syms > 1000,
                   f"only {syms} symbols for a 6k-sub file")
        report("clean shutdown (branch 1)", c.shutdown_clean(), "nonzero/timeout exit")

        # Branch 2: wait opted out — not-ready is an ERROR, never null.
        c = Client(root, init_options={"coldWaitMs": 0})
        c.open(fixture)
        m = c.request(10, "textDocument/documentSymbol", doc, timeout=60)
        if m is None:
            report("opted-out verb answers ContentModified", False, "timed out")
        elif "error" in m:
            report("opted-out verb answers ContentModified",
                   m["error"].get("code") == -32801,
                   f"wrong error: {m['error']}")
        else:
            # A result here means the build won a ~1 ms race on this box —
            # that's not the regression under test (null is), but it makes
            # the branch unexercised, which should be visible, not green.
            report("opted-out verb answers ContentModified",
                   m.get("result") is not None,
                   "null — the cached-lie regression")
        report("clean shutdown (branch 2)", c.shutdown_clean(), "nonzero/timeout exit")

        # Branch 3: a server->client request the client never answers
        # (`client/registerCapability`, sent from `initialized`) must not keep
        # the process alive past `exit`. An editor that quits right after
        # `exit` is this client: its reply is never written.
        c = Client(root, init_options={"coldWaitMs": 0}, answers=False)
        c.open(fixture)
        report("exit with a server request unanswered",
               c.shutdown_clean(close_stdin=False), "nonzero/timeout exit")

        # Branch 4: the same client dying without `exit`: stdin EOF alone.
        c = Client(root, init_options={"coldWaitMs": 0}, answers=False)
        c.open(fixture)
        c.proc.stdin.close()
        try:
            report("stdin EOF with a server request unanswered",
                   c.proc.wait(timeout=15) == 0, "nonzero exit")
        except subprocess.TimeoutExpired:
            c.proc.kill()
            report("stdin EOF with a server request unanswered", False, "timeout")

    print(f"\n{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


main()
