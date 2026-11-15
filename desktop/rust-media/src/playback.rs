//! Som de quem assiste: PCM (16 bits, 48 kHz, estéreo) → caixa de som padrão do Windows (WASAPI).
//!
//! Quem recebe a live empilha o som decodificado em `push`; uma thread própria entrega à placa de
//! som no ritmo dela. Uma pequena reserva (`PREBUFFER`) absorve o atraso variável da rede; se a
//! reserva seca, toca silêncio e junta de novo antes de voltar.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
    eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::AnyError;

pub(crate) const SAMPLE_RATE: u32 = 48_000;
pub(crate) const CHANNELS: usize = 2;
/// Amostras (dos dois canais) em um milissegundo.
const PER_MS: usize = SAMPLE_RATE as usize / 1000 * CHANNELS;
/// Quanto som juntar antes de começar (ou voltar) a tocar.
const PREBUFFER: usize = 60 * PER_MS;
/// Reserva acima disto é atraso acumulado: o mais antigo é jogado fora até voltar a `KEEP`.
const TOO_MUCH: usize = 300 * PER_MS;
const KEEP: usize = 100 * PER_MS;
/// Quanto som fica entregue à placa de cada vez, em quadros (30 ms): mais que isso só soma atraso.
const AHEAD: u32 = SAMPLE_RATE / 1000 * 30;
/// Tamanho pedido para o buffer da placa, em unidades de 100 ns (40 ms).
const DEVICE_BUFFER: i64 = 400_000;

struct Shared {
    queue: Mutex<VecDeque<i16>>,
    /// Volume de 0 a 1, guardado como os bits de um `f32`.
    volume: AtomicU32,
    closed: AtomicBool,
    /// Vezes em que a reserva secou com a live tocando.
    underruns: AtomicU64,
    /// Quadros de som (uma amostra de cada canal) entregues à placa, fora o silêncio.
    played: AtomicU64,
    /// A saída de som abriu? Sem caixa de som o som é só decodificado e descartado.
    open: AtomicBool,
    /// Tempo de CPU da thread que entrega o som à placa, em microssegundos.
    cpu_micros: AtomicU64,
}

pub(crate) struct Speaker {
    shared: Arc<Shared>,
}

impl Speaker {
    pub fn new(volume: f32) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            volume: AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()),
            closed: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            played: AtomicU64::new(0),
            open: AtomicBool::new(false),
            cpu_micros: AtomicU64::new(0),
        });
        let thread = shared.clone();
        std::thread::spawn(move || {
            if let Err(e) = render(&thread) {
                eprintln!("saída de som: {e}");
            }
            thread.open.store(false, Ordering::Relaxed);
        });
        Self { shared }
    }

    /// Empilha som decodificado (amostras intercaladas: esquerda, direita, ...).
    pub fn push(&self, pcm: &[i16]) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.extend(pcm);
        if queue.len() > TOO_MUCH {
            // Sempre um número par de amostras: descartar uma só trocaria os canais.
            let extra = (queue.len() - KEEP) & !1;
            queue.drain(..extra);
        }
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    pub fn played(&self) -> u64 {
        self.shared.played.load(Ordering::Relaxed)
    }

    pub fn is_open(&self) -> bool {
        self.shared.open.load(Ordering::Relaxed)
    }

    pub fn cpu(&self) -> std::time::Duration {
        std::time::Duration::from_micros(self.shared.cpu_micros.load(Ordering::Relaxed))
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

/// Fecha o evento da placa de som ao sair, por qualquer caminho.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: o identificador foi criado por CreateEventW e é fechado uma vez só.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn render(shared: &Shared) -> Result<(), AnyError> {
    // SAFETY: chamadas COM/WASAPI com ponteiros para variáveis locais vivas durante a chamada; o
    // buffer devolvido por GetBuffer vale até o ReleaseBuffer e tem o tamanho pedido.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * CHANNELS as u32 * 2,
            nBlockAlign: (CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // AUTOCONVERTPCM: o Windows converte para o formato da placa (44,1 kHz, 5.1, 32 bits...).
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            DEVICE_BUFFER,
            0,
            &format,
            None,
        )?;
        let event = Event(CreateEventW(None, false, false, None)?);
        client.SetEventHandle(event.0)?;
        let output: IAudioRenderClient = client.GetService()?;
        let size = client.GetBufferSize()?;
        client.Start()?;
        shared.open.store(true, Ordering::Relaxed);

        let mut playing = false;
        while !shared.closed.load(Ordering::Relaxed) {
            // A placa avisa a cada ~10 ms; o prazo é só para notar o fechamento.
            WaitForSingleObject(event.0, 200);
            shared
                .cpu_micros
                .store(crate::thread_cpu().as_micros() as u64, Ordering::Relaxed);
            let padding = client.GetCurrentPadding()?;
            let frames = (size - padding.min(size)).min(AHEAD.saturating_sub(padding));
            if frames == 0 {
                continue;
            }
            let wanted = frames as usize * CHANNELS;
            let buffer =
                std::slice::from_raw_parts_mut(output.GetBuffer(frames)?.cast::<i16>(), wanted);
            let volume = f32::from_bits(shared.volume.load(Ordering::Relaxed));
            let mut queue = shared.queue.lock().unwrap();
            if !playing && queue.len() >= PREBUFFER {
                playing = true;
            }
            let taken = if playing { wanted.min(queue.len()) } else { 0 };
            for (slot, sample) in buffer.iter_mut().zip(queue.drain(..taken)) {
                *slot = (f32::from(sample) * volume) as i16;
            }
            drop(queue);
            buffer[taken..].fill(0);
            if playing && taken < wanted {
                playing = false;
                shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            shared
                .played
                .fetch_add((taken / CHANNELS) as u64, Ordering::Relaxed);
            let silent = taken == 0 || volume == 0.0;
            output.ReleaseBuffer(
                frames,
                if silent {
                    AUDCLNT_BUFFERFLAGS_SILENT.0 as u32
                } else {
                    0
                },
            )?;
        }
        let _ = client.Stop();
    }
    Ok(())
}
