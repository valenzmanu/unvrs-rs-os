// Run: cargo build --locked -p observatory --example preview && node observatory/tests/crew-tree.mjs
// (needs Playwright: PLAYWRIGHT=<path to the playwright package>, default ~/node_modules/playwright)
//
// The cockpit's Crew zone in a real browser at 776 and 1440 px, fed by a stand-in kernel:
// - L1's own workers and nested L3s hang under the seat that started them, one row each;
// - a live snapshot change updates the rows in place (the same DOM nodes, no remount, the rows
//   below do not jump) and adds / removes rows;
// - 12 workers show 8 rows and "+4 more · open diagram", which opens the diagram;
// - a worker row opens its drawer, the Crew heading opens the diagram, Esc closes each;
// - nothing scrolls sideways and no bare "?" is drawn.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { homedir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { mkdir } from 'node:fs/promises';
import { startKernel, worker } from './crew-fixture.mjs';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT || `${homedir()}/node_modules/playwright`);
const root = fileURLToPath(new URL('../..', import.meta.url));
const bin = process.env.PREVIEW_BIN || `${root}/target/debug/examples/preview`;
const port = 18000 + Math.floor(Math.random() * 900);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const evidence = process.argv[2];
if (evidence) await mkdir(evidence, { recursive: true });

const kernel = await startKernel([
  worker(180, 1, 'unvrs-rs', 'porting the cards'),
  worker(181, 1, 'unvrs-rs'),
  worker(20, 2, 'brand', 'top'),
  worker(21, 20, 'brand', 'child'),
]);
const server = spawn(bin, [String(port), `127.0.0.1:${kernel.port}`], { stdio: 'inherit' });
await once(server, 'spawn');
await sleep(1500);
assert.equal(server.exitCode, null, 'preview server still running');
const browser = await chromium.launch({ args: ['--host-resolver-rules=MAP unvrs.localhost 127.0.0.1'] });

const until = async (what, fn, ms = 8000) => {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    assert.ok(Date.now() < end, `timed out: ${what}`);
    await sleep(100);
  }
};

try {
  for (const width of [776, 1440]) {
    kernel.state.workers = [
      worker(180, 1, 'unvrs-rs', 'porting the cards'),
      worker(181, 1, 'unvrs-rs'),
      worker(20, 2, 'brand', 'top'),
      worker(21, 20, 'brand', 'child'),
    ];
    const pg = await browser.newPage({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
    const errors = [];
    pg.on('pageerror', (e) => errors.push(e.message));
    // headless Chromium stalls while the galaxy's worker renders; it is not under test
    const blocked = new Set();
    await pg.route(/\/cosmos\.(js|wasm)(\?.*)?$/, (r) => {
      blocked.add(new URL(r.request().url()).pathname);
      return r.abort();
    });
    await pg.goto(`http://unvrs.localhost:${port}/preview`);
    await pg.waitForSelector('.z-crew .wrow', { timeout: 20000 });
    assert.equal(await pg.evaluate(() => fetch('/cosmos.wasm').then(() => false, () => true)), true);
    assert.ok(blocked.has('/cosmos.js') && blocked.has('/cosmos.wasm'), 'Cosmos requests blocked');

    const rows = () => pg.$$eval('.z-crew .wrow', (l) => l.map((b) => [b.dataset.pid, b.dataset.depth]));
    const order = () => pg.$$eval('.z-crew [data-pid]', (l) => l.map((b) => b.dataset.pid));
    assert.deepEqual(await rows(), [['180', '0'], ['181', '0'], ['20', '0'], ['21', '1']], `rows@${width}`);
    // each under the seat that started it: L1 (1), brand (2), then travel (3) with none
    assert.deepEqual(await order(), ['1', '180', '181', '2', '20', '21', '3'], `order@${width}`);
    assert.match(await pg.innerText('.z-crew'), /L3 · unvrs-rs · Claude/);
    assert.equal(await pg.innerText('.z-crew .wrow[data-pid="180"] .wdoing'), 'porting the cards');
    assert.equal(await pg.innerText('.z-crew .wrow[data-pid="181"] .wdoing'), 'Intent of 181');

    const fits = () => pg.evaluate(() => {
      const wide = [...document.querySelectorAll('.z-crew *')]
        .filter((n) => n.getBoundingClientRect().right > innerWidth + 0.5).map((n) => n.className);
      const q = /(^|[\s·(])\?([\s·)]|$)/m.test(document.querySelector('.z-crew').innerText);
      return { side: document.scrollingElement.scrollWidth > innerWidth + 1, wide, q };
    });
    assert.deepEqual(await fits(), { side: false, wide: [], q: false }, `fit@${width}`);
    const heights = await pg.$$eval('.z-crew .wrow', (l) => l.map((b) => b.getBoundingClientRect().height));
    assert.equal(new Set(heights).size, 1, `every worker row is the same height@${width}: ${heights}`);

    // live update: mark the nodes, change the snapshot, and the same nodes carry the new text
    await pg.evaluate(() => {
      document.querySelectorAll('.z-crew [data-pid]').forEach((n) => { n.__keep = n.dataset.pid; });
      document.querySelector('.z-crew').__keep = 'zone';
    });
    const y = (pid) => pg.$eval(`.z-crew [data-pid="${pid}"]`, (n) => n.getBoundingClientRect().y);
    const y2 = await y(2), y20 = await y(20);
    kernel.state.workers[0] = worker(180, 1, 'unvrs-rs', 'now reviewing the diff');
    kernel.state.workers.push(worker(22, 21, 'brand', 'grandchild'));
    await until('new text and the new row', async () =>
      (await pg.innerText('.z-crew .wrow[data-pid="180"] .wdoing')) === 'now reviewing the diff'
      && (await rows()).length === 5);
    assert.deepEqual(await rows(), [['180', '0'], ['181', '0'], ['20', '0'], ['21', '1'], ['22', '2']]);
    assert.deepEqual(await pg.$$eval('.z-crew [data-pid]', (l) => l.map((n) => n.__keep)),
      ['1', '180', '181', '2', '20', '21', undefined, '3'], `kept nodes@${width} (the new row 22 is the only new node)`);
    assert.equal(await pg.$eval('.z-crew', (n) => n.__keep), 'zone', 'the zone is not remounted');
    // nothing above the new row moved
    assert.equal(await y(2), y2, 'seat above the change did not jump');
    assert.equal(await y(20), y20, 'row above the change did not jump');
    // one finishes: its row goes, the rest keep their nodes
    kernel.state.workers = kernel.state.workers.filter((w) => w.pid !== 181);
    await until('row removed', async () => (await rows()).length === 4);
    assert.equal(await pg.$eval('.z-crew .wrow[data-pid="180"]', (n) => n.__keep), '180');
    assert.equal(await pg.$eval('.z-crew .wrow[data-pid="20"]', (n) => n.__keep), '20');

    // a worker row opens its drawer (not the diagram); Esc closes it
    await pg.click('.z-crew .wrow[data-pid="180"]');
    await pg.waitForSelector('#drawer');
    assert.match(await pg.innerText('#drawer'), /PID 180/);
    assert.equal(await pg.locator('#diagram').count(), 0, 'the drawer, not the diagram');
    await pg.keyboard.press('Escape');
    await until('drawer closed', async () => (await pg.locator('#drawer').count()) === 0);
    // the Crew heading opens the diagram
    await pg.click('#crew-open');
    await pg.waitForSelector('#diagram');
    const card = pg.locator('#diagram .node[data-key="pid:180"]');
    assert.equal(await card.locator('.ngo').innerText(), 'Go: Ok, go');
    assert.equal(await card.locator('.ndone').innerText(), 'Done when: Report cites sources');
    assert.equal(await pg.locator('#diagram .node[data-key="group:loose"]').count(), 0, 'nested workers have a parent');
    for (const [parent, child] of [['pid:20', 'pid:21'], ['pid:21', 'pid:22']]) {
      assert.equal(await pg.locator(`#diagram .node[data-key="${child}"]`).count(), 1, `${child} drawn once`);
      assert.ok(await pg.$eval(`#diagram .node[data-key="${child}"]`,
        (n, key) => n.getBoundingClientRect().top > document.querySelector(`.node[data-key="${key}"]`).getBoundingClientRect().bottom, parent), `${child} below ${parent}`);
    }
    await card.evaluate((n) => { n.__keep = 'card'; });
    kernel.state.workers[0].done_when = 'Checked report cites sources';
    kernel.state.workers[0].econ = { role: 'worker', reason: 'implementation task; default deep reasoning' };
    await until('admission and route update in place', async () =>
      (await card.locator('.ndone').innerText()) === 'Done when: Checked report cites sources'
      && (await card.locator('.nroute').count()) === 1);
    assert.equal(await card.evaluate((n) => n.__keep), 'card');
    assert.deepEqual(await pg.$$eval('#diagram .node.k-l3', (nodes) => nodes.flatMap((n) => {
      const card = n.getBoundingClientRect();
      return [...n.querySelectorAll('.nalign')].filter((line) => {
        const r = line.getBoundingClientRect();
        return r.top < card.top || r.bottom > card.bottom;
      }).map(() => n.dataset.key);
    })), [], 'admission lines fit their cards');
    await until('L1 frame settled after its card grew', async () => card.evaluate((n) => {
      const worker = n.getBoundingClientRect();
      const frame = document.querySelector('.dgm-frame[data-key="frame:l1"]').getBoundingClientRect();
      return frame.left <= worker.left && frame.right >= worker.right && frame.bottom >= worker.bottom + 20;
    }));
    if (evidence) await pg.screenshot({ path: `${evidence}/admission-${width}.png` });
    await pg.keyboard.press('Escape');
    await until('diagram closed', async () => (await pg.locator('#diagram').count()) === 0);

    // 12 workers: eight rows, then the link into the diagram
    kernel.state.workers = Array.from({ length: 12 }, (_, i) => worker(100 + i, 2, 'brand', `task ${i}`));
    await until('capped', async () => (await rows()).length === 8);
    assert.equal(await pg.innerText('.z-crew .tree-more'), '+4 more · open diagram');
    assert.deepEqual(await fits(), { side: false, wide: [], q: false }, `capped fit@${width}`);
    await pg.click('.z-crew .tree-more');
    await pg.waitForSelector('#diagram');
    assert.equal(await pg.locator('#diagram .node[data-key^="pid:"], #diagram [data-pid]').count() >= 12, true, 'the diagram has them all');
    await pg.keyboard.press('Escape');
    // none: the seats only
    kernel.state.workers = [];
    await until('no workers', async () => (await rows()).length === 0);
    assert.equal(await pg.locator('.z-crew .tree-more').count(), 0);
    assert.deepEqual(errors, [], 'no page errors');
    await pg.close();
    console.log(`Crew tree @${width}px: rows, nesting, live update in place, cap, drawer, diagram and admission lines passed.`);
  }
} finally {
  await browser.close();
  server.kill();
  await kernel.close();
}
