// The page's side of the server: starts the kernel worker and speaks LSP's
// base protocol over the server's stdin and stdout.
//
// Needs a cross-origin isolated page (shared memory between workers).
function launch({ wasm, shim, files, args, env }) {
  return {
    wasm: new URL(wasm, location.href).href,
    shim: new URL(shim, location.href).href,
    files,
    args: ['perl-lsp', ...args],
    env: ['HOME=/home', 'XDG_CACHE_HOME=/home/.cache', ...env],
  };
}

// Runs one CLI invocation (`perl-lsp --heatmap /work`, ...) to completion in
// a process of its own: its own kernel, file system and memory, so it shares
// nothing with a running server.
export function runCommand({ wasm, shim, files, args, env = [] }) {
  if (!crossOriginIsolated) throw new Error('perl-lsp needs a cross-origin isolated page for its threads');
  const kernel = new Worker(new URL('./kernel.js', import.meta.url), { type: 'module', name: 'perl-lsp-command' });
  const out = [], err = [];
  return new Promise((resolve, reject) => {
    const text = chunks => new TextDecoder().decode(concat(chunks));
    const done = code => { kernel.terminate(); resolve({ code, stdout: text(out), stderr: text(err) }); };
    kernel.onmessage = ev => {
      const d = ev.data;
      if (d.out === 1) out.push(d.data);
      else if (d.out === 2) err.push(d.data);
      else if (d.log) err.push(new TextEncoder().encode(d.log));
      else if (d.trap) { kernel.terminate(); reject(new Error('the command trapped: ' + d.trap.split('\n')[0])); }
      else if (d.fatal) { kernel.terminate(); reject(new Error('the command could not start: ' + d.fatal)); }
      else if (d.exit !== undefined) done(d.exit);
    };
    kernel.onerror = e => reject(new Error('the kernel worker failed: ' + (e.message || 'unknown error')));
    kernel.postMessage({ start: launch({ wasm, shim, files, args, env }) });
  });
}

function concat(chunks) {
  const joined = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let offset = 0;
  for (const c of chunks) { joined.set(c, offset); offset += c.length; }
  return joined;
}

export function startServer({ wasm, shim, files, env = [] }) {
  if (!crossOriginIsolated) throw new Error('perl-lsp needs a cross-origin isolated page for its threads');
  const kernel = new Worker(new URL('./kernel.js', import.meta.url), { type: 'module', name: 'perl-lsp-kernel' });
  const encoder = new TextEncoder(), decoder = new TextDecoder();
  const pending = new Map(), listeners = [], logs = [], logListeners = [];
  let nextId = 0, buffered = new Uint8Array(0);
  let stop;
  const exited = new Promise(resolve => { stop = resolve; });

  function dispatch(message) {
    if (message.id !== undefined && message.method === undefined) {
      const done = pending.get(message.id);
      pending.delete(message.id);
      if (done) message.error ? done.reject(Object.assign(new Error(message.error.message), message.error)) : done.resolve(message.result);
    } else {
      // A request from the server is answered `null`: the page is a
      // client with no capabilities to register.
      if (message.id !== undefined) send({ jsonrpc: '2.0', id: message.id, result: null });
      for (const l of listeners) l(message.method, message.params);
    }
  }
  function feed(chunk) {
    const joined = new Uint8Array(buffered.length + chunk.length);
    joined.set(buffered); joined.set(chunk, buffered.length);
    buffered = joined;
    for (;;) {
      const head = decoder.decode(buffered.subarray(0, Math.min(buffered.length, 256)));
      const m = /^Content-Length: (\d+)\r\n(?:[^\r]+\r\n)*\r\n/.exec(head);
      if (!m) return;
      const start = m[0].length, end = start + Number(m[1]);
      if (buffered.length < end) return;
      const body = decoder.decode(buffered.subarray(start, end));
      buffered = buffered.slice(end);
      dispatch(JSON.parse(body));
    }
  }
  function send(message) {
    const body = encoder.encode(JSON.stringify(message));
    const head = encoder.encode(`Content-Length: ${body.length}\r\n\r\n`);
    const frame = new Uint8Array(head.length + body.length);
    frame.set(head); frame.set(body, head.length);
    kernel.postMessage({ stdin: frame });
  }
  function log(text) {
    logs.push(text);
    for (const l of logListeners) l(text);
  }
  function fail(reason) {
    for (const p of pending.values()) p.reject(new Error(reason));
    pending.clear();
    stop(reason);
  }

  kernel.onmessage = ev => {
    const d = ev.data;
    if (d.out === 1) feed(d.data);
    else if (d.out === 2) log(decoder.decode(d.data));
    else if (d.log) log(d.log);
    else if (d.trap) { logs.push(d.trap); fail('the server trapped: ' + d.trap.split('\n')[0]); }
    else if (d.fatal) fail('the server could not start: ' + d.fatal);
    else if (d.exit !== undefined) fail('the server exited with code ' + d.exit);
  };
  kernel.onerror = e => fail('the kernel worker failed: ' + (e.message || 'unknown error'));
  kernel.postMessage({ start: launch({ wasm, shim, files, args: [], env }) });

  return {
    request(method, params) {
      const id = ++nextId;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        send({ jsonrpc: '2.0', id, method, params });
      });
    },
    notify(method, params) { send({ jsonrpc: '2.0', method, params }); },
    onNotification(listener) { listeners.push(listener); },
    logs,
    onLog(listener) { logListeners.push(listener); },
    exited,
    terminate() { kernel.terminate(); fail('terminated'); },
  };
}
