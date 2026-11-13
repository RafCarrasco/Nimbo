// TRANSPORT DOUBLE: stdio instead of WebView2. Runs the REAL injected bridge script (with the
// origin substituted by the REAL Rust code) in a vm whose `chrome.webview` double forwards
// page messages to the REAL Rust dispatcher (desktop/rust/examples/bridge-stdio.rs) and
// delivers its replies back as `message` events. Timers are fake and advanced explicitly.
// This proves the JS <-> Rust protocol only; native WebView2 delivery, frame attribution and
// script injection remain Windows NATIVE-GATE items. Synthetic loopback data only.
// CP2 S2-core: the example also runs the REAL loopback login receiver. Login URLs (which carry
// the single-use state) are kept in memory and only ever printed through `redact`; stdout is
// consumed by this pipe only. `quiet` makes any host input throw (event-loop delivery proof).
// NIMBO_BRIDGE_STDIO_BIN, when set, runs that prebuilt binary instead of `cargo run` (no build).
import { spawn, spawnSync } from 'node:child_process';
import readline from 'node:readline';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

export const ORIGIN = 'http://127.0.0.1:4173';
const manifest = fileURLToPath(new URL('../../rust/Cargo.toml', import.meta.url));
const cargo = ['+1.99.1', '--quiet', '--manifest-path', manifest, '--locked', '--features', 'test-host', '--example', 'bridge-stdio'];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export function buildExample() {
  if (process.env.NIMBO_BRIDGE_STDIO_BIN) return;
  const r = spawnSync('cargo', [cargo[0], 'build', ...cargo.slice(1)], { encoding: 'utf8', timeout: 15 * 60 * 1000 });
  if (r.status !== 0) throw new Error(`bridge-stdio build failed: ${r.error?.message || r.stderr}`);
}

function fakeClock() {
  let now = 0, seq = 0;
  const timers = new Map();
  return {
    setTimeout: (fn, ms) => { timers.set(++seq, { at: now + Number(ms || 0), fn }); return seq; },
    clearTimeout: (id) => { timers.delete(id); },
    pending: () => timers.size,
    advance(ms) {
      const end = now + ms;
      for (;;) {
        const next = [...timers].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
        if (!next) break;
        timers.delete(next[0]);
        now = next[1].at;
        next[1].fn();
      }
      now = end;
    },
  };
}

/** Login URL with the state removed (the only form that may ever be printed). */
export const redact = (url) => String(url).replace(/state=[^&]*/g, 'state=<redacted>');

export async function createBridgeHost(t, { bin = process.env.NIMBO_BRIDGE_STDIO_BIN } = {}) {
  const args = ['--test-host-url', ORIGIN + '/'];
  const child = bin
    ? spawn(bin, args, { stdio: ['pipe', 'pipe', 'pipe'] })
    : spawn('cargo', [cargo[0], 'run', ...cargo.slice(1), '--', ...args], { stdio: ['pipe', 'pipe', 'pipe'] });
  const h = { codes: [], stderr: [], toPage: [], toPageBytes: [], loginUrls: [], held: [], hold: false, quiet: false, pages: [], clock: fakeClock(), pid: child.pid };
  const waiters = [];
  const settle = () => { for (const w of [...waiters]) if (w.test()) { waiters.splice(waiters.indexOf(w), 1); w.resolve(); } };
  const until = (test, what, ms = 60000) => new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out waiting for ' + what)), ms);
    waiters.push({ test, resolve: () => { clearTimeout(timer); resolve(); } });
    settle();
  });
  let lastDump, lastScript, lastStats;
  readline.createInterface({ input: child.stdout }).on('line', (line) => {
    const m = JSON.parse(line);
    if (m.to === 'page') {
      h.toPage.push(m.msg);
      h.toPageBytes.push(Buffer.byteLength(line) - Buffer.byteLength('{"to":"page","msg":}'));
      if (m.msg.kind === 'ready') h.gen = m.msg.gen;
      if (h.hold) h.held.push(m.msg);
      else h.deliver(m.msg);
    } else if ('dump' in m) lastDump = m.dump;
    else if ('script' in m) lastScript = m.script;
    else if ('login-url' in m) h.loginUrls.push(m['login-url']);
    else if ('stats' in m) lastStats = m.stats;
    else if ('shutdown' in m) h.shutdownReport = m.shutdown;
    settle();
  });
  readline.createInterface({ input: child.stderr }).on('line', (line) => {
    h.stderr.push(line);
    if (line.startsWith('nimbo-bridge: ')) h.codes.push(line.slice('nimbo-bridge: '.length));
    settle();
  });
  const exited = new Promise((resolve) => child.on('exit', resolve));
  h.exited = exited;
  t.after(async () => {
    h.quiet = false;
    child.stdin.end();
    const timer = setTimeout(() => child.kill('SIGKILL'), 10000);
    await exited;
    clearTimeout(timer);
  });
  h.control = (line) => {
    if (h.quiet) throw new Error('quiet guard: no host input allowed while awaiting event-loop delivery');
    child.stdin.write(line + '\n');
  };
  // Round trip through the host: every reply to earlier lines is delivered before this resolves.
  h.dump = async () => {
    lastDump = undefined;
    h.control('dump');
    await until(() => lastDump !== undefined, 'dump');
    await new Promise((resolve) => setImmediate(resolve));
    return lastDump;
  };
  h.flush = h.dump;
  h.script = async () => {
    lastScript = undefined;
    h.control('script');
    await until(() => lastScript !== undefined, 'script');
    return lastScript;
  };
  h.send = (msg, { source = ORIGIN + '/', top = true } = {}) => h.control(JSON.stringify({ attr: { source, top }, msg }));
  h.sendRaw = (raw, { source = ORIGIN + '/', top = true } = {}) => h.control(JSON.stringify({ attr: { source, top }, raw }));
  h.waitCode = (code, from = 0) => until(() => h.codes.slice(from).includes(code), 'code ' + code);
  // Fixed code on stderr, then 200 ms without any host -> page message.
  h.expectDrop = async (code, send) => {
    h.control('tick 1000'); // fresh log window, so the per-window budget never hides the code
    const replies = h.toPage.length, from = h.codes.length;
    send();
    await h.waitCode(code, from);
    await sleep(200);
    if (h.toPage.length !== replies) throw new Error(`unexpected reply after ${code}: ${JSON.stringify(h.toPage.slice(replies))}`);
  };
  // Next login URL (index `from`), parsed; never printed except through `redact`.
  h.loginUrl = async (from = h.loginUrls.length, ms = 5000) => {
    await until(() => h.loginUrls.length > from, 'login-url line', ms);
    const url = new URL(h.loginUrls[from]);
    return { url: url.href, port: Number(url.searchParams.get('port')), state: url.searchParams.get('state'), origin: url.origin, path: url.pathname };
  };
  h.stats = async () => {
    lastStats = undefined;
    h.control('stats');
    await until(() => lastStats !== undefined, 'stats');
    return lastStats;
  };
  h.limits = (limits) => h.control('limits ' + Object.entries(limits).map(([k, v]) => `${k}=${v}`).join(' '));
  h.quit = async () => {
    h.control('quit');
    await until(() => h.shutdownReport !== undefined, 'shutdown report', 5000);
    await exited;
    return h.shutdownReport;
  };
  h.deliver = (msg) => { for (const page of h.pages) page.dispatch(msg); };
  h.release = () => { h.hold = false; for (const msg of h.held.splice(0)) h.deliver(msg); };
  // A document: `top`, `origin` and `webview` select the guard cases; `transform` edits the
  // script text (explicitly labeled source-transformed copies only).
  h.load = async ({ top = true, origin = ORIGIN, webview = true, transform = (s) => s } = {}) => {
    const listeners = new Set();
    const page = { sent: [], dispatch: (data) => { for (const fn of [...listeners]) fn({ data }); } };
    const double = {
      postMessage(msg) { page.sent.push(msg); h.send(JSON.parse(JSON.stringify(msg)), { top }); },
      addEventListener(type, fn) { if (type === 'message') listeners.add(fn); },
      removeEventListener(type, fn) { listeners.delete(fn); },
    };
    const window = webview ? { chrome: { webview: double } } : {};
    window.top = top ? window : {};
    page.window = window;
    page.context = vm.createContext({ window, location: { origin }, setTimeout: h.clock.setTimeout, clearTimeout: h.clock.clearTimeout });
    vm.runInContext(transform(await h.script()), page.context, { filename: 'desktop/rust/bridge/nimbo-desktop.js (host-substituted)' });
    h.pages.push(page);
    return page;
  };
  h.control('content-loading');
  return h;
}
