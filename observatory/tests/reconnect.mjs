// Run: node observatory/tests/reconnect.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
const boot = readFileSync(new URL('../src/boot.js', import.meta.url), 'utf8');
const reconnect = boot.slice(0, boot.indexOf('  const canvas =')) + '})();';

for (const initial of ['open', 'closing', 'closed', 60, 100, 150]) {
  const timers = [], listeners = {};
  const lost = { hidden: true };
  const ws = { readyState: { open: 1, closing: 2, closed: 3, 'late-closed': 3 }[initial],
    addEventListener: (type, fn) => { listeners[type] = fn; } };
  let reloaded, probes = 0, elapsed = 0;
  const location = { pathname: '/preview', search: '?sim=problem', hash: '', href: 'http://localhost:7628/preview?sim=problem', replace: u => { reloaded = u; } };
  const context = {
    document: { getElementById: () => lost },
    window: { ipc: typeof initial === 'number' ? undefined : { ws } },
    WebSocket: { CLOSING: 2, CLOSED: 3 }, location,
    history: { replaceState: (_state, _title, url) => { location.href = new URL(url, location.href).href; } },
    setTimeout: (fn, delay) => timers.push({ fn, at: elapsed + delay }),
    fetch: async () => { if (++probes === 1) throw new Error('offline'); return { ok: true }; },
  };
  vm.runInNewContext(reconnect, context);
  if (typeof initial === 'number') {
    elapsed = initial; // server dies before the first 300ms watcher retry
    context.window.ipc = { ws };
    ws.readyState = 3;
    assert.equal(listeners.close, undefined, 'close event already missed');
    assert.equal(timers[0].at, 300);
    const watch = timers.shift(); elapsed = watch.at; watch.fn();
  }
  if (initial === 'open') { assert.equal(lost.hidden, true); listeners.close(); }
  listeners.close(); // a duplicate close never starts a second retry loop
  assert.equal(lost.hidden, false);
  assert.equal(timers.length, 1);
  context.history.replaceState({}, '', '/preview??sim=problem#');
  assert.equal(location.href, 'http://localhost:7628/preview?sim=problem');
  for (let i = 0; i < 2; i++) {
    const timer = timers.shift(); elapsed = timer.at; timer.fn();
    await new Promise(resolve => setImmediate(resolve));
  }
  assert.equal(probes, 2);
  assert.equal(reloaded, 'http://localhost:7628/preview?sim=problem');
  assert.equal(timers.length, 0);
}
console.log('Reconnect: open, closing, closed-before-watch, 60/100/150ms early closes, retry and original URL passed.');
