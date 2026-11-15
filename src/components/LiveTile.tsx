import { useEffect, useRef, useState } from 'react';
import {
  ClipboardCopy,
  ImageOff,
  LoaderCircle,
  Maximize,
  Minimize,
  PictureInPicture2,
  RotateCw,
  Video,
  Volume1,
  Volume2,
  VolumeX,
  WifiOff,
  X,
} from 'lucide-react';
import { desktop } from '../desktop';
import { APP_LABEL } from '../lib/version';
import { Elapsed, Viewers } from './LiveMeta';

export type TileState = 'playing' | 'connecting' | 'failed';

export function LiveTile({
  name,
  stream,
  own,
  camera,
  state,
  hint,
  volume,
  muted,
  onVolume,
  onMute,
  onClose,
  onSelect,
  selected,
  onRetry,
  compact,
  style,
  startedAt,
  clockOffset,
  viewers,
  game,
}: {
  name: string;
  stream: MediaStream | null;
  own: boolean;
  camera: boolean;
  state: TileState;
  // Explicação do que fazer quando a live não conecta por um motivo conhecido.
  hint?: string;
  volume: number;
  muted: boolean;
  onVolume: (value: number) => void;
  onMute: (value: boolean) => void;
  onClose: () => void;
  // Clique na live: destacar (ou voltar para a grade, se já for a destacada).
  onSelect?: () => void;
  selected?: boolean;
  onRetry?: () => void;
  compact?: boolean;
  style?: React.CSSProperties;
  // Início da live na hora do servidor e a correção do relógio daqui, para o tempo no ar.
  startedAt?: number;
  clockOffset?: number;
  // Quem está assistindo (nomes) e o jogo transmitido.
  viewers?: string[];
  game?: string;
}) {
  const tile = useRef<HTMLElement>(null);
  const video = useRef<HTMLVideoElement>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [hasAudio, setHasAudio] = useState(false);
  // Ter um stream não basta: só consideramos a live no ar quando chegam quadros de vídeo.
  const [hasFrames, setHasFrames] = useState(false);
  const [slow, setSlow] = useState(false);
  const [copied, setCopied] = useState(false);
  // Resolução e quadros por segundo que estão chegando ("1080p · 60 fps").
  const [quality, setQuality] = useState('');
  const [floating, setFloating] = useState(false);
  // Estado das conexões (rota, bytes e quadros) para colar a quem cuida do Nimbo quando uma live trava.
  const copyDiagnostics = async () => {
    const debug = (window as unknown as { nimboDebug?: () => Promise<unknown> }).nimboDebug;
    const report = {
      quando: new Date().toISOString(),
      app: APP_LABEL,
      live: name,
      estado: state,
      quadros: hasFrames,
      conexoes: (await debug?.().catch(() => null)) ?? [],
    };
    await navigator.clipboard.writeText(JSON.stringify(report, null, 1)).catch(() => {});
    setCopied(true);
  };
  // A própria prévia nunca toca áudio: evita que o som capturado volte para a captura (eco).
  const silent = own || muted;

  useEffect(() => {
    const v = video.current;
    if (!v) return;
    v.srcObject = stream;
    const sync = () => setHasAudio(Boolean(stream?.getAudioTracks().length));
    sync();
    const frames = () => setHasFrames(v.videoWidth > 0 && v.readyState >= 2);
    frames();
    v.addEventListener('loadeddata', frames);
    v.addEventListener('resize', frames);
    v.addEventListener('emptied', frames);
    setSlow(false);
    const timer = setTimeout(() => setSlow(true), own ? 8000 : 15000);
    stream?.addEventListener('addtrack', sync);
    stream?.addEventListener('removetrack', sync);
    return () => {
      clearTimeout(timer);
      v.removeEventListener('loadeddata', frames);
      v.removeEventListener('resize', frames);
      v.removeEventListener('emptied', frames);
      stream?.removeEventListener('addtrack', sync);
      stream?.removeEventListener('removetrack', sync);
      v.srcObject = null;
    };
  }, [stream]);

  // Mede o que chega de verdade: a altura da imagem e quantos quadros o player recebeu por segundo.
  // A contagem é a do próprio player, que continua valendo com a janela coberta ou minimizada (medido:
  // contar pelos quadros desenhados na tela dava 3 por segundo numa live de 59).
  useEffect(() => {
    const v = video.current;
    if (!v || typeof v.getVideoPlaybackQuality !== 'function') return;
    // Todos os quadros que chegaram ao player, inclusive os que ele não desenhou por estar coberto.
    const shown = () => v.getVideoPlaybackQuality().totalVideoFrames;
    let last = shown();
    let at = performance.now();
    const timer = setInterval(() => {
      const [now, frames] = [performance.now(), shown()];
      const fps = Math.round(((frames - last) * 1000) / Math.max(1, now - at));
      [last, at] = [frames, now];
      setQuality(v.videoHeight ? `${v.videoHeight}p${fps > 0 ? ` · ${fps} fps` : ''}` : '');
    }, 2000);
    return () => {
      clearInterval(timer);
      setQuality('');
    };
  }, [stream]);

  // Janela flutuante (fica por cima dos outros programas, para assistir jogando).
  useEffect(() => {
    const v = video.current;
    if (!v) return;
    const enter = () => setFloating(true);
    const leave = () => setFloating(false);
    v.addEventListener('enterpictureinpicture', enter);
    v.addEventListener('leavepictureinpicture', leave);
    return () => {
      v.removeEventListener('enterpictureinpicture', enter);
      v.removeEventListener('leavepictureinpicture', leave);
    };
  }, []);
  const canFloat = typeof document !== 'undefined' && document.pictureInPictureEnabled;
  const toggleFloating = () => {
    if (document.pictureInPictureElement) void document.exitPictureInPicture().catch(() => {});
    else void video.current?.requestPictureInPicture().catch(() => {});
  };

  useEffect(() => {
    const v = video.current;
    if (!v) return;
    v.muted = silent;
    v.volume = Math.min(1, Math.max(0, volume / 100));
    if (v.srcObject && v.paused)
      v.play().catch((e: DOMException) => {
        // Sem interação prévia o navegador só permite autoplay sem som.
        if (e.name === 'NotAllowedError' && !silent) onMute(true);
      });
  }, [silent, volume, stream, onMute]);

  useEffect(() => {
    const onChange = () => setFullscreen(document.fullscreenElement === tile.current);
    document.addEventListener('fullscreenchange', onChange);
    return () => document.removeEventListener('fullscreenchange', onChange);
  }, []);

  const toggleFullscreen = () => {
    if (document.fullscreenElement) void document.exitFullscreen().catch(() => {});
    else void tile.current?.requestFullscreen().catch(() => {});
  };
  // A faixa chegou (a conexão funciona), mas nenhum quadro: a captura de quem transmite está vazia,
  // típico de jogo em tela cheia exclusiva capturado como janela.
  const noImage = state === 'playing' && !hasFrames && slow && !camera;
  const VolumeIcon = silent || volume === 0 ? VolumeX : volume < 50 ? Volume1 : Volume2;

  return (
    <section
      ref={tile}
      className={
        'live-tile' +
        (compact ? ' compact' : '') +
        (onSelect ? ' selectable' : '') +
        (selected ? ' selected' : '') +
        (own && camera ? ' mirror' : '')
      }
      style={style}
      title={onSelect && !fullscreen ? (selected ? 'Voltar para a grade' : 'Destacar esta live') : undefined}
      onClick={(e) => {
        // Cliques nos controles (volume, tela cheia, fechar) não mudam o destaque.
        if ((e.target as HTMLElement).closest('button, input')) return;
        if (!fullscreen) onSelect?.();
      }}
      // Como no Discord: duplo clique na imagem abre (ou fecha) a tela cheia.
      onDoubleClick={(e) => {
        if ((e.target as HTMLElement).closest('button, input')) return;
        toggleFullscreen();
      }}
    >
      <video ref={video} autoPlay playsInline muted={silent} />
      {(state !== 'playing' || !hasFrames) && (
        <div className="tile-placeholder">
          {state === 'failed' ? (
            <WifiOff size={28} />
          ) : noImage ? (
            <ImageOff size={28} />
          ) : (
            <LoaderCircle size={28} className="spin" />
          )}
          <span>
            {state === 'failed'
              ? 'Não foi possível conectar a esta live'
              : noImage
                ? own
                  ? 'Sua captura não está gerando imagem'
                  : 'Conectado, mas sem imagem desta transmissão'
                : slow
                  ? 'A conexão está demorando…'
                  : 'Conectando à transmissão…'}
          </span>
          {noImage && !compact && (
            <small className="tile-hint">
              {own
                ? desktop
                  ? 'Jogo em tela cheia não aparece na captura de janela. Troque a fonte (botão ao lado de Encerrar) e escolha a tela inteira; no app atualizado, escolher o jogo já faz isso.'
                  : 'Jogo em tela cheia não aparece na captura de janela. Troque a fonte e escolha a tela inteira, ou use o app do Nimbo para Windows.'
                : 'O problema é na captura de quem transmite. Se for um jogo em tela cheia, peça para transmitir a tela inteira.'}
            </small>
          )}
          {hint && !noImage && !compact && <small className="tile-hint">{hint}</small>}
          {!compact && (state === 'failed' || slow) && (
            <div className="tile-actions">
              {!own && onRetry && (
                <button className="button ghost" onClick={onRetry}>
                  <RotateCw size={15} /> Tentar de novo
                </button>
              )}
              <button className="button ghost" onClick={() => void copyDiagnostics()}>
                <ClipboardCopy size={15} /> {copied ? 'Copiado' : 'Copiar diagnóstico'}
              </button>
            </div>
          )}
        </div>
      )}
      <div className="tile-top">
        <span className="live-badge">AO VIVO</span>
        {!compact && <Elapsed startedAt={startedAt} clockOffset={clockOffset} />}
        <span className="tile-name">
          {camera && <Video size={14} aria-label="Webcam" />}
          {name}
        </span>
        {game && !compact && <span className="tile-game">{game}</span>}
        <Viewers names={viewers || []} />
        {!compact && (
          <button
            className="tile-button"
            aria-label="Fechar visualização"
            title="Fechar visualização"
            onClick={onClose}
          >
            <X size={16} />
          </button>
        )}
      </div>
      {!compact && (
        <div className="tile-controls">
          {!own && !hasAudio && !camera && (
            <span
              className="no-audio"
              title="Quem transmite não enviou som. No navegador, é preciso marcar 'Compartilhar áudio' ao escolher a tela; no app do Nimbo o som vai pela opção de áudio."
            >
              <VolumeX size={16} aria-hidden /> Sem som
            </span>
          )}
          {!own && hasAudio && (
            <div className="volume">
              <button
                className="tile-button"
                aria-label={silent ? 'Ativar som' : 'Silenciar'}
                title={silent ? 'Ativar som' : 'Silenciar'}
                onClick={() => onMute(!muted)}
              >
                <VolumeIcon size={18} />
              </button>
              <input
                type="range"
                min={0}
                max={100}
                value={silent ? 0 : volume}
                aria-label={'Volume de ' + name}
                style={{ '--fill': (silent ? 0 : volume) + '%' } as React.CSSProperties}
                onChange={(e) => {
                  const value = Number(e.target.value);
                  onVolume(value);
                  if (value > 0 && muted) onMute(false);
                }}
              />
              <span className="volume-value">{silent ? 0 : volume}%</span>
            </div>
          )}
          {own && <span className="tile-hint">Sua prévia não reproduz áudio</span>}
          <div className="tile-right push-right">
            {quality && hasFrames && (
              <span className="tile-quality" title="Imagem que está chegando agora">
                {quality}
              </span>
            )}
            {canFloat && !own && hasFrames && (
              <button
                className="tile-button"
                aria-label={floating ? 'Fechar a janela flutuante' : 'Janela flutuante'}
                title={floating ? 'Fechar a janela flutuante' : 'Janela flutuante (fica por cima dos outros programas)'}
                aria-pressed={floating}
                onClick={toggleFloating}
              >
                <PictureInPicture2 size={18} />
              </button>
            )}
            <button
              className="tile-button"
              aria-label={fullscreen ? 'Sair da tela cheia' : 'Tela cheia'}
              title={fullscreen ? 'Sair da tela cheia' : 'Tela cheia'}
              onClick={toggleFullscreen}
            >
              {fullscreen ? <Minimize size={18} /> : <Maximize size={18} />}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
