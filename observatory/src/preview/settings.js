// Settings overlay: every kernel setting with its explanation, opened by the gear beside Hide
// cockpit (or the , key). It reads GET /api/settings and writes POST /api/settings with the
// page's CSRF token (docs/design/settings-api.md). It lives outside the LiveView mount
// (#main), so opening and closing it never changes the cockpit. The views are pure functions
// of the state that return HTML (tests run them in node: observatory/tests/settings.mjs);
// every control names what it does in data-* attributes, read by one delegated handler.
(() => {
  'use strict';

  const SECTIONS = [
    ['routing', 'Routing', 'How the kernel picks a harness, model and reasoning effort for each task.'],
    ['sources', 'Sources', 'Where workers may read context from, and what stays out.'],
    ['projects', 'Projects', 'Each project’s own settings, from its project.toml.'],
    ['safety', 'Safety & autonomy', 'The guards that stop runaway or stuck work.'],
    ['kernel', 'Kernel & watchdog', 'Timers, ports and limits built into the kernel.'],
    ['overrides', 'Overrides', 'Environment variables that change behaviour. They are read when the kernel starts and win over the files.'],
  ];
  const SOURCE_TEXT = {
    default: ['default', 'Nothing sets this: the built-in default applies.'],
    file: ['file', 'Set in a configuration file.'],
    env: ['env', 'Set by an environment variable of the kernel process.'],
  };
  const BYPASS = 'bypasses your routing rules';
  // row fields that say what a rule does (the rest say when it matches)
  const OUTCOME = new Set(['role', 'reasoning', 'harness', 'model', 'effort', 'captain_fallback', 'reason']);
  const FIELD_NAMES = { ranks: 'Crew levels', kinds: 'Task kinds', judgment: 'Judgment', thoroughness: 'Thoroughness',
    role: 'Model profile', reasoning: 'Reasoning', reason: 'Reason', harness: 'Harness', model: 'Model', effort: 'Effort', captain_fallback: 'Captain fallback' };

  // ------------------------------------------------------------------ small helpers
  const esc = (v) => String(v).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
  const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
  const has = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
  const clone = (v) => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));
  const canon = (v) => JSON.stringify(v, (_k, x) => (x && typeof x === 'object' && !Array.isArray(x)
    ? Object.keys(x).sort().reduce((o, k) => { o[k] = x[k]; return o; }, {}) : x));
  const same = (a, b) => canon(a === undefined ? null : a) === canon(b === undefined ? null : b);
  const fieldName = (f) => FIELD_NAMES[f] || (f.charAt(0).toUpperCase() + f.slice(1)).replace(/_/g, ' ');
  // a row's fields in reading order: what it matches first, then what it does
  const ORDER = ['ranks', 'kinds', 'judgment', 'thoroughness', 'harness', 'model', 'role', 'reasoning', 'effort', 'reason'];
  const fieldsOf = (s) => Object.entries(s.fields || {}).sort(([a], [b]) => {
    const [i, j] = [ORDER.indexOf(a), ORDER.indexOf(b)];
    const rank = (k, x) => (x >= 0 ? x : OUTCOME.has(k) ? 50 : 4.5);
    return rank(a, i) - rank(b, j);
  });
  const isRows = (s) => (s.type === 'ordered-list' || s.type === 'table') && s.fields && typeof s.fields === 'object';
  // Model profiles edit in place; longer rules read as sentences.
  const sentenceRows = (s) => isRows(s) && (s.type === 'table' || (!s.key.startsWith('econ.profiles.') && Object.keys(s.fields).length > 3));

  /** A duration-ish integer in words, from its key's unit suffix ("" if it has none). */
  const unitWords = (key, n) => {
    if (typeof n !== 'number' || !Number.isFinite(n)) return '';
    const m = /_(ms|secs?|seconds|minutes|mins)$/.exec(key);
    if (!m) return '';
    const s = m[1] === 'ms' ? n / 1000 : /^min/.test(m[1]) ? n * 60 : n;
    if (s < 1) return `${n} ms`;
    if (s < 90) return `${+s.toFixed(1)} s`;
    if (s < 5400) return `${+(s / 60).toFixed(1)} min`;
    if (s < 172800) return `${+(s / 3600).toFixed(1)} h`;
    return `${+(s / 86400).toFixed(1)} days`;
  };

  /** A value in plain words (never a bare "?"). */
  const words = (s, v) => {
    if (v === null || v === undefined) return 'not set';
    if (typeof v === 'boolean') return v ? 'on' : 'off';
    if (typeof v === 'number') { const u = unitWords(s.key, v); return u && u !== `${v} ms` ? `${v} (${u})` : String(v); }
    if (typeof v === 'string') return v === '' ? 'empty' : v;
    if (Array.isArray(v)) {
      if (!v.length) return 'none';
      if (v.every((x) => x === null || typeof x !== 'object')) return v.join(', ');
      if (isRows(s) && !sentenceRows(s)) return v.map((r) => rowWords(s, r)).join(' → ');
      return plural(v.length, s.type === 'table' ? 'row' : 'rule');
    }
    return Object.entries(v).map(([k, x]) => `${k}: ${Array.isArray(x) ? x.join(', ') : x}`).join(' · ');
  };

  /** A short row: its field values in order ("claude newest opus"). */
  const rowWords = (s, r) => fieldsOf(s).map(([f]) => r && r[f]).filter((x) => x !== undefined && x !== null && x !== '')
    .map((x) => (Array.isArray(x) ? x.join(', ') : x)).join(' ') || 'empty row';

  /** A rule as a sentence: "kind decide and judgment high → judge · decision task". */
  const sentence = (s, r) => {
    r = r || {};
    const cond = [], out = [];
    for (const [f, d] of fieldsOf(s)) {
      const v = r[f];
      if (v === undefined || v === null || v === '' || (Array.isArray(v) && !v.length) || f === 'reason') continue;
      const name = f === 'ranks' ? (v.length === 1 ? 'level' : 'levels') : f === 'kinds' ? (v.length === 1 ? 'kind' : 'kinds')
        : fieldName(f).toLowerCase();
      const val = Array.isArray(v) ? v.join(' or ') : String(v);
      if (OUTCOME.has(f)) out.push(f === 'role' ? val : f === 'reasoning' ? `${val} reasoning` : `${name} ${val}`);
      else cond.push(`${name} ${val}`);
      void d;
    }
    const when = cond.length ? cond.join(' and ') : 'any task';
    return { when, then: out.length ? out.join(', ') : 'no change', reason: r.reason ? String(r.reason) : '' };
  };

  /** "econ.toml › policy.rules", "source[0].id" style. */
  const where = (s) => {
    if (s.file) {
      const p = Array.isArray(s.toml_path) ? s.toml_path.reduce((a, x) => (typeof x === 'number' ? `${a}[${x}]` : a ? `${a}.${x}` : String(x)), '') : '';
      return p ? `${s.file} › ${p}` : s.file;
    }
    if (s.section === 'overrides' || /^overrides\./.test(s.key)) return `environment › ${s.key.replace(/^overrides\./, '')}`;
    return 'built into the kernel';
  };

  /** The explanation, with its "Example:" on a line of its own. */
  const explain = (text) => {
    const t = String(text || '');
    const i = t.search(/\bExample:/);
    if (i < 0) return `<p class="st-expl">${esc(t)}</p>`;
    return `<p class="st-expl">${esc(t.slice(0, i).trim())}</p><p class="st-ex">${esc(t.slice(i).trim())}</p>`;
  };

  const ago = (now, at) => {
    if (typeof at !== 'number' || !at) return 'never';
    const s = Math.max(0, Math.round((now - at) / 1000));
    if (s < 45) return 'just now';
    if (s < 5400) return `${Math.max(1, Math.round(s / 60))}m ago`;
    if (s < 172800) return `${Math.round(s / 3600)}h ago`;
    return `${Math.round(s / 86400)}d ago`;
  };

  // ------------------------------------------------------------------ state
  /** The empty state: nothing loaded, nothing staged. */
  const initial = () => ({
    data: null, error: null, loading: false, now: Date.now(),
    section: 'routing', tab: 'settings',
    staged: {},   // key → new value
    invalid: {},  // key → why the staged value cannot be saved
    errors: {},   // key → the kernel's refusal
    editing: null, // "key#row" whose edit form is open
    drafts: {},   // key → text typed into a chip input
    review: false, confirm: false, saving: false,
    reverting: null, revertConfirm: false, revertError: null, // the history row being reverted
    toast: null,  // { text, tone } shown after a save or revert
    csrf: true,   // the page carries a write token (meta unvrs-settings-csrf)
  });

  const schemaOf = (st) => (st.data && Array.isArray(st.data.schema) ? st.data.schema : []);
  const entries = (st, section) => schemaOf(st).filter((s) => s.section === section);
  const find = (st, key) => schemaOf(st).find((s) => s.key === key);
  const saved = (st, key) => (st.data && st.data.values && has(st.data.values, key) ? st.data.values[key] : null);
  const current = (st, key) => (has(st.staged, key) ? st.staged[key] : saved(st, key));

  /** Client-side checks; the kernel validates again on save. */
  const check = (s, v) => {
    if (s.type === 'int') {
      if (typeof v !== 'number' || !Number.isInteger(v)) return 'Use a whole number.';
      if (s.range && typeof s.range.min === 'number' && v < s.range.min) return `Use ${s.range.min} or more.`;
      if (s.range && typeof s.range.max === 'number' && v > s.range.max) return `Use ${s.range.max} or less.`;
    }
    if (s.type === 'enum' && Array.isArray(s.allowed) && !s.allowed.some((a) => same(a, v))) return `Choose one of: ${s.allowed.join(', ')}.`;
    if ((s.type === 'string' || s.type === 'path') && typeof v !== 'string') return 'Enter some text.';
    if (s.type === 'path' && typeof v === 'string' && !v.trim()) return 'Enter a path.';
    if (isRows(s) && Array.isArray(v)) {
      for (let i = 0; i < v.length; i++) {
        for (const [f, d] of Object.entries(s.fields)) {
          const x = v[i] && v[i][f];
          if (d.required && (x === undefined || x === null || x === '' || (Array.isArray(x) && !x.length))) {
            return `Row ${i + 1} needs ${fieldName(f).toLowerCase()}.`;
          }
        }
      }
    }
    return null;
  };

  /** Stages `v` for `key` (or unstages it when it equals the saved value). */
  const stage = (st, key, v) => {
    const s = find(st, key);
    if (!s || !s.editable) return st;
    delete st.errors[key];
    if (same(v, saved(st, key))) { delete st.staged[key]; delete st.invalid[key]; return st; }
    st.staged[key] = v;
    const why = check(s, v);
    if (why) st.invalid[key] = why; else delete st.invalid[key];
    return st;
  };

  // ------------------------------------------------------------------ views
  const badge = (cls, text, title) => `<span class="st-badge ${cls}"${title ? ` title="${esc(title)}"` : ''}>${esc(text)}</span>`;

  /** The section rail: one button per section with its count (and staged changes). */
  const rail = (st) => {
    const items = SECTIONS.map(([id, label]) => {
      const list = entries(st, id);
      const staged = list.filter((s) => has(st.staged, s.key)).length;
      const cur = st.tab === 'settings' && !st.review && st.section === id;
      return `<li><button type="button" class="st-sec${id === 'overrides' ? ' st-amber' : ''}" data-sec="${id}"${cur ? ' aria-current="true"' : ''}>`
        + `<span class="st-sl">${esc(label)}</span>`
        + (staged ? `<span class="st-staged" title="${plural(staged, 'staged change')}">●${staged}</span>` : '')
        + `<span class="st-n" aria-label="${plural(list.length, 'setting')}">${list.length}</span></button></li>`;
    }).join('');
    return `<nav class="st-rail" aria-label="Settings sections"><ul>${items}</ul></nav>`;
  };

  const fid = (...p) => esc(p.join('|'));

  /** A segmented control or a select for an enum (`opts` [[value, text]]). */
  const choice = (key, name, value, opts, extra = '') => {
    if (opts.length <= 4 && opts.every(([, t]) => String(t).length <= 12)) {
      return `<div class="st-seg" role="radiogroup" aria-label="${esc(name)}">` + opts.map(([v, t]) => {
        const on = same(v, value);
        return `<button type="button" role="radio" aria-checked="${on}" class="st-opt" data-op="set" data-k="${esc(key)}"${extra}`
          + ` data-v="${esc(canon(v))}" data-fid="${fid(key, 'opt', canon(v), extra)}">${esc(t)}</button>`;
      }).join('') + `</div>`;
    }
    return `<select class="st-select" data-op="set" data-k="${esc(key)}"${extra} aria-label="${esc(name)}" data-fid="${fid(key, 'sel', extra)}">`
      + opts.map(([v, t]) => `<option value="${esc(canon(v))}"${same(v, value) ? ' selected' : ''}>${esc(t)}</option>`).join('') + `</select>`;
  };

  /** Chips for a list; `allowed` makes them toggles, else free text with an add box. */
  const chips = (st, key, name, list, allowed, extra = '') => {
    list = Array.isArray(list) ? list : [];
    if (Array.isArray(allowed)) {
      return `<div class="st-chips" role="group" aria-label="${esc(name)}">` + allowed.map((a) => {
        const on = list.some((x) => same(x, a));
        return `<button type="button" class="st-chip st-toggle" aria-pressed="${on}" data-op="toggle" data-k="${esc(key)}"${extra}`
          + ` data-v="${esc(canon(a))}" data-fid="${fid(key, 'tog', canon(a), extra)}">${esc(a)}</button>`;
      }).join('') + `</div>`;
    }
    const draft = st.drafts[key] || '';
    return `<div class="st-chips" role="group" aria-label="${esc(name)}">`
      + (list.length ? '' : `<span class="st-none">none</span>`)
      + list.map((x, i) => `<span class="st-chip">${esc(x)}<button type="button" class="st-x" data-op="unchip" data-k="${esc(key)}"`
        + ` data-i="${i}" aria-label="Remove ${esc(x)}" data-fid="${fid(key, 'unchip', i)}">×</button></span>`).join('')
      + `<span class="st-add"><input type="text" class="st-input st-mono" data-op="draft" data-k="${esc(key)}" value="${esc(draft)}"`
      + ` placeholder="add…" aria-label="Add to ${esc(name)}" spellcheck="false" data-fid="${fid(key, 'draft')}">`
      + `<button type="button" class="st-btn" data-op="chip" data-k="${esc(key)}" data-fid="${fid(key, 'chip')}">Add</button></span></div>`;
  };

  /** One field of a row (in a profile row or a rule's edit form). */
  const rowField = (st, s, i, f, d, v) => {
    const ex = ` data-i="${i}" data-f="${esc(f)}" title="${esc(d.explanation || '')}"`;
    const name = `${fieldName(f)}, row ${i + 1}`;
    if (d.type === 'list') return chips(st, s.key, name, v, d.allowed, ex);
    if (d.type === 'enum' && Array.isArray(d.allowed)) {
      const none = d.required ? [] : [[null, OUTCOME.has(f) ? 'not set' : 'any']];
      return choice(s.key, name, v === undefined ? null : v, none.concat(d.allowed.map((a) => [a, String(a)])), ex);
    }
    if (d.type === 'bool') {
      return `<button type="button" role="switch" class="st-switch" aria-checked="${!!v}" aria-label="${esc(name)}" data-op="rowbool"`
        + ` data-k="${esc(s.key)}"${ex} data-fid="${fid(s.key, 'rb', i, f)}"><span class="st-knob"></span><span class="st-sw">${esc(fieldName(f))}: ${v ? 'on' : 'off'}</span></button>`;
    }
    const num = d.type === 'int';
    return `<input type="${num ? 'number' : 'text'}" class="st-input${d.type === 'path' || f === 'model' ? ' st-mono' : ''}" data-op="rowset" data-k="${esc(s.key)}"${ex}`
      + ` value="${esc(v === undefined || v === null ? '' : v)}" aria-label="${esc(name)}" spellcheck="false"${num ? ' step="1"' : ''}`
      + `${d.required ? ' required' : ''} data-fid="${fid(s.key, 'rf', i, f)}">`;
  };

  const moveButtons = (s, i, n) => `<span class="st-move">`
    + `<button type="button" class="st-ib" data-op="up" data-k="${esc(s.key)}" data-i="${i}" aria-label="Move row ${i + 1} up"${i === 0 ? ' disabled' : ''} data-fid="${fid(s.key, 'up', i)}">↑</button>`
    + `<button type="button" class="st-ib" data-op="down" data-k="${esc(s.key)}" data-i="${i}" aria-label="Move row ${i + 1} down"${i === n - 1 ? ' disabled' : ''} data-fid="${fid(s.key, 'down', i)}">↓</button>`
    + `<button type="button" class="st-ib st-del" data-op="del" data-k="${esc(s.key)}" data-i="${i}" aria-label="Remove row ${i + 1}" data-fid="${fid(s.key, 'del', i)}">×</button></span>`;

  /** Ordered rows: draggable, with up/down; short rows edit in place, long ones as sentences + form. */
  const rows = (st, s, v) => {
    v = Array.isArray(v) ? v : [];
    const long = sentenceRows(s), ordered = s.type === 'ordered-list';
    const items = v.map((r, i) => {
      const open = st.editing === `${s.key}#${i}`;
      const drag = ordered ? ` draggable="true"` : '';
      let body;
      if (long) {
        const t = sentence(s, r);
        body = `<span class="st-sent"><span class="st-when">${esc(t.when)}</span> <span class="st-arrow" aria-label="then">→</span> `
          + `<strong class="st-then">${esc(t.then)}</strong>${t.reason ? ` <span class="st-why">· ${esc(t.reason)}</span>` : ''}</span>`
          + `<button type="button" class="st-btn" data-op="edit" data-k="${esc(s.key)}" data-i="${i}" aria-expanded="${open}" data-fid="${fid(s.key, 'edit', i)}">${open ? 'Done' : 'Edit'}</button>`;
      } else {
        body = `<span class="st-cells">` + fieldsOf(s).map(([f, d]) => rowField(st, s, i, f, d, r && r[f])).join('') + `</span>`;
      }
      const form = long && open
        ? `<div class="st-form" role="group" aria-label="Edit row ${i + 1}">` + fieldsOf(s).map(([f, d]) => `<div class="st-ff">`
          + `<span class="st-fl">${esc(fieldName(f))}${d.required ? '' : ' <span class="st-opt-t">optional</span>'}</span>`
          + rowField(st, s, i, f, d, r && r[f]) + `<span class="st-fx">${esc(d.explanation || '')}</span></div>`).join('') + `</div>`
        : '';
      return `<li class="st-row${open ? ' st-open' : ''}" data-k="${esc(s.key)}" data-i="${i}">`
        + `<span class="st-rn${ordered ? ' st-grip' : ''}" aria-hidden="true"${drag}${ordered ? ' title="Drag to reorder"' : ''}>${ordered ? '⠿' : ''}${i + 1}</span>${body}${ordered || long ? moveButtons(s, i, v.length) : ''}${form}</li>`;
    }).join('');
    return `<ol class="st-rows${long ? ' st-long' : ''}" aria-label="${esc(s.label)}">${items || '<li class="st-none">No rows.</li>'}</ol>`
      + `<button type="button" class="st-btn st-addrow" data-op="addrow" data-k="${esc(s.key)}" data-fid="${fid(s.key, 'addrow')}">+ Add ${long ? 'rule' : 'row'}</button>`;
  };

  /** The editor for one editable setting. */
  const editor = (st, s) => {
    const v = current(st, s.key);
    const lbl = `lbl-${s.key}`;
    switch (s.type) {
      case 'enum':
        return choice(s.key, s.label, v, (Array.isArray(s.allowed) ? s.allowed : []).map((a) => [a, String(a)]));
      case 'bool':
        return `<button type="button" role="switch" class="st-switch" aria-checked="${!!v}" aria-labelledby="${esc(lbl)}" data-op="bool"`
          + ` data-k="${esc(s.key)}" data-fid="${fid(s.key, 'bool')}"><span class="st-knob"></span><span class="st-sw">${v ? 'on' : 'off'}</span></button>`;
      case 'int': {
        const r = s.range || {};
        const big = typeof r.max === 'number' && r.max > 1e9;
        const hint = typeof r.min === 'number' && typeof r.max === 'number' && !big ? `${r.min} – ${r.max}`
          : typeof r.min === 'number' ? `at least ${r.min}` : typeof r.max === 'number' ? `at most ${r.max}` : 'whole number';
        const u = unitWords(s.key, v);
        return `<span class="st-num"><input type="number" class="st-input st-mono" step="1" data-op="int" data-k="${esc(s.key)}"`
          + `${typeof r.min === 'number' ? ` min="${r.min}"` : ''}${typeof r.max === 'number' ? ` max="${r.max}"` : ''}`
          + ` value="${esc(v === null || v === undefined ? '' : v)}" aria-labelledby="${esc(lbl)}" data-fid="${fid(s.key, 'int')}">`
          + `<span class="st-hint">${esc(hint)}${u ? ` · ${esc(u)}` : ''}</span></span>`;
      }
      case 'string': case 'path':
        return `<input type="text" class="st-input st-wide${s.type === 'path' ? ' st-mono' : ''}" data-op="text" data-k="${esc(s.key)}"`
          + ` value="${esc(v === null || v === undefined ? '' : v)}" aria-labelledby="${esc(lbl)}" spellcheck="false" data-fid="${fid(s.key, 'text')}">`;
      case 'list':
        return chips(st, s.key, s.label, v, s.allowed);
      case 'ordered-list': case 'table':
        if (isRows(s)) return rows(st, s, v);
        return chips(st, s.key, s.label, v, s.allowed);
      default:
        return `<code class="st-val">${esc(words(s, v))}</code>`;
    }
  };

  /** A read-only value, in words (rows as sentences or chains). */
  const readOnly = (st, s) => {
    const v = current(st, s.key);
    if (isRows(s) && Array.isArray(v) && v.length) {
      return `<ol class="st-rows st-ro-rows">` + v.map((r, i) => {
        if (!sentenceRows(s)) return `<li class="st-row"><span class="st-rn">${i + 1}</span><span class="st-mono">${esc(rowWords(s, r))}</span></li>`;
        const t = sentence(s, r);
        return `<li class="st-row"><span class="st-rn">${i + 1}</span><span class="st-sent">${esc(t.when)} → <strong>${esc(t.then)}</strong>${t.reason ? ` · ${esc(t.reason)}` : ''}</span></li>`;
      }).join('') + `</ol>`;
    }
    return `<code class="st-val">${esc(words(s, v))}</code>`;
  };

  /** One setting card. */
  const card = (st, s) => {
    const v = current(st, s.key), saved0 = saved(st, s.key);
    const src = (st.data && st.data.effective_source && st.data.effective_source[s.key]) || 'default';
    const [srcText, srcTip] = SOURCE_TEXT[src] || [String(src), ''];
    const dirty = has(st.staged, s.key);
    const bypass = String(s.explanation || '').includes(BYPASS) || (s.section === 'overrides' && /_MODEL$/.test(s.key));
    const amber = s.section === 'overrides';
    const differs = s.default !== undefined && !same(v, s.default);
    const badges = [
      badge(`st-src st-src-${esc(src)}`, srcText, srcTip),
      badge(s.apply === 'restart' ? 'st-restart' : 'st-live', s.apply === 'restart' ? 'after restart' : 'applies now',
        s.apply === 'restart' ? 'Takes effect when the kernel restarts.' : 'Takes effect for the next decisions; running tasks keep theirs.'),
      s.sensitive ? badge('st-sens', 'sensitive', 'Changing this can weaken safeguards; saving asks you to confirm.') : '',
      bypass && v !== null && v !== undefined && v !== '' ? badge('st-bypass', BYPASS) : '',
      s.editable ? '' : badge('st-ro', 'read-only in v1', 'Shown for reference; this version only edits routing and sources.'),
    ].join('');
    const err = st.invalid[s.key] || st.errors[s.key];
    return `<article class="st-card${dirty ? ' st-dirty' : ''}${s.editable ? '' : ' st-rocard'}${amber ? ' st-amber' : ''}" data-key="${esc(s.key)}" aria-labelledby="${esc(`lbl-${s.key}`)}">`
      + `<div class="st-ch"><h4 class="st-label" id="${esc(`lbl-${s.key}`)}">${esc(s.label)}</h4><span class="st-badges">${badges}</span></div>`
      + explain(s.explanation)
      + `<div class="st-ed">${s.editable ? editor(st, s) : readOnly(st, s)}</div>`
      + (err ? `<p class="st-err" role="alert">${esc(err)}</p>` : '')
      + `<div class="st-cf">`
      + (dirty ? `<span class="st-was">was <code>${esc(words(s, saved0))}</code></span><button type="button" class="st-link" data-op="undo" data-k="${esc(s.key)}" data-fid="${fid(s.key, 'undo')}">Undo</button>` : '')
      + (s.default !== undefined
        ? `<span class="st-def">default <code>${esc(words(s, s.default))}</code></span>` : '')
      + (s.editable && differs && s.default !== null
        ? `<button type="button" class="st-link" data-op="reset" data-k="${esc(s.key)}" data-fid="${fid(s.key, 'reset')}">Reset to default</button>` : '')
      + `<code class="st-where" title="Where this setting lives">${esc(where(s))}</code></div></article>`;
  };

  /** Settings named `<group>.<id>.<field>` (sources, projects) grouped per record. */
  const groups = (list) => {
    const out = [];
    for (const s of list) {
      const m = /^(sources|projects)\.(.+)\.([^.]+)$/.exec(s.key);
      const id = m ? m[2] : '';
      let g = out.find((x) => x.id === id);
      if (!g) { g = { id, items: [] }; out.push(g); }
      g.items.push(s);
    }
    return out;
  };

  /** A source in one line: "handbook — git at ~/handbook · internal · excludes private/**". */
  const recordWords = (st, g) => {
    const val = (f) => { const s = g.items.find((x) => x.key.endsWith(`.${f}`)); return s ? current(st, s.key) : undefined; };
    const bits = [];
    const kind = val('kind'), uri = val('uri');
    if (kind || uri) bits.push([kind, uri ? `at ${uri}` : ''].filter(Boolean).join(' '));
    if (val('sensitivity')) bits.push(String(val('sensitivity')));
    const ex = val('exclude');
    if (Array.isArray(ex) && ex.length) bits.push(`excludes ${ex.join(', ')}`);
    if (val('purpose')) bits.push(String(val('purpose')));
    return bits.join(' · ');
  };

  /** The live harness catalog (read-only): models, efforts, sign-in and age per harness. */
  const catalog = (st) => {
    const c = st.data && st.data.live_catalog;
    if (!c || typeof c !== 'object' || !Object.keys(c).length) return '';
    const hs = Object.entries(c).map(([h, e]) => {
      e = e || {};
      const models = Array.isArray(e.models) ? e.models : [];
      const signed = e.signed_in === true ? badge('st-ok', 'signed in') : e.signed_in === false ? badge('st-bad', 'not signed in') : badge('st-ro', 'sign-in unknown');
      const quota = e.quota_available === false ? badge('st-bad', 'no quota') : e.quota_available === true ? badge('st-ok', 'quota available') : '';
      const rows = models.map((m) => `<tr><td class="st-mono">${esc(m.id || 'unnamed')}${m.is_default ? ' <span class="st-dflt">default</span>' : ''}</td>`
        + `<td>${(Array.isArray(m.efforts) && m.efforts.length ? m.efforts.map((x) => `<span class="st-eff">${esc(x)}</span>`).join('') : '<span class="st-none">none reported</span>')}</td>`
        + `<td>${m.quota_available === false ? `<span class="st-badtext">${esc(m.quota_error || 'no quota')}</span>` : 'ok'}</td></tr>`).join('');
      return `<section class="st-harness" aria-label="${esc(h)}"><div class="st-hh"><h5>${esc(h)}</h5>${signed}${quota}`
        + `<span class="st-age" title="${esc(e.source ? `source: ${e.source}` : '')}">refreshed ${esc(ago(st.now, e.observed_at))}</span></div>`
        + (e.error ? `<p class="st-err">${esc(e.error)}</p>` : '')
        + (models.length ? `<table class="st-table"><thead><tr><th>Model</th><th>Efforts</th><th>Quota</th></tr></thead><tbody>${rows}</tbody></table>`
          : `<p class="st-none">No models reported.</p>`) + `</section>`;
    }).join('');
    return `<section class="st-catalog" aria-labelledby="st-cat-h"><h4 class="st-sub" id="st-cat-h">Live model catalog <span class="st-ro-t">read-only</span></h4>`
      + `<p class="st-lede">What each harness offers right now; the routing rules above choose from it.</p>${hs}</section>`;
  };

  /** The main pane for the current section. */
  const pane = (st) => {
    if (st.error) return `<p class="st-msg st-err" role="alert">${esc(st.error)}</p>`;
    if (!st.data) return `<p class="st-msg" role="status">Loading settings from the kernel…</p>`;
    if (st.tab === 'history') return history(st);
    if (st.review && Object.keys(st.staged).length) return review(st);
    const [id, label, lede] = SECTIONS.find(([s]) => s === st.section) || SECTIONS[0];
    const list = entries(st, id);
    let body;
    if (!list.length) body = `<p class="st-msg">No settings in this section.</p>`;
    else if (id === 'sources' || id === 'projects') {
      body = groups(list).map((g) => `<section class="st-group" aria-label="${esc(g.id)}"><h4 class="st-gh"><span class="st-gid">${esc(g.id)}</span>`
        + `<span class="st-gsum">${esc(recordWords(st, g))}</span></h4>${g.items.map((s) => card(st, s)).join('')}</section>`).join('');
    } else body = list.map((s) => card(st, s)).join('');
    const warn = id === 'overrides' ? `<p class="st-warnline" role="note">Overrides are environment variables; a model override ${BYPASS}. Change them where the kernel is started.</p>` : '';
    return `<h3 class="st-head${id === 'overrides' ? ' st-amber' : ''}" id="st-sec-title">${esc(label)}</h3><p class="st-lede">${esc(lede)}</p>${warn}`
      + body + (id === 'routing' ? catalog(st) : '');
  };

  /** A value for before → after: rows as numbered sentences, the rest in words. */
  const shown = (s, v) => {
    if (s && isRows(s) && Array.isArray(v) && v.length) {
      return `<ol class="st-mini">` + v.map((r) => {
        if (!sentenceRows(s)) return `<li>${esc(rowWords(s, r))}</li>`;
        const t = sentence(s, r);
        return `<li>${esc(t.when)} → <strong>${esc(t.then)}</strong>${t.reason ? ` · ${esc(t.reason)}` : ''}</li>`;
      }).join('') + `</ol>`;
    }
    return `<code>${esc(s ? words(s, v) : (v === null || v === undefined ? 'not set' : typeof v === 'object' ? JSON.stringify(v) : String(v)))}</code>`;
  };

  /** The staged changes, in schema order. */
  const changes = (st) => schemaOf(st).filter((s) => has(st.staged, s.key));
  const needsConfirm = (st) => changes(st).some((s) => s.sensitive);

  /** Review: each staged change before → after; sensitive ones need the confirm box. */
  const review = (st) => {
    const list = changes(st);
    const sens = list.filter((s) => s.sensitive);
    const items = list.map((s) => {
      const err = st.invalid[s.key] || st.errors[s.key];
      return `<li class="st-rv${s.sensitive ? ' st-rv-sens' : ''}"><div class="st-rvh"><strong>${esc(s.label)}</strong>`
        + `<code class="st-where">${esc(where(s))}</code>${s.sensitive ? badge('st-sens', 'sensitive') : ''}`
        + badge(s.apply === 'restart' ? 'st-restart' : 'st-live', s.apply === 'restart' ? 'after restart' : 'applies now')
        + `<button type="button" class="st-link" data-op="undo" data-k="${esc(s.key)}" data-fid="${fid(s.key, 'rvundo')}">Drop this change</button></div>`
        + `<div class="st-ba"><div class="st-b"><span class="st-bal">before</span>${shown(s, saved(st, s.key))}</div>`
        + `<span class="st-arrow" aria-hidden="true">→</span><div class="st-a"><span class="st-bal">after</span>${shown(s, st.staged[s.key])}</div></div>`
        + (err ? `<p class="st-err" role="alert">${esc(err)}</p>` : '') + `</li>`;
    }).join('');
    const box = sens.length
      ? `<label class="st-confirm"><input type="checkbox" data-act="confirm"${st.confirm ? ' checked' : ''}> `
        + `I understand ${sens.length === 1 ? 'this change' : `these ${sens.length} changes`} can weaken safeguards `
        + `(${esc(sens.map((s) => s.label).join(', '))}) and want to save ${sens.length === 1 ? 'it' : 'them'}.</label>` : '';
    return `<h3 class="st-head" id="st-sec-title">Review ${plural(list.length, 'change')}</h3>`
      + `<p class="st-lede">Nothing is written until you press Save. Each change is checked by the kernel; a refused one stays here with the reason.</p>`
      + `<ul class="st-rvl">${items}</ul>${box}`
      + `<button type="button" class="st-btn" data-act="back">Back to settings</button>`;
  };

  const when = (now, at) => {
    if (typeof at !== 'number') return 'time unknown';
    const d = new Date(at);
    const pad = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())} · ${ago(now, at)}`;
  };

  /** Whether a history record can be reverted now (and why not). */
  const revertable = (st, c) => {
    const s = find(st, c.key);
    if (!s) return [false, 'This setting no longer exists.'];
    if (!s.editable) return [false, 'Read-only in v1.'];
    if (!same(saved(st, c.key), c.after)) return [false, 'Changed again since; revert the newer change first.'];
    return [true, ''];
  };

  /** History: every saved change, newest first, each with Revert. */
  const history = (st) => {
    const h = Array.isArray(st.data.history) ? st.data.history.slice().sort((a, b) => (b.at || 0) - (a.at || 0)) : [];
    const items = h.map((c) => {
      const s = find(st, c.key);
      const [ok, why] = revertable(st, c);
      const open = st.reverting === c.change_id;
      const orig = c.reverts ? h.find((x) => x.change_id === c.reverts) : null;
      return `<li class="st-hi${open ? ' st-open' : ''}" data-change="${esc(c.change_id)}"><div class="st-rvh"><strong>${esc(s ? s.label : c.key)}</strong>`
        + `<code class="st-where">${esc(s ? where(s) : c.key)}</code>`
        + `<span class="st-hw">${esc(when(st.now, c.at))} · by ${esc(c.by || 'unknown caller')}</span>`
        + (c.reverts ? badge('st-ro', orig ? `reverts the change of ${when(st.now, orig.at).split(' · ')[0]}` : 'a revert') : '')
        + (ok ? `<button type="button" class="st-btn" data-act="revert" data-change="${esc(c.change_id)}" aria-expanded="${open}">Revert</button>`
          : `<span class="st-hint" title="${esc(why)}">${esc(why)}</span>`) + `</div>`
        + `<div class="st-ba"><div class="st-b"><span class="st-bal">before</span>${shown(s, c.before)}</div>`
        + `<span class="st-arrow" aria-hidden="true">→</span><div class="st-a"><span class="st-bal">after</span>${shown(s, c.after)}</div></div>`
        + (open ? `<div class="st-rvform" role="group" aria-label="Revert this change"><span>Put <strong>${esc(s ? s.label : c.key)}</strong> back to ${shown(s, c.before)}?</span>`
          + (s && s.sensitive ? `<label class="st-confirm"><input type="checkbox" data-act="revconfirm"${st.revertConfirm ? ' checked' : ''}> I understand this can weaken safeguards.</label>` : '')
          + `<span class="st-rvbtns"><button type="button" class="st-btn" data-act="revcancel">Cancel</button>`
          + `<button type="button" class="st-btn st-primary" data-act="revgo" data-change="${esc(c.change_id)}"${(s && s.sensitive && !st.revertConfirm) || st.saving || !st.csrf ? ' disabled' : ''}>Revert</button></span>`
          + (st.revertError ? `<p class="st-err" role="alert">${esc(st.revertError)}</p>` : '') + `</div>` : '')
        + `</li>`;
    }).join('');
    return `<h3 class="st-head" id="st-sec-title">History</h3><p class="st-lede">Every saved change, newest first. Revert puts that one setting back and records a new change.</p>`
      + (h.length ? `<ul class="st-rvl">${items}</ul>` : `<p class="st-msg">No changes saved yet.</p>`);
  };

  /** The sticky footer: N changes · Review · Save (only while something is staged). */
  const footer = (st) => {
    const n = Object.keys(st.staged).length;
    if (!n || !st.data) return '';
    const bad = Object.keys(st.invalid).filter((k) => has(st.staged, k)).length;
    const blocked = bad > 0 || st.saving || !st.csrf || (st.review && needsConfirm(st) && !st.confirm);
    const note = !st.csrf ? 'Saving works only on the Observatory at http://unvrs.localhost:7576.'
      : bad ? `${plural(bad, 'change')} to fix before saving.`
      : needsConfirm(st) && !st.confirm ? 'Includes a sensitive change: Save asks you to confirm it.' : '';
    return `<footer class="st-foot" role="region" aria-label="Staged changes"><span class="st-fn"><strong>${plural(n, 'change')}</strong>`
      + (note ? `<span class="st-fnote">${esc(note)}</span>` : '') + `</span>`
      + `<button type="button" class="st-btn" data-act="discard"${st.saving ? ' disabled' : ''}>Discard</button>`
      + `<button type="button" class="st-btn" data-act="review" aria-pressed="${!!st.review}"${st.saving ? ' disabled' : ''}>${st.review ? 'Back' : 'Review'}</button>`
      + `<button type="button" class="st-btn st-primary" data-act="save"${blocked ? ' disabled' : ''}>${st.saving ? 'Saving…' : 'Save'}</button></footer>`;
  };

  /** The POST bodies that save the staged changes (contract: one settings.set per key). */
  const requests = (st) => changes(st).map((s) => ({ op: 'settings.set', key: s.key, value: st.staged[s.key], confirm: !!(s.sensitive && st.confirm) }));

  /** The kernel's refusal in plain words. */
  const refusal = (status, body) => {
    const e = body && typeof body.error === 'string' && body.error.trim() ? body.error.trim() : '';
    if (status === 403) return `The kernel refused this page${e ? `: ${e}` : ''}. Reload the Observatory at http://unvrs.localhost:7576 and try again.`;
    if (status === 409) return e || 'This setting changed since; revert the newer change first.';
    if (status >= 500) return `The kernel could not write the change${e ? `: ${e}` : ''}.`;
    return e || `The kernel refused the change (HTTP ${status}).`;
  };

  const summary = (st) => {
    if (!st.data) return '';
    const n = schemaOf(st).length;
    const ed = schemaOf(st).filter((s) => s.editable).length;
    return `${plural(n, 'setting')} · ${ed} editable`;
  };

  /** The whole overlay. */
  const view = (st) => `<div class="st-bar">`
    + `<h2 id="st-title">Settings</h2><span class="st-sum">${esc(summary(st))}</span>`
    + `<div class="st-tabs" role="tablist" aria-label="View">`
    + `<button type="button" class="st-tab" role="tab" data-tab="settings" aria-selected="${st.tab === 'settings'}">Settings</button>`
    + `<button type="button" class="st-tab" role="tab" data-tab="history" aria-selected="${st.tab === 'history'}">History</button></div>`
    + `<kbd class="st-key" aria-hidden="true">Esc</kbd>`
    + `<button type="button" class="st-close" data-act="close" aria-label="Close settings (Esc)">×</button></div>`
    + `<div class="st-body">${rail(st)}<div class="st-main"><div class="st-pane" id="st-pane" tabindex="-1">${pane(st)}</div>${footer(st)}</div></div>`
    + (st.toast ? `<div class="st-toast st-${esc(st.toast.tone || 'ok')}" role="status">${esc(st.toast.text)}</div>` : '');

  // ------------------------------------------------------------------ edits (pure: state, control → state)
  const parse = (t) => { try { return JSON.parse(t); } catch (e) { return t; } };

  /** Applies one control's action (`a`: its data-* plus the input's value) to the staged state. */
  const act = (st, a) => {
    const s = find(st, a.k);
    if (!s || !s.editable) return st;
    const base = clone(current(st, s.key));
    const i = a.i === undefined ? -1 : Number(a.i);
    const rowsOf = () => (Array.isArray(base) ? base : []);
    const setRow = (list) => stage(st, s.key, list);
    const inRow = a.f !== undefined && i >= 0;
    switch (a.op) {
      case 'set': {
        const v = parse(a.v !== undefined ? a.v : a.value);
        if (inRow) { const l = rowsOf(); l[i] = { ...(l[i] || {}) }; if (v === null || v === '') delete l[i][a.f]; else l[i][a.f] = v; return setRow(l); }
        return stage(st, s.key, v);
      }
      case 'bool': return stage(st, s.key, !base);
      case 'rowbool': { const l = rowsOf(); l[i] = { ...(l[i] || {}), [a.f]: !(l[i] || {})[a.f] }; return setRow(l); }
      case 'int': {
        const t = String(a.value).trim();
        const n = t === '' ? NaN : Number(t);
        if (!Number.isInteger(n)) { st.staged[s.key] = t; st.invalid[s.key] = 'Use a whole number.'; return st; }
        return stage(st, s.key, n);
      }
      case 'text': return stage(st, s.key, String(a.value));
      case 'rowset': {
        const d = s.fields[a.f] || {};
        const l = rowsOf(); l[i] = { ...(l[i] || {}) };
        const t = String(a.value);
        if (t === '' && !d.required) delete l[i][a.f]; else l[i][a.f] = d.type === 'int' && /^-?\d+$/.test(t.trim()) ? Number(t) : t;
        return setRow(l);
      }
      case 'toggle': {
        const v = parse(a.v);
        const flip = (list) => { list = Array.isArray(list) ? list.slice() : []; const at = list.findIndex((x) => same(x, v)); if (at >= 0) list.splice(at, 1); else list.push(v); return list; };
        if (inRow) {
          const l = rowsOf(); l[i] = { ...(l[i] || {}) };
          const allowed = (s.fields[a.f] || {}).allowed;
          let next = flip(l[i][a.f]);
          if (Array.isArray(allowed)) next = allowed.filter((x) => next.some((y) => same(x, y)));
          if (next.length) l[i][a.f] = next; else delete l[i][a.f];
          return setRow(l);
        }
        const next = flip(base);
        return stage(st, s.key, Array.isArray(s.allowed) ? s.allowed.filter((x) => next.some((y) => same(x, y))) : next);
      }
      case 'draft': st.drafts[s.key] = String(a.value); return st;
      case 'chip': {
        const t = String(st.drafts[s.key] || a.value || '').trim();
        if (!t) return st;
        const l = Array.isArray(base) ? base : [];
        if (!l.includes(t)) l.push(t);
        st.drafts[s.key] = '';
        return stage(st, s.key, l);
      }
      case 'unchip': { const l = Array.isArray(base) ? base : []; l.splice(i, 1); return stage(st, s.key, l); }
      case 'up': case 'down': case 'move': {
        const l = rowsOf();
        const to = a.op === 'up' ? i - 1 : a.op === 'down' ? i + 1 : Number(a.to);
        if (i < 0 || to < 0 || to >= l.length || to === i) return st;
        const [r] = l.splice(i, 1); l.splice(to, 0, r);
        if (st.editing === `${s.key}#${i}`) st.editing = `${s.key}#${to}`;
        return setRow(l);
      }
      case 'del': {
        const l = rowsOf(); l.splice(i, 1);
        if (st.editing && st.editing.startsWith(`${s.key}#`)) st.editing = null;
        return setRow(l);
      }
      case 'addrow': {
        const l = rowsOf();
        const row = {};
        for (const [f, d] of Object.entries(s.fields || {})) {
          if (!d.required) continue;
          row[f] = d.type === 'enum' && Array.isArray(d.allowed) && d.allowed.length ? d.allowed[0] : d.type === 'list' ? [] : f === 'reason' ? 'new rule' : '';
        }
        l.push(row);
        if (sentenceRows(s)) st.editing = `${s.key}#${l.length - 1}`;
        return setRow(l);
      }
      case 'edit': st.editing = st.editing === `${s.key}#${i}` ? null : `${s.key}#${i}`; return st;
      case 'undo': delete st.staged[s.key]; delete st.invalid[s.key]; delete st.errors[s.key]; return st;
      case 'reset': return stage(st, s.key, clone(s.default));
      default: return st;
    }
  };

  const api = { SECTIONS, initial, view, rail, pane, card, esc, words, sentence, where, act, stage, check, current, requests, refusal, revertable, footer };
  if (typeof globalThis !== 'undefined') globalThis.__unvrsSettings = api;
  if (typeof document === 'undefined' || typeof window === 'undefined') return;

  // ------------------------------------------------------------------ the page side
  let st = initial();
  let root = null, back = null, outside = [], dragFrom = null;

  const focusables = () => [...root.querySelectorAll('button, input, select, textarea, [tabindex]:not([tabindex="-1"])')]
    .filter((n) => !n.disabled && n.offsetParent !== null);

  /** Re-renders, keeping the focus (and caret) on the same control. */
  const draw = () => {
    if (!root) return;
    st.now = Date.now();
    const a = document.activeElement;
    const id = a && root.contains(a) && (a.dataset.fid ? ['data-fid', a.dataset.fid] : a.dataset.sec ? ['data-sec', a.dataset.sec]
      : a.dataset.tab ? ['data-tab', a.dataset.tab] : a.dataset.act ? ['data-act', a.dataset.act] : null);
    let sel = null;
    try { if (a && typeof a.selectionStart === 'number') sel = [a.selectionStart, a.selectionEnd]; } catch (e) { sel = null; }
    const scroll = root.querySelector('.st-pane')?.scrollTop || 0;
    root.innerHTML = view(st);
    const p = root.querySelector('.st-pane'); if (p) p.scrollTop = scroll;
    if (!id) return;
    const n = root.querySelector(`[${id[0]}="${CSS.escape(id[1])}"]`);
    if (!n) return;
    n.focus({ preventScroll: true });
    if (sel) try { n.setSelectionRange(sel[0], sel[1]); } catch (e) { /* number inputs have no caret */ }
  };

  const load = async () => {
    st.loading = true;
    st.csrf = !!csrf();
    try {
      const r = await fetch('/api/settings', { headers: { Accept: 'application/json' }, cache: 'no-store' });
      const body = await r.json().catch(() => null);
      if (!r.ok || !body || !Array.isArray(body.schema)) {
        throw new Error((body && body.error) || (r.status === 404
          ? 'This kernel does not serve settings yet (no /api/settings).'
          : `The kernel did not return settings (HTTP ${r.status}).`));
      }
      st.data = body; st.error = null;
    } catch (e) {
      st.error = e && e.message ? e.message : 'Could not reach the kernel.';
    }
    st.loading = false;
    draw();
  };

  // the page's write token (served only at the exact write origin; contract: browser authentication)
  const csrf = () => document.querySelector('meta[name="unvrs-settings-csrf"]')?.content || '';

  /** One write; resolves to { ok, change } or { ok: false, error } (never throws). */
  const post = async (body) => {
    try {
      const r = await fetch('/api/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-UNVRS-CSRF': csrf() },
        body: JSON.stringify(body),
      });
      const b = await r.json().catch(() => null);
      if (r.ok && b && b.ok) return { ok: true, change: b.change };
      return { ok: false, error: refusal(r.status, b) };
    } catch (e) {
      return { ok: false, error: 'Could not reach the kernel; nothing was saved.' };
    }
  };

  let toastTimer = 0;
  const toast = (text, tone = 'ok') => {
    st.toast = { text, tone };
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { st.toast = null; draw(); }, 5000);
  };

  /** Saves every staged change in turn; what the kernel refuses stays staged with its reason. */
  const save = async () => {
    if (st.saving || !Object.keys(st.staged).length) return;
    if (needsConfirm(st) && !st.confirm) { st.review = true; draw(); return; }
    st.saving = true; draw();
    const reqs = requests(st);
    let done = 0;
    for (const q of reqs) {
      const r = await post(q);
      if (r.ok) { done++; delete st.staged[q.key]; delete st.errors[q.key]; delete st.invalid[q.key]; }
      else st.errors[q.key] = r.error;
    }
    st.saving = false;
    const failed = reqs.length - done;
    if (!failed) { st.review = false; st.confirm = false; st.editing = null; }
    await load();
    toast(failed ? `Saved ${done} of ${reqs.length}; ${plural(failed, 'change')} refused, see the reasons.` : `Saved ${plural(done, 'change')}.`, failed ? 'bad' : 'ok');
    if (failed && !st.review) {
      const first = find(st, reqs.find((q) => st.errors[q.key]).key);
      if (first) st.section = first.section;
    }
    draw();
  };

  const revert = async (id) => {
    const c = (st.data.history || []).find((x) => x.change_id === id);
    if (!c || st.saving) return;
    const s = find(st, c.key);
    st.saving = true; st.revertError = null; draw();
    const r = await post({ op: 'settings.revert', change_id: id, confirm: !!(s && s.sensitive && st.revertConfirm) });
    st.saving = false;
    if (r.ok) {
      st.reverting = null; st.revertConfirm = false;
      await load();
      toast(`Reverted ${s ? s.label : c.key}.`);
    } else st.revertError = r.error;
    draw();
  };

  const gear = () => document.getElementById('settings-open');

  const open = () => {
    if (root) return;
    back = document.activeElement;
    root = document.createElement('div');
    root.id = 'settings';
    root.setAttribute('role', 'dialog');
    root.setAttribute('aria-modal', 'true');
    root.setAttribute('aria-labelledby', 'st-title');
    document.body.appendChild(root);
    // the page behind is out of reach while the overlay is open (restored on close)
    outside = [...document.body.children].filter((n) => n !== root && n.tagName !== 'SCRIPT' && !n.inert);
    outside.forEach((n) => { n.inert = true; });
    const g = gear(); if (g) g.setAttribute('aria-expanded', 'true');
    draw();
    const c = root.querySelector('.st-close'); if (c) c.focus({ preventScroll: true });
    load();
  };

  const close = () => {
    if (!root) return;
    root.remove(); root = null;
    outside.forEach((n) => { n.inert = false; }); outside = [];
    const g = gear(); if (g) g.removeAttribute('aria-expanded');
    const to = g || (back && back.isConnected ? back : null);
    if (to) to.focus({ preventScroll: true });
    back = null;
  };

  const control = (n) => ({ ...n.dataset, value: n.value });

  document.addEventListener('click', (e) => {
    const t = e.target instanceof Element ? e.target : null;
    if (!t) return;
    if (t.closest('#settings-open')) { e.preventDefault(); root ? close() : open(); return; }
    if (!root || !root.contains(t)) return;
    const b = t.closest('button');
    if (!b || b.disabled) return;
    if (b.dataset.act === 'close') close();
    else if (b.dataset.sec) { st.section = b.dataset.sec; st.tab = 'settings'; st.editing = null; draw(); root.querySelector('.st-pane').scrollTop = 0; }
    else if (b.dataset.tab) { st.tab = b.dataset.tab; st.review = false; draw(); }
    else if (b.dataset.act === 'review') { st.review = !st.review; st.tab = 'settings'; draw(); root.querySelector('.st-pane').scrollTop = 0; }
    else if (b.dataset.act === 'back') { st.review = false; draw(); }
    else if (b.dataset.act === 'discard') { st.staged = {}; st.invalid = {}; st.errors = {}; st.review = false; st.confirm = false; st.editing = null; draw(); }
    else if (b.dataset.act === 'save') save();
    else if (b.dataset.act === 'revert') { st.reverting = st.reverting === b.dataset.change ? null : b.dataset.change; st.revertConfirm = false; st.revertError = null; draw(); }
    else if (b.dataset.act === 'revcancel') { st.reverting = null; st.revertError = null; draw(); }
    else if (b.dataset.act === 'revgo') revert(b.dataset.change);
    else if (b.dataset.op === 'up' || b.dataset.op === 'down') {
      // the focus follows the moved row (onto its other arrow at either end)
      const to = Number(b.dataset.i) + (b.dataset.op === 'up' ? -1 : 1);
      act(st, control(b)); draw();
      const n = root.querySelector(`[data-fid="${CSS.escape(`${b.dataset.k}|${b.dataset.op}|${to}`)}"]`);
      const other = root.querySelector(`[data-fid="${CSS.escape(`${b.dataset.k}|${b.dataset.op === 'up' ? 'down' : 'up'}|${to}`)}"]`);
      const f = n && !n.disabled ? n : other; if (f) f.focus({ preventScroll: true });
    } else if (b.dataset.op) { act(st, control(b)); if (!Object.keys(st.staged).length) st.review = false; draw(); }
  });

  // typing stages as you go; selects and number fields stage on change too
  const typed = (e) => {
    const n = e.target;
    if (!root || !(n instanceof Element) || !root.contains(n) || !n.dataset.op || n.tagName === 'BUTTON') return;
    act(st, control(n)); draw();
  };
  document.addEventListener('input', typed);
  document.addEventListener('change', (e) => {
    const n = e.target;
    if (n instanceof HTMLSelectElement) typed(e);
    else if (root && n instanceof HTMLInputElement && n.type === 'checkbox' && root.contains(n)) {
      if (n.dataset.act === 'confirm') st.confirm = n.checked;
      if (n.dataset.act === 'revconfirm') st.revertConfirm = n.checked;
      draw();
    }
  });

  // rows drag by their grip to a new place (the ↑ ↓ buttons do the same from the keyboard)
  document.addEventListener('dragstart', (e) => {
    // only the ⠿ grip drags (a draggable row would swallow clicks and text selection in its fields)
    const g = root && e.target instanceof Element && e.target.closest('#settings .st-grip[draggable="true"]');
    const r = g && g.closest('.st-row');
    if (!r) return;
    dragFrom = { k: r.dataset.k, i: Number(r.dataset.i) };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', `${r.dataset.k}#${r.dataset.i}`);
  });
  document.addEventListener('dragover', (e) => {
    const r = dragFrom && e.target instanceof Element && e.target.closest('#settings .st-row');
    if (r && r.dataset.k === dragFrom.k) e.preventDefault();
  });
  document.addEventListener('drop', (e) => {
    const r = dragFrom && e.target instanceof Element && e.target.closest('#settings .st-row');
    if (r && r.dataset.k === dragFrom.k) {
      e.preventDefault();
      act(st, { op: 'move', k: dragFrom.k, i: dragFrom.i, to: Number(r.dataset.i) }); draw();
    }
    dragFrom = null;
  });

  const typing = (n) => n instanceof Element && (n.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(n.tagName));

  document.addEventListener('keydown', (e) => {
    if (!root) {
      if (e.key === ',' && !e.ctrlKey && !e.metaKey && !e.altKey && !e.repeat && !typing(e.target)) {
        e.preventDefault(); open();
      }
      return;
    }
    if (e.key === 'Escape') {
      e.preventDefault(); e.stopPropagation();
      if (st.editing) { const k = st.editing; st.editing = null; draw(); const [key, i] = k.split('#'); const b = root.querySelector(`[data-fid="${CSS.escape(`${key}|edit|${i}`)}"]`); if (b) b.focus(); }
      else close();
      return;
    }
    if (e.key === 'Enter' && e.target instanceof HTMLInputElement && e.target.dataset.op === 'draft') {
      e.preventDefault(); act(st, { ...control(e.target), op: 'chip' }); draw(); return;
    }
    if (e.key === 'Tab') {
      // the focus stays in the overlay
      const f = focusables();
      if (!f.length) { e.preventDefault(); return; }
      const [first, last] = [f[0], f[f.length - 1]];
      if (e.shiftKey && (document.activeElement === first || !root.contains(document.activeElement))) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && (document.activeElement === last || !root.contains(document.activeElement))) { e.preventDefault(); first.focus(); }
    }
  }, true);
})();
