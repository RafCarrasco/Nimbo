// CP2 S3 correction6 — PURE regressions for the UrlLog EXPECTED-ACTIVATION observation boundary.
// NOT browser integration cases: synthetic CDP Network.* event sequences modelled on the shapes the
// run1 trace actually retained (callback POST Document -> 200, then ONE custom-scheme Document GET
// whose only outcome is Network.loadingFailed net::ERR_ABORTED canceled). No socket, no process,
// no browser. classifyUrl stays fail closed: the custom scheme is still 'external'; only an armed,
// fully proven context removes that single request from violations().
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { describe, test } from 'node:test';
import { UrlLog, classifyUrl, maskUrl, scanForSecrets } from './helpers/offline-evidence.mjs';

const SCHEME = 'golive-testhost-2e6f92e8f8cf4af8';
const ORIGIN = 'http://127.0.0.1:4173';
const PORT = 40715;
const CALLBACK = `http://127.0.0.1:${PORT}/callback`;
const STATE = '0123456789abcdef'.repeat(3);
const OTHER = 'fedcba9876543210'.repeat(3);
const SID = '6DBE997FB1376C02421244F1D578DBC4';
const FRAME = 'FRAME-MAIN-1';
const ACTIVATION = `${SCHEME}://auth?state=${STATE}`;
const commit = (s) => createHash('sha256').update('nimbo-s3-activation-state\0').update(s).digest('hex');

/** One genuine Deliver flow in run1 order; `o` overrides individual fields to forge variants. */
function flow(o = {}) {
  const sid = o.sid ?? SID;
  const ev = [
    ['Network.requestWillBeSent', { requestId: 'LOGIN', frameId: FRAME, type: 'Document', request: { url: `${ORIGIN}/desktop-login?port=${PORT}&state=${STATE}&devUser=alice`, method: 'GET', headers: {} } }, sid],
    ['Network.responseReceived', { requestId: 'LOGIN', response: { url: `${ORIGIN}/desktop-login`, status: 200, remoteIPAddress: '127.0.0.1' } }, sid],
    ['Network.requestWillBeSent', { requestId: 'SRC', frameId: o.srcFrame ?? FRAME, type: o.srcType ?? 'Document',
      request: { url: o.callback ?? CALLBACK, method: o.srcMethod ?? 'POST', headers: o.srcHeaders ?? { Origin: ORIGIN } } }, sid],
  ];
  if (o.srcStatus !== null) ev.push(['Network.responseReceived', { requestId: 'SRC', response: { url: o.callback ?? CALLBACK, status: o.srcStatus ?? 200, remoteIPAddress: '127.0.0.1' } }, sid]);
  const attempt = ['Network.requestWillBeSent', { requestId: 'ACT', frameId: o.actFrame ?? FRAME, type: o.actType ?? 'Document',
    request: { url: o.url ?? ACTIVATION, method: o.actMethod ?? 'GET', headers: {} }, ...(o.redirect ? { redirectResponse: { url: CALLBACK, status: 302 } } : {}) }, o.actSid ?? sid];
  if (o.attemptFirst) ev.splice(3, 0, attempt); else if (o.attempt !== false) ev.push(attempt);
  if (o.response) ev.push(['Network.responseReceived', { requestId: 'ACT', response: { url: o.url ?? ACTIVATION, status: 200, remoteIPAddress: '' } }, sid]);
  if (o.failure !== false && o.attempt !== false) ev.push(['Network.loadingFailed', { requestId: 'ACT', errorText: 'net::ERR_ABORTED', canceled: true }, o.failSid ?? sid]);
  for (const extra of o.extra ?? []) ev.push(extra);
  return ev;
}
const replay = (log, events) => { for (const [m, p, s] of events) log.record(m, p, s); return log; };
const armed = (opts = {}) => {
  const log = new UrlLog({ activation: { scheme: SCHEME, origin: ORIGIN } });
  const ctx = log.armActivation({ state: opts.state ?? STATE, sessionId: opts.sessionId ?? SID, callbackUrl: opts.callbackUrl ?? CALLBACK });
  return { log, ctx };
};
const activationEntries = (log) => log.violations().filter((e) => e.requestId === 'ACT' || e.url?.startsWith('golive'));

describe('PURE UrlLog expected-activation: genuine Deliver attempt (B1 reproduction)', () => {
  test('A1 armed exact context + run1-shaped Deliver flow -> no violation, one proven context, reconciled', () => {
    const { log } = armed();
    replay(log, flow());
    assert.deepEqual(log.violations(), [], 'the single proven non-network activation attempt is not an offline violation');
    const r = log.reconcileActivations();
    assert.deepEqual({ ok: r.ok, armed: r.armed, proven: r.proven, unproven: r.unproven }, { ok: true, armed: 1, proven: 1, unproven: [] });
  });
  test('A2 the complete ordered masked log is retained (nothing filtered or deleted)', () => {
    const { log } = armed();
    replay(log, flow());
    assert.equal(log.entries.length, 6);
    const act = log.entries.find((e) => e.requestId === 'ACT' && e.method === 'Network.requestWillBeSent');
    assert.equal(act.url, `${SCHEME}://auth?state=<masked>`);
    assert.equal(act.url_class, 'external', 'classification itself is unchanged');
    assert.equal(log.entries.at(-1).method, 'Network.loadingFailed');
  });
  test('A3 settled only after the attempt AND its loadingFailed were observed', () => {
    const { log, ctx } = armed();
    const ev = flow();
    replay(log, ev.slice(0, -1));
    assert.equal(log.activationSettled(ctx), false, 'attempt without failure proof is not settled');
    replay(log, ev.slice(-1));
    assert.equal(log.activationSettled(ctx), true);
  });
  test('A4 structured proof metadata for independent E1/T2 validation; state only as commitment', () => {
    const { log } = armed();
    replay(log, flow());
    const ev = log.activationEvidence();
    assert.equal(ev.capability, 'expected-activation');
    assert.equal(ev.scheme, SCHEME);
    assert.equal(ev.origin, ORIGIN);
    assert.equal(ev.max_contexts, 16);
    assert.equal(ev.contexts.length, 1);
    const c = ev.contexts[0];
    assert.equal(c.proven, true);
    assert.deepEqual(c.errors, []);
    assert.deepEqual(c.reasons, []);
    assert.equal(c.sessionId, SID);
    assert.equal(c.callback, CALLBACK);
    assert.equal(c.state_commitment, commit(STATE));
    assert.deepEqual(c.source, { requestId: 'SRC', frameId: FRAME, seq: 2, http_method: 'POST', type: 'Document', origin_ok: true, redirect: false, status: 200, response_seq: 3 });
    assert.deepEqual(c.attempt, { requestId: 'ACT', frameId: FRAME, seq: 4, http_method: 'GET', type: 'Document', redirect: false,
      url: `${SCHEME}://auth?state=<masked>`, canonical: true, scheme_match: true, observed_state_commitment: commit(STATE), after_source_response: true });
    assert.deepEqual(c.failure, { seq: 5, errorText: 'net::ERR_ABORTED', canceled: true });
    const text = JSON.stringify({ entries: log.entries, activation: ev, log });
    assert.deepEqual(scanForSecrets(text, [STATE]), [], 'retained evidence never carries the raw state');
    assert.ok(!text.includes(STATE));
  });
});

describe('PURE UrlLog expected-activation: default and classification preserved (characterization)', () => {
  test('D1 default UrlLog (capability disabled) still reports the custom-scheme Document request', () => {
    const log = replay(new UrlLog(), flow());
    assert.equal(log.violations().length, 1);
    assert.equal(log.violations()[0].requestId, 'ACT');
  });
  test('D2 arming without the explicit opt-in capability is refused', () => {
    assert.throws(() => new UrlLog().armActivation({ state: STATE, sessionId: SID, callbackUrl: CALLBACK }), /activation capability disabled/);
  });
  test('D3 classifyUrl/maskUrl unchanged: unknown and custom protocols are external; loopback/internal positives', () => {
    assert.equal(classifyUrl(ACTIVATION), 'external');
    assert.equal(classifyUrl('foo://bar'), 'external');
    assert.equal(classifyUrl('golive://auth?state=' + STATE), 'external');
    assert.equal(classifyUrl('http://127.0.0.1:9/x'), 'loopback');
    assert.equal(classifyUrl('about:blank'), 'internal');
    assert.equal(classifyUrl('data:text/plain,x'), 'internal');
    assert.equal(maskUrl(ACTIVATION), `${SCHEME}://auth?state=<masked>`);
  });
  test('D4 loopback/internal traffic in an armed session stays clean; nothing promoted by prefix', () => {
    const { log } = armed();
    replay(log, [...flow(), ['Network.requestWillBeSent', { requestId: 'X1', frameId: FRAME, type: 'Image', request: { url: 'data:image/png;base64,AAAA', method: 'GET' } }, SID]]);
    assert.deepEqual(log.violations(), []);
    for (const url of ['http://127.0.0.1.evil.test/callback', 'https://example.test/', 'ws://10.0.0.1/', 'wss://example.test/']) {
      const x = armed();
      replay(x.log, [...flow(), ['Network.requestWillBeSent', { requestId: 'X2', frameId: FRAME, type: 'Document', request: { url, method: 'GET' } }, SID]]);
      assert.ok(x.log.violations().some((e) => e.requestId === 'X2'), url + ' remains a violation');
      assert.equal(x.log.reconcileActivations().ok, false, url + ' invalidates the context');
    }
  });
});

describe('PURE UrlLog expected-activation: forged or unproven attempts fail closed', () => {
  const variants = {
    'release scheme golive': { url: `golive://auth?state=${STATE}` },
    'other test-host scheme': { url: `golive-testhost-0123456789abcdef://auth?state=${STATE}` },
    'arbitrary custom scheme': { url: `evil://auth?state=${STATE}` },
    'wrong state': { url: `${SCHEME}://auth?state=${OTHER}` },
    'uppercase state': { url: `${SCHEME}://auth?state=${STATE.toUpperCase()}` },
    'slash variant': { url: `${SCHEME}://auth/?state=${STATE}` },
    'encoded variant': { url: `${SCHEME}://auth?state=%30${STATE.slice(1)}` },
    'extra query': { url: `${ACTIVATION}&x=1` },
    'fragment': { url: `${ACTIVATION}#f` },
    'authority variant': { url: `${SCHEME}://user@auth?state=${STATE}` },
    'token-bearing URI': { url: `${SCHEME}://auth?state=${STATE}&idToken=eyJabc` },
    'wrong origin header': { srcHeaders: { Origin: 'http://127.0.0.1:9999' } },
    'missing origin header': { srcHeaders: {} },
    'wrong callback port': { callback: 'http://127.0.0.1:1/callback' },
    'source not POST': { srcMethod: 'GET' },
    'source not Document': { srcType: 'XHR' },
    'attempt in another session': { actSid: 'OTHER-SESSION' },
    'attempt in another frame': { actFrame: 'FRAME-CHILD' },
    'attempt not Document': { actType: 'Other' },
    'attempt not GET': { actMethod: 'POST' },
    'source 403 (wrong state page)': { srcStatus: 403 },
    'source response missing': { srcStatus: null },
    'attempt before source response': { attemptFirst: true },
    'missing loadingFailed': { failure: false },
    'loadingFailed in another session': { failSid: 'OTHER-SESSION' },
    'responseReceived for the attempt': { response: true },
    'redirect on the attempt': { redirect: true },
    'duplicate attempt': { extra: [['Network.requestWillBeSent', { requestId: 'ACT2', frameId: FRAME, type: 'Document', request: { url: ACTIVATION, method: 'GET' } }, SID],
      ['Network.loadingFailed', { requestId: 'ACT2', errorText: 'net::ERR_ABORTED', canceled: true }, SID]] },
    'duplicate failure': { extra: [['Network.loadingFailed', { requestId: 'ACT', errorText: 'net::ERR_ABORTED', canceled: true }, SID]] },
    'websocket on the attempt id': { extra: [['Network.webSocketCreated', { requestId: 'ACT', url: 'ws://127.0.0.1:1/' }, SID]] },
    'duplicate source callback': { extra: [['Network.requestWillBeSent', { requestId: 'SRC2', frameId: FRAME, type: 'Document', request: { url: CALLBACK, method: 'POST', headers: { Origin: ORIGIN } } }, SID]] },
  };
  for (const [name, o] of Object.entries(variants)) {
    test(`F ${name} -> context unproven, reconcile fails, custom-scheme request stays a violation`, () => {
      const { log } = armed();
      replay(log, flow(o));
      assert.equal(log.reconcileActivations().ok, false, name + ': reconcile');
      assert.equal(log.activationEvidence().contexts[0].proven, false, name + ': proven');
      assert.ok(log.violations().some((e) => e.method === 'Network.requestWillBeSent' && e.requestId === 'ACT'), name + ': the unproven attempt remains a violation');
    });
  }
  test('F missing expected attempt (plain/expired/cancelled page) cannot pass merely because violations() is empty', () => {
    const { log } = armed();
    replay(log, flow({ attempt: false }));
    assert.deepEqual(log.violations(), []);
    const r = log.reconcileActivations();
    assert.equal(r.ok, false);
    assert.deepEqual(r.unproven, [0]);
  });
  test('F an attempt in an unarmed session stays a violation while another session is proven', () => {
    const { log } = armed();
    replay(log, [...flow(), ...flow({ sid: 'UNARMED' })]);
    assert.equal(log.violations().length, 1);
    assert.equal(log.violations()[0].sessionId, 'UNARMED');
  });
});

describe('PURE UrlLog expected-activation: bounded, validated arming', () => {
  const make = () => new UrlLog({ activation: { scheme: SCHEME, origin: ORIGIN } });
  test('B1 capability options are validated (never the release scheme or a non-loopback origin)', () => {
    for (const activation of [{ scheme: 'golive', origin: ORIGIN }, { scheme: 'evil', origin: ORIGIN }, { scheme: SCHEME + 'x', origin: ORIGIN },
      { scheme: SCHEME, origin: 'http://example.test' }, { scheme: SCHEME, origin: ORIGIN + '/' }, {}]) {
      assert.throws(() => new UrlLog({ activation }), /activation/);
    }
  });
  test('B2 arm rejects malformed state, session or callback', () => {
    for (const bad of [{ state: STATE.toUpperCase() }, { state: STATE.slice(1) }, { state: 'g'.repeat(48) }, { sessionId: '' }, { sessionId: null },
      { callbackUrl: 'http://example.test/callback' }, { callbackUrl: `http://127.0.0.1:${PORT}/callback?x=1` }, { callbackUrl: `https://127.0.0.1:${PORT}/callback` }]) {
      assert.throws(() => make().armActivation({ state: STATE, sessionId: SID, callbackUrl: CALLBACK, ...bad }), /activation/);
    }
  });
  test('B3 at most 16 contexts; one context per session', () => {
    const log = make();
    for (let i = 0; i < 16; i++) assert.equal(log.armActivation({ state: STATE, sessionId: 'S' + i, callbackUrl: CALLBACK }), i);
    assert.throws(() => log.armActivation({ state: STATE, sessionId: 'S16', callbackUrl: CALLBACK }), /activation/);
    const one = make();
    one.armActivation({ state: STATE, sessionId: SID, callbackUrl: CALLBACK });
    assert.throws(() => one.armActivation({ state: OTHER, sessionId: SID, callbackUrl: CALLBACK }), /activation/);
  });
  test('B4 two genuine flows in two armed sessions -> both proven, per-success count 2', () => {
    const log = make();
    log.armActivation({ state: STATE, sessionId: 'S-A', callbackUrl: CALLBACK });
    log.armActivation({ state: OTHER, sessionId: 'S-B', callbackUrl: CALLBACK });
    replay(log, [...flow({ sid: 'S-A' }), ...flow({ sid: 'S-B', url: `${SCHEME}://auth?state=${OTHER}` })]);
    assert.deepEqual(log.violations(), []);
    assert.deepEqual(log.reconcileActivations(), { ok: true, armed: 2, proven: 2, unproven: [] });
  });
  test('B5 no context armed -> reconcile is not ok (an expected success cannot be vacuous)', () => {
    assert.equal(make().reconcileActivations().ok, false);
  });
});
