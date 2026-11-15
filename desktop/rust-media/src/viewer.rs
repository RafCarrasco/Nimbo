//! Player: assiste a uma live. É o lado de quem recebe, em Rust, sem navegador.
//!
//! - **Rede**: uma conexão WebRTC só de recepção (str0m). Quem assiste faz a oferta, com um soquete
//!   por placa de rede, igual ao que o motor faz ao aceitar (`Engine::accept`). A sinalização fica
//!   fora daqui: `offer()` entrega o JSON `{type, sdp}` que vai para quem transmite e `answer()`
//!   recebe o JSON de volta. Como no navegador, os testes de caminho só começam depois da resposta,
//!   e é este lado que manda o primeiro pacote.
//! - **Vídeo**: H.264 decodificado pela placa (`decode.rs`), com reserva em software. O quadro
//!   pronto fica disponível em NV12 na memória (`Frame::data`) e, quando veio da placa, também
//!   como textura Direct3D 11 (`Frame::gpu`).
//! - **Som**: Opus → PCM → caixa de som padrão (`playback.rs`), com volume.
//!
//! Não há fila de suavização no vídeo: cada quadro é entregue assim que fica pronto. Um quadro que
//! não pode ser decodificado (perda na rede, decodificador reiniciado) faz o player pedir um
//! quadro completo a quem transmite e descartar o que chegar até lá.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, KeyframeRequestKind, MediaData, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::AnyError;
pub use crate::decode::GpuFrame;
use crate::decode::{self, Decoder, Pool};
use crate::engine::{bind_all, install_crypto, local_ip, public_address};
use crate::playback::{self, Speaker};

/// Sem resposta de quem transmite por este tempo, a tentativa acaba.
const ANSWER_WAIT: Duration = Duration::from_secs(30);
/// Com a resposta em mãos, quanto esperar a conexão fechar (o site usa o mesmo prazo).
const CONNECT_WAIT: Duration = Duration::from_secs(15);
/// Rede que some por este tempo encerra a conexão (quem usa o player abre outra).
const LOST: Duration = Duration::from_secs(10);
/// Sem nenhum pacote de quem transmite por este tempo, a conexão passa a "sem resposta". Uma live
/// viva nunca fica tanto tempo calada: mesmo pausada ou com a tela parada, o motor manda imagem e
/// relatórios várias vezes por segundo. Medido: o aviso do próprio WebRTC só vinha depois de ~20 s.
const SILENT: Duration = Duration::from_secs(3);
/// Intervalo mínimo entre dois pedidos de quadro completo.
const KEYFRAME_GAP: Duration = Duration::from_millis(300);
/// Quadros esperando o decodificador acima disto: ele ficou para trás, pula para o próximo completo.
const BACKLOG: usize = 30;
/// Quadros entregues ao decodificador sem sair nenhum: ele travou, é recriado.
const STARVED: u32 = 30;
/// Pacotes que o WebRTC segura para remontar um quadro. Um quadro completo em 1080p passa de 200
/// pacotes; o padrão da biblioteca (30) é pequeno para isso.
const REORDER: usize = 512;
/// Maior pacote Opus decodificado: 120 ms.
const OPUS_FRAMES: usize = playback::SAMPLE_RATE as usize * 120 / 1000;
/// De quanto em quanto tempo cada thread anota o tempo de CPU que já gastou.
const CPU_EVERY: Duration = Duration::from_millis(250);

/// O que pedir e como decodificar.
#[derive(Clone, Debug)]
pub struct Options {
    /// Pede o som da live na oferta (só faça isso quando o anúncio da live tiver `sound`).
    pub audio: bool,
    /// Volume do som, de 0 a 1.
    pub volume: f32,
    /// Força a decodificação por software (testes da reserva).
    pub software: bool,
    /// Copia cada quadro para a memória (`Frame::data`). Quem desenha direto da textura da placa
    /// (`Frame::gpu`) desliga para poupar a cópia.
    pub memory: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            audio: true,
            volume: 1.0,
            software: false,
            memory: true,
        }
    }
}

/// Um quadro pronto para desenhar.
pub struct Frame {
    /// Número de ordem, crescente a cada quadro decodificado (começa em 1).
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    /// Bytes por linha de `data`, nos dois planos. Hoje é sempre igual a `width`.
    pub stride: usize,
    /// NV12: `stride * height` bytes de luz (Y) e, em seguida, `stride * height / 2` bytes de cor
    /// (U e V intercalados, metade da resolução nos dois sentidos). Vazio com `Options::memory`
    /// desligado.
    ///
    /// Cor: a live do motor chega em BT.709 de faixa completa (medido em 15/11/2026 com faixas de cor
    /// conhecida). Para desenhar: `R = Y + 1,5748 (V - 128)`, `G = Y - 0,1873 (U - 128) - 0,4681 (V -
    /// 128)`, `B = Y + 1,8556 (U - 128)`, sem o ajuste de 16 a 235 da TV.
    pub data: Vec<u8>,
    /// O mesmo quadro ainda na placa de vídeo, quando foi ela que decodificou.
    pub gpu: Option<GpuFrame>,
    /// Quando chegou o primeiro pacote deste quadro.
    pub received: Instant,
    /// Quando o quadro ficou pronto.
    pub decoded: Instant,
    pool: Pool,
}

impl Frame {
    /// Plano de luz: `height` linhas de `stride` bytes.
    pub fn y(&self) -> &[u8] {
        let end = (self.stride * self.height as usize).min(self.data.len());
        &self.data[..end]
    }

    /// Plano de cor: `height / 2` linhas de `stride` bytes, U e V intercalados.
    pub fn uv(&self) -> &[u8] {
        let start = (self.stride * self.height as usize).min(self.data.len());
        &self.data[start..]
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        decode::give_back(&self.pool, std::mem::take(&mut self.data));
    }
}

/// Em que pé está a conexão.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Esperando a resposta de quem transmite ou testando os caminhos de rede.
    Connecting,
    Connected,
    /// A rede parou de responder; volta sozinha ou encerra depois de alguns segundos.
    Unstable,
    /// Acabou (fechada por quem usa, não conectou no prazo ou a rede sumiu). Para assistir de novo,
    /// crie outro `Viewer`.
    Closed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "conectando",
            Self::Connected => "conectado",
            Self::Unstable => "sem resposta",
            Self::Closed => "encerrado",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Connecting,
            1 => Self::Connected,
            2 => Self::Unstable,
            _ => Self::Closed,
        }
    }
}

/// Números da conexão. Os contadores são totais desde o começo; quem mostra calcula a diferença.
#[derive(Clone, Debug)]
pub struct Stats {
    pub state: State,
    /// Caminho escolhido, só com o tipo de cada ponta: `host:ipv4-privado -> host:ipv4-privado`.
    /// Vazio antes de conectar.
    pub route: String,
    /// Resolução do último quadro decodificado.
    pub width: u32,
    pub height: u32,
    /// O decodificador em uso; vazio antes do primeiro quadro.
    pub decoder: String,
    /// Quadros completos que a rede entregou.
    pub frames_received: u64,
    pub frames_decoded: u64,
    /// Quadros jogados fora: esperando um quadro completo, erro ou fila do decodificador.
    pub frames_dropped: u64,
    /// Tempo gasto decodificando (e copiando para a memória), somado.
    pub decode_time: Duration,
    /// Do primeiro pacote de cada quadro até ele ficar pronto, somado.
    pub latency: Duration,
    /// Quadros na fila do decodificador agora.
    pub backlog: usize,
    /// Pedidos de quadro completo feitos a quem transmite.
    pub keyframes_requested: u64,
    /// Pacotes de vídeo que faltaram mesmo depois dos reenvios.
    pub packets_missing: u64,
    /// Pedidos de reenvio de pacote.
    pub nacks: u64,
    /// Perda na chegada no último segundo, de 0 a 1.
    pub loss: f32,
    /// Bytes recebidos pela conexão.
    pub bytes_received: u64,
    /// Taxa de chegada no último segundo, em bits por segundo.
    pub bitrate: u32,
    /// Ida e volta até quem transmite.
    pub rtt: Option<Duration>,
    pub audio_packets: u64,
    /// Quadros de som (uma amostra de cada canal, 48 kHz) decodificados.
    pub audio_frames: u64,
    /// Quadros de som entregues à caixa de som.
    pub audio_played: u64,
    /// Vezes em que o som ficou sem reserva e teve de juntar de novo.
    pub audio_underruns: u64,
    /// A caixa de som abriu.
    pub audio_output: bool,
    /// Há quanto tempo saiu o último quadro.
    pub since_last_frame: Option<Duration>,
    /// Tempo de CPU gasto pela thread da rede (WebRTC e decodificação do som).
    pub cpu_network: Duration,
    /// Tempo de CPU gasto pela thread do decodificador de vídeo (inclui a cópia para a memória).
    pub cpu_decoder: Duration,
    /// Tempo de CPU gasto pela thread que entrega o som à placa.
    pub cpu_audio: Duration,
}

/// Dispositivo Direct3D do decodificador, guardado para quem desenha sem cópia.
struct SharedDevice(ID3D11Device);
// SAFETY: o dispositivo é criado com a proteção multithread ligada (decode.rs) e o próprio
// ID3D11Device é de uso livre entre threads; só o contexto imediato não é, e ele não sai daqui.
unsafe impl Send for SharedDevice {}

struct Shared {
    state: AtomicU8,
    latest: Mutex<Option<Arc<Frame>>>,
    fresh: Condvar,
    serial: AtomicU64,
    route: Mutex<String>,
    decoder: Mutex<String>,
    device: Mutex<Option<SharedDevice>>,
    width: AtomicU32,
    height: AtomicU32,
    frames_received: AtomicU64,
    frames_decoded: AtomicU64,
    frames_dropped: AtomicU64,
    decode_micros: AtomicU64,
    latency_micros: AtomicU64,
    backlog: AtomicUsize,
    keyframes_requested: AtomicU64,
    packets_missing: AtomicU64,
    nacks: AtomicU64,
    loss: AtomicU32,
    bytes_received: AtomicU64,
    bitrate: AtomicU32,
    /// Ida e volta em microssegundos; `u64::MAX` = ainda não medido.
    rtt_micros: AtomicU64,
    audio_packets: AtomicU64,
    audio_frames: AtomicU64,
    last_frame: Mutex<Option<Instant>>,
    cpu_network: AtomicU64,
    cpu_decoder: AtomicU64,
    speaker: Option<Speaker>,
}

impl Shared {
    fn set_state(&self, state: State) {
        self.state.store(state as u8, Ordering::Relaxed);
        if state == State::Closed {
            // Acorda quem espera um quadro que não vem mais.
            self.fresh.notify_all();
        }
    }
}

enum Msg {
    /// Pacote da rede: conteúdo, de onde veio e por qual endereço local entrou.
    Packet(Vec<u8>, SocketAddr, SocketAddr),
    Tick,
    /// Resposta de quem transmite, os caminhos que ela traz e por onde devolver o resultado.
    Answer(
        Box<SdpAnswer>,
        Vec<(SocketAddr, &'static str)>,
        SyncSender<Result<(), String>>,
    ),
    Candidate(Candidate, &'static str),
    /// O decodificador precisa de um quadro completo.
    Keyframe,
    Close,
}

/// Um quadro H.264 inteiro a caminho do decodificador.
struct Unit {
    data: Arc<[u8]>,
    /// Veio em sequência do anterior? Se não, falta um pedaço da live antes dele.
    contiguous: bool,
    received: Instant,
}

/// Uma conexão que assiste a uma live. Fechar é soltar o valor (ou `close`).
pub struct Viewer {
    shared: Arc<Shared>,
    sender: Sender<Msg>,
    offer: Vec<u8>,
}

impl Viewer {
    /// Abre os soquetes, descobre o endereço público e monta a oferta. Pode levar até ~1 s (a
    /// consulta do endereço público); chame fora da thread da interface.
    pub fn new(options: Options) -> Result<Self, AnyError> {
        install_crypto();
        let mut rtc = RtcConfig::new()
            // Só o que este player decodifica: assim quem transmite pelo navegador também escolhe H.264.
            .clear_codecs()
            .enable_h264(true)
            .enable_opus(true, false)
            .set_reordering_size_video(REORDER)
            .set_stats_interval(Some(Duration::from_secs(1)))
            .build(Instant::now());

        let sockets = bind_all()?;
        let default = local_ip();
        let mut locals = Vec::new();
        for (local, socket) in &sockets {
            rtc.add_local_candidate(Candidate::host(*local, "udp")?);
            locals.push((*local, "host"));
            if local.ip() == default
                && let Some(public) = public_address(socket)
                && public != *local
                && let Ok(candidate) = Candidate::server_reflexive(public, *local, "udp")
            {
                rtc.add_local_candidate(candidate);
                locals.push((public, "srflx"));
            }
        }

        let mut change = rtc.sdp_api();
        let video = change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        let audio = options
            .audio
            .then(|| change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None));
        let (offer, pending) = change.apply().ok_or("a oferta saiu vazia")?;
        let offer = serde_json::to_vec(&offer)?;

        let shared = Arc::new(Shared {
            state: AtomicU8::new(State::Connecting as u8),
            latest: Mutex::default(),
            fresh: Condvar::new(),
            serial: AtomicU64::new(0),
            route: Mutex::default(),
            decoder: Mutex::default(),
            device: Mutex::default(),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            frames_received: AtomicU64::new(0),
            frames_decoded: AtomicU64::new(0),
            frames_dropped: AtomicU64::new(0),
            decode_micros: AtomicU64::new(0),
            latency_micros: AtomicU64::new(0),
            backlog: AtomicUsize::new(0),
            keyframes_requested: AtomicU64::new(0),
            packets_missing: AtomicU64::new(0),
            nacks: AtomicU64::new(0),
            loss: AtomicU32::new(0),
            bytes_received: AtomicU64::new(0),
            bitrate: AtomicU32::new(0),
            rtt_micros: AtomicU64::new(u64::MAX),
            audio_packets: AtomicU64::new(0),
            audio_frames: AtomicU64::new(0),
            last_frame: Mutex::default(),
            cpu_network: AtomicU64::new(0),
            cpu_decoder: AtomicU64::new(0),
            speaker: options.audio.then(|| Speaker::new(options.volume)),
        });

        let (sender, inbox) = channel();
        // Uma thread por soquete só escuta a rede e acorda a conexão.
        for (local, socket) in &sockets {
            let reader = socket.try_clone()?;
            reader.set_read_timeout(Some(Duration::from_secs(1)))?;
            let (sender, local) = (sender.clone(), *local);
            std::thread::spawn(move || listen(&reader, local, &sender));
        }

        let (units, unit_inbox) = channel();
        {
            let (shared, sender) = (shared.clone(), sender.clone());
            std::thread::spawn(move || decode_loop(&unit_inbox, &shared, &sender, &options));
        }
        {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let connection = Connection {
                    rtc,
                    sockets,
                    pending: Some(pending),
                    video,
                    audio,
                    locals,
                    remotes: Vec::new(),
                    units,
                    opus: None,
                    last_seq: None,
                    asked: None,
                    bytes_before: None,
                };
                if let Err(e) = connection.run(&shared, &inbox) {
                    eprintln!("player: {e}");
                }
                shared.set_state(State::Closed);
            });
        }
        Ok(Self {
            shared,
            sender,
            offer,
        })
    }

    /// A oferta para mandar a quem transmite: JSON `{"type":"offer","sdp":"..."}`, já com os
    /// endereços deste lado. É o corpo que `Engine::accept` e o `POST /offer` do servidor de
    /// teste esperam.
    pub fn offer(&self) -> &[u8] {
        &self.offer
    }

    /// Entrega a resposta de quem transmite (JSON `{"type":"answer","sdp":"..."}`). A partir daqui
    /// o player testa os caminhos e conecta.
    pub fn answer(&self, answer: &[u8]) -> Result<(), AnyError> {
        let answer: SdpAnswer = serde_json::from_slice(answer)?;
        let remotes = candidates_in(&answer.to_sdp_string());
        let (reply, result) = sync_channel(1);
        self.sender
            .send(Msg::Answer(Box::new(answer), remotes, reply))
            .map_err(|_| "a conexão já acabou")?;
        match result.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err("a conexão não respondeu".into()),
        }
    }

    /// Mais um endereço de quem transmite, quando ele chega depois da resposta. Aceita a linha
    /// como vem do navegador (`candidate:...`) ou do SDP (`a=candidate:...`).
    pub fn add_remote_candidate(&self, candidate: &str) -> Result<(), AnyError> {
        let line = candidate.trim().trim_start_matches("a=");
        let kind = candidate_kind(line);
        let candidate = Candidate::from_sdp_string(line)?;
        self.sender
            .send(Msg::Candidate(candidate, kind))
            .map_err(|_| "a conexão já acabou")?;
        Ok(())
    }

    /// O quadro mais recente, se já houver algum.
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.shared.latest.lock().unwrap().clone()
    }

    /// Espera até `timeout` por um quadro mais novo que `after` (o `serial` do último que quem chama
    /// já desenhou; 0 no começo). Devolve sempre o mais recente: quadros intermediários são pulados.
    pub fn next_frame(&self, after: u64, timeout: Duration) -> Option<Arc<Frame>> {
        let deadline = Instant::now() + timeout;
        let mut latest = self.shared.latest.lock().unwrap();
        loop {
            if let Some(frame) = latest.as_ref()
                && frame.serial > after
            {
                return Some(frame.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.state() == State::Closed {
                return None;
            }
            latest = self.shared.fresh.wait_timeout(latest, left).unwrap().0;
        }
    }

    /// Dispositivo Direct3D 11 em que os quadros são decodificados (`Frame::gpu`). Existe depois do
    /// primeiro quadro, e só quando é a placa que decodifica.
    pub fn device(&self) -> Option<ID3D11Device> {
        self.shared
            .device
            .lock()
            .unwrap()
            .as_ref()
            .map(|device| device.0.clone())
    }

    pub fn state(&self) -> State {
        State::from_u8(self.shared.state.load(Ordering::Relaxed))
    }

    /// Volume do som, de 0 a 1.
    pub fn set_volume(&self, volume: f32) {
        if let Some(speaker) = &self.shared.speaker {
            speaker.set_volume(volume);
        }
    }

    pub fn stats(&self) -> Stats {
        let s = &self.shared;
        let speaker = s.speaker.as_ref();
        let rtt = s.rtt_micros.load(Ordering::Relaxed);
        Stats {
            state: self.state(),
            route: s.route.lock().unwrap().clone(),
            width: s.width.load(Ordering::Relaxed),
            height: s.height.load(Ordering::Relaxed),
            decoder: s.decoder.lock().unwrap().clone(),
            frames_received: s.frames_received.load(Ordering::Relaxed),
            frames_decoded: s.frames_decoded.load(Ordering::Relaxed),
            frames_dropped: s.frames_dropped.load(Ordering::Relaxed),
            decode_time: Duration::from_micros(s.decode_micros.load(Ordering::Relaxed)),
            latency: Duration::from_micros(s.latency_micros.load(Ordering::Relaxed)),
            backlog: s.backlog.load(Ordering::Relaxed),
            keyframes_requested: s.keyframes_requested.load(Ordering::Relaxed),
            packets_missing: s.packets_missing.load(Ordering::Relaxed),
            nacks: s.nacks.load(Ordering::Relaxed),
            loss: f32::from_bits(s.loss.load(Ordering::Relaxed)),
            bytes_received: s.bytes_received.load(Ordering::Relaxed),
            bitrate: s.bitrate.load(Ordering::Relaxed),
            rtt: (rtt != u64::MAX).then(|| Duration::from_micros(rtt)),
            audio_packets: s.audio_packets.load(Ordering::Relaxed),
            audio_frames: s.audio_frames.load(Ordering::Relaxed),
            audio_played: speaker.map_or(0, Speaker::played),
            audio_underruns: speaker.map_or(0, Speaker::underruns),
            audio_output: speaker.is_some_and(Speaker::is_open),
            since_last_frame: s.last_frame.lock().unwrap().map(|at| at.elapsed()),
            cpu_network: Duration::from_micros(s.cpu_network.load(Ordering::Relaxed)),
            cpu_decoder: Duration::from_micros(s.cpu_decoder.load(Ordering::Relaxed)),
            cpu_audio: speaker.map_or(Duration::ZERO, Speaker::cpu),
        }
    }

    /// Encerra a conexão. Soltar o `Viewer` faz o mesmo.
    pub fn close(&self) {
        let _ = self.sender.send(Msg::Close);
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.close();
    }
}

/// Escuta um soquete e entrega cada pacote à conexão. Termina quando a conexão termina.
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
                // Mesmo cuidado do motor: ConnectionReset em UDP no Windows volta na hora e sem parar.
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

/// Tipo de um endereço, sem o endereço em si. Mesmos nomes do diagnóstico do site (`addressKind`).
fn address_kind(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V6(v6) => match v6.segments()[0] {
            s if s & 0xfe00 == 0xfc00 => "ipv6-local",
            s if s & 0xffc0 == 0xfe80 => "ipv6-enlace",
            _ => "ipv6-global",
        },
        IpAddr::V4(v4) => match v4.octets() {
            [10, ..] | [192, 168, ..] | [172, 16..=31, ..] => "ipv4-privado",
            [100, 64..=127, ..] => "ipv4-cgnat",
            // 26.x é a faixa da rede virtual Radmin; 25.x, a da Hamachi.
            [25 | 26, ..] => "rede-virtual",
            _ => "ipv4-publico",
        },
    }
}

/// O tipo de um candidato (`host`, `srflx`, `relay`...), lido da linha dele.
fn candidate_kind(line: &str) -> &'static str {
    let mut fields = line.split_whitespace().skip_while(|field| *field != "typ");
    match fields.nth(1) {
        Some("host") => "host",
        Some("srflx") => "srflx",
        Some("relay") => "relay",
        _ => "prflx",
    }
}

/// Endereços que a resposta de quem transmite traz, com o tipo de cada um.
fn candidates_in(sdp: &str) -> Vec<(SocketAddr, &'static str)> {
    let mut found = Vec::new();
    for line in sdp.lines() {
        let Some(candidate) = line.trim().strip_prefix("a=candidate:") else {
            continue;
        };
        let fields: Vec<&str> = candidate.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        if let (Ok(ip), Ok(port)) = (fields[4].parse::<IpAddr>(), fields[5].parse::<u16>()) {
            let address = SocketAddr::new(ip, port);
            if !found.iter().any(|(known, _)| *known == address) {
                found.push((address, candidate_kind(candidate)));
            }
        }
    }
    found
}

/// `host:ipv4-privado`: o tipo de uma ponta do caminho escolhido. Endereço que ninguém anunciou
/// foi descoberto durante os testes (`prflx`).
fn side(address: SocketAddr, known: &[(SocketAddr, &'static str)]) -> String {
    let kind = known
        .iter()
        .find(|(candidate, _)| *candidate == address)
        .map_or("prflx", |(_, kind)| kind);
    format!("{kind}:{}", address_kind(address.ip()))
}

/// A conexão WebRTC de quem assiste e o que ela precisa para entregar vídeo e som.
struct Connection {
    rtc: Rtc,
    sockets: Vec<(SocketAddr, UdpSocket)>,
    /// A oferta que espera a resposta.
    pending: Option<SdpPendingOffer>,
    video: Mid,
    audio: Option<Mid>,
    locals: Vec<(SocketAddr, &'static str)>,
    remotes: Vec<(SocketAddr, &'static str)>,
    units: Sender<Unit>,
    opus: Option<opus::Decoder>,
    /// Último número de pacote de vídeo entregue, para contar os que faltaram.
    last_seq: Option<u64>,
    /// Quando foi o último pedido de quadro completo.
    asked: Option<Instant>,
    /// Bytes recebidos na medição anterior.
    bytes_before: Option<(u64, Instant)>,
}

impl Connection {
    fn run(mut self, shared: &Shared, inbox: &Receiver<Msg>) -> Result<(), AnyError> {
        let start = Instant::now();
        let mut answered: Option<Instant> = None;
        let mut connected = false;
        // Desde quando a rede sumiu (None = está respondendo).
        let mut lost: Option<Instant> = None;
        // Último pacote recebido de quem transmite, e se o silêncio já foi anunciado.
        let mut heard = Instant::now();
        let mut silent = false;
        let mut measured = Instant::now();
        loop {
            if measured.elapsed() >= CPU_EVERY {
                measured = Instant::now();
                shared
                    .cpu_network
                    .store(crate::thread_cpu().as_micros() as u64, Ordering::Relaxed);
            }
            let timeout = match self.rtc.poll_output()? {
                Output::Timeout(at) => at,
                Output::Transmit(packet) => {
                    // Sai pelo mesmo endereço local que o WebRTC escolheu para este caminho. Falha de
                    // envio (rede do computador piscou) não encerra.
                    let socket = self
                        .sockets
                        .iter()
                        .find(|(local, _)| *local == packet.source)
                        .or(self.sockets.first());
                    if let Some((_, socket)) = socket {
                        let _ = socket.send_to(&packet.contents, packet.destination);
                    }
                    continue;
                }
                Output::Event(event) => {
                    match event {
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                            lost.get_or_insert_with(Instant::now);
                            shared.set_state(State::Unstable);
                        }
                        Event::IceConnectionStateChange(_) => {
                            lost = None;
                            if connected && !silent {
                                shared.set_state(State::Connected);
                            }
                        }
                        Event::Connected => {
                            connected = true;
                            heard = Instant::now();
                            shared.set_state(State::Connected);
                        }
                        Event::MediaData(data) => self.media(data, shared),
                        Event::PeerStats(stats) => {
                            if let Some((bytes, at)) = self
                                .bytes_before
                                .replace((stats.peer_bytes_rx, stats.timestamp))
                            {
                                let seconds =
                                    stats.timestamp.saturating_duration_since(at).as_secs_f64();
                                if seconds > 0.2 {
                                    let bits =
                                        stats.peer_bytes_rx.saturating_sub(bytes) as f64 * 8.0;
                                    shared
                                        .bitrate
                                        .store((bits / seconds) as u32, Ordering::Relaxed);
                                }
                            }
                            shared
                                .bytes_received
                                .store(stats.peer_bytes_rx, Ordering::Relaxed);
                            shared.loss.store(
                                stats.ingress_loss_fraction.unwrap_or(0.0).to_bits(),
                                Ordering::Relaxed,
                            );
                            let pair = stats.selected_candidate_pair;
                            if let Some(rtt) = stats
                                .rtt
                                .or(pair.as_ref().and_then(|pair| pair.current_round_trip_time))
                            {
                                shared
                                    .rtt_micros
                                    .store(rtt.as_micros() as u64, Ordering::Relaxed);
                            }
                            if let Some(pair) = pair {
                                *shared.route.lock().unwrap() = format!(
                                    "{} -> {}",
                                    side(pair.local.addr, &self.locals),
                                    side(pair.remote.addr, &self.remotes)
                                );
                            }
                        }
                        Event::MediaIngressStats(stats) if stats.mid == self.video => {
                            shared.nacks.store(stats.nacks, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                    continue;
                }
            };
            let waited = answered.map_or(start.elapsed() > ANSWER_WAIT, |at| {
                !connected && at.elapsed() > CONNECT_WAIT
            });
            if connected && !silent && heard.elapsed() > SILENT {
                silent = true;
                shared.set_state(State::Unstable);
            }
            if !self.rtc.is_alive()
                || waited
                || lost.is_some_and(|since| since.elapsed() > LOST)
                || (connected && heard.elapsed() > LOST)
            {
                return Ok(());
            }
            // Espera menor que 1 ms não dorme de verdade no Windows (ver o motor): piso de 1 ms.
            let wait = timeout.saturating_duration_since(Instant::now());
            let message = if wait.is_zero() {
                None
            } else {
                inbox.recv_timeout(wait.max(Duration::from_millis(1))).ok()
            };
            match message {
                Some(Msg::Packet(data, source, local)) => {
                    // Qualquer um que alcance a porta pode mandar lixo: pacote que não é do WebRTC é
                    // ignorado, não derruba a conexão.
                    let Ok(contents) = data.as_slice().try_into() else {
                        continue;
                    };
                    let receive = Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: local,
                        contents,
                    };
                    if self
                        .rtc
                        .handle_input(Input::Receive(Instant::now(), receive))
                        .is_ok()
                    {
                        heard = Instant::now();
                        if silent {
                            silent = false;
                            if connected && lost.is_none() {
                                shared.set_state(State::Connected);
                            }
                        }
                    }
                }
                Some(Msg::Answer(answer, remotes, reply)) => {
                    let result = match self.pending.take() {
                        Some(pending) => self
                            .rtc
                            .sdp_api()
                            .accept_answer(pending, *answer)
                            .map_err(|e| e.to_string()),
                        None => Err("esta oferta já recebeu resposta".into()),
                    };
                    if result.is_ok() {
                        self.remotes = remotes;
                        answered = Some(Instant::now());
                    }
                    let _ = reply.send(result);
                }
                Some(Msg::Candidate(candidate, kind)) => {
                    self.remotes.push((candidate.addr(), kind));
                    self.rtc.add_remote_candidate(candidate);
                }
                Some(Msg::Keyframe) => self.ask_keyframe(shared),
                Some(Msg::Close) => return Ok(()),
                Some(Msg::Tick) | None => self.rtc.handle_input(Input::Timeout(Instant::now()))?,
            }
        }
    }

    /// Um quadro de vídeo ou um pacote de som que a rede acabou de completar.
    fn media(&mut self, data: MediaData, shared: &Shared) {
        let codec = data.params.spec().codec;
        if data.mid == self.video {
            if codec != Codec::H264 {
                return;
            }
            // Pacotes que faltaram entre este quadro e o anterior (os reenvios já foram tentados).
            let (first, last) = (**data.seq_range.start(), **data.seq_range.end());
            if let Some(before) = self.last_seq
                && first > before + 1
            {
                shared
                    .packets_missing
                    .fetch_add(first - before - 1, Ordering::Relaxed);
            }
            self.last_seq = Some(last);
            shared.frames_received.fetch_add(1, Ordering::Relaxed);
            shared.backlog.fetch_add(1, Ordering::Relaxed);
            let _ = self.units.send(Unit {
                data: data.data,
                contiguous: data.contiguous,
                received: data.network_time,
            });
        } else if Some(data.mid) == self.audio && codec == Codec::Opus {
            shared.audio_packets.fetch_add(1, Ordering::Relaxed);
            if self.opus.is_none() {
                self.opus = opus::Decoder::new(playback::SAMPLE_RATE, opus::Channels::Stereo).ok();
            }
            let Some(opus) = self.opus.as_mut() else {
                return;
            };
            let mut pcm = [0i16; OPUS_FRAMES * playback::CHANNELS];
            if let Ok(frames) = opus.decode(&data.data, &mut pcm, false) {
                shared
                    .audio_frames
                    .fetch_add(frames as u64, Ordering::Relaxed);
                if let Some(speaker) = &shared.speaker {
                    speaker.push(&pcm[..frames * playback::CHANNELS]);
                }
            }
        }
    }

    /// Pede um quadro completo a quem transmite, sem repetir o pedido em menos de `KEYFRAME_GAP`.
    fn ask_keyframe(&mut self, shared: &Shared) {
        if self.asked.is_some_and(|at| at.elapsed() < KEYFRAME_GAP) {
            return;
        }
        let Some(mut writer) = self.rtc.writer(self.video) else {
            return;
        };
        let asked = writer
            .request_keyframe(None, KeyframeRequestKind::Pli)
            .or_else(|_| writer.request_keyframe(None, KeyframeRequestKind::Fir));
        if asked.is_ok() {
            self.asked = Some(Instant::now());
            shared.keyframes_requested.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Thread do decodificador: recebe os quadros H.264 na ordem e publica o mais recente decodificado.
fn decode_loop(units: &Receiver<Unit>, shared: &Shared, network: &Sender<Msg>, options: &Options) {
    let pool = Pool::default();
    let mut decoder: Option<Decoder> = None;
    // Esperando um quadro completo: no começo, depois de perda e depois de erro.
    let mut waiting = true;
    // Entregas seguidas ao decodificador sem sair imagem.
    let mut starved = 0u32;
    // Últimos parâmetros da sequência e da imagem (SPS e PPS), com o código de início, para
    // completar um quadro completo que venha sem eles depois de o decodificador ser recriado.
    let mut parameters: [Option<Vec<u8>>; 2] = [None, None];
    let mut pictures = Vec::new();
    let mut joined = Vec::new();
    let drop_frame = |waiting: &mut bool| {
        *waiting = true;
        shared.frames_dropped.fetch_add(1, Ordering::Relaxed);
        let _ = network.send(Msg::Keyframe);
    };
    let mut measured = Instant::now();
    while let Ok(unit) = units.recv() {
        if measured.elapsed() >= CPU_EVERY {
            measured = Instant::now();
            shared
                .cpu_decoder
                .store(crate::thread_cpu().as_micros() as u64, Ordering::Relaxed);
        }
        let behind = shared.backlog.fetch_sub(1, Ordering::Relaxed);
        let key = decode::has_idr(&unit.data);
        if !unit.contiguous || behind > BACKLOG {
            waiting = true;
        }
        if waiting && !key {
            drop_frame(&mut waiting);
            continue;
        }
        let mut seen = [false, false];
        for nal in decode::units(&unit.data) {
            let slot = match nal.first().map(|byte| byte & 0x1f) {
                Some(7) => 0,
                Some(8) => 1,
                _ => continue,
            };
            seen[slot] = true;
            let mut stored = vec![0, 0, 0, 1];
            stored.extend_from_slice(nal);
            parameters[slot] = Some(stored);
        }
        let data: &[u8] = if key && !(seen[0] && seen[1]) {
            joined.clear();
            for (slot, stored) in parameters.iter().enumerate() {
                if !seen[slot]
                    && let Some(stored) = stored
                {
                    joined.extend_from_slice(stored);
                }
            }
            joined.extend_from_slice(&unit.data);
            &joined
        } else {
            &unit.data
        };

        let started = Instant::now();
        if decoder.is_none() {
            match Decoder::new(options.software, options.memory, &pool) {
                Ok((created, name)) => {
                    *shared.device.lock().unwrap() = created.device().map(SharedDevice);
                    *shared.decoder.lock().unwrap() = name;
                    decoder = Some(created);
                }
                Err(e) => {
                    eprintln!("player: sem decodificador de vídeo: {e}");
                    drop_frame(&mut waiting);
                    continue;
                }
            }
        }
        let Some(active) = decoder.as_mut() else {
            continue;
        };
        if let Err(e) = active.decode(data, &pool, &mut pictures) {
            eprintln!("player: decodificador: {e}");
            decoder = None;
            starved = 0;
            pictures.clear();
            drop_frame(&mut waiting);
            continue;
        }
        waiting = false;
        if pictures.is_empty() {
            starved += 1;
            if starved >= STARVED {
                eprintln!("player: o decodificador parou de entregar imagem; recriando");
                decoder = None;
                starved = 0;
                drop_frame(&mut waiting);
            }
            continue;
        }
        starved = 0;
        for picture in pictures.drain(..) {
            let now = Instant::now();
            let frame = Arc::new(Frame {
                serial: shared.serial.fetch_add(1, Ordering::Relaxed) + 1,
                width: picture.width,
                height: picture.height,
                stride: picture.width as usize,
                data: picture.data,
                gpu: picture.gpu,
                received: unit.received,
                decoded: now,
                pool: pool.clone(),
            });
            shared.width.store(frame.width, Ordering::Relaxed);
            shared.height.store(frame.height, Ordering::Relaxed);
            shared.frames_decoded.fetch_add(1, Ordering::Relaxed);
            shared.latency_micros.fetch_add(
                now.saturating_duration_since(unit.received).as_micros() as u64,
                Ordering::Relaxed,
            );
            *shared.last_frame.lock().unwrap() = Some(now);
            *shared.latest.lock().unwrap() = Some(frame);
            shared.fresh.notify_all();
        }
        shared
            .decode_micros
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
    // A conexão acabou: solta o último quadro (e a textura do decodificador) antes do decodificador.
    *shared.latest.lock().unwrap() = None;
    *shared.device.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn names_the_kind_of_an_address_like_the_site_does() {
        let kind = |text: &str| address_kind(text.parse().unwrap());
        assert_eq!(kind("192.168.15.9"), "ipv4-privado");
        assert_eq!(kind("172.20.1.1"), "ipv4-privado");
        assert_eq!(kind("172.32.1.1"), "ipv4-publico");
        assert_eq!(kind("100.72.0.1"), "ipv4-cgnat");
        assert_eq!(kind("26.10.20.30"), "rede-virtual");
        assert_eq!(kind("200.1.2.3"), "ipv4-publico");
        assert_eq!(kind("fd00::5"), "ipv6-local");
        assert_eq!(kind("fe80::1"), "ipv6-enlace");
        assert_eq!(kind("2804:14c::9"), "ipv6-global");
    }

    #[test]
    fn reads_the_candidates_of_an_answer_with_their_kind() {
        let sdp = "v=0\r\na=candidate:1 1 udp 2122260223 192.168.0.5 50000 typ host\r\n\
                   a=candidate:2 1 UDP 1686052607 200.1.2.3 50000 typ srflx raddr 192.168.0.5 rport 50000\r\n\
                   a=candidate:4 1 udp 2122260223 abc123.local 50001 typ host\r\n\
                   a=candidate:1 1 udp 2122260223 192.168.0.5 50000 typ host\r\n";
        let found = candidates_in(sdp);
        assert_eq!(
            found,
            vec![
                ("192.168.0.5:50000".parse().unwrap(), "host"),
                ("200.1.2.3:50000".parse().unwrap(), "srflx"),
            ]
        );
        assert_eq!(
            side("192.168.0.5:50000".parse().unwrap(), &found),
            "host:ipv4-privado"
        );
        // Endereço que ninguém anunciou: descoberto no teste de caminho.
        assert_eq!(
            side("201.9.9.9:4000".parse().unwrap(), &found),
            "prflx:ipv4-publico"
        );
    }

    /// A oferta do player é aceita pelo motor de quem transmite, com vídeo e som, e a resposta dele
    /// é aceita de volta. Não passa mídia: sem captura, o motor não tem o que mandar.
    #[test]
    fn the_engine_accepts_the_offer_and_the_player_accepts_the_answer() {
        let engine = Engine::new();
        let viewer = Viewer::new(Options {
            volume: 0.0,
            ..Options::default()
        })
        .unwrap();
        let offer = String::from_utf8(viewer.offer().to_vec()).unwrap();
        assert!(offer.contains("\"type\":\"offer\""), "{offer}");
        assert!(offer.contains("m=video"), "{offer}");
        assert!(offer.contains("m=audio"), "{offer}");
        assert!(offer.contains("a=recvonly"), "{offer}");
        assert!(offer.contains("H264/90000"), "{offer}");
        assert!(!offer.contains("VP8"), "{offer}");
        assert!(offer.contains("a=candidate:"), "{offer}");

        let (id, answer) = engine.accept(viewer.offer()).unwrap();
        viewer.answer(&answer).unwrap();
        // A mesma oferta não aceita duas respostas.
        assert!(viewer.answer(&answer).is_err());
        assert_eq!(viewer.state(), State::Connecting);
        engine.close(id);
        viewer.close();
    }
}
