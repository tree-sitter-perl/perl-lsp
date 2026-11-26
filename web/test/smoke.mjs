// Runs the real server in headless Chromium on the repo's demo project and
// asks it the questions the demo page will: `node test/smoke.mjs <wasm>`.
// Catches what the wasm build job cannot: a module that links but does not
// run, or answers differently from the native server.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';
import { serve } from './serve.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const wasm = path.resolve(process.argv[2] ?? path.join(repo, 'target/wasm32-wasip1-threads/debug/perl-lsp.wasm'));
assert.ok(fs.existsSync(wasm), `no module at ${wasm}; run scripts/wasm/build.sh`);

function perlFiles(dir) {
  const tree = {};
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) tree[entry.name] = perlFiles(full);
    else if (/\.(pm|pl|t)$/.test(entry.name)) tree[entry.name] = fs.readFileSync(full, 'utf8');
  }
  return tree;
}
const files = perlFiles(path.join(repo, 'demo'));
const uri = 'file:///work/app.pl';
const text = files['app.pl'];
// The position of `needle`'s first character, `offset` characters in.
function at(needle, offset = 0) {
  const lines = text.split('\n');
  const line = lines.findIndex(l => l.includes(needle));
  assert.notEqual(line, -1, `demo/app.pl no longer contains ${needle}`);
  return { line, character: lines[line].indexOf(needle) + offset };
}

const web = path.join(repo, 'web');
const server = await serve(url => {
  if (url === '/') return path.join(web, 'test/smoke.html');
  if (url === '/perl-lsp.wasm') return wasm;
  if (url.startsWith('/shim/')) return path.join(web, 'node_modules/@bjorn3/browser_wasi_shim/dist', url.slice('/shim/'.length));
  if (url.startsWith('/runtime/')) return path.join(web, 'runtime', url.slice('/runtime/'.length));
});
const browser = await chromium.launch();
let failed = false;
try {
  const page = await browser.newPage();
  page.on('pageerror', e => console.error('[page]', e.message));
  page.on('console', m => console.error('[console]', m.text()));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => window.startServer);
  await page.evaluate(files => {
    window.lsp = window.startServer({ wasm: '/perl-lsp.wasm', shim: '/shim/index.js', files });
  }, files);
  const REQUEST_TIMEOUT_MS = 60000;
  const request = (method, params) => Promise.race([
    page.evaluate(([m, p]) => window.lsp.request(m, p), [method, params]),
    page.evaluate(() => window.lsp.exited).then(why => { throw new Error(why); }),
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${method} got no answer in ${REQUEST_TIMEOUT_MS} ms`)), REQUEST_TIMEOUT_MS)),
  ]);
  const notify = (method, params) => page.evaluate(([m, p]) => window.lsp.notify(m, p), [method, params]);
  const doc = { textDocument: { uri } };

  try {
    const init = await request('initialize', {
      processId: null,
      rootUri: 'file:///work',
      capabilities: {},
      workspaceFolders: [{ uri: 'file:///work', name: 'work' }],
    });
    assert.equal(init.serverInfo.name, 'perl-lsp');
    await notify('initialized', {});
    await notify('textDocument/didOpen', { textDocument: { uri, languageId: 'perl', version: 1, text } });

    // The type comes through `make_account` in another file, which the
    // resolver thread lands after the open; poll until it has.
    let hover;
    for (const deadline = Date.now() + 30000; ;) {
      hover = await request('textDocument/hover', { ...doc, position: at('$acct', 1) });
      if (hover?.contents.value.includes('Account') || Date.now() > deadline) break;
      await new Promise(r => setTimeout(r, 200));
    }
    assert.match(hover.contents.value, /type: Account/, 'hover types $acct through the imported sub');

    const definition = await request('textDocument/definition', { ...doc, position: at('make_account', 1) });
    assert.equal([definition].flat()[0].uri, 'file:///work/lib/Bank.pm', 'goto-def crosses into Bank.pm');

    const completion = await request('textDocument/completion', { ...doc, position: at('$acct->deposit', '$acct->'.length) });
    const labels = (completion.items ?? completion).map(i => i.label);
    for (const method of ['balance', 'deposit', 'describe', 'owner']) assert.ok(labels.includes(method), `completion offers ${method}`);

    const references = await request('textDocument/references', {
      ...doc,
      position: at('deposit', 1),
      context: { includeDeclaration: true },
    });
    assert.deepEqual(references.map(r => r.uri.replace('file:///work/', '')).sort(), ['app.pl', 'lib/Account.pm'], 'references span both files');

    console.log('browser smoke test passed');
  } catch (e) {
    failed = true;
    console.error(e);
    console.error('--- server log ---\n' + (await page.evaluate(() => window.lsp.logs.join(''))).slice(-4000));
  }
} finally {
  await browser.close();
  server.close();
}
process.exit(failed ? 1 : 0);
