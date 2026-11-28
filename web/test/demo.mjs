// Drives the built demo page: `node test/demo.mjs <site-dir> [screenshot]`.
// Served WITHOUT the isolation headers, as GitHub Pages serves it, so the
// page has to get isolation from its own service worker before the server
// can start. Then every scene is clicked and its visible outcome checked
// against the `expect` it carries in `demo/scenes.json`.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';
import { serveDirectory } from './serve.mjs';

// What the page shows for each kind of answer. A scene's `expect` is shared
// with the native runner (`e2e/demo_scenes.py`): `text` is something the
// answer contains, `lands` the file and line a jump ends on, `report` text in
// the report dialog, `status` the status line.
const SHOWN_IN = {
  hover: '.monaco-hover-content',
  complete: '.suggest-widget',
  definition: '.reference-zone-widget',
  references: '.reference-zone-widget',
  implementation: '.reference-zone-widget',
  outline: '.quick-input-widget',
  signature: '.parameter-hints-widget',
  rename: '.rename-box',
  diagnostic: '.marker-widget',
  codeAction: '.action-widget',
  inlay: '.view-lines',
  incomingCalls: '#results',
  outgoingCalls: '#results',
  subtypes: '#results',
};
const scenes = JSON.parse(fs.readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), '../demo/scenes.json'), 'utf8')).groups.flatMap(g => g.scenes);
const EXPECT = Object.fromEntries(scenes.map(s => {
  const e = s.expect;
  if (e.lands) return [s.id, { file: e.lands[0], line: e.lands[1] }];
  if (e.report) return [s.id, e];
  if (e.status) return [s.id, ['#status', e.status]];
  assert.ok(SHOWN_IN[s.action], `scene ${s.id}: no place to look for a ${s.action} answer`);
  return [s.id, [SHOWN_IN[s.action], e.text]];
}));

const site = path.resolve(process.argv[2] ?? 'site');
const server = await serveDirectory(site, { isolated: false });
const browser = await chromium.launch();
const failures = [];
let dead = false;
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  page.on('pageerror', e => { if (e.message !== 'Canceled') console.error('[page]', e.message); });
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => document.getElementById('status').className !== '', null, { timeout: 90000 });
  assert.equal(await page.evaluate(() => crossOriginIsolated), true, 'the service worker isolates the page');
  assert.match(await page.textContent('#status'), /^Server ready/);

  const ids = await page.$$eval('[data-scene]', buttons => buttons.map(b => b.dataset.scene));
  assert.deepEqual(ids.filter(id => !EXPECT[id]), [], 'every scene has an expectation');

  for (const id of ids) {
    const expected = EXPECT[id];
    try {
      // Cross-file answers arrive once the resolver has landed the other
      // files, so each scene is retried until it shows or time runs out.
      if (expected.report) {
        await page.keyboard.press('Escape');
        await page.click(`[data-scene="${id}"]`);
        await page.locator('#report[open]').waitFor({ timeout: 120000 });
        await page.frameLocator('#report iframe').getByText(expected.report).first().waitFor({ timeout: 10000 });
        if (process.argv[3]) await page.screenshot({ path: process.argv[3].replace(/\.png$/, `-${id}.png`) });
        await page.click('#report button');
        console.log(`  ✓ ${id}`);
        continue;
      }
      await assert.doesNotReject(async () => {
        for (const deadline = Date.now() + 30000; ;) {
          await page.keyboard.press('Escape');
          await page.keyboard.press('Escape');
          await page.click(`[data-scene="${id}"]`);
          try {
            if (Array.isArray(expected)) {
              await page.locator(expected[0]).filter({ hasText: expected[1].replaceAll(' ', ' ') }).or(page.locator(expected[0]).filter({ hasText: expected[1] })).first().waitFor({ state: 'visible', timeout: 3000 });
            } else {
              await page.waitForFunction(({ file, line }) => {
                const current = document.querySelector('nav [aria-current="true"]')?.dataset.path;
                const editor = window.monaco.editor.getEditors()[0];
                return current === file && editor.getModel().getLineContent(editor.getPosition().lineNumber).includes(line);
              }, expected, { timeout: 3000 });
            }
            return;
          } catch (e) {
            if (Date.now() > deadline) throw e;
          }
        }
      });
      console.log(`  ✓ ${id}`);
    } catch (e) {
      failures.push(id);
      console.log(`  ✗ ${id}: ${String(e.message).split('\n')[0]}`);
      if (expected.report) console.log('    the page says: ' + await page.textContent('#results'));
      if (process.argv[3]) await page.screenshot({ path: process.argv[3].replace(/\.png$/, `-${id}.png`) });
      // A dead server answers nothing more: the remaining scenes would
      // each wait out their deadline to say the same thing.
      if ((await page.textContent('#status')).includes('trapped')) {
        dead = true;
        break;
      }
    }
  }
  // A thread that died may have taken no scene with it; the run still
  // failed.
  if (!dead && (await page.evaluate(() => (window.serverLog ?? []).join(''))).includes('RuntimeError')) {
    dead = true;
    failures.push('a server thread trapped');
  }
  // A trap is printed whole: the frames that say what the thread was doing
  // are at the far end of its backtrace.
  if (failures.length) console.log('server log:\n' + (await page.evaluate(() => (window.serverLog ?? []).join(''))).slice(dead ? -60000 : -40000));
  await page.keyboard.press('Escape');
  await page.click('[data-scene="chain"]');
  await page.locator('.monaco-hover-content').first().waitFor({ timeout: 5000 }).catch(() => {});
  if (process.argv[3]) await page.screenshot({ path: process.argv[3] });
} finally {
  await browser.close();
  server.close();
}
console.log(failures.length ? `demo page test FAILED: ${failures.join(', ')}` : 'demo page test passed');
process.exit(failures.length ? 1 : 0);
