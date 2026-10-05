// Run: node observatory/tests/settings.mjs
// The Settings overlay's views, rendered from the API fixture: every section, every type,
// no bare "?", and the staged edits each editor makes.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const src = readFileSync(new URL('../src/preview/settings.js', import.meta.url), 'utf8');
const fixture = JSON.parse(readFileSync(new URL('../fixtures/settings.json', import.meta.url), 'utf8'));
const ctx = {};
vm.runInNewContext(src, ctx);
const S = ctx.__unvrsSettings;
assert.ok(S, 'settings.js exposes its views');

const fresh = (patch = {}) => Object.assign(S.initial(), { data: structuredClone(fixture), now: 1790890000000 + 120000 }, patch);
const html = (st) => S.view(st);
const text = (h) => h.replace(/<[^>]+>/g, ' ').replace(/&[a-z#0-9]+;/g, ' ');
const noQuestion = (h, where) => {
  for (const bad of ['>?<', '· ?', '? ·', '"?"', ' ? ']) assert.ok(!h.includes(bad), `${JSON.stringify(bad)} in ${where}`);
  assert.ok(!/\bundefined\b|\bNaN\b|\[object Object\]/.test(text(h)), `undefined/NaN/object text in ${where}`);
};

// ---------------------------------------------------------------- every section renders its cards
const counts = {};
for (const s of fixture.schema) counts[s.section] = (counts[s.section] || 0) + 1;
for (const [id, label] of S.SECTIONS) {
  const h = html(fresh({ section: id }));
  noQuestion(h, id);
  assert.ok(h.includes(`data-sec="${id}" aria-current="true"`), `${id} is current`);
  assert.equal((h.match(/class="st-card[ "]/g) || []).length, counts[id] || 0, `${id} cards`);
  assert.ok(h.includes(`>${S.esc(label)}</h3>`), `${id} heading`);
  // the rail counts every section
  for (const [other] of S.SECTIONS) {
    assert.ok(new RegExp(`data-sec="${other}"[^>]*>.*?<span class="st-n"[^>]*>${counts[other] || 0}</span>`).test(h), `${other} count`);
  }
}

// ---------------------------------------------------------------- every card says what it is
for (const s of fixture.schema) {
  const h = html(fresh({ section: s.section }));
  const at = h.indexOf(`data-key="${s.key}"`);
  assert.ok(at > 0, `${s.key} card`);
  const card = h.slice(at, h.indexOf('</article>', at));
  assert.ok(card.includes(S.esc(s.label)), `${s.key} label`);
  const expl = s.explanation.split(/\bExample:/)[0].trim();
  assert.ok(card.includes(S.esc(expl)), `${s.key} explanation`);
  if (/Example:/.test(s.explanation)) assert.ok(card.includes('class="st-ex">Example:'), `${s.key} example on its own line`);
  const source = fixture.effective_source[s.key] || 'default';
  assert.ok(card.includes(`st-src-${source}`), `${s.key} source badge ${source}`);
  assert.ok(card.includes(s.apply === 'restart' ? '>after restart<' : '>applies now<'), `${s.key} apply badge`);
  assert.equal(card.includes('>read-only in v1<'), !s.editable, `${s.key} read-only badge`);
  assert.ok(card.includes(`class="st-where"`) && card.includes(S.esc(S.where(s))), `${s.key} where it lives`);
  if (s.file) assert.ok(S.where(s).startsWith(`${s.file} › `), `${s.key}: file › path`);
  if (s.sensitive) assert.ok(card.includes('>sensitive<'), `${s.key} sensitive`);
  if (!s.editable) assert.ok(!/<input|<select|data-op="/.test(card), `${s.key} has no editor`);
}

// ---------------------------------------------------------------- every type gets its editor
const routing = html(fresh({ section: 'routing' }));
const sources = html(fresh({ section: 'sources' }));
const cardOf = (h, key) => { const at = h.indexOf(`data-key="${key}"`); return h.slice(at, h.indexOf('</article>', at)); };
assert.ok(cardOf(routing, 'econ.policy.default_role').includes('role="radiogroup"'), 'short enum: segmented');
assert.ok(cardOf(routing, 'econ.reasoning.deep').includes('<select'), 'long enum: select');
assert.ok(cardOf(routing, 'econ.policy.allow_max').includes('role="switch"'), 'bool: switch');
const int = cardOf(routing, 'econ.catalog.refresh_minutes');
assert.ok(int.includes('type="number"') && int.includes('min="1"') && int.includes('at least 1') && int.includes('5 min'), 'int: number with range');
assert.ok(cardOf(sources, 'sources.handbook.purpose').includes('data-op="text"'), 'string: input');
assert.ok(/st-mono[^>]*data-op="text"|data-op="text"[^>]*st-mono/.test(cardOf(sources, 'sources.handbook.uri')) || cardOf(sources, 'sources.handbook.uri').includes('st-wide st-mono'), 'path: mono input');
const list = cardOf(sources, 'sources.handbook.exclude');
assert.ok(list.includes('class="st-chip"') && list.includes('Remove private/**') && list.includes('data-op="draft"'), 'list: chips');
const prof = cardOf(routing, 'econ.profiles.judge');
assert.ok(prof.includes('class="st-rn st-grip" aria-hidden="true" draggable="true"') && prof.includes('Move row 1 up') && prof.includes('Move row 2 down'), 'ordered-list: draggable rows with up/down');
const rules = cardOf(routing, 'econ.policy.rules');
assert.ok(rules.includes('>kind decide<') && rules.includes('>judge<') && rules.includes('decision task'), 'rules read as sentences');
assert.ok(rules.includes('>levels 1 or 2<'), 'ranks read as levels');
assert.equal((rules.match(/data-op="edit"/g) || []).length, fixture.values['econ.policy.rules'].length, 'an Edit per rule');
// sources group per record with a sentence
assert.ok(sources.includes('class="st-gid">handbook<'), 'source record heading');
// overrides: amber and the bypass warning
const ov = html(fresh({ section: 'overrides' }));
assert.ok(ov.includes('st-head st-amber') && ov.includes('>bypasses your routing rules<'), 'overrides amber + bypass');
// the live catalog on routing only
assert.ok(routing.includes('Live model catalog') && routing.includes('example-model-id') && routing.includes('signed in') && routing.includes('refreshed 2m ago'), 'catalog');
assert.ok(!sources.includes('Live model catalog'));

// ---------------------------------------------------------------- edits stage, validate and undo
let st = fresh();
S.act(st, { op: 'set', k: 'econ.policy.default_role', v: '"judge"' });
assert.equal(st.staged['econ.policy.default_role'], 'judge');
S.act(st, { op: 'set', k: 'econ.policy.default_role', v: '"worker"' });
assert.ok(!('econ.policy.default_role' in st.staged), 'back to the saved value: unstaged');
S.act(st, { op: 'bool', k: 'econ.policy.allow_max' });
assert.equal(st.staged['econ.policy.allow_max'], true);
S.act(st, { op: 'int', k: 'econ.catalog.refresh_minutes', value: '0' });
assert.equal(st.invalid['econ.catalog.refresh_minutes'], 'Use 1 or more.');
assert.ok(html(st).includes('Use 1 or more.'));
S.act(st, { op: 'int', k: 'econ.catalog.refresh_minutes', value: '2.5' });
assert.equal(st.invalid['econ.catalog.refresh_minutes'], 'Use a whole number.');
S.act(st, { op: 'int', k: 'econ.catalog.refresh_minutes', value: '10' });
assert.equal(st.staged['econ.catalog.refresh_minutes'], 10);
assert.ok(!st.invalid['econ.catalog.refresh_minutes']);
S.act(st, { op: 'draft', k: 'sources.handbook.exclude', value: 'secrets/**' });
S.act(st, { op: 'chip', k: 'sources.handbook.exclude' });
assert.deepEqual([...st.staged['sources.handbook.exclude']], ['private/**', 'secrets/**']);
S.act(st, { op: 'unchip', k: 'sources.handbook.exclude', i: '0' });
assert.deepEqual([...st.staged['sources.handbook.exclude']], ['secrets/**']);
S.act(st, { op: 'down', k: 'econ.profiles.judge', i: '0' });
assert.deepEqual([...st.staged['econ.profiles.judge'].map((r) => r.model)], ['newest fable', 'newest opus']);
S.act(st, { op: 'move', k: 'econ.profiles.judge', i: 1, to: 0 });
assert.ok(!('econ.profiles.judge' in st.staged), 'moved back: unstaged');
S.act(st, { op: 'toggle', k: 'econ.policy.rules', i: '3', f: 'ranks', v: '3' });
assert.deepEqual([...st.staged['econ.policy.rules'][3].ranks], [3]);
S.act(st, { op: 'set', k: 'econ.policy.rules', i: '3', f: 'judgment', value: 'null' });
S.act(st, { op: 'rowset', k: 'econ.policy.rules', i: '3', f: 'reason', value: '' });
assert.equal(st.invalid['econ.policy.rules'], 'Row 4 needs reason.');
S.act(st, { op: 'undo', k: 'econ.policy.rules' });
assert.ok(!('econ.policy.rules' in st.staged) && !st.invalid['econ.policy.rules']);
S.act(st, { op: 'addrow', k: 'econ.policy.rules' });
assert.equal(st.staged['econ.policy.rules'].length, fixture.values['econ.policy.rules'].length + 1);
assert.equal(st.editing, `econ.policy.rules#${fixture.values['econ.policy.rules'].length}`, 'a new rule opens its form');
assert.ok(html(st).includes(`aria-label="Edit row ${fixture.values['econ.policy.rules'].length + 1}"`));
S.act(st, { op: 'reset', k: 'econ.reasoning.deep' });
assert.ok(!('econ.reasoning.deep' in st.staged), 'already the default');
// read-only keys never stage
S.act(st, { op: 'set', k: 'safety.watchdog_grace_ms', v: '1' });
assert.ok(!('safety.watchdog_grace_ms' in st.staged));
// a staged card shows what it was and an Undo; the rail counts it
const h = html(Object.assign(st, { section: 'routing' }));
assert.ok(cardOf(h, 'econ.policy.allow_max').includes('was <code>off</code>'));
assert.ok(/data-sec="sources"[^>]*>.*?st-staged/.test(h), 'rail marks staged sections');
noQuestion(h, 'staged');

// ---------------------------------------------------------------- footer, review, save requests
st = fresh();
assert.equal(S.footer(st), '', 'no footer while nothing is staged');
S.act(st, { op: 'set', k: 'econ.policy.default_role', v: '"judge"' });
S.act(st, { op: 'set', k: 'econ.reasoning.deep', value: '"medium"' });
let foot = S.footer(st);
assert.ok(foot.includes('<strong>2 changes</strong>') && foot.includes('>Review<') && foot.includes('>Save<'), 'N changes · Review · Save');
assert.ok(foot.includes('Save asks you to confirm'), 'sensitive change announced');
assert.ok(!/data-act="save" disabled/.test(foot), 'Save opens the review first');
st.review = true;
foot = S.footer(st);
assert.ok(/data-act="save" disabled/.test(foot), 'sensitive: Save waits for the confirm box');
let rv = html(st);
assert.ok(rv.includes('Review 2 changes') && rv.includes('data-act="confirm"') && rv.includes('Deep reasoning effort'), 'review lists changes and the confirm box');
assert.ok(rv.includes('<span class="st-bal">before</span><code>high</code>') && rv.includes('<span class="st-bal">after</span><code>medium</code>'), 'before → after');
noQuestion(rv, 'review');
assert.deepEqual(JSON.parse(JSON.stringify(S.requests(st))), [
  { op: 'settings.set', key: 'econ.policy.default_role', value: 'judge', confirm: false },
  { op: 'settings.set', key: 'econ.reasoning.deep', value: 'medium', confirm: false },
]);
st.confirm = true;
assert.ok(!/data-act="save" disabled/.test(S.footer(st)), 'confirmed: Save enabled');
assert.equal(S.requests(st)[1].confirm, true, 'sensitive change carries confirm: true');
assert.equal(S.requests(st)[0].confirm, false, 'only sensitive changes claim confirmation');
// rules travel as the whole ordered array
S.act(st, { op: 'down', k: 'econ.policy.rules', i: '0' });
const rq = S.requests(st).find((q) => q.key === 'econ.policy.rules');
assert.equal(rq.value.length, fixture.values['econ.policy.rules'].length); assert.equal(rq.value[1].reason, 'seat judgment');
assert.ok(html(st).includes('<ol class="st-mini">'), 'rules review as sentences');
// invalid blocks save; no page token blocks save with a reason
S.act(st, { op: 'int', k: 'econ.catalog.refresh_minutes', value: '0' });
assert.ok(/data-act="save" disabled/.test(S.footer(st)) && S.footer(st).includes('1 change to fix'));
S.act(st, { op: 'undo', k: 'econ.catalog.refresh_minutes' });
st.csrf = false;
assert.ok(/data-act="save" disabled/.test(S.footer(st)) && S.footer(st).includes('http://unvrs.localhost:7576'));
// the kernel's refusals in plain words, shown on the card
assert.equal(S.refusal(400, { ok: false, error: 'Deep reasoning effort must be one of: low, medium, high.' }), 'Deep reasoning effort must be one of: low, medium, high.');
assert.ok(S.refusal(403, { ok: false, error: 'CSRF value missing' }).startsWith('The kernel refused this page: CSRF value missing.'));
assert.ok(S.refusal(500, null).startsWith('The kernel could not write the change'));
assert.ok(!S.refusal(502, null).includes('?'));
st.errors['econ.reasoning.deep'] = 'Deep reasoning effort must be one of: low, medium, high.';
st.review = false; st.section = 'routing';
assert.ok(cardOf(html(st), 'econ.reasoning.deep').includes('role="alert">Deep reasoning effort must be one of'), 'inline kernel error');

// ---------------------------------------------------------------- history, newest first, with Revert
const hist = fresh({ tab: 'history' });
hist.data.history.push({ change_id: 'settings-1790880000000-1', key: 'econ.reasoning.deep', before: 'high', after: 'medium', by: 'captain', at: 1790880000000, reverts: null });
hist.data.history.unshift({ change_id: 'settings-1790895000000-3', key: 'econ.policy.default_role', before: 'worker', after: 'judge', by: 'observatory', at: 1790895000000, reverts: null });
hist.data.values['econ.policy.default_role'] = 'judge';
let hh = html(hist);
noQuestion(hh, 'history');
const order = [...hh.matchAll(/data-change="(settings-[^"]+)"/g)].map((m) => m[1]).filter((x, i, a) => a.indexOf(x) === i);
assert.deepEqual(order, ['settings-1790895000000-3', 'settings-1790890000000-1', 'settings-1790880000000-1'], 'newest first');
assert.ok(/data-act="revert" data-change="settings-1790895000000-3"/.test(hh), 'the current value can be reverted');
assert.ok(/data-act="revert" data-change="settings-1790890000000-1"/.test(hh), 'the fixture change is current too');
assert.ok(!/data-act="revert" data-change="settings-1790880000000-1"/.test(hh) && hh.includes('Changed again since'), 'an older change of the same key cannot be');
hist.reverting = 'settings-1790890000000-1';
hh = html(hist);
assert.ok(hh.includes('data-act="revconfirm"') && /data-act="revgo"[^>]*disabled/.test(hh), 'a sensitive revert needs its confirm box');
hist.revertConfirm = true;
assert.ok(!/data-act="revgo"[^>]*disabled/.test(html(hist)));

// ---------------------------------------------------------------- words never fall back to "?"
assert.equal(S.words({ key: 'x', type: 'string' }, null), 'not set');
assert.equal(S.words({ key: 'safety.watchdog_grace_ms', type: 'int' }, 120000), '120000 (2 min)');
assert.equal(S.words({ key: 'x', type: 'list' }, []), 'none');
console.log('settings.mjs: ok');

// Research pins use the existing profile editor and survive staged save payloads.
const researchState = fresh({ section: 'routing' });
const researchCard = cardOf(html(researchState), 'econ.profiles.research');
assert.ok(researchCard.includes('newest sonnet') && researchCard.includes('newest sol'));
assert.ok(researchCard.includes('data-f="effort"') && researchCard.includes('aria-label="Effort, row 2"'));
assert.ok(researchCard.includes('Captain fallback: on') && researchCard.includes('aria-label="Captain fallback, row 2"'));
assert.ok(researchCard.includes(S.esc(fixture.schema.find((s) => s.key === 'econ.profiles.research').fields.effort.explanation)), 'pin explanation in the control title');
S.act(researchState, { op: 'set', k: 'econ.profiles.research', i: '1', f: 'effort', value: '"light"' });
assert.equal(researchState.staged['econ.profiles.research'][1].effort, 'light');
assert.equal(researchState.staged['econ.profiles.research'][1].captain_fallback, true);
S.act(researchState, { op: 'rowbool', k: 'econ.profiles.research', i: '1', f: 'captain_fallback' });
assert.equal(researchState.staged['econ.profiles.research'][1].captain_fallback, false);
assert.ok(S.requests(researchState).some((r) => r.key === 'econ.profiles.research' && r.value[1].captain_fallback === false));
