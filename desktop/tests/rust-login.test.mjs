// CP2 S2-core L4 — label: "transport double: stdio instead of WebView2; REAL receiver over REAL
// loopback HTTP". The REAL injected script (vm double) drives the REAL Rust dispatcher and the
// REAL LoginFlow + loopback receiver inside desktop/rust/examples/bridge-stdio.rs; this harness
// plays the browser with node:http / node:net on 127.0.0.1 only. A posted result is NOT a
// Firebase sign-in: LOGIN NOT DELIVERED until the real producer/consumer proof (S2-integration).
// Synthetic credentials only. Login URLs carry the single-use state: they are never printed.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import { before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { ORIGIN, buildExample, createBridgeHost } from './helpers/webview-double.mjs';
import { loadDesktopConsumer } from './helpers/oracle.mjs';

const LABEL = 'transport double: stdio instead of WebView2; REAL receiver over REAL loopback HTTP — ';
const plain = (value) => JSON.parse(JSON.stringify(value));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const settled = (promise) => {
  const s = { done: false };
  promise.then((value) => Object.assign(s, { done: true, value }), (error) => Object.assign(s, { done: true, error }));
  return s;
};
const mainCjs = readFileSync(fileURLToPath(new URL('../main.cjs', import.meta.url)), 'utf8');
const DONE_PAGE = mainCjs.slice(mainCjs.indexOf('const LOGIN_DONE_PAGE = `') + 25).split('`')[0];
const CANCELLED = 'Login cancelado.', EXPIRED = 'O login expirou. Tente de novo.';
const MISSING = 'O Google não devolveu a credencial. Tente de novo.';
const STATE = /^[0-9a-f]{48}$/;
// S3: the example's isolated activation scheme, recomputed here (FNV-1a 64 of its fixed seed).
function fnv1a64(seed) {
  let h = 0xcbf29ce484222325n;
  for (const b of Buffer.from(seed)) h = ((h ^ BigInt(b)) * 0x100000001b3n) & 0xffffffffffffffffn;
  return h.toString(16).padStart(16, '0');
}
const SCHEME = 'golive-testhost-' + fnv1a64('bridge-stdio');
const SCRIPT = "(function(){var a=document.getElementById('open');if(a&&a.href){location.href=a.href;}})();";
const CSP = `default-src 'none'; style-src 'unsafe-inline'; script-src 'sha256-${createHash('sha256').update(SCRIPT).digest('base64')}'; base-uri 'none'; form-action 'none'`;
/** The activation page's anchor href (the state-only link); never printed. */
const hrefOf = (body) => body.match(/<a id="open" href="([^"]+)" rel="noreferrer">Abrir o app<\/a>/)?.[1];
/** Same link with one state hex digit changed. */
const forge = (href) => href.replace(/state=(.)/, (_, c) => 'state=' + (c === 'a' ? 'b' : 'a'));

// Prebuilt example binary: the override, else the debug example `buildExample` produced.
const targetDir = process.env.CARGO_TARGET_DIR || fileURLToPath(new URL('../rust/target', import.meta.url));
const BIN = process.env.NIMBO_BRIDGE_STDIO_BIN || path.join(targetDir, 'debug', 'examples', 'bridge-stdio');

before(() => buildExample(), { timeout: 15 * 60 * 1000 });

async function ready(t) {
  const h = await createBridgeHost(t, { bin: BIN });
  const page = await h.load();
  await h.flush();
  assert.equal(h.gen, 1, 'host answered hello with ready for generation 1');
  return { h, page, bridge: page.window.nimboDesktop };
}
async function waitFor(test, what, ms = 2000) {
  const end = Date.now() + ms;
  while (!test()) {
    if (Date.now() > end) throw new Error('timed out (real time) waiting for ' + what);
    await sleep(5);
  }
}
const form = (fields) => new URLSearchParams(fields).toString();
function post(port, body, { method = 'POST', path: target = '/callback', headers = {} } = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, method, path: target, agent: false,
      headers: { 'content-type': 'application/x-www-form-urlencoded', 'content-length': Buffer.byteLength(body), ...headers } }, (res) => {
      let data = '';
      res.setEncoding('utf8');
      res.on('data', (c) => { data += c; });
      res.on('end', () => resolve({ status: res.statusCode, type: res.headers['content-type'], headers: res.headers, body: data }));
    });
    req.on('error', reject);
    req.end(body);
  });
}
/** Raw bytes; resolves with whatever came back before close/reset. */
function raw(port, bytes) {
  return new Promise((resolve) => {
    const socket = net.connect(port, '127.0.0.1');
    const chunks = [];
    let done = false;
    const finish = (error) => { if (!done) { done = true; resolve({ data: Buffer.concat(chunks).toString('utf8'), error }); } };
    socket.on('data', (c) => chunks.push(c));
    socket.on('error', (e) => finish(e.code));
    socket.on('close', () => finish());
    socket.end(bytes);
  });
}
const head = (port, extra, length) => `POST /callback HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\n${extra}Content-Length: ${length}\r\n\r\n`;
async function refused(port) {
  const end = Date.now() + 2000;
  for (;;) {
    const code = await new Promise((resolve) => {
      const s = net.connect(port, '127.0.0.1');
      s.on('connect', () => { s.destroy(); resolve('open'); });
      s.on('error', (e) => resolve(e.code));
    });
    if (code === 'ECONNREFUSED') return;
    if (Date.now() > end) throw new Error('receiver port still accepting after 2 s');
    await sleep(20);
  }
}
/** Privacy: no stderr line may contain the state or a token (assert without echoing them). */
function assertPrivate(h, secrets) {
  for (const secret of secrets) {
    assert.ok(!h.stderr.some((line) => line.includes(secret)), 'stderr never carries the state or a token');
  }
}
/** Every signIn invoke the page posted got exactly one result (or none yet when `pendingOk`). */
function assertOneResultPerSignIn(h, page) {
  const ids = page.sent.filter((m) => m.method === 'nimbo:signIn').map((m) => m.id);
  for (const id of ids) {
    const n = h.toPage.filter((m) => m.kind === 'result' && m.id === id && m.gen === h.gen).length;
    assert.equal(n, 1, `signIn id ${id}: exactly one result`);
  }
}

test(LABEL + 'event-loop delivery: valid POST -> result with NO further host input (quiet guard)', async (t) => {
  const { h, page, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl();
  assert.equal(login.origin, ORIGIN);
  assert.equal(login.path, '/desktop-login');
  assert.ok(login.port >= 1024 && login.port <= 65535);
  assert.ok(STATE.test(login.state), 'state is 48 lowercase hex');
  h.quiet = true; // from here on, any stdin line to the host throws
  const res = await post(login.port, form({ state: login.state, idToken: 'synthetic-id', accessToken: 'synthetic-access' }));
  await waitFor(() => signIn.done, 'signIn result delivered by the event loop', 2000);
  h.quiet = false;
  assert.equal(res.status, 200);
  assert.equal(res.type, 'text/html; charset=utf-8');
  // S3 Deliver: the app-served activation page (D1=B1), not the plain Electron page.
  assert.notEqual(res.body, DONE_PAGE, 'Deliver serves the activation page');
  assert.equal(hrefOf(res.body), `${SCHEME}://auth?state=${login.state}`, 'href is the isolated state-only link');
  assert.equal(res.body.split(login.state).length - 1, 1, 'state exactly once (inside href)');
  for (const text of ['Pronto, você entrou.', 'Pode fechar esta aba e voltar ao app Nimbo.', 'Se o Nimbo não voltou para a frente, clique em Abrir o app.']) assert.ok(res.body.includes(text), text);
  assert.equal(res.headers['content-security-policy'], CSP, 'CSP pins the sha256 of the inline script');
  assert.equal(res.headers['referrer-policy'], 'no-referrer');
  const scripts = [...res.body.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
  assert.deepEqual(scripts, [SCRIPT], 'exactly the pinned script');
  assert.ok(Buffer.byteLength(res.body) <= 4096, 'page within 4 KiB');
  assert.ok(!res.body.includes('synthetic'));
  assert.ok(!res.body.includes('golive://'), 'never the real scheme in the test host');
  assert.deepEqual(plain(signIn.value), { ok: true, value: { idToken: 'synthetic-id', accessToken: 'synthetic-access' } });
  await refused(login.port);
  for (const code of ['login-begin', 'login-credential-delivered', 'login-result-posted']) assert.ok(h.codes.includes(code), code);
  assertPrivate(h, [login.state, 'synthetic-id', 'synthetic-access']);
  assertOneResultPerSignIn(h, page);
  const stats = await h.stats();
  assert.equal(stats.activation, true, 'activation slot reserved before activation');
});

test(LABEL + 'activation link: exactly one activation, replay and forged links are ignored, slot cleared', async (t) => {
  const { h, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl(0);
  const res = await post(login.port, form({ state: login.state, idToken: 'synthetic-id', accessToken: 'synthetic-access' }));
  const href = hrefOf(res.body);
  assert.ok(href, 'activation page href');
  await waitFor(() => signIn.done, 'signIn result');
  assert.equal((await h.stats()).activation, true);
  const send = async (link, code) => {
    const from = h.codes.length;
    h.link(link);
    await h.waitCode(code, from);
    assert.ok(!h.codes.slice(from).includes(code === 'link-activated' ? 'link-ignored' : 'link-activated'));
  };
  await send(forge(href), 'link-ignored');
  assert.equal((await h.stats()).activation, true, 'a forged link never consumes the slot');
  await send(href.replace(SCHEME, 'golive'), 'link-ignored');
  await send(href, 'link-activated');
  assert.equal((await h.stats()).activation, false, 'slot consumed');
  await send(href, 'link-ignored');
  assertPrivate(h, [login.state, 'synthetic-id', 'synthetic-access']);
  // A new signIn clears an unconsumed slot of the previous login.
  const second = settled(bridge.signIn());
  const login2 = await h.loginUrl(1);
  const href2 = hrefOf((await post(login2.port, form({ state: login2.state, idToken: 'synthetic-id' }))).body);
  await waitFor(() => second.done, 'second result');
  assert.equal((await h.stats()).activation, true);
  const third = settled(bridge.signIn());
  await h.loginUrl(2);
  assert.equal((await h.stats()).activation, false, 'new login cleared the slot');
  await send(href2, 'link-ignored');
  assert.equal(third.done, false);
  assertPrivate(h, [login.state, login2.state, 'synthetic-id', 'synthetic-access']);
});

test(LABEL + 'non-Deliver 200 pages stay the plain Electron page (no link, no state)', async (t) => {
  const { h, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl();
  // A connection accepted BEFORE delivery, completed after it: login no longer pending.
  const late = net.connect(login.port, '127.0.0.1');
  const chunks = [];
  late.on('data', (c) => chunks.push(c));
  const closed = new Promise((resolve) => { late.on('close', resolve); late.on('error', resolve); });
  late.write('POST /callback HTTP/1.1\r\n');
  await sleep(150); // accepted and registered by the acceptor (20 ms poll) before delivery
  const res = await post(login.port, form({ state: login.state, idToken: 'synthetic-id' }));
  assert.ok(hrefOf(res.body), 'the delivering POST gets the activation page');
  const body = form({ state: login.state, idToken: 'synthetic-second' });
  late.end(`Host: 127.0.0.1:${login.port}\r\nContent-Length: ${body.length}\r\n\r\n${body}`);
  await closed;
  const text = Buffer.concat(chunks).toString('utf8');
  assert.ok(text.startsWith('HTTP/1.1 200 OK\r\n'), 'late POST answered 200');
  assert.ok(text.endsWith(DONE_PAGE), 'plain Electron page on the not-pending path');
  assert.ok(!text.includes(login.state) && !text.includes('Content-Security-Policy'), 'no link on non-Deliver 200');
  await waitFor(() => signIn.done, 'one result');
  assert.deepEqual(plain(signIn.value), { ok: true, value: { idToken: 'synthetic-id', accessToken: '' } });
  h.control('tick 1'); // receiver codes reach stderr at the next host drain
  await h.waitCode('login-ignored-not-pending');
});

test(LABEL + 'contracts tests 3/4 equivalents: 404s, 403 invalid link, missing idToken', async (t) => {
  const { h, page, bridge } = await ready(t);
  const first = settled(bridge.signIn());
  const login = await h.loginUrl();
  assert.equal((await post(login.port, '', { method: 'GET' })).status, 404);
  assert.equal((await post(login.port, 'x=1', { path: '/other' })).status, 404);
  for (const state of ['short', 'f'.repeat(48), login.state.toUpperCase()]) {
    const res = await post(login.port, form({ state, idToken: 'synthetic-id' }));
    assert.equal(res.status, 403);
    assert.equal(res.type, 'text/plain; charset=utf-8');
    assert.equal(res.body, 'Link de login inválido.');
  }
  assert.equal(first.done, false, 'no lockout: still pending');
  const res = await post(login.port, form({ state: login.state, accessToken: 'only-access' }));
  assert.equal(res.status, 200);
  await waitFor(() => first.done, 'missing idToken result');
  assert.deepEqual(plain(first.value), { ok: false, error: MISSING });
  assertPrivate(h, [login.state, 'only-access']);
  assertOneResultPerSignIn(h, page);
});

test(LABEL + '64 KiB + 1 is closed without a response and the login survives', async (t) => {
  const { h, page, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl();
  const body = form({ state: login.state, idToken: 'x' }).padEnd(65537, 'a');
  const res = await raw(login.port, head(login.port, '', 65537) + body);
  assert.equal(res.data, '', 'no response byte');
  assert.equal(signIn.done, false);
  assert.equal((await h.stats()).active, true);
  const ok = await post(login.port, form({ state: login.state, idToken: 'synthetic-id' }));
  assert.equal(ok.status, 200);
  await waitFor(() => signIn.done, 'result');
  assert.deepEqual(plain(signIn.value), { ok: true, value: { idToken: 'synthetic-id', accessToken: '' } });
  assertOneResultPerSignIn(h, page);
});

test(LABEL + 'hardening over real sockets: Host, Origin, Transfer-Encoding, Content-Length', async (t) => {
  const { h, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl();
  const body = form({ state: login.state, idToken: 'synthetic-id' });
  const status = async (bytes) => Number((await raw(login.port, bytes)).data.slice(9, 12));
  assert.equal(await status(`POST /callback HTTP/1.1\r\nHost: localhost:${login.port}\r\nContent-Length: ${body.length}\r\n\r\n${body}`), 404);
  assert.equal(await status(head(login.port, 'Origin: https://evil.test\r\n', body.length) + body), 403);
  assert.equal(await status(head(login.port, 'Transfer-Encoding: chunked\r\n', body.length) + body), 400);
  assert.equal(await status(`POST /callback HTTP/1.1\r\nHost: 127.0.0.1:${login.port}\r\n\r\n`), 411);
  assert.equal(await status(head(login.port, 'Origin: null\r\n', body.length) + body), 200, 'Origin null accepted');
  await waitFor(() => signIn.done, 'result');
  assert.equal(signIn.value.ok, true);
});

test(LABEL + 'replacement and cancel settle the caller once with "Login cancelado." and close the port', async (t) => {
  const { h, page, bridge } = await ready(t);
  const a = settled(bridge.signIn());
  const loginA = await h.loginUrl(0);
  const b = settled(bridge.signIn());
  const loginB = await h.loginUrl(1);
  await waitFor(() => a.done, 'replaced caller');
  assert.deepEqual(plain(a.value), { ok: false, error: CANCELLED });
  await refused(loginA.port);
  assert.notEqual(loginA.state, loginB.state, 'fresh state per login');
  assert.deepEqual(plain(await bridge.cancelSignIn()), { ok: true });
  assert.deepEqual(plain(b.value), { ok: false, error: CANCELLED }, 'settled before the cancel result');
  await refused(loginB.port);
  assertOneResultPerSignIn(h, page);
});

test(LABEL + 'host lifetime 300 s on the virtual clock: "O login expirou. Tente de novo."', async (t) => {
  const { h, page, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl();
  h.control('tick 300000');
  await h.flush();
  assert.equal(signIn.done, false, 'the deadline itself is still valid');
  h.control('tick 1');
  await h.flush();
  assert.deepEqual(plain(signIn.value), { ok: false, error: EXPIRED });
  await refused(login.port);
  assertOneResultPerSignIn(h, page);
});

test(LABEL + '(a) raw literal-quote unit bound: 65,400 `"` delivered intact, exact outbound size', async (t) => {
  // DIRECT UNIT BOUND via node:net (not a browser body; a browser would send %22, see the Rust
  // encoded-bound test). JS .length is only used on this ASCII body.
  const { h, page, bridge } = await ready(t);
  const signIn = settled(bridge.signIn());
  const login = await h.loginUrl();
  const body = `state=${login.state}&idToken=` + '"'.repeat(65400);
  assert.ok(body.length <= 65536);
  const res = await raw(login.port, head(login.port, '', body.length) + body);
  assert.ok(res.data.startsWith('HTTP/1.1 200 OK\r\n'));
  await waitFor(() => signIn.done, 'result');
  assert.equal(signIn.value.value.idToken.length, 65400);
  assert.ok([...signIn.value.value.idToken].every((c) => c === '"'));
  const index = h.toPage.findIndex((m) => m.kind === 'result' && m.ok === true);
  const id = page.sent.find((m) => m.method === 'nimbo:signIn').id;
  const envelope = JSON.stringify({ v: 1, kind: 'result', id, gen: 1, ok: true, value: { idToken: '', accessToken: '' } });
  assert.equal(h.toPageBytes[index], Buffer.byteLength(envelope) + 2 * 65400, 'outbound JSON = envelope + 2 x 65,400');
});

test(LABEL + 'L6 unchanged src/desktop.ts consumer: desktopSignIn resolves the credential and rejects with each Electron text', async (t) => {
  const { h, bridge } = await ready(t);
  const consumer = loadDesktopConsumer(bridge);
  const ok = settled(consumer.api.desktopSignIn());
  let login = await h.loginUrl(0);
  await post(login.port, form({ state: login.state, idToken: 'synthetic-id', accessToken: 'synthetic-access' }));
  await waitFor(() => ok.done, 'consumer resolve');
  assert.deepEqual(plain(ok.value), { idToken: 'synthetic-id', accessToken: 'synthetic-access' });

  const cancelled = settled(consumer.api.desktopSignIn());
  await h.loginUrl(1);
  await bridge.cancelSignIn();
  await waitFor(() => cancelled.done, 'cancel');
  assert.equal(cancelled.error?.message, CANCELLED);

  const expired = settled(consumer.api.desktopSignIn());
  await h.loginUrl(2);
  h.control('tick 300001');
  await waitFor(() => expired.done, 'expiry');
  assert.equal(expired.error?.message, EXPIRED);

  const missing = settled(consumer.api.desktopSignIn());
  login = await h.loginUrl(3);
  await post(login.port, form({ state: login.state }));
  await waitFor(() => missing.done, 'missing');
  assert.equal(missing.error?.message, MISSING);
});

function procStatus(pid) {
  const text = readFileSync(`/proc/${pid}/status`, 'utf8');
  const field = (name) => Number(text.match(new RegExp(`^${name}:\\s+(\\d+)`, 'm'))[1]);
  return { threads: field('Threads'), rssKiB: field('VmRSS') };
}

test(LABEL + 'burst: 500 x signIn -> cancel with idle clients stays within GLOBAL budgets; shutdown gate', { timeout: 120000 }, async (t) => {
  const { h, bridge } = await ready(t);
  const base = procStatus(h.pid);
  const samples = [];
  const sample = async () => {
    const s = await h.stats();
    samples.push({ ...s, ...procStatus(h.pid) });
  };
  const idle = [];
  let busy = 0, cancelledCount = 0;
  let lastSample = Date.now();
  for (let i = 0; i < 500; i++) {
    const from = h.loginUrls.length;
    const signIn = settled(bridge.signIn());
    await h.flush();
    if (!signIn.done && i % 125 === 0) {
      const { port } = await h.loginUrl(from);
      for (let k = 0; k < 4; k++) idle.push(net.connect(port, '127.0.0.1').on('error', () => {}));
      await sleep(100);
    }
    await bridge.cancelSignIn();
    await waitFor(() => signIn.done, 'signIn settled');
    if (signIn.value.error === CANCELLED) cancelledCount++;
    else if (signIn.value.error === 'Não foi possível iniciar o login: ocupado') busy++;
    else assert.fail('unexpected signIn outcome');
    if (Date.now() - lastSample >= 100) { await sample(); lastSample = Date.now(); }
  }
  await sample();
  assert.ok(samples.length >= 3, 'sampled during the burst');
  for (const s of samples) {
    assert.ok(s.workers <= 4 && s.acceptors <= 4, `counters ${s.workers}/${s.acceptors}`);
    assert.ok(s.threads <= base.threads + 8, `OS threads ${base.threads} -> ${s.threads}`);
    assert.ok(s.rssKiB - base.rssKiB <= 32 * 1024, `RSS growth ${s.rssKiB - base.rssKiB} KiB`);
  }
  const last = samples.at(-1);
  assert.ok(last.max_workers <= 4 && last.max_acceptors <= 4, `real maxima ${last.max_workers}/${last.max_acceptors}`);
  assert.equal(cancelledCount + busy, 500);
  assert.ok(cancelledCount > 0);
  // A pending login with an idle client at quit: settled, then bounded shutdown to zero.
  await sleep(200); // let replaced acceptors drain so this login is not answered "ocupado"
  const from = h.loginUrls.length;
  const pending = settled(bridge.signIn());
  const { port } = await h.loginUrl(from);
  idle.push(net.connect(port, '127.0.0.1').on('error', () => {}));
  await sleep(50);
  const report = await h.quit();
  assert.deepEqual({ workers: report.workers, acceptors: report.acceptors }, { workers: 0, acceptors: 0 });
  assert.ok(report.waited_ms <= 2000, `bounded wait ${report.waited_ms} ms`);
  assert.deepEqual(plain(pending.value), { ok: false, error: CANCELLED });
  await refused(port);
  for (const s of idle) s.destroy();
  assert.ok(h.codes.includes('login-shutdown'));
});
