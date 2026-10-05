// Page glue: reconnects the live bridge, starts the Cosmos window off the main thread,
// tells it when to sleep (hidden tab, reduced motion), where the window's glass is and
// where to frame the galaxy, and helps the Rust deck and cockpit with what only the page
// can see: keys pressed before anything is focused, #section deep links, focus, and
// remembering whether the cockpit was folded away. Navigation and the cockpit's state
// are decided in Rust (view.rs).
(() => {
  'use strict';
  const lost = document.getElementById('lost');
  // ponytail: LiveView 0.7.10 adds delimiters already present in search/hash;
  // remove this exact-route correction when its history initialization is fixed.
  const replaceState = history.replaceState;
  history.replaceState = function (state, title, url) {
    const { pathname, search, hash } = location;
    if (url === pathname + '?' + search + '#' + hash) url = pathname + search + hash;
    return replaceState.call(this, state, title, url);
  };
  const reconnectUrl = location.href;
  const probe = () =>
    fetch('/api/snapshot', { cache: 'no-store' })
      .then((r) => (r.ok ? location.replace(reconnectUrl) : setTimeout(probe, 2000)))
      .catch(() => setTimeout(probe, 2000));
  let reconnecting = false;
  const reconnect = () => {
    if (reconnecting) return;
    reconnecting = true;
    lost.hidden = false;
    setTimeout(probe, 1500);
  };
  const watchSocket = () => {
    const ws = window.ipc && window.ipc.ws;
    if (!ws) return setTimeout(watchSocket, 300);
    ws.addEventListener('close', reconnect);
    // The interpreter can connect and close before this watcher first sees it.
    if (ws.readyState === WebSocket.CLOSING || ws.readyState === WebSocket.CLOSED) reconnect();
  };
  watchSocket();

  const canvas = document.getElementById('cosmos');
  const hero = document.getElementById('hero');
  if (!canvas || !hero) return;
  const phaseEl = document.getElementById('cosmos-phase');
  const statsEl = document.getElementById('cosmos-stats');
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  const fmt = new Intl.NumberFormat('en-US');
  const onMsg = (m) => {
    if (m.type === 'error') { console.warn(m.text); hero.classList.add('nocosmos'); return; }
    if (m.type !== 'stats') return;
    phaseEl.textContent = m.phase ? `EPOCH ${String(m.epoch).padStart(2, '0')} · ${m.phase}` : '';
    statsEl.textContent = m.still
      ? `${fmt.format(m.stars)} stars · still`
      : `${fmt.format(m.stars)} stars · ${m.fps} fps · ${m.ms.toFixed(1)} ms`;
    window.__cosmos = m;
    window.__cosmosFrames = (window.__cosmosFrames || 0) + 1;
  };
  const size = () => {
    const r = canvas.getBoundingClientRect();
    return { type: 'resize', w: r.width, h: r.height, dpr: window.devicePixelRatio || 1 };
  };
  const pending = [];
  let send = (m) => pending.push(m);
  const ready = (fn) => { send = fn; pending.splice(0).forEach(fn); };
  window.__cosmosSend = (m) => send(m);
  const init = Object.assign(size(), {
    type: 'init', reduced: reduced.matches, wasm: new URL('/cosmos.wasm', location.href).href,
  });
  let started = false;
  if (canvas.transferControlToOffscreen && window.Worker) {
    try {
      const worker = new Worker('/cosmos.js');
      const off = canvas.transferControlToOffscreen();
      worker.onmessage = (e) => onMsg(e.data);
      worker.postMessage(Object.assign(init, { canvas: off }), [off]);
      ready((m) => worker.postMessage(m));
      started = true;
    } catch (e) { console.warn('cosmos: worker unavailable, drawing on the page', e); }
  }
  if (!started) {
    const s = document.createElement('script');
    s.src = '/cosmos.js';
    s.onload = () => {
      const eng = new window.CosmosEngine(onMsg);
      eng.msg(Object.assign(init, { canvas }));
      ready((m) => eng.msg(m));
    };
    document.head.appendChild(s);
  }

  let resizeTimer = 0;
  new ResizeObserver(() => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => send(size()), 120);
  }).observe(canvas);
  const flags = { type: 'run', visible: !document.hidden, onscreen: true, focused: document.hasFocus() };
  const run = () => send(Object.assign({}, flags));
  document.addEventListener('visibilitychange', () => { flags.visible = !document.hidden; run(); });
  new IntersectionObserver((es) => { flags.onscreen = es[es.length - 1].isIntersecting; run(); }).observe(hero);
  window.addEventListener('focus', () => { flags.focused = true; run(); });
  window.addEventListener('blur', () => { flags.focused = false; run(); });
  reduced.addEventListener('change', () => send({ type: 'reduced', on: reduced.matches }));
  run();

  // The live console carries a few numbers for the sky: working sessions become green
  // stars, open calls pulse pink, and each new seat move streaks a comet.
  let last = { w: -1, c: -1, m: null };
  setInterval(() => {
    const el = document.getElementById('cosmos-signals');
    if (!el) return;
    const w = +el.dataset.workers || 0, c = +el.dataset.calls || 0, m = el.dataset.move || '0';
    if (w !== last.w || c !== last.c) send({ type: 'signals', workers: w, calls: c });
    if (last.m !== null && m !== last.m && m !== '0') send({ type: 'comet' });
    last = { w, c, m };
  }, 500);

  // The ship's window is laid out by the page: #viewport is its glass (the renderer paints
  // the frame around it), and the galaxy is framed in the cockpit's clear sight (#sight),
  // or in the whole glass while the cockpit is folded away (the camera glides between the
  // two). Fractions of the canvas.
  const sent = { window: '', focus: '' };
  const rect = (type, el) => {
    const c = canvas.getBoundingClientRect();
    if (!el || c.width < 1 || c.height < 1) return null;
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) return null;
    const f = (v) => Math.round(Math.min(1, Math.max(0, v)) * 1000) / 1000;
    return { type, x0: f((r.left - c.left) / c.width), y0: f((r.top - c.top) / c.height),
      x1: f((r.right - c.left) / c.width), y1: f((r.bottom - c.top) / c.height) };
  };
  const put = (m) => {
    if (!m) return;
    const key = `${m.x0},${m.y0},${m.x1},${m.y1}`;
    if (key !== sent[m.type]) { sent[m.type] = key; send(m); }
  };
  const sendWindow = () => {
    const vp = document.getElementById('viewport');
    const bridge = document.getElementById('bridge');
    const sight = document.getElementById('sight');
    put(rect('window', vp));
    const up = !bridge || bridge.dataset.cockpit !== 'hidden';
    put((up && rect('focus', sight)) || rect('focus', vp));
  };
  window.__cosmosFrame = sendWindow;
  window.addEventListener('resize', () => setTimeout(sendWindow, 140));
  setInterval(sendWindow, 1000);
})();

// The cockpit (live page only): H pressed while nothing is focused goes to the toggle;
// focus never stays inside a cockpit that has folded away; the captain's choice is
// remembered across reloads (localStorage) and restored by pressing the toggle once the
// live bridge is up, with the cockpit kept out of sight until then.
(() => {
  'use strict';
  const KEY = 'unvrs.cockpit';
  const store = {
    get: () => { try { return localStorage.getItem(KEY); } catch (e) { return null; } },
    set: (v) => { try { localStorage.setItem(KEY, v); } catch (e) { /* private mode */ } },
  };
  let restoring = store.get() === 'hidden';
  if (restoring) {
    document.body.classList.add('cockpit-restoring');
    setTimeout(() => { restoring = false; document.body.classList.remove('cockpit-restoring'); }, 6000);
  }
  let byKey = 0; // when H was last pressed: a reveal by keyboard puts focus on the deck
  window.addEventListener('keydown', (e) => {
    if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    if (e.key !== 'h' && e.key !== 'H') return;
    const t = e.target;
    const btn = document.getElementById('cockpit-toggle');
    if (!btn) return;
    byKey = performance.now();
    if (t === document.body || t === document.documentElement || !t) {
      e.preventDefault();
      btn.dispatchEvent(new KeyboardEvent('keydown', { key: e.key, bubbles: true }));
    }
  }, true);
  let seen = null;
  const changed = (bridge) => {
    const now = bridge.dataset.cockpit;
    if (!now || now === seen) return;
    const first = seen === null;
    seen = now;
    if (restoring) {
      if (now === 'shown') { const b = document.getElementById('cockpit-toggle'); if (b) b.click(); return; }
      restoring = false;
      document.body.classList.remove('cockpit-restoring');
      return; // a restored state on load: leave focus where the browser put it
    } else if (!first) {
      store.set(now);
    }
    const cockpit = document.getElementById('cockpit');
    const btn = document.getElementById('cockpit-toggle');
    const a = document.activeElement;
    if (now === 'hidden') {
      // never leave focus inside what just disappeared
      if (btn && (!a || a === document.body || (cockpit && cockpit.contains(a)))) btn.focus({ preventScroll: true });
    } else if (!first && performance.now() - byKey < 1500) {
      const deck = document.getElementById('deck');
      if (deck) deck.focus({ preventScroll: true });
    }
    if (window.__cosmosFrame) { window.__cosmosFrame(); setTimeout(window.__cosmosFrame, 500); }
  };
  new MutationObserver(() => {
    const bridge = document.getElementById('bridge');
    if (bridge && bridge.classList.contains('live')) changed(bridge);
  }).observe(document.getElementById('main'), { subtree: true, childList: true, attributes: true, attributeFilter: ['data-cockpit'] });
})();

// The deck (live page only): keys typed while nothing is focused go to the deck, the URL
// fragment names the current section, and a #section link opens on that section.
(() => {
  'use strict';
  const KEYS = new Set(['ArrowLeft', 'ArrowRight', 'Home', 'End', '1', '2', '3', '4', '5', '6']);
  window.addEventListener('keydown', (e) => {
    if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    if (!KEYS.has(e.key)) return;
    const t = e.target;
    const deck = document.getElementById('deck');
    if (!deck) return;
    const bridge = document.getElementById('bridge');
    if (bridge && bridge.dataset.cockpit === 'hidden') return; // the captain is watching the sky
    if (t === document.body || t === document.documentElement || !t) {
      e.preventDefault();
      deck.focus({ preventScroll: true });
      deck.dispatchEvent(new KeyboardEvent('keydown', { key: e.key, bubbles: true }));
    } else if (bridge && bridge.contains(t) && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      e.preventDefault(); // the deck handles it (Rust); do not also scroll sideways
    }
  });
  const want = location.hash.replace(/^#(slide-)?/, '');
  let opened = !want;
  setInterval(() => {
    const bridge = document.getElementById('bridge');
    if (!bridge || !bridge.classList.contains('live')) return;
    if (!opened) {
      const b = document.querySelector(`.decknav button[data-slide="${CSS.escape(want)}"]`);
      opened = true;
      if (b) { b.click(); return; }
    }
    const id = bridge.dataset.slide;
    if (id && location.hash !== `#${id}`) {
      history.replaceState(null, '', `#${id}`);
      const tab = document.querySelector(`.decknav button[data-slide="${CSS.escape(id)}"]`);
      // keep the current tab in view on the phone's scrolling tab row; scrolling the row
      // (not scrollIntoView) leaves the Tab-key starting point alone
      const row = tab && tab.closest('ol');
      if (row && row.scrollWidth > row.clientWidth) {
        const t = tab.getBoundingClientRect(), r = row.getBoundingClientRect();
        row.scrollLeft += t.left - r.left - (r.width - t.width) / 2;
      }
    }
  }, 250);
})();
