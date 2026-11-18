//! Motor de mídia: captura uma tela ou janela, codifica uma vez e envia por WebRTC para cada
//! espectador. É o que a casca do app (e o modo `serve` de teste) usa.
//!
//! - Um codificador para todos: a mesma sequência H.264 vai para cada conexão.
//! - O vídeo é codificado pela placa sem sair da GPU (`hw.rs`); sem esse codificador, cai para
//!   software (OpenH264) em metade da resolução e 30 quadros por segundo.
//! - Controle de banda: a taxa do codificador segue o espectador com a rede mais apertada.
//! - Áudio do jogo (`set_audio`): capturado por processo, codificado em Opus uma vez (`audio.rs`) e
//!   enviado na mesma conexão do vídeo para quem pediu áudio na oferta.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use windows::core::Interface;
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::AnyError;
use crate::audio;
use crate::hw::HwEncoder;

/// Piso do controle de banda: abaixo disto a imagem fica ruim demais.
const MIN_BITRATE: u32 = 800_000;
/// Sem nenhum pacote do espectador por este tempo, a banda estimada dele deixa de valer.
const STALE: Duration = Duration::from_secs(2);
const SOFTWARE_FPS: u32 = 30;
/// Depois de um erro no codificador, espera isto antes de recriar (criar leva de 30 a 300 ms).
const RETRY: Duration = Duration::from_secs(1);
/// Intervalo mínimo entre quadros completos.
const KEYFRAME_GAP: Duration = Duration::from_millis(300);
/// De quanto em quanto o relógio do motor olha se precisa repetir um quadro.
const TICK: Duration = Duration::from_millis(50);
/// Com a tela parada, repete o último quadro neste ritmo.
const IDLE_EVERY: Duration = Duration::from_millis(1000);
/// Com a live pausada, manda a tela de pausa neste ritmo.
const PAUSE_EVERY: Duration = Duration::from_millis(500);
/// Cor da tela de pausa (o fundo escuro do Nimbo), em RGBA 0–1 e em BGRA 0–255.
const PAUSE_COLOR: [f32; 4] = [0.043, 0.063, 0.125, 1.0];
const PAUSE_BGRA: [u8; 4] = [32, 16, 11, 255];
/// Teto de conexões abertas: um canal tem no máximo 20 pessoas, cada uma com a prévia e uma nova tentativa.
const MAX_VIEWERS: usize = 48;
/// Rede que some por este tempo encerra a conexão do espectador (ele refaz do lado dele).
const LOST: Duration = Duration::from_secs(10);
const SOFTWARE_BITRATE: u32 = 4_000_000;

/// Um degrau da escada de qualidade: altura máxima da imagem, teto de quadros por segundo e a taxa
/// abaixo da qual o motor desce para o degrau seguinte.
struct Step {
    height: u32,
    fps: u32,
    floor: u32,
}

/// Escada de qualidade. Medido em 15/11/2026 com o espectador limitado a 3 Mbps: o controle de banda
/// pedia 0,8 Mbps, mas o codificador da placa não comprime 1080p com movimento abaixo de ~4 Mbps;
/// continuava mandando 4 Mbps, 40% se perdia e quem assistia ficava sem imagem. Imagem menor precisa
/// de bem menos bits, então a taxa pedida passa a caber.
const LADDER: [Step; 5] = [
    Step {
        height: u32::MAX,
        fps: u32::MAX,
        floor: 4_000_000,
    },
    Step {
        height: 720,
        fps: u32::MAX,
        floor: 2_800_000,
    },
    Step {
        height: 720,
        fps: 30,
        floor: 1_700_000,
    },
    Step {
        height: 540,
        fps: 30,
        floor: 1_000_000,
    },
    Step {
        height: 360,
        fps: 30,
        floor: 0,
    },
];
/// Para descer, a taxa pedida precisa ficar apertada por este número de medições seguidas (uma por
/// segundo): uma oscilação curta da estimativa não muda a resolução.
const STEP_DOWN_AFTER: u8 = 2;
/// Para subir, a taxa pedida precisa passar com folga do piso do degrau de cima por mais tempo:
/// subir e descer toda hora troca a resolução na tela de quem assiste.
const STEP_UP_AFTER: u8 = 6;
/// Subida que não se sustentou (desceu de novo em menos de `BOUNCE`) dobra a espera da próxima, até
/// este teto. Medido com 3 Mbps de banda: sem isto a live subia para 720p e caía a cada ~12 s, com
/// travadas de 1 a 4 s em cada volta.
const STEP_UP_MAX: u8 = 60;
const BOUNCE: Duration = Duration::from_secs(25);
/// Perda a partir da qual o motor conclui que está mandando mais do que o caminho aguenta. Perda
/// pequena e constante (3% medidos) é ruído da rede e se resolve com reenvio, sem baixar a taxa.
const HEAVY_LOSS: f32 = 0.10;
/// Por quanto tempo a taxa fica presa abaixo do que foi entregue; dobra a cada recaída, até o teto.
const LIMIT_HOLD: Duration = Duration::from_secs(30);
const LIMIT_HOLD_MAX: Duration = Duration::from_secs(240);

/// Teto da taxa depois de uma perda alta: uma folga abaixo do que de fato chegou a quem assiste
/// (o que foi enviado a ele, com reenvios, menos a fração perdida).
fn delivered_limit(sent: u64, loss: f32) -> u32 {
    let delivered = sent as f64 * f64::from(1.0 - loss.clamp(0.0, 1.0));
    ((delivered * 0.85) as u64).clamp(u64::from(MIN_BITRATE), u64::from(u32::MAX)) as u32
}

/// Degrau mantido por este tempo depois de subir: a espera volta ao normal.
const SETTLED: Duration = Duration::from_secs(90);

/// O degrau que a taxa pedida comporta: o primeiro cujo piso ela alcança.
fn fitting_step(wanted: u32) -> usize {
    LADDER
        .iter()
        .position(|rung| wanted >= rung.floor)
        .unwrap_or(LADDER.len() - 1)
}

/// O degrau em que o motor deve ficar. `fit`: o degrau que a taxa pedida comporta. `over`: o
/// codificador produz bem mais do que o pedido (não comprime tanto nesta resolução).
fn next_step(
    step: usize,
    fit: usize,
    over: bool,
    tight_for: u8,
    roomy_for: u8,
    up_after: u8,
) -> usize {
    let last = LADDER.len() - 1;
    if tight_for >= STEP_DOWN_AFTER && step < last {
        // Desce direto para onde a taxa cabe; se o problema é só o codificador, um degrau.
        fit.max(step + 1).min(last)
    } else if roomy_for >= up_after && step > 0 && !over {
        step - 1
    } else {
        step
    }
}
/// Servidores STUN: os mesmos do site, para descobrir o endereço público.
const STUN_SERVERS: [&str; 2] = ["stun.cloudflare.com:3478", "stun.l.google.com:19302"];

/// O que capturar.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    PrimaryMonitor,
    /// Índice do monitor, como o Windows enumera (a partir de 1).
    Monitor(usize),
    /// Janela, pelo identificador (HWND).
    Window(isize),
}

/// De onde vem o som da live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioSource {
    /// Só o som deste processo e dos filhos dele (o jogo).
    Include(u32),
    /// Todo o som do computador, menos o deste processo e dos filhos dele. Inclui as vozes de apps
    /// de chamada (Discord...): para o "Computador" da live, use [`AudioSource::SystemExcept`].
    Exclude(u32),
    /// Todo o som do computador, menos o de `own` e dos filhos dele (o próprio Nimbo: passe
    /// `std::process::id()`; o WebView2 é filho dele) e menos o dos apps de chamada de voz
    /// (`nimbo_audio::CALL_APPS`: Discord, TeamSpeak, Teams, Zoom...), para quem está na chamada
    /// não se ouvir na live. É o que o "Computador" do site pede (`audio: "system"`). Um programa
    /// que começa a tocar depois entra em menos de um segundo. Ver
    /// `nimbo_audio::Mode::SystemExcept`.
    SystemExcept { own: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Quality {
    pub fps: u32,
    /// Teto da taxa de bits, em bits por segundo.
    pub bitrate: u32,
    /// Altura máxima enviada; telas maiores são reduzidas na GPU.
    pub max_height: u32,
}

impl Default for Quality {
    fn default() -> Self {
        Self {
            fps: 60,
            bitrate: 8_000_000,
            max_height: 1080,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub capturing: bool,
    pub viewers: usize,
    pub encoder: String,
    pub target_bitrate: u32,
    /// Quanto o codificador produziu de verdade no último segundo medido, em bits por segundo.
    pub produced_bitrate: u32,
    pub frames_encoded: usize,
    /// A captura de áudio está ligada e viva.
    pub audio: bool,
    /// Pacotes Opus (20 ms cada) codificados; só anda enquanto alguém ouve.
    pub audio_packets: usize,
}

/// Um quadro já codificado (H.264 Annex B), o mesmo para todos os espectadores.
struct Encoded {
    data: Arc<[u8]>,
    at: Instant,
}

/// O que acorda a conexão de um espectador.
enum Msg {
    Frame(Arc<Encoded>),
    Audio(Arc<audio::Packet>),
    /// Pacote da rede: conteúdo, de onde veio e por qual endereço local entrou.
    Packet(Vec<u8>, SocketAddr, SocketAddr),
    Tick,
    Close,
}

/// O que o capturador/codificador e as conexões compartilham.
struct Shared {
    /// Uma fila por espectador: nenhum quadro pode ser pulado, senão a imagem quebra.
    subscribers: Mutex<HashMap<u64, Sender<Msg>>>,
    /// Alguém entrou ou perdeu pacotes: o próximo quadro precisa ser completo.
    keyframe: AtomicBool,
    viewers: AtomicUsize,
    encoded: AtomicUsize,
    encoder: Mutex<String>,
    /// Banda estimada de cada espectador, em bits por segundo.
    estimates: Mutex<HashMap<u64, (u64, Instant)>>,
    /// Caminho de cada espectador no último segundo medido: fração de pacotes perdidos e quanto foi
    /// enviado a ele (bits por segundo, com reenvios).
    losses: Mutex<HashMap<u64, (f32, u64, Instant)>>,
    /// Taxa de bits que o codificador está usando agora.
    target: AtomicU32,
    /// Taxa que o codificador produziu de verdade no último segundo medido.
    produced: AtomicU32,
    next_id: AtomicU64,
    quality: Mutex<Quality>,
    /// A fonte capturada fechou (a janela ou o jogo foi encerrado).
    ended: AtomicBool,
    coder: Mutex<Coder>,
    /// Live pausada (o jogo saiu da frente): em vez da tela, vai uma cor só.
    paused: AtomicBool,
    /// Quem pediu áudio na oferta: só essas conexões recebem os pacotes Opus.
    listeners: Mutex<HashMap<u64, Sender<Msg>>>,
    /// Há captura de áudio ligada: a taxa dela sai da banda do vídeo.
    audio: AtomicBool,
    audio_packets: AtomicUsize,
    /// Zero do tempo RTP do áudio. Vale para o motor inteiro, então trocar a captura de áudio no
    /// meio da live não faz o tempo voltar.
    epoch: Instant,
    /// Tempo RTP em que a última captura de áudio parou. A placa de som pode andar um pouco mais
    /// rápido que o relógio; a captura seguinte nunca começa antes deste ponto.
    audio_rtp: AtomicU64,
}

impl Shared {
    fn publish(&self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        self.encoded.fetch_add(1, Ordering::Relaxed);
        let frame = Arc::new(Encoded {
            data: data.into(),
            at: Instant::now(),
        });
        // Quem saiu teve a fila fechada: sai da lista.
        self.subscribers
            .lock()
            .unwrap()
            .retain(|_, s| s.send(Msg::Frame(frame.clone())).is_ok());
    }

    fn publish_audio(&self, packet: audio::Packet) {
        self.audio_packets.fetch_add(1, Ordering::Relaxed);
        let packet = Arc::new(packet);
        self.listeners
            .lock()
            .unwrap()
            .retain(|_, s| s.send(Msg::Audio(packet.clone())).is_ok());
    }
}

enum Codec264 {
    Hardware(Box<HwEncoder>),
    Software {
        encoder: Box<Encoder>,
        last: Instant,
        /// Último quadro em BGRA, na resolução enviada: é o que se repete com a tela parada.
        small: Vec<u8>,
        dims: (usize, usize),
    },
}

/// Deixa passar, na média, `fps` quadros por segundo de uma fonte que entrega mais do que isso.
///
/// Descartar "o quadro que chegou cedo demais desde o último aceito" derruba a taxa para um divisor
/// da taxa da fonte. Medido em 15/11/2026: uma janela a 75 quadros por segundo saía a 37,5 com teto de
/// 60 (um sim, um não), e uma tela de 144 Hz dá 48. Aqui o teto segue um relógio próprio: passa o
/// primeiro quadro depois de cada batida, então a média fica no teto, com intervalos um pouco
/// desiguais (em 75 Hz, quatro quadros em cada cinco).
struct Pacer {
    due: Instant,
}

impl Pacer {
    /// Quadro que chega até isto antes da batida conta como pontual (a fonte não é um relógio exato).
    const EARLY: Duration = Duration::from_millis(2);

    fn new(now: Instant) -> Self {
        Self { due: now }
    }

    fn accept(&mut self, now: Instant, fps: u32) -> bool {
        if fps == u32::MAX {
            return true;
        }
        if now + Self::EARLY < self.due {
            return false;
        }
        let gap = Duration::from_micros(1_000_000 / u64::from(fps.max(1)));
        // A batida seguinte conta a partir da anterior (é o que mantém a média). Depois de uma pausa
        // (tela parada), no máximo um intervalo de atraso é recuperado: nada de rajada.
        self.due = (self.due + gap).max(now.checked_sub(gap).unwrap_or(now));
        true
    }
}

/// O codificador de vídeo e o que ele lembra entre quadros. Fica no estado compartilhado porque dois
/// lugares o alimentam: a captura (quadro novo) e o relógio do motor (repetir o quadro quando a tela
/// está parada, mandar a tela de pausa, atender um pedido de quadro completo).
struct Coder {
    codec: Option<Codec264>,
    /// Tamanho da fonte para o qual o codificador foi criado; janela redimensionada recria.
    size: (u32, u32),
    /// Dispositivo Direct3D da captura atual: cada captura cria o seu, e o codificador é preso a ele.
    device: usize,
    out: Vec<Vec<u8>>,
    checked: Instant,
    /// Última vez que o codificador aceitou um quadro (novo ou repetido).
    fed: Instant,
    /// Ritmo dos quadros novos: o teto de quadros por segundo é aplicado aqui, não na captura.
    pacer: Pacer,
    last_key: Instant,
    /// Depois de um erro, só tenta de novo a partir daqui.
    retry_at: Instant,
    /// Bytes de vídeo produzidos desde `checked`: mostra se a tela está parada.
    produced: u64,
    /// Degrau atual da escada de qualidade (`LADDER`).
    step: usize,
    /// Há quantas medições seguidas a taxa está apertada demais para o degrau, ou folgada.
    tight_for: u8,
    roomy_for: u8,
    /// Quantas medições folgadas seguidas são precisas para subir (dobra quando a subida não vinga).
    up_after: u8,
    /// Quando o motor subiu de degrau pela última vez.
    raised: Option<Instant>,
    /// Teto da taxa depois de uma perda alta, e desde quando vale.
    limit: Option<(u32, Instant)>,
    limit_hold: Duration,
}

// SAFETY: o Coder só é usado com o Mutex do estado compartilhado travado, uma thread por vez. O
// codificador da placa já é Send; o OpenH264 não tem vínculo com a thread que o criou.
unsafe impl Send for Coder {}

impl Coder {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            codec: None,
            size: (0, 0),
            device: 0,
            out: Vec::new(),
            checked: now,
            fed: now,
            pacer: Pacer::new(now),
            last_key: now,
            retry_at: now,
            produced: 0,
            step: 0,
            tight_for: 0,
            roomy_for: 0,
            up_after: STEP_UP_AFTER,
            raised: None,
            limit: None,
            limit_hold: LIMIT_HOLD,
        }
    }

    /// Solta o codificador (a captura parou ou vai trocar de fonte).
    fn reset(&mut self) {
        self.codec = None;
        self.size = (0, 0);
        self.device = 0;
    }
}

/// Copia o quadro para a memória em metade da resolução (caminho por software).
fn shrink(
    frame: &mut Frame,
    small: &mut Vec<u8>,
    dims: &mut (usize, usize),
) -> Result<(), AnyError> {
    let mut buffer = frame.buffer()?;
    let (width, height, pitch) = (
        buffer.width() as usize,
        buffer.height() as usize,
        buffer.row_pitch() as usize,
    );
    let raw = buffer.as_raw_buffer();
    // Metade da resolução, com lados pares (exigência do H.264).
    let (w, h) = ((width / 2) & !1, (height / 2) & !1);
    small.resize(w * h * 4, 0);
    for y in 0..h {
        let row = &raw[y * 2 * pitch..];
        let line = &mut small[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            line[x * 4..x * 4 + 4].copy_from_slice(&row[x * 8..x * 8 + 4]);
        }
    }
    *dims = (w, h);
    Ok(())
}

fn software(bitrate: u32) -> Result<Codec264, AnyError> {
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate.min(SOFTWARE_BITRATE)))
        .max_frame_rate(FrameRate::from_hz(SOFTWARE_FPS as f32))
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .complexity(Complexity::Low)
        .skip_frames(false);
    let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)?;
    Ok(Codec264::Software {
        encoder: Box::new(encoder),
        last: Instant::now(),
        small: Vec::new(),
        dims: (0, 0),
    })
}

impl Shared {
    /// Entrega um quadro capturado (`Some`) ou pede para repetir o último (`None`). Um erro aqui
    /// nunca derruba a captura: o codificador é recriado e a live segue.
    fn feed(&self, frame: Option<&mut Frame>) {
        let mut coder = self.coder.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() < coder.retry_at {
            return;
        }
        if let Err(e) = self.try_feed(&mut coder, frame) {
            eprintln!("codificador de vídeo falhou ({e}); recriando");
            coder.reset();
            coder.retry_at = Instant::now() + RETRY;
            self.keyframe.store(true, Ordering::Relaxed);
        }
    }

    fn try_feed(&self, coder: &mut Coder, mut frame: Option<&mut Frame>) -> Result<(), AnyError> {
        match frame.as_deref() {
            Some(frame) => self.codec_for(coder, frame)?,
            // Repetição sem codificador: ainda não chegou nenhum quadro desta captura.
            None if coder.codec.is_none() => return Ok(()),
            None => {}
        }
        // Teto de quadros por segundo: o da qualidade pedida e o do degrau da escada. A captura entrega
        // mais rápido que o teto (ver `launch`) e o ritmo certo sai daqui. O codificador por software
        // tem o próprio teto, mais abaixo.
        let cap = self.quality.lock().unwrap().fps.min(LADDER[coder.step].fps);
        if frame.is_some()
            && matches!(coder.codec, Some(Codec264::Hardware(_)))
            && !coder.pacer.accept(Instant::now(), cap)
        {
            return Ok(());
        }
        // Ninguém assistindo ainda: não codifica, só guarda o quadro. A captura só entrega quadro
        // quando a tela muda; sem isto, quem entrasse com a tela parada não teria o que ver.
        if self.viewers.load(Ordering::Relaxed) == 0 {
            if let (Some(frame), Some(codec)) = (frame.as_mut(), coder.codec.as_mut()) {
                match codec {
                    Codec264::Hardware(encoder) => encoder.copy(frame.as_raw_texture()),
                    // Trazer o quadro da GPU para a memória custa caro: sem plateia, um por segundo basta.
                    Codec264::Software {
                        last, small, dims, ..
                    } => {
                        if last.elapsed() >= Duration::from_secs(1) {
                            *last = Instant::now();
                            shrink(frame, small, dims)?;
                        }
                    }
                }
            }
            return Ok(());
        }
        self.adapt(coder);
        if coder.codec.is_none() {
            // A escada trocou de degrau: recria já com este quadro, ou espera o próximo capturado.
            match frame.as_deref() {
                Some(frame) => self.codec_for(coder, frame)?,
                None => return Ok(()),
            }
        }
        let paused = self.paused.load(Ordering::Relaxed);
        let mut keyframe = self.keyframe.swap(false, Ordering::Relaxed);
        // Cada pedido de quadro completo custa caro (~250 KB em 1080p) e vale para todos: junta os
        // pedidos que chegam em rajada (um espectador entrando costuma pedir duas vezes).
        if keyframe && coder.last_key.elapsed() < KEYFRAME_GAP {
            self.keyframe.store(true, Ordering::Relaxed);
            keyframe = false;
        }
        let Coder { codec, out, .. } = coder;
        let accepted = match codec.as_mut().ok_or("sem codificador")? {
            Codec264::Hardware(encoder) => {
                if paused {
                    encoder.fill(PAUSE_COLOR);
                } else if let Some(frame) = frame.as_deref() {
                    encoder.copy(frame.as_raw_texture());
                }
                encoder.submit(keyframe, out)?
            }
            Codec264::Software {
                encoder,
                last,
                small,
                dims,
            } => {
                let fresh = frame.is_some() && !paused;
                if fresh
                    && last.elapsed()
                        < Duration::from_micros(1_000_000 / u64::from(SOFTWARE_FPS) - 2_000)
                {
                    false
                } else {
                    if let (true, Some(frame)) = (fresh, frame.as_mut()) {
                        shrink(frame, small, dims)?;
                    }
                    if dims.0 == 0 || dims.1 == 0 {
                        false
                    } else {
                        if paused {
                            for pixel in small.as_chunks_mut::<4>().0 {
                                *pixel = PAUSE_BGRA;
                            }
                        }
                        *last = Instant::now();
                        let yuv = YUVBuffer::from_rgb_source(BgraSliceU8::new(small, *dims));
                        if keyframe {
                            encoder.force_intra_frame();
                        }
                        out.push(encoder.encode(&yuv)?.to_vec());
                        true
                    }
                }
            }
        };
        if accepted {
            coder.fed = Instant::now();
            if keyframe {
                coder.last_key = coder.fed;
            }
        } else if keyframe {
            // O quadro foi descartado: o pedido de quadro completo vale para o próximo.
            self.keyframe.store(true, Ordering::Relaxed);
        }
        let ready = std::mem::take(&mut coder.out);
        for data in ready {
            coder.produced += data.len() as u64;
            self.publish(data);
        }
        Ok(())
    }

    fn codec_for(&self, coder: &mut Coder, frame: &Frame) -> Result<(), AnyError> {
        let input = (frame.width(), frame.height());
        let device = frame.device().as_raw() as usize;
        if coder.codec.is_some() && coder.size == input && coder.device == device {
            return Ok(());
        }
        // Solta o antigo antes de criar o novo: os dois juntos disputariam a placa.
        coder.codec = None;
        let quality = *self.quality.lock().unwrap();
        let rung = &LADDER[coder.step];
        let height = quality.max_height.min(rung.height).min(input.1);
        let scale = f64::from(height) / f64::from(input.1);
        let output = (
            (f64::from(input.0) * scale) as u32,
            (f64::from(input.1) * scale) as u32,
        );
        let bitrate = self.target.load(Ordering::Relaxed);
        let codec = match HwEncoder::new(
            frame.device(),
            frame.device_context(),
            input,
            output,
            quality.fps.min(rung.fps),
            bitrate,
        ) {
            Ok(encoder) => {
                *self.encoder.lock().unwrap() =
                    format!("{} {}x{}", encoder.name, encoder.width, encoder.height);
                Codec264::Hardware(Box::new(encoder))
            }
            Err(e) => {
                eprintln!("sem codificador na placa de vídeo ({e}); usando software");
                let software = software(bitrate)?;
                *self.encoder.lock().unwrap() = "OpenH264 (software)".into();
                software
            }
        };
        coder.codec = Some(codec);
        coder.size = input;
        coder.device = device;
        // Codificador novo começa por um quadro completo, sem esperar o intervalo entre pedidos.
        coder.last_key = Instant::now() - KEYFRAME_GAP;
        self.keyframe.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Controle de banda: uma vez por segundo, ajusta a taxa para a conexão mais apertada.
    fn adapt(&self, coder: &mut Coder) {
        let elapsed = coder.checked.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let produced = (coder.produced as f64 * 8.0 / elapsed.as_secs_f64()) as u64;
        coder.checked = Instant::now();
        coder.produced = 0;
        self.produced
            .store(produced.min(u64::from(u32::MAX)) as u32, Ordering::Relaxed);
        let ceiling = self.quality.lock().unwrap().bitrate;
        // Só conta quem respondeu há pouco: conexão que caiu sem avisar para de dar retorno, a
        // estimativa dela despenca e arrastaria a qualidade de todos até o tempo limite.
        let lowest = self
            .estimates
            .lock()
            .unwrap()
            .values()
            .filter(|(_, seen)| seen.elapsed() < STALE)
            .map(|(bps, _)| *bps)
            .min();
        let Some(lowest) = lowest else { return };
        // O áudio sai da mesma banda: o vídeo fica com o que sobra.
        let audio = if self.audio.load(Ordering::Relaxed) {
            u64::from(audio::BITRATE)
        } else {
            0
        };
        let mut wanted = ((lowest * 9 / 10)
            .saturating_sub(audio)
            .min(u64::from(u32::MAX)) as u32)
            .clamp(MIN_BITRATE.min(ceiling), ceiling);
        let current = self.target.load(Ordering::Relaxed);
        // Freio pela perda medida. A estimativa de banda, sozinha, oscila demais num caminho que
        // descarta o excesso sem atrasar (medido com 3 Mbps: ia de 0,8 a 3,7 Mbps a cada ~10 s, e a
        // imagem travava em cada volta). Com perda alta, o que chega a quem assiste é o que foi
        // enviado menos o que se perdeu: a taxa fica presa uma folga abaixo disso por um tempo, e
        // também não desce muito além, porque esse tanto comprovadamente passa.
        // O espectador com perda alta a quem menos chega: o teto que ele comporta e quanto foi enviado.
        let worst = self
            .losses
            .lock()
            .unwrap()
            .values()
            .filter(|(loss, _, seen)| *loss >= HEAVY_LOSS && seen.elapsed() < STALE)
            .map(|(loss, sent, _)| (delivered_limit(*sent, *loss), *sent))
            .min();
        if let Some((limit, sent)) = worst {
            match coder.limit {
                // Perda com o envio já dentro do teto: o teto ainda está alto. Aperta e segura por
                // mais tempo.
                Some((old, _)) if sent <= u64::from(old) / 10 * 12 => {
                    coder.limit_hold = (coder.limit_hold * 2).min(LIMIT_HOLD_MAX);
                    coder.limit = Some((old.min(limit), Instant::now()));
                }
                // Perda com o envio ainda acima do teto: o codificador não terminou de descer
                // (troca de degrau, reenvios). Não aperta mais, senão a taxa desce até o mínimo sem
                // necessidade.
                Some(_) => {}
                None => coder.limit = Some((limit, Instant::now())),
            }
        }
        match coder.limit {
            Some((_, since)) if since.elapsed() >= coder.limit_hold => {
                coder.limit = None;
                // Passou o prazo sem recaída: da próxima vez o prazo volta ao normal.
                if since.elapsed() >= coder.limit_hold * 2 {
                    coder.limit_hold = LIMIT_HOLD;
                }
            }
            Some((limit, _)) => {
                let limit = limit.min(ceiling);
                wanted = wanted.clamp((limit / 10 * 6).min(limit), limit);
            }
            None => {}
        }
        // Tela parada (menu, pausa, carregamento): o codificador gasta bem menos que o alvo e a banda
        // estimada cai só por falta de uso, não por rede ruim. Seguir essa queda deixaria todo mundo na
        // qualidade mínima por dezenas de segundos quando a imagem voltasse a se mexer.
        let idle = produced < u64::from(current) / 2;
        if wanted < current && idle {
            return;
        }
        // Escada de qualidade (só no codificador da placa; o de software já trabalha em resolução menor).
        if matches!(coder.codec, Some(Codec264::Hardware(_))) {
            // O codificador não acompanha a taxa pedida: produz bem mais do que o alvo em vigor.
            let over = produced > u64::from(current) * 13 / 10;
            let fit = fitting_step(wanted);
            let tight = fit > coder.step || (over && wanted <= current);
            let roomy = coder.step > 0
                && !over
                && u64::from(wanted) >= u64::from(LADDER[coder.step - 1].floor) * 5 / 4;
            coder.tight_for = if tight {
                coder.tight_for.saturating_add(1)
            } else {
                0
            };
            coder.roomy_for = if roomy {
                coder.roomy_for.saturating_add(1)
            } else {
                0
            };
            // Subiu e ficou: a espera para subir volta ao normal.
            if coder.raised.is_some_and(|at| at.elapsed() >= SETTLED) {
                coder.raised = None;
                coder.up_after = STEP_UP_AFTER;
            }
            let step = next_step(
                coder.step,
                fit,
                over,
                coder.tight_for,
                coder.roomy_for,
                coder.up_after,
            );
            if step != coder.step {
                if step > coder.step {
                    // Desceu logo depois de subir: a rede não aguenta o degrau de cima por enquanto.
                    if coder.raised.is_some_and(|at| at.elapsed() < BOUNCE) {
                        coder.up_after = coder.up_after.saturating_mul(2).min(STEP_UP_MAX);
                    }
                    coder.raised = None;
                } else {
                    coder.raised = Some(Instant::now());
                }
                coder.step = step;
                coder.tight_for = 0;
                coder.roomy_for = 0;
                self.target.store(wanted, Ordering::Relaxed);
                // Outro tamanho de imagem é outro codificador: o próximo quadro capturado recria.
                coder.codec = None;
                return;
            }
        }
        if wanted.abs_diff(current) > current / 12 {
            self.target.store(wanted, Ordering::Relaxed);
            match coder.codec.as_mut() {
                Some(Codec264::Hardware(encoder)) => encoder.set_bitrate(wanted),
                // O OpenH264 não troca a taxa no ar: recria (é rápido) e recomeça de um quadro completo.
                Some(Codec264::Software { dims, small, .. }) => {
                    let (dims, small) = (*dims, std::mem::take(small));
                    if let Ok(Codec264::Software { encoder, last, .. }) = software(wanted) {
                        coder.codec = Some(Codec264::Software {
                            encoder,
                            last,
                            small,
                            dims,
                        });
                        self.keyframe.store(true, Ordering::Relaxed);
                    }
                }
                None => {}
            }
        }
    }
}

struct Capture {
    shared: Arc<Shared>,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = Arc<Shared>;
    type Error = AnyError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { shared: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // Janela encolhida a quase nada: nenhum codificador aceita, e devolver erro aqui encerraria a
        // captura de vez.
        if frame.width() < 16 || frame.height() < 16 {
            return Ok(());
        }
        self.shared.feed(Some(frame));
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.shared.ended.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// Relógio do motor: a captura só entrega quadro quando a tela muda. Com a tela parada, é ele que
/// atende quem acabou de entrar (precisa de um quadro completo), mantém a tela de pausa no ar e
/// repete o último quadro de vez em quando para a conexão não parecer morta.
fn ticker(shared: std::sync::Weak<Shared>) {
    loop {
        std::thread::sleep(TICK);
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if shared.viewers.load(Ordering::Relaxed) == 0 {
            continue;
        }
        let quiet = shared
            .coder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fed
            .elapsed();
        let due = if shared.keyframe.load(Ordering::Relaxed) {
            quiet >= TICK
        } else if shared.paused.load(Ordering::Relaxed) {
            quiet >= PAUSE_EVERY
        } else {
            quiet >= IDLE_EVERY
        };
        if due {
            shared.feed(None);
        }
    }
}

/// Endereço da máquina na rede local, pela rota padrão.
pub(crate) fn local_ip() -> IpAddr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").and_then(|()| s.local_addr()))
        .map_or(IpAddr::from([127, 0, 0, 1]), |a| a.ip())
}

/// Pergunta a um servidor STUN com que endereço público este soquete aparece na internet.
/// É o que permite a conexão direta com quem está fora da rede local.
pub(crate) fn public_address(socket: &UdpSocket) -> Option<SocketAddr> {
    const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
    // Pedido de "binding": tipo 0x0001, tamanho 0, cookie mágico e 12 bytes de identificação.
    let mut request = [0u8; 20];
    request[1] = 0x01;
    request[4..8].copy_from_slice(&MAGIC);
    let id = Instant::now().elapsed().as_nanos() as u64
        ^ u64::from(std::process::id())
        ^ 0x9E37_79B9_7F4A_7C15;
    request[8..16].copy_from_slice(&id.to_le_bytes());
    request[16..20].copy_from_slice(&(socket.local_addr().ok()?.port() as u32).to_le_bytes());

    socket
        .set_read_timeout(Some(Duration::from_millis(400)))
        .ok()?;
    for server in STUN_SERVERS {
        let Some(target) = server
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.find(SocketAddr::is_ipv4))
        else {
            continue;
        };
        if socket.send_to(&request, target).is_err() {
            continue;
        }
        let mut buf = [0u8; 512];
        let Ok((n, from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        // Só vale a resposta do servidor perguntado, com a mesma identificação.
        if from != target || n < 20 || buf[0..2] != [0x01, 0x01] || buf[8..20] != request[8..20] {
            continue;
        }
        let mut i = 20;
        while i + 4 <= n {
            let kind = u16::from_be_bytes([buf[i], buf[i + 1]]);
            let len = usize::from(u16::from_be_bytes([buf[i + 2], buf[i + 3]]));
            let value = i + 4;
            if value + len > n {
                break;
            }
            // XOR-MAPPED-ADDRESS, IPv4: porta e endereço vêm embaralhados com o cookie mágico.
            if kind == 0x0020 && len >= 8 && buf[value + 1] == 0x01 {
                let port = u16::from_be_bytes([buf[value + 2], buf[value + 3]]) ^ 0x2112;
                let ip = [
                    buf[value + 4] ^ MAGIC[0],
                    buf[value + 5] ^ MAGIC[1],
                    buf[value + 6] ^ MAGIC[2],
                    buf[value + 7] ^ MAGIC[3],
                ];
                return Some(SocketAddr::new(IpAddr::from(ip), port));
            }
            i = value + len.div_ceil(4) * 4;
        }
    }
    None
}

/// Reescreve a oferta para o vídeo e o som saírem no mesmo "stream" (`a=msid`).
///
/// O navegador só mantém som e imagem em sincronia quando os dois chegam com o mesmo
/// identificador de stream. Quem só recebe não manda `a=msid`, e a biblioteca de WebRTC, sem ele,
/// sorteia um identificador para cada mídia; aqui todas ganham o mesmo.
fn same_stream(sdp: &str) -> String {
    let mut out = String::with_capacity(sdp.len() + 64);
    let mut media = false;
    for line in sdp.lines() {
        if line.starts_with("m=") {
            media = line.starts_with("m=audio") || line.starts_with("m=video");
        }
        if media && line.starts_with("a=msid:") {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
        if media && let Some(mid) = line.strip_prefix("a=mid:") {
            out.push_str(&format!("a=msid:nimbo nimbo-{mid}\r\n"));
        }
    }
    out
}

/// Instala a criptografia do processo, que o WebRTC exige. Vale uma vez, para o motor e para o player.
pub(crate) fn install_crypto() {
    static CRYPTO: Once = Once::new();
    CRYPTO.call_once(|| str0m::crypto::from_feature_flags().install_process_default());
}

pub struct Engine {
    shared: Arc<Shared>,
    control: Mutex<Option<CaptureControl<Capture, AnyError>>>,
    audio: Mutex<Option<nimbo_audio::Capture>>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        install_crypto();
        let quality = Quality::default();
        let shared = Arc::new(Shared {
            subscribers: Mutex::default(),
            keyframe: AtomicBool::new(false),
            viewers: AtomicUsize::new(0),
            encoded: AtomicUsize::new(0),
            encoder: Mutex::default(),
            estimates: Mutex::default(),
            losses: Mutex::default(),
            target: AtomicU32::new(quality.bitrate),
            next_id: AtomicU64::new(1),
            quality: Mutex::new(quality),
            ended: AtomicBool::new(false),
            coder: Mutex::new(Coder::new()),
            paused: AtomicBool::new(false),
            listeners: Mutex::default(),
            audio: AtomicBool::new(false),
            audio_packets: AtomicUsize::new(0),
            epoch: Instant::now(),
            audio_rtp: AtomicU64::new(0),
            produced: AtomicU32::new(0),
        });
        let weak = Arc::downgrade(&shared);
        std::thread::spawn(move || ticker(weak));
        Self {
            shared,
            control: Mutex::new(None),
            audio: Mutex::new(None),
        }
    }

    /// Liga, troca ou desliga (`None`) o som da live. Vale antes ou durante a live; trocar a
    /// fonte de vídeo (`start`) não mexe no som. O som é codificado em Opus uma vez e vai, na mesma
    /// conexão do vídeo, para os espectadores que pediram áudio na oferta.
    pub fn set_audio(&self, source: Option<AudioSource>) -> Result<(), AnyError> {
        let mut current = self.audio.lock().unwrap();
        // A captura anterior termina (a thread dela sai) antes de a nova começar: as duas juntas
        // mandariam pacotes misturados.
        drop(current.take());
        self.shared.audio.store(false, Ordering::Relaxed);
        let Some(source) = source else { return Ok(()) };
        let mode = match source {
            AudioSource::Include(pid) => nimbo_audio::Mode::Include(pid),
            AudioSource::Exclude(pid) => nimbo_audio::Mode::Exclude(pid),
            AudioSource::SystemExcept { own } => nimbo_audio::Mode::SystemExcept { own },
        };
        let start = audio::frames_in(self.shared.epoch.elapsed())
            .max(self.shared.audio_rtp.load(Ordering::Relaxed));
        let mut packer =
            audio::Packer::new(start).map_err(|e| format!("codificador Opus falhou: {e}"))?;
        let shared = self.shared.clone();
        let mut warned = false;
        // Codifica na própria thread de captura: 20 ms de Opus custam bem menos de 1 ms, e assim
        // o som não passa por mais uma fila.
        let capture = nimbo_audio::Capture::start(mode, move |pcm| {
            // Sem ninguém ouvindo não codifica nada.
            let listening = !shared.listeners.lock().unwrap().is_empty();
            let result = packer.push(pcm, Instant::now(), listening, |packet| {
                shared.publish_audio(packet);
            });
            shared.audio_rtp.store(packer.next_rtp(), Ordering::Relaxed);
            if let Err(e) = result
                && !warned
            {
                warned = true;
                eprintln!("codificador Opus: {e}");
            }
        })
        .map_err(|e| format!("captura de áudio falhou: {e}"))?;
        *current = Some(capture);
        self.shared.audio.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Começa a capturar. Trocar de fonte é parar e começar de novo; os espectadores continuam
    /// conectados e recebem a fonte nova.
    pub fn start(&self, source: Source, quality: Quality) -> Result<(), AnyError> {
        // Enquanto a live começa, mede os roteadores de casa: o primeiro espectador de fora vem depois.
        measure_hops();
        // Confere a fonte nova antes de largar a que está no ar: uma janela que já fechou não pode
        // congelar a live que estava funcionando.
        match source {
            Source::PrimaryMonitor => drop(Monitor::primary()?),
            Source::Monitor(index) => drop(Monitor::from_index(index)?),
            Source::Window(hwnd) => {
                if !Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void).is_valid() {
                    return Err("essa janela não pode ser capturada (fechada ou invisível)".into());
                }
            }
        }
        self.stop_capture();
        self.shared.paused.store(false, Ordering::Relaxed);
        *self.shared.quality.lock().unwrap() = quality;
        self.shared.target.store(quality.bitrate, Ordering::Relaxed);
        self.shared.ended.store(false, Ordering::Relaxed);
        let control = match source {
            Source::PrimaryMonitor => self.launch(Monitor::primary()?, quality)?,
            Source::Monitor(index) => self.launch(Monitor::from_index(index)?, quality)?,
            Source::Window(hwnd) => {
                let window = Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
                if !window.is_valid() {
                    return Err("essa janela não pode ser capturada (fechada ou invisível)".into());
                }
                self.launch(window, quality)?
            }
        };
        *self.control.lock().unwrap() = Some(control);
        Ok(())
    }

    fn launch<T>(
        &self,
        item: T,
        quality: Quality,
    ) -> Result<CaptureControl<Capture, AnyError>, AnyError>
    where
        T: TryInto<GraphicsCaptureItemType> + Send + 'static,
    {
        let settings = Settings::new(
            item,
            CursorCaptureSettings::Default,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            // Metade do intervalo do teto: com o intervalo inteiro, o Windows descartava o quadro que
            // chegava um pouco antes da hora, e uma tela de 75 Hz saía a 37,5 quadros por segundo (144 Hz
            // dá 48). Assim a captura entrega no máximo o dobro do teto, e o `Pacer` acerta a média.
            MinimumUpdateIntervalSettings::Custom(Duration::from_micros(
                500_000 / u64::from(quality.fps.max(1)),
            )),
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            self.shared.clone(),
        );
        Capture::start_free_threaded(settings).map_err(|e| format!("captura falhou: {e}").into())
    }

    fn stop_capture(&self) {
        if let Some(control) = self.control.lock().unwrap().take() {
            let _ = control.stop();
        }
        // O codificador é preso ao dispositivo da captura que acabou.
        self.shared
            .coder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reset();
    }

    /// Pausa a live: quem assiste vê uma tela escura em vez da captura, até despausar. É o que
    /// acontece quando o jogo transmitido sai da frente (Alt+Tab).
    pub fn set_paused(&self, paused: bool) {
        if self.shared.paused.swap(paused, Ordering::Relaxed) != paused {
            // A troca de imagem é brusca: um quadro completo evita borrão na transição.
            self.shared.keyframe.store(true, Ordering::Relaxed);
        }
    }

    /// Para a captura (vídeo e som) e desconecta todo mundo.
    pub fn stop(&self) {
        self.stop_capture();
        drop(self.audio.lock().unwrap().take());
        self.shared.audio.store(false, Ordering::Relaxed);
        self.shared.listeners.lock().unwrap().clear();
        for (_, viewer) in self.shared.subscribers.lock().unwrap().drain() {
            let _ = viewer.send(Msg::Close);
        }
        self.shared.encoder.lock().unwrap().clear();
        self.shared.paused.store(false, Ordering::Relaxed);
    }

    /// Aceita a oferta WebRTC de um espectador (JSON `{type, sdp}`) e devolve o identificador da
    /// conexão e a resposta (JSON `{type, sdp}`), já com os endereços por onde ele pode chegar.
    pub fn accept(&self, offer: &[u8]) -> Result<(u64, Vec<u8>), AnyError> {
        if self.shared.subscribers.lock().unwrap().len() >= MAX_VIEWERS {
            return Err("espectadores demais conectados".into());
        }
        let offer: SdpOffer = serde_json::from_slice(offer)?;
        let remotes = remote_addresses(&offer.to_sdp_string());
        let offer = SdpOffer::from_sdp_string(&same_stream(&offer.to_sdp_string()))?;
        let ceiling = self.shared.quality.lock().unwrap().bitrate;
        let mut rtc = RtcConfig::new()
            .enable_bwe(Some(Bitrate::bps(u64::from(ceiling) / 2)))
            // Uma vez por segundo a conexão informa a perda no caminho (freio da taxa, em `adapt`).
            .set_stats_interval(Some(Duration::from_secs(1)))
            .build(Instant::now());

        // Um soquete por endereço local. Com um só soquete "em qualquer endereço" não dá para saber por
        // qual placa um pacote entrou, e o WebRTC precisa disso para casar com o caminho certo.
        let sockets = bind_all()?;
        let default = local_ip();
        for (local, socket) in &sockets {
            rtc.add_local_candidate(Candidate::host(*local, "udp")?);
            // O endereço público só existe para a placa que sai para a internet por IPv4; no IPv6
            // global o endereço da placa já é o que o mundo vê.
            if local.ip() == default
                && let Some(public) = public_address(socket)
                && public != *local
                && let Ok(candidate) = Candidate::server_reflexive(public, *local, "udp")
            {
                rtc.add_local_candidate(candidate);
            }
        }
        let answer = serde_json::to_vec(&rtc.sdp_api().accept_offer(offer)?)?;
        // Antes de a resposta sair daqui: quando quem assiste souber o nosso endereço, a passagem
        // de volta já está aberta nos roteadores deste lado.
        let quiet = Quiet::start(&sockets, remotes, quiet_hops());

        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, inbox) = channel();
        self.shared
            .subscribers
            .lock()
            .unwrap()
            .insert(id, sender.clone());

        // Uma thread por soquete só escuta a rede e acorda a conexão: nada fica girando à toa.
        for (local, socket) in &sockets {
            let reader = socket.try_clone()?;
            reader.set_read_timeout(Some(Duration::from_secs(1)))?;
            let (sender, local) = (sender.clone(), *local);
            std::thread::spawn(move || listen(&reader, local, &sender));
        }

        let shared = self.shared.clone();
        std::thread::spawn(move || {
            shared.viewers.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = viewer(rtc, &sockets, quiet, &shared, &inbox, id) {
                eprintln!("espectador {id}: {e}");
            }
            shared.subscribers.lock().unwrap().remove(&id);
            shared.listeners.lock().unwrap().remove(&id);
            shared.estimates.lock().unwrap().remove(&id);
            shared.losses.lock().unwrap().remove(&id);
            shared.viewers.fetch_sub(1, Ordering::Relaxed);
        });
        Ok((id, answer))
    }

    /// Encerra a conexão de um espectador.
    pub fn close(&self, viewer: u64) {
        self.shared.listeners.lock().unwrap().remove(&viewer);
        if let Some(sender) = self.shared.subscribers.lock().unwrap().remove(&viewer) {
            let _ = sender.send(Msg::Close);
        }
    }

    pub fn stats(&self) -> Stats {
        Stats {
            capturing: self
                .control
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| !c.is_finished())
                && !self.shared.ended.load(Ordering::Relaxed),
            viewers: self.shared.viewers.load(Ordering::Relaxed),
            encoder: self.shared.encoder.lock().unwrap().clone(),
            target_bitrate: self.shared.target.load(Ordering::Relaxed),
            produced_bitrate: self.shared.produced.load(Ordering::Relaxed),
            frames_encoded: self.shared.encoded.load(Ordering::Relaxed),
            audio: self
                .audio
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(nimbo_audio::Capture::is_running),
            audio_packets: self.shared.audio_packets.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Uma conexão de espectador: só repassa os quadros e o som que os codificadores únicos produziram.
/// Dorme até chegar um quadro, um pacote de som, um pacote da rede ou a hora que o WebRTC pediu.
///
/// Vídeo e som vão com o instante (`Instant`) em que foram capturados: é com ele que o WebRTC diz
/// ao navegador como alinhar os dois.
/// Escuta um soquete e entrega cada pacote à conexão do espectador, dizendo por qual endereço
/// local ele entrou. Só termina quando a conexão termina.
fn listen(reader: &UdpSocket, local: SocketAddr, sender: &Sender<Msg>) {
    // 64 KB: um pacote maior que o buffer daria erro no Windows em vez de ser só ignorado.
    let mut buf = vec![0u8; 65_536];
    loop {
        match reader.recv_from(&mut buf) {
            Ok((n, source)) => {
                if sender
                    .send(Msg::Packet(buf[..n].to_vec(), source, local))
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => {
                // ConnectionReset em UDP no Windows é só um aviso ICMP de porta fechada, e volta na
                // hora e sem parar: espera um pouco para não girar à toa. Qualquer outro erro de
                // leitura também não é motivo para ficar surdo. Depois acorda a conexão, para ela
                // notar se acabou.
                if !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if sender.send(Msg::Tick).is_err() {
                    return;
                }
            }
        }
    }
}

/// Quantos endereços locais oferecer, no máximo, de cada família (placas virtuais e endereços
/// IPv6 temporários às vezes vêm às dezenas). Um limite por família para uma não tirar a vez da outra.
const MAX_IPV4: usize = 6;
const MAX_IPV6: usize = 4;

/// Endereços locais por onde um espectador pode chegar: todas as placas de rede, IPv4 e IPv6.
/// É o que conecta duas pessoas atrás de operadoras com CGNAT (pelo IPv6) ou numa rede virtual de
/// jogo; só a rota padrão em IPv4 não basta para elas.
fn local_addresses() -> Vec<IpAddr> {
    let listed = if_addrs::get_if_addrs()
        .map(|list| list.into_iter().map(|interface| interface.ip()).collect())
        .unwrap_or_default();
    choose_addresses(listed, local_ip())
}

/// Dos endereços das placas, quais oferecer e em que ordem: a rota padrão primeiro, depois os
/// outros IPv4, depois IPv6.
fn choose_addresses(mut found: Vec<IpAddr>, default: IpAddr) -> Vec<IpAddr> {
    let usable = |ip: &IpAddr| match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        // fe80::/10 só vale dentro do mesmo enlace e exige indicar a placa: fica de fora.
        IpAddr::V6(v6) => {
            !v6.is_loopback() && !v6.is_unspecified() && (v6.segments()[0] & 0xffc0) != 0xfe80
        }
    };
    found.push(default);
    found.retain(usable);
    found.sort_by_key(|ip| (*ip != default, ip.is_ipv6(), *ip));
    found.dedup();
    let (mut v4, mut v6) = (0, 0);
    found.retain(|ip| {
        let count = if ip.is_ipv6() { &mut v6 } else { &mut v4 };
        *count += 1;
        *count <= if ip.is_ipv6() { MAX_IPV6 } else { MAX_IPV4 }
    });
    found
}

/// Abre um soquete em cada endereço local. Um endereço que não aceita (placa caindo) é pulado.
pub(crate) fn bind_all() -> Result<Vec<(SocketAddr, UdpSocket)>, AnyError> {
    let mut sockets = Vec::new();
    for ip in local_addresses() {
        if let Ok(socket) = UdpSocket::bind(SocketAddr::new(ip, 0))
            && let Ok(local) = socket.local_addr()
        {
            sockets.push((local, socket));
        }
    }
    if sockets.is_empty() {
        // Sem nenhuma placa listada: o jeito antigo, um soquete só na rota padrão.
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        let local = SocketAddr::new(local_ip(), socket.local_addr()?.port());
        sockets.push((local, socket));
    }
    Ok(sockets)
}

/// Quanto dura, no máximo, o silêncio inicial de um espectador.
const QUIET_MAX: Duration = Duration::from_millis(1500);

/// O prazo do silêncio. `NIMBO_QUIET_MS` troca o valor nos testes de rede (ver docs/rust-app.md).
fn quiet_max() -> Duration {
    static MAX: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("NIMBO_QUIET_MS")
            .ok()
            .and_then(|text| text.parse().ok())
            .map_or(QUIET_MAX, Duration::from_millis)
    })
}
/// Saltos de vida dos pacotes durante o silêncio quando não deu para medir os roteadores de casa:
/// atravessa até dois em série (medido: com 2 saltos a passagem não abre em casa com dois roteadores,
/// com 3 abre) e morre no primeiro roteador do provedor.
const QUIET_HOPS: u32 = 3;
/// Com mais que isto a medição está errada (ou a rede é estranha demais para confiar nela).
const MAX_QUIET_HOPS: u32 = 5;

/// Saltos medidos: roteadores de casa no caminho, mais um. Zero enquanto não mediu.
static MEASURED_HOPS: AtomicU32 = AtomicU32::new(0);

/// Vida dos pacotes do silêncio: o mínimo que sai de casa. Com um salto a mais que o necessário, o
/// pacote de quem tem um roteador só chega um salto mais longe e pode bater no roteador de quem
/// assiste, que é justamente o que o silêncio existe para evitar (0.5.3 usava 4 para todo mundo).
fn quiet_hops() -> u32 {
    match MEASURED_HOPS.load(Ordering::Relaxed) {
        0 => QUIET_HOPS,
        hops => hops,
    }
}

/// Mede uma vez, em segundo plano, quantos roteadores de casa há no caminho para a internet.
fn measure_hops() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| match home_routers() {
            Some(routers) => {
                MEASURED_HOPS.store((routers + 1).min(MAX_QUIET_HOPS), Ordering::Relaxed);
                eprintln!("[nimbo] roteadores de casa no caminho: {routers}");
            }
            None => eprintln!("[nimbo] roteadores de casa: não deu para medir"),
        });
    });
}

/// Pergunta ao Windows a rota até um endereço público e conta os roteadores de casa no começo dela.
fn home_routers() -> Option<u32> {
    use std::os::windows::process::CommandExt;
    // Sem janela de console piscando atrás do app.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("tracert")
        .args(["-d", "-h", "5", "-w", "300", "1.1.1.1"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    count_home_routers(&String::from_utf8_lossy(&output.stdout))
}

/// Na saída do tracert, quantos saltos seguidos, a partir do primeiro, têm endereço de rede de casa
/// (ou de CGNAT da operadora, que também é um NAT a atravessar). Para no primeiro que não tem ou
/// que não respondeu.
fn count_home_routers(trace: &str) -> Option<u32> {
    let mut count = 0;
    for line in trace.lines() {
        let mut words = line.split_whitespace();
        if words.next().and_then(|word| word.parse::<u32>().ok()) != Some(count + 1) {
            continue;
        }
        let home = words
            .next_back()
            .and_then(|word| word.parse::<Ipv4Addr>().ok())
            .is_some_and(|ip| {
                let [a, b, ..] = ip.octets();
                ip.is_private() || (a == 100 && (64..128).contains(&b))
            });
        if !home {
            break;
        }
        count += 1;
    }
    (count > 0).then_some(count)
}

/// Endereços UDP que quem assiste ofereceu, lidos da oferta: para onde abrir a passagem de volta.
fn remote_addresses(sdp: &str) -> Vec<SocketAddr> {
    let mut found = Vec::new();
    for line in sdp.lines() {
        let Some(candidate) = line.trim().strip_prefix("a=candidate:") else {
            continue;
        };
        let fields: Vec<&str> = candidate.split_whitespace().collect();
        // fundação, componente, transporte, prioridade, endereço, porta, "typ", tipo...
        if fields.len() < 6 || !fields[2].eq_ignore_ascii_case("udp") {
            continue;
        }
        // Nome ".local" (endereço escondido pelo navegador) não é endereço: fica de fora.
        if let (Ok(ip), Ok(port)) = (fields[4].parse::<IpAddr>(), fields[5].parse::<u16>())
            && port != 0
        {
            let address = SocketAddr::new(ip, port);
            if !found.contains(&address) {
                found.push(address);
            }
        }
    }
    found
}
/// Intervalo entre pacotes para o mesmo destino durante o silêncio: mantém a passagem aberta sem
/// encher a rede de pacotes que vão morrer no caminho (cada um pode gerar um aviso de erro de volta).
const QUIET_EVERY: Duration = Duration::from_millis(300);

/// Silêncio inicial de cada espectador.
///
/// Há roteador que, ao receber de fora um pacote que ninguém de dentro pediu, barra aquela conversa
/// de vez, mesmo depois de o lado de dentro responder. Medido em 15/11/2026 na rede do dono com um
/// espectador remoto de teste: com o outro lado mandando primeiro, a conexão falhou no app novo e no
/// antigo; com o outro lado calado até a máquina de dentro mandar, conectou nos dois. O motor respondia
/// à oferta e já saía testando os caminhos, então os pacotes dele sempre chegavam ao roteador de quem
/// assiste antes do primeiro pacote de quem assiste. No app antigo a ordem era a contrária.
///
/// Por isso, até o espectador mandar o primeiro pacote (ou `QUIET_MAX`), o que sai leva poucos saltos
/// de vida: atravessa o roteador de quem transmite, abrindo a passagem de volta, e morre antes de
/// chegar ao roteador de quem assiste. Na rede local não há roteador no caminho: o pacote chega, a
/// resposta vem e o silêncio acaba na hora.
///
/// A passagem é aberta já ao aceitar a oferta, com um pacote vazio para cada endereço de quem
/// assiste, sem esperar a vez de cada teste de caminho do WebRTC (um a cada 50 ms, e o endereço
/// público é dos últimos da fila): quem assiste pode mandar o primeiro pacote antes disso.
struct Quiet {
    active: bool,
    /// Endereços de quem assiste, para manter a passagem aberta enquanto durar o silêncio.
    remotes: Vec<SocketAddr>,
    opened: Instant,
    /// Saltos normais de cada soquete, para devolver no fim. `None`: não deu para limitar os saltos
    /// desse soquete, então ele fica calado durante o silêncio.
    normal: Vec<Option<u32>>,
    /// Último pacote de cada soquete para cada destino.
    sent: HashMap<(usize, SocketAddr), Instant>,
}

impl Quiet {
    fn start(sockets: &[(SocketAddr, UdpSocket)], remotes: Vec<SocketAddr>, life: u32) -> Self {
        let normal = sockets
            .iter()
            .map(|(_, socket)| {
                let before = hops(socket)?;
                set_hops(socket, life).then_some(before)
            })
            .collect();
        let quiet = Self {
            active: true,
            remotes,
            opened: Instant::now(),
            normal,
            sent: HashMap::new(),
        };
        quiet.open(sockets);
        quiet
    }

    /// Um pacote vazio de cada soquete para cada endereço de quem assiste, com a vida limitada: abre
    /// a passagem de volta nos roteadores deste lado. Quem recebe (na rede local) ignora.
    fn open(&self, sockets: &[(SocketAddr, UdpSocket)]) {
        for ((local, socket), normal) in sockets.iter().zip(&self.normal) {
            if normal.is_none() {
                continue;
            }
            for remote in &self.remotes {
                if remote.is_ipv4() == local.is_ipv4() {
                    let _ = socket.send_to(&[0], remote);
                }
            }
        }
    }

    /// Repete a abertura de tempos em tempos enquanto o silêncio durar.
    fn keep_open(&mut self, sockets: &[(SocketAddr, UdpSocket)], now: Instant) {
        if self.active && now.saturating_duration_since(self.opened) >= QUIET_EVERY {
            self.opened = now;
            self.open(sockets);
        }
    }

    /// Este pacote pode sair agora? Durante o silêncio, só de tempos em tempos para cada destino, e
    /// só pelos soquetes com os saltos limitados.
    fn allows(&mut self, socket: usize, destination: SocketAddr, now: Instant) -> bool {
        if !self.active {
            return true;
        }
        if self.normal.get(socket).copied().flatten().is_none() {
            return false;
        }
        match self.sent.get(&(socket, destination)) {
            Some(last) if now.saturating_duration_since(*last) < QUIET_EVERY => false,
            _ => {
                self.sent.insert((socket, destination), now);
                true
            }
        }
    }

    /// Fim do silêncio: o espectador mandou o primeiro pacote, ou o prazo acabou.
    fn end(&mut self, sockets: &[(SocketAddr, UdpSocket)]) {
        if !self.active {
            return;
        }
        self.active = false;
        self.sent = HashMap::new();
        for ((_, socket), normal) in sockets.iter().zip(&self.normal) {
            if let Some(normal) = normal {
                set_hops(socket, *normal);
            }
        }
    }
}

/// Saltos de vida (TTL no IPv4, "hop limit" no IPv6) dos pacotes que saem deste soquete.
fn hops(socket: &UdpSocket) -> Option<u32> {
    let handle = socket2::SockRef::from(socket);
    match socket.local_addr().ok()? {
        SocketAddr::V4(_) => handle.ttl_v4().ok(),
        SocketAddr::V6(_) => handle.unicast_hops_v6().ok(),
    }
}

fn set_hops(socket: &UdpSocket, hops: u32) -> bool {
    let handle = socket2::SockRef::from(socket);
    match socket.local_addr() {
        Ok(SocketAddr::V4(_)) => handle.set_ttl_v4(hops).is_ok(),
        Ok(SocketAddr::V6(_)) => handle.set_unicast_hops_v6(hops).is_ok(),
        Err(_) => false,
    }
}

/// Com `NIMBO_TRACE=1`, escreve no stderr o que acontece com cada espectador (testes de rede).
fn trace(id: u64, start: Instant, what: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("NIMBO_TRACE").is_some()) {
        eprintln!(
            "[espectador {id}] +{} ms {what}",
            start.elapsed().as_millis()
        );
    }
}

fn viewer(
    mut rtc: Rtc,
    sockets: &[(SocketAddr, UdpSocket)],
    mut quiet: Quiet,
    shared: &Shared,
    inbox: &Receiver<Msg>,
    id: u64,
) -> Result<(), AnyError> {
    let start = Instant::now();
    let mut video: Option<Mid> = None;
    let mut sound: Option<Mid> = None;
    let mut connected = false;
    // Desde quando a rede deste espectador sumiu (None = está respondendo).
    let mut lost: Option<Instant> = None;
    // Total de bytes enviados na medição anterior, para saber a taxa entre duas medições.
    let mut sent_before: Option<(u64, Instant)> = None;
    loop {
        if quiet.active && start.elapsed() >= quiet_max() {
            quiet.end(sockets);
        }
        quiet.keep_open(sockets, Instant::now());
        let timeout = match rtc.poll_output()? {
            Output::Timeout(at) => at,
            Output::Transmit(packet) => {
                // Falha de envio (rede do computador piscou) não encerra: o WebRTC reenvia o que importa.
                // Sai pelo mesmo endereço local que o WebRTC escolheu para este caminho.
                let index = sockets
                    .iter()
                    .position(|(local, _)| *local == packet.source)
                    .unwrap_or(0);
                if let Some((_, socket)) = sockets.get(index)
                    && quiet.allows(index, packet.destination, Instant::now())
                {
                    let _ = socket.send_to(&packet.contents, packet.destination);
                }
                continue;
            }
            Output::Event(event) => {
                match event {
                    // Rede que oscila costuma voltar em segundos: só desiste depois de LOST.
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        trace(id, start, "ice: Disconnected");
                        lost.get_or_insert_with(Instant::now);
                    }
                    Event::IceConnectionStateChange(state) => {
                        trace(id, start, &format!("ice: {state:?}"));
                        lost = None;
                    }
                    Event::Connected => {
                        trace(id, start, "conectado");
                        connected = true;
                        shared.keyframe.store(true, Ordering::Relaxed);
                        let target = u64::from(shared.target.load(Ordering::Relaxed));
                        let ceiling = u64::from(shared.quality.lock().unwrap().bitrate);
                        // Primeiro valor = quanto preencher com pacotes vazios quando o vídeo gasta menos.
                        // Zero: sem isso cada espectador recebia até o alvo inteiro em enchimento, mesmo
                        // com a tela parada (medido: 1 Mbps de vídeo virava 7 Mbps na rede).
                        let _ = target;
                        rtc.bwe()
                            .set_bitrate(Bitrate::ZERO, Bitrate::bps(ceiling * 3 / 2));
                    }
                    Event::MediaAdded(media) if media.kind == MediaKind::Video => {
                        video = Some(media.mid)
                    }
                    // Só quem pediu para receber áudio entra na lista do Opus; os outros seguem
                    // só com o vídeo.
                    Event::MediaAdded(media)
                        if media.kind == MediaKind::Audio && media.direction.is_sending() =>
                    {
                        sound = Some(media.mid);
                        let sender = shared.subscribers.lock().unwrap().get(&id).cloned();
                        if let Some(sender) = sender {
                            shared.listeners.lock().unwrap().insert(id, sender);
                        }
                    }
                    Event::KeyframeRequest(_) => shared.keyframe.store(true, Ordering::Relaxed),
                    // Quanto esta conexão aguenta; o codificador único segue a mais apertada.
                    Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                        let mut estimates = shared.estimates.lock().unwrap();
                        let seen = estimates.get(&id).map_or_else(Instant::now, |e| e.1);
                        estimates.insert(id, (estimate.as_u64(), seen));
                    }
                    Event::PeerStats(stats) => {
                        // Bytes enviados a este espectador desde a medição anterior, com reenvios.
                        let sent = sent_before
                            .replace((stats.peer_bytes_tx, stats.timestamp))
                            .and_then(|(bytes, at)| {
                                let seconds =
                                    stats.timestamp.saturating_duration_since(at).as_secs_f64();
                                (seconds > 0.2).then(|| {
                                    (stats.peer_bytes_tx.saturating_sub(bytes) as f64 * 8.0
                                        / seconds) as u64
                                })
                            });
                        if let (Some(loss), Some(sent)) = (stats.egress_loss_fraction, sent) {
                            if loss >= 0.05 {
                                trace(
                                    id,
                                    start,
                                    &format!(
                                        "perda {:.0}% com {:.1} Mbps enviados",
                                        loss * 100.0,
                                        sent as f64 / 1e6
                                    ),
                                );
                            }
                            shared
                                .losses
                                .lock()
                                .unwrap()
                                .insert(id, (loss, sent, Instant::now()));
                        }
                    }
                    _ => {}
                }
                continue;
            }
        };
        // Quem não conecta em 15 s (página fechada, rede bloqueada) libera a vaga.
        if !rtc.is_alive()
            || (!connected && start.elapsed() > Duration::from_secs(15))
            || lost.is_some_and(|since| since.elapsed() > LOST)
        {
            trace(
                id,
                start,
                &format!(
                    "encerrado: vivo={} conectado={connected} semRede={:?}",
                    rtc.is_alive(),
                    lost.map(|since| since.elapsed())
                ),
            );
            return Ok(());
        }
        // O controle de banda dita o ritmo de envio pacote a pacote; atrasar muito esses avisos faz a
        // banda estimada despencar (testado com janelas de 4 ms). Mas espera menor que 1 ms não dorme de
        // verdade no Windows: vira um laço girando, com rajadas de 30% de um núcleo. O piso de 1 ms
        // mantém o ritmo (o Chrome agrupa em 5 ms) sem girar.
        let mut wait = timeout.saturating_duration_since(Instant::now());
        if quiet.active {
            // Acorda no fim do silêncio mesmo que nada chegue, e a tempo de repetir a abertura.
            wait = wait
                .min(quiet_max().saturating_sub(start.elapsed()))
                .min(QUIET_EVERY);
        }
        let message = if wait.is_zero() {
            None
        } else {
            inbox.recv_timeout(wait.max(Duration::from_millis(1))).ok()
        };
        match message {
            Some(Msg::Frame(frame)) => {
                // Antes de conectar não há para onde mandar; ao conectar, pede-se um quadro completo.
                if let (true, Some(mid)) = (connected, video)
                    && let Some(writer) = rtc.writer(mid)
                {
                    // O codificador gera perfil High: prefere o formato que anuncia isso (0x64), depois
                    // Main (0x4d), e só então o primeiro que houver. Modo de empacotamento 0 fica por último.
                    let pt = writer
                        .payload_params()
                        .filter(|p| p.spec().codec == Codec::H264)
                        .min_by_key(|p| {
                            let format = p.spec().format;
                            let profile = match format.profile_level_id.map(|id| id >> 16) {
                                Some(0x64) => 0,
                                Some(0x4d) => 1,
                                _ => 2,
                            };
                            (format.packetization_mode == Some(0), profile)
                        })
                        .map(|p| p.pt());
                    if let Some(pt) = pt {
                        let micros = frame.at.saturating_duration_since(start).as_micros() as u64;
                        writer.write(
                            pt,
                            frame.at,
                            MediaTime::from_90khz(micros * 9 / 100),
                            frame.data.clone(),
                        )?;
                    }
                }
            }
            Some(Msg::Audio(packet)) => {
                if let (true, Some(mid)) = (connected, sound)
                    && let Some(writer) = rtc.writer(mid)
                {
                    let pt = writer
                        .payload_params()
                        .find(|p| p.spec().codec == Codec::Opus)
                        .map(|p| p.pt());
                    if let Some(pt) = pt {
                        writer.write(
                            pt,
                            packet.at,
                            MediaTime::new(packet.rtp, Frequency::FORTY_EIGHT_KHZ),
                            packet.data.clone(),
                        )?;
                    }
                }
            }
            Some(Msg::Packet(data, source, local)) => {
                // O espectador já mandou: o roteador dele espera a resposta, pode falar normalmente.
                quiet.end(sockets);
                if let Some(entry) = shared.estimates.lock().unwrap().get_mut(&id) {
                    entry.1 = Instant::now();
                }
                // Qualquer um que alcance a porta pode mandar lixo: pacote que não é do WebRTC é
                // ignorado, não derruba o espectador.
                let Ok(contents) = data.as_slice().try_into() else {
                    continue;
                };
                let receive = Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: local,
                    contents,
                };
                if rtc
                    .handle_input(Input::Receive(Instant::now(), receive))
                    .is_err()
                {
                    continue;
                }
            }
            Some(Msg::Close) => return Ok(()),
            Some(Msg::Tick) | None => rtc.handle_input(Input::Timeout(Instant::now()))?,
        }
    }
}

#[cfg(test)]
mod tests {
    use str0m::media::Direction;

    use super::*;

    /// Quantos quadros por segundo passam pelo teto com uma fonte regular de `source` quadros por segundo.
    fn paced(source: f64, cap: u32) -> f64 {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        let seconds = 10.0;
        let mut accepted = 0u32;
        let mut frame = 0u32;
        loop {
            let at = f64::from(frame) / source;
            if at >= seconds {
                break;
            }
            if pacer.accept(start + Duration::from_secs_f64(at), cap) {
                accepted += 1;
            }
            frame += 1;
        }
        f64::from(accepted) / seconds
    }

    #[test]
    fn pacer_keeps_the_cap_whatever_the_source_rate() {
        // Antes, 75 Hz dava 37,5 e 144 Hz dava 48 com teto de 60.
        for source in [72.0, 75.0, 90.0, 120.0, 144.0, 165.0, 240.0] {
            let rate = paced(source, 60);
            assert!((59.0..=61.0).contains(&rate), "fonte {source}: {rate}");
            let rate = paced(source, 30);
            assert!((29.5..=30.5).contains(&rate), "fonte {source} a 30: {rate}");
        }
    }

    #[test]
    fn pacer_passes_every_frame_of_a_source_at_or_below_the_cap() {
        for source in [24.0, 30.0, 59.94, 60.0] {
            let rate = paced(source, 60);
            assert!((rate - source).abs() < 0.2, "fonte {source}: {rate}");
        }
        assert_eq!(paced(144.0, u32::MAX), 144.0);
    }

    #[test]
    fn pacer_does_not_burst_after_a_pause() {
        let start = Instant::now();
        let mut pacer = Pacer::new(start);
        assert!(pacer.accept(start, 60));
        // Tela parada por 5 s; depois, quadros a cada 4 ms. Em 100 ms passam no máximo 8 (seis no
        // ritmo e a folga de um intervalo), não os 25 que chegaram.
        let resume = start + Duration::from_secs(5);
        let passed = (0..25)
            .filter(|i| pacer.accept(resume + Duration::from_millis(4 * i), 60))
            .count();
        assert!((6..=8).contains(&passed), "{passed}");
    }

    #[test]
    fn offers_every_network_card_with_the_default_route_first() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let default = ip("192.168.0.10");
        let listed = vec![
            ip("127.0.0.1"),
            ip("fe80::1"),
            ip("fd00::5"),
            ip("26.10.20.30"),
            ip("169.254.3.4"),
            ip("192.168.0.10"),
            ip("::1"),
            ip("2804:14c::9"),
            ip("26.10.20.30"),
        ];
        assert_eq!(
            choose_addresses(listed, default),
            vec![
                ip("192.168.0.10"),
                ip("26.10.20.30"),
                ip("2804:14c::9"),
                ip("fd00::5")
            ]
        );
        // Sem lista de placas, sobra a rota padrão; e só de volta local não se oferece nada.
        assert_eq!(choose_addresses(Vec::new(), default), vec![default]);
        assert!(choose_addresses(Vec::new(), ip("127.0.0.1")).is_empty());
    }

    #[test]
    fn quiet_start_lets_only_the_first_packets_out_until_the_viewer_speaks() {
        // No endereço da placa, e não em 127.0.0.1: há máquina com filtro de rede que não entrega UDP
        // entre dois soquetes de loopback (visto na do dono), e a live de verdade usa o da placa.
        let socket = UdpSocket::bind((local_ip(), 0)).unwrap();
        let local = socket.local_addr().unwrap();
        let before = hops(&socket).unwrap();
        let sockets = [(local, socket)];
        let (near, far) = (
            "10.0.0.2:5000".parse().unwrap(),
            "10.0.0.3:5000".parse().unwrap(),
        );

        // Quem assiste, de mentira, na mesma máquina: tem de receber o pacote de abertura.
        let viewer = UdpSocket::bind((local_ip(), 0)).unwrap();
        viewer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let remotes = vec![viewer.local_addr().unwrap(), "[::1]:9".parse().unwrap()];
        let mut quiet = Quiet::start(&sockets, remotes, 2);
        assert_eq!(hops(&sockets[0].1), Some(2));
        let mut buf = [0u8; 8];
        let (n, from) = viewer.recv_from(&mut buf).unwrap();
        assert_eq!((n, from), (1, sockets[0].0));
        let now = Instant::now();
        assert!(quiet.allows(0, near, now));
        assert!(!quiet.allows(0, near, now + QUIET_EVERY / 2));
        // Passado o intervalo, mais um para manter a passagem aberta.
        assert!(quiet.allows(0, near, now + QUIET_EVERY));
        // Cada destino tem a sua vez; soquete desconhecido fica calado.
        assert!(quiet.allows(0, far, now));
        assert!(!quiet.allows(7, far, now));

        quiet.end(&sockets);
        assert_eq!(hops(&sockets[0].1), Some(before));
        assert!(quiet.allows(0, near, now));
        assert!(quiet.allows(7, far, now));
        // Terminar de novo não muda nada.
        quiet.end(&sockets);
        assert_eq!(hops(&sockets[0].1), Some(before));
    }

    #[test]
    fn counts_the_home_routers_at_the_start_of_the_route() {
        // Dois roteadores em série (o da pessoa e o da operadora), depois um salto que não responde.
        let two = "\r\nRastreando a rota para 1.1.1.1 com no máximo 5 saltos\r\n\r\n  1    <1 ms    <1 ms    <1 ms  192.168.0.1 \r\n  2    <1 ms    <1 ms    <1 ms  192.168.15.1 \r\n  3     *        *        *     Esgotado o tempo limite do pedido.\r\n  4     1 ms     2 ms     2 ms  200.1.2.3 \r\n";
        assert_eq!(count_home_routers(two), Some(2));
        // Um roteador só, depois a operadora.
        let one = "Tracing route to 1.1.1.1 over a maximum of 5 hops\n\n  1     1 ms     1 ms     1 ms  192.168.1.1\n  2     3 ms     3 ms     3 ms  187.10.20.30\n";
        assert_eq!(count_home_routers(one), Some(1));
        // CGNAT da operadora também é um NAT a atravessar.
        let cgnat = "  1     1 ms     1 ms     1 ms  10.0.0.1\n  2     3 ms     3 ms     3 ms  100.64.0.1\n  3     5 ms     5 ms     5 ms  8.8.8.8\n";
        assert_eq!(count_home_routers(cgnat), Some(2));
        // Sem resposta do primeiro salto, ou saída que não é do tracert: não chuta.
        assert_eq!(
            count_home_routers("  1     *        *        *     Request timed out.\n"),
            None
        );
        assert_eq!(count_home_routers("comando não encontrado"), None);
    }

    #[test]
    fn reads_the_viewer_addresses_from_the_offer() {
        let sdp = "v=0\r\na=candidate:1 1 udp 2122260223 192.168.0.5 50000 typ host\r\n\
                   a=candidate:2 1 UDP 1686052607 200.1.2.3 50000 typ srflx raddr 192.168.0.5 rport 50000\r\n\
                   a=candidate:3 1 tcp 1518280447 192.168.0.5 9 typ host tcptype active\r\n\
                   a=candidate:4 1 udp 2122260223 abc123.local 50001 typ host\r\n\
                   a=candidate:5 1 udp 2122260223 fd00::5 50002 typ host\r\n\
                   a=candidate:1 1 udp 2122260223 192.168.0.5 50000 typ host\r\n";
        let expected: Vec<SocketAddr> = ["192.168.0.5:50000", "200.1.2.3:50000", "[fd00::5]:50002"]
            .iter()
            .map(|text| text.parse().unwrap())
            .collect();
        assert_eq!(remote_addresses(sdp), expected);
    }

    #[test]
    fn the_quality_ladder_steps_down_fast_and_up_slowly() {
        let last = LADDER.len() - 1;
        // Uma medição apertada só não muda nada; duas seguidas descem.
        assert_eq!(
            next_step(0, 1, false, STEP_DOWN_AFTER - 1, 0, STEP_UP_AFTER),
            0
        );
        assert_eq!(next_step(0, 1, false, STEP_DOWN_AFTER, 0, STEP_UP_AFTER), 1);
        // Desce direto para o degrau que a taxa comporta, sem parar nos do meio.
        assert_eq!(next_step(0, 3, false, STEP_DOWN_AFTER, 0, STEP_UP_AFTER), 3);
        // Só o codificador não acompanha (a taxa em si caberia aqui): um degrau.
        assert_eq!(next_step(1, 1, true, STEP_DOWN_AFTER, 0, STEP_UP_AFTER), 2);
        // Do último degrau não há para onde descer.
        assert_eq!(next_step(last, last, true, u8::MAX, 0, STEP_UP_AFTER), last);
        // Para subir, a folga tem de durar o que a espera atual pede, e um degrau por vez.
        assert_eq!(
            next_step(3, 0, false, 0, STEP_UP_AFTER - 1, STEP_UP_AFTER),
            3
        );
        assert_eq!(next_step(3, 0, false, 0, STEP_UP_AFTER, STEP_UP_AFTER), 2);
        assert_eq!(
            next_step(3, 0, false, 0, STEP_UP_AFTER, STEP_UP_AFTER * 2),
            3
        );
        assert_eq!(next_step(0, 0, false, 0, u8::MAX, STEP_UP_AFTER), 0);
        // Não sobe enquanto o codificador estoura a taxa.
        assert_eq!(next_step(2, 0, true, 0, u8::MAX, STEP_UP_AFTER), 2);
        // A taxa escolhe o degrau pelo piso; os pisos descem e o último aceita qualquer taxa.
        assert_eq!(fitting_step(8_000_000), 0);
        assert_eq!(fitting_step(3_000_000), 1);
        assert_eq!(fitting_step(2_000_000), 2);
        assert_eq!(fitting_step(1_200_000), 3);
        assert_eq!(fitting_step(800_000), last);
        assert!(LADDER.windows(2).all(|pair| pair[0].floor > pair[1].floor));
        assert_eq!(LADDER[last].floor, 0);
    }

    #[test]
    fn heavy_loss_caps_the_rate_below_what_actually_arrived() {
        // 6,5 Mbps enviados com 55% de perda: chegam 2,9 Mbps; o teto fica 15% abaixo.
        let limit = delivered_limit(6_500_000, 0.55);
        assert!((2_400_000..2_550_000).contains(&limit), "{limit}");
        // Nunca abaixo do piso do controle de banda, nem com perda total ou valor absurdo.
        assert_eq!(delivered_limit(6_500_000, 1.0), MIN_BITRATE);
        assert_eq!(delivered_limit(500_000, 0.5), MIN_BITRATE);
        assert_eq!(delivered_limit(6_500_000, 7.0), MIN_BITRATE);
        assert_eq!(delivered_limit(4_000_000, -1.0), 3_400_000);
    }

    #[test]
    fn many_addresses_of_one_family_do_not_crowd_out_the_other() {
        let mut listed: Vec<IpAddr> = (1..=20u8).map(|n| IpAddr::from([10, 0, 0, n])).collect();
        listed.extend((1..=20u16).map(|n| IpAddr::from([0x2804, 0, 0, 0, 0, 0, 0, n])));
        let chosen = choose_addresses(listed, IpAddr::from([10, 0, 0, 7]));
        assert_eq!(chosen[0], IpAddr::from([10, 0, 0, 7]));
        assert_eq!(chosen.iter().filter(|ip| ip.is_ipv4()).count(), MAX_IPV4);
        assert_eq!(chosen.iter().filter(|ip| ip.is_ipv6()).count(), MAX_IPV6);
    }

    #[test]
    fn same_stream_gives_every_media_the_same_stream_id() {
        let offer = "v=0\r\na=msid-semantic: WMS\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\n\
                     a=recvonly\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\na=msid:- x\r\n\
                     m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:2\r\n";
        assert_eq!(
            same_stream(offer),
            "v=0\r\na=msid-semantic: WMS\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\n\
             a=msid:nimbo nimbo-0\r\na=recvonly\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\n\
             a=msid:nimbo nimbo-1\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:2\r\n"
        );
    }

    /// Oferta de quem só assiste (vídeo e som), feita pela própria biblioteca de WebRTC.
    fn viewer_offer(audio: bool) -> SdpOffer {
        let mut viewer = Rtc::new(Instant::now());
        let mut change = viewer.sdp_api();
        change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        if audio {
            change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
        }
        change.apply().unwrap().0
    }

    fn answer_to(offer: &SdpOffer) -> String {
        // Instala a criptografia do processo, que o WebRTC exige.
        drop(Engine::new());
        let offer = SdpOffer::from_sdp_string(&same_stream(&offer.to_sdp_string())).unwrap();
        let mut rtc = Rtc::new(Instant::now());
        rtc.sdp_api().accept_offer(offer).unwrap().to_sdp_string()
    }

    #[test]
    fn answer_sends_video_and_opus_in_one_stream() {
        let answer = answer_to(&viewer_offer(true));
        let sections: Vec<&str> = answer.split("\r\nm=").skip(1).collect();
        assert_eq!(sections.len(), 2, "{answer}");
        for section in &sections {
            assert!(section.contains("a=sendonly"), "{answer}");
            assert!(section.contains("a=msid:nimbo nimbo-"), "{answer}");
        }
        assert!(sections[0].starts_with("video"), "{answer}");
        assert!(sections[1].starts_with("audio"), "{answer}");
        assert!(sections[1].contains("opus/48000/2"), "{answer}");
    }

    #[test]
    fn video_only_offer_gets_a_video_only_answer() {
        let answer = answer_to(&viewer_offer(false));
        assert_eq!(answer.matches("\r\nm=").count(), 1, "{answer}");
        assert!(answer.contains("\r\nm=video"), "{answer}");
    }
}
