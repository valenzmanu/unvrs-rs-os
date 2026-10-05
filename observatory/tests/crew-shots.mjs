// Run: PREVIEW_BIN=<preview example> node observatory/tests/crew-shots.mjs <label> <out-dir>
// Screenshots of the cockpit (and of its Crew zone) at 776 and 1440 px for two snapshots fed by
// the stand-in kernel: L1 with two workers plus nested L3s ("tree"), and 12 workers ("many").
// Used for the before (base build) / after (this build) evidence; it asserts nothing.
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { homedir } from 'node:os';
import { mkdirSync } from 'node:fs';
import { startKernel, worker } from './crew-fixture.mjs';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT || `${homedir()}/node_modules/playwright`);
const [label, out] = process.argv.slice(2);
const bin = process.env.PREVIEW_BIN;
if (!label || !out || !bin) throw new Error('usage: PREVIEW_BIN=... node crew-shots.mjs <label> <out-dir>');
mkdirSync(out, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const port = 18000 + Math.floor(Math.random() * 900);

const sets = {
  tree: [
    worker(180, 1, 'unvrs-rs', 'adding the go quote and done-when lines to the diagram cards'),
    worker(181, 1, 'unvrs-rs', 'worker tree in the Observatory Crew zone', { model: 'claude-sonnet-5-5', effort: 'high' }),
    worker(20, 2, 'brand', 'drafting the landing page copy'),
    worker(21, 20, 'brand', 'checking contrast of the new palette'),
    worker(22, 20, 'brand', null, { model: null, effort: null, state: 'held' }),
  ],
  many: Array.from({ length: 12 }, (_, i) => worker(100 + i, 2 + (i % 2), i % 2 ? 'travel' : 'brand', `task number ${i} with a fairly long description of the work in progress`)),
};
const kernel = await startKernel(sets.tree);
const server = spawn(bin, [String(port), `127.0.0.1:${kernel.port}`], { stdio: 'ignore' });
await once(server, 'spawn');
await sleep(1500);
const browser = await chromium.launch({ args: ['--host-resolver-rules=MAP unvrs.localhost 127.0.0.1'] });
try {
  for (const width of [776, 1440]) {
    for (const [name, workers] of Object.entries(sets)) {
      kernel.state.workers = workers;
      const pg = await browser.newPage({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
      await pg.route(/\/cosmos\.(js|wasm)(\?.*)?$/, (r) => r.abort());
      await pg.goto(`http://unvrs.localhost:${port}/preview`);
      await pg.waitForSelector('.z-crew', { timeout: 20000 });
      await sleep(2500);
      await pg.screenshot({ path: `${out}/${label}-${name}-${width}-cockpit.png` });
      await pg.locator('.z-crew').screenshot({ path: `${out}/${label}-${name}-${width}-crew.png` });
      await pg.close();
    }
  }
} finally {
  await browser.close();
  server.kill();
  await kernel.close();
}
