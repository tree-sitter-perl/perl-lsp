// Assembles the static demo site:
//   node build-site.mjs <wasm> <out-dir> <perl5-dir>
// <perl5-dir> is where the project's cpanfile was installed, e.g.
//   cpm install -L deps --cpanfile demo/project/cpanfile   ->  deps/lib/perl5
// The result is self-contained (no CDN), so it can be served from any
// static host, GitHub Pages included.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const web = path.dirname(fileURLToPath(import.meta.url));
const [wasm, out, perl5] = process.argv.slice(2);
if (!wasm || !out || !perl5) {
  console.error('usage: node build-site.mjs <perl-lsp.wasm> <out-dir> <perl5-dir>');
  process.exit(2);
}

// The project the page opens ships as one JSON tree, which is also what
// the runtime takes to fill the server's file system.
function sources(dir, keep) {
  const tree = {};
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const sub = sources(full, keep);
      if (Object.keys(sub).length) tree[entry.name] = sub;
    } else if (keep.test(entry.name)) {
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
fs.writeFileSync(path.join(out, 'project.json'), JSON.stringify(sources(path.join(web, 'demo/project'), /\.(pm|pl|t|rhai)$|^cpanfile$/)));
// Dependencies are a second file: the page gives them to the server whole
// but opens one in the editor only when a jump lands in it.
const deps = sources(perl5, /\.pm$/);
if (!deps['Mojolicious.pm']) throw new Error(`${perl5} does not hold the demo's dependencies`);
fs.writeFileSync(path.join(out, 'deps.json'), JSON.stringify(deps));
fs.rmSync(path.join(out, 'project'), { recursive: true });
// Visit counting is a property of one deployment, so it comes from the
// environment: GOATCOUNTER_URL=https://<code>.goatcounter.com/count
const counter = process.env.GOATCOUNTER_URL;
if (counter) {
  if (!/^https:\/\/[\w.-]+\/count$/.test(counter)) throw new Error(`GOATCOUNTER_URL is not a GoatCounter endpoint: ${counter}`);
  const page = path.join(out, 'index.html');
  fs.writeFileSync(page, fs.readFileSync(page, 'utf8').replace('<meta name="goatcounter" content="">', `<meta name="goatcounter" content="${counter}">`));
}
console.log(`site written to ${out}`);
