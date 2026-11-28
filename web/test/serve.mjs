// A static server for the tests. `resolve` maps a URL path to a file;
// `isolated` sends the two headers that make a page cross-origin isolated
// (off to exercise the demo's service worker, which adds them itself).
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';

const types = { '.js': 'text/javascript', '.html': 'text/html', '.css': 'text/css', '.json': 'application/json', '.wasm': 'application/wasm', '.ttf': 'font/ttf' };

export function serve(resolve, { isolated = true } = {}) {
  const server = http.createServer((req, res) => {
    const file = resolve(path.posix.normalize(decodeURIComponent(req.url.split('?')[0])));
    if (!file || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
      res.writeHead(404);
      return res.end();
    }
    const headers = { 'content-type': types[path.extname(file)] || 'application/octet-stream' };
    if (isolated) {
      headers['cross-origin-opener-policy'] = 'same-origin';
      headers['cross-origin-embedder-policy'] = 'require-corp';
    }
    res.writeHead(200, headers);
    fs.createReadStream(file).pipe(res);
  });
  return new Promise(done => server.listen(0, '127.0.0.1', () => done(server)));
}

export const serveDirectory = (dir, options) =>
  serve(url => path.join(dir, url.endsWith('/') ? url + 'index.html' : url), options);
