// Preview page glue: the Copy buttons. The command goes to the clipboard in the page (the
// server never sees it); nothing is sent anywhere. Works for rows the LiveView adds later.
(() => {
  'use strict';
  const fallback = (text) => {
    const t = document.createElement('textarea');
    t.value = text; t.setAttribute('readonly', ''); t.style.position = 'fixed'; t.style.opacity = '0';
    document.body.appendChild(t); t.select();
    let ok = false; try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    t.remove(); return ok;
  };
  document.addEventListener('click', (e) => {
    const b = e.target instanceof Element && e.target.closest('button.copy[data-copy]');
    if (!b) return;
    e.preventDefault(); e.stopPropagation();
    const text = b.dataset.copy;
    const done = (ok) => {
      if (!ok) return;
      b.setAttribute('data-copied', '');
      clearTimeout(b.__t); b.__t = setTimeout(() => b.removeAttribute('data-copied'), 1600);
      window.__copied = text;
    };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(() => done(true), () => done(fallback(text)));
    } else done(fallback(text));
  }, true);
})();

// A drawer that opens takes the keyboard focus (on its close button), so Esc and Tab
// work from there; H still folds the whole cockpit away. When it closes, the focus goes
// back to what opened it (a crew diagram card), so Esc keeps working.
(() => {
  'use strict';
  let had = false, back = null;
  new MutationObserver(() => {
    const d = document.getElementById('drawer');
    if (d && !had) {
      back = document.activeElement;
      const c = d.querySelector('.dclose'); if (c) c.focus({ preventScroll: true });
    } else if (!d && had) {
      const g = document.getElementById('diagram');
      const to = back && back.isConnected ? back : g && g.querySelector('.dgm-close');
      if (to) to.focus({ preventScroll: true });
      back = null;
    }
    had = !!d;
  }).observe(document.getElementById('main'), { subtree: true, childList: true });
})();

// The crew diagram is laid out on the server for the screen width: report it (on load,
// when the diagram opens, after a resize, and each second while it is open) through a
// hidden input. When the diagram opens it takes the focus and scrolls the card it was
// opened for into view.
(() => {
  'use strict';
  let open = false, timer = 0;
  const report = () => {
    const i = document.getElementById('dgm-vw');
    if (!i) return;
    const w = String(Math.round(window.innerWidth));
    if (i.value === w) return;
    i.value = w;
    i.dispatchEvent(new Event('input', { bubbles: true }));
  };
  window.addEventListener('resize', () => { clearTimeout(timer); timer = setTimeout(report, 150); });
  // resize events ride on animation frames, which a busy galaxy can starve: check anyway
  setInterval(() => { if (open) report(); }, 1000);
  new MutationObserver(() => {
    report();
    const g = document.getElementById('diagram');
    if (g && !open) {
      const f = g.querySelector('.node.focus');
      if (f) { f.scrollIntoView({ block: 'center', inline: 'center' }); f.focus({ preventScroll: true }); }
      else {
        // a canvas wider than the screen opens centred on L1 (the middle of the canvas)
        const sc = g.querySelector('.dgm-scroll');
        if (sc) sc.scrollLeft = (sc.scrollWidth - sc.clientWidth) / 2;
        const c = g.querySelector('.dgm-close'); if (c) c.focus({ preventScroll: true });
      }
    } else if (!g && open) {
      const h = document.getElementById('crew-open'); if (h) h.focus({ preventScroll: true });
    }
    open = !!g;
  }).observe(document.getElementById('main'), { subtree: true, childList: true });
})();
