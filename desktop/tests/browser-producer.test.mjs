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

// ================================================================ PURE helper regressions
// NOT browser integration cases and NOT guarded: synthetic /proc trees through auditProc's
// injectable `procRoot`/`fs` only — no socket is opened and no process is started, so they also
// run (and are reported separately) on the host where the live suite above is refused.
const PHDR = '  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n';
const prow = (local, rem, st, inode) => `   0: ${local} ${rem} ${st} 00000000:00000000 00:00000000 00000000 10000        0 ${inode} 1 0000000000000000 100 0 0 10 0\n`;
const P_TABLES = ['tcp', 'tcp6', 'udp', 'udp6', 'raw', 'raw6'];
const LO_LISTEN = prow('0100007F:1F90', '00000000:0000', '0A', '111');
const EXT_EST = prow('0100007F:9C40', '08080808:01BB', '01', '222');
const pstat = (pid, comm, state = 'S', start = 4242) => `${pid} (${comm}) ${state} 1 1 1 0 -1 4194560 100 0 0 0 1 1 0 0 20 0 1 0 ${start} 1000 100\n`;
/** `/proc/<pid>/status` excerpt in kernel layout (the thread-group fields the collector reads). */
const pstatus = (pid, comm, state, threads) => `Name:\t${comm}\nUmask:\t0022\nState:\t${state} (${state === 'Z' ? 'zombie' : state === 'X' ? 'dead' : 'sleeping'})\n`
  + `Tgid:\t${pid}\nNgid:\t0\nPid:\t${pid}\nPPid:\t1\nTracerPid:\t0\n${threads === null ? '' : `Threads:\t${threads}\n`}SigQ:\t0/127698\n`;
const perr = (code) => Object.assign(new Error('synthetic ' + code), { code });
/**
 * Synthetic /proc. Each value is a string/array, an Error (thrown), or a list of those under
 * `seq` (consumed per read, last one repeats) to model state changing between reads.
 */
function synthProc(spec) {
  const calls = {};
  const get = (p) => {
    const n = (calls[p] = (calls[p] || 0) + 1);
    let v = spec[p];
    if (v && v.seq) v = v.seq[Math.min(n, v.seq.length) - 1];
    if (v === undefined) throw perr('ENOENT');
    if (v instanceof Error) throw v;
    return v;
  };
  return { calls, fs: { readFileSync: (p) => get(p), readdirSync: (p) => get(p), readlinkSync: (p) => get(p) } };
}
/** One healthy process: valid stat/comm, readable fd dir and all six tables (tcp has `tcp`). */
function procEntry(spec, pid, { comm = 'chrome', tcp = PHDR + LO_LISTEN, fds = { 3: 'socket:[111]' }, start = 4242 } = {}) {
  const b = `/sp/${pid}`;
  spec[`${b}/stat`] = pstat(pid, comm, 'S', start);
  spec[`${b}/comm`] = comm + '\n';
  spec[`${b}/fd`] = Object.keys(fds);
  for (const [fd, target] of Object.entries(fds)) spec[`${b}/fd/${fd}`] = target;
  for (const t of P_TABLES) spec[`${b}/net/${t}`] = t === 'tcp' ? tcp : PHDR;
  return spec;
}
function healthySpec(opts) {
  const spec = { '/sp': ['100', 'self'] };
  return procEntry(spec, 100, opts);
}
const synthAudit = (spec) => { const s = synthProc(spec); return { a: auditProc({ procRoot: '/sp', fs: s.fs }), calls: s.calls }; };

describe('PURE auditProc regressions (synthetic /proc; no socket, no process; not browser cases)', () => {
  test('P0 positive control: healthy loopback-only live process -> complete clean audit', () => {
    const { a } = synthAudit(healthySpec());
    assert.equal(a.pass, true, JSON.stringify(a));
    assert.equal(a.processes.length, 1);
    assert.equal(a.processes[0].inet_sockets.length, 1);
  });
  test('P1 positive control: an owned external ESTABLISHED socket is a violation', () => {
    const { a } = synthAudit(healthySpec({ tcp: PHDR + LO_LISTEN + EXT_EST, fds: { 3: 'socket:[111]', 4: 'socket:[222]' } }));
    assert.equal(a.pass, false);
    assert.ok(a.violations.some((v) => v.inode === '222' && /remote/.test(v.reason)));
  });
  test('A2 fd directory EACCES on an active process holding an external socket -> NOT a clean audit', () => {
    const spec = healthySpec({ tcp: PHDR + LO_LISTEN + EXT_EST, fds: { 3: 'socket:[111]', 4: 'socket:[222]' } });
    spec['/sp/100/fd'] = perr('EACCES');
    const { a, calls } = synthAudit(spec);
    assert.equal(calls['/sp/100/fd'], 1, 'the fd directory read was actually attempted');
    assert.equal(a.processes[0].comm, 'chrome', 'valid identity metadata was read');
    assert.equal(a.pass, false, 'unreadable fd directory must not report a complete clean audit');
  });
  test('A3 one net table (tcp) EACCES on an active process -> NOT a clean audit', () => {
    const spec = healthySpec({ tcp: PHDR + LO_LISTEN + EXT_EST, fds: { 3: 'socket:[111]', 4: 'socket:[222]' } });
    spec['/sp/100/net/tcp'] = perr('EACCES');
    const { a, calls } = synthAudit(spec);
    assert.equal(calls['/sp/100/net/tcp'], 1, 'the tcp table read was actually attempted');
    assert.equal(calls['/sp/100/fd'], 1, 'fd directory was readable and read');
    assert.equal(a.pass, false, 'unreadable tcp table must not report a complete clean audit');
  });
  test('A4 all six net tables EACCES on an active process -> NOT a clean audit', () => {
    const spec = healthySpec({ tcp: PHDR + LO_LISTEN + EXT_EST, fds: { 3: 'socket:[111]', 4: 'socket:[222]' } });
    for (const t of P_TABLES) spec[`/sp/100/net/${t}`] = perr('EACCES');
    const { a, calls } = synthAudit(spec);
    assert.deepEqual(P_TABLES.map((t) => calls[`/sp/100/net/${t}`]), [1, 1, 1, 1, 1, 1], 'every table read was attempted');
    assert.equal(a.pass, false, 'all tables unreadable must not report a complete clean audit');
  });
  test('C1 explicit sanitized read/identity coverage per process (healthy and A2)', () => {
    const ok = synthAudit(healthySpec()).a.processes[0];
    assert.equal(ok.status, 'live');
    assert.deepEqual(ok.identity, { starttime: '4242', state: 'S', stable: true });
    assert.deepEqual(ok.reads, { stat: 'ok', comm: 'ok', fd: 'ok', fd_links: 'ok', 'net/tcp': 'ok', 'net/tcp6': 'ok', 'net/udp': 'ok',
      'net/udp6': 'ok', 'net/raw': 'ok', 'net/raw6': 'ok' });
    assert.deepEqual(ok.errors, []);
    const spec = healthySpec();
    spec['/sp/100/fd'] = perr('EACCES');
    const { a } = synthAudit(spec);
    const p = a.processes[0];
    assert.equal(p.reads.fd, 'EACCES');
    assert.deepEqual(p.errors, [{ input: 'fd', code: 'EACCES' }]);
    assert.deepEqual(a.incomplete.map((x) => [x.pid, x.status]), [[100, 'live']]);
    assert.ok(!JSON.stringify(a).includes('/sp') && !JSON.stringify(a).includes('synthetic'), 'no raw paths or messages retained');
  });
  const malformed = {
    'M1 garbage stat': (s) => { s['/sp/100/stat'] = 'garbage\n'; return 'stat'; },
    'M2 empty comm on a live process': (s) => { s['/sp/100/comm'] = '\n'; return 'comm'; },
    'M3 tcp table row that does not parse': (s) => { s['/sp/100/net/tcp'] = PHDR + LO_LISTEN + '   1: not-a-row\n'; return 'net/tcp'; },
    'M4 udp table without its header line (empty file)': (s) => { s['/sp/100/net/udp'] = ''; return 'net/udp'; },
    'M5 tcp6 row with a non-hex address': (s) => { s['/sp/100/net/tcp6'] = PHDR + prow('ZZ00007F:1F90', '00000000:0000', '0A', '555'); return 'net/tcp6'; },
  };
  for (const [name, mutate] of Object.entries(malformed)) {
    test(name + ' -> required input MALFORMED/EMPTY, NOT a clean audit', () => {
      const spec = healthySpec();
      const input = mutate(spec);
      const { a } = synthAudit(spec);
      assert.equal(a.pass, false, 'malformed required input must not pass');
      assert.ok(a.processes[0].errors.some((e) => e.input === input && ['MALFORMED', 'EMPTY'].includes(e.code)), JSON.stringify(a.processes[0].errors));
    });
  }
  test('N1 /proc enumeration EACCES -> recorded, fail closed (no throw, no clean audit)', () => {
    const { a } = synthAudit({ '/sp': perr('EACCES') });
    assert.deepEqual(a.enumeration, { ok: false, code: 'EACCES', entries: 0 });
    assert.equal(a.pass, false);
  });
  test('N2 /proc enumeration with no pid entries -> EMPTY, fail closed', () => {
    const { a } = synthAudit({ '/sp': ['self', 'net'] });
    assert.deepEqual(a.enumeration, { ok: false, code: 'EMPTY', entries: 0 });
    assert.equal(a.pass, false);
  });
  test('N3 healthy enumeration is recorded', () => {
    assert.deepEqual(synthAudit(healthySpec()).a.enumeration, { ok: true, code: null, entries: 1 });
  });
  // Lifecycle: an error alone never proves exit. Proven exit = stat recheck ENOENT AND the pid
  // absent from one bounded re-enumeration; terminal = the same start time read twice in state Z/X
  // plus the whole-thread-group proof (T1-T18 below).
  test('L1 proven vanished process (stat ENOENT + absent on re-enumeration) is classified, not counted live', () => {
    const spec = healthySpec();
    spec['/sp'] = { seq: [['100', '200'], ['100']] };
    const { a } = synthAudit(spec);
    const p = a.processes.find((x) => x.pid === 200);
    assert.equal(p.status, 'vanished');
    assert.deepEqual(p.lifecycle, { stat_recheck: 'ENOENT', listed_on_reenumeration: false });
    assert.deepEqual(a.incomplete, []);
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
  });
  test('L2 ENOENT on fd/tables while the pid is still listed with a stable live stat -> unresolved, fail', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    spec['/sp/200/stat'] = pstat(200, 'chrome');
    spec['/sp/200/comm'] = 'chrome\n';
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'live');
    assert.deepEqual(a.incomplete.map((x) => x.pid), [200]);
    assert.equal(a.pass, false);
  });
  test('L3 stat recheck ENOENT but the pid is STILL listed -> not proof of exit, fail', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'live');
    assert.equal(a.pass, false);
  });
  test('L4 terminal (zombie) process: same start time in state Z on both stat reads -> classified terminal', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    spec['/sp/200/stat'] = pstat(200, 'chrome', 'Z', 777);
    spec['/sp/200/comm'] = 'chrome\n';
    spec['/sp/200/fd'] = [];
    spec['/sp/200/status'] = pstatus(200, 'chrome', 'Z', 1);
    spec['/sp/200/task'] = ['200'];
    const { a } = synthAudit(spec);
    const p = a.processes.find((x) => x.pid === 200);
    assert.equal(p.status, 'terminal');
    assert.deepEqual(p.identity, { starttime: '777', state: 'Z', stable: true });
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
  });
  test('L5 empty comm on a stable state-S process is not proof of exit -> fail', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    spec['/sp/200/stat'] = pstat(200, 'chrome', 'S', 777);
    spec['/sp/200/comm'] = '\n';
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'live');
    assert.equal(a.pass, false);
  });
  test('F1 fd closed mid-audit: readlink ENOENT AND absent from one fd re-listing -> counted closed, clean', () => {
    const spec = healthySpec({ fds: { 3: 'socket:[111]', 4: 'socket:[222]' } });
    spec['/sp/100/fd'] = { seq: [['3', '4'], ['3']] };
    spec['/sp/100/fd/4'] = perr('ENOENT');
    const { a } = synthAudit(spec);
    assert.equal(a.processes[0].fds_closed, 1);
    assert.equal(a.processes[0].reads.fd_links, 'ok');
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
  });
  test('F2 readlink ENOENT while the fd is STILL listed -> unresolved fd race, fail', () => {
    const spec = healthySpec({ fds: { 3: 'socket:[111]', 4: 'socket:[222]' } });
    spec['/sp/100/fd/4'] = perr('ENOENT');
    const { a } = synthAudit(spec);
    assert.ok(a.processes[0].errors.some((e) => e.input === 'fd_links' && e.code === 'ENOENT'));
    assert.equal(a.pass, false);
  });
  test('F3 readlink EACCES -> unreadable fd, fail', () => {
    const spec = healthySpec({ fds: { 3: 'socket:[111]', 4: 'socket:[222]' } });
    spec['/sp/100/fd/4'] = perr('EACCES');
    const { a } = synthAudit(spec);
    assert.ok(a.processes[0].errors.some((e) => e.input === 'fd_links' && e.code === 'EACCES'));
    assert.equal(a.pass, false);
  });
  test('S1 self-audit: the auditor\'s own fd-directory handle is listed but must not become an unresolved fd race', () => {
    // Linux semantics: listing /proc/<self>/fd shows the descriptor used to read that directory.
    // A one-shot readdir closes it before any readlink (ENOENT, and a re-listing shows the same
    // number again); a held directory handle keeps it valid while the links are read.
    const spec = healthySpec({ fds: { 0: 'pipe:[1]', 3: 'socket:[111]' } });
    const open = new Set();
    const lowest = () => { let n = 25; while (open.has(n)) n++; return n; };
    const base = synthProc(spec).fs;
    const listing = () => ['0', '3', ...[...open].map(String)];
    const fsx = {
      ...base,
      readdirSync: (p) => { if (p !== '/sp/100/fd') return base.readdirSync(p); const n = lowest(); open.add(n); const l = listing(); open.delete(n); return l; },
      opendirSync: (p) => {
        if (p !== '/sp/100/fd') throw new Error('unexpected opendir ' + p);
        const n = lowest(); open.add(n);
        const entries = listing().map((name) => ({ name }));
        return { readSync: () => entries.shift() ?? null, closeSync: () => { open.delete(n); } };
      },
      readlinkSync: (p) => {
        const m = /^\/sp\/100\/fd\/(\d+)$/.exec(p);
        if (m && Number(m[1]) >= 25) { if (open.has(Number(m[1]))) return '/sp/100/fd'; throw perr('ENOENT'); }
        return base.readlinkSync(p);
      },
    };
    const a = auditProc({ procRoot: '/sp', fs: fsx });
    assert.equal(a.processes[0].reads.fd_links, 'ok', JSON.stringify(a.processes[0].errors));
    assert.equal(a.pass, true);
    assert.equal(open.size, 0, 'every directory handle closed');
  });
  test('V1 a violation observed before the process provably vanished is preserved -> fail', () => {
    const spec = healthySpec();
    spec['/sp'] = { seq: [['100', '200'], ['100']] };
    procEntry(spec, 200, { tcp: PHDR + EXT_EST, fds: { 4: 'socket:[222]' }, start: 777 });
    spec['/sp/200/net/udp'] = perr('ENOENT');
    spec['/sp/200/stat'] = { seq: [pstat(200, 'chrome', 'S', 777), perr('ENOENT')] };
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'vanished');
    assert.ok(a.violations.some((v) => v.pid === 200 && v.inode === '222'));
    assert.equal(a.pass, false);
  });
  test('V2 an external socket is still detected when another table of the process is unreadable', () => {
    const spec = healthySpec({ tcp: PHDR + EXT_EST, fds: { 4: 'socket:[222]' } });
    spec['/sp/100/net/udp6'] = perr('EACCES');
    const { a } = synthAudit(spec);
    assert.ok(a.violations.some((v) => v.inode === '222'));
    assert.deepEqual(a.incomplete.map((x) => x.errors), [[{ input: 'net/udp6', code: 'EACCES' }]]);
    assert.equal(a.pass, false);
  });
  test('U1 legitimate non-inet (unix) socket inode in no inet table -> counted, clean', () => {
    const { a } = synthAudit(healthySpec({ fds: { 3: 'socket:[111]', 5: 'socket:[909]', 6: 'pipe:[1]', 7: '/dev/null' } }));
    assert.equal(a.processes[0].non_inet_socket_inodes, 1);
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
  });
  test('H1 valid header-only tables and no fds -> complete clean audit', () => {
    const { a } = synthAudit(healthySpec({ tcp: PHDR, fds: {} }));
    assert.equal(a.processes[0].inet_sockets.length, 0);
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
  });
  test('I1 live identity change (start time differs between the two stat reads, all reads ok) -> CHANGED, fail', () => {
    const spec = healthySpec();
    spec['/sp/100/stat'] = { seq: [pstat(100, 'chrome', 'S', 4242), pstat(100, 'chrome', 'S', 9999)] };
    const { a } = synthAudit(spec);
    assert.equal(a.processes[0].identity.stable, false);
    assert.ok(a.processes[0].errors.some((e) => e.input === 'identity' && e.code === 'CHANGED'));
    assert.equal(a.pass, false, 'PID reuse during the audit is unresolved');
  });
  test('R1 every read ok but the identity recheck fails (exit right after the reads) -> proven vanished, never stable live', () => {
    const spec = healthySpec();
    spec['/sp'] = { seq: [['100', '200'], ['100']] };
    procEntry(spec, 200, { comm: 'node', start: 777 });
    spec['/sp/200/stat'] = { seq: [pstat(200, 'node', 'S', 777), perr('ENOENT')] };
    const { a } = synthAudit(spec);
    const p = a.processes.find((x) => x.pid === 200);
    assert.equal(p.status, 'vanished');
    assert.deepEqual(p.errors, [{ input: 'identity', code: 'ENOENT' }]);
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
  });
  test('R2 every read ok, identity recheck ENOENT but the pid is still listed -> unresolved, fail', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    procEntry(spec, 200, { comm: 'node', start: 777 });
    spec['/sp/200/stat'] = { seq: [pstat(200, 'node', 'S', 777), perr('ENOENT')] };
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'live');
    assert.equal(a.pass, false);
  });
  test('I2 pid reused while its old fd/tables vanished (recheck = new start time, still listed) -> fail', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    spec['/sp/200/stat'] = { seq: [pstat(200, 'chrome', 'S', 777), pstat(200, 'node', 'S', 999)] };
    spec['/sp/200/comm'] = 'chrome\n';
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'live');
    assert.equal(a.pass, false);
  });
  test('I3 state Z on recheck but a different start time -> not terminal, fail', () => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    spec['/sp/200/stat'] = { seq: [pstat(200, 'chrome', 'S', 777), pstat(200, 'chrome', 'Z', 778)] };
    spec['/sp/200/comm'] = 'chrome\n';
    spec['/sp/200/fd'] = [];
    const { a } = synthAudit(spec);
    assert.equal(a.processes.find((x) => x.pid === 200).status, 'live');
    assert.equal(a.pass, false);
  });

  // Thread-group corroboration (F1-R). A leader in state Z/X is not whole-group exit: on Linux the
  // leader can pthread_exit while another thread keeps running and holding sockets; then the leader's
  // fd directory is EACCES and its net tables ENOENT. "terminal" needs, after the reads, ONE bounded
  // read each of status (Threads), the task listing and stat: Threads 1, the task listing exactly the
  // leader, and the same start time still in Z/X. Anything else stays an incomplete live process.
  /** Real-kernel shape of a Z leader (pid 200, start 777): fd EACCES, all six net tables ENOENT. */
  const zSpec = ({ threads = 1, tasks = ['200'], state = 'Z' } = {}) => {
    const spec = healthySpec();
    spec['/sp'] = ['100', '200'];
    spec['/sp/200/stat'] = pstat(200, 'python3', state, 777);
    spec['/sp/200/comm'] = 'python3\n';
    spec['/sp/200/fd'] = perr('EACCES');
    for (const t of P_TABLES) spec[`/sp/200/net/${t}`] = perr('ENOENT');
    spec['/sp/200/status'] = pstatus(200, 'python3', state, threads);
    spec['/sp/200/task'] = tasks;
    return spec;
  };
  const z200 = (a) => a.processes.find((x) => x.pid === 200);
  test('T1 genuine ordinary zombie (Threads 1, task listing exactly the leader, same start time in Z) -> terminal, clean', () => {
    const { a, calls } = synthAudit(zSpec());
    assert.equal(z200(a).status, 'terminal');
    assert.deepEqual(z200(a).lifecycle, { stat_recheck: 'ok', state: 'Z' });
    assert.deepEqual(a.incomplete, []);
    assert.equal(a.pass, true, JSON.stringify(a.incomplete));
    assert.deepEqual([calls['/sp/200/status'], calls['/sp/200/task'], calls['/sp/200/stat']], [1, 1, 3], 'one bounded corroboration read each');
  });
  test('T2 terminal record retains its whole-thread-group proof', () => {
    assert.deepEqual(z200(synthAudit(zSpec()).a).thread_group,
      { reads: { status: 'ok', task: 'ok', stat: 'ok' }, threads: 1, tasks: ['200'], starttime: '777', state: 'Z', proven: true });
  });
  test('T3 leader exited, another thread still alive (Z leader, Threads 2, two tasks) -> NOT terminal, incomplete, fail', () => {
    const { a } = synthAudit(zSpec({ threads: 2, tasks: ['200', '201'] }));
    const p = z200(a);
    assert.equal(p.status, 'live', 'a surviving thread means the process is alive');
    assert.equal(p.lifecycle, null);
    assert.deepEqual(a.incomplete.map((x) => x.pid), [200]);
    assert.equal(a.pass, false, 'unread fd/net of a live thread group must not pass');
    assert.deepEqual(p.thread_group, { reads: { status: 'ok', task: 'ok', stat: 'ok' }, threads: 2, tasks: ['200', '201'], starttime: '777', state: 'Z', proven: false });
  });
  const unproven = {
    'T4 Threads 1 but the task listing has two tasks (mismatch)': { threads: 1, tasks: ['200', '201'] },
    'T5 Threads 2 but the task listing has only the leader (mismatch)': { threads: 2, tasks: ['200'] },
    'T6 task listing names only another thread, not the leader': { threads: 1, tasks: ['201'] },
    'T7 status has no Threads line (malformed)': { threads: null, read: 'status', code: 'MALFORMED' },
    'T8 status EACCES': { mut: (s) => { s['/sp/200/status'] = perr('EACCES'); }, read: 'status', code: 'EACCES' },
    'T9 status missing (ENOENT)': { mut: (s) => { delete s['/sp/200/status']; }, read: 'status', code: 'ENOENT' },
    'T10 task listing EACCES': { mut: (s) => { s['/sp/200/task'] = perr('EACCES'); }, read: 'task', code: 'EACCES' },
    'T11 task listing empty': { tasks: [], read: 'task', code: 'EMPTY' },
    'T12 task listing with a non-numeric entry (malformed)': { tasks: ['200', 'x'], read: 'task', code: 'MALFORMED' },
    'T13 identity changes during the corroboration (third stat: new start time)': { mut: (s) => { s['/sp/200/stat'] = { seq: [pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'Z', 777), pstat(200, 'node', 'S', 999)] }; } },
    'T14 stat disappears during the corroboration (third stat ENOENT, still listed)': { mut: (s) => { s['/sp/200/stat'] = { seq: [pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'Z', 777), perr('ENOENT')] }; }, read: 'stat', code: 'ENOENT' },
    'T15 stat malformed during the corroboration': { mut: (s) => { s['/sp/200/stat'] = { seq: [pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'Z', 777), 'garbage\n'] }; }, read: 'stat', code: 'MALFORMED' },
  };
  for (const [name, c] of Object.entries(unproven)) {
    test(name + ' -> no whole-group proof, NOT terminal, fail', () => {
      const spec = zSpec(c);
      if (c.mut) c.mut(spec);
      const { a } = synthAudit(spec);
      const p = z200(a);
      assert.equal(p.status, 'live');
      assert.equal(p.thread_group.proven, false);
      if (c.read) assert.equal(p.thread_group.reads[c.read], c.code);
      assert.deepEqual(a.incomplete.map((x) => x.pid), [200]);
      assert.equal(a.pass, false);
    });
  }
  test('T16 state X (dead) leader with complete whole-group proof -> terminal', () => {
    const { a } = synthAudit(zSpec({ state: 'X' }));
    assert.equal(z200(a).status, 'terminal');
    assert.equal(a.pass, true);
  });
  test('T17 healthy multithreaded live process: no corroboration reads, thread_group null, clean', () => {
    const spec = healthySpec();
    spec['/sp/100/status'] = pstatus(100, 'chrome', 'S', 4);
    spec['/sp/100/task'] = ['100', '101', '102', '103'];
    const { a, calls } = synthAudit(spec);
    assert.equal(a.processes[0].status, 'live');
    assert.equal(a.processes[0].thread_group, null);
    assert.equal(calls['/sp/100/status'], undefined);
    assert.equal(calls['/sp/100/task'], undefined);
    assert.equal(a.pass, true);
  });
  test('T18 vanished process keeps thread_group null (lifecycle path unchanged)', () => {
    const spec = healthySpec();
    spec['/sp'] = { seq: [['100', '200'], ['100']] };
    const { a } = synthAudit(spec);
    assert.equal(z200(a).status, 'vanished');
    assert.equal(z200(a).thread_group, null);
  });
  test('T19 corroboration stat shows another zombie (state Z, different start time: pid reused) -> NOT terminal, fail', () => {
    const spec = zSpec();
    spec['/sp/200/stat'] = { seq: [pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'Z', 778)] };
    const { a } = synthAudit(spec);
    assert.equal(z200(a).status, 'live');
    assert.deepEqual([z200(a).thread_group.starttime, z200(a).thread_group.proven], ['778', false]);
    assert.equal(a.pass, false);
  });
  test('T20 corroboration stat no longer in Z/X (same start time, state S) -> NOT terminal, fail', () => {
    const spec = zSpec();
    spec['/sp/200/stat'] = { seq: [pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'Z', 777), pstat(200, 'python3', 'S', 777)] };
    const { a } = synthAudit(spec);
    assert.equal(z200(a).status, 'live');
    assert.equal(z200(a).thread_group.proven, false);
    assert.equal(a.pass, false);
  });
});

// PURE caller regressions: the L2 `audit` used by the live tier, driven with synthetic auditProc
// results (`run`) into a throwaway record list (`into`); the live evidence object is untouched.
describe('PURE L2 audit caller enforcement (synthetic /proc; no socket, no process; not browser cases)', () => {
  const REQ = ['chrome', 'node', 'bridge-stdio'];
  const tierSpec = () => {
    const spec = { '/sp': ['100', '101', '102'] };
    procEntry(spec, 100, { comm: 'chrome' });
    procEntry(spec, 101, { comm: 'node', start: 5000 });
    procEntry(spec, 102, { comm: 'bridge-stdio', start: 6000 });
    return spec;
  };
  const runOf = (spec) => () => auditProc({ procRoot: '/sp', fs: synthProc(spec).fs });
  test('K0 positive control: complete clean audit with live required processes is accepted', () => {
    const into = [];
    audit('K0', REQ, { run: runOf(tierSpec()), into });
    assert.equal(into.length, 1);
    assert.equal(into[0].pass, true);
  });
  test('K1 incomplete audit (A2 fd EACCES on chrome, no violation, comms present) is REJECTED by the caller', () => {
    const spec = tierSpec();
    spec['/sp/100/fd'] = perr('EACCES');
    const into = [];
    assert.throws(() => audit('K1', REQ, { run: runOf(spec), into }), /complete clean audit/);
    assert.equal(into.length, 1, 'evidence retained before the assertion');
  });
  test('K2 retained record keeps sanitized per-process read/error/lifecycle coverage for re-evaluation', () => {
    const spec = tierSpec();
    spec['/sp/100/net/tcp'] = perr('EACCES');
    const into = [];
    assert.throws(() => audit('K2', REQ, { run: runOf(spec), into }));
    const rec = JSON.parse(JSON.stringify(into[0]));
    assert.deepEqual(rec.enumeration, { ok: true, code: null, entries: 3 });
    assert.deepEqual(rec.incomplete, [{ pid: 100, comm: 'chrome', status: 'live', errors: [{ input: 'net/tcp', code: 'EACCES' }] }]);
    const p = rec.processes.find((x) => x.pid === 100);
    for (const k of ['status', 'identity', 'lifecycle', 'reads', 'errors', 'fds_read', 'fds_closed', 'inet_sockets', 'non_inet_socket_inodes']) assert.ok(k in p, k);
    assert.equal(p.reads['net/tcp'], 'EACCES');
    assert.deepEqual(rec.live_comms, ['bridge-stdio', 'chrome', 'node']);
  });
  test('K3 a provably vanished required process does not satisfy live coverage', () => {
    const spec = tierSpec();
    spec['/sp'] = { seq: [['100', '101', '102'], ['100', '101']] };
    spec['/sp/102/stat'] = { seq: [pstat(102, 'bridge-stdio', 'S', 6000), perr('ENOENT')] };
    spec['/sp/102/fd'] = perr('ENOENT');
    const into = [];
    assert.throws(() => audit('K3', REQ, { run: runOf(spec), into }), /audited processes include/);
    assert.deepEqual(into[0].missing_required, ['bridge-stdio']);
    assert.equal(into[0].pass, true, 'the audit itself is complete; only live coverage is missing');
  });
  /** Real-kernel shape of a Z leader: fd EACCES, net ENOENT, plus its status/task observations. */
  const zLeader = (spec, pid, comm, start, threads, tasks) => {
    spec[`/sp/${pid}/stat`] = pstat(pid, comm, 'Z', start);
    spec[`/sp/${pid}/fd`] = perr('EACCES');
    for (const t of P_TABLES) spec[`/sp/${pid}/net/${t}`] = perr('ENOENT');
    spec[`/sp/${pid}/status`] = pstatus(pid, comm, 'Z', threads);
    spec[`/sp/${pid}/task`] = tasks;
  };
  test('K4 extra process whose leader exited while another thread is alive -> REJECTED; retained record keeps the thread-group observation', () => {
    const spec = tierSpec();
    spec['/sp'] = ['100', '101', '102', '103'];
    procEntry(spec, 103, { comm: 'python3', start: 7000 });
    zLeader(spec, 103, 'python3', 7000, 2, ['103', '104']);
    const into = [];
    assert.throws(() => audit('K4', REQ, { run: runOf(spec), into }), /complete clean audit/);
    const p = JSON.parse(JSON.stringify(into[0])).processes.find((x) => x.pid === 103);
    assert.equal(p.status, 'live');
    assert.deepEqual(p.thread_group, { reads: { status: 'ok', task: 'ok', stat: 'ok' }, threads: 2, tasks: ['103', '104'], starttime: '7000', state: 'Z', proven: false });
  });
  test('K5 a genuine extra zombie is accepted; every retained process carries thread_group and the terminal proof is kept', () => {
    const spec = tierSpec();
    spec['/sp'] = ['100', '101', '102', '103'];
    procEntry(spec, 103, { comm: 'python3', start: 7000 });
    zLeader(spec, 103, 'python3', 7000, 1, ['103']);
    const into = [];
    audit('K5', REQ, { run: runOf(spec), into });
    const rec = JSON.parse(JSON.stringify(into[0]));
    assert.ok(rec.processes.every((p) => 'thread_group' in p));
    assert.deepEqual(rec.processes.find((x) => x.pid === 103).thread_group,
      { reads: { status: 'ok', task: 'ok', stat: 'ok' }, threads: 1, tasks: ['103'], starttime: '7000', state: 'Z', proven: true });
    assert.deepEqual(rec.processes.filter((x) => x.pid !== 103).map((x) => x.thread_group), [null, null, null]);
  });
});
