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
  // revision1 R1: the expected MAIN frame is pinned explicitly (callers read it from Page.getFrameTree).
  const ctx = log.armActivation({ state: opts.state ?? STATE, sessionId: opts.sessionId ?? SID, frameId: opts.frameId ?? FRAME, callbackUrl: opts.callbackUrl ?? CALLBACK });
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
    // revision1 R2: exact shapes strengthened with the observed Origin and the URI commitments.
    assert.deepEqual(c.source, { requestId: 'SRC', frameId: FRAME, seq: 2, http_method: 'POST', type: 'Document', origin: ORIGIN, origin_ok: true, redirect: false, status: 200, response_seq: 3 });
    assert.deepEqual(c.attempt, { requestId: 'ACT', frameId: FRAME, seq: 4, http_method: 'GET', type: 'Document', redirect: false,
      url: `${SCHEME}://auth?state=<masked>`, canonical: true, scheme_match: true, observed_state_commitment: commit(STATE),
      observed_uri_commitment: createHash('sha256').update('nimbo-s3-activation-uri\0').update(ACTIVATION).digest('hex'), after_source_response: true });
    assert.equal(c.frameId, FRAME);
    assert.equal(c.expected_uri_commitment, c.attempt.observed_uri_commitment);
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
    assert.throws(() => new UrlLog().armActivation({ state: STATE, sessionId: SID, frameId: FRAME, callbackUrl: CALLBACK }), /activation capability disabled/);
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
      assert.throws(() => make().armActivation({ state: STATE, sessionId: SID, frameId: FRAME, callbackUrl: CALLBACK, ...bad }), /activation/);
    }
  });
  test('B3 at most 16 contexts; one context per session', () => {
    const log = make();
    for (let i = 0; i < 16; i++) assert.equal(log.armActivation({ state: STATE, sessionId: 'S' + i, frameId: FRAME, callbackUrl: CALLBACK }), i);
    assert.throws(() => log.armActivation({ state: STATE, sessionId: 'S16', frameId: FRAME, callbackUrl: CALLBACK }), /activation/);
    const one = make();
    one.armActivation({ state: STATE, sessionId: SID, frameId: FRAME, callbackUrl: CALLBACK });
    assert.throws(() => one.armActivation({ state: OTHER, sessionId: SID, frameId: FRAME, callbackUrl: CALLBACK }), /activation/);
  });
  test('B4 two genuine flows in two armed sessions -> both proven, per-success count 2', () => {
    const log = make();
    log.armActivation({ state: STATE, sessionId: 'S-A', frameId: FRAME, callbackUrl: CALLBACK });
    log.armActivation({ state: OTHER, sessionId: 'S-B', frameId: FRAME, callbackUrl: CALLBACK });
    replay(log, [...flow({ sid: 'S-A' }), ...flow({ sid: 'S-B', url: `${SCHEME}://auth?state=${OTHER}` })]);
    assert.deepEqual(log.violations(), []);
    assert.deepEqual(log.reconcileActivations(), { ok: true, armed: 2, proven: 2, unproven: [] });
  });
  test('B5 no context armed -> reconcile is not ok (an expected success cannot be vacuous)', () => {
    assert.equal(make().reconcileActivations().ok, false);
  });
});

// revision1 R1 (correction7): the caller-pinned expected MAIN frame is a mandatory, enforced field of
// each armed context. Matching source and attempt frames are not proof that either is the root frame.
describe('PURE UrlLog expected-activation: caller-pinned main frame (revision1 R1)', () => {
  const make = () => new UrlLog({ activation: { scheme: SCHEME, origin: ORIGIN } });
  test('R1a identical valid source + attempt in a wrong (child) frame -> unproven; the attempt stays a violation', () => {
    const { log } = armed();
    replay(log, flow({ srcFrame: 'FRAME-CHILD', actFrame: 'FRAME-CHILD' }));
    assert.equal(log.activationEvidence().contexts[0].proven, false, 'a matching child frame must not replace the pinned main frame');
    assert.equal(log.reconcileActivations().ok, false);
    assert.ok(log.violations().some((e) => e.method === 'Network.requestWillBeSent' && e.requestId === 'ACT'));
  });
  test('R1b positive control: source + attempt in the actual pinned main frame -> proven', () => {
    const { log } = armed();
    replay(log, flow());
    assert.equal(log.activationEvidence().contexts[0].proven, true);
    assert.deepEqual(log.violations(), []);
  });
  test('R1c the expected main-frame pin is mandatory and validated', () => {
    for (const frameId of [undefined, null, '', 42, 'a b', 'x'.repeat(129), '<script>', 'FRAME\n1']) {
      assert.throws(() => make().armActivation({ state: STATE, sessionId: SID, frameId, callbackUrl: CALLBACK }), /activation frame/, String(frameId));
    }
  });
  test('R1d the expected frame identity is retained in the context evidence', () => {
    const { log } = armed();
    replay(log, flow());
    assert.equal(log.activationEvidence().contexts[0].frameId, FRAME);
  });
  test('R1e only the source in a child frame, or only the attempt -> unproven', () => {
    for (const o of [{ srcFrame: 'FRAME-CHILD' }, { actFrame: 'FRAME-CHILD' }]) {
      const { log } = armed();
      replay(log, flow(o));
      assert.equal(log.reconcileActivations().ok, false, JSON.stringify(o));
    }
  });
});

// revision1 R2 (correction7): with the capability opted in, the ordered log itself retains the SAFE
// independently observed fields the verifier needs (CDP frameId, canonical loopback Origin or a fixed
// marker, and domain-separated URI/state commitments computed from the raw URI BEFORE masking), so
// E1/T2 never have to trust mutable context booleans. The default log shape is unchanged.
describe('PURE UrlLog expected-activation: independently observed event fields (revision1 R2)', () => {
  const uriCommit = (u) => createHash('sha256').update('nimbo-s3-activation-uri\0').update(u).digest('hex');
  const rws = (log, id) => log.entries.find((e) => e.method === 'Network.requestWillBeSent' && e.requestId === id);
  test('R2a default (disabled) log entries keep their exact original shape (characterization)', () => {
    const log = replay(new UrlLog(), flow());
    for (const e of log.entries.filter((x) => x.method === 'Network.requestWillBeSent')) {
      assert.deepEqual(Object.keys(e), ['seq', 'method', 'sessionId', 'requestId', 'url', 'url_class', 'http_method', 'type', 'redirect_from']);
    }
  });
  test('R2b opted in: the source request event carries its frame and safe Origin; the attempt event its frame and commitments', () => {
    const { log } = armed();
    replay(log, flow());
    const src = rws(log, 'SRC'), act = rws(log, 'ACT');
    assert.equal(src.frameId, FRAME);
    assert.equal(src.origin, ORIGIN);
    assert.equal(act.frameId, FRAME);
    assert.equal(act.uri_commitment, uriCommit(ACTIVATION));
    assert.equal(act.state_commitment, commit(STATE));
  });
  test('R2c context evidence carries the expected canonical-URI commitment, bound to the observed event', () => {
    const { log } = armed();
    replay(log, flow());
    const c = log.activationEvidence().contexts[0];
    assert.equal(c.expected_uri_commitment, uriCommit(ACTIVATION));
    assert.equal(c.attempt.observed_uri_commitment, rws(log, 'ACT').uri_commitment);
    assert.equal(c.source.origin, ORIGIN);
    assert.equal(c.proven, true);
  });
  test('R2d malformed or non-loopback Origin -> fixed nonsecret marker (never the raw header) and unproven', () => {
    for (const bad of ['https://evil.test', `${ORIGIN}/`, 'http://127.0.0.1:99999', 'null', 'http://localhost:4173', `${ORIGIN}\r\nX-Evil: 1`, 'http://127.0.0.1:04173', 7]) {
      const { log } = armed();
      replay(log, flow({ srcHeaders: { Origin: bad } }));
      assert.equal(rws(log, 'SRC').origin, '<invalid-origin>', String(bad));
      assert.equal(log.reconcileActivations().ok, false, String(bad));
      const text = JSON.stringify({ entries: log.entries, a: log.activationEvidence() });
      assert.ok(!text.includes('evil') && !text.includes('X-Evil') && !text.includes('localhost'), 'raw header never retained');
    }
    const { log } = armed();
    replay(log, flow({ srcHeaders: {} }));
    assert.equal(rws(log, 'SRC').origin, null);
    assert.equal(log.reconcileActivations().ok, false);
  });
  test('R2e missing or malformed event frameId -> null/marker and unproven', () => {
    for (const [f, want] of [[undefined, null], ['bad frame', '<invalid-frame>'], [42, '<invalid-frame>']]) {
      const { log } = armed();
      replay(log, flow({ srcFrame: f, actFrame: f }).map(([m, p, s]) => [m, f === undefined && p.frameId !== undefined && m === 'Network.requestWillBeSent' && p.requestId !== 'LOGIN' ? (({ frameId, ...rest }) => rest)(p) : p, s]));
      assert.equal(rws(log, 'SRC').frameId, want);
      assert.equal(rws(log, 'ACT').frameId, want);
      assert.equal(log.reconcileActivations().ok, false);
    }
  });
  test('R2f a noncanonical raw URI with the same masked shape gets a different event commitment and stays unproven', () => {
    const raw = `${SCHEME.toUpperCase()}://auth?state=${STATE}`;
    const { log } = armed();
    replay(log, flow({ url: raw }));
    const act = rws(log, 'ACT');
    assert.equal(act.url, maskUrl(ACTIVATION), 'masked shape looks plausible');
    assert.notEqual(act.uri_commitment, uriCommit(ACTIVATION));
    assert.equal(act.uri_commitment, uriCommit(raw));
    assert.equal(log.reconcileActivations().ok, false);
  });
  test('R2g retained opted-in log + evidence scan clean; no raw state anywhere', () => {
    const { log } = armed();
    replay(log, flow());
    const text = JSON.stringify({ entries: log.entries, a: log.activationEvidence() });
    assert.deepEqual(scanForSecrets(text, [STATE]), []);
  });
});
