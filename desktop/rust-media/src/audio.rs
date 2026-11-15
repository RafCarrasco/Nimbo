//! Áudio da live: junta o PCM capturado (`nimbo_audio`) em pacotes Opus de 20 ms e diz a que
//! instante cada pacote corresponde, para o navegador tocar o som junto com o vídeo.
//!
//! Um codificador só, como no vídeo: o mesmo pacote Opus vai para todos os espectadores.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nimbo_audio::{CHANNELS, FRAME_BYTES, SAMPLE_RATE};
use opus::{Application, Bitrate, Channels, Encoder};

/// Quadros (uma amostra de cada canal) por pacote: 20 ms, o tamanho que os navegadores esperam.
const PACKET_FRAMES: u64 = SAMPLE_RATE as u64 / 50;
const PACKET_SAMPLES: usize = PACKET_FRAMES as usize * CHANNELS as usize;
/// Taxa do Opus: som de jogo e música em estéreo sem perda audível.
pub(crate) const BITRATE: u32 = 128_000;
/// Teto de um pacote: cabe inteiro num pacote de rede. O Opus respeita o tamanho do buffer.
const PACKET_MAX: usize = 1000;
/// Diferença entre a posição e o relógio acima da qual o relógio é reacertado de uma vez (a
/// captura travou ou o computador dormiu).
const RESYNC: Duration = Duration::from_millis(100);

/// Quantos quadros de áudio cabem em `time`.
pub(crate) fn frames_in(time: Duration) -> u64 {
    (time.as_micros() * u128::from(SAMPLE_RATE) / 1_000_000) as u64
}

fn span(frames: u64) -> Duration {
    Duration::from_micros(frames * 1_000_000 / u64::from(SAMPLE_RATE))
}

/// Um pacote Opus (20 ms), o mesmo para todos os espectadores.
pub(crate) struct Packet {
    pub data: Arc<[u8]>,
    /// Instante em que o primeiro quadro do pacote foi capturado.
    pub at: Instant,
    /// Tempo RTP (48 kHz) do primeiro quadro.
    pub rtp: u64,
}

/// A que instante corresponde cada posição do fluxo de áudio.
///
/// O som chega em blocos de 10 ms com atraso variável. Carimbar cada pacote com a hora de chegada
/// do bloco faria o áudio "tremer" em relação ao vídeo; aqui a posição manda e a hora de chegada só
/// corrige, devagar, a diferença de ritmo entre o relógio da placa de som e o do computador.
#[derive(Default)]
struct Clock {
    /// Instante da posição zero.
    origin: Option<Instant>,
}

impl Clock {
    /// O fluxo chegou à posição `end` agora (`now`): o último quadro acabou de ser capturado.
    fn sync(&mut self, end: u64, now: Instant) {
        let seen = now.checked_sub(span(end)).unwrap_or(now);
        let Some(origin) = self.origin else {
            self.origin = Some(seen);
            return;
        };
        let late = seen.saturating_duration_since(origin);
        let early = origin.saturating_duration_since(seen);
        self.origin = Some(if late.max(early) > RESYNC {
            seen
        } else if early.is_zero() {
            origin + late / 32
        } else {
            origin.checked_sub(early / 32).unwrap_or(origin)
        });
    }

    fn at(&self, position: u64, now: Instant) -> Instant {
        // Nunca no futuro: o WebRTC usa este instante para dizer ao navegador que horas são.
        self.origin
            .map_or(now, |origin| (origin + span(position)).min(now))
    }
}

/// Recebe PCM (16 bits, 48 kHz, estéreo) em blocos de qualquer tamanho e entrega pacotes Opus.
pub(crate) struct Packer {
    opus: Encoder,
    /// Amostras que ainda não completaram um pacote.
    pending: Vec<i16>,
    /// Tempo RTP do primeiro quadro desta captura.
    start: u64,
    /// Quadros recebidos e quadros já fechados em pacotes, desde o início desta captura.
    received: u64,
    packed: u64,
    clock: Clock,
    out: [u8; PACKET_MAX],
}

impl Packer {
    /// `start` é o tempo RTP do primeiro quadro: quando a captura é trocada no meio da live, o
    /// tempo segue de onde estava em vez de voltar a zero.
    pub(crate) fn new(start: u64) -> Result<Self, opus::Error> {
        // LowDelay: só o modo de música do Opus (CELT), com 4 ms a menos de atraso.
        let mut opus = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::LowDelay)?;
        opus.set_bitrate(Bitrate::Bits(BITRATE as i32))?;
        Ok(Self {
            opus,
            pending: Vec::with_capacity(PACKET_SAMPLES * 2),
            start,
            received: 0,
            packed: 0,
            clock: Clock::default(),
            out: [0; PACKET_MAX],
        })
    }

    /// Tempo RTP do próximo quadro que chegar.
    pub(crate) fn next_rtp(&self) -> u64 {
        self.start + self.received
    }

    /// `pcm` acabou de ser capturado (`now`). Com `encode` falso (ninguém ouvindo) o tempo anda,
    /// mas nada é codificado.
    pub(crate) fn push(
        &mut self,
        pcm: &[u8],
        now: Instant,
        encode: bool,
        mut emit: impl FnMut(Packet),
    ) -> Result<(), opus::Error> {
        let samples = pcm.as_chunks::<2>().0;
        self.pending
            .extend(samples.iter().map(|sample| i16::from_le_bytes(*sample)));
        self.received += (pcm.len() / FRAME_BYTES) as u64;
        self.clock.sync(self.received, now);
        let mut result = Ok(());
        let mut used = 0;
        for frame in self.pending.as_chunks::<PACKET_SAMPLES>().0 {
            used += PACKET_SAMPLES;
            let position = self.packed;
            self.packed += PACKET_FRAMES;
            if !encode {
                continue;
            }
            match self.opus.encode(frame, &mut self.out) {
                Ok(len) => emit(Packet {
                    data: self.out[..len].into(),
                    at: self.clock.at(position, now),
                    rtp: self.start + position,
                }),
                // O pacote se perde (o navegador disfarça 20 ms); o fluxo continua.
                Err(error) => result = Err(error),
            }
        }
        self.pending.drain(..used);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    /// `frames` quadros de um tom de 440 Hz, no formato que a captura entrega.
    fn tone(from: u64, frames: u64) -> Vec<u8> {
        (from..from + frames)
            .flat_map(|n| {
                let phase = n as f64 * 440.0 * std::f64::consts::TAU / f64::from(SAMPLE_RATE);
                let sample = ((phase.sin() * 8000.0) as i16).to_le_bytes();
                [sample[0], sample[1], sample[0], sample[1]]
            })
            .collect()
    }

    #[test]
    fn clock_follows_the_position_not_the_arrival_jitter() {
        let start = Instant::now();
        let mut clock = Clock::default();
        // Blocos de 10 ms que chegam ora na hora, ora 8 ms atrasados.
        for block in 1..=500u64 {
            let jitter = if block % 3 == 0 { ms(8) } else { ms(0) };
            clock.sync(block * 480, start + ms(block * 10) + jitter);
        }
        let now = start + ms(6000);
        let step = clock.at(96_000, now) - clock.at(95_040, now);
        assert_eq!(step, ms(20));
        // O atraso médio (menos de 3 ms) é absorvido; os 8 ms de um bloco isolado, não.
        let offset = clock.at(0, now).saturating_duration_since(start);
        assert!(offset < ms(4), "{offset:?}");
    }

    #[test]
    fn clock_follows_a_sound_card_slower_than_the_computer() {
        let start = Instant::now();
        let mut clock = Clock::default();
        // Placa de som 1% lenta: 475 quadros a cada 10 ms do computador, por um minuto.
        for block in 1..=6000u64 {
            clock.sync(block * 475, start + ms(block * 10));
        }
        let now = start + ms(60_000);
        let error = now - clock.at(6000 * 475, now);
        assert!(error < ms(5), "{error:?}");
    }

    #[test]
    fn clock_resyncs_at_once_after_a_stall() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.sync(480, start + ms(10));
        // Meio segundo sem som nenhum (a captura pulou o buraco): a posição continua de onde parou.
        clock.sync(960, start + ms(520));
        assert_eq!(clock.at(960, start + ms(520)), start + ms(520));
    }

    #[test]
    fn clock_never_points_to_the_future() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.sync(480, start + ms(10));
        assert_eq!(clock.at(48_000, start + ms(10)), start + ms(10));
    }

    #[test]
    fn pcm_becomes_20ms_stereo_opus_packets() {
        let start = Instant::now();
        let mut packer = Packer::new(1000).unwrap();
        let mut packets = Vec::new();
        // Blocos de 10 ms menos um quadro, como o Windows às vezes entrega: 100 blocos = 998 ms.
        for block in 0..100u64 {
            let pcm = tone(block * 479, 479);
            let now = start + ms((block + 1) * 10);
            packer
                .push(&pcm, now, true, |packet| packets.push(packet))
                .unwrap();
        }
        assert_eq!(packets.len(), 49);
        let mut decoder = opus::Decoder::new(SAMPLE_RATE, Channels::Stereo).unwrap();
        let mut pcm = [0i16; PACKET_SAMPLES];
        let mut loudest = 0;
        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(packet.rtp, 1000 + index as u64 * PACKET_FRAMES);
            assert!(packet.data.len() <= PACKET_MAX);
            assert_eq!(
                opus::packet::get_nb_channels(&packet.data).unwrap(),
                Channels::Stereo
            );
            let frames = decoder.decode(&packet.data, &mut pcm, false).unwrap();
            assert_eq!(frames as u64, PACKET_FRAMES);
            loudest = loudest.max(pcm.iter().map(|s| s.unsigned_abs()).max().unwrap());
            if index > 0 {
                assert!(packet.at >= packets[index - 1].at);
            }
        }
        // O tom entrou com amplitude 8000 e tem de sair parecido, não silêncio.
        assert!((6000..10_000).contains(&loudest), "{loudest}");
    }

    #[test]
    fn time_keeps_running_while_nobody_listens() {
        let start = Instant::now();
        let mut packer = Packer::new(0).unwrap();
        let mut packets = Vec::new();
        for block in 0..10u64 {
            let now = start + ms((block + 1) * 10);
            packer
                .push(&tone(block * 480, 480), now, false, |p| packets.push(p))
                .unwrap();
        }
        assert!(packets.is_empty());
        packer
            .push(&tone(4800, 960), start + ms(120), true, |p| packets.push(p))
            .unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].rtp, 4800);
        assert_eq!(packer.next_rtp(), 5760);
    }
}
