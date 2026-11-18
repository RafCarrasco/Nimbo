//! Lives nativas do canal em que a pessoa está: assistir (uma conexão do player por live) e
//! transmitir (o motor responde a oferta de cada pessoa que quer assistir).
//!
//! A sinalização é a do site, para valer entre app nativo, app atual e site:
//! - quem assiste (`Watching`) espelha o `NativeReceiver` de `src/live/native.ts`: oferta com um
//!   código (`nonce`), resposta só vale com o mesmo código, 3 tentativas, 12 s para conectar, 5 s de
//!   rede calada, e a conexão é refeita quando a live recomeça ou passa a ter (ou deixa de ter) som;
//! - quem transmite (`Broadcasting`) faz o que `answerOffer`/`closeNativeOut` fazem em
//!   `src/live/room.ts`: uma conexão no motor por pessoa, a anterior da mesma pessoa é fechada, e a
//!   de quem saiu do canal também.
//!
//! Diferença de propósito para o site: lá o navegador abre conexão com toda live nativa do canal,
//! mesmo as que não estão no palco. Aqui só as que estão no palco, para a placa não decodificar
//! vídeo que ninguém vê e quem transmite não contar espectador que não está assistindo.
//!
//! Prévia da própria live (`src/live/preview.ts`): 3 s depois de começar e a cada 45 s, uma foto de
//! 256 px de largura do que está sendo transmitido vai para o canal (JPEG em base64, até 14000
//! caracteres). A foto não sai do motor: é tirada pelo mesmo caminho das miniaturas do seletor (GDI,
//! `capture_thumbnail`), que não mexe na captura da live.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use nimbo_core::room::{Broadcasts, PREVIEW_LIMIT, Room};
use nimbo_media::engine::{AudioSource, Engine, Quality, Source};
use nimbo_media::viewer::{self, Frame, Viewer};
use serde_json::json;

const TRIES: u32 = 3;
const CONNECT: Duration = Duration::from_millis(12_000);
/// O motor encerra a conexão do lado dele sem aviso: com a rede calada por este tempo, tenta de novo.
const LOST: Duration = Duration::from_millis(5_000);
/// Prévia da própria live: a primeira depois de tanto tempo, e depois de quanto em quanto.
const PREVIEW_FIRST: Duration = Duration::from_millis(3_000);
const PREVIEW_EVERY: Duration = Duration::from_millis(45_000);
const PREVIEW_WIDTH: u32 = 256;

pub fn live_key(uid: &str, kind: &str) -> String {
    format!("{uid}:{kind}")
}

pub fn parse_key(key: &str) -> (&str, &str) {
    key.rsplit_once(':').unwrap_or((key, ""))
}

/// Como está uma live para quem assiste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveState {
    Playing,
    Connecting,
    Failed,
}

#[derive(Clone, PartialEq)]
struct Wanted {
    uid: String,
    kind: String,
    stream_id: String,
    sound: bool,
}

struct Incoming {
    live: Wanted,
    nonce: String,
    tries: u32,
    opened: Instant,
    /// Número desta tentativa: o que chega de uma tentativa antiga é descartado.
    attempt: u64,
    viewer: Option<Arc<Viewer>>,
    answered: bool,
    connected: bool,
    silent_since: Option<Instant>,
    stop: Arc<AtomicBool>,
}

enum Report {
    Opened {
        key: String,
        attempt: u64,
        viewer: Arc<Viewer>,
    },
    Failed {
        key: String,
        attempt: u64,
        why: String,
    },
}

/// Lado de quem assiste: uma conexão só de recepção para cada live nativa que está no palco.
pub struct Watching {
    room: Arc<Room>,
    uid: String,
    volume: f32,
    conns: HashMap<String, Incoming>,
    /// Lives que já esgotaram as tentativas ("uid:tipo|id da live"); só tenta de novo quando a live
    /// recomeça ou a pessoa pede.
    gave_up: HashSet<String>,
    attempts: u64,
    reports: (Sender<Report>, Receiver<Report>),
    repaint: egui::Context,
    /// Último motivo de uma tentativa ter acabado, por live (para mostrar e para o relatório).
    pub last_error: HashMap<String, String>,
}

impl Watching {
    pub fn new(room: Arc<Room>, uid: &str, volume: f32, repaint: &egui::Context) -> Self {
        Self {
            room,
            uid: uid.to_owned(),
            volume,
            conns: HashMap::new(),
            gave_up: HashSet::new(),
            attempts: 0,
            reports: channel(),
            repaint: repaint.clone(),
            last_error: HashMap::new(),
        }
    }

    /// Abre conexão para as lives nativas que estão no palco e fecha as que acabaram ou saíram dele.
    pub fn sync(
        &mut self,
        broadcasts: &Broadcasts,
        members: &[String],
        on_stage: &BTreeSet<String>,
    ) {
        let mut native: HashMap<String, Wanted> = HashMap::new();
        for (uid, kinds) in broadcasts {
            if *uid == self.uid || !members.contains(uid) {
                continue;
            }
            for (kind, live) in kinds.iter().filter(|(_, live)| live.native) {
                let wanted = Wanted {
                    uid: uid.clone(),
                    kind: kind.clone(),
                    stream_id: live.stream_id.clone(),
                    sound: live.sound,
                };
                native.insert(live_key(uid, kind), wanted);
            }
        }
        // Live acabou, recomeçou, saiu do palco, ou passou a ter (ou deixou de ter) som: fecha.
        let stale: Vec<String> = self
            .conns
            .iter()
            .filter(|(key, conn)| !on_stage.contains(*key) || native.get(*key) != Some(&conn.live))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.close(&key);
        }
        self.gave_up.retain(|entry| {
            entry.split_once('|').is_some_and(|(key, stream_id)| {
                native
                    .get(key)
                    .is_some_and(|live| live.stream_id == stream_id)
            })
        });
        for (key, live) in native {
            if on_stage.contains(&key)
                && !self.conns.contains_key(&key)
                && !self.gave_up.contains(&format!("{key}|{}", live.stream_id))
            {
                self.open(key, live, 0);
            }
        }
    }

    fn open(&mut self, key: String, live: Wanted, tries: u32) {
        self.attempts += 1;
        let attempt = self.attempts;
        let nonce = random_id();
        let stop = Arc::new(AtomicBool::new(false));
        self.conns.insert(
            key.clone(),
            Incoming {
                live: live.clone(),
                nonce: nonce.clone(),
                tries,
                opened: Instant::now(),
                attempt,
                viewer: None,
                answered: false,
                connected: false,
                silent_since: None,
                stop: stop.clone(),
            },
        );
        let (room, reports, repaint, volume) = (
            self.room.clone(),
            self.reports.0.clone(),
            self.repaint.clone(),
            self.volume,
        );
        // Abrir o player leva até ~1 s (ele descobre o endereço público): fora da thread que desenha.
        // A mesma thread depois acorda a tela a cada quadro novo.
        std::thread::spawn(move || {
            let failed = |why: String| {
                let _ = reports.send(Report::Failed {
                    key: key.clone(),
                    attempt,
                    why,
                });
                repaint.request_repaint();
            };
            let options = viewer::Options {
                audio: live.sound,
                volume,
                software: false,
                memory: true,
            };
            let viewer = match Viewer::new(options) {
                Ok(viewer) => Arc::new(viewer),
                Err(e) => return failed(format!("o player não abriu: {e}")),
            };
            let offer = serde_json::from_slice::<serde_json::Value>(viewer.offer()).ok();
            let Some(sdp) = offer.as_ref().and_then(|offer| offer["sdp"].as_str()) else {
                return failed("o player montou uma oferta sem conteúdo".into());
            };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let _ = reports.send(Report::Opened {
                key: key.clone(),
                attempt,
                viewer: viewer.clone(),
            });
            let (on_error, wake) = (reports.clone(), repaint.clone());
            let error_key = key.clone();
            room.send_native_offer(&live.uid, &live.kind, sdp, &nonce, move |result| {
                if let Err(e) = result {
                    let _ = on_error.send(Report::Failed {
                        key: error_key,
                        attempt,
                        why: format!("oferta não enviada: {e}"),
                    });
                    wake.request_repaint();
                }
            });
            let mut last = 0;
            while !stop.load(Ordering::Relaxed) && viewer.state() != viewer::State::Closed {
                if let Some(frame) = viewer.next_frame(last, Duration::from_millis(250)) {
                    last = frame.serial;
                }
                // Também sem quadro novo: o estado pode ter mudado (conectou, calou, caiu).
                repaint.request_repaint();
            }
            repaint.request_repaint();
        });
    }

    /// Resposta do motor de quem transmite à nossa oferta.
    pub fn on_answer(&mut self, from: &str, kind: &str, sdp: &str, nonce: &str) {
        let key = live_key(from, kind);
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        // Só vale a resposta à oferta desta conexão.
        let Some(viewer) = conn
            .viewer
            .clone()
            .filter(|_| conn.nonce == nonce && !conn.answered)
        else {
            return;
        };
        conn.answered = true;
        let (reports, repaint, attempt) =
            (self.reports.0.clone(), self.repaint.clone(), conn.attempt);
        let answer = json!({ "type": "answer", "sdp": sdp }).to_string();
        std::thread::spawn(move || {
            if let Err(e) = viewer.answer(answer.as_bytes()) {
                let _ = reports.send(Report::Failed {
                    key,
                    attempt,
                    why: format!("resposta recusada: {e}"),
                });
                repaint.request_repaint();
            }
        });
    }

    /// Chamado a cada quadro da interface: recolhe o que as threads avisaram e confere os prazos.
    pub fn tick(&mut self) {
        while let Ok(report) = self.reports.1.try_recv() {
            match report {
                Report::Opened {
                    key,
                    attempt,
                    viewer,
                } => match self
                    .conns
                    .get_mut(&key)
                    .filter(|conn| conn.attempt == attempt)
                {
                    Some(conn) => conn.viewer = Some(viewer),
                    None => viewer.close(),
                },
                Report::Failed { key, attempt, why } => {
                    if self
                        .conns
                        .get(&key)
                        .is_some_and(|conn| conn.attempt == attempt)
                    {
                        self.retry_later(&key, why);
                    }
                }
            }
        }
        let now = Instant::now();
        let mut failed: Vec<(String, &'static str)> = Vec::new();
        for (key, conn) in &mut self.conns {
            let state = conn.viewer.as_ref().map(|viewer| viewer.state());
            match state {
                Some(viewer::State::Connected) => {
                    // Conectou: falhas antigas não contam mais.
                    conn.connected = true;
                    conn.tries = 0;
                    conn.silent_since = None;
                }
                Some(viewer::State::Closed) => failed.push((key.clone(), "conexão falhou")),
                Some(viewer::State::Unstable) => {
                    if now.duration_since(*conn.silent_since.get_or_insert(now)) >= LOST {
                        failed.push((key.clone(), "conexão caiu"));
                    }
                }
                Some(viewer::State::Connecting) | None => {
                    if !conn.connected && now.duration_since(conn.opened) >= CONNECT {
                        failed.push((key.clone(), "sem conexão no prazo"));
                    }
                }
            }
        }
        for (key, why) in failed {
            self.retry_later(&key, why.to_owned());
        }
    }

    /// Uma tentativa acabou mal: abre outra, até o limite; depois disso a live fica como "não conectou".
    fn retry_later(&mut self, key: &str, why: String) {
        let Some(conn) = self.conns.get(key) else {
            return;
        };
        let (failures, live) = (conn.tries + 1, conn.live.clone());
        self.last_error.insert(key.to_owned(), why);
        self.close(key);
        if failures < TRIES {
            self.open(key.to_owned(), live, failures);
        } else {
            self.gave_up.insert(format!("{key}|{}", live.stream_id));
        }
    }

    /// "Tentar de novo" (de uma pessoa) ou volta da rede (de todos): esquece as desistências e fecha
    /// as conexões; o próximo `sync` abre de novo.
    pub fn reset(&mut self, uid: Option<&str>) {
        let keys: Vec<String> = self
            .conns
            .keys()
            .filter(|key| uid.is_none_or(|uid| parse_key(key).0 == uid))
            .cloned()
            .collect();
        for key in keys {
            self.close(&key);
        }
        self.gave_up
            .retain(|entry| uid.is_some_and(|uid| !entry.starts_with(&format!("{uid}:"))));
    }

    pub fn state(&self, key: &str) -> LiveState {
        let Some(conn) = self.conns.get(key) else {
            let gave_up = self
                .gave_up
                .iter()
                .any(|entry| entry.split_once('|').is_some_and(|(base, _)| base == key));
            return if gave_up {
                LiveState::Failed
            } else {
                LiveState::Connecting
            };
        };
        match &conn.viewer {
            Some(viewer)
                if viewer.state() == viewer::State::Connected && viewer.latest().is_some() =>
            {
                LiveState::Playing
            }
            _ => LiveState::Connecting,
        }
    }

    /// O quadro mais recente de uma live, se ela já está chegando.
    pub fn frame(&self, key: &str) -> Option<Arc<Frame>> {
        self.conns.get(key)?.viewer.as_ref()?.latest()
    }

    /// A live tem som chegando (para mostrar o volume ou "Sem som").
    pub fn has_sound(&self, key: &str) -> bool {
        self.conns.get(key).is_some_and(|conn| conn.live.sound)
    }

    /// Números de cada conexão aberta, para o relatório de medidas.
    pub fn stats(&self) -> Vec<(String, viewer::Stats)> {
        self.conns
            .iter()
            .filter_map(|(key, conn)| Some((key.clone(), conn.viewer.as_ref()?.stats())))
            .collect()
    }

    fn close(&mut self, key: &str) {
        if let Some(conn) = self.conns.remove(key) {
            conn.stop.store(true, Ordering::Relaxed);
            if let Some(viewer) = conn.viewer {
                viewer.close();
            }
        }
    }

    pub fn close_all(&mut self) {
        let keys: Vec<String> = self.conns.keys().cloned().collect();
        for key in keys {
            self.close(&key);
        }
        self.gave_up.clear();
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// O que transmitir e com que som.
#[derive(Clone, Debug)]
pub struct Capture {
    pub source: Source,
    pub audio: Option<AudioSource>,
    /// Nome do jogo, quando a fonte é um jogo (vai no anúncio da live, até 48 caracteres). Título de
    /// janela qualquer não vai: pode expor o que a pessoa não quis mostrar.
    pub game: String,
}

/// Quanto custou a última prévia (para `--stats`).
#[derive(Clone, Copy, Debug, Default)]
pub struct PreviewCost {
    pub millis: f64,
    pub chars: usize,
    pub sent: u32,
}

/// Foto pequena da fonte da live, como data URL (JPEG), ou `None` quando não deu (janela
/// minimizada, protegida, imagem toda preta).
fn snapshot(source: Source) -> Option<String> {
    let target = match source {
        Source::Window(hwnd) => sources::CaptureTarget::Window(hwnd),
        Source::Monitor(index) => sources::CaptureTarget::Monitor(index),
        Source::PrimaryMonitor => sources::CaptureTarget::Monitor(
            sources::list_monitors()
                .into_iter()
                .find(|monitor| monitor.primary)
                .map_or(1, |monitor| monitor.index),
        ),
    };
    let shot = sources::capture_thumbnail_as(
        target,
        PREVIEW_WIDTH,
        PREVIEW_WIDTH * 4,
        sources::ImageFormat::Png,
    )
    .ok()?;
    let image = image::load_from_memory(&shot.data).ok()?;
    // A mesma escada de qualidade do site: a primeira que couber no limite das regras.
    for quality in [50, 35, 20] {
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality);
        if image.to_rgb8().write_with_encoder(encoder).is_err() {
            return None;
        }
        let data = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&jpeg)
        );
        if data.len() <= PREVIEW_LIMIT {
            return Some(data);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastState {
    Starting,
    Live,
    Failed,
    Stopped,
}

enum Order {
    /// Oferta de alguém que quer assistir.
    Offer {
        from: String,
        sdp: String,
        nonce: String,
    },
    Stop,
}

/// Lado de quem transmite: a tela (ou janela) capturada pelo motor, anunciada no canal como live
/// nativa, e uma conexão no motor para cada pessoa que pede para assistir.
pub struct Broadcasting {
    engine: Arc<Engine>,
    orders: Sender<Order>,
    state: Arc<AtomicU8>,
    error: Arc<Mutex<String>>,
    /// Conexão aberta no motor para cada pessoa que assiste.
    out: Arc<Mutex<BTreeMap<String, u64>>>,
    /// Quem está no canal agora (o motor só responde a quem está).
    members: Arc<Mutex<Vec<String>>>,
    /// A última prévia gravada (data URL) e quanto ela custou.
    preview: Arc<Mutex<(Option<String>, PreviewCost)>>,
}

impl Broadcasting {
    /// Começa a captura e anuncia a live. Tudo acontece numa thread própria: o resultado aparece em
    /// `state()`.
    pub fn start(room: Arc<Room>, capture: Capture, repaint: &egui::Context) -> Self {
        let (engine, state, error) = (
            Arc::new(Engine::new()),
            Arc::new(AtomicU8::new(0)),
            Arc::new(Mutex::new(String::new())),
        );
        let (out, members) = (
            Arc::new(Mutex::new(BTreeMap::new())),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (orders, inbox) = channel();
        let preview = Arc::new(Mutex::new((None, PreviewCost::default())));
        let this = Self {
            engine: engine.clone(),
            orders,
            state: state.clone(),
            error: error.clone(),
            out: out.clone(),
            members: members.clone(),
            preview: preview.clone(),
        };
        let repaint = repaint.clone();
        std::thread::spawn(move || {
            let set = |value: BroadcastState| {
                state.store(value as u8, Ordering::Relaxed);
                repaint.request_repaint();
            };
            let stream_id = random_id();
            let started = engine
                .set_audio(capture.audio)
                .and_then(|()| engine.start(capture.source, Quality::default()))
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    room.publish_stream(
                        "screen",
                        &stream_id,
                        capture.audio.is_some(),
                        &capture.game,
                    )
                    .map_err(|e| e.to_string())
                });
            if let Err(e) = started {
                *error.lock().expect("erro da transmissão") = e;
                engine.stop();
                return set(BroadcastState::Failed);
            }
            set(BroadcastState::Live);
            let mut next_preview = Instant::now() + PREVIEW_FIRST;
            // Uma oferta por vez, na ordem em que chegaram: duas ofertas da mesma pessoa (nova
            // tentativa) não podem deixar duas conexões abertas no motor. Entre uma e outra, a prévia.
            loop {
                let wait = next_preview.saturating_duration_since(Instant::now());
                let (from, sdp, nonce) = match inbox.recv_timeout(wait) {
                    Ok(Order::Offer { from, sdp, nonce }) => (from, sdp, nonce),
                    Ok(Order::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        next_preview = Instant::now() + PREVIEW_EVERY;
                        let started = Instant::now();
                        if let Some(data) = snapshot(capture.source) {
                            let millis = started.elapsed().as_secs_f64() * 1000.0;
                            if room.publish_preview("screen", &data).is_ok() {
                                let mut slot = preview.lock().expect("prévia");
                                let sent = slot.1.sent + 1;
                                *slot = (
                                    Some(data.clone()),
                                    PreviewCost {
                                        millis,
                                        chars: data.len(),
                                        sent,
                                    },
                                );
                                repaint.request_repaint();
                            }
                        }
                        continue;
                    }
                };
                let present = |who: &str| {
                    members
                        .lock()
                        .expect("membros")
                        .iter()
                        .any(|member| member == who)
                };
                if !present(&from) {
                    continue;
                }
                let key = live_key(&from, "screen");
                if let Some(previous) = out.lock().expect("espectadores").remove(&key) {
                    engine.close(previous);
                }
                let offer = json!({ "type": "offer", "sdp": sdp }).to_string();
                let Ok((id, answer)) = engine.accept(offer.as_bytes()) else {
                    continue;
                };
                let answer = serde_json::from_slice::<serde_json::Value>(&answer).ok();
                let answer = answer.as_ref().and_then(|answer| answer["sdp"].as_str());
                // A pessoa pode ter saído enquanto o motor respondia.
                match answer.filter(|_| present(&from)) {
                    Some(sdp) => {
                        out.lock().expect("espectadores").insert(key, id);
                        room.send_native_answer(&from, "screen", sdp, &nonce, |_| {});
                    }
                    None => engine.close(id),
                }
            }
            for (_, id) in std::mem::take(&mut *out.lock().expect("espectadores")) {
                engine.close(id);
            }
            engine.stop();
            room.unpublish_stream("screen");
            set(BroadcastState::Stopped);
        });
        this
    }

    pub fn state(&self) -> BroadcastState {
        match self.state.load(Ordering::Relaxed) {
            0 => BroadcastState::Starting,
            1 => BroadcastState::Live,
            2 => BroadcastState::Failed,
            _ => BroadcastState::Stopped,
        }
    }

    pub fn error(&self) -> String {
        self.error.lock().expect("erro da transmissão").clone()
    }

    /// Oferta de alguém que quer assistir à live.
    pub fn offer(&self, from: &str, sdp: &str, nonce: &str) {
        let _ = self.orders.send(Order::Offer {
            from: from.to_owned(),
            sdp: sdp.to_owned(),
            nonce: nonce.to_owned(),
        });
    }

    /// Quem está no canal agora: fecha no motor a conexão de quem saiu.
    pub fn set_members(&self, members: &[String]) {
        *self.members.lock().expect("membros") = members.to_vec();
        let mut out = self.out.lock().expect("espectadores");
        let gone: Vec<String> = out
            .keys()
            .filter(|key| !members.iter().any(|member| member == parse_key(key).0))
            .cloned()
            .collect();
        for key in gone {
            if let Some(id) = out.remove(&key) {
                self.engine.close(id);
            }
        }
    }

    pub fn stats(&self) -> nimbo_media::engine::Stats {
        self.engine.stats()
    }

    /// A última prévia gravada no canal (data URL), para mostrar no próprio ladrilho.
    pub fn preview(&self) -> Option<String> {
        self.preview.lock().expect("prévia").0.clone()
    }

    pub fn preview_cost(&self) -> PreviewCost {
        self.preview.lock().expect("prévia").1
    }

    /// Encerra a live: fecha as conexões, para a captura e tira o anúncio (na thread da transmissão).
    pub fn stop(&self) {
        let _ = self.orders.send(Order::Stop);
    }
}

impl Drop for Broadcasting {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Identificador ao acaso (para o código da oferta e o id da live), no jeito de um UUID sem traços.
fn random_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
