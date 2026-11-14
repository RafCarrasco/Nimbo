// Ponte do app em Rust com o site: mesmo nome e mesmos métodos de desktop/preload.cjs, para o
// src/desktop.ts funcionar sem mudança. O Rust troca o marcador de origem antes de injetar.
(() => {
  'use strict';
  // O WebView2 roda este script em todo frame; a ponte só existe na página principal do Nimbo.
  // Esconder aqui é conveniência: quem barra de verdade é o Rust (ver bridge.rs).
  if (window.top !== window.self || location.origin !== __NIMBO_APP_ORIGIN__) return;

  // Procura o invoke só na hora da chamada: não depende da ordem dos scripts de inicialização.
  const invoke = (command, args) => window.__TAURI_INTERNALS__.invoke(command, args);
  const unavailable = () => Promise.resolve({ ok: false, error: 'Ainda não disponível no app em Rust.' });
  // Registro de ouvinte que nunca dispara: devolve o "cancelar" que o site espera receber.
  const neverFires = () => () => {};

  // Atualização (update.rs): o Rust avisa por um canal criado aqui e entregue em watch_update.
  // Canal não pede permissão do Tauri: é um retorno de chamada que só esse comando recebe.
  const updates = { channel: '', ready: new Set(), progress: new Set() };
  const onUpdateEvent = (packet) => {
    // O Tauri embrulha cada aviso em { message, index }; o fim do canal vem sem `kind`.
    const event = packet && (packet.message || packet);
    if (!event) return;
    if (event.kind === 'ready') for (const tell of updates.ready) tell({ version: String(event.version) });
    if (event.kind === 'progress') for (const tell of updates.progress) tell(Number(event.percent) || 0);
  };

  const bridge = {
    version: 1,
    engine: 'rust',
    appVersion: __NIMBO_APP_VERSION__,
    signIn: () => invoke('sign_in'),
    cancelSignIn: () => invoke('cancel_sign_in'),
    setCaptureAudio: unavailable,
    startAudio: unavailable,
    stopAudio: unavailable,
    watchUpdate: (onReady) => {
      updates.ready.add(onReady);
      updates.channel ||= '__CHANNEL__:' + window.__TAURI_INTERNALS__.transformCallback(onUpdateEvent);
      return invoke('watch_update', { events: updates.channel });
    },
    installUpdate: () => invoke('install_update'),
    onUpdateProgress: (onProgress) => {
      updates.progress.add(onProgress);
      return () => updates.progress.delete(onProgress);
    },
    startGameWatch: unavailable,
    stopGameWatch: unavailable,
    onGameState: neverFires,
    onGameEnded: neverFires,
    onAudio: neverFires,
    // Motor de mídia do app: captura, codifica uma vez e envia para todos (src/live/native.ts).
    media: Object.freeze({
      start: (source, quality, options) => invoke('media_start', { source, quality, options: options ?? null }),
      pause: (paused) => invoke('media_pause', { paused: Boolean(paused) }),
      state: () => invoke('media_state'),
      accept: (offer) => invoke('media_accept', { offer }),
      close: (id) => invoke('media_close', { id }),
      stop: () => invoke('media_stop'),
      // Seletor "o que compartilhar": lista, miniatura e ícone de cada item.
      sources: () => invoke('media_sources'),
      thumbnail: (id) => invoke('media_thumbnail', { id }),
      icon: (id) => invoke('media_icon', { id }),
    }),
  };

  // Congelada e fixa como a do contextBridge do Electron: a página usa, não troca.
  Object.defineProperty(window, 'nimboDesktop', {
    value: Object.freeze(bridge),
    enumerable: true,
    writable: false,
    configurable: false,
  });
})();
