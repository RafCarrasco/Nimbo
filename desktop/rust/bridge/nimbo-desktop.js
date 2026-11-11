// Nimbo desktop bridge v1 for the Rust/WebView2 host (CP2 S1). Injected with
// AddScriptToExecuteOnDocumentCreated; the host substitutes the app origin (JSON string).
// Same public surface and coercions as desktop/preload.cjs. Wire protocol: plan §4.3.
(function nimboDesktopBridge() {
  'use strict';
  if (window.top !== window) return;
  if (location.origin !== __APP_ORIGIN__) return;
  if (!window.chrome || !window.chrome.webview) return;
  const webview = window.chrome.webview;
  const MAX_QUEUED = 64, MAX_PENDING = 64, MAX_ID = 4294967295;
  const READY_MS = 5000, CALL_MS = 30000, SIGN_IN_MS = 330000;
  const BUSY = 'Ponte ocupada.', UNAVAILABLE = 'Ponte indisponível.', NO_ANSWER = 'Ponte sem resposta.';
  let state = 'waiting', gen = 0, nextId = 1, readyTimer;
  const queued = [], pending = new Map(), listeners = new Map();

  function unavailable() {
    state = 'unavailable';
    clearTimeout(readyTimer);
    for (const call of queued.splice(0)) call.reject(new Error(UNAVAILABLE));
  }
  function start(call) {
    if (pending.size >= MAX_PENDING) return call.resolve({ ok: false, error: BUSY });
    if (nextId > MAX_ID) {
      unavailable(); // ids are never reused or wrapped within a document
      return call.reject(new Error(UNAVAILABLE));
    }
    const id = nextId++;
    const ms = call.method === 'nimbo:signIn' ? SIGN_IN_MS : CALL_MS;
    const timer = setTimeout(() => {
      if (pending.delete(id)) call.reject(new Error(NO_ANSWER));
    }, ms);
    pending.set(id, { call, timer });
    webview.postMessage({ v: 1, kind: 'invoke', id, gen, method: call.method, args: call.args });
  }
  function invoke(method, args) {
    return new Promise((resolve, reject) => {
      const call = { method, args, resolve, reject };
      if (state === 'unavailable') reject(new Error(UNAVAILABLE));
      else if (state === 'ready') start(call);
      else if (queued.length >= MAX_QUEUED) resolve({ ok: false, error: BUSY });
      else queued.push(call);
    });
  }
  function on(channel, listener) {
    if (!listeners.has(channel)) listeners.set(channel, new Set());
    listeners.get(channel).add(listener);
    return () => { listeners.get(channel)?.delete(listener); };
  }

  webview.addEventListener('message', (event) => {
    const m = event.data;
    if (!m || typeof m !== 'object' || m.v !== 1) return;
    if (m.kind === 'ready') {
      if (state !== 'waiting' || !Number.isSafeInteger(m.gen) || m.gen < 1) return;
      state = 'ready';
      gen = m.gen;
      clearTimeout(readyTimer);
      for (const call of queued.splice(0)) start(call);
      return;
    }
    if (state !== 'ready' || m.gen !== gen) return;
    if (m.kind === 'result' || m.kind === 'reject') {
      const entry = pending.get(m.id);
      if (!entry) return; // unknown or forgotten after its deadline
      pending.delete(m.id);
      clearTimeout(entry.timer);
      if (m.kind === 'reject') entry.call.reject(new Error(String(m.error)));
      else if (m.ok === true) entry.call.resolve({ ok: true, value: m.value });
      else entry.call.resolve({ ok: false, error: String(m.error) });
    } else if (m.kind === 'event' && typeof m.channel === 'string' && Array.isArray(m.args)) {
      for (const listener of [...(listeners.get(m.channel) || [])]) listener(m.args);
    }
  });

  const api = {
    version: 1,
    signIn: () => invoke('nimbo:signIn', []),
    cancelSignIn: () => invoke('nimbo:signIn:cancel', []),
    setCaptureAudio: (wanted) => invoke('nimbo:capture:audio', [Boolean(wanted)]),
    startAudio: () => invoke('nimbo:audio:start', []),
    stopAudio: (id) => invoke('nimbo:audio:stop', [String(id)]),
    watchUpdate: (onReady) => {
      on('nimbo:update:ready', (args) => onReady(args[0]));
      return invoke('nimbo:update:watch', []);
    },
    installUpdate: () => invoke('nimbo:update:install', []),
    onUpdateProgress: (onProgress) => on('nimbo:update:progress', (args) => onProgress(Number(args[0]) || 0)),
    startGameWatch: () => invoke('nimbo:game:start', []),
    stopGameWatch: (id) => invoke('nimbo:game:stop', [String(id)]),
    onGameState: (id, onChange) => on('nimbo:game:state', (args) => {
      if (args[0] === id) onChange(Boolean(args[1]));
    }),
    onGameEnded: (id, onEnded) => on('nimbo:game:ended', (args) => {
      if (args[0] === id) onEnded();
    }),
    onAudio: (id, onData, onEnd) => {
      const offData = on('nimbo:audio:data', (args) => {
        if (args[0] === id) onData(args[1]);
      });
      const offEnd = on('nimbo:audio:end', (args) => {
        if (args[0] !== id) return;
        cleanup();
        onEnd(args[1] || '');
      });
      const cleanup = () => { offData(); offEnd(); };
      return cleanup;
    },
  };
  Object.defineProperty(window, 'nimboDesktop', { value: Object.freeze(api), enumerable: true });
  readyTimer = setTimeout(unavailable, READY_MS);
  webview.postMessage({ v: 1, kind: 'hello' });
})();
