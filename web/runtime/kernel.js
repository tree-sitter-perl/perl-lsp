// The kernel worker: owns the one file system and answers WASI calls for
// every wasm thread. It never blocks, which is what lets it create the
// thread workers while a thread sits in `Atomics.wait` for its answer.
let wasi, memory, module;
let nextTid = 1;
const threads = new Map();

// Stdin is a ring both sides index by running byte counts: header[0] is
// bytes written, header[1] bytes read. Allocated up front because the page
// may write its first request while the module is still compiling.
const STDIN_BYTES = 1 << 20;
const stdin = new SharedArrayBuffer(8 + STDIN_BYTES);
// The kernel cannot block on a full ring, so what does not fit waits here
// until the reader reports progress.
const stdinBacklog = [];

function writeStdin(chunk) {
  if (chunk) stdinBacklog.push(chunk);
  const header = new Int32Array(stdin, 0, 2), ring = new Uint8Array(stdin, 8);
  let written = Atomics.load(header, 0);
  while (stdinBacklog.length) {
    const free = ring.length - (written - Atomics.load(header, 1));
    if (free === 0) break;
    const head = stdinBacklog[0], n = Math.min(free, head.length);
    for (let i = 0; i < n; i++) ring[(written + i) % ring.length] = head[i];
    written += n;
    if (n === head.length) stdinBacklog.shift(); else stdinBacklog[0] = head.subarray(n);
  }
  Atomics.store(header, 0, written);
  Atomics.notify(header, 0);
}

// The module imports its memory, and the import's declared limits are not
// readable through the JS API, so they are read from the import section.
function importedMemoryLimits(buffer) {
  const b = new Uint8Array(buffer);
  let p = 8;
  const leb = () => {
    let v = 0, shift = 0, byte;
    do { byte = b[p++]; v |= (byte & 0x7f) << shift; shift += 7; } while (byte & 0x80);
    return v >>> 0;
  };
  const skipName = () => { const n = leb(); p += n; };
  const IMPORTS = 2, FUNC = 0, TABLE = 1, MEMORY = 2, GLOBAL = 3, TAG = 4;
  while (p < b.length) {
    const id = b[p++], size = leb(), end = p + size;
    if (id !== IMPORTS) { p = end; continue; }
    for (let n = leb(); n > 0; n--) {
      skipName(); skipName();
      const kind = b[p++];
      if (kind === FUNC) leb();
      else if (kind === TAG) { p++; leb(); }
      else if (kind === GLOBAL) p += 2;
      else {
        if (kind === TABLE) p++;
        const flags = leb(), initial = leb(), maximum = flags & 1 ? leb() : undefined;
        if (kind === MEMORY) return { initial, maximum };
      }
    }
    break;
  }
  throw new Error('the module imports no memory; it is not a threaded build');
}

function directory(shim, tree) {
  const entries = new Map();
  for (const [name, value] of Object.entries(tree)) {
    entries.set(name, typeof value === 'string'
      ? new shim.File(new TextEncoder().encode(value))
      : directory(shim, value));
  }
  return new shim.Directory(entries);
}

function spawn(arg, main) {
  const tid = main ? 0 : nextTid++;
  const reply = new SharedArrayBuffer(8);
  const worker = new Worker(new URL('./thread.js', import.meta.url), { type: 'module', name: 'perl-lsp-thread-' + tid });
  threads.set(tid, worker);
  worker.onmessage = ev => {
    const d = ev.data;
    if (d.rpc) {
      let ret;
      try {
        ret = d.rpc === 'thread-spawn' ? spawn(d.args[0], false) : wasi.wasiImport[d.rpc](...d.args);
      } catch (e) {
        postMessage({ log: 'kernel: ' + d.rpc + ' threw ' + e + '\n' });
        ret = 28; // EINVAL
      }
      const cell = new Int32Array(reply);
      Atomics.store(cell, 1, ret | 0);
      Atomics.store(cell, 0, 1);
      Atomics.notify(cell, 0);
    } else if (d.stdinRead) {
      writeStdin();
    } else {
      postMessage(d);
      if (d.exit !== undefined) for (const t of threads.values()) t.terminate();
    }
  };
  worker.onerror = e => postMessage({ trap: 'thread ' + tid + ' failed to start: ' + e.message });
  worker.postMessage({ module, memory, tid, arg, main, reply, stdin });
  return tid;
}

async function start({ wasm, shim: shimUrl, files, args, env }) {
  const shim = await import(shimUrl);
  const bytes = await (await fetch(wasm)).arrayBuffer();
  memory = new WebAssembly.Memory({ ...importedMemoryLimits(bytes), shared: true });
  module = await WebAssembly.compile(bytes);
  const silent = () => shim.ConsoleStdout.lineBuffered(() => {});
  wasi = new shim.WASI(args, env, [
    new shim.OpenFile(new shim.File([])),
    silent(),
    silent(),
    new shim.PreopenDirectory('/work', directory(shim, files).contents),
    new shim.PreopenDirectory('/home', new Map()),
  ], { debug: false });
  // The shim reads guest memory through its instance; every thread shares
  // this one memory, so a stand-in instance is enough.
  wasi.inst = { exports: { memory } };
  spawn(0, true);
}

onmessage = ev => {
  if (ev.data.start) start(ev.data.start).catch(e => postMessage({ fatal: String(e.message || e) }));
  else if (ev.data.stdin) writeStdin(ev.data.stdin);
};
