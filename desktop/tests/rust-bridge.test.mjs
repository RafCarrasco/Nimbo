// CP2 S1 bridge v1 — label: "transport double: stdio instead of WebView2". The REAL injected
// script talks to the REAL Rust dispatcher through desktop/rust/examples/bridge-stdio.rs. This is
// protocol evidence only, never native WebView2 evidence (see helpers/webview-double.mjs).
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { ORIGIN, buildExample, createBridgeHost } from './helpers/webview-double.mjs';
import { loadDesktopConsumer } from './helpers/oracle.mjs';

const plain = (value) => JSON.parse(JSON.stringify(value));
const KEYS = ['version', 'signIn', 'cancelSignIn', 'setCaptureAudio', 'startAudio', 'stopAudio', 'watchUpdate', 'installUpdate', 'onUpdateProgress', 'startGameWatch', 'stopGameWatch', 'onGameState', 'onGameEnded', 'onAudio'];
const INTERIM = 'Login pelo app ainda não disponível nesta prévia.';
const LABEL = 'transport double: stdio instead of WebView2 — ';
const settled = (promise) => {
  const s = { done: false };
  promise.then((value) => Object.assign(s, { done: true, value }), (error) => Object.assign(s, { done: true, error }));
  return s;
};
const microtasks = () => new Promise((resolve) => setImmediate(resolve));

before(() => buildExample(), { timeout: 15 * 60 * 1000 });

async function ready(t) {
  const h = await createBridgeHost(t);
  const page = await h.load();
  await h.flush();
  assert.equal(h.gen, 1, 'host answered hello with ready for generation 1');
  return { h, page, bridge: page.window.nimboDesktop };
}

test(LABEL + 'exact v1 surface, Electron "nothing selected" answers, coercion and Result shape', async (t) => {
  const { h, page, bridge } = await ready(t);
  assert.deepEqual(Object.keys(bridge).sort(), [...KEYS].sort());
  assert.equal(bridge.version, 1);
  assert.ok(Object.isFrozen(bridge));
  assert.throws(() => { 'use strict'; page.window.nimboDesktop = {}; });
  assert.deepEqual(plain(page.sent[0]), { v: 1, kind: 'hello' });
  assert.deepEqual(plain(await bridge.startAudio()), { ok: true, value: null });
  assert.deepEqual(plain(await bridge.startGameWatch()), { ok: true, value: null });
  assert.deepEqual(plain(await bridge.installUpdate()), { ok: true, value: false });
  assert.deepEqual(plain(await bridge.signIn()), { ok: false, error: INTERIM });
  assert.deepEqual(plain(await bridge.cancelSignIn()), { ok: true });
  assert.deepEqual(plain(await bridge.stopAudio(123)), { ok: true }, 'String(id) coercion reaches the host');
  assert.deepEqual(plain(await bridge.stopGameWatch({ toString: () => 'w1' })), { ok: true });
  assert.deepEqual(plain(page.sent.at(-1)), { v: 1, kind: 'invoke', id: 7, gen: 1, method: 'nimbo:game:stop', args: ['w1'] });
  await bridge.setCaptureAudio('truthy');
  assert.equal((await h.dump()).audio_wanted, true);
  await bridge.setCaptureAudio(0);
  assert.equal((await h.dump()).audio_wanted, false);
  assert.deepEqual(await h.dump(), { audio_wanted: false, generation: 1, document: 'ready', last_id: 9 });
  assert.equal(h.clock.pending(), 0, 'ready timer and every call deadline released');
});

test(LABEL + 'not installed in iframes, foreign origins or without chrome.webview', async (t) => {
  const h = await createBridgeHost(t);
  for (const options of [{ top: false }, { origin: 'https://golive-ad118.web.app' }, { origin: 'http://localhost:4173' }, { webview: false }]) {
    const page = await h.load(options);
    assert.equal(page.window.nimboDesktop, undefined, JSON.stringify(options));
    assert.equal(page.sent.length, 0, 'no hello either');
  }
  assert.equal(h.clock.pending(), 0);
});

test(LABEL + 'listener plumbing, no audio (host -> page events injected through the double)', async (t) => {
  const { h, bridge } = await ready(t);
  const event = (channel, ...args) => h.deliver({ v: 1, kind: 'event', gen: h.gen, channel, args });
  const chunks = [], ends = [];
  const cleanup = bridge.onAudio('a', (chunk) => chunks.push(chunk), (error) => ends.push(error));
  event('nimbo:audio:data', 'other', [1]);
  event('nimbo:audio:end', 'other', 'ignored');
  event('nimbo:audio:data', 'a', [0, 128]);
  h.deliver({ v: 1, kind: 'event', gen: h.gen + 1, channel: 'nimbo:audio:data', args: ['a', [9]] });
  assert.deepEqual(plain(chunks), [[0, 128]], 'stale-generation event ignored');
  event('nimbo:audio:end', 'a', null);
  event('nimbo:audio:end', 'a', 'again');
  event('nimbo:audio:data', 'a', [1]);
  assert.deepEqual(ends, [''], 'onEnd once, both listeners removed');
  cleanup(); cleanup();
  const stop = bridge.onAudio('b', () => assert.fail('removed'), () => assert.fail('removed'));
  stop(); stop();
  event('nimbo:audio:data', 'b', [1]);
  event('nimbo:audio:end', 'b', '');

  const progress = [];
  const off = bridge.onUpdateProgress((p) => progress.push(p));
  event('nimbo:update:progress', 42.6);
  event('nimbo:update:progress', 'invalid');
  off();
  event('nimbo:update:progress', 7);
  assert.deepEqual(progress, [42.6, 0]);

  const states = [], ended = [];
  const offState = bridge.onGameState('w', (active) => states.push(active));
  const offEnded = bridge.onGameEnded('w', () => ended.push(true));
  event('nimbo:game:state', 'x', true);
  event('nimbo:game:state', 'w', 1);
  event('nimbo:game:state', 'w', 0);
  event('nimbo:game:ended', 'x');
  event('nimbo:game:ended', 'w');
  offState(); offEnded();
  event('nimbo:game:state', 'w', true);
  event('nimbo:game:ended', 'w');
  assert.deepEqual(states, [true, false]);
  assert.deepEqual(ended, [true]);
  assert.ok(!h.toPage.some((m) => m.kind === 'event'), 'the CP2 host itself never emits events');
});

test(LABEL + 'watchUpdate keeps a permanent listener and installUpdate answers false', async (t) => {
  const { h, bridge } = await ready(t);
  const seen = [];
  assert.deepEqual(plain(await bridge.watchUpdate((info) => seen.push(plain(info)))), { ok: true, value: null });
  h.deliver({ v: 1, kind: 'event', gen: h.gen, channel: 'nimbo:update:ready', args: [{ version: '0.4.2-fixture' }] });
  h.deliver({ v: 1, kind: 'event', gen: h.gen, channel: 'nimbo:update:ready', args: [{ version: '0.4.3-fixture' }] });
  assert.deepEqual(seen, [{ version: '0.4.2-fixture' }, { version: '0.4.3-fixture' }], 'no unsubscribe invented');
  assert.equal((await bridge.installUpdate()).value, false);
});

test(LABEL + 'unchanged src/desktop.ts consumer: desktopSignIn rejects with the S1 interim text', async (t) => {
  const { bridge } = await ready(t);
  const consumer = loadDesktopConsumer(bridge);
  await assert.rejects(consumer.api.desktopSignIn(), (error) => error.message === INTERIM);
  assert.equal(await consumer.api.desktopAudioTrack(), null);
});

test(LABEL + 'adversarial controls: no reply within 200 ms and the exact fixed code', async (t) => {
  const { h } = await ready(t);
  const call = (id, extra = {}) => ({ v: 1, kind: 'invoke', id, gen: h.gen, method: 'nimbo:audio:start', args: [], ...extra });
  await h.expectDrop('bridge-unauthorized', () => h.send(call(100), { source: 'https://evil.test/' }));
  await h.expectDrop('bridge-unauthorized', () => h.send(call(100), { source: 'http://localhost:4173/' }));
  await h.expectDrop('bridge-unauthorized', () => h.send(call(100), { top: false }));
  await h.expectDrop('bridge-stale', () => h.send(call(100, { gen: h.gen + 1 })));
  await h.expectDrop('bridge-malformed', () => h.send(call(100, { v: 2 })));
  await h.expectDrop('bridge-malformed', () => h.send([1, 2]));
  await h.expectDrop('bridge-malformed', () => h.sendRaw('{"v":1,'));
  await h.expectDrop('bridge-unknown-method', () => h.send(call(100, { method: 'nimbo:capture:start' })));
  await h.expectDrop('bridge-malformed', () => h.send(call(100, { method: 'nimbo:audio:stop', args: [123] })));
  // 5 KiB with an extra key (key-shape AND size); the exact size boundary is tested separately.
  await h.expectDrop('bridge-malformed', () => h.send(call(100, { pad: 'x'.repeat(5 * 1024) })));
  await h.expectDrop('bridge-duplicate-hello', () => h.send({ v: 1, kind: 'hello' }));
  h.send(call(200));
  await h.flush();
  assert.deepEqual(plain(h.toPage.at(-1)), { v: 1, kind: 'result', id: 200, gen: 1, ok: true, value: null });
  await h.expectDrop('bridge-replayed-id', () => h.send(call(200)));
  await h.expectDrop('bridge-replayed-id', () => h.send(call(150)));
  // Navigation: the old generation is stale; the new document needs its own hello.
  h.control('content-loading');
  await h.expectDrop('bridge-stale', () => h.send(call(300, { gen: 1 })));
  await h.expectDrop('bridge-not-ready', () => h.send(call(300, { gen: 2 })));
  h.control('disable bridge-test-disabled');
  await h.expectDrop('bridge-disabled', () => h.send({ v: 1, kind: 'hello' }));
  assert.deepEqual(await h.dump(), { audio_wanted: false, generation: 2, document: 'disabled', last_id: null });
});

// Tester F1: valid-shaped messages padded with JSON whitespace to an exact byte length, sent as
// the exact raw text, so ONLY the inbound size limit can reject the 4097-byte copy.
const padded = (msg, bytes) => {
  const text = JSON.stringify(msg);
  const out = text.slice(0, -1) + ' '.repeat(bytes - Buffer.byteLength(text)) + '}';
  assert.equal(Buffer.byteLength(out), bytes);
  assert.deepEqual(JSON.parse(out), msg);
  return out;
};

test(LABEL + 'inbound limit: valid 4096-byte hello/invoke accepted, 4097-byte copies dropped without reply', async (t) => {
  const h = await createBridgeHost(t);
  // Deterministic no-reply check: `flush` is a stdout round trip, so any reply to the raw line
  // arrives before it resolves; the fixed code (stderr) is awaited separately.
  const drop = async (code, raw) => {
    h.control('tick 1000');
    const replies = h.toPage.length, from = h.codes.length;
    h.sendRaw(raw);
    await h.flush();
    assert.deepEqual(h.toPage.slice(replies), [], 'no reply for ' + code);
    await h.waitCode(code, from);
  };
  const accept = async (raw) => {
    const replies = h.toPage.length, from = h.codes.length;
    h.sendRaw(raw);
    await h.flush();
    assert.deepEqual(h.codes.slice(from), [], 'no drop code');
    return h.toPage.slice(replies);
  };
  const hello = { v: 1, kind: 'hello' };
  await drop('bridge-malformed', padded(hello, 4097));
  await drop('bridge-malformed', JSON.stringify({ v: 1, kind: 'hello', gen: 1 }));
  assert.deepEqual(await h.dump(), { audio_wanted: false, generation: 1, document: 'awaiting-hello', last_id: null });
  assert.deepEqual(plain(await accept(padded(hello, 4096))), [{ v: 1, kind: 'ready', gen: 1 }]);
  const call = { v: 1, kind: 'invoke', id: 5, gen: 1, method: 'nimbo:capture:audio', args: [true] };
  await drop('bridge-malformed', padded(call, 4097));
  assert.deepEqual(await h.dump(), { audio_wanted: false, generation: 1, document: 'ready', last_id: 0 });
  assert.deepEqual(plain(await accept(padded(call, 4096))), [{ v: 1, kind: 'result', id: 5, gen: 1, ok: true }]);
  assert.deepEqual(await h.dump(), { audio_wanted: true, generation: 1, document: 'ready', last_id: 5 });
});

test(LABEL + '65 queued before ready and 65 in flight: the 65th is "Ponte ocupada."; timers released', async (t) => {
  const h = await createBridgeHost(t);
  const page = await h.load();
  const bridge = page.window.nimboDesktop;
  const queued = Array.from({ length: 65 }, () => bridge.startAudio());
  assert.deepEqual(plain(await queued[64]), { ok: false, error: 'Ponte ocupada.' });
  assert.equal(page.sent.length, 1, 'only hello was posted before ready');
  await h.flush();
  for (const p of queued.slice(0, 64)) assert.deepEqual(plain(await p), { ok: true, value: null });
  assert.equal(h.clock.pending(), 0);
  const inflight = Array.from({ length: 65 }, () => bridge.startGameWatch());
  assert.deepEqual(plain(await inflight[64]), { ok: false, error: 'Ponte ocupada.' });
  await h.flush();
  for (const p of inflight.slice(0, 64)) assert.deepEqual(plain(await p), { ok: true, value: null });
  assert.equal(h.clock.pending(), 0);
  assert.equal((await h.dump()).last_id, 128);
});

test(LABEL + 'ready withheld 5 s (host disabled the document): queued promises reject, later calls reject at once', async (t) => {
  const h = await createBridgeHost(t);
  h.control('disable bridge-test-disabled');
  const from = h.codes.length;
  const page = await h.load();
  await h.waitCode('bridge-disabled', from);
  const bridge = page.window.nimboDesktop;
  const calls = [bridge.startAudio(), bridge.signIn(), bridge.installUpdate()].map(settled);
  h.clock.advance(4999);
  await microtasks();
  assert.ok(calls.every((c) => !c.done));
  h.clock.advance(1);
  await microtasks();
  for (const c of calls) assert.equal(c.error?.message, 'Ponte indisponível.');
  await assert.rejects(bridge.startAudio(), (error) => error.message === 'Ponte indisponível.');
  // A late or forged ready cannot revive the bridge.
  h.deliver({ v: 1, kind: 'ready', gen: 1 });
  await assert.rejects(bridge.cancelSignIn(), (error) => error.message === 'Ponte indisponível.');
  assert.equal(h.clock.pending(), 0);
  assert.equal(page.sent.length, 1, 'nothing but hello was ever posted');
});

test(LABEL + 'deadlines: 30 s "Ponte sem resposta." (host dropped it), signIn 330 s, late result ignored', async (t) => {
  const { h, bridge } = await ready(t);
  const from = h.codes.length;
  const lost = settled(bridge.stopAudio('x'.repeat(200)));
  await h.waitCode('bridge-malformed', from);
  await h.flush();
  h.clock.advance(29999);
  await microtasks();
  assert.equal(lost.done, false);
  h.clock.advance(1);
  await microtasks();
  assert.equal(lost.error?.message, 'Ponte sem resposta.');
  assert.equal(h.clock.pending(), 0);

  h.hold = true;
  const signIn = settled(bridge.signIn());
  await h.flush();
  assert.equal(h.held.length, 1, 'host answered; the double holds the reply');
  h.clock.advance(30000);
  await microtasks();
  assert.equal(signIn.done, false, 'signIn is not bound by the 30 s deadline');
  h.clock.advance(300000);
  await microtasks();
  assert.equal(signIn.error?.message, 'Ponte sem resposta.');
  h.release(); // the late result for a forgotten id is ignored
  await microtasks();
  assert.equal(signIn.error?.message, 'Ponte sem resposta.');
  assert.deepEqual(plain(await bridge.startAudio()), { ok: true, value: null }, 'bridge still usable');
  assert.equal(h.clock.pending(), 0);
});

test(LABEL + 'id exhaustion without wrap or reuse (source-transformed copy starts at the last id)', async (t) => {
  const h = await createBridgeHost(t);
  const transform = (s) => {
    assert.equal(s.split('nextId = 1,').length, 2);
    return s.replace('nextId = 1,', 'nextId = 4294967295,');
  };
  const page = await h.load({ transform });
  await h.flush();
  const bridge = page.window.nimboDesktop;
  assert.deepEqual(plain(await bridge.startAudio()), { ok: true, value: null });
  assert.equal(page.sent.at(-1).id, 4294967295);
  await assert.rejects(bridge.startAudio(), (error) => error.message === 'Ponte indisponível.');
  await assert.rejects(bridge.installUpdate(), (error) => error.message === 'Ponte indisponível.');
  assert.equal(page.sent.length, 2, 'no id beyond 2^32-1 was ever posted');
  assert.deepEqual(await h.dump(), { audio_wanted: false, generation: 1, document: 'ready', last_id: 4294967295 });
});

test('release isolation: the stdio example needs test-host and test-host is refused in release', () => {
  const manifest = fileURLToPath(new URL('../rust/Cargo.toml', import.meta.url));
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('CARGO_PROFILE_')));
  const check = (...args) => spawnSync('cargo', ['+1.99.1', 'check', '--manifest-path', manifest, '--locked', ...args], { encoding: 'utf8', env, timeout: 15 * 60 * 1000 });
  const plainRelease = check('--release', '--example', 'bridge-stdio');
  assert.notEqual(plainRelease.status, 0);
  assert.match(plainRelease.stderr, /requires the features: `test-host`/);
  const withFeature = check('--release', '--features', 'test-host', '--example', 'bridge-stdio');
  assert.notEqual(withFeature.status, 0);
  assert.match(withFeature.stdout + withFeature.stderr, /NIMBO_TEST_HOST_FORBIDDEN_IN_RELEASE/);
  assert.ok(ORIGIN.startsWith('http://127.0.0.1:'), 'loopback-only fixture origin');
});
