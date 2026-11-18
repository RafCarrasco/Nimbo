//! Comandos do motor de mídia (`nimbo-media`): a página pede a captura e repassa a oferta WebRTC de
//! cada pessoa que quer assistir; o motor captura, codifica uma vez e envia para todos.
//! O contrato é o `media` de `src/desktop.ts` (ver também `src/live/native.ts`).

use std::sync::atomic::{AtomicBool, AtomicI8, Ordering};
use std::sync::{Arc, Mutex};

use nimbo_media::engine::{AudioSource, Engine, Quality, Source};
use nimbo_native::{Watch, WatchEvent, WatchTarget};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::bridge::{FromApp, Reply};

/// O motor é um só por app: uma captura, vários espectadores.
#[derive(Default)]
pub struct Media {
    engine: Engine,
    /// Vigia do jogo transmitido: diz quando ele sai da frente (Alt+Tab) e quando fecha.
    watch: Mutex<Option<Watch>>,
    game: Arc<Game>,
}

/// O que a vigia viu por último. A página consulta pelo `media_state`.
#[derive(Default)]
struct Game {
    /// -1 = não se sabe (ou não há jogo vigiado), 0 = em segundo plano, 1 = na frente.
    foreground: AtomicI8,
    exited: AtomicBool,
}

impl Game {
    fn reset(&self) {
        self.foreground.store(-1, Ordering::Relaxed);
        self.exited.store(false, Ordering::Relaxed);
    }
}

/// `{ monitor: n }` ou `{ window: hwnd }`; sem fonte, a tela principal.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceArg {
    Monitor(usize),
    Window(isize),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityArg {
    fps: u32,
    bitrate: u32,
    max_height: u32,
}

impl QualityArg {
    /// A página é quem pede, mas os limites são do app: valores absurdos não chegam ao codificador.
    fn checked(&self) -> Quality {
        Quality {
            fps: self.fps.clamp(15, 60),
            bitrate: self.bitrate.clamp(500_000, 20_000_000),
            max_height: self.max_height.clamp(360, 2160),
        }
    }
}

/// `"system"` = todo o som do computador menos o do próprio Nimbo e o dos apps de chamada de voz
/// (Discord, Teams...); `{ app: pid }` = só o daquele programa. O formato é o que o site manda desde
/// a primeira versão do app: o significado de `"system"` mudou aqui, não no site.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioArg {
    System,
    App(u32),
}

#[derive(Debug, Default, Deserialize)]
pub struct StartOptions {
    audio: Option<AudioArg>,
    /// Processo do jogo a vigiar.
    watch: Option<u32>,
}

#[derive(Serialize)]
pub struct Accepted {
    id: u64,
    answer: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// A fonte capturada ainda existe.
    capturing: bool,
    /// O jogo vigiado está na frente; `null` se não se sabe ou não há jogo vigiado.
    foreground: Option<bool>,
    /// O processo do jogo terminou.
    ended: bool,
    viewers: usize,
    encoder: String,
    target_bitrate: u32,
    /// Quanto o codificador produziu de verdade no último segundo, em bits por segundo.
    produced_bitrate: u32,
    audio: bool,
}

/// Uma oferta WebRTC legítima tem poucos KB; acima disto nem tenta interpretar.
const MAX_OFFER: usize = 256 * 1024;

/// Começa (ou troca) a captura. Quem já assiste continua conectado.
#[tauri::command]
pub async fn media_start(
    _caller: FromApp,
    app: AppHandle,
    source: Option<SourceArg>,
    quality: QualityArg,
    options: Option<StartOptions>,
) -> Reply<()> {
    let source = match source {
        None => Source::PrimaryMonitor,
        Some(SourceArg::Monitor(index)) => Source::Monitor(index),
        Some(SourceArg::Window(hwnd)) => Source::Window(hwnd),
    };
    // Janela minimizada não entrega imagem: volta para a tela antes de capturar, como o app antigo.
    if let Source::Window(hwnd) = source
        && nimbo_native::window_info(hwnd).is_ok_and(|info| info.minimized)
    {
        nimbo_native::restore(hwnd);
        let _ = tauri::async_runtime::spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(350))
        })
        .await;
    }
    let quality = quality.checked();
    let options = options.unwrap_or_default();
    // Criar a captura fala com o Windows e pode demorar: fora da thread da interface.
    let started = tauri::async_runtime::spawn_blocking(move || {
        let media = app.state::<Media>();
        media
            .engine
            .start(source, quality)
            .map_err(|e| e.to_string())?;
        // O som é opcional: se a captura de áudio falhar, a live segue só com a imagem.
        let audio = options.audio.map(|audio| match audio {
            // Tudo menos o próprio app (e o navegador embutido, que é filho dele), para as lives
            // que a pessoa está assistindo não voltarem na live dela, e menos os apps de chamada,
            // para quem está na chamada com ela não se ouvir.
            AudioArg::System => AudioSource::SystemExcept {
                own: std::process::id(),
            },
            AudioArg::App(pid) => AudioSource::Include(pid),
        });
        if let Err(e) = media.engine.set_audio(audio) {
            eprintln!("[nimbo] som da live indisponível: {e}");
        }
        media.follow(options.watch);
        Ok::<(), String>(())
    })
    .await;
    match started {
        Ok(Ok(())) => Reply::done(),
        Ok(Err(e)) => Reply::error(format!("Não foi possível capturar: {e}")),
        Err(_) => Reply::error("Não foi possível capturar."),
    }
}

impl Media {
    /// Troca o jogo vigiado (ou para de vigiar, com `None`).
    fn follow(&self, pid: Option<u32>) {
        let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        // A vigia anterior termina antes: os avisos dela não podem cair no jogo novo.
        drop(watch.take());
        self.game.reset();
        let Some(pid) = pid else { return };
        let game = self.game.clone();
        *watch = nimbo_native::watch(WatchTarget::Process(pid), move |event| match event {
            WatchEvent::Foreground(front) => {
                game.foreground.store(i8::from(front), Ordering::Relaxed)
            }
            WatchEvent::Exited => game.exited.store(true, Ordering::Relaxed),
        })
        .ok();
    }
}

/// Entrega ao motor a oferta de um espectador e devolve a resposta dele.
#[tauri::command]
pub async fn media_accept(_caller: FromApp, app: AppHandle, offer: String) -> Reply<Accepted> {
    if offer.len() > MAX_OFFER {
        return Reply::error("Oferta inválida.");
    }
    // Descobrir o endereço público (STUN) pode levar até ~1 s: fora da thread da interface.
    let accepted = tauri::async_runtime::spawn_blocking(move || {
        app.state::<Media>()
            .engine
            .accept(offer.as_bytes())
            .map_err(|e| e.to_string())
    })
    .await;
    match accepted {
        Ok(Ok((id, answer))) => match String::from_utf8(answer) {
            Ok(answer) => Reply::ok(Accepted { id, answer }),
            Err(_) => Reply::error("Resposta inválida do motor de vídeo."),
        },
        Ok(Err(e)) => Reply::error(format!("Não foi possível conectar o espectador: {e}")),
        Err(_) => Reply::error("Não foi possível conectar o espectador."),
    }
}

/// Encerra a conexão de um espectador.
#[tauri::command]
pub async fn media_close(_caller: FromApp, app: AppHandle, id: u64) -> Reply<()> {
    app.state::<Media>().engine.close(id);
    Reply::done()
}

/// Para a captura e desconecta todo mundo.
#[tauri::command]
pub async fn media_stop(_caller: FromApp, app: AppHandle) -> Reply<()> {
    let stopped = tauri::async_runtime::spawn_blocking(move || {
        let media = app.state::<Media>();
        media.follow(None);
        media.engine.stop();
    })
    .await;
    match stopped {
        Ok(()) => Reply::done(),
        Err(_) => Reply::error("Não foi possível parar a captura."),
    }
}

/// Pausa ou retoma a live: pausada, quem assiste vê uma tela escura no lugar da captura.
#[tauri::command]
pub async fn media_pause(_caller: FromApp, app: AppHandle, paused: bool) -> Reply<()> {
    app.state::<Media>().engine.set_paused(paused);
    Reply::done()
}

/// Como está a captura e o jogo vigiado. A página consulta de tempos em tempos.
#[tauri::command]
pub async fn media_state(_caller: FromApp, app: AppHandle) -> Reply<State> {
    let media = app.state::<Media>();
    let stats = media.engine.stats();
    Reply::ok(State {
        capturing: stats.capturing,
        foreground: match media.game.foreground.load(Ordering::Relaxed) {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        ended: media.game.exited.load(Ordering::Relaxed),
        viewers: stats.viewers,
        encoder: stats.encoder,
        target_bitrate: stats.target_bitrate,
        produced_bitrate: stats.produced_bitrate,
        audio: stats.audio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_follows_the_shape_the_site_sends() {
        let monitor: SourceArg = serde_json::from_str(r#"{"monitor":2}"#).unwrap();
        assert!(matches!(monitor, SourceArg::Monitor(2)));
        let window: SourceArg = serde_json::from_str(r#"{"window":65852}"#).unwrap();
        assert!(matches!(window, SourceArg::Window(65852)));
        let none: Option<SourceArg> = serde_json::from_str("null").unwrap();
        assert!(none.is_none());
        assert!(serde_json::from_str::<SourceArg>(r#"{"file":"c:/x"}"#).is_err());
    }

    #[test]
    fn quality_is_clamped_to_what_the_encoder_supports() {
        let wild: QualityArg =
            serde_json::from_str(r#"{"fps":1000,"bitrate":1,"maxHeight":99999}"#).unwrap();
        let q = wild.checked();
        assert_eq!((q.fps, q.bitrate, q.max_height), (60, 500_000, 2160));
        let normal: QualityArg =
            serde_json::from_str(r#"{"fps":60,"bitrate":6000000,"maxHeight":1080}"#).unwrap();
        let q = normal.checked();
        assert_eq!((q.fps, q.bitrate, q.max_height), (60, 6_000_000, 1080));
    }

    #[test]
    fn start_options_follow_the_shape_the_site_sends() {
        let system: StartOptions = serde_json::from_str(r#"{"audio":"system"}"#).unwrap();
        assert!(matches!(system.audio, Some(AudioArg::System)));
        assert!(system.watch.is_none());
        let app: StartOptions =
            serde_json::from_str(r#"{"audio":{"app":4242},"watch":4242}"#).unwrap();
        assert!(matches!(app.audio, Some(AudioArg::App(4242))));
        assert_eq!(app.watch, Some(4242));
        let silent: StartOptions = serde_json::from_str("{}").unwrap();
        assert!(silent.audio.is_none());
    }

    #[test]
    fn game_state_starts_unknown() {
        let game = Game::default();
        game.reset();
        assert_eq!(game.foreground.load(Ordering::Relaxed), -1);
        assert!(!game.exited.load(Ordering::Relaxed));
    }
}
