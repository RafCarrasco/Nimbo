// CP2 S2-integration L5 — label: "WebView2 transport doubled by a CDP shim; auth = local Firebase
// Auth emulator, NOT Google". The UNCHANGED app bundle (emulator mode) runs in Chrome; a LABELED
// CDP shim stands in for `window.chrome.webview` and is followed by the REAL injected bridge
// script from `bridge-stdio script`. Main-frame default-context binding calls are forwarded to
// the REAL Rust dispatcher (`content-loading` first, on each new main-frame default context, over
// one ordered WebSocket); its replies are dispatched back as `message` events. The REAL loopback
// receiver gets the POST from the unchanged producer page in a second tab; the app's Firebase SDK
// then runs signInWithCredential against the local Auth emulator (127.0.0.1:9099) and the local
// Database emulator holds the approval. Runs ONLY inside the approved network-none container.
// Emulator mode forces inMemoryPersistence: session persistence CANNOT be observed here.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { after, before, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { ORIGIN, createBridgeHost, redact } from './helpers/webview-double.mjs';
import { DEFENSE_FLAGS, autoAttachAll, clickButton, evaluate, launchChrome, openTab, waitForPage, webviewShimSource } from './helpers/cdp.mjs';
import { UrlLog, auditProc, isolationGuard, maskUrl, scanForSecrets } from './helpers/offline-evidence.mjs';
import { startStaticServer } from './helpers/static-server.mjs';

const LABEL = 'L5 WebView2 transport doubled by a CDP shim; auth = local Firebase Auth emulator, NOT Google — ';
const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const DIST = path.resolve(ROOT, '..', 'producer-dist');
const EVIDENCE = path.resolve(ROOT, '..', 'evidence');
const GUARD = isolationGuard();
const AUTH = 'http://127.0.0.1:9099';
const BIND = '__nimboShimPost', DISPATCH = '__nimboShimDispatch';
const mainCjs = fs.readFileSync(path.join(ROOT, 'desktop/main.cjs'), 'utf8');
const DONE_PAGE = mainCjs.slice(mainCjs.indexOf('const LOGIN_DONE_PAGE = `') + 25).split('`')[0];
const ALICE = 'alice@nimbo.test';
const ALICE_ID_TOKEN = JSON.stringify({ sub: 'dev-alice', email: ALICE, email_verified: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(test, what, ms = 15000) {
  for (const end = Date.now() + ms; !test(); await sleep(20)) if (Date.now() > end) throw new Error('timed out waiting for ' + what);
}
async function refused(port) {
  for (const end = Date.now() + 2000; ; await sleep(20)) {
    const code = await new Promise((resolve) => { const s = net.connect(port, '127.0.0.1'); s.on('connect', () => { s.destroy(); resolve('open'); }); s.on('error', (e) => resolve(e.code)); });
    if (code === 'ECONNREFUSED') return;
    if (Date.now() > end) throw new Error('receiver port still accepting after 2 s');
  }
}
/** SDK-independent Auth emulator state (admin REST with the emulator's documented owner bearer). */
async function emulatorAccounts() {
  const r = await fetch(`${AUTH}/identitytoolkit.googleapis.com/v1/projects/demo-nimbo/accounts:batchGet?maxResults=100`, { headers: { Authorization: 'Bearer owner' } });
  const body = await r.json();
  return { status: r.status, users: (body.users || []).map((u) => ({ email: u.email ?? null,
    providers: (u.providerUserInfo || []).map((p) => p.providerId).sort(), has_localId: typeof u.localId === 'string' })) };
}

const ev = { label: LABEL.slice(0, -3), guard: GUARD, steps: [], audits: [], persistence: 'NOT OBSERVABLE: emulator mode forces inMemoryPersistence' };
const seen = [];
let chrome, server, urlLog, sessions, callbackRequests;
const app = { sessionId: null, mainFrameId: null, ctx: null, frameUrl: null, contentLoadings: 0, forwarded: 0, droppedNotMainDefault: 0, dispatched: 0, dispatchErrors: [] };
let host = null;

function onEvent(method, p, sessionId) {
  urlLog.record(method, p, sessionId);
  if (method === 'Network.requestWillBeSent' && /^http:\/\/127\.0\.0\.1:\d+\/callback$/.test(p.request?.url || '')) {
    callbackRequests.push({ sessionId, requestId: p.requestId, url: p.request.url, method: p.request.method,
      origin: p.request.headers?.Origin ?? p.request.headers?.origin ?? null, type: p.type, status: null });
  }
  if (method === 'Network.responseReceived') {
    const r = callbackRequests.find((x) => x.sessionId === sessionId && x.requestId === p.requestId);
    if (r) Object.assign(r, { status: p.response.status, remoteIPAddress: p.response.remoteIPAddress });
  }
  if (sessionId !== app.sessionId || !host) return;
  // Shim harness: the host's view of the main frame, in WebSocket arrival order.
  if (method === 'Page.frameNavigated' && !p.frame.parentId) app.frameUrl = p.frame.url;
  if (method === 'Runtime.executionContextCreated' && p.context.auxData?.isDefault && p.context.auxData.frameId === app.mainFrameId) {
    app.ctx = p.context.id;
    app.contentLoadings++;
    host.control('content-loading'); // new top-level document BEFORE any of its messages
  }
  if (method === 'Runtime.executionContextDestroyed' && p.executionContextId === app.ctx) app.ctx = null;
  if (method === 'Runtime.executionContextsCleared') app.ctx = null;
  if (method === 'Runtime.bindingCalled' && p.name === BIND) {
    if (p.executionContextId !== app.ctx) { app.droppedNotMainDefault++; return; }
    app.forwarded++;
    host.sendRaw(p.payload, { source: app.frameUrl, top: true });
  }
}

async function produce(loginUrl, { state } = {}) {
  const tab = await openTab(chrome.cdp, sessions);
  const url = new URL(loginUrl);
  if (state) url.searchParams.set('state', state);
  url.searchParams.set('devUser', 'alice');
  const callback = `http://127.0.0.1:${Number(url.searchParams.get('port'))}/callback`;
  const from = callbackRequests.length;
  await chrome.cdp.send('Page.navigate', { url: url.href }, tab.sessionId);
  await waitForPage(chrome.cdp, tab.sessionId, `document.readyState === 'complete' && [...document.querySelectorAll('button')].some((b) => b.textContent.includes('Continuar com Google'))`, 'producer page');
  assert.equal(await clickButton(chrome.cdp, tab.sessionId, 'Continuar com Google'), true);
  await waitForPage(chrome.cdp, tab.sessionId, `location.href === ${JSON.stringify(callback)} && document.readyState === 'complete'`, 'navigation to /callback');
  await until(() => callbackRequests.slice(from).some((r) => r.sessionId === tab.sessionId && r.status), 'callback response');
  const req = callbackRequests.slice(from).find((r) => r.sessionId === tab.sessionId);
  const b = await chrome.cdp.send('Network.getResponseBody', { requestId: req.requestId }, tab.sessionId);
  const text = await evaluate(chrome.cdp, tab.sessionId, 'document.body ? document.body.innerText : ""');
  const html = await evaluate(chrome.cdp, tab.sessionId, 'document.documentElement.outerHTML');
  await chrome.cdp.send('Target.closeTarget', { targetId: tab.targetId });
  return { req, body: b.base64Encoded ? Buffer.from(b.body, 'base64').toString('utf8') : b.body, text, html };
}

const appEval = (expr) => evaluate(chrome.cdp, app.sessionId, expr);
const appHas = (text, ms) => waitForPage(chrome.cdp, app.sessionId, `document.body && document.body.innerText.includes(${JSON.stringify(text)})`, 'app text ' + text, ms);
const appText = () => appEval('document.body ? document.body.innerText : ""');
const loginScreenIdle = `[...document.querySelectorAll('button')].some((b) => b.textContent.includes('Continuar com Google') && !b.disabled)`;

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

describe('S2-integration L5 consumer', { skip: GUARD.ok ? false : 'refused outside the isolated network-none container: ' + GUARD.reasons.join('; ') }, () => {
  before(async () => {
    assert.ok(fs.existsSync(path.join(DIST, 'index.html')), 'emulator-mode bundle prebuilt at ' + DIST);
    const ready = await (await fetch(AUTH + '/')).json();
    assert.equal(ready.authEmulator?.ready, true, 'local Auth emulator running (firebase emulators:exec)');
    const pre = await emulatorAccounts();
    assert.deepEqual(pre, { status: 200, users: [] }, 'fresh in-memory Auth emulator');
    server = await startStaticServer({ root: DIST, port: Number(new URL(ORIGIN).port) });
    urlLog = new UrlLog();
    callbackRequests = [];
    chrome = await launchChrome({ chrome: process.env.NIMBO_CHROME, userDataDir: path.resolve(ROOT, '..', 'chrome-l5'), flags: DEFENSE_FLAGS });
    ev.chrome = { browser: chrome.browser, flags: chrome.flags };
    sessions = await autoAttachAll(chrome.cdp, { onEvent });
  });
  after(async () => {
    if (chrome) ev.chrome.exit = await chrome.close();
    if (server) await server.close();
  });

  test(LABEL + 'cancel, reload-stale, wrong-state then fresh retry -> SDK sign-in -> pending -> approval', { timeout: 300000 }, async (t) => {
    const h = await createBridgeHost(t);
    host = h;
    t.after(() => { host = null; });
    const tab = await openTab(chrome.cdp, sessions);
    app.sessionId = tab.sessionId;
    app.mainFrameId = (await chrome.cdp.send('Page.getFrameTree', {}, tab.sessionId)).frameTree.frame.id;
    await chrome.cdp.send('Runtime.addBinding', { name: BIND }, tab.sessionId);
    await chrome.cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: webviewShimSource(BIND, DISPATCH) }, tab.sessionId);
    const injected = await h.script();
    assert.ok(injected.includes(JSON.stringify(ORIGIN)), 'REAL injected script with the host-substituted origin');
    await chrome.cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: injected }, tab.sessionId);
    let chain = Promise.resolve();
    h.pages.push({ dispatch: (msg) => {
      const ctx = app.ctx;
      chain = chain.then(() => evaluate(chrome.cdp, app.sessionId, `window.${DISPATCH}(${JSON.stringify(msg)})`, { contextId: ctx }))
        .then(() => { app.dispatched++; }, (e) => { app.dispatchErrors.push(String(e.message).slice(0, 80)); });
    } });

    await t.test('app tab WITHOUT devUser shows LoginScreen with a ready desktop bridge', async () => {
      await chrome.cdp.send('Page.navigate', { url: ORIGIN + '/' }, app.sessionId);
      await waitForPage(chrome.cdp, app.sessionId, loginScreenIdle, 'LoginScreen');
      assert.equal(await appEval('location.search'), '');
      assert.equal(await appEval('window.nimboDesktop && window.nimboDesktop.version'), 1);
      await until(() => h.toPage.some((m) => m.kind === 'ready' && m.gen === h.gen) && h.gen >= 2, 'ready for the app document');
      assert.equal(app.contentLoadings, 1);
      ev.steps.push({ step: 'load', gen: h.gen, contentLoadings: app.contentLoadings, frame: maskUrl(app.frameUrl) });
    });

    await t.test('cancel from the app\'s cancel button -> "Login cancelado."', async () => {
      h.control('tick 1000'); // fresh 1 s log window on the virtual clock (host timer), far below the 300 s lifetime
      const from = h.loginUrls.length;
      assert.equal(await clickButton(chrome.cdp, app.sessionId, 'Continuar com Google'), true);
      const login = await h.loginUrl(from);
      seen.push(login.state);
      await appHas('Termine o login no navegador que abriu.');
      assert.equal(await clickButton(chrome.cdp, app.sessionId, 'Cancelar'), true);
      await appHas('Login cancelado.');
      await waitForPage(chrome.cdp, app.sessionId, loginScreenIdle, 'LoginScreen idle after cancel');
      await refused(login.port);
      assert.ok(h.codes.includes('login-cancelled'));
      ev.steps.push({ step: 'cancel', login_url: redact(login.url), shown: 'Login cancelado.', port_refused: true });
    });

    await t.test('app reload while pending -> no result ever posted (login-result-dropped-stale on the POST)', async () => {
      h.control('tick 1000'); // fresh 1 s log window on the virtual clock (host timer), far below the 300 s lifetime
      const from = h.loginUrls.length;
      assert.equal(await clickButton(chrome.cdp, app.sessionId, 'Continuar com Google'), true);
      const login = await h.loginUrl(from);
      seen.push(login.state);
      const genBefore = h.gen, loadsBefore = app.contentLoadings;
      await chrome.cdp.send('Page.reload', {}, app.sessionId);
      await waitForPage(chrome.cdp, app.sessionId, loginScreenIdle, 'LoginScreen after reload');
      await until(() => h.gen > genBefore, 'ready for the reloaded document');
      assert.equal(app.contentLoadings, loadsBefore + 1);
      const codesFrom = h.codes.length, credentialResults = h.toPage.filter((m) => m.value?.idToken).length;
      const r = await produce(login.url);
      assert.equal(r.req.status, 200);
      assert.equal(r.body, DONE_PAGE);
      await h.waitCode('login-result-dropped-stale', codesFrom);
      await sleep(500);
      assert.equal(h.toPage.filter((m) => m.value?.idToken).length, credentialResults, 'no credential result posted to any document');
      assert.equal(await appEval(loginScreenIdle), true, 'still LoginScreen');
      assert.ok(!(await appText()).includes('Aguardando aprovação'));
      assert.deepEqual(await emulatorAccounts(), { status: 200, users: [] }, 'no Auth emulator account was created');
      ev.steps.push({ step: 'reload-stale', login_url: redact(login.url), gen_before: genBefore, gen_after: h.gen, done_page: true,
        code: 'login-result-dropped-stale', accounts_after: 0 });
    });

    await t.test('fresh click works: wrong-state POST -> still LoginScreen; correct POST -> SDK sign-in -> "Aguardando aprovação"', async () => {
      h.control('tick 1000'); // fresh 1 s log window on the virtual clock (host timer), far below the 300 s lifetime
      const from = h.loginUrls.length;
      assert.equal(await clickButton(chrome.cdp, app.sessionId, 'Continuar com Google'), true);
      const login = await h.loginUrl(from);
      seen.push(login.state);
      assert.ok(!seen.slice(0, -1).includes(login.state), 'fresh state');
      const wrong = (login.state[0] === 'a' ? 'b' : 'a') + login.state.slice(1);
      const bad = await produce(login.url, { state: wrong });
      assert.equal(bad.req.status, 403);
      assert.equal(bad.text.trim(), 'Link de login inválido.');
      await sleep(300);
      assert.ok((await appText()).includes('Termine o login no navegador que abriu.'), 'still LoginScreen, waiting');
      const good = await produce(login.url);
      assert.equal(good.req.status, 200);
      assert.equal(good.req.origin, ORIGIN);
      assert.equal(good.body, DONE_PAGE);
      for (const s of [login.state, ALICE_ID_TOKEN]) assert.ok(!good.html.includes(s), 'done page has no token/state');
      await appHas('Aguardando aprovação', 30000);
      await appHas(ALICE);
      // Receiver codes reach stderr at the next drain (the success notify), not at the 403 itself.
      assert.ok(h.codes.includes('callback-state-rejected'), 'wrong-state rejection recorded by the receiver');
      const accounts = await emulatorAccounts();
      assert.equal(accounts.status, 200);
      assert.deepEqual(accounts.users, [{ email: ALICE, providers: ['google.com'], has_localId: true }], 'SDK-independent: signInWithIdp created alice with google.com');
      // Contract endpoint as written (GET /emulator/v1/.../accounts): recorded, not relied upon.
      const contractGet = await fetch(`${AUTH}/emulator/v1/projects/demo-nimbo/accounts`, { headers: { Authorization: 'Bearer owner' } });
      ev.steps.push({ step: 'wrong-state-then-success', login_url: redact(login.url), wrong_status: bad.req.status, good_status: good.req.status,
        origin_header: good.req.origin, pending_screen: true, email_shown: ALICE, accounts: accounts.users,
        contract_get_emulator_v1_accounts_status: contractGet.status });
      await contractGet.text();
    });

    await t.test('unchanged scripts/emulator-approve.mjs -> pending screen disappears', async () => {
      const r = spawnSync(process.execPath, ['scripts/emulator-approve.mjs'], { cwd: ROOT, encoding: 'utf8', timeout: 30000 });
      assert.equal(r.status, 0, r.stderr);
      assert.match(r.stdout, /Aprovados: alice@nimbo\.test/);
      await waitForPage(chrome.cdp, app.sessionId, `document.body && !document.body.innerText.includes('Aguardando aprovação')`, 'pending screen gone', 30000);
      const text = await appText();
      assert.ok(!text.includes('Continuar com Google'), 'not back on LoginScreen');
      ev.steps.push({ step: 'approve', approve_stdout: r.stdout.trim(), pending_gone: true, heading: await appEval(`document.querySelector('h1')?.textContent ?? null`) });
    });

    assert.deepEqual(app.dispatchErrors, [], 'every host reply dispatched into the live main-frame context');
    assert.ok(!h.stderr.some((l) => seen.some((s) => l.includes(s)) || l.includes('dev-alice')), 'stderr private');
    audit('L5 end (all tier processes alive)', ['chrome', 'node', 'bridge-stdio', 'java']);
    ev.shim = { binding: BIND, contentLoadings: app.contentLoadings, forwarded: app.forwarded, droppedNotMainDefault: app.droppedNotMainDefault,
      dispatched: app.dispatched, label: 'CDP shim stands in for WebView2 chrome.webview (NOT native evidence)' };
    ev.codes = [...h.codes];
  });

  test(LABEL + 'offline EVIDENCE: CDP URL log loopback-only, /proc audit clean, retained evidence scanned', async () => {
    const violations = urlLog.violations();
    ev.url_log = { entries: urlLog.entries, requestWillBeSent: urlLog.count('Network.requestWillBeSent'), loadingFailed: urlLog.count('Network.loadingFailed'),
      webSocketCreated: urlLog.count('Network.webSocketCreated'), violations, sessions: [...sessions.values()].map((s) => ({ type: s.type, detached: !!s.detached })) };
    ev.callback_requests = callbackRequests.map((r) => ({ url: maskUrl(r.url), method: r.method, origin: r.origin, status: r.status, type: r.type, remoteIPAddress: r.remoteIPAddress }));
    ev.static_requests = server.requests.slice();
    assert.ok(ev.url_log.requestWillBeSent >= 10, 'non-empty URL log');
    assert.ok(urlLog.entries.some((e) => e.url_class === 'loopback' && /^http:\/\/127\.0\.0\.1:9099\//.test(e.url || '')), 'Auth emulator traffic was observed');
    assert.deepEqual(violations, [], 'every URL is loopback/internal');
    audit('L5 final', ['chrome', 'node', 'java']);
    fs.mkdirSync(EVIDENCE, { recursive: true });
    const text = JSON.stringify(ev, null, 2);
    assert.deepEqual(scanForSecrets(text, [...seen, ALICE_ID_TOKEN]), [], 'retained evidence carries no state/token/key');
    fs.writeFileSync(path.join(EVIDENCE, 'l5-consumer.json'), text);
  });
});
