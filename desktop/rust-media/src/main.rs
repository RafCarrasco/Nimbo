//! Etapas 1 e 2 do plano (docs/rust-app.md): captura nativa da tela e codificação H.264 pela
//! placa de vídeo, com números. O modo serve envia por WebRTC; hw mede a codificação direto na GPU.
//!
//!   nimbo-media capture [segundos]            mede quadros por segundo e CPU só da captura
//!   nimbo-media encode  [segundos] [arquivo]  captura + codifica em H.264 para um .mp4
//!   nimbo-media serve   [porta] [exclude|include <pid>]
//!                                             envia a tela por WebRTC para navegadores; com
//!                                             `exclude`/`include`, envia também o som
//!   nimbo-media hw [s] [largura] [altura] [fps] codifica pela placa direto na GPU e mede
//!   nimbo-media watch <http://host:porta> [opções]
//!                                             assiste ao `serve` com o player em Rust (watch.rs)

mod hwbench;
mod serve;
mod watch;

use std::time::{Duration, Instant};

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use nimbo_media::engine::AudioSource;
pub(crate) use nimbo_media::{AnyError, hw, process_cpu};

struct Job {
    seconds: u64,
    /// Arquivo de saída; `None` mede só a captura.
    output: Option<String>,
    width: u32,
    height: u32,
}

struct Capture {
    job: Job,
    encoder: Option<VideoEncoder>,
    start: Instant,
    cpu_start: Duration,
    frames: u64,
    /// Tempo gasto dentro do envio ao codificador, para o custo por quadro.
    encode_time: Duration,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = Job;
    type Error = AnyError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let job = ctx.flags;
        let encoder = match &job.output {
            Some(path) => Some(VideoEncoder::new(
                VideoSettingsBuilder::new(job.width, job.height)
                    // H.264: é o que os navegadores de quem assiste recebem por WebRTC.
                    .sub_type(VideoSettingsSubType::H264)
                    .frame_rate(60)
                    .bitrate(8_000_000),
                AudioSettingsBuilder::default().disabled(true),
                ContainerSettingsBuilder::default(),
                path,
            )?),
            None => None,
        };
        Ok(Self {
            job,
            encoder,
            start: Instant::now(),
            cpu_start: process_cpu(),
            frames: 0,
            encode_time: Duration::ZERO,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        self.frames += 1;
        if let Some(encoder) = self.encoder.as_mut() {
            let t = Instant::now();
            encoder.send_frame(frame)?;
            self.encode_time += t.elapsed();
        }
        let elapsed = self.start.elapsed();
        if elapsed.as_secs() >= self.job.seconds {
            if let Some(encoder) = self.encoder.take() {
                encoder.finish()?;
            }
            let cpu = process_cpu().saturating_sub(self.cpu_start);
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
            let secs = elapsed.as_secs_f64();
            println!(
                "{{\"modo\":\"{}\",\"tamanho\":\"{}x{}\",\"segundos\":{:.1},\"quadros\":{},\"fps\":{:.1},\"cpu_um_nucleo_pct\":{:.1},\"cpu_total_pct\":{:.2},\"ms_por_quadro\":{:.3}}}",
                if self.job.output.is_some() {
                    "encode"
                } else {
                    "capture"
                },
                self.job.width,
                self.job.height,
                secs,
                self.frames,
                self.frames as f64 / secs,
                cpu.as_secs_f64() / secs * 100.0,
                cpu.as_secs_f64() / secs * 100.0 / cores,
                self.encode_time.as_secs_f64() * 1000.0 / self.frames.max(1) as f64,
            );
            control.stop();
        }
        Ok(())
    }
}

fn main() -> Result<(), AnyError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("capture");
    let seconds = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    if mode == "hw" {
        return hwbench::run(&args);
    }
    if mode == "watch" {
        return watch::run(&args);
    }
    if mode == "serve" {
        let port = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3070);
        let pid = args.get(3).and_then(|s| s.parse().ok());
        let audio = match (args.get(2).map(String::as_str), pid) {
            (None, _) => None,
            (Some("exclude"), Some(pid)) => Some(AudioSource::Exclude(pid)),
            (Some("include"), Some(pid)) => Some(AudioSource::Include(pid)),
            _ => return Err("áudio: use `exclude <pid>` ou `include <pid>`".into()),
        };
        return serve::run(port, audio);
    }
    let output = match mode {
        "capture" => None,
        "encode" => Some(
            args.get(2)
                .cloned()
                .unwrap_or_else(|| "nimbo-media.mp4".into()),
        ),
        other => {
            return Err(format!(
                "modo desconhecido: {other} (use capture, encode, serve, hw ou watch)"
            )
            .into());
        }
    };
    let monitor = Monitor::primary()?;
    let job = Job {
        seconds,
        output,
        width: monitor.width()?,
        height: monitor.height()?,
    };
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::Default,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        // Até 60 quadros por segundo, como a live.
        MinimumUpdateIntervalSettings::Custom(Duration::from_micros(16_666)),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        job,
    );
    Capture::start(settings).map_err(|e| format!("captura falhou: {e}"))?;
    Ok(())
}
