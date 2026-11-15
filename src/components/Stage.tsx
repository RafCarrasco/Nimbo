import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { LayoutGrid, MonitorPlay, RectangleHorizontal, Users, Video } from 'lucide-react';
import { Avatar } from './Avatar';
import { Elapsed, Viewers } from './LiveMeta';
import { LiveTile, type TileState } from './LiveTile';
import { liveKey, parseLiveKey, type LiveSnapshot } from '../live/room';
import type { StreamKind } from '../live/capture';
import { nativeOutdated } from '../live/native';
import { fitGrid, GAP } from '../lib/layout';

function useSize<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Medição imediata: o ResizeObserver só notifica no próximo quadro pintado.
    const style = getComputedStyle(el);
    setSize({
      width: el.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
      height: el.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom),
    });
    const observer = new ResizeObserver(([entry]) =>
      setSize({ width: entry.contentRect.width, height: entry.contentRect.height }),
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, size] as const;
}

type AudioPrefs = Record<string, { volume: number; muted: boolean }>;
const AUDIO_KEY = 'nimbo.audio';
function loadAudio(): AudioPrefs {
  try {
    return JSON.parse(localStorage.getItem(AUDIO_KEY) || '{}');
  } catch {
    return {};
  }
}

export function Stage({
  uid,
  channelName,
  live,
  visible,
  setVisible,
  nameOf,
  photoOf,
  onRetry,
  lead,
}: {
  uid: string;
  channelName: string;
  live: LiveSnapshot;
  visible: string[];
  setVisible: (update: (ids: string[]) => string[]) => void;
  nameOf: (id: string) => string;
  photoOf?: (id: string) => string | undefined;
  onRetry: (id: string) => void;
  // Botão à esquerda do nome do canal (ocultar ou mostrar a barra lateral).
  lead?: ReactNode;
}) {
  const [layout, setLayout] = useState<'grid' | 'focus'>('grid');
  const [focused, setFocused] = useState('');
  const [audio, setAudio] = useState<AudioPrefs>(loadAudio);
  const [area, size] = useSize<HTMLDivElement>();

  useEffect(() => {
    try {
      localStorage.setItem(AUDIO_KEY, JSON.stringify(audio));
    } catch {
      // Preferência apenas local.
    }
  }, [audio]);

  // Cada live é "uid:screen" ou "uid:camera". As próprias aparecem assim que a captura começa; um
  // registro antigo no banco sem captura local é ignorado.
  const ownKeys = (Object.keys(live.local) as StreamKind[]).map((kind) => liveKey(uid, kind));
  const otherKeys = Object.entries(live.broadcasts).flatMap(([id, kinds]) =>
    id === uid ? [] : (Object.keys(kinds) as StreamKind[]).map((kind) => liveKey(id, kind)),
  );
  const active = [...ownKeys, ...otherKeys];
  const shown = visible.filter((key) => active.includes(key));
  const toggle = (key: string) =>
    setVisible((keys) => (keys.includes(key) ? keys.filter((x) => x !== key) : [...keys, key]));
  const setPref = useCallback(
    (key: string, patch: Partial<{ volume: number; muted: boolean }>) =>
      setAudio((old) => ({ ...old, [key]: { ...(old[key] ?? { volume: 100, muted: false }), ...patch } })),
    [],
  );

  // Como no Discord: clicar numa live a destaca (as outras vão para a faixa de baixo);
  // clicar na destacada volta para a grade.
  const focusMode = layout === 'focus' && shown.length > 1;
  const main = focusMode ? shown.find((key) => key === focused) || shown[0] : '';
  const strip = focusMode ? shown.filter((key) => key !== main) : [];
  const gridKeys = focusMode ? [main] : shown;
  const stripHeight = strip.length ? 132 : 0;
  const fit = fitGrid(gridKeys.length, size.width, size.height - stripHeight - (strip.length ? GAP : 0));
  const select = (key: string) => {
    if (shown.length < 2) return;
    if (focusMode && key === main) setLayout('grid');
    else {
      setFocused(key);
      setLayout('focus');
    }
  };

  const labelOf = (key: string) => {
    const { uid: id, kind } = parseLiveKey(key);
    if (id === uid) return kind === 'camera' ? 'Sua câmera' : 'Sua tela';
    return nameOf(id);
  };

  // Quem está assistindo uma live (só gente que está no canal), com "Você" para a própria pessoa.
  const viewersOf = (key: string) =>
    (live.watchers?.[key] || [])
      .filter((id) => live.members.includes(id))
      .map((id) => (id === uid ? 'Você' : nameOf(id)));

  const tileFor = (key: string, compact = false) => {
    const { uid: id, kind } = parseLiveKey(key);
    const own = id === uid;
    const streamId = live.broadcasts[id]?.[kind]?.streamId;
    const stream = (own ? live.local[kind] : streamId ? live.remote[id]?.[streamId] : undefined) || null;
    // Com as faixas já anunciadas mas a rede ainda negociando, é "conectando", não "sem imagem".
    // Live enviada pelo motor do app tem conexão própria: o estado vem dela, não da conexão do navegador.
    const native = own ? undefined : live.native?.[key];
    const state: TileState = native
      ? native === 'playing' && !stream
        ? 'connecting'
        : native
      : live.failed.includes(id)
        ? 'failed'
        : stream && (own || !live.connecting.includes(id))
          ? 'playing'
          : 'connecting';
    const pref = audio[key] || { volume: 100, muted: false };
    // Live do app numa versão anterior à correção de rede: diz o que fazer em vez de só "não conectou".
    const old = native && native !== 'playing' ? nativeOutdated(live.apps?.[id]) : '';
    const hint = old
      ? `O Nimbo de ${nameOf(id)} está na versão ${old}, que não conecta com algumas redes. Peça para fechar e abrir o app: a atualização aparece em seguida.`
      : undefined;
    return (
      <LiveTile
        key={key}
        name={labelOf(key)}
        stream={stream}
        own={own}
        camera={kind === 'camera'}
        state={state}
        hint={hint}
        volume={pref.volume}
        muted={pref.muted}
        onVolume={(volume) => setPref(key, { volume })}
        onMute={(muted) => setPref(key, { muted })}
        onClose={() => setVisible((keys) => keys.filter((x) => x !== key))}
        onSelect={shown.length > 1 ? () => select(key) : undefined}
        selected={focusMode && key === main}
        onRetry={() => onRetry(id)}
        compact={compact}
        style={compact ? undefined : { width: fit.width, height: fit.height }}
        startedAt={live.broadcasts[id]?.[kind]?.startedAt}
        clockOffset={live.clockOffset}
        viewers={viewersOf(key)}
        game={live.broadcasts[id]?.[kind]?.game}
      />
    );
  };

  return (
    <section className="stage">
      <header className="stage-head">
        {lead}
        <div className="stage-title">
          <h1>{channelName}</h1>
          <span className="muted">
            <Users size={14} /> {live.members.length}
          </span>
        </div>
        <div className="live-chips" role="toolbar" aria-label="Lives do canal">
          {active.map((key) => {
            const { uid: id, kind } = parseLiveKey(key);
            return (
              <button
                key={key}
                className={'live-chip' + (visible.includes(key) ? ' on' : '')}
                aria-pressed={visible.includes(key)}
                title={visible.includes(key) ? 'Ocultar live' : 'Assistir live'}
                onClick={() => toggle(key)}
              >
                <span className="dot" />
                {id === uid ? 'Você' : nameOf(id)}
                {kind === 'camera' && <Video size={13} aria-label="câmera" />}
              </button>
            );
          })}
        </div>
        {shown.length > 1 && (
          <div className="segmented" role="group" aria-label="Layout">
            <button
              className={!focusMode ? 'on' : ''}
              title="Grade"
              aria-label="Grade"
              onClick={() => setLayout('grid')}
            >
              <LayoutGrid size={16} />
            </button>
            <button
              className={focusMode ? 'on' : ''}
              title="Destaque"
              aria-label="Destaque"
              onClick={() => setLayout('focus')}
            >
              <RectangleHorizontal size={16} />
            </button>
          </div>
        )}
      </header>
      <div className="stage-area" ref={area}>
        {shown.length === 0 && active.length > 0 ? (
          // Como no Discord: cada live do canal vira um cartão, com quem transmite, o que é, há quanto
          // tempo está no ar e quantos estão assistindo.
          <div className="live-cards">
            <strong className="live-cards-title">Ao vivo agora</strong>
            <div className="live-cards-grid">
              {active.map((key) => {
                const { uid: id, kind } = parseLiveKey(key);
                const own = id === uid;
                const broadcast = live.broadcasts[id]?.[kind];
                const name = own ? 'Você' : nameOf(id);
                return (
                  <button
                    key={key}
                    className="live-card"
                    title={own ? 'Ver a sua prévia' : 'Assistir a live de ' + name}
                    onClick={() => toggle(key)}
                  >
                    <span className="live-card-head">
                      <Avatar photo={photoOf?.(id)} name={own ? nameOf(id) : name} />
                      <span className="live-card-text">
                        <strong>{name}</strong>
                        <small>
                          {kind === 'camera' ? <Video size={12} aria-hidden /> : <MonitorPlay size={12} aria-hidden />}
                          {kind === 'camera' ? 'Webcam' : broadcast?.game || 'Tela'}
                        </small>
                      </span>
                    </span>
                    <span className="live-card-meta">
                      <span className="live-badge">AO VIVO</span>
                      <Elapsed startedAt={broadcast?.startedAt} clockOffset={live.clockOffset} />
                      <Viewers names={viewersOf(key)} />
                      <span className="live-card-action">{own ? 'Ver prévia' : 'Assistir'}</span>
                    </span>
                  </button>
                );
              })}
            </div>
            <span className="muted">Dá para assistir várias ao mesmo tempo, pelos nomes acima ou pela barra lateral.</span>
          </div>
        ) : shown.length === 0 ? (
          <div className="stage-empty">
            <img src="/nimbo-icon-256.png" alt="" />
            <strong>{active.length ? 'Escolha uma live para assistir' : 'Ninguém ao vivo por aqui'}</strong>
            <span className="muted">
              {active.length
                ? 'Clique nos nomes acima ou na barra lateral. Dá para assistir várias ao mesmo tempo.'
                : 'Compartilhe sua tela ou webcam pelos botões no canto inferior esquerdo.'}
            </span>
          </div>
        ) : (
          <>
            <div
              className="tile-grid"
              style={{ width: fit.cols * fit.width + (fit.cols - 1) * GAP + 1, gap: GAP }}
            >
              {gridKeys.map((key) => tileFor(key))}
            </div>
            {strip.length > 0 && <div className="tile-strip">{strip.map((key) => tileFor(key, true))}</div>}
          </>
        )}
      </div>
    </section>
  );
}
