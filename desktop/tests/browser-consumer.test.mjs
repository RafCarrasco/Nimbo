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
/**
 * fix6: the bridge-stdio fixture's isolated activation scheme, derived independently from its seed
 * (`golive-testhost-<FNV-1a 64("bridge-stdio")>`, examples/bridge-stdio.rs STDIO_SCHEME_SEED). The
 * UrlLog expected-activation capability is armed per successful Deliver tab with this exact scheme.
 */
function fnv1a64Hex(seed) {
  let h = 0xcbf29ce484222325n;
  for (const b of Buffer.from(seed)) h = BigInt.asUintN(64, (h ^ BigInt(b)) * 0x100000001b3n);
  return h.toString(16).padStart(16, '0');
}
const ACTIVATION_SCHEME = 'golive-testhost-' + fnv1a64Hex('bridge-stdio');
/**
 * S3 Deliver page (D1=B1): NOT the plain page; the login state exactly once, only inside the
 * a#open href of this host's ISOLATED scheme; both Electron sentences; no token/identity data.
 * Returns sanitized booleans only (the href/state is never retained in evidence).
 */
function assertActivationPage(body, html, state) {
  assert.notEqual(body, DONE_PAGE, 'Deliver serves the S3 activation page, not the plain page');
  const href = /<a id="open" href="([^"]+)" rel="noreferrer">Abrir o app<\/a>/.exec(body)?.[1] || '';
  assert.ok(new RegExp(`^golive-testhost-[0-9a-f]{16}://auth\\?state=${state}$`).test(href), 'href is the isolated state-only activation link');
  assert.equal(body.split(state).length - 1, 1, 'state exactly once in the response (inside href)');
  assert.equal(html.split(state).length - 1, 1, 'state exactly once in the rendered document (inside href)');
  assert.ok(!body.includes('golive://'), 'never the real scheme');
  for (const text of ['Pronto, você entrou.', 'Pode fechar esta aba e voltar ao app Nimbo.', 'Abrir o app']) assert.ok(body.includes(text), text);
  for (const secret of [ALICE_ID_TOKEN, 'alice@nimbo.test', 'idToken', 'dev-alice']) assert.ok(!body.includes(secret) && !html.includes(secret), 'page carries no token/identity');
  return { activation_page: true, state_once_in_isolated_href: true };
}
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

async function produce(loginUrl, { state, activate } = {}) {
  const tab = await openTab(chrome.cdp, sessions);
  const url = new URL(loginUrl);
  if (state) url.searchParams.set('state', state);
  url.searchParams.set('devUser', 'alice');
  const callback = `http://127.0.0.1:${Number(url.searchParams.get('port'))}/callback`;
  // fix6: armed BEFORE the real action, only for an expected successful Deliver of this login.
  const activation = activate ? urlLog.armActivation({ state: activate, sessionId: tab.sessionId, callbackUrl: callback }) : null;
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
  if (activation !== null) {
    await until(() => urlLog.activationSettled(activation), 'the Deliver page activation attempt and its loadingFailed');
    assert.equal(urlLog.activationEvidence().contexts[activation].proven, true, 'exactly one proven non-network activation attempt for this Deliver page');
  }
  await chrome.cdp.send('Target.closeTarget', { targetId: tab.targetId });
  return { req, body: b.base64Encoded ? Buffer.from(b.body, 'base64').toString('utf8') : b.body, text, html, activation };
}

const appEval = (expr) => evaluate(chrome.cdp, app.sessionId, expr);
const appHas = (text, ms) => waitForPage(chrome.cdp, app.sessionId, `document.body && document.body.innerText.includes(${JSON.stringify(text)})`, 'app text ' + text, ms);
const appText = () => appEval('document.body ? document.body.innerText : ""');
const loginScreenIdle = `[...document.querySelectorAll('button')].some((b) => b.textContent.includes('Continuar com Google') && !b.disabled)`;

/**
 * Retained audit record first (sanitized per-process read/error/identity/lifecycle coverage, so
 * the saved evidence can be re-evaluated independently), then direct enforcement: a complete
 * clean audit (a.pass, which also rejects any incomplete live process), no violation, and the
 * required processes among LIVE audited processes (a vanished or terminal record never satisfies
 * coverage).
 */
function audit(label, required, { run = auditProc, into = ev.audits } = {}) {
  const a = run();
  const comms = new Set(a.processes.map((p) => p.comm));
  const live = new Set(a.processes.filter((p) => p.status === 'live').map((p) => p.comm));
  const missing = required.filter((c) => !live.has(c));
  into.push({ label, pass: a.pass, enumeration: a.enumeration, missing_required: missing, comms: [...comms].sort(), live_comms: [...live].sort(),
    violations: a.violations, incomplete: a.incomplete,
    processes: a.processes.map((p) => ({ pid: p.pid, comm: p.comm, status: p.status, identity: p.identity, lifecycle: p.lifecycle, thread_group: p.thread_group, reads: p.reads,
      errors: p.errors, fds_read: p.fds_read, fds_closed: p.fds_closed, inet_sockets: p.inet_sockets.length, non_inet_socket_inodes: p.non_inet_socket_inodes })),
    sockets: a.processes.flatMap((p) => p.inet_sockets.map((s) => ({ pid: p.pid, comm: p.comm, ...s }))) });
  assert.deepEqual(missing, [], `${label}: audited processes include ${required.join(', ')}`);
  assert.deepEqual(a.violations, [], `${label}: every socket of every process is loopback`);
  assert.equal(a.pass, true, `${label}: complete clean audit (no unread or unresolved live-process input)`);
}

describe('S3 L5 consumer', { skip: GUARD.ok ? false : 'refused outside the isolated network-none container: ' + GUARD.reasons.join('; ') }, () => {
  before(async () => {
    assert.ok(fs.existsSync(path.join(DIST, 'index.html')), 'emulator-mode bundle prebuilt at ' + DIST);
    const ready = await (await fetch(AUTH + '/')).json();
    assert.equal(ready.authEmulator?.ready, true, 'local Auth emulator running (firebase emulators:exec)');
    const pre = await emulatorAccounts();
    assert.deepEqual(pre, { status: 200, users: [] }, 'fresh in-memory Auth emulator');
    server = await startStaticServer({ root: DIST, port: Number(new URL(ORIGIN).port) });
    assert.equal(ACTIVATION_SCHEME, 'golive-testhost-2e6f92e8f8cf4af8', 'fixture scheme as observed in the run1 trace');
    urlLog = new UrlLog({ activation: { scheme: ACTIVATION_SCHEME, origin: ORIGIN } });
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
      const r = await produce(login.url, { activate: login.state });
      assert.equal(r.req.status, 200);
      assertActivationPage(r.body, r.html, login.state); // still Deliver at the receiver
      await h.waitCode('login-result-dropped-stale', codesFrom);
      await sleep(500);
      assert.equal(h.toPage.filter((m) => m.value?.idToken).length, credentialResults, 'no credential result posted to any document');
      assert.equal(await appEval(loginScreenIdle), true, 'still LoginScreen');
      assert.ok(!(await appText()).includes('Aguardando aprovação'));
      assert.deepEqual(await emulatorAccounts(), { status: 200, users: [] }, 'no Auth emulator account was created');
      ev.steps.push({ step: 'reload-stale', login_url: redact(login.url), gen_before: genBefore, gen_after: h.gen, activation_page: true,
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
      const good = await produce(login.url, { activate: login.state });
      assert.equal(good.req.status, 200);
      assert.equal(good.req.origin, ORIGIN);
      assertActivationPage(good.body, good.html, login.state);
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
    const reconciled = urlLog.reconcileActivations();
    ev.url_log = { entries: urlLog.entries, requestWillBeSent: urlLog.count('Network.requestWillBeSent'), loadingFailed: urlLog.count('Network.loadingFailed'),
      webSocketCreated: urlLog.count('Network.webSocketCreated'), violations, sessions: [...sessions.values()].map((s) => ({ type: s.type, detached: !!s.detached })),
      activation: urlLog.activationEvidence(), activation_reconcile: reconciled };
    ev.callback_requests = callbackRequests.map((r) => ({ url: maskUrl(r.url), method: r.method, origin: r.origin, status: r.status, type: r.type, remoteIPAddress: r.remoteIPAddress }));
    ev.static_requests = server.requests.slice();
    assert.ok(ev.url_log.requestWillBeSent >= 10, 'non-empty URL log');
    assert.ok(urlLog.entries.some((e) => e.url_class === 'loopback' && /^http:\/\/127\.0\.0\.1:9099\//.test(e.url || '')), 'Auth emulator traffic was observed');
    assert.deepEqual(violations, [], 'every URL is loopback/internal (except proven activation attempts)');
    assert.deepEqual(reconciled, { ok: true, armed: 2, proven: 2, unproven: [] }, 'one proven activation attempt per successful Deliver page (reload-stale, good-after-wrong)');
    audit('L5 final', ['chrome', 'node', 'java']);
    fs.mkdirSync(EVIDENCE, { recursive: true });
    const text = JSON.stringify(ev, null, 2);
    assert.deepEqual(scanForSecrets(text, [...seen, ALICE_ID_TOKEN]), [], 'retained evidence carries no state/token/key');
    fs.writeFileSync(path.join(EVIDENCE, 'l5-consumer.json'), text);
  });
});

// ================================================================ PURE caller regressions
// NOT browser integration cases and NOT guarded: the L5 `audit` used by the live tier, driven with
// synthetic auditProc results (`run`) over an injected fake /proc into a throwaway record list
// (`into`). No socket is opened, no process is started, the live evidence object is untouched.
const PHDR = '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n';
const LO_LISTEN = `   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 10000        0 111 1 0000000000000000 100 0 0 10 0\n`;
const perr = (code) => Object.assign(new Error('synthetic ' + code), { code });
const pstat = (pid, comm, state, start) => `${pid} (${comm}) ${state} 1 1 1 0 -1 4194560 100 0 0 0 1 1 0 0 20 0 1 0 ${start} 1000 100\n`;
/** `/proc/<pid>/status` excerpt in kernel layout (the thread-group fields the collector reads). */
const pstatus = (pid, comm, state, threads) => `Name:\t${comm}\nUmask:\t0022\nState:\t${state} (zombie)\nTgid:\t${pid}\nNgid:\t0\nPid:\t${pid}\nPPid:\t1\n`
  + `TracerPid:\t0\nThreads:\t${threads}\nSigQ:\t0/127698\n`;
function synthFs(spec) {
  const calls = {};
  const get = (p) => {
    const n = (calls[p] = (calls[p] || 0) + 1);
    let v = spec[p];
    if (v && v.seq) v = v.seq[Math.min(n, v.seq.length) - 1];
    if (v === undefined) throw perr('ENOENT');
    if (v instanceof Error) throw v;
    return v;
  };
  return { readFileSync: get, readdirSync: get, readlinkSync: get };
}
function tierSpec() {
  const spec = { '/sp': ['100', '101', '102', '103'] };
  for (const [pid, comm] of [[100, 'chrome'], [101, 'node'], [102, 'bridge-stdio'], [103, 'java']]) {
    spec[`/sp/${pid}/stat`] = pstat(pid, comm, 'S', 1000 + pid);
    spec[`/sp/${pid}/comm`] = comm + '\n';
    spec[`/sp/${pid}/fd`] = ['3'];
    spec[`/sp/${pid}/fd/3`] = 'socket:[111]';
    for (const t of ['tcp', 'tcp6', 'udp', 'udp6', 'raw', 'raw6']) spec[`/sp/${pid}/net/${t}`] = t === 'tcp' ? PHDR + LO_LISTEN : PHDR;
  }
  return spec;
}
const runOf = (spec) => () => auditProc({ procRoot: '/sp', fs: synthFs(spec) });
describe('PURE L5 audit caller enforcement (synthetic /proc; no socket, no process; not browser cases)', () => {
  const REQ = ['chrome', 'node', 'bridge-stdio', 'java'];
  test('K0 positive control: complete clean audit with live required processes is accepted', () => {
    const into = [];
    audit('K0', REQ, { run: runOf(tierSpec()), into });
    assert.equal(into[0].pass, true);
  });
  test('K1 incomplete audit (all net tables EACCES on java, no violation, comms present) is REJECTED by the caller', () => {
    const spec = tierSpec();
    for (const t of ['tcp', 'tcp6', 'udp', 'udp6', 'raw', 'raw6']) spec[`/sp/103/net/${t}`] = perr('EACCES');
    const into = [];
    assert.throws(() => audit('K1', REQ, { run: runOf(spec), into }), /complete clean audit/);
    assert.equal(into.length, 1, 'evidence retained before the assertion');
  });
  test('K2 retained record keeps sanitized per-process read/error/lifecycle coverage for re-evaluation', () => {
    const spec = tierSpec();
    spec['/sp/101/fd'] = perr('EACCES');
    const into = [];
    assert.throws(() => audit('K2', REQ, { run: runOf(spec), into }));
    const rec = JSON.parse(JSON.stringify(into[0]));
    assert.deepEqual(rec.enumeration, { ok: true, code: null, entries: 4 });
    assert.deepEqual(rec.incomplete, [{ pid: 101, comm: 'node', status: 'live', errors: [{ input: 'fd', code: 'EACCES' }] }]);
    const p = rec.processes.find((x) => x.pid === 101);
    for (const k of ['status', 'identity', 'lifecycle', 'reads', 'errors', 'fds_read', 'fds_closed', 'inet_sockets', 'non_inet_socket_inodes']) assert.ok(k in p, k);
    assert.equal(p.reads.fd, 'EACCES');
    assert.deepEqual(rec.live_comms, ['bridge-stdio', 'chrome', 'java', 'node']);
  });
  test('K3 a terminal (zombie) required process does not satisfy live coverage', () => {
    const spec = tierSpec();
    spec['/sp/103/stat'] = pstat(103, 'java', 'Z', 1103);
    spec['/sp/103/fd'] = [];
    for (const t of ['tcp', 'tcp6', 'udp', 'udp6', 'raw', 'raw6']) spec[`/sp/103/net/${t}`] = perr('ENOENT');
    spec['/sp/103/status'] = pstatus(103, 'java', 'Z', 1);
    spec['/sp/103/task'] = ['103'];
    const into = [];
    assert.throws(() => audit('K3', REQ, { run: runOf(spec), into }), /audited processes include/);
    assert.deepEqual(into[0].missing_required, ['java']);
  });
  /** Real-kernel shape of an extra Z leader (pid 104): fd EACCES, net ENOENT, status/task observations. */
  const withZLeader = (threads, tasks) => {
    const spec = tierSpec();
    spec['/sp'] = ['100', '101', '102', '103', '104'];
    spec['/sp/104/stat'] = pstat(104, 'python3', 'Z', 1104);
    spec['/sp/104/comm'] = 'python3\n';
    spec['/sp/104/fd'] = perr('EACCES');
    for (const t of ['tcp', 'tcp6', 'udp', 'udp6', 'raw', 'raw6']) spec[`/sp/104/net/${t}`] = perr('ENOENT');
    spec['/sp/104/status'] = pstatus(104, 'python3', 'Z', threads);
    spec['/sp/104/task'] = tasks;
    return spec;
  };
  test('K4 extra process whose leader exited while another thread is alive -> REJECTED; retained record keeps the thread-group observation', () => {
    const into = [];
    assert.throws(() => audit('K4', REQ, { run: runOf(withZLeader(2, ['104', '105'])), into }), /complete clean audit/);
    const p = JSON.parse(JSON.stringify(into[0])).processes.find((x) => x.pid === 104);
    assert.equal(p.status, 'live');
    assert.deepEqual(p.thread_group, { reads: { status: 'ok', task: 'ok', stat: 'ok' }, threads: 2, tasks: ['104', '105'], starttime: '1104', state: 'Z', proven: false });
  });
  test('K5 a genuine extra zombie is accepted; every retained process carries thread_group and the terminal proof is kept', () => {
    const into = [];
    audit('K5', REQ, { run: runOf(withZLeader(1, ['104'])), into });
    const rec = JSON.parse(JSON.stringify(into[0]));
    assert.ok(rec.processes.every((p) => 'thread_group' in p));
    assert.deepEqual(rec.processes.find((x) => x.pid === 104).thread_group,
      { reads: { status: 'ok', task: 'ok', stat: 'ok' }, threads: 1, tasks: ['104'], starttime: '1104', state: 'Z', proven: true });
    assert.deepEqual(rec.processes.filter((x) => x.pid !== 104).map((x) => x.thread_group), [null, null, null, null]);
  });
});
