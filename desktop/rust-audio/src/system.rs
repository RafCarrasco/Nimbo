//! Som do computador sem o Nimbo e sem os apps de chamada ([`crate::Mode::SystemExcept`]).
//!
//! O process loopback do Windows só inclui OU exclui uma árvore de processos por fluxo, e aqui são
//! várias a tirar (o Nimbo e cada app de chamada). Então o caminho é o inverso: um fluxo "só este
//! programa" para cada programa que está tocando e pode ir para a live, e a soma deles. Duas
//! threads:
//! - a de escolha (`choose`) olha a cada [`REFRESH`] quem tem sessão de áudio ativa (em todas as
//!   saídas de som), decide quem entra (`procs.rs`) e abre ou fecha os fluxos. Abrir fala com o
//!   serviço de áudio do Windows e pode demorar, então fica longe do som;
//! - a do som (`pump`), que lê todos os fluxos, mistura (`mix.rs`) e entrega ao `on_pcm`.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{S_OK, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::System::Threading::{WaitForMultipleObjects, WaitForSingleObject};
use windows::core::Interface;

use crate::mix::Mixer;
use crate::procs::{Procs, Rules, roots};
use crate::{ACTIVATION_TIMEOUT, AudioPriority, Error, Event, Stats, Stream, TICK_MS, com, win};

/// De quanto em quanto tempo a lista de sessões ativas é lida (~0,4 ms, medido). Um programa que
/// começa a tocar entra em até isso, mais a foto dos processos e a abertura do fluxo (~3 ms cada).
const REFRESH: Duration = Duration::from_millis(500);
/// De quanto em quanto tempo a foto dos processos e a lista de saídas de som são refeitas mesmo
/// sem programa novo: pega o PID reaproveitado e o programa aberto que ganhou um filho bloqueado.
const FULL: Duration = Duration::from_secs(5);
/// Um programa que passa este tempo sem sessão ativa sai (o fluxo dele custa um pouco mesmo
/// calado). Se voltar a tocar, entra de novo como novo.
const LINGER: Duration = Duration::from_secs(30);
/// Depois de falhar ao abrir o fluxo de um programa, espera isto antes de tentar de novo.
const RETRY: Duration = Duration::from_secs(5);
/// Fila de cada fluxo no Windows (100 ms, em unidades de 100 ns). Maior que a do fluxo único: a
/// thread do som lê vários fluxos e pode se atrasar um pouco sem perder nada.
const BUFFER_HNS: i64 = 1_000_000;
/// Mais programas que isso ao mesmo tempo é desperdício; os excedentes ficam de fora.
const MAX_STREAMS: usize = 32;

enum Command {
    Add(u64, Stream),
    Remove(u64),
}

/// Contas da thread de escolha, somadas às da mistura no fim.
#[derive(Default)]
struct Chosen {
    streams_opened: u64,
    refreshes: u64,
    refresh_micros: u64,
}

pub(crate) fn run(
    own: u32,
    stop: &Arc<Event>,
    ready: mpsc::Sender<()>,
    sources: &Arc<Mutex<Vec<u32>>>,
    mut on_pcm: impl FnMut(&[u8]),
) -> Result<Stats, Error> {
    // Auto-reset e compartilhado por todos os fluxos: qualquer pacote acorda a thread do som, que
    // lê todos.
    let arrived = Arc::new(Event::new(false)?);
    let (commands, received) = mpsc::channel();
    let (dead, died) = mpsc::channel();
    let (first, first_done) = mpsc::channel();
    let chooser = {
        let (stop, arrived, sources) =
            (Arc::clone(stop), Arc::clone(&arrived), Arc::clone(sources));
        thread::Builder::new()
            .name("nimbo-audio-list".into())
            .spawn(move || {
                com(|| {
                    Ok(choose(
                        own, &stop, &arrived, &commands, &died, first, &sources,
                    ))
                })
            })
            .map_err(|_| Error::Thread)?
    };
    // Sem conseguir ler nem a lista de sessões não há som do computador: o erro volta para quem
    // pediu a captura. Os fluxos que deram certo antes do erro morrem com o canal.
    let started = match first_done.recv_timeout(ACTIVATION_TIMEOUT * 2) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(Error::Thread),
    };
    if let Err(error) = started {
        stop.set();
        let _ = chooser.join();
        return Err(error);
    }
    let _ = ready.send(());
    let priority = AudioPriority::new();
    let result = pump(stop, &arrived, &received, &dead, &mut on_pcm);
    drop(priority);
    // A captura acabou (pedido ou erro): a escolha também para. Os fluxos ainda no canal são
    // soltos aqui, com o COM desta thread ligado.
    stop.set();
    let chosen = chooser.join().ok().and_then(Result::ok).unwrap_or_default();
    drop(received);
    result.map(|stats| Stats {
        streams_opened: chosen.streams_opened,
        refreshes: chosen.refreshes,
        refresh_micros: chosen.refresh_micros,
        ..stats
    })
}

fn pump(
    stop: &Event,
    arrived: &Event,
    commands: &Receiver<Command>,
    dead: &Sender<u64>,
    on_pcm: &mut impl FnMut(&[u8]),
) -> Result<Stats, Error> {
    let handles = [stop.0, arrived.0];
    let started = Instant::now();
    let mut mixer = Mixer::default();
    let mut streams: Vec<(u64, Stream)> = Vec::new();
    let mut discontinuities = 0;
    let mut wakes = 0;
    loop {
        // Acorda a cada pacote de qualquer fluxo (uma vez por fluxo a cada 10 ms). Medido com 7
        // fluxos: só pelo relógio (10 ms) acorda 8 vezes menos e gasta 1,1% de um núcleo em vez de
        // 1,9%, mas o som sai 10 ms mais tarde e o atraso treme 4,6 ms em vez de 1 ms.
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
        for command in commands.try_iter() {
            match command {
                Command::Add(id, stream) => {
                    mixer.add(id);
                    streams.push((id, stream));
                }
                Command::Remove(id) => {
                    mixer.remove(id);
                    streams.retain(|(open, _)| *open != id);
                }
            }
        }
        let now = started.elapsed();
        streams.retain_mut(|(id, stream)| {
            let read = stream.drain(|packet| {
                discontinuities += u64::from(packet.discontinuity);
                mixer.packet(*id, now, packet.frames, packet.pcm);
            });
            // Um fluxo que falha (saída de som trocada ou desligada) sai; a escolha abre de novo.
            if read.is_err() {
                mixer.remove(*id);
                let _ = dead.send(*id);
            }
            read.is_ok()
        });
        mixer.flush(now, on_pcm);
    }
    let (frames, silence_frames) = mixer.emitted();
    Ok(Stats {
        frames,
        silence_frames,
        discontinuities,
        gap_frames: mixer.gap_frames,
        trimmed_frames: mixer.trimmed_frames,
        wakes,
        ..Stats::default()
    })
}

/// Um programa com fluxo aberto.
struct Open {
    id: u64,
    /// Para perceber o PID reaproveitado por outro processo.
    created: Option<u64>,
    /// Última vez que a sessão dele estava ativa.
    active: Instant,
}

struct Chooser {
    own: u32,
    devices: IMMDeviceEnumerator,
    /// Um gerenciador de sessões por saída de som. Pedir de novo a cada volta custa ~2 ms de CPU
    /// (medido); a lista de saídas é refeita a cada [`FULL`].
    outputs: Vec<IAudioSessionManager2>,
    outputs_at: Option<Instant>,
    /// Última foto dos processos. Custa ~3 ms de CPU (medido): só é tirada quando aparece um
    /// programa tocando que ainda não foi julgado, e a cada [`FULL`].
    procs: Procs,
    snapshot_at: Option<Instant>,
    open: HashMap<u32, Open>,
    /// Programas tocando que não podem entrar, segundo a última foto.
    rejected: HashSet<u32>,
    /// Programas cujo fluxo falhou, e quando tentar de novo.
    retry: HashMap<u32, Instant>,
    next_id: u64,
    chosen: Chosen,
}

fn choose(
    own: u32,
    stop: &Event,
    arrived: &Arc<Event>,
    commands: &Sender<Command>,
    dead: &Receiver<u64>,
    first: Sender<Result<(), Error>>,
    sources: &Mutex<Vec<u32>>,
) -> Chosen {
    // SAFETY: criação COM comum, nesta thread já ligada ao COM.
    let devices = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(win("MMDeviceEnumerator"));
    let devices = match devices {
        Ok(devices) => devices,
        Err(error) => {
            let _ = first.send(Err(error));
            return Chosen::default();
        }
    };
    let mut chooser = Chooser {
        own,
        devices,
        outputs: Vec::new(),
        outputs_at: None,
        procs: Procs::default(),
        snapshot_at: None,
        open: HashMap::new(),
        rejected: HashSet::new(),
        retry: HashMap::new(),
        next_id: 1,
        chosen: Chosen::default(),
    };
    let result = chooser.refresh(arrived, commands, sources);
    let failed = result.is_err();
    let _ = first.send(result);
    if failed {
        return chooser.chosen;
    }
    let wait = REFRESH.as_millis() as u32;
    // SAFETY: o evento vive durante toda a função.
    while unsafe { WaitForSingleObject(stop.0, wait) } == WAIT_TIMEOUT {
        for id in dead.try_iter() {
            chooser.forget(id);
        }
        // Erro passageiro (a lista de saídas mudou no meio): fica como está e tenta na próxima.
        let _ = chooser.refresh(arrived, commands, sources);
    }
    chooser.chosen
}

impl Chooser {
    /// O fluxo deste programa morreu na thread do som: some da lista e pode ser aberto de novo
    /// daqui a pouco. A saída de som pode ter mudado: a lista de saídas é refeita.
    fn forget(&mut self, id: u64) {
        let now = Instant::now();
        self.open.retain(|&pid, open| {
            let keep = open.id != id;
            if !keep {
                self.retry.insert(pid, now + REFRESH);
            }
            keep
        });
        self.outputs_at = None;
    }

    fn load_outputs(&mut self) -> Result<(), Error> {
        self.outputs.clear();
        // SAFETY: chamadas COM simples; os objetos ficam guardados em `self.outputs`.
        unsafe {
            let outputs = self
                .devices
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(win("EnumAudioEndpoints"))?;
            for index in 0..outputs.GetCount().map_err(win("GetCount"))? {
                // Uma saída desligada no meio da conta é só pulada.
                if let Ok(output) = outputs.Item(index)
                    && let Ok(manager) = output.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
                {
                    self.outputs.push(manager);
                }
            }
        }
        self.outputs_at = Some(Instant::now());
        Ok(())
    }

    /// PIDs com sessão de áudio ativa em alguma saída de som.
    fn active_sessions(&self) -> Result<HashSet<u32>, Error> {
        let mut active = HashSet::new();
        for manager in &self.outputs {
            // SAFETY: chamadas COM simples; os objetos são soltos ao sair do laço.
            unsafe {
                let sessions = manager
                    .GetSessionEnumerator()
                    .map_err(win("GetSessionEnumerator"))?;
                for index in 0..sessions.GetCount().unwrap_or(0) {
                    let Ok(session) = sessions.GetSession(index) else {
                        continue;
                    };
                    if !matches!(session.GetState(), Ok(state) if state == AudioSessionStateActive)
                    {
                        continue;
                    }
                    let Ok(session) = session.cast::<IAudioSessionControl2>() else {
                        continue;
                    };
                    // "Sons do sistema" é do processo 0: não dá para pegar só ela.
                    if session.IsSystemSoundsSession() == S_OK {
                        continue;
                    }
                    if let Ok(pid) = session.GetProcessId()
                        && pid != 0
                    {
                        active.insert(pid);
                    }
                }
            }
        }
        Ok(active)
    }

    fn refresh(
        &mut self,
        arrived: &Arc<Event>,
        commands: &Sender<Command>,
        sources: &Mutex<Vec<u32>>,
    ) -> Result<(), Error> {
        let started = Instant::now();
        let stale = |at: Option<Instant>| at.is_none_or(|at| started.duration_since(at) >= FULL);
        if stale(self.outputs_at) {
            self.load_outputs()?;
        }
        let active = match self.active_sessions() {
            Ok(active) => active,
            // A saída de som mudou: pega a lista nova e tenta mais uma vez.
            Err(_) => {
                self.load_outputs()?;
                self.active_sessions()?
            }
        };
        self.retry.retain(|_, until| *until > started);
        let undecided = active.iter().any(|pid| {
            !self.open.contains_key(pid)
                && !self.rejected.contains(pid)
                && !self.retry.contains_key(pid)
        });
        let now = Instant::now();
        for (pid, open) in &mut self.open {
            if active.contains(pid) {
                open.active = now;
            }
        }
        // Primeiro quem já está aberto (continua, se ainda vale), depois quem começou a tocar.
        let mut wanted: Vec<u32> = self
            .open
            .iter()
            .filter(|(_, open)| now.duration_since(open.active) < LINGER)
            .map(|(&pid, _)| pid)
            .collect();
        wanted.sort_unstable();
        if undecided || stale(self.snapshot_at) {
            self.procs = Procs::snapshot()?;
            self.snapshot_at = Some(now);
            let (procs, open) = (&self.procs, &self.open);
            let rules = Rules::new(procs, self.own);
            // Revalida quem está aberto: o processo fechou (ou o PID é de outro agora), ou ganhou
            // um descendente bloqueado (abriu o Discord, por exemplo).
            wanted.retain(|pid| procs.created(*pid) == open[pid].created && rules.allowed(*pid));
            self.rejected = active
                .iter()
                .copied()
                .filter(|&pid| !procs.contains(pid) || !rules.allowed(pid))
                .collect();
            let mut fresh: Vec<u32> = active
                .iter()
                .copied()
                .filter(|pid| !open.contains_key(pid) && !self.retry.contains_key(pid))
                .filter(|pid| !self.rejected.contains(pid))
                .collect();
            fresh.sort_unstable();
            wanted.extend(fresh);
            wanted = roots(procs, &wanted);
        }
        wanted.truncate(MAX_STREAMS);

        // Fecha antes de abrir: na troca de um filho pelo pai, um instante sem o som é melhor que
        // um instante com ele dobrado.
        self.open.retain(|pid, open| {
            let keep = wanted.contains(pid);
            if !keep {
                let _ = commands.send(Command::Remove(open.id));
            }
            keep
        });
        let tree = PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE;
        let mut opened = false;
        for pid in wanted {
            if self.open.contains_key(&pid) {
                continue;
            }
            match Stream::open(pid, tree, BUFFER_HNS, arrived) {
                Ok(stream) => {
                    let id = self.next_id;
                    self.next_id += 1;
                    let _ = commands.send(Command::Add(id, stream));
                    let created = self.procs.created(pid);
                    self.open.insert(
                        pid,
                        Open {
                            id,
                            created,
                            active: now,
                        },
                    );
                    self.chosen.streams_opened += 1;
                    opened = true;
                }
                Err(_) => {
                    self.retry.insert(pid, now + RETRY);
                }
            }
        }
        if opened {
            // Acorda a thread do som para ela pegar os fluxos novos já.
            arrived.set();
        }
        if let Ok(mut list) = sources.lock() {
            list.clear();
            list.extend(self.open.keys());
            list.sort_unstable();
        }
        self.chosen.refreshes += 1;
        self.chosen.refresh_micros += started.elapsed().as_micros() as u64;
        Ok(())
    }
}
