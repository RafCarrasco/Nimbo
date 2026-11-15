// Live de tela enviada pelo motor em Rust do app, e não pelo navegador.
//
// Quem transmite: o app captura e codifica uma vez só (pela placa de vídeo) e manda a mesma imagem
// para todos. A página só repassa a sinalização: recebe a oferta de quem quer assistir, entrega ao
// motor e devolve a resposta.
//
// Quem assiste (site, celular ou app): vê `native: true` no anúncio da live e abre uma conexão
// própria, só de recepção, direto com o motor do transmissor.
import { attachGameWatch, desktop, type GameWatch } from '../desktop';
import { pickNativeSource } from '../components/SourcePicker';
import { CaptureCancelled } from '../lib/errors';
import type { Broadcasts } from './room';
import type { AudioMode, StreamKind } from './capture';

export type NativeQuality = { fps: number; bitrate: number; maxHeight: number };
// nonce: liga a resposta à oferta que a pediu. Sem isso, com duas ofertas pendentes (nova tentativa),
// a resposta da primeira seria aplicada na conexão da segunda e ela nunca conectaria.
export type NativeSignal = {
  type: 'offer' | 'answer';
  sdp: RTCSessionDescriptionInit;
  native: StreamKind;
  nonce?: string;
};

// Só existe no app em Rust; no navegador e no app antigo fica indefinido.
export const nativeMedia = desktop?.media;

// Prévia local de cada live nativa: é uma conexão de espectador com o próprio motor.
const previews = new WeakMap<MediaStream, { pc: RTCPeerConnection; id: number; sound: boolean }>();
// A live nativa leva som? Vai no anúncio da live: quem assiste só pede áudio se houver.
export const nativeSound = (stream: MediaStream) => Boolean(previews.get(stream)?.sound);
// Quantas prévias estão abertas: uma troca de fonte que falha não pode derrubar a live que já está no ar.
let activePreviews = 0;
// Chamadas ao app que não podem virar erro solto no console se o app recusar.
const quiet = (call: Promise<unknown> | undefined) => void call?.catch(() => {});
// A página recarregou com uma live no ar: a captura antiga ficaria rodando no app sem ninguém para pará-la.
quiet(nativeMedia?.stop());
export const isNativeLive = (stream: MediaStream) => previews.has(stream);

// Espera os endereços de rede serem levantados, com teto: a oferta vai inteira numa mensagem só.
function gathered(pc: RTCPeerConnection, maxMs: number) {
  if (pc.iceGatheringState === 'complete') return Promise.resolve();
  return new Promise<void>((resolve) => {
    const done = () => {
      pc.removeEventListener('icegatheringstatechange', check);
      clearTimeout(timer);
      resolve();
    };
    const check = () => pc.iceGatheringState === 'complete' && done();
    const timer = setTimeout(done, maxMs);
    pc.addEventListener('icegatheringstatechange', check);
  });
}

// Começa a captura no motor e devolve a prévia. O MediaStream devolvido representa a live: é o que
// o resto do app recebe no lugar do stream do getDisplayMedia.
export async function captureScreenNative(quality: NativeQuality, audio: AudioMode = 'none'): Promise<MediaStream> {
  const media = nativeMedia;
  if (!media) throw new Error('Disponível só no app.');
  // A pessoa escolhe a tela ou janela na própria página. App mais antigo, sem a lista: tela principal.
  // Fica antes de tudo: desistir aqui não pode mexer numa live que já está no ar (troca de fonte).
  let source: Parameters<typeof media.start>[0] = null;
  let chosen: Awaited<ReturnType<typeof pickNativeSource>> = null;
  if (media.sources) {
    chosen = await pickNativeSource(media);
    if (!chosen) throw new CaptureCancelled();
    source = chosen.source;
  }
  // Som: "só do programa" precisa saber qual é o programa; sem isso vale o som do computador (sempre
  // sem o do próprio Nimbo, para as lives que você assiste não voltarem na sua).
  const sound = audio === 'none' ? undefined : audio === 'window' && chosen?.pid ? { app: chosen.pid } : ('system' as const);
  // Vigia o programa escolhido: se é um jogo em tela cheia (transmitido pelo monitor), a live pausa
  // quando ele sai da frente; em qualquer caso, acaba quando ele fecha.
  const watched = chosen?.kind === 'window' ? chosen.pid : undefined;
  const started = await media.start(source, quality, { audio: sound, watch: watched });
  if (!started.ok) throw new Error(started.error);
  const pc = new RTCPeerConnection();
  try {
    pc.addTransceiver('video', { direction: 'recvonly' });
    let giveUp: ReturnType<typeof setTimeout> | undefined;
    const track = new Promise<MediaStreamTrack>((resolve, reject) => {
      pc.ontrack = (e) => resolve(e.track);
      giveUp = setTimeout(() => reject(new Error('O vídeo do app não respondeu. Tente de novo.')), 10000);
    }).finally(() => clearTimeout(giveUp));
    // Se um passo anterior falhar, esta espera é abandonada: não pode virar erro solto no console.
    track.catch(() => {});
    await pc.setLocalDescription();
    await gathered(pc, 1500);
    const answer = await media.accept(JSON.stringify(pc.localDescription));
    if (!answer.ok) throw new Error(answer.error);
    await pc.setRemoteDescription(JSON.parse(answer.value.answer) as RTCSessionDescriptionInit);
    const stream = new MediaStream([await track]);
    previews.set(stream, { pc, id: answer.value.id, sound: Boolean(sound) });
    activePreviews++;
    followCapture(stream, chosen?.title || 'Tela', Boolean(watched) && source !== null && 'monitor' in source, Boolean(chosen?.game));
    return stream;
  } catch (e) {
    pc.close();
    if (!activePreviews) quiet(media.stop());
    throw e;
  }
}

// Acompanha a captura pelo estado do app: pausa quando o jogo em tela cheia sai da frente, e avisa
// quando o jogo fecha ou a janela capturada deixa de existir.
function followCapture(stream: MediaStream, label: string, pausable: boolean, game: boolean) {
  const media = nativeMedia;
  if (!media?.state) return;
  const changes: Array<(active: boolean) => void> = [];
  const endings: Array<() => void> = [];
  let active: boolean | null = null;
  let ended = false;
  const timer = setInterval(async () => {
    if (!previews.has(stream)) return watch.stop();
    const result = await media.state!().catch(() => null);
    if (!result?.ok || ended) return;
    const { capturing, foreground, ended: exited } = result.value;
    if (pausable && foreground !== null && foreground !== active) {
      active = foreground;
      changes.forEach((cb) => cb(foreground));
    }
    if (exited || !capturing) {
      ended = true;
      watch.stop();
      // Jogo que fechou: a live termina com aviso. Janela comum que sumiu: termina como se a pessoa
      // tivesse parado de compartilhar.
      if (game) endings.forEach((cb) => cb());
      else stream.getVideoTracks()[0]?.dispatchEvent(new Event('ended'));
    }
  }, pausable ? 500 : 1000);
  const watch: GameWatch = {
    label,
    onChange: (cb) => {
      changes.push(cb);
      if (active !== null) cb(active);
    },
    onEnded: (cb) => {
      endings.push(cb);
      if (ended && game) cb();
    },
    stop: () => clearInterval(timer),
  };
  attachGameWatch(stream, watch);
}

// Fecha só a prévia. A captura continua (troca de fonte) até alguém chamar stopNative.
export function releaseNativePreview(stream: MediaStream) {
  const preview = previews.get(stream);
  if (!preview) return;
  previews.delete(stream);
  activePreviews--;
  preview.pc.close();
  quiet(nativeMedia?.close(preview.id));
}

export function stopNative(stream: MediaStream) {
  releaseNativePreview(stream);
  quiet(nativeMedia?.stop());
}

// Entrega ao motor a oferta de um espectador e devolve a resposta para mandar de volta.
export async function answerNative(offer: RTCSessionDescriptionInit) {
  const media = nativeMedia;
  if (!media) throw new Error('Disponível só no app.');
  const result = await media.accept(JSON.stringify(offer));
  if (!result.ok) throw new Error(result.error);
  return { id: result.value.id, answer: JSON.parse(result.value.answer) as RTCSessionDescriptionInit };
}

// Acrescenta stereo=1 ao formato Opus da oferta (RFC 7587: sem isso, quem envia pode mandar mono).
export function stereoOpus(sdp: string) {
  const opus = [...sdp.matchAll(/^a=rtpmap:(\d+) opus\/48000\/2\r?$/gim)].map((m) => m[1]);
  let out = sdp;
  for (const pt of opus)
    out = out.replace(new RegExp(`^(a=fmtp:${pt} (?!.*stereo=)[^\\r\\n]*)`, 'm'), '$1;stereo=1');
  return out;
}

type Incoming = {
  pc: RTCPeerConnection;
  uid: string;
  kind: StreamKind;
  streamId: string;
  stream: MediaStream;
  sound: boolean;
  nonce: string;
  // Falhas seguidas desta live; volta a zero quando conecta.
  tries: number;
  timer: ReturnType<typeof setTimeout>;
  // Espera depois de "desconectado" antes de refazer a conexão.
  lost?: ReturnType<typeof setTimeout>;
};

type Deps = {
  ice: () => RTCConfiguration;
  send: (to: string, msg: NativeSignal) => Promise<void>;
  // Chamado quando o vídeo de uma live nativa chega ou vai embora.
  onChange: () => void;
};

const TRIES = 3;
const CONNECT_MS = 12000;
// O motor encerra a conexão do lado dele sem aviso; o navegador só marca "falhou" bem depois.
const LOST_MS = 5000;

// Lado de quem assiste: uma conexão só de recepção para cada live nativa do canal.
export class NativeReceiver {
  private conns = new Map<string, Incoming>();
  // Lives que já esgotaram as tentativas; só tenta de novo quando a live recomeça.
  private gaveUp = new Set<string>();

  constructor(private deps: Deps) {}

  // Abre conexão para as lives nativas anunciadas e fecha as que acabaram.
  sync(broadcasts: Broadcasts, members: string[], self: string) {
    const wanted = new Map<string, { uid: string; kind: StreamKind; streamId: string; sound: boolean }>();
    for (const [uid, kinds] of Object.entries(broadcasts)) {
      if (uid === self || !members.includes(uid)) continue;
      for (const [kind, live] of Object.entries(kinds))
        if (live?.native)
          wanted.set(`${uid}:${kind}`, {
            uid,
            kind: kind as StreamKind,
            streamId: live.streamId,
            sound: Boolean(live.sound),
          });
    }
    for (const [key, conn] of this.conns)
      {
      const live = wanted.get(key);
      // Live acabou, recomeçou, ou passou a ter (ou deixou de ter) som: refaz a conexão.
      if (live?.streamId !== conn.streamId || live.sound !== conn.sound) this.close(key);
    }
    for (const key of [...this.gaveUp]) {
      const [base, streamId] = key.split('|');
      if (wanted.get(base)?.streamId !== streamId) this.gaveUp.delete(key);
    }
    for (const [key, live] of wanted)
      if (!this.conns.has(key) && !this.gaveUp.has(`${key}|${live.streamId}`)) this.open(key, live, 0);
  }

  private open(
    key: string,
    live: { uid: string; kind: StreamKind; streamId: string; sound: boolean },
    tries: number,
  ) {
    const pc = new RTCPeerConnection(this.deps.ice());
    const stream = new MediaStream();
    const retry = () => {
      if (this.conns.get(key)?.pc !== pc) return;
      const failures = conn.tries + 1;
      this.close(key);
      if (failures < TRIES) this.open(key, live, failures);
      else {
        this.gaveUp.add(`${key}|${live.streamId}`);
        console.warn('[nimbo] live do app não conectou', live.uid.slice(0, 5));
        this.deps.onChange();
      }
    };
    const nonce = crypto.randomUUID();
    const conn: Incoming = { pc, ...live, stream, nonce, tries, timer: setTimeout(retry, CONNECT_MS) };
    this.conns.set(key, conn);
    pc.addTransceiver('video', { direction: 'recvonly' });
    if (live.sound) pc.addTransceiver('audio', { direction: 'recvonly' });
    pc.ontrack = ({ track }) => {
      stream.addTrack(track);
      track.onunmute = () => this.deps.onChange();
      this.deps.onChange();
    };
    pc.onconnectionstatechange = () => {
      clearTimeout(conn.lost);
      if (pc.connectionState === 'connected') {
        clearTimeout(conn.timer);
        // Conectou: falhas antigas não contam mais (três quedas espalhadas em horas não são desistência).
        conn.tries = 0;
      } else if (pc.connectionState === 'failed') retry();
      else if (pc.connectionState === 'disconnected') conn.lost = setTimeout(retry, LOST_MS);
      this.deps.onChange();
    };
    void (async () => {
      const offer = await pc.createOffer();
      // Sem pedir estéreo, o navegador recebe o som do jogo em mono.
      await pc.setLocalDescription({ type: 'offer', sdp: stereoOpus(offer.sdp || '') });
      await gathered(pc, 2000);
      if (this.conns.get(key)?.pc !== pc || !pc.localDescription) return;
      await this.deps.send(live.uid, { type: 'offer', sdp: pc.localDescription.toJSON(), native: live.kind, nonce });
    })().catch((e) => {
      console.warn('[nimbo] oferta para o app', e);
      retry();
    });
  }

  // Resposta do motor do transmissor à nossa oferta.
  async onAnswer(uid: string, kind: StreamKind, sdp: RTCSessionDescriptionInit, nonce?: string) {
    const conn = this.conns.get(`${uid}:${kind}`);
    // Só vale a resposta à oferta desta conexão (ver nonce em NativeSignal).
    if (!conn || conn.nonce !== nonce || conn.pc.signalingState !== 'have-local-offer') return;
    await conn.pc.setRemoteDescription(sdp);
  }

  // Vídeos recebidos, por participante e pelo id anunciado da live (como os do navegador).
  streams() {
    const out: Record<string, Record<string, MediaStream>> = {};
    for (const conn of this.conns.values())
      if (conn.stream.getTracks().length) (out[conn.uid] ??= {})[conn.streamId] = conn.stream;
    return out;
  }

  has(uid: string) {
    for (const conn of this.conns.values()) if (conn.uid === uid && conn.stream.getTracks().length) return true;
    return false;
  }

  // Como está cada live nativa, para o player: tocando, conectando ou sem conseguir conectar.
  states(broadcasts: Broadcasts, self: string) {
    const out: Record<string, 'playing' | 'connecting' | 'failed'> = {};
    for (const [uid, kinds] of Object.entries(broadcasts)) {
      if (uid === self) continue;
      for (const [kind, live] of Object.entries(kinds)) {
        if (!live?.native) continue;
        const key = `${uid}:${kind}`;
        const conn = this.conns.get(key);
        out[key] = this.gaveUp.has(`${key}|${live.streamId}`)
          ? 'failed'
          : conn && conn.pc.connectionState === 'connected' && conn.stream.getTracks().length
            ? 'playing'
            : 'connecting';
      }
    }
    return out;
  }

  // "Tentar de novo" ou volta da rede: esquece as desistências e refaz as conexões dessa pessoa
  // (ou de todos). Quem chama faz o sync em seguida.
  reset(uid?: string) {
    for (const [key, conn] of [...this.conns]) if (!uid || conn.uid === uid) this.close(key);
    for (const key of [...this.gaveUp]) if (!uid || key.startsWith(uid + ':')) this.gaveUp.delete(key);
  }

  // Para a conta de uso do servidor de retransmissão.
  connections() {
    return [...this.conns.values()].map((c) => c.pc);
  }

  private close(key: string) {
    const conn = this.conns.get(key);
    if (!conn) return;
    clearTimeout(conn.timer);
    clearTimeout(conn.lost);
    this.conns.delete(key);
    conn.pc.onconnectionstatechange = null;
    conn.pc.close();
    this.deps.onChange();
  }

  closeAll() {
    for (const key of [...this.conns.keys()]) this.close(key);
    this.gaveUp.clear();
  }
}
