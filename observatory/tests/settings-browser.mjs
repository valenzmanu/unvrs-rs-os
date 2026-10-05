// Run: cargo build --locked -p observatory --example preview && node observatory/tests/settings-browser.mjs
// (needs Playwright: PLAYWRIGHT=<path to the playwright package>, default ~/node_modules/playwright)
//
// The Settings overlay in a real browser, against the example's in-memory settings API
// (UNVRS_SETTINGS_FIXTURE=observatory/fixtures/settings.json) at 776 and 1440 px:
// - opening, using and closing it leaves the cockpit's DOM and every box exactly as before;
// - every section, the review and the history fit the width (no sideways scroll) and say no bare "?";
// - every POST carries X-UNVRS-CSRF from the page's meta and no Authorization header;
// - Esc closes it and the focus returns to the gear; Tab stays inside.
// - Agent fault text fits and survives Settings; calm Agent and both drawers remain usable.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { homedir } from 'node:os';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT || `${homedir()}/node_modules/playwright`);
const root = fileURLToPath(new URL('../..', import.meta.url));
const port = 18000 + Math.floor(Math.random() * 900);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// a fresh in-memory settings API for each width (saves change it)
let server = null;
const stop = async () => {
  if (server?.pid && server.exitCode === null && server.signalCode === null) {
    const exited = once(server, 'exit');
    server.kill();
    await exited;
  }
  server = null;
};
const serve = async () => {
  await stop();
  server = spawn(`${root}/target/debug/examples/preview`, [String(port), '127.0.0.1:9'], {
    env: { ...process.env, UNVRS_SETTINGS_FIXTURE: `${root}/observatory/fixtures/settings.json` }, stdio: 'inherit',
  });
  await once(server, 'spawn');
  await sleep(1500);
  assert.equal(server.exitCode, null, 'preview server still running');
};
const browser = await chromium.launch({ args: ['--host-resolver-rules=MAP unvrs.localhost 127.0.0.1'] });
try {
  for (const width of [776, 1440]) {
    await serve();
    const pg = await browser.newPage({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
    const errors = [];
    pg.on('pageerror', (e) => errors.push(e.message));
    const posts = [];
    pg.on('request', (r) => { if (r.method() === 'POST' && r.url().endsWith('/api/settings')) posts.push(r.headers()); });
    // headless Chromium stops drawing frames for good, at random, while the galaxy's worker renders
    // (the base build's Crew diagram does the same); the galaxy is not under test, so it stays off
    const blocked = new Set();
    await pg.route(/\/cosmos\.(js|wasm)(\?.*)?$/, (r) => {
      blocked.add(new URL(r.request().url()).pathname);
      return r.abort();
    });
    await pg.goto(`http://unvrs.localhost:${port}/preview?sim=problem`);
    await pg.waitForSelector('#settings-open', { timeout: 20000 });
    await pg.evaluate(() => { const m = document.createElement('meta'); m.name = 'unvrs-settings-csrf'; m.content = 'csrf-under-test'; document.head.appendChild(m); });
    await sleep(1200);
    assert.ok(blocked.has('/cosmos.js'), 'Cosmos script request blocked');
    // The aborted script cannot request WASM; independently verify that route is blocked too.
    assert.equal(await pg.evaluate(() => fetch('/cosmos.wasm').then(() => false, () => true)), true);
    assert.ok(blocked.has('/cosmos.wasm'), 'Cosmos WASM request blocked');
    const agent = pg.locator('.light[data-key="drv:agent"]');
    const fault = await agent.innerText();
    assert.match(await agent.getAttribute('class'), /t-bad/);
    assert.match(fault, /down/);
    assert.match(fault, /PID 31/);
    assert.match(fault, /claude exited 1: rate limit reached for this account/);
    assert.equal(await agent.evaluate((n) => {
      const b = n.getBoundingClientRect(), w = n.querySelector('.lword');
      const r = w.getBoundingClientRect();
      return b.left >= 0 && b.right <= innerWidth && r.right <= b.right
        && n.scrollWidth <= n.clientWidth + 1 && w.scrollWidth <= w.clientWidth + 1;
    }), true, `Agent fault text fits@${width}`);
    // the cockpit as it stands (ages tick, so digits are masked)
    const snap = () => pg.evaluate(() => ({
      dom: document.getElementById('main').outerHTML.replace(/\d+/g, '#'),
      body: [...document.body.children].map((n) => `${n.tagName}#${n.id}${n.inert ? ' inert' : ''}`).join(','),
      boxes: [...document.querySelectorAll('#main *')].filter((n) => !n.closest('svg')).map((n) => { const r = n.getBoundingClientRect(); return [r.x, r.y, r.width, r.height].map(Math.round).join(' '); }).join('|'),
    }));
    const before = await snap();

    // open with the gear: a dialog that takes the focus; the page behind is inert
    await pg.click('#settings-open');
    await pg.waitForSelector('#settings .st-card');
    assert.equal(await pg.evaluate(() => document.getElementById('main').inert), true, 'cockpit inert while open');
    assert.equal(await pg.getAttribute('#settings', 'aria-modal'), 'true');
    const overflow = () => pg.evaluate(() => {
      const r = document.getElementById('settings');
      const p = r.querySelector('.st-pane');
      const wide = [...r.querySelectorAll('*')].filter((n) => n.getBoundingClientRect().right > innerWidth + 0.5).map((n) => n.className);
      const q = /(^|[\s·(])\?([\s·)]|$)/m.test(r.innerText);
      return { side: p.scrollWidth > p.clientWidth + 1, wide, q };
    });
    for (const sec of ['routing', 'sources', 'projects', 'safety', 'kernel', 'overrides']) {
      await pg.click(`#settings [data-sec="${sec}"]`);
      const o = await overflow();
      assert.deepEqual(o, { side: false, wide: [], q: false }, `${sec}@${width}`);
    }
    // Research effort and fallback fields use the same ordered profile editor.
    await pg.click('#settings [data-sec="routing"]');
    const researchKey = 'econ.profiles.research';
    const effort = `#settings select[data-k="${researchKey}"][data-i="1"][data-f="effort"]`;
    const fallback = `#settings [data-op="rowbool"][data-k="${researchKey}"][data-i="1"][data-f="captain_fallback"]`;
    assert.equal(await pg.inputValue(effort), '"low"');
    assert.equal(await pg.getAttribute(fallback, 'aria-checked'), 'true');
    assert.match(await pg.textContent(fallback), /Captain fallback: on/);
    await pg.selectOption(effort, '"light"');
    await pg.click(fallback);
    assert.equal(await pg.getAttribute(fallback, 'aria-checked'), 'false');
    assert.deepEqual(await overflow(), { side: false, wide: [], q: false }, `research editor@${width}`);
    await pg.click(`#settings [data-op="undo"][data-k="${researchKey}"]`);
    assert.equal(await pg.inputValue(effort), '"low"');
    assert.equal(await pg.getAttribute(fallback, 'aria-checked'), 'true');
    // a rule's edit form, then stage: a rule moved, a sensitive enum, a chip
    await pg.click('#settings [data-op="edit"][data-i="0"]');
    assert.deepEqual(await overflow(), { side: false, wide: [], q: false }, `rule form@${width}`);
    await pg.click('#settings [data-op="down"][data-k="econ.policy.rules"][data-i="0"]');
    // drag the second judge candidate above the first by its grip, then put it back with ↑/↓
    const grips = pg.locator('#settings [data-key="econ.profiles.judge"] .st-grip');
    await grips.nth(1).dragTo(grips.nth(0));
    const models = () => pg.$$eval('#settings [data-key="econ.profiles.judge"] input', (l) => l.map((i) => i.value));
    assert.deepEqual(await models(), ['newest fable', 'newest opus'], 'dragged');
    await pg.click('#settings [data-op="down"][data-k="econ.profiles.judge"][data-i="0"]');
    assert.deepEqual(await models(), ['newest opus', 'newest fable'], 'moved back from the keyboard');
    assert.equal(await pg.evaluate(() => document.activeElement.getAttribute('aria-label')), 'Move row 2 up', 'focus follows the row');
    await pg.selectOption('#settings select[data-k="econ.reasoning.deep"]', '"medium"');
    await pg.click('#settings [data-sec="sources"]');
    await pg.fill('#settings input[data-op="draft"][data-k="sources.handbook.exclude"]', 'secrets/**');
    await pg.keyboard.press('Enter');
    assert.match(await pg.textContent('#settings .st-foot'), /3 changes/);
    await pg.click('#settings [data-act="review"]');
    assert.deepEqual(await overflow(), { side: false, wide: [], q: false }, `review@${width}`);
    assert.equal(await pg.isDisabled('#settings [data-act="save"]'), true, 'sensitive: confirm first');
    await pg.check('#settings [data-act="confirm"]');
    await pg.click('#settings [data-act="save"]');
    await pg.waitForFunction(() => /Saved 3 changes/.test(document.querySelector('#settings .st-toast')?.textContent || ''));
    assert.equal(posts.length, 3);
    for (const h of posts) {
      assert.equal(h['x-unvrs-csrf'], 'csrf-under-test', 'CSRF header on every POST');
      assert.equal(h['content-type'], 'application/json');
      assert.equal(h.authorization, undefined, 'no bearer from the browser');
    }
    // history: newest first, revert the sensitive one with its confirm
    await pg.click('#settings [data-tab="history"]');
    assert.deepEqual(await overflow(), { side: false, wide: [], q: false }, `history@${width}`);
    const deep = '#settings .st-hi:has(code.st-where:text-is("econ.toml › reasoning.deep"))';
    await pg.click(`${deep} [data-act="revert"]`);
    await pg.check('#settings [data-act="revconfirm"]');
    await pg.click('#settings [data-act="revgo"]');
    await pg.waitForFunction(() => /Reverted/.test(document.querySelector('#settings .st-toast')?.textContent || ''));
    assert.equal(posts.length, 4);
    assert.equal(posts[3]['x-unvrs-csrf'], 'csrf-under-test');

    // Tab stays inside; Esc closes and the focus returns to the gear
    for (let i = 0; i < 40; i++) await pg.keyboard.press('Tab');
    assert.equal(await pg.evaluate(() => document.getElementById('settings').contains(document.activeElement)), true, 'focus trapped');
    await pg.keyboard.press('Escape');
    assert.equal(await pg.$('#settings'), null, 'closed');
    assert.equal(await pg.evaluate(() => document.activeElement.id), 'settings-open', 'focus back on the gear');
    // the cockpit is exactly as it was
    const after = await snap();
    assert.equal(after.body, before.body, 'body children and inert restored');
    assert.equal(after.dom, before.dom, 'cockpit DOM unchanged');
    assert.equal(after.boxes, before.boxes, 'cockpit layout unchanged');
    assert.equal(await agent.innerText(), fault, 'Agent fault unchanged after Settings');
    // the , key opens it again
    await pg.keyboard.press(',');
    await pg.waitForSelector('#settings');
    await pg.keyboard.press('Escape');
    await agent.click();
    await pg.waitForSelector('#drawer');
    assert.match(await pg.textContent('#drawer'), /Agent driver/);
    assert.match(await pg.textContent('#drawer'), /rate limit reached/);
    await pg.click('#drawer .dclose');
    await pg.waitForSelector('#drawer', { state: 'detached' });
    // A fresh calm fixture exercises the non-fault light through the same browser renderer.
    await pg.goto(`http://unvrs.localhost:${port}/preview?sim=calm`);
    await pg.waitForSelector('.light[data-key="drv:agent"].t-ok');
    assert.equal(await agent.innerText(), 'Agent');
    await agent.click();
    await pg.waitForSelector('#drawer');
    assert.match(await pg.textContent('#drawer'), /Agent driver/);
    await pg.click('#drawer .dclose');
    await pg.waitForSelector('#drawer', { state: 'detached' });
    await pg.click('#settings-open');
    await pg.waitForSelector('#settings .st-card');
    await pg.keyboard.press('Escape');
    assert.equal(await pg.evaluate(() => document.activeElement.id), 'settings-open');
    assert.equal(await agent.innerText(), 'Agent', 'calm Agent unchanged after Settings');
    assert.deepEqual(errors, [], 'no page errors');
    await pg.close();
    console.log(`settings-agent-browser @${width}: ok (Cosmos JS/WASM blocked)`);
  }
} finally {
  try { await browser.close(); } finally { await stop(); }
}
