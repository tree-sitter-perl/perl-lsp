// One wasm thread. File-system calls go to the kernel worker so every
// thread sees one file table; the thread blocks on shared memory until the
// kernel answers. Calls that touch no shared host state are served here.
let reply, memory, stdin;

function rpc(name, args) {
  Atomics.store(reply, 0, 0);
  postMessage({ rpc: name, args });
  Atomics.wait(reply, 0, 0);
  return Atomics.load(reply, 1);
}

const view = () => new DataView(memory.buffer);
const bytes = () => new Uint8Array(memory.buffer);
const CLOCK_MONOTONIC = 1;
const nowNs = clock => clock === CLOCK_MONOTONIC
  ? BigInt(Math.round(performance.now() * 1e6))
  : BigInt(Date.now()) * 1000000n;

function iovecs(iovs, count) {
  const v = view(), out = [];
  for (let i = 0; i < count; i++) out.push([v.getUint32(iovs + i * 8, true), v.getUint32(iovs + i * 8 + 4, true)]);
  return out;
}

const local = {
  clock_time_get(clock, _precision, out) {
    view().setBigUint64(out, nowNs(clock), true);
    return 0;
  },
  sched_yield() { return 0; },
  random_get(buf, len) {
    // getRandomValues refuses a view over shared memory and more than 64 KiB.
    const tmp = new Uint8Array(len);
    for (let i = 0; i < len; i += 65536) crypto.getRandomValues(tmp.subarray(i, Math.min(len, i + 65536)));
    bytes().set(tmp, buf);
    return 0;
  },
  // Only clock subscriptions arrive here (thread sleeps and timed parks).
  poll_oneoff(subs, events, count, neventsOut) {
    const v = view();
    let waitMs = Infinity;
    for (let i = 0; i < count; i++) {
      const sub = subs + i * 48;
      if (v.getUint8(sub + 8) !== 0) continue;
      const clock = v.getUint32(sub + 16, true), timeout = v.getBigUint64(sub + 24, true);
      const absolute = v.getUint16(sub + 40, true) & 1;
      waitMs = Math.min(waitMs, Number(absolute ? timeout - nowNs(clock) : timeout) / 1e6);
    }
    if (waitMs > 0 && waitMs !== Infinity) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, waitMs);
    v.setBigUint64(events, v.getBigUint64(subs, true), true);
    v.setUint16(events + 8, 0, true);
    v.setUint8(events + 10, 0);
    v.setUint32(neventsOut, 1, true);
    return 0;
  },
  proc_exit(code) {
    postMessage({ exit: code });
    throw new ProcExit();
  },
  fd_read(fd, iovs, count, nreadOut) {
    if (fd !== 0) return rpc('fd_read', [fd, iovs, count, nreadOut]);
    const header = new Int32Array(stdin, 0, 2), ring = new Uint8Array(stdin, 8);
    let read = Atomics.load(header, 1);
    while (Atomics.load(header, 0) === read) Atomics.wait(header, 0, read);
    const written = Atomics.load(header, 0);
    let got = 0;
    for (const [ptr, len] of iovecs(iovs, count)) {
      const n = Math.min(len, written - read);
      const guest = bytes();
      for (let i = 0; i < n; i++) guest[ptr + i] = ring[(read + i) % ring.length];
      read += n; got += n;
    }
    Atomics.store(header, 1, read);
    postMessage({ stdinRead: true });
    view().setUint32(nreadOut, got, true);
    return 0;
  },
  fd_write(fd, iovs, count, nwrittenOut) {
    if (fd !== 1 && fd !== 2) return rpc('fd_write', [fd, iovs, count, nwrittenOut]);
    const parts = iovecs(iovs, count).map(([ptr, len]) => bytes().slice(ptr, ptr + len));
    const total = parts.reduce((n, p) => n + p.length, 0);
    const data = new Uint8Array(total);
    let offset = 0;
    for (const p of parts) { data.set(p, offset); offset += p.length; }
    postMessage({ out: fd, data });
    view().setUint32(nwrittenOut, total, true);
    return 0;
  },
};

class ProcExit extends Error {}

onmessage = ev => {
  const d = ev.data;
  reply = new Int32Array(d.reply); memory = d.memory; stdin = d.stdin;
  const wasiImports = {};
  for (const imp of WebAssembly.Module.imports(d.module)) {
    if (imp.module !== 'wasi_snapshot_preview1') continue;
    wasiImports[imp.name] = local[imp.name] ?? ((...args) => rpc(imp.name, args));
  }
  const instance = new WebAssembly.Instance(d.module, {
    env: { memory },
    wasi_snapshot_preview1: wasiImports,
    wasi: { 'thread-spawn': arg => rpc('thread-spawn', [arg]) },
  });
  try {
    if (d.main) {
      instance.exports._start();
      postMessage({ exit: 0 });
    } else {
      instance.exports.wasi_thread_start(d.tid, d.arg);
    }
  } catch (e) {
    if (!(e instanceof ProcExit)) postMessage({ trap: 'thread ' + d.tid + ': ' + (e.stack || e) });
  }
};
