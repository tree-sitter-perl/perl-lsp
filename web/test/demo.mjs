// Drives the built demo page: `node test/demo.mjs <site-dir> [screenshot]`.
// Served WITHOUT the isolation headers, as GitHub Pages serves it, so the
// page has to get isolation from its own service worker before the server
// can start. Then every scene is clicked and its visible outcome checked: a
// scene with no expectation here fails the run.
import assert from 'node:assert/strict';
import path from 'node:path';
import { chromium } from 'playwright';
import { serveDirectory } from './serve.mjs';

const hover = text => ['.monaco-hover-content', text];
const suggest = text => ['.suggest-widget', text];
const peek = text => ['.reference-zone-widget', text];
const results = text => ['#results', text];
// A definition scene ends on another line or file: [file, text of the line].
// A report scene opens a dialog whose framed page mentions `report`.
const lands = (file, line) => ({ file, line });

const EXPECT = {
  infer: hover('type: Shop::Cart'),
  chain: hover('returns: Shop::Cart'),
  maybe: hover('may be undef'),
  narrow: hover('type: Shop::Item'),
  ref: hover('type: ArrayRef'),
  inlay: ['.view-lines', '→ HashRef'],
  'flow-return': hover('type: Shop::Item'),
  'flow-noreturn': hover('type: Shop::Item'),
  'flow-ordie': hover('type: Shop::Item'),
  'flow-loop': hover('type: Shop::Item'),
  'flow-reassign': hover('may be undef here'),
  'flow-redundant': hover('this guard is redundant'),
  'flow-closure': hover('type: Shop::Item|…'),
  'flow-try': hover('type: Shop::Item|…'),
  'flow-deref': hover('hash deref dies at runtime'),
  union: hover('Shop::Discount::Fixed|Shop::Discount::Percent'),
  'union-arms': hover('class Shop::Discount::Percent'),
  'union-def': peek('Percent.pm'),
  'union-warn': hover('is not defined in Shop::Discount::Fixed'),
  shape: suggest('paths'),
  'named-complete': suggest('express'),
  'named-def': lands('lib/Shop/Shipping.pm', '$opt{weight}'),
  'named-rename': ['.rename-box', ''],
  tuple: hover('type: Numeric'),
  'tuple-sub': hover('returns: Sequence'),
  def: lands('lib/Shop/Util.pm', 'sub format_price'),
  role: lands('lib/Shop/Role/Describable.pm', 'sub describe'),
  super: lands('lib/Shop/Discount.pm', 'sub describe'),
  ctor: lands('lib/Shop/Cart.pm', "currency  => $args{currency}"),
  key: lands('lib/Shop/Util.pm', 'return { net =>'),
  dynamic: hover('resolved from'),
  refs: peek('Describable.pm'),
  impl: peek('Percent.pm'),
  outline: ['.quick-input-widget', 'has_tags'],
  complete: suggest('add_shipping'),
  keys: suggest('gross'),
  signature: ['.parameter-hints-widget', '$amount'],
  rename: ['.rename-box', ''],
  docs: hover('Adds $quantity of an item'),
  typo: ['.marker-widget', 'is not defined in Shop::Cart'],
  import: ['.action-widget', "Import 'round_cents' from Shop::Util"],
  'mojo-helper': hover('returns: Shop::Cart'),
  'mojo-helper-def': lands('lib/Shop/Web.pm', 'helper( current_cart'),
  'mojo-route': lands('lib/Shop/Web/Controller/Cart.pm', 'sub show'),
  'mojo-docs': hover('my $route = $r->get'),
  'mojo-source': lands('local/lib/perl5/Mojolicious/Controller.pm', 'sub render'),
  'dbic-rs': hover('type: DBIx::Class::ResultSet'),
  'dbic-row': hover('type: Row'),
  'dbic-column': lands('lib/Shop/Schema/Result/Product.pm', 'price =>'),
  'dbic-rel': lands('lib/Shop/Schema/Result/Product.pm', 'has_many( reviews'),
  'minion-task': lands('lib/Shop/Jobs.pm', 'send_receipt => sub'),
  'minion-args': lands('lib/Shop/Jobs.pm', '$args{total}'),
  'minion-options': suggest('attempts'),
  'minion-docs': hover('class Minion::Job'),
  events: hover('registration'),
  'tt-isa': hover('type: Numeric'),
  'tt-instance': hover('type: Shop::Cart'),
  'tt-narrow': hover('type: Numeric'),
  'moo-handles': hover('returns: HashRef'),
  'moo-ctor': suggest('points'),
  'moo-clearer': lands('lib/Shop/Customer.pm', "clearer => 'forget_address'"),
  'moo-source': lands('local/lib/perl5/Moo.pm', 'package Moo'),
  heatmap: { report: 'perl-lsp usage heatmap' },
  callers: results('add_shipping'),
  callees: results('round_cents'),
  subtypes: results('Shop::Discount::Fixed'),
  class: lands('lib/Shop/Order.pm', 'field $cart'),
  'plugin-def': lands('lib/Shop/Flags.pm', 'feature_flag new_checkout'),
  'plugin-refs': peek('flags.pl'),
  'plugin-rename': ['.rename-box', ''],
  'plugin-source': lands('.perl-lsp/flags.rhai', 'fn patterns'),
  // The edit restarts the server; the scene after it only answers on the
  // restarted one.
  'plugin-edit': ['#status', 'Plugin reloaded'],
  'plugin-after': lands('lib/Shop/Flags.pm', 'feature_flag beta_search'),
};

const site = path.resolve(process.argv[2] ?? 'site');
const server = await serveDirectory(site, { isolated: false });
const browser = await chromium.launch();
const failures = [];
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
    }
  }
  if (failures.length) console.log('server log:\n' + (await page.evaluate(() => (window.serverLog ?? []).join(''))).slice(-40000));
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
