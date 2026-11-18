//! Mistura de vários fluxos "só este programa" num fluxo só, contínuo.
//!
//! Medido (process loopback, 48 kHz): enquanto o Windows está tocando alguma coisa, cada fluxo
//! entrega um pacote de 10 ms por ciclo do mixer do Windows, mesmo quando o programa dele está
//! calado (pacote de silêncio). Os fluxos andam juntos, no relógio da placa. O carimbo de tempo dos
//! pacotes não serve para alinhar: é só o início do fluxo mais a contagem de quadros, e fluxos
//! abertos em momentos diferentes têm carimbos defasados entre si.
//!
//! Então cada programa tem a sua fila, e sai da mistura o que todos os programas "vivos" (que
//! entregaram pacote há menos de [`IDLE`]) já entregaram: o atraso é só a diferença de chegada
//! entre os fluxos de um mesmo ciclo. Um programa que para de entregar deixa de segurar os outros
//! depois de [`IDLE`]. Um programa que entra (ou volta) começa na posição atual da saída; o que
//! entra fica só com o pacote mais recente: o que o Windows guardou antes de a mistura ler o fluxo
//! novo (abrir vários fluxos seguidos leva alguns milissegundos) deixaria esse programa atrasado em
//! relação aos outros. Quando ninguém entrega nada (nada toca no computador), o silêncio anda pelo
//! relógio, como no fluxo único (ver `timeline.rs`).

use std::collections::VecDeque;
use std::time::Duration;

use crate::timeline::{IDLE, Timeline};
use crate::{CHANNELS, SAMPLE_RATE};

const SAMPLES_PER_FRAME: usize = CHANNELS as usize;

/// Um programa vivo atrasado em relação a outro mais que isto (relógios diferentes, fluxo
/// engasgado) deixa de segurar a saída: ela anda até [`SKEW_TARGET`] atrás do mais adiantado e o
/// atrasado ganha um trecho de silêncio. Normalmente a diferença é de no máximo um pacote (10 ms).
const MAX_SKEW: usize = SAMPLE_RATE as usize / 1000 * 80;
const SKEW_TARGET: usize = SAMPLE_RATE as usize / 1000 * 20;

struct Track {
    id: u64,
    /// Amostras (estéreo intercalado) a partir da posição atual da saída.
    queue: VecDeque<i16>,
    /// Quando chegou o último pacote.
    last: Option<Duration>,
    /// Quadros do último pacote.
    last_frames: usize,
    /// Acabou de entrar: na próxima saída fica só com o último pacote.
    fresh: bool,
}

impl Track {
    fn frames(&self) -> usize {
        self.queue.len() / SAMPLES_PER_FRAME
    }

    fn live(&self, now: Duration) -> bool {
        self.last
            .is_some_and(|last| now.saturating_sub(last) < IDLE)
    }
}

#[derive(Default)]
pub(crate) struct Mixer {
    tracks: Vec<Track>,
    timeline: Timeline,
    sum: Vec<i32>,
    out: Vec<u8>,
    /// Quadros em que um programa vivo ficou sem som e entrou silêncio no lugar (lacuna criada
    /// aqui, não pelo Windows).
    pub gap_frames: u64,
    /// Quadros antigos descartados de programas que acabaram de entrar.
    pub trimmed_frames: u64,
}

impl Mixer {
    pub fn add(&mut self, id: u64) {
        self.tracks.push(Track {
            id,
            queue: VecDeque::new(),
            last: None,
            last_frames: 0,
            fresh: false,
        });
    }

    pub fn remove(&mut self, id: u64) {
        self.tracks.retain(|track| track.id != id);
    }

    /// Quadros entregues (som + silêncio) e quanto disso foi silêncio do relógio.
    pub fn emitted(&self) -> (u64, u64) {
        (self.timeline.emitted, self.timeline.filled)
    }

    /// Chegou um pacote de `frames` quadros do programa `id` (`pcm` vazio = silêncio).
    pub fn packet(&mut self, id: u64, now: Duration, frames: u32, pcm: Option<&[u8]>) {
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == id) else {
            return;
        };
        // Só o fluxo recém-aberto: um programa que volta depois de uma pausa (ou uma demora desta
        // thread, com todos os pacotes esperando no Windows) não tem nada velho a descartar.
        if track.last.is_none() {
            track.fresh = true;
        }
        track.last = Some(now);
        track.last_frames = frames as usize;
        match pcm {
            Some(pcm) => track.queue.extend(
                pcm.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|sample| i16::from_le_bytes(*sample)),
            ),
            None => track
                .queue
                .extend(std::iter::repeat_n(0, frames as usize * SAMPLES_PER_FRAME)),
        }
    }

    /// Entrega ao `on_pcm` o que já pode sair.
    pub fn flush(&mut self, now: Duration, on_pcm: &mut impl FnMut(&[u8])) {
        for track in self.tracks.iter_mut().filter(|track| track.fresh) {
            let extra = track.frames().saturating_sub(track.last_frames);
            track.queue.drain(..extra * SAMPLES_PER_FRAME);
            self.trimmed_frames += extra as u64;
            track.fresh = false;
        }
        let live = self.tracks.iter().filter(|track| track.live(now));
        let (slowest, fastest) = live
            .fold(None, |range: Option<(usize, usize)>, track| {
                let frames = track.frames();
                Some(range.map_or((frames, frames), |(lo, hi)| {
                    (lo.min(frames), hi.max(frames))
                }))
            })
            .unzip();
        let ready = match (slowest, fastest) {
            (Some(slowest), Some(fastest)) if fastest - slowest > MAX_SKEW => fastest - SKEW_TARGET,
            (Some(slowest), Some(_)) => slowest,
            // Ninguém entregando: sai o que sobrou nas filas e, depois, silêncio pelo relógio.
            _ => self.tracks.iter().map(Track::frames).max().unwrap_or(0),
        };
        if ready > 0 {
            self.emit(ready, now, on_pcm);
        } else if slowest.is_none() {
            crate::silence(self.timeline.silence_due(now), on_pcm);
        }
    }

    fn emit(&mut self, frames: usize, now: Duration, on_pcm: &mut impl FnMut(&[u8])) {
        let samples = frames * SAMPLES_PER_FRAME;
        self.sum.clear();
        self.sum.resize(samples, 0);
        for track in &mut self.tracks {
            let have = track.queue.len().min(samples);
            for (sum, &sample) in self.sum.iter_mut().zip(track.queue.iter()) {
                *sum += i32::from(sample);
            }
            if have < samples && track.live(now) {
                self.gap_frames += ((samples - have) / SAMPLES_PER_FRAME) as u64;
            }
            track.queue.drain(..have);
        }
        self.out.clear();
        self.out.extend(
            self.sum.iter().flat_map(|&sum| {
                (sum.clamp(i16::MIN.into(), i16::MAX.into()) as i16).to_le_bytes()
            }),
        );
        on_pcm(&self.out);
        self.timeline.packet(now, frames as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn pcm(value: i16, frames: usize) -> Vec<u8> {
        std::iter::repeat_n(value.to_le_bytes(), frames * SAMPLES_PER_FRAME)
            .flatten()
            .collect()
    }

    /// Junta o que sai, como amostras.
    fn collect(out: &mut Vec<i16>) -> impl FnMut(&[u8]) + '_ {
        |bytes| {
            out.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b)),
            )
        }
    }

    #[test]
    fn programs_are_summed_and_clipped() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.add(2);
        mixer.packet(1, ms(10), 480, Some(&pcm(30_000, 480)));
        mixer.packet(2, ms(10), 480, Some(&pcm(10_000, 480)));
        mixer.flush(ms(10), &mut collect(&mut out));
        assert_eq!(out.len(), 960);
        assert!(out.iter().all(|&s| s == i16::MAX));
        mixer.packet(1, ms(20), 480, Some(&pcm(-5, 480)));
        mixer.packet(2, ms(20), 480, Some(&pcm(7, 480)));
        out.clear();
        mixer.flush(ms(20), &mut collect(&mut out));
        assert!(out.iter().all(|&s| s == 2));
        assert_eq!(mixer.gap_frames, 0);
    }

    #[test]
    fn waits_for_the_other_program_of_the_same_cycle() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.add(2);
        for cycle in 1..=3 {
            mixer.packet(1, ms(cycle * 10), 480, Some(&pcm(1, 480)));
            mixer.packet(2, ms(cycle * 10), 480, Some(&pcm(2, 480)));
            mixer.flush(ms(cycle * 10), &mut collect(&mut out));
        }
        // O programa 1 chega primeiro: sai só quando o 2 chegar.
        mixer.packet(1, ms(40), 480, Some(&pcm(1, 480)));
        mixer.flush(ms(40), &mut collect(&mut out));
        assert_eq!(out.len(), 3 * 960);
        mixer.packet(2, ms(41), 480, Some(&pcm(2, 480)));
        mixer.flush(ms(41), &mut collect(&mut out));
        assert_eq!(out.len(), 4 * 960);
        assert!(out.iter().all(|&s| s == 3));
    }

    #[test]
    fn a_program_that_stops_releases_the_others() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.add(2);
        mixer.packet(1, ms(10), 480, Some(&pcm(1, 480)));
        mixer.packet(2, ms(10), 480, Some(&pcm(2, 480)));
        mixer.flush(ms(10), &mut collect(&mut out));
        // O 2 fecha: o 1 espera até o 2 deixar de estar vivo, e nada se perde.
        let mut tick = 20;
        while tick <= 100 {
            mixer.packet(1, ms(tick), 480, Some(&pcm(1, 480)));
            mixer.flush(ms(tick), &mut collect(&mut out));
            tick += 10;
        }
        assert_eq!(out.len(), 10 * 960);
        assert_eq!(mixer.gap_frames, 0);
        assert_eq!(mixer.emitted(), (4800, 0));
    }

    #[test]
    fn a_new_program_starts_at_the_current_position() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        for tick in [10, 20] {
            mixer.packet(1, ms(tick), 480, Some(&pcm(1, 480)));
            mixer.flush(ms(tick), &mut collect(&mut out));
        }
        mixer.add(2);
        mixer.packet(1, ms(30), 480, Some(&pcm(1, 480)));
        mixer.packet(2, ms(30), 480, Some(&pcm(2, 480)));
        mixer.flush(ms(30), &mut collect(&mut out));
        assert_eq!(out.len(), 3 * 960);
        assert!(out[2 * 960..].iter().all(|&s| s == 3));
    }

    #[test]
    fn a_new_program_keeps_only_its_latest_packet() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.add(2);
        // O 1 foi aberto antes e o Windows guardou três pacotes dele; o 2 tem um só.
        mixer.packet(1, ms(10), 480, Some(&pcm(1, 480)));
        mixer.packet(1, ms(10), 480, Some(&pcm(1, 480)));
        mixer.packet(1, ms(10), 480, Some(&pcm(5, 480)));
        mixer.packet(2, ms(10), 480, Some(&pcm(2, 480)));
        mixer.flush(ms(10), &mut collect(&mut out));
        assert_eq!(out.len(), 960);
        assert!(out.iter().all(|&s| s == 7));
        assert_eq!(mixer.trimmed_frames, 960);
        // Daqui em diante nada é descartado.
        mixer.packet(1, ms(20), 480, Some(&pcm(1, 480)));
        mixer.packet(2, ms(20), 480, Some(&pcm(2, 480)));
        mixer.flush(ms(20), &mut collect(&mut out));
        assert_eq!(out.len(), 2 * 960);
        assert_eq!(mixer.trimmed_frames, 960);
    }

    #[test]
    fn a_late_wake_loses_nothing() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.add(2);
        for tick in [10, 20] {
            mixer.packet(1, ms(tick), 480, Some(&pcm(1, 480)));
            mixer.packet(2, ms(tick), 480, Some(&pcm(2, 480)));
            mixer.flush(ms(tick), &mut collect(&mut out));
        }
        // Esta thread ficou 60 ms sem rodar (jogo pesado): seis pacotes de cada um esperando.
        for _ in 0..6 {
            mixer.packet(1, ms(80), 480, Some(&pcm(1, 480)));
            mixer.packet(2, ms(80), 480, Some(&pcm(2, 480)));
        }
        mixer.flush(ms(80), &mut collect(&mut out));
        assert_eq!(out.len(), 8 * 960);
        assert!(out.iter().all(|&s| s == 3));
        assert_eq!((mixer.trimmed_frames, mixer.gap_frames), (0, 0));
    }

    #[test]
    fn a_lagging_program_gets_a_gap_instead_of_holding_everyone() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.add(2);
        // O 2 entrega pacotes vazios de vez em quando (vivo), mas sem quadros: fica para trás.
        for tick in (10..=100).step_by(10) {
            mixer.packet(1, ms(tick), 480, Some(&pcm(1, 480)));
            mixer.packet(2, ms(tick), 0, Some(&[]));
            mixer.flush(ms(tick), &mut collect(&mut out));
        }
        // Aos 90 ms passou de 80 ms de diferença: a saída andou até 20 ms atrás do 1, e o 2 ganhou
        // uma lacuna desse tamanho.
        assert_eq!(out.len() / 2, 90 * 48 - 960);
        assert_eq!(mixer.gap_frames, 90 * 48 - 960);
    }

    #[test]
    fn silence_follows_the_clock_when_nothing_plays() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        for tick in (10..=1000).step_by(10) {
            mixer.flush(ms(tick), &mut collect(&mut out));
        }
        let (emitted, filled) = mixer.emitted();
        assert_eq!(emitted, filled);
        // Um período (10 ms) atrás do relógio, como no fluxo único.
        assert!((480..960).contains(&(48_000 - emitted)), "{emitted}");
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn silent_packets_count_as_sound() {
        let mut mixer = Mixer::default();
        let mut out = Vec::new();
        mixer.add(1);
        mixer.packet(1, ms(10), 480, None);
        mixer.flush(ms(10), &mut collect(&mut out));
        assert_eq!(out.len(), 960);
        assert_eq!(mixer.emitted(), (480, 0));
    }
}
