//! Áudio do jogo: captura o som do Windows por processo (WASAPI process loopback, Windows 10
//! 2004+), no lugar do auxiliar em C# (`desktop/native/NimboAudio.cs`).
//!
//! Três modos: [`Mode::Include`] pega só um processo e seus filhos (o jogo); [`Mode::Exclude`] pega
//! tudo menos um processo e seus filhos; [`Mode::SystemExcept`] pega tudo menos o próprio Nimbo e
//! os apps de chamada de voz ([`CALL_APPS`]), com os filhos deles. Os dois primeiros são um fluxo
//! do Windows cada; o terceiro soma vários (ver `system.rs`).
//!
//! O formato é o mesmo do auxiliar: PCM de 16 bits, little-endian, 48 kHz, estéreo intercalado.
//!
//! Diferente do auxiliar, o fluxo é contínuo. O Windows para de entregar pacotes em alguns
//! momentos (medido: 30 a 50 ms cada vez que o alvo começa ou para de tocar; e, pela documentação
//! da API, enquanto nada toca no computador). O auxiliar simplesmente pulava esse tempo; aqui ele
//! vira silêncio, preenchido pelo relógio (ver `timeline.rs`). Assim o tempo de um trecho é só a
//! posição dele: `quadro / 48000` segundos desde o início.
//!
//! O silêncio que vem do Windows não é zero puro: a conversão para 16 bits deixa um ruído de ±1
//! (igual no auxiliar). O silêncio preenchido aqui é zero.

mod calls;
mod mix;
mod procs;
mod system;
mod timeline;

use std::fmt;
use std::mem::ManuallyDrop;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, PROCESS_LOOPBACK_MODE,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVE_FORMAT_PCM, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{BLOB, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, Interface, Ref, implement, w};

use timeline::Timeline;

pub use calls::{CALL_APPS, is_call_app};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const BITS_PER_SAMPLE: u16 = 16;
/// Bytes de um quadro (uma amostra de cada canal).
pub const FRAME_BYTES: usize = 4;

/// Fila pedida ao Windows, em unidades de 100 ns (20 ms, igual ao auxiliar).
const BUFFER_HNS: i64 = 200_000;
/// Mesmo sem aviso do Windows a thread acorda neste intervalo, para o silêncio andar.
const TICK_MS: u32 = 10;
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Bloco de zeros reaproveitado para entregar silêncio (100 ms).
static SILENCE: [u8; SAMPLE_RATE as usize / 10 * FRAME_BYTES] =
    [0; SAMPLE_RATE as usize / 10 * FRAME_BYTES];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Só o som deste processo e dos filhos dele.
    Include(u32),
    /// Todo o som do computador, menos o deste processo e dos filhos dele.
    Exclude(u32),
    /// Todo o som do computador, menos o de `own` (o próprio Nimbo; o WebView2 é filho dele) e o
    /// dos apps de chamada de voz ([`CALL_APPS`]), sempre com os filhos. É o "Computador" da live:
    /// quem está numa chamada com você não se ouve nela.
    ///
    /// O Windows só tira uma árvore de processos por fluxo; aqui cada programa que está tocando
    /// vira um fluxo [`Mode::Include`] e os fluxos são somados. Um programa que começa a tocar
    /// depois entra em menos de um segundo; o que passa 30 s sem tocar sai (e volta quando tocar).
    /// Diferenças para o [`Mode::Exclude`]: os sons do sistema (a sessão "Sons do sistema") não
    /// entram, e um programa que tenha o Nimbo ou um app de chamada entre os descendentes também
    /// não (o fluxo dele levaria junto o som bloqueado).
    ///
    /// `own` deve ser o processo que captura (`std::process::id()`) ou um ancestral dele: medido,
    /// a própria captura cria uma sessão de áudio ativa nesse processo, e sem estar excluído ele
    /// entraria como mais um programa tocando, com todos os filhos.
    SystemExcept { own: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Uma chamada do Windows falhou: `step` diz qual e `code` é o HRESULT.
    Windows { step: &'static str, code: i32 },
    /// O Windows não respondeu ao pedido de ativação.
    ActivationTimeout,
    /// A thread de captura não pôde ser criada ou caiu (pânico dentro do `on_pcm`).
    Thread,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows { step, code } => write!(f, "{step} 0x{code:08X}"),
            Self::ActivationTimeout => f.write_str("ativação expirou"),
            Self::Thread => f.write_str("a thread de captura caiu"),
        }
    }
}

impl std::error::Error for Error {}

fn win(step: &'static str) -> impl Fn(windows::core::Error) -> Error {
    move |error| Error::Windows {
        step,
        code: error.code().0,
    }
}

/// O que a captura entregou até parar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Quadros entregues ao `on_pcm` (som + silêncio).
    pub frames: u64,
    /// Quantos deles foram silêncio preenchido aqui, porque o Windows não entregou nada.
    pub silence_frames: u64,
    /// Pacotes que o Windows marcou como descontínuos: faltou um pedaço do som (a fila estourou,
    /// por exemplo). O primeiro pacote de cada fluxo não conta.
    pub discontinuities: u64,
    /// Só no [`Mode::SystemExcept`]: fluxos de programas abertos (contando reaberturas).
    pub streams_opened: u64,
    /// Só no [`Mode::SystemExcept`]: quadros em que um programa que estava tocando ficou sem som na
    /// mistura (lacuna criada pela mistura, não pelo Windows).
    pub gap_frames: u64,
    /// Só no [`Mode::SystemExcept`]: quadros antigos descartados de programas que acabaram de
    /// entrar na mistura (o que o Windows guardou antes de o fluxo novo ser lido).
    pub trimmed_frames: u64,
    /// Quantas vezes a thread do som acordou para ler o Windows.
    pub wakes: u64,
    /// Só no [`Mode::SystemExcept`]: quantas vezes a lista de programas foi refeita e o tempo
    /// gasto nisso, em microssegundos.
    pub refreshes: u64,
    pub refresh_micros: u64,
}

struct Event(HANDLE);

// SAFETY: um evento do Windows é um objeto do núcleo; sinalizar, esperar e fechar pelo handle
// vale de qualquer thread.
unsafe impl Send for Event {}
// SAFETY: idem; as funções usadas aqui não exigem acesso exclusivo.
unsafe impl Sync for Event {}

impl Event {
    fn new(manual_reset: bool) -> Result<Self, Error> {
        // SAFETY: sem atributos de segurança e sem nome; os dois ponteiros podem ser nulos.
        unsafe { CreateEventW(None, manual_reset, false, None) }
            .map(Self)
            .map_err(win("CreateEvent"))
    }

    fn set(&self) {
        // SAFETY: o handle é válido enquanto `self` existir.
        let _ = unsafe { SetEvent(self.0) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: o handle veio do CreateEventW e é fechado uma única vez, aqui.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Uma captura em andamento. Para ao chamar [`Capture::stop`] ou ao ser descartada.
pub struct Capture {
    stop: Arc<Event>,
    thread: Option<JoinHandle<Result<Stats, Error>>>,
    sources: Arc<Mutex<Vec<u32>>>,
}

impl Capture {
    /// Começa a capturar e só volta quando o som já está correndo (ou com o erro).
    ///
    /// `on_pcm` roda na thread de captura e recebe um número inteiro de quadros por vez; precisa
    /// ser rápido (copiar, enfileirar), senão a fila do Windows estoura e o som falha.
    ///
    /// Um PID que não existe não é erro para o Windows: a captura começa e entrega silêncio. O
    /// mesmo vale quando o processo fecha no meio; quem decide o fim da live é o vigia do jogo.
    pub fn start(mode: Mode, on_pcm: impl FnMut(&[u8]) + Send + 'static) -> Result<Self, Error> {
        let stop = Arc::new(Event::new(true)?);
        let sources = Arc::new(Mutex::new(Vec::new()));
        let (ready, started) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        let thread_sources = Arc::clone(&sources);
        let thread = thread::Builder::new()
            .name("nimbo-audio".into())
            .spawn(move || match mode {
                Mode::Include(pid) => com(|| {
                    let tree = PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE;
                    session(pid, tree, &thread_stop, ready, on_pcm)
                }),
                Mode::Exclude(pid) => com(|| {
                    let tree = PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE;
                    session(pid, tree, &thread_stop, ready, on_pcm)
                }),
                Mode::SystemExcept { own } => {
                    com(|| system::run(own, &thread_stop, ready, &thread_sources, on_pcm))
                }
            })
            .map_err(|_| Error::Thread)?;
        match started.recv() {
            Ok(()) => Ok(Self {
                stop,
                thread: Some(thread),
                sources,
            }),
            // A thread saiu sem avisar que começou: o motivo é o que ela devolveu.
            Err(_) => Err(match thread.join() {
                Ok(Err(error)) => error,
                _ => Error::Thread,
            }),
        }
    }

    /// `false` quando a captura morreu sozinha (erro do Windows); [`Capture::stop`] diz por quê.
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    /// Processos cujo som está indo agora, um por fluxo (cada um leva junto os filhos). Só no
    /// [`Mode::SystemExcept`]; nos outros modos, vazio.
    pub fn sources(&self) -> Vec<u32> {
        self.sources
            .lock()
            .map(|sources| sources.clone())
            .unwrap_or_default()
    }

    /// Para a captura e espera a thread sair. Depois disso o `on_pcm` não é mais chamado.
    pub fn stop(mut self) -> Result<Stats, Error> {
        self.join()
    }

    fn join(&mut self) -> Result<Stats, Error> {
        self.stop.set();
        match self.thread.take() {
            Some(thread) => thread.join().unwrap_or(Err(Error::Thread)),
            None => Err(Error::Thread),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.join();
        }
    }
}

/// Roda `f` com o COM ligado nesta thread (multithread: os objetos de áudio passam de uma thread
/// da captura para outra sem empacotamento).
fn com<T>(f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    // SAFETY: primeira chamada COM desta thread, que é só nossa; o par é o CoUninitialize abaixo.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(win("CoInitializeEx"))?;
    let result = f();
    // SAFETY: par do CoInitializeEx acima; os objetos COM já foram soltos dentro de `f`.
    unsafe { CoUninitialize() };
    result
}

/// Avisa a thread de captura que o Windows terminou de ativar o cliente de áudio.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

fn activate(pid: u32, tree: PROCESS_LOOPBACK_MODE) -> Result<IAudioClient, Error> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: tree,
            },
        },
    };
    // Sem o ManuallyDrop, a PROPVARIANT tentaria liberar o blob ao sair, e ele mora na pilha.
    let mut variant = ManuallyDrop::new(PROPVARIANT::default());
    // SAFETY: PROPVARIANT recém-zerada; escrevemos o tipo e o campo que corresponde a ele. O blob
    // aponta para `params`, que vive até o fim desta função.
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&raw const params).cast_mut().cast(),
        };
    }
    let (done, completed) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(done).into();
    // SAFETY: o caminho é uma constante terminada em zero, o IID e a PROPVARIANT são referências
    // válidas durante a chamada e o Windows guarda a própria referência ao `handler`.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )
    }
    .map_err(win("ativação"))?;
    if completed.recv_timeout(ACTIVATION_TIMEOUT).is_err() {
        return Err(Error::ActivationTimeout);
    }
    let mut result = HRESULT(0);
    let mut client = None;
    // SAFETY: os dois ponteiros apontam para variáveis locais; a ativação já terminou.
    unsafe { operation.GetActivateResult(&mut result, &mut client) }.map_err(win("ativação"))?;
    result.ok().map_err(win("ativação"))?;
    client
        .ok_or(Error::Windows {
            step: "ativação",
            code: result.0,
        })?
        .cast()
        .map_err(win("ativação"))
}

/// Um pacote lido do Windows.
struct Packet<'a> {
    frames: u32,
    /// `None` quando o Windows marcou o pacote como silêncio.
    pcm: Option<&'a [u8]>,
    /// O Windows perdeu som antes deste pacote.
    discontinuity: bool,
}

/// Um fluxo de process loopback já correndo. Para ao ser descartado.
struct Stream {
    // A ordem importa: os campos são soltos nesta ordem, e o Windows usa o evento até o fim do
    // cliente.
    client: IAudioClient,
    capture: IAudioCaptureClient,
    _arrived: Arc<Event>,
    /// Ainda não chegou pacote (o primeiro costuma vir marcado como descontínuo).
    fresh: bool,
}

// SAFETY: os objetos de áudio do WASAPI são de modelo livre; as threads que usam um `Stream`
// (a que escolhe os programas e a que lê o som) estão as duas no COM multithread (`com`), onde
// um ponteiro de interface vale em qualquer thread sem empacotamento.
unsafe impl Send for Stream {}

impl Stream {
    /// Abre o fluxo do `pid` (incluindo ou excluindo a árvore dele) com uma fila de `buffer_hns`
    /// (unidades de 100 ns). O Windows sinaliza `arrived` a cada pacote.
    fn open(
        pid: u32,
        tree: PROCESS_LOOPBACK_MODE,
        buffer_hns: i64,
        arrived: &Arc<Event>,
    ) -> Result<Self, Error> {
        let client = activate(pid, tree)?;
        let block = CHANNELS * BITS_PER_SAMPLE / 8;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: BITS_PER_SAMPLE,
            cbSize: 0,
        };
        // O dispositivo virtual não tem formato próprio: o Windows converte para o que pedimos.
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // SAFETY: `format` é uma estrutura local válida durante a chamada.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                buffer_hns,
                0,
                &format,
                None,
            )
        }
        .map_err(win("Initialize"))?;
        // SAFETY: o evento fica guardado no `Stream` e só é solto depois do cliente.
        unsafe { client.SetEventHandle(arrived.0) }.map_err(win("SetEventHandle"))?;
        // SAFETY: chamada COM simples num cliente já inicializado.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.map_err(win("GetService"))?;
        // SAFETY: idem.
        unsafe { client.Start() }.map_err(win("Start"))?;
        Ok(Self {
            client,
            capture,
            _arrived: Arc::clone(arrived),
            fresh: true,
        })
    }

    /// Entrega ao `on_packet` todos os pacotes que já chegaram.
    fn drain(&mut self, mut on_packet: impl FnMut(Packet<'_>)) -> Result<(), Error> {
        loop {
            // SAFETY: chamada COM simples.
            let pending =
                unsafe { self.capture.GetNextPacketSize() }.map_err(win("GetNextPacketSize"))?;
            if pending == 0 {
                return Ok(());
            }
            let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0, 0);
            // SAFETY: os ponteiros apontam para variáveis locais.
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
            }
            .map_err(win("GetBuffer"))?;
            // Fila vazia apesar do aviso: nada a entregar, e não conta como pacote.
            if frames > 0 {
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                let discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
                on_packet(Packet {
                    frames,
                    // SAFETY: o Windows garante `frames` quadros em `data` até o ReleaseBuffer
                    // abaixo, e a fatia não sai do `on_packet`.
                    pcm: (!silent).then(|| unsafe {
                        std::slice::from_raw_parts(data, frames as usize * FRAME_BYTES)
                    }),
                    discontinuity: discontinuity && !self.fresh,
                });
                self.fresh = false;
            }
            // SAFETY: devolve exatamente o que o GetBuffer entregou.
            unsafe { self.capture.ReleaseBuffer(frames) }.map_err(win("ReleaseBuffer"))?;
            if frames == 0 {
                return Ok(());
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: chamada COM simples; se falhar não há o que fazer, o fluxo está acabando.
        let _ = unsafe { self.client.Stop() };
    }
}

/// Prioridade de áudio para a thread atual enquanto viver: um jogo usando todos os núcleos não
/// pode atrasar a captura.
struct AudioPriority(Option<HANDLE>);

impl AudioPriority {
    fn new() -> Self {
        let mut task = 0;
        // SAFETY: nome constante terminado em zero e ponteiro para variável local.
        Self(unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task) }.ok())
    }
}

impl Drop for AudioPriority {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            // SAFETY: handle devolvido pelo AvSetMmThreadCharacteristicsW nesta mesma thread (o
            // valor não sai da thread que o criou).
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

/// [`Mode::Include`] e [`Mode::Exclude`]: um fluxo só do Windows.
fn session(
    pid: u32,
    tree: PROCESS_LOOPBACK_MODE,
    stop: &Event,
    ready: mpsc::Sender<()>,
    mut on_pcm: impl FnMut(&[u8]),
) -> Result<Stats, Error> {
    let arrived = Arc::new(Event::new(false)?);
    let mut stream = Stream::open(pid, tree, BUFFER_HNS, &arrived)?;
    let _ = ready.send(());
    let priority = AudioPriority::new();
    let result = pump(&mut stream, stop, &arrived, &mut on_pcm);
    drop(priority);
    drop(stream);
    result
}

fn silence(frames: u64, on_pcm: &mut impl FnMut(&[u8])) {
    let mut bytes = frames as usize * FRAME_BYTES;
    while bytes > 0 {
        let chunk = bytes.min(SILENCE.len());
        on_pcm(&SILENCE[..chunk]);
        bytes -= chunk;
    }
}

fn pump(
    stream: &mut Stream,
    stop: &Event,
    arrived: &Event,
    on_pcm: &mut impl FnMut(&[u8]),
) -> Result<Stats, Error> {
    let handles = [stop.0, arrived.0];
    let started = Instant::now();
    let mut timeline = Timeline::default();
    let mut discontinuities = 0;
    let mut wakes = 0;
    loop {
        // SAFETY: os dois handles pertencem a eventos vivos durante toda a função.
        let wait = unsafe { WaitForMultipleObjects(&handles, false, TICK_MS) };
        if wait == WAIT_OBJECT_0 {
            break;
        }
        wakes += 1;
        if wait == WAIT_FAILED {
            return Err(win("WaitForMultipleObjects")(
                windows::core::Error::from_thread(),
            ));
        }
        silence(timeline.silence_due(started.elapsed()), on_pcm);
        stream.drain(|packet| {
            match packet.pcm {
                Some(pcm) => on_pcm(pcm),
                None => silence(u64::from(packet.frames), on_pcm),
            }
            discontinuities += u64::from(packet.discontinuity);
            timeline.packet(started.elapsed(), u64::from(packet.frames));
        })?;
    }
    Ok(Stats {
        frames: timeline.emitted,
        silence_frames: timeline.filled,
        discontinuities,
        wakes,
        ..Stats::default()
    })
}
