// A static server for the runtime, the shim and one wasm module, sending the
// two headers that make a page cross-origin isolated.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const web = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const types = { '.js': 'text/javascript', '.html': 'text/html', '.wasm': 'application/wasm' };

export function serve(wasmPath) {
  const routes = [
    ['/perl-lsp.wasm', () => wasmPath],
    ['/shim/', rest => path.join(web, 'node_modules/@bjorn3/browser_wasi_shim/dist', rest)],
    ['/runtime/', rest => path.join(web, 'runtime', rest)],
    ['/', rest => path.join(web, 'test', rest || 'smoke.html')],
  ];
  const server = http.createServer((req, res) => {
    const url = path.posix.normalize(req.url.split('?')[0]);
    const [prefix, resolve] = routes.find(([p]) => url === p || (p.endsWith('/') && url.startsWith(p)));
    const file = resolve(url.slice(prefix.length));
    if (!fs.existsSync(file) || !fs.statSync(file).isFile()) {
      res.writeHead(404);
      return res.end();
    }
    res.writeHead(200, {
      'content-type': types[path.extname(file)] || 'application/octet-stream',
      'cross-origin-opener-policy': 'same-origin',
      'cross-origin-embedder-policy': 'require-corp',
    });
    fs.createReadStream(file).pipe(res);
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server)));
}
