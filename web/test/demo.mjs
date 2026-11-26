// Drives the built demo page: `node test/demo.mjs <site-dir>`. Served
// WITHOUT the isolation headers, as GitHub Pages serves it, so the page has
// to get isolation from its own service worker before the server can start.
import assert from 'node:assert/strict';
import path from 'node:path';
import { chromium } from 'playwright';
import { serveDirectory } from './serve.mjs';

const site = path.resolve(process.argv[2] ?? 'site');
const server = await serveDirectory(site, { isolated: false });
const browser = await chromium.launch();
let failed = false;
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  page.on('pageerror', e => console.error('[page]', e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  try {
    await page.waitForFunction(() => document.getElementById('status').className !== '', null, { timeout: 90000 });
    assert.equal(await page.evaluate(() => crossOriginIsolated), true, 'the service worker isolates the page');
    assert.match(await page.textContent('#status'), /^Server ready/);

    await page.click('#scenes li:nth-child(1) button');
    const hover = page.locator('.monaco-hover-content');
    await hover.filter({ hasText: 'Account' }).waitFor({ timeout: 30000 });

    await page.keyboard.press('Escape');
    await page.click('#scenes li:nth-child(2) button');
    await page.waitForFunction(() => document.querySelector('#files button[aria-current="true"]')?.dataset.path === 'lib/Bank.pm', null, { timeout: 30000 });

    if (process.argv[3]) await page.screenshot({ path: process.argv[3] });
    console.log('demo page test passed');
  } catch (e) {
    failed = true;
    console.error(e);
    console.error('status:', await page.textContent('#status'));
  }
} finally {
  await browser.close();
  server.close();
}
process.exit(failed ? 1 : 0);
