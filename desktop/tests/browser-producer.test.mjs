// CP2 S2-integration L2 — label: "producer=branch 7bd16d0 blob bde5b24, emulator-mode bundle,
// Chrome for Testing 145 via CDP, REAL receiver". The UNCHANGED web source (AuthScreens.tsx blob
// bde5b24, identical since 7bd16d0) is built in emulator mode and served by a loopback static
// server; the REAL Rust loopback receiver runs inside the REAL bridge-stdio example and the REAL
// injected bridge script runs in the vm transport double (stdio instead of WebView2). Runs ONLY
// inside the approved network-none container (isolationGuard); refused anywhere else. Emulator
// mode: the "Google" credential is the synthetic devUser JSON — no Google, no real credential.
// The done page is the existing fixed response; it is NOT a Firebase sign-in (that is L5).
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { after, before, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { ORIGIN, createBridgeHost, redact } from './helpers/webview-double.mjs';
import { DEFENSE_FLAGS, autoAttachAll, clickButton, evaluate, launchChrome, openTab, waitForPage } from './helpers/cdp.mjs';
import { UrlLog, auditProc, isolationGuard, maskUrl, scanForSecrets } from './helpers/offline-evidence.mjs';
import { startStaticServer } from './helpers/static-server.mjs';

const LABEL = 'L2 producer=branch 7bd16d0 blob bde5b24, emulator-mode bundle, Chrome for Testing 145 via CDP, REAL receiver — ';
const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const DIST = path.resolve(ROOT, '..', 'producer-dist');
const EVIDENCE = path.resolve(ROOT, '..', 'evidence');
const GUARD = isolationGuard();
const EXPECTED_BLOBS = {
  'src/components/AuthScreens.tsx': 'bde5b24a8a2ec8d623c01295219eef21f617eb75',
  'src/session.ts': '6391aa5434bb0b18bb1d8e82736e32d0e3010d8a',
  'src/firebase.ts': '67439a8ebf6b54e60e2fe581c419fb283780db58',
};
const mainCjs = fs.readFileSync(path.join(ROOT, 'desktop/main.cjs'), 'utf8');
const DONE_PAGE = mainCjs.slice(mainCjs.indexOf('const LOGIN_DONE_PAGE = `') + 25).split('`')[0];
const ALICE_ID_TOKEN = JSON.stringify({ sub: 'dev-alice', email: 'alice@nimbo.test', email_verified: true });
const INVALID_LINK = 'Link de login inválido.';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const plain = (v) => JSON.parse(JSON.stringify(v));
const settled = (p) => { const s = { done: false }; p.then((value) => Object.assign(s, { done: true, value }), (error) => Object.assign(s, { done: true, error })); return s; };
const gitBlob = (buf) => createHash('sha1').update(`blob ${buf.length}\0`).update(buf).digest('hex');
async function until(test, what, ms = 15000) {
  for (const end = Date.now() + ms; !test(); await sleep(20)) if (Date.now() > end) throw new Error('timed out waiting for ' + what);
}

const ev = { label: LABEL.slice(0, -3), guard: GUARD, scenarios: [], audits: [], secrets_seen: [] };
let chrome, server, urlLog, sessions, callbackRequests;

/** Bundle provenance: exact source blobs, mode markers, manifest hash (values of keys never kept). */
function provenance() {
  const blobs = Object.fromEntries(Object.keys(EXPECTED_BLOBS).map((f) => [f, gitBlob(fs.readFileSync(path.join(ROOT, f)))]));
  const manifest = [];
  let blob = Buffer.alloc(0);
  const walk = (d) => { for (const n of fs.readdirSync(d).sort()) { const p = path.join(d, n); if (fs.statSync(p).isDirectory()) walk(p); else {
    const data = fs.readFileSync(p); manifest.push([path.relative(DIST, p), data.length, createHash('sha256').update(data).digest('hex')]);
    if (/\.(js|html|css|map)$/.test(n)) blob = Buffer.concat([blob, data]); } } };
  walk(DIST);
  manifest.sort();
  const fsrc = fs.readFileSync(path.join(ROOT, 'src/firebase.ts'), 'utf8');
  const keys = [...fsrc.matchAll(/apiKey: '([^']*)'/g)].map((m) => m[1]);
  const prod = keys.length === 2 ? keys[1] : null;
  return { blobs, bundle_manifest_sha256: createHash('sha256').update(JSON.stringify(manifest)).digest('hex'), bundle_files: manifest.length,
    markers: { 'demo-nimbo': blob.includes('demo-nimbo'), '127.0.0.1:9099': blob.includes('127.0.0.1:9099'),
      production_apiKey_extracted: prod !== null, production_apiKey_absent: prod !== null && !blob.includes(prod) },
    fixture_sha256: createHash('sha256').update(fs.readFileSync(process.env.NIMBO_BRIDGE_STDIO_BIN)).digest('hex') };
}

/** Drives the producer page for one login URL; returns the callback page observations. */
async function produce(loginUrl, { state }) {
  const tab = await openTab(chrome.cdp, sessions);
  const url = new URL(loginUrl);
  if (state) url.searchParams.set('state', state);
  url.searchParams.set('devUser', 'alice');
  const port = Number(url.searchParams.get('port'));
  const callback = `http://127.0.0.1:${port}/callback`;
  const from = callbackRequests.length;
  await chrome.cdp.send('Page.navigate', { url: url.href }, tab.sessionId);
  await waitForPage(chrome.cdp, tab.sessionId, `document.readyState === 'complete' && [...document.querySelectorAll('button')].some((b) => b.textContent.includes('Continuar com Google'))`, 'producer page');
  assert.equal(await evaluate(chrome.cdp, tab.sessionId, 'location.pathname'), '/desktop-login');
  assert.equal(await clickButton(chrome.cdp, tab.sessionId, 'Continuar com Google'), true, 'clicked Continuar com Google');
  await waitForPage(chrome.cdp, tab.sessionId, `location.href === ${JSON.stringify(callback)} && document.readyState === 'complete'`, 'top-level navigation to /callback');
  await until(() => callbackRequests.slice(from).some((r) => r.sessionId === tab.sessionId && r.status), 'callback response event');
  const req = callbackRequests.slice(from).find((r) => r.sessionId === tab.sessionId && r.url === callback);
  const body = await chrome.cdp.send('Network.getResponseBody', { requestId: req.requestId }, tab.sessionId);
  const page = await evaluate(chrome.cdp, tab.sessionId, `({ title: document.title, h1: document.querySelector('h1')?.textContent ?? null,
    text: document.body ? document.body.innerText : '', html: document.documentElement.outerHTML, top: window.top === window })`);
  await chrome.cdp.send('Target.closeTarget', { targetId: tab.targetId });
  return { req, body: body.base64Encoded ? Buffer.from(body.body, 'base64').toString('utf8') : body.body, page, callback };
}

function audit(label, required) {
  const a = auditProc();
  const comms = new Set(a.processes.map((p) => p.comm));
  const missing = required.filter((c) => !comms.has(c));
  ev.audits.push({ label, pass: a.pass, missing_required: missing, comms: [...comms].sort(), violations: a.violations,
    processes: a.processes.map((p) => ({ pid: p.pid, comm: p.comm, inet_sockets: p.inet_sockets.length, non_inet_socket_inodes: p.non_inet_socket_inodes })),
    sockets: a.processes.flatMap((p) => p.inet_sockets.map((s) => ({ pid: p.pid, comm: p.comm, ...s }))) });
  assert.deepEqual(missing, [], `${label}: audited processes include ${required.join(', ')}`);
  assert.deepEqual(a.violations, [], `${label}: every socket of every process is loopback`);
}

describe('S2-integration L2 producer', { skip: GUARD.ok ? false : 'refused outside the isolated network-none container: ' + GUARD.reasons.join('; ') }, () => {
  before(async () => {
    assert.ok(fs.existsSync(path.join(DIST, 'index.html')), 'emulator-mode bundle prebuilt at ' + DIST);
    ev.provenance = provenance();
    assert.deepEqual(ev.provenance.blobs, EXPECTED_BLOBS, 'served bundle is built from the unchanged web source blobs');
    assert.deepEqual(ev.provenance.markers, { 'demo-nimbo': true, '127.0.0.1:9099': true, production_apiKey_extracted: true, production_apiKey_absent: true });
    server = await startStaticServer({ root: DIST, port: Number(new URL(ORIGIN).port) });
    assert.equal(server.origin, ORIGIN);
    urlLog = new UrlLog();
    callbackRequests = [];
    chrome = await launchChrome({ chrome: process.env.NIMBO_CHROME, userDataDir: path.resolve(ROOT, '..', 'chrome-l2'), flags: DEFENSE_FLAGS });
    ev.chrome = { browser: chrome.browser, flags: chrome.flags };
    sessions = await autoAttachAll(chrome.cdp, { onEvent: (method, p, sessionId) => {
      urlLog.record(method, p, sessionId);
      if (method === 'Network.requestWillBeSent' && /^http:\/\/127\.0\.0\.1:\d+\/callback$/.test(p.request?.url || '')) {
        callbackRequests.push({ sessionId, requestId: p.requestId, url: p.request.url, method: p.request.method,
          origin: p.request.headers?.Origin ?? p.request.headers?.origin ?? null, type: p.type, status: null });
      }
      if (method === 'Network.responseReceived') {
        const r = callbackRequests.find((x) => x.sessionId === sessionId && x.requestId === p.requestId);
        if (r) Object.assign(r, { status: p.response.status, mimeType: p.response.mimeType, remoteIPAddress: p.response.remoteIPAddress });
      }
    } });
  });
  after(async () => {
    if (chrome) ev.chrome.exit = await chrome.close();
    if (server) await server.close();
  });

  test(LABEL + 'valid producer POST -> done page, REAL receiver delivers the alice credential to the vm double', { timeout: 120000 }, async (t) => {
    const h = await createBridgeHost(t);
    const page = await h.load();
    await h.flush();
    const signIn = settled(page.window.nimboDesktop.signIn());
    const login = await h.loginUrl();
    ev.secrets_seen.push(login.state);
    assert.equal(login.origin, ORIGIN, 'login URL already targets the static origin');
    assert.equal(login.path, '/desktop-login');
    const r = await produce(login.url, {});
    assert.equal(r.req.method, 'POST');
    assert.equal(r.req.type, 'Document', 'top-level form navigation');
    assert.equal(r.req.status, 200);
    assert.equal(r.req.origin, ORIGIN, 'browser sent Origin = app origin (http -> http same scheme)');
    assert.equal(r.body, DONE_PAGE, 'response is the existing fixed done page, byte-identical to main.cjs');
    assert.equal(r.page.title, 'Nimbo');
    assert.equal(r.page.h1, 'Pronto, você entrou.');
    for (const secret of [login.state, ALICE_ID_TOKEN, 'alice@nimbo.test', 'idToken']) assert.ok(!r.page.html.includes(secret), 'done page source carries no token/state');
    await until(() => signIn.done, 'signIn result in the vm double');
    assert.deepEqual(plain(signIn.value), { ok: true, value: { idToken: ALICE_ID_TOKEN, accessToken: '' } });
    for (const code of ['login-begin', 'login-credential-delivered', 'login-result-posted']) assert.ok(h.codes.includes(code), code);
    assert.ok(!h.codes.includes('callback-origin-rejected'));
    assert.ok(!h.stderr.some((l) => l.includes(login.state) || l.includes('dev-alice')), 'stderr private');
    const results = h.toPage.filter((m) => m.kind === 'result' && m.ok === true && m.value?.idToken);
    assert.equal(results.length, 1, 'exactly one credential result');
    audit('L2 valid (end)', ['chrome', 'node', 'bridge-stdio']);
    ev.scenarios.push({ name: 'valid', login_url: redact(login.url), callback_status: r.req.status, origin_header: r.req.origin,
      done_page_identical: true, title: r.page.title, h1: r.page.h1, result: 'ok:true idToken=<alice synthetic JSON> accessToken=""',
      codes: [...h.codes] });
  });

  test(LABEL + 'wrong state -> page posts -> 403 body shown, double receives nothing; correct POST later still succeeds', { timeout: 120000 }, async (t) => {
    const h = await createBridgeHost(t);
    const page = await h.load();
    await h.flush();
    const signIn = settled(page.window.nimboDesktop.signIn());
    const login = await h.loginUrl();
    ev.secrets_seen.push(login.state);
    const wrong = (login.state[0] === 'a' ? 'b' : 'a') + login.state.slice(1);
    assert.match(wrong, /^[0-9a-f]{48}$/);
    const before = h.toPage.length;
    const bad = await produce(login.url, { state: wrong });
    assert.equal(bad.req.status, 403);
    assert.equal(bad.body, INVALID_LINK);
    assert.equal(bad.page.text.trim(), INVALID_LINK, '403 body shown');
    await h.flush();
    await sleep(300);
    assert.equal(signIn.done, false, 'login stays pending');
    assert.equal(h.toPage.length, before, 'double received nothing');
    assert.equal((await h.stats()).active, true);
    const good = await produce(login.url, {});
    assert.equal(good.req.status, 200);
    assert.equal(good.body, DONE_PAGE);
    await until(() => signIn.done, 'signIn result after the correct POST');
    assert.deepEqual(plain(signIn.value), { ok: true, value: { idToken: ALICE_ID_TOKEN, accessToken: '' } });
    // Receiver codes reach stderr at the next drain (the success notify), not at the 403 itself.
    assert.ok(h.codes.includes('callback-state-rejected'), 'wrong-state rejection recorded by the receiver');
    audit('L2 wrong-state (end)', ['chrome', 'node', 'bridge-stdio']);
    ev.scenarios.push({ name: 'wrong-state-then-success', login_url: redact(login.url), wrong_status: bad.req.status, wrong_body: bad.body,
      pending_after_wrong: true, good_status: good.req.status, result: 'ok:true idToken=<alice synthetic JSON>', codes: [...h.codes] });
  });

  test(LABEL + 'offline EVIDENCE: CDP URL log loopback-only, /proc audit clean, retained evidence scanned', async () => {
    const violations = urlLog.violations();
    ev.url_log = { entries: urlLog.entries, requestWillBeSent: urlLog.count('Network.requestWillBeSent'), loadingFailed: urlLog.count('Network.loadingFailed'),
      webSocketCreated: urlLog.count('Network.webSocketCreated'), violations, sessions: [...sessions.values()].map((s) => ({ type: s.type, detached: !!s.detached })) };
    ev.callback_requests = callbackRequests.map((r) => ({ url: maskUrl(r.url), method: r.method, origin: r.origin, status: r.status, type: r.type, remoteIPAddress: r.remoteIPAddress }));
    ev.static_requests = server.requests.slice();
    assert.ok(ev.static_requests.length > 0 && ev.static_requests.every((r) => r.method === 'GET' && [200, 404].includes(r.status)), 'static server served GETs only');
    assert.ok(ev.url_log.requestWillBeSent >= 6, 'non-empty URL log');
    assert.deepEqual(violations, [], 'every URL is loopback/internal');
    audit('L2 final', ['chrome', 'node']);
    fs.mkdirSync(EVIDENCE, { recursive: true });
    const { secrets_seen: seen, ...kept } = ev;
    const text = JSON.stringify(kept, null, 2);
    const hits = scanForSecrets(text, [...seen, ALICE_ID_TOKEN]);
    assert.deepEqual(hits, [], 'retained evidence carries no state/token/key');
    fs.writeFileSync(path.join(EVIDENCE, 'l2-producer.json'), text);
  });
});
