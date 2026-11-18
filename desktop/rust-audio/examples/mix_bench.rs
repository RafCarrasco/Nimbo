//! Bancada do som do computador ([`Mode::SystemExcept`]): prova pela própria captura quem entra e
//! quem fica de fora, mede CPU, atraso e lacunas e compara com o modo antigo ([`Mode::Exclude`]).
//!
//!   cargo build --release --examples
//!   mix_bench exclusion                 quem entra e quem fica de fora
//!   mix_bench late [rodadas]            quanto demora a entrar quem começa a tocar depois
//!   mix_bench load <tocadores> [s]      CPU, atraso e lacunas com 1, 3, 6... programas tocando
//!   mix_bench latency                   atraso do som (o tocador para e volta) nos dois modos
//!
//! Os tocadores são cópias do exemplo `quiet_tone` com nomes diferentes (`Discord.exe`,
//! `Jogo.exe`...) na pasta `mixbench` ao lado deste programa, cada um num tom próprio entre 20 e
//! 60 Hz a -70 dBFS: nada audível. A captura fica só na memória; nada é gravado além das cópias.
//! O resultado sai em JSON, uma linha por medida.
//!
//! Medido em 17/11/2026: com as cópias em `%TEMP%`, o Windows Defender pôs duas em quarentena
//! (`Trojan:Win32/Bearfoos.A!ml`, palpite do modelo para um executável novo sem assinatura com nome
//! de programa conhecido) e depois passou a barrar o próprio `quiet_tone.exe` pelo hash. Por isso
//! a pasta fica junto da compilação e os filhos têm nomes neutros (a regra de exclusão é pelo
//! parentesco, não pelo nome do filho). Se acontecer de novo, recompilar o `quiet_tone` muda o hash.
//!
//! Fora o `exclusion`, os tocadores nascem órfãos (`cmd /c start /b`, e o `cmd` sai na hora): se
//! fossem filhos desta bancada, a sessão de áudio que a própria captura cria aqui viraria um
//! programa "tocando" cujo fluxo levaria todos eles juntos, e a medida seria de um fluxo só.

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nimbo_audio::{Capture, Mode, SAMPLE_RATE, Stats};
use std::sync::OnceLock;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
};
use windows::Win32::System::WindowsProgramming::QueryProcessCycleTime;
use windows::core::PWSTR;

const DBFS: f64 = -70.0;
const RATE: usize = SAMPLE_RATE as usize;

fn qpc() -> u64 {
    let (mut count, mut frequency) = (0, 1);
    // SAFETY: ponteiros para variáveis locais.
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (count as u128 * 10_000_000 / frequency as u128) as u64
}

/// PID e nome de todos os processos.
fn processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    // SAFETY: foto dos processos; o handle é fechado no fim.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            out.push((
                entry.th32ProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// Ciclos de CPU gastos por todas as threads do processo. Medido em 17/11/2026: o GetProcessTimes
/// conta em fatias de 15,6 ms e cobra a fatia inteira de quem está rodando quando o relógio do
/// sistema bate, e as threads de áudio acordam justamente com ele; os ciclos não têm esse viés.
fn cpu_of(handle: HANDLE) -> Option<u64> {
    let mut cycles = 0;
    // SAFETY: handle válido e ponteiro para variável local.
    unsafe { QueryProcessCycleTime(handle, &mut cycles) }.ok()?;
    Some(cycles)
}

/// Ciclos por segundo de uma thread ocupada (300 ms girando nesta thread; as outras dormem).
fn cycles_per_second() -> f64 {
    static RATE: OnceLock<f64> = OnceLock::new();
    *RATE.get_or_init(|| {
        let before = own_cpu();
        let started = Instant::now();
        let mut spin = 0u64;
        while started.elapsed() < Duration::from_millis(300) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        (own_cpu() - before) as f64 / started.elapsed().as_secs_f64()
    })
}

/// CPU do processo de áudio do Windows (`audiodg.exe`), se ele deixar ver.
struct AudioDg(Option<HANDLE>);

impl AudioDg {
    fn open() -> Self {
        let pid = processes()
            .into_iter()
            .find(|(_, name)| name.eq_ignore_ascii_case("audiodg.exe"))
            .map(|(pid, _)| pid);
        // SAFETY: só consulta; fechado no Drop.
        Self(pid.and_then(|pid| {
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()
        }))
    }

    fn cpu(&self) -> Option<u64> {
        self.0.and_then(cpu_of)
    }
}

impl Drop for AudioDg {
    fn drop(&mut self) {
        if let Some(handle) = self.0 {
            // SAFETY: aberto no `open`, fechado uma vez.
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

fn own_cpu() -> u64 {
    // SAFETY: pseudo-handle do próprio processo, não precisa fechar.
    cpu_of(unsafe { GetCurrentProcess() }).unwrap_or(0)
}

/// Encerra o processo `pid` só se o executável dele estiver dentro de `dir` (um tocador desta
/// bancada): um PID reaproveitado por outro programa não é tocado.
fn end_player(pid: u32, dir: &Path) {
    // SAFETY: handle aberto e fechado aqui; o buffer do nome é local.
    unsafe {
        let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        let Ok(handle) = OpenProcess(access, false, pid) else {
            return;
        };
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let named = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize]));
        if named.is_ok() && path.parent() == Some(dir) {
            let _ = TerminateProcess(handle, 0);
        }
        let _ = CloseHandle(handle);
    }
}

/// Um tocador começou: `READY <pid> <qpc> <exe> <hz>`.
#[derive(Clone, Debug)]
struct Ready {
    pid: u32,
    qpc: u64,
    name: String,
}

struct Players {
    dir: PathBuf,
    children: Vec<Child>,
    ready: Receiver<Ready>,
    sender: mpsc::Sender<Ready>,
    seen: Vec<Ready>,
    /// `PAUSE <qpc>` e `RESUME <qpc>` dos tocadores.
    events: Arc<Mutex<Vec<(String, u64)>>>,
}

impl Players {
    fn new() -> Self {
        let dir = std::env::current_exe()
            .expect("caminho do mix_bench")
            .with_file_name("mixbench");
        let _ = std::fs::create_dir_all(&dir);
        let (sender, ready) = mpsc::channel();
        Self {
            dir,
            children: Vec::new(),
            ready,
            sender,
            seen: Vec::new(),
            events: Arc::default(),
        }
    }

    /// Copia o `quiet_tone` para `<pasta>/<name>` (se ainda não estiver lá, ou se mudou) e devolve
    /// o caminho.
    fn exe(&self, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        let tone = std::env::current_exe()
            .expect("caminho do mix_bench")
            .with_file_name("quiet_tone.exe");
        let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).ok();
        if size(&path) != size(&tone) {
            std::fs::copy(&tone, &path).expect("copiar quiet_tone.exe");
        }
        path
    }

    /// Abre `<name>` tocando `hertz` por `seconds`, com as opções extras do `quiet_tone`. Órfão:
    /// nasce de um `cmd` que sai na hora, então não é filho desta bancada.
    fn play(&mut self, name: &str, hertz: u32, seconds: u64, extra: &[String], orphan: bool) {
        let exe = self.exe(name);
        let mut command = if orphan {
            let mut command = Command::new("cmd");
            command.args(["/c", "start", "", "/b"]).arg(&exe);
            command
        } else {
            Command::new(&exe)
        };
        let mut child = command
            .arg(seconds.to_string())
            .args(["--hz", &hertz.to_string(), "--dbfs", &DBFS.to_string()])
            .args(extra)
            .stdout(Stdio::piped())
            .spawn()
            .expect("abrir tocador");
        let stdout = child.stdout.take().expect("saída do tocador");
        let sender = self.sender.clone();
        let events = Arc::clone(&self.events);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                match parts[..] {
                    ["READY", pid, qpc, name, _hertz] => {
                        let _ = sender.send(Ready {
                            pid: pid.parse().unwrap_or(0),
                            qpc: qpc.parse().unwrap_or(0),
                            name: name.into(),
                        });
                    }
                    [kind @ ("PAUSE" | "RESUME"), qpc] => {
                        let qpc = qpc.parse().unwrap_or(0);
                        events.lock().unwrap().push((kind.into(), qpc));
                    }
                    _ => {}
                }
            }
        });
        self.children.push(child);
    }

    /// Espera `count` tocadores (no total) avisarem que começaram.
    fn wait(&mut self, count: usize, limit: Duration) {
        let until = Instant::now() + limit;
        while self.seen.len() < count {
            let left = until.saturating_duration_since(Instant::now());
            match self.ready.recv_timeout(left) {
                Ok(ready) => self.seen.push(ready),
                Err(_) => break,
            }
        }
    }

    fn pid(&self, name: &str, nth: usize) -> Option<u32> {
        self.seen
            .iter()
            .filter(|ready| ready.name.eq_ignore_ascii_case(name))
            .nth(nth)
            .map(|ready| ready.pid)
    }
}

impl Drop for Players {
    fn drop(&mut self) {
        // Pelo PID de cada tocador que avisou que começou, conferindo que é mesmo um tocador.
        for ready in self.ready.try_iter().collect::<Vec<_>>() {
            self.seen.push(ready);
        }
        for ready in &self.seen {
            end_player(ready.pid, &self.dir);
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// O que a captura entregou: o canal esquerdo e, para cada entrega, até que quadro foi e quando.
#[derive(Default)]
struct Recording {
    left: Vec<i16>,
    marks: Vec<(u64, u64)>,
    /// Quando a captura foi pedida (qpc).
    asked: u64,
}

impl Recording {
    /// Quando (qpc) o quadro `frame` saiu da captura.
    fn qpc_of(&self, frame: u64) -> Option<u64> {
        let at = self.marks.partition_point(|&(end, _)| end < frame);
        self.marks.get(at).map(|&(_, qpc)| qpc)
    }
}

struct Run {
    recording: Recording,
    stats: Stats,
    seconds: f64,
    own_cpu: u64,
    audiodg_cpu: Option<u64>,
    /// Quando cada processo apareceu em `Capture::sources` (qpc).
    appeared: HashMap<u32, u64>,
    sources: Vec<u32>,
}

fn record(mode: Mode, seconds: f64, watch: bool) -> Run {
    let recording = Arc::new(Mutex::new(Recording {
        asked: qpc(),
        ..Recording::default()
    }));
    let writer = Arc::clone(&recording);
    let mut frames = 0u64;
    let audiodg = AudioDg::open();
    let (cpu_before, dg_before) = (own_cpu(), audiodg.cpu());
    let started = Instant::now();
    let capture = Capture::start(mode, move |pcm| {
        let now = qpc();
        let mut recording = writer.lock().unwrap();
        recording.left.extend(
            pcm.as_chunks::<4>()
                .0
                .iter()
                .map(|f| i16::from_le_bytes([f[0], f[1]])),
        );
        frames += (pcm.len() / 4) as u64;
        recording.marks.push((frames, now));
    })
    .expect("captura");
    let mut appeared = HashMap::new();
    let limit = Duration::from_secs_f64(seconds);
    while started.elapsed() < limit {
        if watch {
            let now = qpc();
            for pid in capture.sources() {
                appeared.entry(pid).or_insert(now);
            }
            thread::sleep(Duration::from_millis(20));
        } else {
            thread::sleep(limit.saturating_sub(started.elapsed()));
        }
    }
    let sources = capture.sources();
    let stats = capture.stop().expect("parar a captura");
    let elapsed = started.elapsed().as_secs_f64();
    let (cpu_after, dg_after) = (own_cpu(), audiodg.cpu());
    let recording = Arc::try_unwrap(recording)
        .ok()
        .expect("captura parada")
        .into_inner()
        .unwrap();
    Run {
        recording,
        stats,
        seconds: elapsed,
        own_cpu: cpu_after - cpu_before,
        audiodg_cpu: dg_before.zip(dg_after).map(|(a, b)| b - a),
        appeared,
        sources,
    }
}

/// Amplitude e fase do tom `hertz` em `x` (transformada num ponto só).
fn tone(x: &[i16], hertz: f64) -> (f64, f64) {
    let step = TAU * hertz / RATE as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &sample) in x.iter().enumerate() {
        let angle = step * n as f64;
        re += f64::from(sample) * angle.cos();
        im -= f64::from(sample) * angle.sin();
    }
    let n = x.len().max(1) as f64;
    (2.0 * (re * re + im * im).sqrt() / n, im.atan2(re))
}

fn expected_amplitude() -> f64 {
    32768.0 * 10f64.powf(DBFS / 20.0)
}

/// Amplitude média do tom entre `from` e `to` segundos, em janelas de 1 s, e o ruído logo ao lado
/// (2 Hz acima, uma frequência que nenhum tocador usa).
fn level(left: &[i16], hertz: f64, from: f64, to: f64) -> (f64, f64) {
    let (mut sum, mut noise, mut count) = (0.0, 0.0, 0.0f64);
    let mut start = (from * RATE as f64) as usize;
    let end = ((to * RATE as f64) as usize).min(left.len());
    while start + RATE <= end {
        let window = &left[start..start + RATE];
        sum += tone(window, hertz).0;
        noise += tone(window, hertz + 2.0).0;
        count += 1.0;
        start += RATE;
    }
    (sum / count.max(1.0), noise / count.max(1.0))
}

/// Primeiro quadro, depois de `from` segundos, em que o tom aparece e fica: janelas de 200 ms a
/// cada 10 ms acima de metade da amplitude por 100 ms seguidos. Um tom que começa no meio da janela
/// dá metade da amplitude, então o quadro devolvido é o meio da primeira dessas janelas.
fn onset(left: &[i16], hertz: f64, from: f64) -> Option<u64> {
    let window = RATE / 5;
    let step = RATE / 100;
    let half = expected_amplitude() / 2.0;
    let starts: Vec<usize> = ((from * RATE as f64) as usize..left.len().saturating_sub(window))
        .step_by(step)
        .collect();
    let above: Vec<bool> = starts
        .iter()
        .map(|&start| tone(&left[start..start + window], hertz).0 > half)
        .collect();
    (0..above.len().saturating_sub(10))
        .find(|&index| above[index..index + 10].iter().all(|&yes| yes))
        .map(|index| (starts[index] + window / 2) as u64)
}

/// Lacunas: em janelas de 200 ms (número inteiro de ciclos para múltiplos de 5 Hz) a fase de um
/// tom contínuo é a mesma em todas; um pedaço perdido ou um silêncio no meio muda a fase das
/// janelas seguintes, e a janela com a falha perde amplitude. Som dobrado (o mesmo programa em
/// dois fluxos) aparece como amplitude perto de 2. Devolve (janelas com falha em algum tom, maior
/// salto de fase em rad, menor e maior amplitude relativa, janelas medidas).
fn gaps(left: &[i16], tones: &[f64], from: f64, to: f64) -> (usize, f64, (f64, f64), usize) {
    let window = RATE / 5;
    let mut start = (from * RATE as f64) as usize;
    let end = ((to * RATE as f64) as usize).min(left.len());
    let expected = expected_amplitude();
    let mut last: Vec<Option<f64>> = vec![None; tones.len()];
    let (mut bad, mut worst_jump, mut windows) = (0, 0.0f64, 0);
    let (mut lowest, mut highest) = (f64::MAX, 0.0f64);
    while start + window <= end {
        let slice = &left[start..start + window];
        let mut broken = false;
        for (index, &hertz) in tones.iter().enumerate() {
            let (amplitude, phase) = tone(slice, hertz);
            let relative = amplitude / expected;
            lowest = lowest.min(relative);
            highest = highest.max(relative);
            if let Some(previous) = last[index] {
                let jump = (phase - previous + TAU / 2.0).rem_euclid(TAU) - TAU / 2.0;
                worst_jump = worst_jump.max(jump.abs());
                broken |= jump.abs() > 0.05;
            }
            broken |= !(0.8..1.25).contains(&relative);
            last[index] = Some(phase);
        }
        bad += usize::from(broken);
        windows += 1;
        start += window;
    }
    (bad, worst_jump, (lowest, highest), windows)
}

/// Atraso de cada entrega: quanto tempo depois do pedido da captura o trecho saiu, menos a duração
/// do som até ali. Devolve (mediana dos 2 primeiros segundos, mediana dos 2 últimos, p99 - p50
/// depois do primeiro segundo), em ms. Início e fim iguais = o som não escorrega em relação ao
/// relógio.
fn delivery(recording: &Recording) -> (f64, f64, f64) {
    let lags: Vec<(f64, f64)> = recording
        .marks
        .iter()
        .map(|&(end, at)| {
            let position = end as f64 / RATE as f64 * 1000.0;
            (
                position,
                (at - recording.asked) as f64 / 10_000.0 - position,
            )
        })
        .collect();
    let Some(&(length, _)) = lags.last() else {
        return (0.0, 0.0, 0.0);
    };
    let median = |mut values: Vec<f64>| -> f64 {
        values.sort_by(f64::total_cmp);
        values.get(values.len() / 2).copied().unwrap_or(0.0)
    };
    let pick = |from: f64, to: f64| -> Vec<f64> {
        lags.iter()
            .filter(|(position, _)| *position >= from && *position < to)
            .map(|&(_, lag)| lag)
            .collect()
    };
    let start = median(pick(0.0, 2000.0));
    let end = median(pick(length - 2000.0, f64::MAX));
    let mut steady = pick(1000.0, f64::MAX);
    steady.sort_by(f64::total_cmp);
    let at = |q: f64| steady[((steady.len() - 1) as f64 * q) as usize];
    let jitter = if steady.is_empty() {
        0.0
    } else {
        at(0.99) - at(0.5)
    };
    (start, end, jitter)
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Include(_) => "include",
        Mode::Exclude(_) => "exclude",
        Mode::SystemExcept { .. } => "system",
    }
}

/// Ciclos em `seconds` como porcentagem de um núcleo.
fn cpu_percent(cycles: u64, seconds: f64) -> f64 {
    cycles as f64 / cycles_per_second() / seconds * 100.0
}

/// O Nimbo de verdade, se estiver aberto: tirar o som dele das medidas, como o app faz.
fn real_nimbo() -> u32 {
    processes()
        .into_iter()
        .find(|(_, name)| name.eq_ignore_ascii_case("nimbo-app.exe"))
        .map_or(0, |(pid, _)| pid)
}

fn named(pids: &[u32]) -> String {
    let names: HashMap<u32, String> = processes().into_iter().collect();
    let list: Vec<String> = pids
        .iter()
        .map(|pid| format!("\"{pid}:{}\"", names.get(pid).map_or("?", String::as_str)))
        .collect();
    format!("[{}]", list.join(","))
}

fn exclusion() {
    let mut players = Players::new();
    let seconds = 30;
    let nimbo_child = players.exe("NimboFilho.exe");
    let teams_child = players.exe("TeamsFilho.exe");
    let discord = players.exe("Discord.exe");
    let spawn =
        |exe: &Path, hertz: u32| vec!["--spawn".into(), format!("{}:{hertz}", exe.display())];
    // (nome, tom, deve entrar?)
    let cases: [(&str, u32, bool); 9] = [
        ("Jogo.exe", 20, true),
        ("Discord.exe", 25, false),
        ("NimboStandIn.exe", 30, false),
        ("NimboFilho.exe (filho do Nimbo)", 35, false),
        ("ms-teams.exe", 60, false),
        ("TeamsFilho.exe (filho do ms-teams)", 40, false),
        ("Tardio.exe (começa 3 s depois)", 45, true),
        ("Launcher.exe (pai de um Discord)", 50, false),
        ("Discord.exe (filho do Launcher)", 55, false),
    ];
    // Aqui os tocadores são filhos da bancada de propósito: o NimboStandIn faz o papel do Nimbo
    // (`own`) e a bancada, pai de um Discord, não pode entrar.
    players.play("Jogo.exe", 20, seconds, &[], false);
    players.play("Discord.exe", 25, seconds, &[], false);
    players.play(
        "NimboStandIn.exe",
        30,
        seconds,
        &spawn(&nimbo_child, 35),
        false,
    );
    players.play("ms-teams.exe", 60, seconds, &spawn(&teams_child, 40), false);
    players.play("Launcher.exe", 50, seconds, &spawn(&discord, 55), false);
    players.wait(8, Duration::from_secs(5));
    // O tardio existe desde já, mas só começa a tocar 3 s depois de a captura começar.
    let late = vec!["--after".into(), "3500".into()];
    players.play("Tardio.exe", 45, seconds, &late, false);
    thread::sleep(Duration::from_millis(500));
    let own = players
        .pid("NimboStandIn.exe", 0)
        .expect("NimboStandIn tocando");

    for mode in [Mode::SystemExcept { own }, Mode::Exclude(own)] {
        let run = record(mode, 9.0, true);
        let left = &run.recording.left;
        let length = left.len() as f64 / RATE as f64;
        for (name, hertz, should) in cases {
            // O tardio só conta depois de começar; os outros, a captura toda menos o 1º segundo.
            let from = if hertz == 45 { 5.0 } else { 1.0 };
            let (amplitude, noise) = level(left, f64::from(hertz), from, length);
            let relative = amplitude / expected_amplitude();
            let inside = relative > 0.5;
            let expected = if matches!(mode, Mode::Exclude(_)) {
                // Modo antigo: tudo menos o Nimbo e o filho dele.
                hertz != 30 && hertz != 35
            } else {
                should
            };
            println!(
                "{{\"test\":\"exclusion\",\"mode\":\"{}\",\"program\":\"{name}\",\"hz\":{hertz},\
                 \"level\":{relative:.3},\"noise\":{:.4},\"inside\":{inside},\
                 \"expected\":{expected},\"ok\":{}}}",
                mode_name(mode),
                noise / expected_amplitude(),
                inside == expected
            );
        }
        println!(
            "{{\"test\":\"sources\",\"mode\":\"{}\",\"sources\":{},\"stats\":\"{:?}\"}}",
            mode_name(mode),
            named(&run.sources),
            run.stats
        );
    }
}

/// Um programa que começa a tocar com a captura já correndo: quanto tempo até aparecer em
/// `Capture::sources` e até o som dele sair da captura, contado do instante em que ele começou a
/// tocar. Repete com o início em fases diferentes do ciclo de escolha (500 ms).
fn late(rounds: usize) {
    let own = real_nimbo();
    for round in 0..rounds {
        for mode in [Mode::SystemExcept { own }, Mode::Exclude(own)] {
            let mut players = Players::new();
            let after = 1000 + (round as u64 * 130) % 520;
            let extra = ["--after".into(), after.to_string()];
            players.play("Tardio.exe", 45, 5, &extra, true);
            let run = record(mode, 2.5 + after as f64 / 1000.0, true);
            players.wait(1, Duration::from_secs(1));
            let Some(ready) = players.seen.first().cloned() else {
                println!("{{\"test\":\"late\",\"error\":\"o tocador não começou\"}}");
                continue;
            };
            let listed = run
                .appeared
                .get(&ready.pid)
                .map(|&at| (at as f64 - ready.qpc as f64) / 1e4);
            let heard = onset(&run.recording.left, 45.0, 0.2)
                .and_then(|frame| run.recording.qpc_of(frame))
                .map(|at| (at as f64 - ready.qpc as f64) / 1e4);
            let show = |value: Option<f64>| value.map_or("null".into(), |v| format!("{v:.0}"));
            println!(
                "{{\"test\":\"late\",\"mode\":\"{}\",\"after_ms\":{after},\"listed_ms\":{},\
                 \"heard_ms\":{}}}",
                mode_name(mode),
                show(listed),
                show(heard)
            );
        }
    }
}

fn load(count: usize, seconds: f64) {
    let mut players = Players::new();
    let tones: Vec<f64> = (0..count).map(|i| 20.0 + 5.0 * i as f64).collect();
    for (index, &hertz) in tones.iter().enumerate() {
        let name = format!("Programa{}.exe", index + 1);
        let length = (3.0 * seconds) as u64 + 10;
        players.play(&name, hertz as u32, length, &[], true);
    }
    players.wait(count, Duration::from_secs(5));
    thread::sleep(Duration::from_millis(500));
    let own = real_nimbo();

    // Base: os tocadores tocando, sem captura nenhuma.
    cycles_per_second();
    let audiodg = AudioDg::open();
    let before = audiodg.cpu();
    let started = Instant::now();
    thread::sleep(Duration::from_secs_f64(seconds));
    let base = before
        .zip(audiodg.cpu())
        .map(|(a, b)| cpu_percent(b - a, started.elapsed().as_secs_f64()));
    println!(
        "{{\"test\":\"load\",\"players\":{count},\"mode\":\"none\",\"audiodg_cpu\":{}}}",
        base.map_or("null".into(), |v| format!("{v:.2}"))
    );

    for mode in [Mode::Exclude(own), Mode::SystemExcept { own }] {
        let run = record(mode, seconds, false);
        let left = &run.recording.left;
        let length = left.len() as f64 / RATE as f64;
        let (bad, jump, (lowest, highest), windows) = gaps(left, &tones, 1.0, length);
        let (lag_start, lag_end, jitter) = delivery(&run.recording);
        let refresh = if run.stats.refreshes > 0 {
            run.stats.refresh_micros as f64 / run.stats.refreshes as f64 / 1000.0
        } else {
            0.0
        };
        println!(
            "{{\"test\":\"load\",\"players\":{count},\"mode\":\"{}\",\"seconds\":{:.1},\
             \"capture_cpu\":{:.3},\"audiodg_cpu\":{},\"bad_windows\":{bad},\"windows\":{windows},\
             \"max_phase_jump\":{jump:.4},\"level\":[{lowest:.3},{highest:.3}],\
             \"lag_ms\":{{\"start\":{lag_start:.1},\"end\":{lag_end:.1},\"p99_p50\":{jitter:.1}}},\
             \"frames\":{},\"discontinuities\":{},\"gap_frames\":{},\"trimmed_frames\":{},\
             \"silence_frames\":{},\
             \"streams_opened\":{},\"wakes_per_s\":{:.0},\"refresh_ms\":{refresh:.2},\"sources\":{}}}",
            mode_name(mode),
            run.seconds,
            cpu_percent(run.own_cpu, run.seconds),
            run.audiodg_cpu.map_or("null".into(), |v| format!(
                "{:.2}",
                cpu_percent(v, run.seconds)
            )),
            run.stats.frames,
            run.stats.discontinuities,
            run.stats.gap_frames,
            run.stats.trimmed_frames,
            run.stats.silence_frames,
            run.stats.streams_opened,
            run.stats.wakes as f64 / run.seconds,
            named(&run.sources),
        );
    }
}

/// Bloco a bloco (25 ms, um ciclo de 40 Hz): amplitude do tom.
fn envelope(left: &[i16]) -> Vec<f64> {
    let block = RATE / 40;
    left.chunks_exact(block)
        .map(|chunk| tone(chunk, 40.0).0 / expected_amplitude())
        .collect()
}

/// O tocador para (Stop do fluxo dele) e volta: quanto tempo até isso aparecer na saída da
/// captura, nos dois modos.
fn latency() {
    let own = real_nimbo();
    for mode in [Mode::Exclude(own), Mode::SystemExcept { own }] {
        let mut players = Players::new();
        // Toca 40 Hz, para aos 2 s por 1 s e volta.
        let extra = ["--pause".into(), "2000:1000".into()];
        players.play("Pausa.exe", 40, 6, &extra, true);
        players.wait(1, Duration::from_secs(5));
        let run = record(mode, 5.0, false);
        let events: Vec<(&str, u64)> = players
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|(kind, at)| (if kind == "PAUSE" { "stop" } else { "resume" }, *at))
            .collect();
        let envelope = envelope(&run.recording.left);
        let block = (RATE / 40) as u64;
        let mut out = Vec::new();
        for (name, at) in events {
            // Primeiro bloco depois do evento que cruza a metade (para baixo ou para cima).
            let first = envelope.iter().enumerate().position(|(index, &level)| {
                let end = (index as u64 + 1) * block;
                let when = run.recording.qpc_of(end).unwrap_or(0);
                when > at
                    && if name == "stop" {
                        level < 0.5
                    } else {
                        level > 0.5
                    }
            });
            let delay = first
                .and_then(|index| run.recording.qpc_of((index as u64 + 1) * block))
                .map(|when| (when as f64 - at as f64) / 1e4);
            out.push(format!(
                "\"{name}_ms\":{}",
                delay.map_or("null".into(), |v| format!("{v:.1}"))
            ));
        }
        println!(
            "{{\"test\":\"latency\",\"mode\":\"{}\",{},\"stats\":\"{:?}\"}}",
            mode_name(mode),
            out.join(","),
            run.stats
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("exclusion") => exclusion(),
        Some("late") => late(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(4)),
        Some("load") => {
            let count = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
            let seconds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15.0);
            load(count, seconds);
        }
        Some("latency") => latency(),
        _ => {
            eprintln!("uso: mix_bench exclusion | late [rodadas] | load <tocadores> [s] | latency")
        }
    }
}
