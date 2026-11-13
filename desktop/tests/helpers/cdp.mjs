// Minimal Chrome DevTools Protocol client over Node's global `WebSocket` (no Playwright, no new
// dependency) for the S2-integration browser tiers, which run ONLY inside the approved
// network-none container. One browser-level WebSocket carries every flattened session, so all
// events arrive in a single order. Also provides the explicitly labeled `chrome.webview` TEST
// SHIM used by tier L5 to stand in for the WebView2 transport (NOT native WebView2 evidence).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Required headless flags (plan §8.4) — always first, recorded by the tiers. */
export const BASE_FLAGS = ['--no-sandbox', '--headless=new', '--disable-gpu', '--disable-dev-shm-usage', '--remote-debugging-port=0'];
/** Browser-level defense in depth (plan §9.8, F0 attempt3): a dead proxy with Chrome's IMPLICIT
 *  loopback bypass (no `<-loopback>` rule), no QUIC, and DNS only for loopback names. The kernel
 *  network namespace remains the guarantee. */
export const DEFENSE_FLAGS = ['--proxy-server=127.0.0.1:1', '--disable-quic',
  '--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1, EXCLUDE localhost',
  '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--disable-component-update',
  '--disable-sync', '--disable-default-apps', '--disable-extensions', '--metrics-recording-only', '--no-pings'];

export class CdpConnection {
  constructor(socket, { timeoutMs = 15000 } = {}) {
    this.socket = socket;
    this.timeoutMs = timeoutMs;
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = new Map();
    this.events = []; // ordered {seq, method, sessionId}; params are NOT retained here (privacy)
    this.closed = false;
    socket.onmessage = (m) => this.#onMessage(typeof m.data === 'string' ? m.data : Buffer.from(m.data).toString('utf8'));
    socket.onclose = () => this.#onClose('closed');
    socket.onerror = () => this.#onClose('closed (error)');
  }
  #onClose(why) {
    this.closed = true;
    for (const [, p] of this.pending) p.reject(new Error('cdp connection ' + why));
    this.pending.clear();
  }
  #onMessage(text) {
    const msg = JSON.parse(text);
    if (msg.id !== undefined) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) p.reject(new Error(`${p.method}: ${msg.error.message} (${msg.error.code})`));
      else p.resolve(msg.result);
      return;
    }
    if (!msg.method) return;
    this.events.push({ seq: this.events.length, method: msg.method, sessionId: msg.sessionId ?? null });
    for (const fn of [...(this.handlers.get(msg.method) || []), ...(this.handlers.get('*') || [])]) fn(msg.params || {}, msg.sessionId ?? null, msg.method);
  }
  on(method, fn) {
    if (!this.handlers.has(method)) this.handlers.set(method, new Set());
    this.handlers.get(method).add(fn);
    return () => this.handlers.get(method).delete(fn);
  }
  send(method, params = {}, sessionId = undefined, timeoutMs = this.timeoutMs) {
    if (this.closed) return Promise.reject(new Error('cdp connection closed'));
    const id = this.nextId++;
    const frame = { id, method, params };
    if (sessionId) frame.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`cdp timeout ${method} after ${timeoutMs} ms`)); }, timeoutMs);
      this.pending.set(id, { method, resolve: (v) => { clearTimeout(timer); resolve(v); }, reject: (e) => { clearTimeout(timer); reject(e); } });
      this.socket.send(JSON.stringify(frame));
    });
  }
  close() { try { this.socket.close(); } catch { /* already closed */ } }
}

export async function connectCdp(wsUrl, options) {
  if (!/^ws:\/\/127\.0\.0\.1:\d+\//.test(wsUrl)) throw new Error('refusing non-loopback DevTools endpoint');
  const socket = new WebSocket(wsUrl);
  await new Promise((resolve, reject) => {
    socket.onopen = resolve;
    socket.onerror = () => reject(new Error('cdp websocket error'));
  });
  return new CdpConnection(socket, options);
}

/** Spawns Chrome (own process group) on a fresh profile dir and connects to its browser target. */
export async function launchChrome({ chrome, userDataDir, flags = [], stderrFile }) {
  if (!chrome) throw new Error('chrome path required');
  fs.mkdirSync(userDataDir, { recursive: true });
  const allFlags = [...BASE_FLAGS, `--user-data-dir=${userDataDir}`, ...flags];
  const errFd = fs.openSync(stderrFile || path.join(userDataDir, '..', path.basename(userDataDir) + '.stderr.log'), 'w');
  const proc = spawn(chrome, [...allFlags, 'about:blank'], { stdio: ['ignore', errFd, errFd], detached: true });
  fs.closeSync(errFd);
  let exited = null;
  const exitP = new Promise((resolve) => proc.on('exit', (code, signal) => { exited = { code, signal }; resolve(exited); }));
  let port = null;
  for (const end = Date.now() + 30000; Date.now() < end && !exited; await sleep(100)) {
    try {
      const [p, wsPath] = fs.readFileSync(path.join(userDataDir, 'DevToolsActivePort'), 'utf8').split('\n');
      if (p && wsPath) { port = Number(p); break; }
    } catch { /* not yet written */ }
  }
  if (!port) {
    try { process.kill(-proc.pid, 'SIGKILL'); } catch { /* gone */ }
    throw new Error('DevToolsActivePort not found' + (exited ? ` (chrome exited ${JSON.stringify(exited)})` : ''));
  }
  const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  const cdp = await connectCdp(version.webSocketDebuggerUrl);
  const close = async () => {
    cdp.close();
    if (!exited) {
      try { process.kill(-proc.pid, 'SIGTERM'); } catch { /* gone */ }
      const t = setTimeout(() => { try { process.kill(-proc.pid, 'SIGKILL'); } catch { /* gone */ } }, 5000);
      await Promise.race([exitP, sleep(10000)]);
      clearTimeout(t);
    }
    return exited;
  };
  return { proc, pid: proc.pid, port, browser: version.Browser, flags: allFlags, cdp, close, exited: () => exited };
}

/**
 * Instruments EVERY target (tabs, frames, workers) before it runs: browser-level and nested
 * flattened auto-attach with waitForDebuggerOnStart, Network/Page/Runtime enabled per session,
 * then resumed. `onEvent(method, params, sessionId)` sees every event in arrival order.
 */
export async function autoAttachAll(cdp, { onEvent } = {}) {
  const sessions = new Map(); // sessionId -> {targetId, type}
  const attach = { autoAttach: true, waitForDebuggerOnStart: true, flatten: true };
  if (onEvent) cdp.on('*', (params, sessionId, method) => onEvent(method, params, sessionId));
  const setup = async (sessionId, info) => {
    sessions.set(sessionId, { targetId: info.targetId, type: info.type, ready: false });
    const tryCmd = (m, p) => cdp.send(m, p, sessionId).catch((e) => ({ error: String(e.message) }));
    await tryCmd('Network.enable', {});
    await tryCmd('Network.setCacheDisabled', { cacheDisabled: true });
    if (info.type === 'page' || info.type === 'iframe') {
      await tryCmd('Page.enable', {});
      await tryCmd('Page.setLifecycleEventsEnabled', { enabled: true });
    }
    await tryCmd('Runtime.enable', {});
    await tryCmd('Target.setAutoAttach', attach);
    await tryCmd('Runtime.runIfWaitingForDebugger', {});
    sessions.get(sessionId).ready = true;
  };
  cdp.on('Target.attachedToTarget', (p) => { void setup(p.sessionId, p.targetInfo); });
  cdp.on('Target.detachedFromTarget', (p) => { const s = sessions.get(p.sessionId); if (s) s.detached = true; });
  await cdp.send('Target.setDiscoverTargets', { discover: true });
  await cdp.send('Target.setAutoAttach', attach);
  return sessions;
}

/** New tab (about:blank), returned once its session is instrumented and resumed. */
export async function openTab(cdp, sessions, ms = 15000) {
  const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
  for (const end = Date.now() + ms; Date.now() < end; await sleep(25)) {
    for (const [sessionId, s] of sessions) if (s.targetId === targetId && s.ready) return { targetId, sessionId };
  }
  throw new Error('new tab was not auto-attached');
}

/** Clicks the first enabled <button> whose text contains `text` (DOM click; returns false if none). */
export const clickButton = (cdp, sessionId, text) => evaluate(cdp, sessionId,
  `(() => { const b = [...document.querySelectorAll('button')].find((x) => x.textContent.includes(${JSON.stringify(text)}));
     if (!b || b.disabled) return false; b.click(); return true; })()`);

/** Evaluates `expression` in a session and returns the by-value result (throws on exceptions). */
export async function evaluate(cdp, sessionId, expression, { contextId, awaitPromise = false, timeoutMs } = {}) {
  const params = { expression, returnByValue: true, awaitPromise };
  if (contextId) params.contextId = contextId;
  const r = await cdp.send('Runtime.evaluate', params, sessionId, timeoutMs);
  if (r.exceptionDetails) throw new Error('page exception: ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
  return r.result ? r.result.value : undefined;
}

/** Polls a page expression until truthy (real time). */
export async function waitForPage(cdp, sessionId, expression, what, ms = 15000) {
  const end = Date.now() + ms;
  let last;
  for (;;) {
    try { last = await evaluate(cdp, sessionId, expression); } catch (e) { last = String(e.message); }
    if (last) return last;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what} (last: ${JSON.stringify(last)?.slice(0, 200)})`);
    await sleep(100);
  }
}

/**
 * LABELED TEST SHIM source: a `window.chrome.webview` stand-in. postMessage(msg) calls the CDP
 * binding with JSON text; the harness dispatches host replies through `dispatchName` as
 * `message` events. Installed with Page.addScriptToEvaluateOnNewDocument BEFORE the real
 * injected bridge script. Not WebView2: proves the page <-> real dispatcher protocol only.
 */
export function webviewShimSource(bindingName, dispatchName) {
  const b = JSON.stringify(bindingName), d = JSON.stringify(dispatchName);
  return `/* NIMBO TEST SHIM (S2-integration L5): CDP stands in for the WebView2 chrome.webview transport. */
(function () {
  if (window.top !== window) return;
  var post = window[${b}];
  if (typeof post !== 'function') return;
  var listeners = new Set();
  var webview = {
    postMessage: function (msg) { post(JSON.stringify(msg)); },
    addEventListener: function (type, fn) { if (type === 'message') listeners.add(fn); },
    removeEventListener: function (type, fn) { listeners.delete(fn); }
  };
  window.chrome = window.chrome || {};
  Object.defineProperty(window.chrome, 'webview', { value: webview, configurable: false });
  Object.defineProperty(window, ${d}, { value: function (data) { listeners.forEach(function (fn) { fn({ data: data }); }); } });
})();`;
}
