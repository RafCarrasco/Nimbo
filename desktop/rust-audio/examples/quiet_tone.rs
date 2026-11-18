//! Só para testar a captura sem fazer barulho: toca um tom que ninguém ouve e sai.
//!
//!   cargo run --release --example quiet_tone -- [segundos] [opções]
//!
//! Sem opções, o tom tem 20 Hz e 8 passos de 32768 de amplitude (-72 dBFS): fica dezenas de
//! decibéis abaixo do que o ouvido percebe nessa frequência, em qualquer volume, mas é som de
//! verdade para o Windows. Com `nimbo-audio include <pid>` ele aparece (pico perto de 0.00024); com
//! `exclude <pid>` some. Escreve `READY <pid> <qpc> <exe> <hz>` quando começa a tocar (`qpc` em unidades
//! de 100 ns do relógio de alta resolução do Windows, o mesmo para todos os processos).
//!
//! Opções (para a bancada do som do computador, `mix_bench`):
//!   --hz N          frequência, de 10 a 60 Hz (padrão 20)
//!   --dbfs N        nível, no máximo -60 dBFS (padrão -72)
//!   --after MS      espera antes de abrir o som (o processo existe, mas ainda não toca)
//!   --pause AT:LEN  aos AT ms de som, para o fluxo por LEN ms e volta (o Windows vê a sessão
//!                   ficar inativa e ativa de novo)
//!   --spawn EXE:HZ  abre um filho (`EXE`, cópia deste programa) tocando `HZ` com o mesmo nível
//!                   e a mesma duração
//!
//! Recusa nível acima de -60 dBFS e frequência fora de 10..=60 Hz: a regra é nenhum som audível.

use std::f64::consts::TAU;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_NOPERSIST,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const RATE: u32 = 48_000;
/// O nível mais alto aceito. Abaixo disso, nessas frequências, nada se ouve.
const MAX_DBFS: f64 = -60.0;

struct Options {
    seconds: u64,
    hertz: f64,
    dbfs: f64,
    after: Duration,
    pause: Option<(Duration, Duration)>,
    spawn: Option<(String, f64)>,
}

fn parse() -> Result<Options, String> {
    let mut options = Options {
        seconds: 5,
        hertz: 20.0,
        dbfs: -72.0,
        after: Duration::ZERO,
        pause: None,
        spawn: None,
    };
    let mut args = std::env::args().skip(1);
    let ms = |text: &str| {
        text.parse()
            .map(Duration::from_millis)
            .map_err(|_| text.to_string())
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("falta o valor de {arg}"));
        match arg.as_str() {
            "--hz" => options.hertz = value()?.parse().map_err(|_| "--hz")?,
            "--dbfs" => options.dbfs = value()?.parse().map_err(|_| "--dbfs")?,
            "--after" => options.after = ms(&value()?)?,
            "--pause" => {
                let text = value()?;
                let (at, len) = text.split_once(':').ok_or("--pause AT:LEN")?;
                options.pause = Some((ms(at)?, ms(len)?));
            }
            "--spawn" => {
                let text = value()?;
                let (exe, hertz) = text.rsplit_once(':').ok_or("--spawn EXE:HZ")?;
                options.spawn = Some((exe.into(), hertz.parse().map_err(|_| "--spawn HZ")?));
            }
            seconds => options.seconds = seconds.parse().map_err(|_| "segundos inválidos")?,
        }
    }
    if options.dbfs > MAX_DBFS || !options.dbfs.is_finite() {
        return Err(format!("nível acima de {MAX_DBFS} dBFS recusado"));
    }
    if !(10.0..=60.0).contains(&options.hertz) {
        return Err("frequência fora de 10..=60 Hz recusada".into());
    }
    Ok(options)
}

/// Agora, em unidades de 100 ns do relógio de alta resolução.
fn qpc() -> u64 {
    let (mut count, mut frequency) = (0, 1);
    // SAFETY: ponteiros para variáveis locais.
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (count as u128 * 10_000_000 / frequency as u128) as u64
}

fn spawn_child(options: &Options) -> Option<Child> {
    let (exe, hertz) = options.spawn.as_ref()?;
    let child = Command::new(exe)
        .arg(options.seconds.to_string())
        .args([
            "--hz",
            &hertz.to_string(),
            "--dbfs",
            &options.dbfs.to_string(),
        ])
        .spawn();
    match child {
        Ok(child) => Some(child),
        Err(error) => {
            eprintln!("ERROR filho: {error}");
            None
        }
    }
}

fn play(options: &Options) -> windows::core::Result<()> {
    thread::sleep(options.after);
    let amplitude = 32768.0 * 10f64.powf(options.dbfs / 20.0);
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 2,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 4,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
        | AUDCLNT_STREAMFLAGS_NOPERSIST;
    // SAFETY: sequência padrão do WASAPI numa thread só; `format` vive durante o Initialize e os
    // objetos COM são soltos antes de o processo sair.
    let (client, render, capacity) = unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        // Fila de 100 ms.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None)?;
        let render: IAudioRenderClient = client.GetService()?;
        let capacity = client.GetBufferSize()?;
        client.Start()?;
        (client, render, capacity)
    };
    let exe = std::env::current_exe().ok();
    let name = exe
        .as_ref()
        .and_then(|path| path.file_name())
        .unwrap_or_default();
    println!(
        "READY {} {} {} {}",
        std::process::id(),
        qpc(),
        name.display(),
        options.hertz
    );

    let started = Instant::now();
    let mut position = 0u64;
    let mut paused = false;
    while started.elapsed() < Duration::from_secs(options.seconds) {
        if let Some((at, len)) = options.pause {
            let now = started.elapsed();
            let inside = now >= at && now < at + len;
            if inside != paused {
                paused = inside;
                // SAFETY: chamadas COM simples. Ao voltar, o que sobrou na fila antes da pausa é
                // descartado.
                unsafe {
                    if paused {
                        client.Stop()?;
                        println!("PAUSE {}", qpc());
                    } else {
                        client.Reset()?;
                        client.Start()?;
                        println!("RESUME {}", qpc());
                    }
                }
            }
        }
        if !paused {
            // SAFETY: o GetBuffer entrega espaço para `free` quadros de 4 bytes, válido até o
            // ReleaseBuffer; a fatia não sai deste bloco.
            unsafe {
                let free = capacity - client.GetCurrentPadding()?;
                if free > 0 {
                    let data = render.GetBuffer(free)?;
                    let samples =
                        std::slice::from_raw_parts_mut(data.cast::<i16>(), free as usize * 2);
                    for frame in samples.as_chunks_mut::<2>().0 {
                        let phase = TAU * options.hertz * position as f64 / f64::from(RATE);
                        frame.fill((amplitude * phase.sin()).round() as i16);
                        position += 1;
                    }
                    render.ReleaseBuffer(free, 0)?;
                }
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: chamada COM simples.
    unsafe { client.Stop() }
}

fn main() {
    let options = match parse() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("ERROR {error}");
            std::process::exit(2);
        }
    };
    let child = spawn_child(&options);
    let result = play(&options);
    if let Some(mut child) = child {
        let _ = child.wait();
    }
    if let Err(error) = result {
        eprintln!("ERROR {error}");
        std::process::exit(1);
    }
}
