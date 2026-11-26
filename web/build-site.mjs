// Assembles the static demo site: `node build-site.mjs <wasm> <out-dir>`.
// The result is self-contained (no CDN), so it can be served from any
// static host, GitHub Pages included.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const web = path.dirname(fileURLToPath(import.meta.url));
const [wasm, out] = process.argv.slice(2);
if (!wasm || !out) {
  console.error('usage: node build-site.mjs <perl-lsp.wasm> <out-dir>');
  process.exit(2);
}

// The project the page opens ships as one JSON tree, which is also what
// the runtime takes to fill the server's file system.
function perlFiles(dir) {
  const tree = {};
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const sub = perlFiles(full);
      if (Object.keys(sub).length) tree[entry.name] = sub;
    } else if (/\.(pm|pl|t)$/.test(entry.name)) {
      tree[entry.name] = fs.readFileSync(full, 'utf8');
    }
  }
  return tree;
}

fs.rmSync(out, { recursive: true, force: true });
fs.cpSync(path.join(web, 'demo'), out, { recursive: true });
fs.cpSync(path.join(web, 'runtime'), path.join(out, 'runtime'), { recursive: true });
fs.cpSync(path.join(web, 'node_modules/@bjorn3/browser_wasi_shim/dist'), path.join(out, 'shim'), { recursive: true });
fs.cpSync(path.join(web, 'node_modules/monaco-editor/min/vs'), path.join(out, 'vs'), { recursive: true });
fs.copyFileSync(wasm, path.join(out, 'perl-lsp.wasm'));
fs.writeFileSync(path.join(out, 'project.json'), JSON.stringify(perlFiles(path.join(web, 'demo/project'))));
fs.rmSync(path.join(out, 'project'), { recursive: true });
console.log(`site written to ${out}`);
