// Private picker transport. Native controller/frame/origin ownership must still be checked by the host.
(function capturePickerBridge() {
  'use strict';
  if (window.top !== window || location.href !== 'https://nimbo-picker.invalid/' || !window.chrome?.webview) return;
  const webview = window.chrome.webview;
  let generation = 0, nextId = 1, ready = false, closed = false;
  const queued = [], pending = new Map();
  const readyTimer = setTimeout(dispose, 5000);
  function dispose() {
    if (closed) return;
    closed = true; clearTimeout(readyTimer);
    const error = new Error('Ponte indisponível.');
    for (const call of queued.splice(0)) call.reject(error);
    for (const { call, timer } of pending.values()) { clearTimeout(timer); call.reject(error); }
    pending.clear();
    webview.removeEventListener('message', receive);
    window.removeEventListener('pagehide', dispose);
  }
  function start(call) {
    if (pending.size >= 32 || nextId > 4294967295) return call.reject(new Error('Ponte ocupada.'));
    const id = nextId++;
    const timer = setTimeout(() => {
      if (pending.delete(id)) call.reject(new Error('Ponte sem resposta.'));
    }, 30000);
    pending.set(id, { call, timer });
    webview.postMessage({ v: 1, kind: 'picker-invoke', id, gen: generation, method: call.method, args: call.args });
  }
  function invoke(method, args) {
    return new Promise((resolve, reject) => {
      const call = { method, args, resolve, reject };
      if (closed) reject(new Error('Ponte indisponível.'));
      else if (ready) start(call);
      else if (queued.length < 32) queued.push(call);
      else reject(new Error('Ponte ocupada.'));
    });
  }
  function receive(event) {
    const m = event.data;
    if (!m || m.v !== 1) return;
    if (m.kind === 'picker-ready') {
      if (ready || !Number.isSafeInteger(m.gen) || m.gen < 1) return;
      ready = true; generation = m.gen; clearTimeout(readyTimer);
      for (const call of queued.splice(0)) start(call);
      return;
    }
    if (!ready || m.kind !== 'picker-result' || m.gen !== generation) return;
    const entry = pending.get(m.id);
    if (!entry) return;
    pending.delete(m.id); clearTimeout(entry.timer);
    if (m.ok === true) entry.call.resolve(m.value);
    else entry.call.reject(new Error(String(m.error || 'Falha no seletor.')));
  }
  webview.addEventListener('message', receive);
  window.addEventListener('pagehide', dispose, { once: true });
  Object.defineProperty(window, 'capturePicker', { enumerable: true, value: Object.freeze({
    list: () => invoke('list', []),
    choose: (id, audio) => invoke('choose', [{ id, audio }]),
    cancel: () => invoke('cancel', []),
    diagnostics: () => invoke('diagnostics', []),
  }) });
  webview.postMessage({ v: 1, kind: 'picker-hello' });
})();
